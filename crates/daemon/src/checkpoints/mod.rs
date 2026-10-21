//! Checkpoints of agent edits (ADR 0019): the snapshot engine, and the checkpoints of chat
//! turns built on it.
//!
//! A snapshot records the whole working tree of a git checkout (tracked files as they are on
//! disk, untracked files that are not ignored) as a tree commit under a hidden ref of the
//! user's repository: `refs/hutan/checkpoints/<session>/<checkpoint>-start` while a turn runs,
//! then `refs/hutan/checkpoints/<session>/<turn>`, the end commit, whose parent is the start
//! commit (one ref per turn). It goes through Hutan's own index
//! (`<HUTAN_HOME>/checkpoints/<repo>/<worktree>.index`, seeded once from the user's index) and
//! git plumbing only, so the user's index, HEAD, branches, stash and working tree are never
//! written ([`git`] lists the environment of every call).
//!
//! - [`caps`]: the bounds of a snapshot and the `lstat` filters that apply them before
//!   anything is hashed; every skipped path is returned with the reason.
//! - `snapshot`: the algorithm (seed, refresh, candidates, filters, raw hashing, tree, commit,
//!   ref).
//! - `refs`: listing, deleting and packing the hidden refs of a repository.
//! - `retention`: which checkpoints go (count, age, bytes, refs).
//! - [`turns`]: the checkpoints of chat turns: a snapshot when a turn starts and when it ends,
//!   their rows in SQLite, the settings, reconcile at start and retention.
//!
//! Snapshots of one checkout run one at a time; once started a snapshot runs to its end even
//! when the caller stops waiting (a turn's prompt goes to the agent after a budget and the
//! turn is marked `late`). Three failures in a row, or less than 2 GiB free, switch the
//! checkout's checkpoints off until [`Checkpoints::switch_on`].

pub mod caps;
mod git;
mod refs;
mod retention;
mod snapshot;
pub mod turns;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

pub use caps::{Caps, SkipReason, Skipped};

/// The namespace of every checkpoint ref.
pub const REF_PREFIX: &str = "refs/hutan/checkpoints/";

/// The store's directory name under `HUTAN_HOME` ([`Config::store`]).
pub const STORE_DIR: &str = "checkpoints";

/// How long a read-only git step may take before it is killed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// The engine's settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// Where the private indexes live: `<HUTAN_HOME>/`[`STORE_DIR`].
    pub store: PathBuf,
    /// The user's home directory: a checkout there is not snapshotted.
    pub user_home: Option<PathBuf>,
    pub caps: Caps,
    pub read_timeout: Duration,
    /// Extra environment of every git call, under the engine's own (tests point
    /// `GIT_CONFIG_GLOBAL` at an empty file).
    pub git_env: BTreeMap<String, String>,
}

impl Config {
    /// Defaults under `store`.
    pub fn new(store: PathBuf, user_home: Option<PathBuf>) -> Self {
        Self {
            store,
            user_home,
            caps: Caps::default(),
            read_timeout: READ_TIMEOUT,
            git_env: BTreeMap::new(),
        }
    }
}

/// What a snapshot records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A turn before its prompt reaches the agent: ref `<name>-start`.
    Start,
    /// A turn when it ended: ref `<name>`, its parent the start commit.
    End,
    /// No commit and no ref: the private index and the object store catch up with the
    /// checkout (the first snapshot of a checkout, taken when a chat opens).
    Warm,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
            Self::Warm => "warm",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a snapshot is of: its ref name and commit trailers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// The session (a ref name component: ASCII letters, digits, `-`, `_`).
    pub session: String,
    /// The ref's last component (same characters): the turn, or the checkpoint while the
    /// agent has not named the turn yet.
    pub name: String,
    /// The `Hutan-Turn` trailer.
    pub turn: Option<String>,
    /// The `Hutan-Checkpoint` trailer (same characters).
    pub checkpoint: Option<String>,
    pub kind: Kind,
    /// The commit this one follows (an end snapshot's start).
    pub parent: Option<String>,
    /// An end whose tree is this one changed nothing: no commit and no ref are written.
    pub unchanged_tree: Option<String>,
}

