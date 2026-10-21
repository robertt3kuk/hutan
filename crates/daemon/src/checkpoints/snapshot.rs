//! One snapshot (spike `docs/spikes/2026-10-21-checkpoints.md` §3.2), under the checkout's lock:
//!
//! 1. **Seed** the private index once: a copy of the user's index (its mtime kept, so git's racy
//!    checks still hold), or `read-tree HEAD` when it has conflict stages or does not exist, or
//!    empty on an unborn branch. Gitlinks and paths with a `filter=` attribute leave it; paths
//!    whose blobs went through eol/ident/encoding conversion get their stat data cleared so
//!    the next step rehashes them as raw bytes.
//! 2. **Stat check** with `diff-files` (no content read) and the untracked, not ignored files
//!    (`ls-files -o --exclude-standard`; `dir/` is a nested repository). When an ignore rule
//!    may have changed (a `.gitignore` among the changes, `info/exclude`, the global excludes
//!    file), entries Hutan added that are ignored by now leave the index, so they are never
//!    read again. When something is stat-dirty, entries too large or no longer regular files
//!    leave too, then `update-index --refresh` compares the rest and `diff-files` lists what
//!    really changed. Both run with attributes switched off: content is compared as raw bytes
//!    and no filter driver runs.
//! 3. **Candidates**: the changed and deleted entries and the untracked files.
//! 4. **Filters** with `lstat` only: secret names, size, non-regular files, readability, then
//!    `check-attr filter`, then the snapshot's file and byte budget (largest skipped first).
//!    A skipped path is left out of the tree and listed.
//! 5. **Hash** the rest with `hash-object -w --no-filters` (raw bytes; symbolic links as their
//!    target text), then `update-index --index-info` (removals first), `write-tree`,
//!    `commit-tree` (its trailers name the session, turn, kind, checkpoint and the user's HEAD
//!    of the moment, so a restore can say HEAD moved), `update-ref`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use super::caps::{self, Candidate, Caps, Kind as FileKind, SkipReason, Skipped};
use super::git::{
    self, Git, nul_records, os, os_args, parse_check_attr, parse_diff_files, parse_staged,
};
use super::{CheckpointError, HEAD_TRAILER, Label, Repo, Snapshot, Unsupported};

/// Total bytes of paths per `hash-object` command line (macOS `ARG_MAX` is 1 MiB).
const ARGS_BYTES: usize = 256 * 1024;

/// The stat-level comparison of the private index with the files (no content read, except
/// git's check of racily clean entries).
const DIFF_FILES: [&str; 4] = ["diff-files", "--raw", "-z", "--no-renames"];

/// Stat-dirty entries up to which only they are checked against the ignore rules
/// (`check-ignore`); past it the whole private index is (`ls-files -c -i`, one pass).
const CHECK_IGNORE_PATHS: usize = 256;

/// What [`take`] wrote.
#[derive(Debug)]
pub(crate) struct Parts {
    reference: Option<String>,
    commit: Option<String>,
    tree: String,
    files_hashed: usize,
    bytes_hashed: u64,
    files_removed: usize,
    skipped: Vec<Skipped>,
    seeded: bool,
}

impl Parts {
    pub fn into_snapshot(
        self,
        repo: &Repo,
        label: &Label,
        started: Instant,
        finished: Instant,
    ) -> Snapshot {
        Snapshot {
            checkout: repo.toplevel.clone(),
            common_dir: repo.common_dir.clone(),
            reference: self.reference,
            commit: self.commit,
            tree: self.tree,
            parent: label.parent.clone(),
            files_hashed: self.files_hashed,
            bytes_hashed: self.bytes_hashed,
            files_removed: self.files_removed,
            skipped: self.skipped,
            seeded: self.seeded,
            started,
            finished,
        }
    }
}

/// What the engine remembers of a checkout between snapshots.
#[derive(Debug, Default)]
pub(crate) struct Memory {
    /// The ignore sources at the last snapshot (`None`: check the private index for entries
    /// that became ignored).
    ignore: Option<IgnoreWatch>,
    /// The tree the private index held after the last snapshot (only Hutan writes it).
    tree: Option<String>,
}

