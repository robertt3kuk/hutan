//! Path confinement and the errors of the file service. Blocking I/O: every function here runs
//! on the blocking pool.
//!
//! A request path is made canonical (symbolic links resolved, `..` gone) and must then lie
//! below one of the daemon's roots, or be `picked`. The file is opened from the root's own
//! descriptor with `O_NOFOLLOW` on every component (`context_file::open_beneath`), so a link
//! swapped in after the check fails the open instead of being followed.

use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::fs::{FileVersion, FsEntry, FsKind};
use rustix::fs::{FileType, Mode, Stat};
use serde_json::json;

use crate::context_file::{open_beneath, open_component};

/// What a file operation can fail with, before it becomes an [`ErrorBody`].
#[derive(Debug)]
pub enum FsError {
    NotFound(String),
    /// `forbidden`; `reason` is `outside_roots`, `symlink`, `git_dir` or `permission`.
    Forbidden {
        reason: &'static str,
        message: String,
    },
    /// `unsupported`; `reason` is `binary`, `not_utf8`, `too_large` or `not_a_file`.
    Unsupported {
        reason: &'static str,
        size: Option<u64>,
        message: String,
    },
    /// `conflict`; `reason` is `version`, `exists` or `missing`.
    Conflict {
        reason: &'static str,
        current: Option<FileVersion>,
        message: String,
    },
    BadRequest(String),
    InvalidState(String),
    Io {
        context: String,
        source: io::Error,
    },
}

impl FsError {
    pub fn bad(message: impl Into<String>) -> Self {
        Self::BadRequest(message.into())
    }

    pub fn forbidden(reason: &'static str, message: impl Into<String>) -> Self {
        Self::Forbidden {
            reason,
            message: message.into(),
        }
    }

    pub fn unsupported(reason: &'static str, message: impl Into<String>) -> Self {
        Self::Unsupported {
            reason,
            size: None,
            message: message.into(),
        }
    }

    /// An I/O error of `context` (what was being done, with the path), mapped to the error a
    /// client should see.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        let context = context.into();
        match source.kind() {
            io::ErrorKind::NotFound => Self::NotFound(format!("{context}: not found")),
            io::ErrorKind::PermissionDenied => Self::forbidden(
                "permission",
                format!("{context}: permission denied by the file system"),
            ),
            _ => match source.raw_os_error() {
                // A symbolic link met where `O_NOFOLLOW` forbids one.
                Some(code) if code == libc::ELOOP => Self::forbidden(
                    "symlink",
                    format!("{context}: a symbolic link changed the path while it was opened"),
                ),
                Some(code) if code == libc::ENOTDIR || code == libc::EISDIR => {
                    Self::unsupported("not_a_file", format!("{context}: not a regular file"))
                }
                _ => Self::Io { context, source },
            },
        }
    }
}

impl From<FsError> for ErrorBody {
    fn from(error: FsError) -> Self {
        match error {
            FsError::NotFound(message) => Self::new(ErrorCode::NotFound, message),
            FsError::Forbidden { reason, message } => {
                let mut body = Self::new(ErrorCode::Forbidden, message);
                body.data = Some(json!({ "reason": reason }));
                body
            }
            FsError::Unsupported {
                reason,
                size,
                message,
            } => {
                let mut body = Self::new(ErrorCode::Unsupported, message);
                body.data = Some(match size {
                    Some(size) => json!({ "reason": reason, "size": size }),
                    None => json!({ "reason": reason }),
                });
                body
            }
            FsError::Conflict {
                reason,
                current,
                message,
            } => {
                let mut body = Self::new(ErrorCode::Conflict, message);
                body.data = Some(json!({ "reason": reason, "current": current }));
                body
            }
            FsError::BadRequest(message) => Self::new(ErrorCode::BadRequest, message),
            FsError::InvalidState(message) => Self::new(ErrorCode::InvalidState, message),
            FsError::Io { context, source } => {
                Self::new(ErrorCode::Internal, format!("{context}: {source}"))
            }
        }
    }
}