impl Label {
    /// A snapshot of `turn`, named by it.
    pub fn new(session: impl Into<String>, turn: impl Into<String>, kind: Kind) -> Self {
        let turn = turn.into();
        Self {
            session: session.into(),
            name: turn.clone(),
            turn: Some(turn),
            checkpoint: None,
            kind,
            parent: None,
            unchanged_tree: None,
        }
    }

    /// The start of a turn the agent has not named yet: named by its checkpoint.
    pub fn before_turn(session: impl Into<String>, checkpoint: impl Into<String>) -> Self {
        let checkpoint = checkpoint.into();
        Self {
            session: session.into(),
            name: checkpoint.clone(),
            turn: None,
            checkpoint: Some(checkpoint),
            kind: Kind::Start,
            parent: None,
            unchanged_tree: None,
        }
    }

    /// A [`Kind::Warm`] snapshot for `session`.
    pub fn warm(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            name: "warm".to_owned(),
            turn: None,
            checkpoint: None,
            kind: Kind::Warm,
            parent: None,
            unchanged_tree: None,
        }
    }

    #[must_use]
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    #[must_use]
    pub fn with_checkpoint(mut self, checkpoint: impl Into<String>) -> Self {
        self.checkpoint = Some(checkpoint.into());
        self
    }

    /// An end that writes nothing when its tree is `tree` (the start's).
    #[must_use]
    pub fn unless_tree(mut self, tree: impl Into<String>) -> Self {
        self.unchanged_tree = Some(tree.into());
        self
    }

    /// `refs/hutan/checkpoints/<session>/<name>-start` for a start, `.../<name>` for an end,
    /// none for a warm-up.
    pub fn reference(&self) -> Option<String> {
        match self.kind {
            Kind::Start => Some(format!("{REF_PREFIX}{}/{}-start", self.session, self.name)),
            Kind::End => Some(format!("{REF_PREFIX}{}/{}", self.session, self.name)),
            Kind::Warm => None,
        }
    }

    fn validate(&self) -> Result<(), CheckpointError> {
        let names = [
            ("session", Some(&self.session)),
            ("name", Some(&self.name)),
            ("turn", self.turn.as_ref()),
            ("checkpoint", self.checkpoint.as_ref()),
        ];
        for (what, value) in names {
            let Some(value) = value else { continue };
            if !is_ref_component(value) {
                return Err(CheckpointError::InvalidLabel(format!(
                    "{what} `{value}` is not 1–128 ASCII letters, digits, `-` or `_`"
                )));
            }
        }
        for (what, value) in [
            ("parent", &self.parent),
            ("unchanged tree", &self.unchanged_tree),
        ] {
            if let Some(oid) = value
                && !is_object_id(oid)
            {
                return Err(CheckpointError::InvalidLabel(format!(
                    "{what} `{oid}` is not an object id"
                )));
            }
        }
        Ok(())
    }
}

/// 1–128 ASCII letters, digits, `-` or `_`: safe in a ref name and a trailer.
fn is_ref_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A SHA-1 or SHA-256 object id in hex.
pub(crate) fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A snapshot that was taken.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The checkout's top level.
    pub checkout: PathBuf,
    /// The repository's common git directory (where the objects and refs are).
    pub common_dir: PathBuf,
    /// The ref written ([`Label::reference`]); `None` for a warm-up and for an end that
    /// changed nothing.
    pub reference: Option<String>,
    /// The commit written, when one was.
    pub commit: Option<String>,
    pub tree: String,
    pub parent: Option<String>,
    /// Files whose content was hashed into the object store (new or changed since the
    /// checkout's previous snapshot); objects that existed are reused, not written again.
    pub files_hashed: usize,
    /// Their size.
    pub bytes_hashed: u64,
    /// Paths dropped since the previous snapshot (deleted, now ignored, now skipped).
    pub files_removed: usize,
    /// Every path left out, with the reason.
    pub skipped: Vec<Skipped>,
    /// Whether the private index was seeded from the user's index (or HEAD) for this one.
    pub seeded: bool,
    /// When the checkout's lock was taken and released: snapshots of one checkout never
    /// overlap.
    pub started: Instant,
    pub finished: Instant,
}

