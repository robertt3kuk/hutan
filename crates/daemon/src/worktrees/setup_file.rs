//! `.hutan/worktree.toml`: what a new worktree of a repository needs — gitignored files copied
//! from the main checkout, commands run in it, commands run before it is removed — and the user's
//! own override in `daemon.toml` (`[worktrees.projects.<name or id>]`).
//!
//! The file is committed with the repository, so the team shares it. That also means anyone who
//! can push to the repository can put commands in it: the commands of the *file* run only after
//! the user confirmed exactly these (a sha-256 of the lists, remembered in `worktree_setup_trust`;
//! see `setup.rs`). Commands the user wrote themselves in `daemon.toml` are trusted by being
//! theirs. Copying gitignored files needs no confirmation: it only reads inside the repository.
//!
//! ```toml
//! copy = [".env", ".env.local", "config/*.local.json"]   # globs relative to the repository, files only
//! setup = ["pnpm install"]                               # run in the new worktree, in order
//! teardown = ["docker compose down"]                     # run before the worktree is removed
//! ```
//!
//! Unknown keys are warned about, not fatal; a key of the wrong type or a list over its bound is
//! an error that stops the setup (and says why), never half a setup.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};

use crate::config::WorktreeProject;

/// The file, relative to the main checkout.
pub const FILE: &str = ".hutan/worktree.toml";

/// The file is a few lines; a larger one is refused rather than read.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Entries of one list.
const MAX_ENTRIES: usize = 100;
/// One glob or command.
const MAX_ENTRY_CHARS: usize = 4096;

/// Files copied by one setup.
pub const MAX_COPY_FILES: usize = 200;
/// The largest file copied.
pub const MAX_COPY_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// All copied files together.
pub const MAX_COPY_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
/// Directory entries a glob search looks at before it gives up.
const MAX_WALK_ENTRIES: usize = 100_000;

/// What the repository's file says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileConfig {
    pub copy: Vec<String>,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
    /// Unknown keys and the like, for the user.
    pub warnings: Vec<String>,
}

/// What a worktree's setup does: the file with the user's override applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub copy: Vec<String>,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
    /// The file's `setup` is what runs (the user did not replace it).
    pub setup_from_file: bool,
    /// The file's `teardown` is what runs.
    pub teardown_from_file: bool,
    pub warnings: Vec<String>,
}

impl Resolved {
    /// Nothing to copy and nothing to run.
    pub fn is_empty(&self) -> bool {
        self.copy.is_empty() && self.setup.is_empty() && self.teardown.is_empty()
    }

    /// The content hash of the commands that need the user's confirmation — the file's `setup`
    /// and `teardown` that run — as lower-case hex; `None` when every command is the user's own.
    pub fn trust_hash(&self) -> Option<String> {
        let setup: &[String] = if self.setup_from_file {
            &self.setup
        } else {
            &[]
        };
        let teardown: &[String] = if self.teardown_from_file {
            &self.teardown
        } else {
            &[]
        };
        if setup.is_empty() && teardown.is_empty() {
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(b"hutan worktree.toml v1\0setup\0");
        for command in setup {
            hash.update(command.as_bytes());
            hash.update([0]);
        }
        hash.update(b"teardown\0");
        for command in teardown {
            hash.update(command.as_bytes());
            hash.update([0]);
        }
        Some(
            hash.finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }

    /// Applies the user's override over the file: a key the user set replaces the file's.
    pub fn new(file: FileConfig, user: &WorktreeProject) -> Self {
        Self {
            setup_from_file: user.setup.is_none() && !file.setup.is_empty(),
            teardown_from_file: user.teardown.is_none() && !file.teardown.is_empty(),
            copy: user.copy.clone().unwrap_or(file.copy),
            setup: user.setup.clone().unwrap_or(file.setup),
            teardown: user.teardown.clone().unwrap_or(file.teardown),
            warnings: file.warnings,
        }
    }
}

/// Checks one list of `copy`, `setup` or `teardown`: bounded, no empty entry, and for `copy`
/// only patterns that stay inside the repository.
pub fn check_list(key: &str, list: &[String]) -> Result<(), String> {
    if list.len() > MAX_ENTRIES {
        return Err(format!("`{key}` has more than {MAX_ENTRIES} entries"));
    }
    for entry in list {
        if entry.trim().is_empty() {
            return Err(format!("`{key}` has an empty entry"));
        }
        if entry.chars().count() > MAX_ENTRY_CHARS {
            return Err(format!(
                "an entry of `{key}` is longer than {MAX_ENTRY_CHARS} characters"
            ));
        }
        if entry.contains('\0') {
            return Err(format!("an entry of `{key}` contains a NUL byte"));
        }
        if key == "copy" {
            check_glob(entry)?;
        }
    }
    Ok(())
}

fn check_glob(pattern: &str) -> Result<(), String> {
    let path = Path::new(pattern);
    if path.is_absolute()
        || pattern.starts_with('!')
        || pattern.starts_with('~')
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!(
            "`copy` entry `{pattern}` must be a path pattern inside the repository (no leading \
             `/`, `~` or `!`, no `..`)"
        ));
    }
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(drop)
        .map_err(|e| format!("`copy` entry `{pattern}` is not a valid glob: {e}"))
}