/// Takes one snapshot of `repo` through the private index `index`. `git` runs in the
/// checkout on the user's index (read-only commands only).
pub(crate) async fn take(
    git: &Git,
    repo: &Repo,
    index: &Path,
    caps: &Caps,
    label: &Label,
    memory: &mut Memory,
) -> Result<Parts, CheckpointError> {
    prepare(index)?;
    let result = Snapshotter::new(git, repo, index, caps)
        .run(label, memory)
        .await;
    if let Err(error) = &result
        && error.counts()
    {
        // Start the next one from a known state (a reseed costs one index copy).
        let _ = fs::remove_file(index);
    }
    result
}

/// The files besides `.gitignore`s in the tree that ignore rules come from (the repository's
/// `info/exclude`, the global excludes file, the configuration files that name it, the top
/// level `.gitignore` even when it ignores itself) and their modification time and size.
/// While none of them changes and no `.gitignore` is among a snapshot's changes, no entry of
/// the private index can have become ignored, and the check for that is skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IgnoreWatch(Vec<(PathBuf, Option<(SystemTime, u64)>)>);

impl IgnoreWatch {
    async fn read(user: &Git, repo: &Repo) -> Result<Self, CheckpointError> {
        let mut files = vec![
            repo.common_dir.join("info/exclude"),
            repo.common_dir.join("config"),
            repo.git_dir.join("config.worktree"),
            repo.toplevel.join(".gitignore"),
        ];
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        if let Some(home) = &home {
            files.push(home.join(".gitconfig"));
        }
        if let Some(xdg) = &xdg {
            files.push(xdg.join("git/config"));
            files.push(xdg.join("git/ignore"));
        }
        let configured = user
            .read(
                &os_args(&["config", "--path", "--get", "core.excludesFile"]),
                None,
            )
            .await?;
        if configured.success && !configured.line().is_empty() {
            files.push(repo.toplevel.join(configured.line()));
        }
        Ok(Self(
            files
                .into_iter()
                .map(|path| {
                    let stamp = stamp(&path);
                    (path, stamp)
                })
                .collect(),
        ))
    }

    fn unchanged(&self) -> bool {
        self.0.iter().all(|(path, seen)| stamp(path) == *seen)
    }
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Whether a path's file name is `.gitignore`.
fn is_gitignore(path: &[u8]) -> bool {
    path.rsplit(|b| *b == b'/').next() == Some(b".gitignore".as_slice())
}

/// Creates the store directory (private to the user) and removes a lock left by a daemon that
/// died during a write: the checkout's lock is held, and only one daemon runs per store.
fn prepare(index: &Path) -> Result<(), CheckpointError> {
    if let Some(dir) = index.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|source| CheckpointError::Io {
                what: format!("create {}", dir.display()),
                source,
            })?;
    }
    match fs::remove_file(with_suffix(index, ".lock")) {
        Ok(()) => tracing::info!(index = %index.display(), "removed a stale checkpoint index lock"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CheckpointError::Io {
                what: "remove a stale index lock".to_owned(),
                source,
            });
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> CheckpointError {
    let what = format!("{what} {}", path.display());
    move |source| CheckpointError::Io { what, source }
}

struct Snapshotter<'a> {
    repo: &'a Repo,
    caps: &'a Caps,
    index: &'a Path,
    /// On the user's index: read-only commands.
    user: &'a Git,
    /// On the private index.
    private: Git,
    /// On the private index, attributes off.
    raw: Git,
    null: String,
    /// Removed from the private index this time.
    removed: BTreeSet<Vec<u8>>,
    skipped: BTreeMap<Vec<u8>, Skipped>,
}

impl<'a> Snapshotter<'a> {
    fn new(user: &'a Git, repo: &'a Repo, index: &'a Path, caps: &'a Caps) -> Self {
        let private = user.on_index(index);
        let raw = private.raw(repo.empty_tree());
        Self {
            repo,
            caps,
            index,
            user,
            private,
            raw,
            null: repo.null_oid(),
            removed: BTreeSet::new(),
            skipped: BTreeMap::new(),
        }
    }