/// A path the request named, after the checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Canonical.
    pub canonical: PathBuf,
    /// The canonical root the path lies below; `/` for a `picked` path outside every root.
    pub root: PathBuf,
    /// `canonical` relative to `root`: empty for the root itself.
    pub relative: PathBuf,
    /// The path lies below a tracked root (not merely picked).
    pub confined: bool,
}

impl Resolved {
    /// Whether some component of the path below the root is `.git`.
    pub fn in_git_dir(&self) -> bool {
        // A picked path outside the roots has no root to measure from: look at every
        // component of it.
        let path = if self.confined {
            &self.relative
        } else {
            &self.canonical
        };
        path.components()
            .any(|component| matches!(component, Component::Normal(name) if name == ".git"))
    }
}

/// The canonical form of every root that exists, without duplicates.
pub fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut canonical: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    canonical.sort();
    canonical.dedup();
    canonical
}

/// The root of `roots` (canonical) that contains `canonical`, the deepest one.
fn containing<'a>(roots: &'a [PathBuf], canonical: &Path) -> Option<&'a PathBuf> {
    roots
        .iter()
        .filter(|root| canonical.starts_with(root))
        .max_by_key(|root| root.components().count())
}

/// Checks `path` (absolute) against `roots` (canonical) and makes it canonical. The path must
/// exist.
pub fn resolve(path: &Path, roots: &[PathBuf], picked: bool) -> Result<Resolved, FsError> {
    require_absolute(path)?;
    let canonical = path
        .canonicalize()
        .map_err(|e| FsError::io(path.display().to_string(), e))?;
    confine(path, canonical, roots, picked)
}

/// The path of a file that may not exist yet: its directory must (and is made canonical), its
/// name must be one plain component. An existing file is resolved like [`resolve`].
pub fn resolve_for_write(
    path: &Path,
    roots: &[PathBuf],
    picked: bool,
) -> Result<Resolved, FsError> {
    require_absolute(path)?;
    match path.canonicalize() {
        Ok(canonical) => return confine(path, canonical, roots, picked),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(FsError::io(path.display().to_string(), e)),
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(FsError::bad(format!("{} has no file name", path.display())));
    };
    // `..` and `.` as the last component, or a trailing slash, name no new file.
    if !matches!(path.components().next_back(), Some(Component::Normal(_))) {
        return Err(FsError::bad(format!(
            "{} does not name a file",
            path.display()
        )));
    }
    let directory = parent
        .canonicalize()
        .map_err(|e| FsError::io(parent.display().to_string(), e))?;
    confine(path, directory.join(name), roots, picked)
}

fn require_absolute(path: &Path) -> Result<(), FsError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(FsError::bad(format!(
            "{} is not an absolute path",
            path.display()
        )))
    }
}

fn confine(
    asked: &Path,
    canonical: PathBuf,
    roots: &[PathBuf],
    picked: bool,
) -> Result<Resolved, FsError> {
    if let Some(root) = containing(roots, &canonical) {
        let relative = canonical
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        return Ok(Resolved {
            root: root.clone(),
            relative,
            canonical,
            confined: true,
        });
    }
    if picked {
        let relative = canonical
            .strip_prefix("/")
            .map(Path::to_path_buf)
            .unwrap_or_default();
        return Ok(Resolved {
            root: PathBuf::from("/"),
            relative,
            canonical,
            confined: false,
        });
    }
    // Named below a root, but a link leads out of it: say so.
    if leaves_through_a_link(asked, roots) {
        return Err(FsError::forbidden(
            "symlink",
            format!(
                "{} leads outside the workspace roots through a symbolic link",
                asked.display()
            ),
        ));
    }
    Err(FsError::forbidden(
        "outside_roots",
        format!("{} is outside the workspace roots", asked.display()),
    ))
}