/// Parses the text of `.hutan/worktree.toml`.
pub fn parse(text: &str) -> Result<FileConfig, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
    let mut config = FileConfig::default();
    for (key, value) in table {
        let slot = match key.as_str() {
            "copy" => &mut config.copy,
            "setup" => &mut config.setup,
            "teardown" => &mut config.teardown,
            other => {
                config
                    .warnings
                    .push(format!("{FILE}: unknown key `{other}`"));
                continue;
            }
        };
        let toml::Value::Array(items) = value else {
            return Err(format!("{FILE}: `{key}` must be a list of strings"));
        };
        for item in items {
            let toml::Value::String(item) = item else {
                return Err(format!("{FILE}: `{key}` must be a list of strings"));
            };
            slot.push(item);
        }
        check_list(&key, slot).map_err(|e| format!("{FILE}: {e}"))?;
    }
    Ok(config)
}

/// Reads the file of the repository whose main checkout is `main`; absent is an empty config.
pub fn read(main: &Path) -> Result<FileConfig, String> {
    let path = main.join(FILE);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileConfig::default()),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {} KiB",
            path.display(),
            MAX_FILE_BYTES / 1024
        ));
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    parse(&text)
}

// Copying -----------------------------------------------------------------------------------

/// What a copy did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyReport {
    /// Relative paths copied.
    pub copied: Vec<String>,
    /// Files and patterns left out, with the reason.
    pub skipped: Vec<String>,
}

/// The files of `main` that `globs` match, before the gitignore check. Sorted, without
/// duplicates; at most [`MAX_WALK_ENTRIES`] directory entries are looked at.
pub fn candidates(main: &Path, globs: &[String], skipped: &mut Vec<String>) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for pattern in globs {
        let Ok(glob) = GlobBuilder::new(pattern).literal_separator(true).build() else {
            skipped.push(format!("{pattern} (not a valid glob)"));
            continue;
        };
        let mut set = GlobSetBuilder::new();
        set.add(glob);
        let Ok(set) = set.build() else {
            continue;
        };
        let components: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
        let literal = components
            .iter()
            .take_while(|c| !c.contains(['*', '?', '[', ']', '{', '}']))
            .count();
        if literal == components.len() {
            // A plain path: no search.
            let path = components.join("/");
            if main
                .join(&path)
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_file())
            {
                found.insert(path);
            }
            continue;
        }
        let base: PathBuf = components[..literal].iter().collect();
        let deep = pattern.contains("**");
        let depth = if deep {
            None
        } else {
            Some(components.len() - literal)
        };
        walk(main, &base, depth, &set, &mut found, skipped, pattern);
    }
    found.into_iter().collect()
}