    async fn run(mut self, label: &Label, memory: &mut Memory) -> Result<Parts, CheckpointError> {
        // Put back only when this snapshot succeeds; until then the next one checks again.
        let Memory {
            ignore: last_rules,
            tree: last_tree,
        } = std::mem::take(memory);
        let seeded = !self.index.exists();
        if seeded {
            self.seed().await?;
        }
        let rules_held = last_rules.as_ref().is_some_and(IgnoreWatch::unchanged);
        let rules = match last_rules {
            Some(rules) if rules_held => rules,
            _ => IgnoreWatch::read(self.user, self.repo).await?,
        };

        let mut dirty = parse_diff_files(&self.raw.read_ok(&DIFF_FILES).await?.stdout);
        let untracked: Vec<Vec<u8>> = nul_records(
            &self
                .private
                .read_ok(&["ls-files", "-z", "-o", "--exclude-standard"])
                .await?
                .stdout,
        )
        .map(<[u8]>::to_vec)
        .collect();
        if untracked.len() > self.caps.max_untracked_files {
            return Err(CheckpointError::Unsupported(
                Unsupported::TooManyUntracked {
                    count: untracked.len(),
                    limit: self.caps.max_untracked_files,
                },
            ));
        }
        // A seeded index holds only what the user's index tracks. Otherwise: every entry when
        // an ignore rule may have changed, else (a `.gitignore` that ignores itself is in no
        // list) at least the stat-dirty ones the next steps would read; `check-ignore` costs
        // about 0.1 ms a path, so past a few hundred the whole index is checked instead.
        if !seeded {
            let reads = dirty.iter().filter(|e| e.status != b'D').count();
            if !rules_held
                || reads > CHECK_IGNORE_PATHS
                || dirty.iter().any(|e| is_gitignore(&e.path))
                || untracked.iter().any(|p| is_gitignore(p))
            {
                self.prune_ignored().await?;
            } else {
                self.drop_ignored(&dirty).await?;
            }
            dirty.retain(|e| !self.removed.contains(&e.path));
        }
        let changed = self.changed(dirty).await?;

        let mut candidates = Vec::new();
        for entry in changed {
            if entry.status == b'D' {
                self.remove(entry.path);
            } else {
                self.classify(entry.path, true, &mut candidates);
            }
        }
        for path in untracked {
            if let Some(dir) = path.strip_suffix(b"/") {
                self.skip(dir.to_vec(), SkipReason::NestedRepository, None, false);
            } else {
                self.classify(path, false, &mut candidates);
            }
        }
        let candidates = self.drop_filtered(candidates).await?;
        let (kept, over) = caps::select(candidates, self.caps);
        for (candidate, reason) in over {
            self.skip(
                candidate.path,
                reason,
                Some(candidate.bytes),
                candidate.tracked,
            );
        }

        let oids = self.hash(&kept).await?;
        let mut info = Vec::new();
        for path in &self.removed {
            self.push_info(&mut info, "0", &self.null, path);
        }
        for (candidate, oid) in kept.iter().zip(&oids) {
            self.push_info(&mut info, candidate.mode, oid, &candidate.path);
        }
        self.index_info(info).await?;

        let tree = match last_tree {
            // Nothing written to the private index: the same tree as last time.
            Some(tree) if !seeded && self.removed.is_empty() && kept.is_empty() => tree,
            _ => self
                .private
                .write_ok(&os_args(&["write-tree"]), None)
                .await?
                .line(),
        };
        // A warm-up records nothing; an end that changed nothing neither.
        let unchanged = label.unchanged_tree.as_deref() == Some(tree.as_str());
        let (commit, reference) = match label.reference() {
            Some(reference) if !unchanged => {
                let commit = self.commit(&tree, label).await?;
                self.private
                    .write_ok(&os_args(&["update-ref", &reference, &commit]), None)
                    .await?;
                (Some(commit), Some(reference))
            }
            _ => (None, None),
        };
        *memory = Memory {
            ignore: Some(rules),
            tree: Some(tree.clone()),
        };

        Ok(Parts {
            reference,
            commit,
            tree,
            files_hashed: kept.len(),
            bytes_hashed: kept.iter().map(|c| c.bytes).sum(),
            files_removed: self.removed.len(),
            skipped: self.skipped.into_values().collect(),
            seeded,
        })
    }