/// Why a checkout cannot have checkpoints (ADR 0019 Decision 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// `/`, a volume root or the user's home directory.
    Root(PathBuf),
    /// A bare repository, or a directory inside `.git`.
    NoWorkTree,
    /// More untracked, not ignored files than `max_untracked_files`.
    TooManyUntracked { count: usize, limit: usize },
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root(path) => write!(
                f,
                "{} is the home directory or a volume root",
                path.display()
            ),
            Self::NoWorkTree => f.write_str("the repository has no working tree"),
            Self::TooManyUntracked { count, limit } => write!(
                f,
                "{count} untracked files (more than {limit}); ignore build output in .gitignore"
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("{} is not in a git repository", path.display())]
    NotARepository { path: PathBuf },
    #[error("checkpoints are not supported here: {0}")]
    Unsupported(Unsupported),
    #[error("invalid checkpoint label: {0}")]
    InvalidLabel(String),
    #[error("git is not installed (no `git` on the daemon's PATH)")]
    GitMissing,
    #[error("git {step} failed: {message}")]
    GitFailed { step: String, message: String },
    #[error("git {step} timed out after {}s", after.as_secs())]
    TimedOut { step: String, after: Duration },
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
    /// Less free space than `min_free_bytes`; checkpoints of the checkout are off from now on.
    #[error("only {} MiB free on {} (checkpoints need {} MiB); checkpoints are off for this checkout", free_bytes / (1024 * 1024), volume.display(), required / (1024 * 1024))]
    LowDisk {
        volume: PathBuf,
        free_bytes: u64,
        required: u64,
    },
    /// Checkpoints of the checkout were switched off by earlier failures or low disk.
    #[error("checkpoints are off for this checkout: {reason}")]
    SwitchedOff { reason: String },
    #[error("snapshot task failed: {0}")]
    Internal(String),
}

impl CheckpointError {
    /// Whether this failure counts towards switching the checkout's checkpoints off (an
    /// unsupported checkout or a bad label is not the checkout failing).
    fn counts(&self) -> bool {
        matches!(
            self,
            Self::GitFailed { .. } | Self::TimedOut { .. } | Self::Io { .. } | Self::Internal(_)
        )
    }
}

/// The snapshot engine of the daemon. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Checkpoints {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    config: Config,
    /// One lock per checkout top level; it also holds the checkout's failure state. Grows by
    /// one small entry per checkout ever snapshotted.
    checkouts: parking_lot::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<Health>>>>,
}

#[derive(Debug, Default)]
struct Health {
    failures: u32,
    off: Option<String>,
    memory: snapshot::Memory,
}

/// The checkout as git sees it.
#[derive(Debug, Clone)]
pub(crate) struct Repo {
    pub toplevel: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    /// `sha1` or `sha256`.
    pub object_format: String,
}

impl Repo {
    /// The empty tree's id in this repository's object format.
    pub fn empty_tree(&self) -> &'static str {
        if self.object_format == "sha256" {
            "6ef19b41225c5369f1c104d45d8d85efa9b057b53b14b4b9b939dd74decc5321"
        } else {
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        }
    }

    /// The all-zero object id (removes an entry in `update-index --index-info`).
    pub fn null_oid(&self) -> String {
        "0".repeat(self.empty_tree().len())
    }
}

impl Checkpoints {
    pub fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                checkouts: parking_lot::Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Snapshots the checkout containing `checkout` under `label`, within the engine's caps.
    ///
    /// Runs on its own task: dropping the returned future stops the wait, not the snapshot.
    pub async fn snapshot(
        &self,
        checkout: &Path,
        label: Label,
    ) -> Result<Snapshot, CheckpointError> {
        let caps = self.inner.config.caps.clone();
        self.snapshot_with(checkout, label, caps).await
    }