fn walk(
    main: &Path,
    base: &Path,
    depth: Option<usize>,
    set: &GlobSet,
    found: &mut BTreeSet<String>,
    skipped: &mut Vec<String>,
    pattern: &str,
) {
    let root = main.join(base);
    if !root.is_dir() {
        return;
    }
    let mut walker = ignore::WalkBuilder::new(&root);
    walker
        .standard_filters(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != OsStr::new(".git"));
    if let Some(depth) = depth {
        walker.max_depth(Some(depth));
    }
    let mut looked_at = 0usize;
    for entry in walker.build().flatten() {
        looked_at += 1;
        if looked_at > MAX_WALK_ENTRIES {
            skipped.push(format!(
                "{pattern} (stopped after looking at {MAX_WALK_ENTRIES} entries)"
            ));
            return;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(main) else {
            continue;
        };
        let text = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if set.is_match(&text) {
            found.insert(text);
        }
    }
}

/// Copies `ignored` (relative paths of gitignored files of `main`) into `dest`, within the bounds.
/// A file that already exists in `dest` is kept; a path under a symlink of `dest` is skipped.
pub fn copy_files(main: &Path, dest: &Path, ignored: &[String], report: &mut CopyReport) {
    let mut total = 0u64;
    for relative in ignored {
        if report.copied.len() >= MAX_COPY_FILES {
            report.skipped.push(format!(
                "{relative} (more than {MAX_COPY_FILES} files; the rest are left out)"
            ));
            continue;
        }
        let from = main.join(relative);
        let to = dest.join(relative);
        let Ok(meta) = from.symlink_metadata() else {
            continue;
        };
        if !meta.file_type().is_file() {
            report
                .skipped
                .push(format!("{relative} (not a regular file)"));
            continue;
        }
        if meta.len() > MAX_COPY_FILE_BYTES {
            report.skipped.push(format!(
                "{relative} (larger than {} MiB)",
                MAX_COPY_FILE_BYTES / (1024 * 1024)
            ));
            continue;
        }
        if total + meta.len() > MAX_COPY_TOTAL_BYTES {
            report.skipped.push(format!(
                "{relative} (the files together would pass {} MiB)",
                MAX_COPY_TOTAL_BYTES / (1024 * 1024)
            ));
            continue;
        }
        // A file the worktree already has is kept, silently: running a setup again is not news.
        if to.symlink_metadata().is_ok() {
            continue;
        }
        if under_symlink(dest, relative) {
            report.skipped.push(format!(
                "{relative} (a folder of the worktree is a symlink)"
            ));
            continue;
        }
        if let Some(parent) = to.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            report.skipped.push(format!("{relative} ({e})"));
            continue;
        }
        match std::fs::copy(&from, &to) {
            Ok(bytes) => {
                total += bytes;
                report.copied.push(relative.clone());
            }
            Err(e) => report.skipped.push(format!("{relative} ({e})")),
        }
    }
}