/// Whether `asked` enters a root and then meets a symbolic link (which is how it got out).
fn leaves_through_a_link(asked: &Path, roots: &[PathBuf]) -> bool {
    let mut entered = false;
    let mut prefix = PathBuf::new();
    for component in normalize(asked).components() {
        prefix.push(component);
        if !entered {
            entered = prefix
                .canonicalize()
                .is_ok_and(|canonical| containing(roots, &canonical).is_some());
        } else if std::fs::symlink_metadata(&prefix).is_ok_and(|m| m.file_type().is_symlink()) {
            return true;
        }
    }
    false
}

/// `path` with `.` and `..` folded away textually (no file system access).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The root's own descriptor, every component opened without following links.
pub fn open_root(root: &Path) -> Result<File, FsError> {
    open_beneath(root, Path::new("")).map_err(|e| FsError::io(root.display().to_string(), e))
}

/// Opens `relative` below an already open root. The last component is opened non-blocking so
/// that a FIFO named by a path does not hang the daemon.
pub fn open_below(root: &File, relative: &Path) -> io::Result<File> {
    let mut directory = root.try_clone()?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "non-local path component",
            ));
        };
        let flags = if components.peek().is_none() {
            libc::O_NONBLOCK
        } else {
            libc::O_DIRECTORY
        };
        directory = open_component(&directory, name, flags)?;
    }
    Ok(directory)
}

/// Opens the resolved path (file or directory).
pub fn open(resolved: &Resolved) -> Result<File, FsError> {
    open_beneath(&resolved.root, &resolved.relative)
        .map_err(|e| FsError::io(resolved.canonical.display().to_string(), e))
}

/// Opens the directory that holds the resolved path.
pub fn open_parent(resolved: &Resolved) -> Result<File, FsError> {
    let parent = resolved.relative.parent().unwrap_or(Path::new(""));
    open_beneath(&resolved.root, parent)
        .map_err(|e| FsError::io(resolved.canonical.display().to_string(), e))
}

/// The version of a file from its stat.
pub fn version_of(stat: &Stat) -> FileVersion {
    FileVersion {
        mtime_ns: mtime_ns(stat),
        size: u64::try_from(stat.st_size).unwrap_or(0),
        ino: stat.st_ino,
    }
}

pub fn mtime_ns(stat: &Stat) -> i64 {
    let seconds = i64::from(stat.st_mtime);
    seconds
        .saturating_mul(1_000_000_000)
        .saturating_add(i64::from(stat.st_mtime_nsec))
}

pub fn kind_of(stat: &Stat) -> FsKind {
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => FsKind::File,
        FileType::Directory => FsKind::Dir,
        FileType::Symlink => FsKind::Symlink,
        _ => FsKind::Other,
    }
}

pub fn is_regular(stat: &Stat) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
}

/// The permission bits of a stat.
pub fn mode_of(stat: &Stat) -> Mode {
    Mode::from_raw_mode(stat.st_mode) & Mode::from_bits_truncate(0o7777)
}

/// `fstat` of an open file.
pub fn fstat(file: &File, what: &Path) -> Result<Stat, FsError> {
    rustix::fs::fstat(file).map_err(|e| FsError::io(what.display().to_string(), e.into()))
}