    /// [`snapshot`](Self::snapshot) within `caps` (the settings of the moment, per project).
    pub async fn snapshot_with(
        &self,
        checkout: &Path,
        label: Label,
        caps: Caps,
    ) -> Result<Snapshot, CheckpointError> {
        let inner = Arc::clone(&self.inner);
        let checkout = checkout.to_owned();
        tokio::spawn(async move { inner.snapshot(&checkout, &label, &caps).await })
            .await
            .map_err(|error| CheckpointError::Internal(error.to_string()))?
    }

    /// The checkout containing `path`: its top level and the repository's common git
    /// directory.
    pub async fn repository(&self, path: &Path) -> Result<(PathBuf, PathBuf), CheckpointError> {
        let repo = self.inner.resolve(path).await?;
        Ok((repo.toplevel, repo.common_dir))
    }

    /// Why checkpoints of the checkout containing `checkout` are off, if they are.
    pub async fn switched_off(&self, checkout: &Path) -> Result<Option<String>, CheckpointError> {
        let repo = self.inner.resolve(checkout).await?;
        let lock = self.inner.lock_for(&repo.toplevel);
        let health = lock.lock().await;
        Ok(health.off.clone())
    }

    /// Switches checkpoints of the checkout containing `checkout` back on and forgets its
    /// failures.
    pub async fn switch_on(&self, checkout: &Path) -> Result<(), CheckpointError> {
        let repo = self.inner.resolve(checkout).await?;
        let lock = self.inner.lock_for(&repo.toplevel);
        let mut health = lock.lock().await;
        health.failures = 0;
        health.off = None;
        Ok(())
    }
}

impl Inner {
    fn git(&self, cwd: &Path) -> git::Git {
        git::Git::new(cwd, &self.config.git_env, self.config.read_timeout)
    }

    fn lock_for(&self, toplevel: &Path) -> Arc<tokio::sync::Mutex<Health>> {
        Arc::clone(
            self.checkouts
                .lock()
                .entry(toplevel.to_owned())
                .or_default(),
        )
    }

    async fn resolve(&self, checkout: &Path) -> Result<Repo, CheckpointError> {
        let not_a_repo = || CheckpointError::NotARepository {
            path: checkout.to_owned(),
        };
        if !checkout.is_dir() {
            return Err(not_a_repo());
        }
        let args = [
            "rev-parse",
            "--path-format=absolute",
            "--absolute-git-dir",
            "--git-common-dir",
            "--show-object-format",
            "--is-bare-repository",
            "--show-toplevel",
        ];
        let output = self.git(checkout).read(&git::os_args(&args), None).await?;
        let text = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = text.lines().collect();
        if !output.success || lines.len() < 5 {
            // The first four print before `--show-toplevel` fails outside a working tree.
            return Err(if lines.len() >= 4 {
                CheckpointError::Unsupported(Unsupported::NoWorkTree)
            } else {
                not_a_repo()
            });
        }
        if lines[3] == "true" {
            return Err(CheckpointError::Unsupported(Unsupported::NoWorkTree));
        }
        Ok(Repo {
            git_dir: PathBuf::from(lines[0]),
            common_dir: PathBuf::from(lines[1]),
            object_format: lines[2].to_owned(),
            toplevel: PathBuf::from(lines[4]),
        })
    }

    /// `/`, a volume root, the home directory.
    fn refused_root(&self, toplevel: &Path) -> bool {
        let home = self
            .config
            .user_home
            .as_ref()
            .and_then(|h| std::fs::canonicalize(h).ok());
        toplevel.parent().is_none()
            || toplevel.parent() == Some(Path::new("/Volumes"))
            || home.as_deref() == Some(toplevel)
    }

    /// `<store>/<repo>/<worktree>.index`: one private index per checkout, named by hashes of
    /// the common and the checkout's own git directory.
    fn private_index(&self, repo: &Repo) -> PathBuf {
        self.config
            .store
            .join(short_hash(&repo.common_dir))
            .join(format!("{}.index", short_hash(&repo.git_dir)))
    }