/// Whether a folder between `dest` and `relative` is a symlink (a tracked link could point the
/// copy outside the worktree).
fn under_symlink(dest: &Path, relative: &str) -> bool {
    let mut path = dest.to_owned();
    let parts: Vec<&str> = relative.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        path.push(part);
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_file_is_read_and_unknown_keys_are_warned_about() {
        let config = parse(
            "copy = [\".env\", \"config/*.local.json\"]\nsetup = [\"pnpm install\"]\n\
             teardown = [\"docker compose down\"]\nretries = 3\n",
        )
        .unwrap();
        assert_eq!(config.copy, strings(&[".env", "config/*.local.json"]));
        assert_eq!(config.setup, strings(&["pnpm install"]));
        assert_eq!(config.teardown, strings(&["docker compose down"]));
        assert_eq!(config.warnings.len(), 1);
        assert!(config.warnings[0].contains("unknown key `retries`"));
        assert_eq!(parse("").unwrap(), FileConfig::default());
    }

    #[test]
    fn a_wrong_type_an_empty_entry_or_a_glob_leaving_the_repository_is_an_error() {
        for bad in [
            "setup = \"pnpm install\"\n",
            "setup = [1]\n",
            "setup = [\"  \"]\n",
            "copy = [\"/etc/passwd\"]\n",
            "copy = [\"../secrets\"]\n",
            "copy = [\"a/../../b\"]\n",
            "copy = [\"~/.ssh/id_rsa\"]\n",
            "copy = [\"[\"]\n",
            "setup = [",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let many = format!("setup = [{}]\n", "\"x\",".repeat(MAX_ENTRIES + 1));
        assert!(parse(&many).unwrap_err().contains("more than"));
    }

    #[test]
    fn the_users_override_replaces_a_key_and_only_the_files_commands_need_trust() {
        let file = FileConfig {
            copy: strings(&[".env"]),
            setup: strings(&["pnpm install"]),
            teardown: strings(&["make clean"]),
            warnings: vec![],
        };
        let plain = Resolved::new(file.clone(), &WorktreeProject::default());
        assert!(plain.setup_from_file && plain.teardown_from_file);
        let hash = plain.trust_hash().unwrap();
        assert_eq!(hash.len(), 64);

        // The user's own setup replaces the file's: only the file's teardown still asks.
        let own_setup = Resolved::new(
            file.clone(),
            &WorktreeProject {
                setup: Some(strings(&["just deps"])),
                ..WorktreeProject::default()
            },
        );
        assert_eq!(own_setup.setup, strings(&["just deps"]));
        assert!(!own_setup.setup_from_file && own_setup.teardown_from_file);
        assert_ne!(own_setup.trust_hash().unwrap(), hash);

        let own_all = Resolved::new(
            file.clone(),
            &WorktreeProject {
                copy: Some(vec![]),
                setup: Some(strings(&["a"])),
                teardown: Some(strings(&["b"])),
            },
        );
        assert!(own_all.copy.is_empty());
        assert_eq!(own_all.trust_hash(), None);

        // A changed command is another hash; the same lists are the same hash.
        let mut changed = file.clone();
        changed.setup = strings(&["pnpm install --frozen-lockfile"]);
        assert_ne!(
            Resolved::new(changed, &WorktreeProject::default()).trust_hash(),
            Some(hash.clone())
        );
        assert_eq!(
            Resolved::new(file, &WorktreeProject::default()).trust_hash(),
            Some(hash)
        );
        // Moving a command from setup to teardown is not the same content.
        let swapped = Resolved::new(
            FileConfig {
                setup: strings(&["make clean"]),
                teardown: strings(&["pnpm install"]),
                ..FileConfig::default()
            },
            &WorktreeProject::default(),
        );
        assert_ne!(swapped.trust_hash(), plain.trust_hash());
    }

    #[test]
    fn candidates_match_globs_without_crossing_folders_and_skip_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for file in [
            ".env",
            ".env.local",
            "config/a.local.json",
            "config/deep/b.local.json",
            "apps/web/.env",
            ".git/config.local.json",
            "notes.txt",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let mut skipped = Vec::new();
        let found = candidates(
            root,
            &strings(&[
                ".env*",
                "config/*.local.json",
                "**/.env",
                "missing",
                "nope/*.x",
            ]),
            &mut skipped,
        );
        assert_eq!(
            found,
            strings(&[".env", ".env.local", "apps/web/.env", "config/a.local.json"])
        );
        assert!(skipped.is_empty(), "{skipped:?}");
        let deep = candidates(root, &strings(&["config/**/*.json"]), &mut skipped);
        assert_eq!(
            deep,
            strings(&["config/a.local.json", "config/deep/b.local.json"])
        );
    }

    #[test]
    fn copying_is_bounded_keeps_existing_files_and_never_follows_a_symlinked_folder() {
        let main = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(main.path().join(".env"), "A=1").unwrap();
        std::fs::write(main.path().join(".env.local"), "B=2").unwrap();
        std::fs::write(
            main.path().join("big.bin"),
            vec![0u8; (MAX_COPY_FILE_BYTES + 1) as usize],
        )
        .unwrap();
        std::fs::create_dir_all(main.path().join("link")).unwrap();
        std::fs::write(main.path().join("link/secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), dest.path().join("link")).unwrap();
        std::fs::write(dest.path().join(".env.local"), "kept").unwrap();

        let mut report = CopyReport::default();
        copy_files(
            main.path(),
            dest.path(),
            &strings(&[".env", ".env.local", "big.bin", "link/secret"]),
            &mut report,
        );
        assert_eq!(report.copied, strings(&[".env"]));
        assert_eq!(
            std::fs::read_to_string(dest.path().join(".env")).unwrap(),
            "A=1"
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join(".env.local")).unwrap(),
            "kept"
        );
        assert!(!outside.path().join("secret").exists());
        assert_eq!(report.skipped.len(), 2, "{:?}", report.skipped);

        // The count is bounded.
        let many: Vec<String> = (0..MAX_COPY_FILES + 5).map(|i| format!("f{i}")).collect();
        for name in &many {
            std::fs::write(main.path().join(name), "x").unwrap();
        }
        let mut report = CopyReport::default();
        copy_files(main.path(), dest.path(), &many, &mut report);
        assert_eq!(report.copied.len(), MAX_COPY_FILES);
        assert_eq!(report.skipped.len(), 5);
    }
}
