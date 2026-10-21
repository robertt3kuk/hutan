//! Bounds of a snapshot (ADR 0019 Decision 6) and the `lstat`-only filters that apply them
//! before any content is read or any object is written.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

const MIB: u64 = 1024 * 1024;

/// The bounds of every snapshot. Defaults are ADR 0019's; `daemon.toml [checkpoints]` sets them
/// from CK-2 on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// A file larger than this is skipped (`too_large`).
    pub max_file_bytes: u64,
    /// New content hashed per snapshot; past it files are skipped largest first (`over_budget`).
    pub snapshot_budget_bytes: u64,
    /// Files hashed per snapshot; past it files are skipped largest first (`too_many_files`).
    pub max_files: usize,
    /// More untracked, not ignored files than this and the checkout is not snapshotted at all.
    pub max_untracked_files: usize,
    /// Less free space than this on the repository's or Hutan's volume switches checkpoints
    /// off for the checkout.
    pub min_free_bytes: u64,
    /// This many failed snapshots in a row switch checkpoints off for the checkout.
    pub max_failures: u32,
    /// File names (not paths) never captured: `*` matches any run of characters, case is
    /// ignored. Likely secrets.
    pub exclude: Vec<String>,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            max_file_bytes: 10 * MIB,
            snapshot_budget_bytes: 64 * MIB,
            max_files: 5_000,
            max_untracked_files: 20_000,
            min_free_bytes: 2 * 1024 * MIB,
            max_failures: 3,
            exclude: DEFAULT_EXCLUDE.iter().map(|p| (*p).to_owned()).collect(),
        }
    }
}

/// ADR 0019 §3.8: names of likely secrets.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    "*.kdbx",
    "credentials*",
];

/// Why a path is not in a snapshot (the domain type of `hutan-core`). Every skip is returned
/// with the snapshot.
pub use hutan_core::checkpoint::SkipReason;

/// A path left out of a snapshot, relative to the checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
    /// Its size, when known from `lstat`.
    pub bytes: Option<u64>,
}

/// What `lstat` says a candidate path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Missing,
    File { bytes: u64, executable: bool },
    Symlink { bytes: u64 },
    Directory,
    Other { bytes: u64 },
}

pub(crate) fn lstat(path: &Path) -> io::Result<Kind> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Kind::Missing),
        // `a/b` where `a` became a file.
        Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => return Ok(Kind::Missing),
        Err(error) => return Err(error),
    };
    let kind = meta.file_type();
    Ok(if kind.is_symlink() {
        Kind::Symlink { bytes: meta.len() }
    } else if kind.is_file() {
        Kind::File {
            bytes: meta.len(),
            executable: meta.permissions().mode() & 0o100 != 0,
        }
    } else if kind.is_dir() {
        Kind::Directory
    } else {
        // A FIFO, socket or device.
        Kind::Other { bytes: meta.len() }
    })
}

/// Whether the daemon may open `path` for reading (checked without opening it).
pub(crate) fn readable(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::READ_OK).is_ok()
}

/// Whether the file name of `path` matches one of `patterns`.
pub(crate) fn excluded(path: &[u8], patterns: &[String]) -> bool {
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    patterns.iter().any(|p| wildcard(p.as_bytes(), name))
}

/// `*`-only glob match, ASCII case-insensitive (the default macOS volume ignores case).
fn wildcard(pattern: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while n < name.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            mark = n;
            p += 1;
        } else if p < pattern.len() && pattern[p].eq_ignore_ascii_case(&name[n]) {
            p += 1;
            n += 1;
        } else if let Some(s) = star {
            p = s + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|b| *b == b'*')
}

/// A file that passed the per-file filters and waits for the snapshot's budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub path: Vec<u8>,
    pub bytes: u64,
    /// `100644`, `100755` or `120000`.
    pub mode: &'static str,
    /// In the private index already (its skip removes the stale entry).
    pub tracked: bool,
}

/// Keeps the smallest candidates within `max_files` and `snapshot_budget_bytes`; the rest is
/// skipped largest first.
pub(crate) fn select(
    mut candidates: Vec<Candidate>,
    caps: &Caps,
) -> (Vec<Candidate>, Vec<(Candidate, SkipReason)>) {
    candidates.sort_by(|a, b| a.bytes.cmp(&b.bytes).then_with(|| a.path.cmp(&b.path)));
    let mut kept = Vec::with_capacity(candidates.len().min(caps.max_files));
    let mut skipped = Vec::new();
    let mut total: u64 = 0;
    for candidate in candidates {
        if kept.len() >= caps.max_files {
            skipped.push((candidate, SkipReason::TooManyFiles));
        } else if total.saturating_add(candidate.bytes) > caps.snapshot_budget_bytes {
            skipped.push((candidate, SkipReason::OverBudget));
        } else {
            total += candidate.bytes;
            kept.push(candidate);
        }
    }
    (kept, skipped)
}

pub(crate) fn path_buf(path: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsStr::from_bytes(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_denylist_matches_names_not_directories() {
        let caps = Caps::default();
        for name in [
            ".env",
            ".env.local",
            "config/.ENV.production",
            "certs/server.pem",
            "a/b/tls.key",
            "id_rsa",
            "id_rsa.pub",
            "home/id_ed25519",
            "vault.kdbx",
            "credentials.json",
            "x.p12",
        ] {
            assert!(excluded(name.as_bytes(), &caps.exclude), "{name}");
        }
        for name in [
            "environment.rs",
            ".envy/x.rs",
            "keys.rs",
            "key",
            "src/credentials/mod.rs",
            "pem.txt",
        ] {
            assert!(!excluded(name.as_bytes(), &caps.exclude), "{name}");
        }
    }

    #[test]
    fn wildcards_backtrack() {
        assert!(wildcard(b"a*b*c", b"axxbyybzc"));
        assert!(wildcard(b"*", b""));
        assert!(!wildcard(b"a*b", b"axxc"));
        assert!(wildcard(b"**x", b"yx"));
    }

    fn candidate(path: &str, bytes: u64) -> Candidate {
        Candidate {
            path: path.as_bytes().to_vec(),
            bytes,
            mode: "100644",
            tracked: false,
        }
    }

    #[test]
    fn selection_skips_the_largest_first() {
        let caps = Caps {
            snapshot_budget_bytes: 1_000,
            max_files: 3,
            ..Caps::default()
        };
        let (kept, skipped) = select(
            vec![
                candidate("big", 700),
                candidate("a", 100),
                candidate("b", 500),
                candidate("c", 1),
                candidate("d", 2),
            ],
            &caps,
        );
        let kept: Vec<_> = kept.iter().map(|c| c.path.clone()).collect();
        assert_eq!(kept, vec![b"c".to_vec(), b"d".to_vec(), b"a".to_vec()]);
        let skipped: Vec<_> = skipped
            .iter()
            .map(|(c, r)| (c.path.clone(), r.clone()))
            .collect();
        assert_eq!(
            skipped,
            vec![
                (b"b".to_vec(), SkipReason::TooManyFiles),
                (b"big".to_vec(), SkipReason::TooManyFiles),
            ]
        );

        let caps = Caps {
            snapshot_budget_bytes: 650,
            ..Caps::default()
        };
        let (kept, skipped) = select(
            vec![candidate("x", 600), candidate("y", 50), candidate("z", 500)],
            &caps,
        );
        assert_eq!(kept.len(), 2);
        assert_eq!(skipped, vec![(candidate("x", 600), SkipReason::OverBudget)]);
    }
}