    // Seed and prune ------------------------------------------------------------------------

    async fn seed(&mut self) -> Result<(), CheckpointError> {
        let copied = self.copy_user_index()?;
        let conflicted = copied
            && !self
                .private
                .read_ok(&["ls-files", "-u", "-z"])
                .await?
                .stdout
                .is_empty();
        if !copied || conflicted {
            let head = self
                .user
                .read(
                    &os_args(&["rev-parse", "-q", "--verify", "HEAD^{tree}"]),
                    None,
                )
                .await?;
            let tree = head.line();
            let args = if head.success && !tree.is_empty() {
                vec!["read-tree", tree.as_str()]
            } else {
                vec!["read-tree", "--empty"]
            };
            self.private.write_ok(&os_args(&args), None).await?;
        }

        let entries = parse_staged(
            &self
                .private
                .read_ok(&["ls-files", "-s", "-z"])
                .await?
                .stdout,
        );
        let autocrlf = matches!(
            self.user
                .read(&os_args(&["config", "--get", "core.autocrlf"]), None)
                .await?
                .line()
                .as_str(),
            "true" | "input"
        );
        let mut input = Vec::new();
        let mut by_path: HashMap<&[u8], &git::IndexEntry> = HashMap::new();
        for entry in &entries {
            if entry.mode == "160000" {
                self.removed.insert(entry.path.clone());
            } else {
                input.extend_from_slice(&entry.path);
                input.push(0);
                by_path.insert(&entry.path, entry);
            }
        }
        let mut info = Vec::new();
        if !input.is_empty() {
            let output = self
                .private
                .read_ok_with(
                    &[
                        "check-attr",
                        "-z",
                        "--stdin",
                        "filter",
                        "text",
                        "eol",
                        "ident",
                        "working-tree-encoding",
                    ],
                    input,
                )
                .await?;
            let mut attrs: BTreeMap<Vec<u8>, HashMap<String, String>> = BTreeMap::new();
            for (path, attr, value) in parse_check_attr(&output.stdout) {
                attrs.entry(path).or_default().insert(attr, value);
            }
            for (path, attrs) in attrs {
                let get = |name: &str| attrs.get(name).map_or("unspecified", String::as_str);
                if is_driver(get("filter")) {
                    self.removed.insert(path);
                    continue;
                }
                let text = get("text");
                let converts = (text != "unset"
                    && (text != "unspecified" || get("eol") != "unspecified" || autocrlf))
                    || get("ident") == "set"
                    || !matches!(get("working-tree-encoding"), "unspecified" | "unset");
                if converts && let Some(entry) = by_path.get(path.as_slice()) {
                    // Same blob, no stat data: the next refresh compares the raw bytes.
                    let (mode, oid) = (entry.mode.clone(), entry.oid.clone());
                    self.push_info(&mut info, &mode, &oid, &path);
                }
            }
        }
        for path in &self.removed {
            self.push_info(&mut info, "0", &self.null, path);
        }
        self.index_info(info).await?;
        // What left here is reported where it shows up again: as untracked.
        self.removed.clear();
        Ok(())
    }