/// The entry of `fs.stat` / `fs.list` for `name` in `directory`; a link is described, not
/// followed.
pub fn entry_at(directory: &File, dir_path: &Path, name: &str) -> io::Result<FsEntry> {
    let stat = rustix::fs::statat(directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    Ok(FsEntry {
        name: name.to_owned(),
        path: dir_path.join(name),
        kind: kind_of(&stat),
        size: u64::try_from(stat.st_size).unwrap_or(0),
        mtime_ns: mtime_ns(&stat),
        ino: stat.st_ino,
        ignored: false,
    })
}

/// An owned descriptor of an open file, for `rustix::fs::Dir`.
pub fn into_fd(file: File) -> OwnedFd {
    file.into()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn roots(dir: &Path) -> Vec<PathBuf> {
        canonical_roots(&[dir.to_path_buf()])
    }

    fn reason(error: FsError) -> String {
        match error {
            FsError::Forbidden { reason, .. } => reason.to_owned(),
            other => panic!("not forbidden: {other:?}"),
        }
    }

    #[test]
    fn a_path_below_a_root_resolves_and_anything_else_is_refused() {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "a").unwrap();
        std::fs::write(work.path().join("secret.txt"), "s").unwrap();
        let roots = roots(&repo);

        let ok = resolve(&repo.join("src/a.rs"), &roots, false).unwrap();
        assert_eq!(ok.relative, Path::new("src/a.rs"));
        assert!(ok.confined);
        // `..` that comes back is fine; `..` that leaves is not, existing target or not.
        let back = resolve(&repo.join("src/../src/a.rs"), &roots, false).unwrap();
        assert_eq!(back.relative, Path::new("src/a.rs"));
        assert_eq!(
            reason(resolve(&repo.join("../secret.txt"), &roots, false).unwrap_err()),
            "outside_roots"
        );
        assert_eq!(
            reason(resolve(&repo.join("src/../../secret.txt"), &roots, false).unwrap_err()),
            "outside_roots"
        );
        // Relative paths are not guessed at.
        assert!(matches!(
            resolve(Path::new("src/a.rs"), &roots, false),
            Err(FsError::BadRequest(_))
        ));
    }

    #[test]
    fn symlinks_that_leave_the_roots_are_refused_and_those_that_stay_are_followed() {
        let work = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "a").unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        symlink(outside.path().join("secret"), repo.join("leak")).unwrap();
        symlink(outside.path(), repo.join("leakdir")).unwrap();
        symlink(repo.join("src/a.rs"), repo.join("alias")).unwrap();
        let roots = roots(&repo);

        assert_eq!(
            reason(resolve(&repo.join("leak"), &roots, false).unwrap_err()),
            "symlink"
        );
        assert_eq!(
            reason(resolve(&repo.join("leakdir/secret"), &roots, false).unwrap_err()),
            "symlink"
        );
        let alias = resolve(&repo.join("alias"), &roots, false).unwrap();
        assert_eq!(alias.relative, Path::new("src/a.rs"));
        // `picked` is the user naming the file themselves.
        let picked = resolve(&repo.join("leak"), &roots, true).unwrap();
        assert!(!picked.confined);
        assert_eq!(
            picked.canonical,
            outside.path().canonicalize().unwrap().join("secret")
        );
    }

    #[test]
    fn a_file_to_create_needs_a_directory_below_a_root_and_one_plain_name() {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let roots = roots(&repo);

        let new = resolve_for_write(&repo.join("new.txt"), &roots, false).unwrap();
        assert_eq!(new.relative, Path::new("new.txt"));
        assert!(matches!(
            resolve_for_write(&repo.join("missing/new.txt"), &roots, false),
            Err(FsError::NotFound(_))
        ));
        assert!(matches!(
            resolve_for_write(&repo.join(".."), &roots, false),
            Err(FsError::Forbidden { .. })
        ));
        let outside = work.path().join("elsewhere.txt");
        assert_eq!(
            reason(resolve_for_write(&outside, &roots, false).unwrap_err()),
            "outside_roots"
        );
    }

    #[test]
    fn git_directories_are_recognised_below_the_root_only() {
        let work = tempfile::tempdir().unwrap();
        let repo = work.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/config"), "").unwrap();
        let roots = roots(&repo);
        let inside = resolve(&repo.join(".git/config"), &roots, false).unwrap();
        assert!(inside.in_git_dir());
        let other = resolve(&repo, &roots, false).unwrap();
        assert!(!other.in_git_dir());
    }
}