    async fn snapshot(
        &self,
        checkout: &Path,
        label: &Label,
        caps: &Caps,
    ) -> Result<Snapshot, CheckpointError> {
        label.validate()?;
        let repo = self.resolve(checkout).await?;
        if self.refused_root(&repo.toplevel) {
            return Err(CheckpointError::Unsupported(Unsupported::Root(
                repo.toplevel,
            )));
        }
        let lock = self.lock_for(&repo.toplevel);
        let mut health = lock.lock().await;
        let started = Instant::now();
        if let Some(reason) = &health.off {
            return Err(CheckpointError::SwitchedOff {
                reason: reason.clone(),
            });
        }
        let result = match self.check_disk(&repo, caps) {
            Ok(()) => {
                let index = self.private_index(&repo);
                let git = self.git(&repo.toplevel);
                snapshot::take(
                    &git,
                    &repo,
                    &index,
                    caps,
                    label,
                    &mut health.memory,
                )
                .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(parts) => {
                health.failures = 0;
                Ok(parts.into_snapshot(&repo, label, started, Instant::now()))
            }
            Err(error) => {
                if let CheckpointError::LowDisk { .. } = &error {
                    health.off = Some(error.to_string());
                } else if error.counts() {
                    health.failures += 1;
                    if health.failures >= caps.max_failures {
                        health.off = Some(format!(
                            "{} snapshots in a row failed, the last with: {error}",
                            health.failures
                        ));
                    }
                }
                tracing::warn!(checkout = %repo.toplevel.display(), %error, "checkpoint snapshot failed");
                Err(error)
            }
        }
    }

    /// Fails when the repository's or Hutan's volume has less than `min_free_bytes` free.
    fn check_disk(&self, repo: &Repo, caps: &Caps) -> Result<(), CheckpointError> {
        let required = caps.min_free_bytes;
        let store = existing_ancestor(&self.config.store);
        for volume in [repo.common_dir.as_path(), store] {
            let stats = rustix::fs::statvfs(volume).map_err(|errno| CheckpointError::Io {
                what: format!("free space of {}", volume.display()),
                source: errno.into(),
            })?;
            let free = stats.f_bavail.saturating_mul(stats.f_frsize);
            if free < required {
                return Err(CheckpointError::LowDisk {
                    volume: volume.to_owned(),
                    free_bytes: free,
                    required,
                });
            }
        }
        Ok(())
    }
}

/// `path` or its nearest existing ancestor (the store may not exist yet).
fn existing_ancestor(path: &Path) -> &Path {
    path.ancestors()
        .find(|p| p.exists())
        .unwrap_or_else(|| Path::new("/"))
}

/// 16 hex digits of SHA-256 over the path's bytes.
fn short_hash(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_make_hidden_refs_and_reject_what_a_ref_cannot_hold() {
        let label = Label::new("01JAB", "7", Kind::End);
        assert_eq!(
            label.reference().as_deref(),
            Some("refs/hutan/checkpoints/01JAB/7")
        );
        assert_eq!(
            Label::before_turn("01JAB", "01CP").reference().as_deref(),
            Some("refs/hutan/checkpoints/01JAB/01CP-start")
        );
        assert_eq!(Label::warm("01JAB").reference(), None);
        assert!(label.validate().is_ok());
        for (session, turn) in [
            ("", "1"),
            ("a/b", "1"),
            ("a", ".."),
            ("a", "x.lock"),
            ("a b", "1"),
        ] {
            assert!(
                Label::new(session, turn, Kind::Start).validate().is_err(),
                "{session:?} {turn:?}"
            );
        }
        assert!(
            Label::new("s", "1", Kind::End)
                .with_parent("xyz")
                .validate()
                .is_err()
        );
        assert!(
            Label::new("s", "1", Kind::End)
                .with_parent("4b825dc642cb6eb9a060e54bf8d69288fbee4904")
                .validate()
                .is_ok()
        );
        assert!(
            Label::new("s", "1", Kind::End)
                .with_checkpoint("a.b")
                .validate()
                .is_err()
        );
        assert!(
            Label::new("s", "1", Kind::End)
                .unless_tree("tree")
                .validate()
                .is_err()
        );
    }
}