    /// Copies the user's index to the private one, keeping its mtime. `false` when there is
    /// none. Reading never locks or changes it (git replaces it by rename, so a read sees a
    /// whole file even while `index.lock` is held).
    fn copy_user_index(&self) -> Result<bool, CheckpointError> {
        let user_index = self.repo.git_dir.join("index");
        let bytes = match fs::read(&user_index) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error("read", &user_index)(source)),
        };
        let modified = fs::metadata(&user_index)
            .and_then(|m| m.modified())
            .map_err(io_error("stat", &user_index))?;
        let seed = with_suffix(self.index, ".seed");
        let file = fs::File::create(&seed).map_err(io_error("create", &seed))?;
        io::Write::write_all(&mut &file, &bytes).map_err(io_error("write", &seed))?;
        file.set_modified(modified)
            .map_err(io_error("set mtime of", &seed))?;
        drop(file);
        fs::rename(&seed, self.index).map_err(io_error("rename", &seed))?;
        Ok(true)
    }

    /// Marks for removal the entries Hutan added that are ignored by now and that the user's
    /// index does not track, so they are never read again.
    async fn prune_ignored(&mut self) -> Result<(), CheckpointError> {
        let ignored = self
            .private
            .read_ok(&["ls-files", "-z", "-c", "-i", "--exclude-standard"])
            .await?;
        if ignored.stdout.is_empty() {
            return Ok(());
        }
        let tracked = self.user.read_ok(&["ls-files", "-z", "-c"]).await?;
        let tracked: HashSet<&[u8]> = nul_records(&tracked.stdout).collect();
        for path in nul_records(&ignored.stdout) {
            if !tracked.contains(path) {
                self.removed.insert(path.to_vec());
            }
        }
        Ok(())
    }

    /// Marks for removal the stat-dirty entries that are ignored by now and that the user's
    /// index does not track (exactly what `check-ignore` on the user's index reports), so
    /// neither the refresh nor hashing reads them.
    async fn drop_ignored(&mut self, dirty: &[git::DiffEntry]) -> Result<(), CheckpointError> {
        let mut input = Vec::new();
        for entry in dirty.iter().filter(|e| e.status != b'D') {
            input.extend_from_slice(&entry.path);
            input.push(0);
        }
        if input.is_empty() {
            return Ok(());
        }
        let output = self
            .user
            .read(&os_args(&["check-ignore", "-z", "--stdin"]), Some(input))
            .await?;
        // Exit 1 with nothing printed: none is ignored.
        if !output.success && !(output.stdout.is_empty() && output.stderr.trim().is_empty()) {
            return Err(CheckpointError::GitFailed {
                step: "check-ignore".to_owned(),
                message: output.message(),
            });
        }
        for path in nul_records(&output.stdout) {
            self.removed.insert(path.to_vec());
        }
        Ok(())
    }

    // Changes -------------------------------------------------------------------------------

    /// Entries of the private index whose file changed or went away, compared as raw bytes;
    /// `dirty` is what the stat data says.
    async fn changed(
        &mut self,
        dirty: Vec<git::DiffEntry>,
    ) -> Result<Vec<git::DiffEntry>, CheckpointError> {
        if dirty.iter().all(|e| e.status == b'D') {
            return Ok(dirty);
        }
        // The refresh reads every stat-dirty file: entries it must not read leave first.
        for entry in &dirty {
            let full = self.repo.toplevel.join(os(&entry.path));
            let drop = match caps::lstat(&full) {
                Ok(FileKind::File { bytes, .. }) => {
                    if bytes > self.caps.max_file_bytes {
                        self.skip(entry.path.clone(), SkipReason::TooLarge, Some(bytes), false);
                        true
                    } else if !caps::readable(&full) {
                        self.skip(
                            entry.path.clone(),
                            SkipReason::Unreadable,
                            Some(bytes),
                            false,
                        );
                        true
                    } else {
                        false
                    }
                }
                Ok(FileKind::Other { bytes }) => {
                    self.skip(
                        entry.path.clone(),
                        SkipReason::NotRegular,
                        Some(bytes),
                        false,
                    );
                    true
                }
                Ok(FileKind::Directory) => true,
                Ok(FileKind::Missing | FileKind::Symlink { .. }) => false,
                Err(_) => {
                    self.skip(entry.path.clone(), SkipReason::Unreadable, None, false);
                    true
                }
            };
            if drop {
                self.removed.insert(entry.path.clone());
            }
        }
        // Pruned and dropped entries leave before the refresh (the final update repeats them).
        let mut info = Vec::new();
        for path in &self.removed {
            self.push_info(&mut info, "0", &self.null, path);
        }
        self.index_info(info).await?;
        self.raw
            .write_ok(
                &os_args(&["update-index", "-q", "--ignore-missing", "--refresh"]),
                None,
            )
            .await?;
        Ok(parse_diff_files(
            &self.raw.read_ok(&DIFF_FILES).await?.stdout,
        ))
    }

    // Filters -------------------------------------------------------------------------------

    /// `lstat` filters of one changed or untracked path.
    fn classify(&mut self, path: Vec<u8>, tracked: bool, candidates: &mut Vec<Candidate>) {
        let full = self.repo.toplevel.join(os(&path));
        let (bytes, mode) = match caps::lstat(&full) {
            Ok(FileKind::Missing | FileKind::Directory) => {
                // Gone, or a directory now (its files are listed as untracked).
                if tracked {
                    self.remove(path);
                }
                return;
            }
            Ok(FileKind::Other { bytes }) => {
                self.skip(path, SkipReason::NotRegular, Some(bytes), tracked);
                return;
            }
            Err(_) => {
                self.skip(path, SkipReason::Unreadable, None, tracked);
                return;
            }
            Ok(FileKind::Symlink { bytes }) => (bytes, "120000"),
            Ok(FileKind::File { bytes, executable }) => {
                (bytes, if executable { "100755" } else { "100644" })
            }
        };
        if caps::excluded(&path, &self.caps.exclude) {
            self.skip(path, SkipReason::SecretName, Some(bytes), tracked);
        } else if bytes > self.caps.max_file_bytes {
            self.skip(path, SkipReason::TooLarge, Some(bytes), tracked);
        } else if mode != "120000" && !caps::readable(&full) {
            self.skip(path, SkipReason::Unreadable, Some(bytes), tracked);
        } else {
            candidates.push(Candidate {
                path,
                bytes,
                mode,
                tracked,
            });
        }
    }

    /// Skips candidates with a `filter=` attribute (Git LFS and the like).
    async fn drop_filtered(
        &mut self,
        candidates: Vec<Candidate>,
    ) -> Result<Vec<Candidate>, CheckpointError> {
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let mut input = Vec::new();
        for candidate in &candidates {
            input.extend_from_slice(&candidate.path);
            input.push(0);
        }
        let output = self
            .private
            .read_ok_with(&["check-attr", "-z", "--stdin", "filter"], input)
            .await?;
        let drivers: HashMap<Vec<u8>, String> = parse_check_attr(&output.stdout)
            .into_iter()
            .filter(|(_, _, value)| is_driver(value))
            .map(|(path, _, value)| (path, value))
            .collect();
        let mut kept = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if let Some(driver) = drivers.get(&candidate.path) {
                self.skip(
                    candidate.path,
                    SkipReason::Filter {
                        driver: driver.clone(),
                    },
                    Some(candidate.bytes),
                    candidate.tracked,
                );
            } else {
                kept.push(candidate);
            }
        }
        Ok(kept)
    }

    fn skip(&mut self, path: Vec<u8>, reason: SkipReason, bytes: Option<u64>, tracked: bool) {
        if tracked {
            self.removed.insert(path.clone());
        }
        self.skipped.entry(path.clone()).or_insert(Skipped {
            path: caps::path_buf(&path),
            reason,
            bytes,
        });
    }

    fn remove(&mut self, path: Vec<u8>) {
        self.removed.insert(path);
    }

    // Writes --------------------------------------------------------------------------------

    /// `hash-object -w --no-filters` of `kept`, in order: raw file bytes; a symbolic link's
    /// target text through a temporary file (`hash-object` would follow the link).
    async fn hash(&self, kept: &[Candidate]) -> Result<Vec<String>, CheckpointError> {
        if kept.is_empty() {
            return Ok(Vec::new());
        }
        let links_dir = with_suffix(self.index, ".links");
        let mut paths: Vec<OsString> = Vec::with_capacity(kept.len());
        let mut links = false;
        for (n, candidate) in kept.iter().enumerate() {
            if candidate.mode == "120000" {
                if !links {
                    let _ = fs::remove_dir_all(&links_dir);
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&links_dir)
                        .map_err(io_error("create", &links_dir))?;
                    links = true;
                }
                let full = self.repo.toplevel.join(os(&candidate.path));
                let target = fs::read_link(&full).map_err(io_error("read link", &full))?;
                let file = links_dir.join(n.to_string());
                fs::write(&file, target.into_os_string().into_vec())
                    .map_err(io_error("write", &file))?;
                paths.push(file.into_os_string());
            } else {
                paths.push(os(&candidate.path));
            }
        }
        let result = self.hash_paths(&paths).await;
        if links {
            let _ = fs::remove_dir_all(&links_dir);
        }
        let oids = result?;
        if oids.len() != kept.len() {
            return Err(CheckpointError::GitFailed {
                step: "hash-object".to_owned(),
                message: format!("{} ids for {} files", oids.len(), kept.len()),
            });
        }
        Ok(oids)
    }

    async fn hash_paths(&self, paths: &[OsString]) -> Result<Vec<String>, CheckpointError> {
        let mut oids = Vec::with_capacity(paths.len());
        let mut start = 0;
        while start < paths.len() {
            let mut end = start;
            let mut size = 0;
            while end < paths.len() && (end == start || size + paths[end].len() < ARGS_BYTES) {
                size += paths[end].len() + 1;
                end += 1;
            }
            let mut args: Vec<&OsStr> = os_args(&["hash-object", "-w", "--no-filters", "--"]);
            args.extend(paths[start..end].iter().map(OsString::as_os_str));
            let output = self.private.write_ok(&args, None).await?;
            oids.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::to_owned),
            );
            start = end;
        }
        Ok(oids)
    }

    fn push_info(&self, info: &mut Vec<u8>, mode: &str, oid: &str, path: &[u8]) {
        info.extend_from_slice(format!("{mode} {oid}\t").as_bytes());
        info.extend_from_slice(path);
        info.push(0);
    }

    async fn index_info(&self, info: Vec<u8>) -> Result<(), CheckpointError> {
        if info.is_empty() {
            return Ok(());
        }
        self.private
            .write_ok(
                &os_args(&["update-index", "--add", "--replace", "-z", "--index-info"]),
                Some(info),
            )
            .await
            .map(drop)
    }

    async fn commit(&self, tree: &str, label: &Label) -> Result<String, CheckpointError> {
        let kind = label.kind;
        let mut message = format!(
            "Hutan checkpoint ({kind})\n\nHutan-Session: {}\n",
            label.session
        );
        if let Some(turn) = &label.turn {
            message.push_str(&format!("Hutan-Turn: {turn}\n"));
        }
        message.push_str(&format!("Hutan-Kind: {kind}\n"));
        if let Some(checkpoint) = &label.checkpoint {
            message.push_str(&format!("Hutan-Checkpoint: {checkpoint}\n"));
        }
        let head = self
            .user
            .read(&os_args(&["rev-parse", "-q", "--verify", "HEAD"]), None)
            .await?;
        if head.success && super::is_object_id(&head.line()) {
            message.push_str(&format!("{HEAD_TRAILER}: {}\n", head.line()));
        }
        let mut args = vec!["commit-tree", "--no-gpg-sign", "-m", message.as_str()];
        if let Some(parent) = &label.parent {
            args.extend(["-p", parent.as_str()]);
        }
        args.push(tree);
        Ok(self.private.write_ok(&os_args(&args), None).await?.line())
    }
}

/// Whether a `filter` attribute value names a driver (`set` without a name runs none).
fn is_driver(value: &str) -> bool {
    !matches!(value, "unspecified" | "unset" | "set" | "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_named_filters_are_drivers() {
        assert!(is_driver("lfs"));
        for value in ["unspecified", "unset", "set", ""] {
            assert!(!is_driver(value));
        }
    }

    #[test]
    fn suffixes_append_to_the_file_name() {
        assert_eq!(
            with_suffix(Path::new("/s/a.index"), ".lock"),
            PathBuf::from("/s/a.index.lock")
        );
    }
}
