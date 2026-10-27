//! File operations: `fs.create`, `fs.rename`, `fs.move`, `fs.copy`, `fs.trash` and
//! `fs.restore` (protocol 0.54, capability `fs.ops`; ADR 0012 amendment 2026-10-27). Blocking
//! I/O.
//!
//! Every path is confined like `fs.write`'s ([`confine`]): its folder is made canonical and must
//! lie below a root the daemon tracks, and the folder is then opened from the root's own
//! descriptor with `O_NOFOLLOW` on every component, so the entry is looked up (`statat`,
//! `renameat`, `mkdirat`) in the folder that was checked. The last component is never followed:
//! a symbolic link is renamed, moved, trashed or copied as a link. Beyond `fs.write`:
//! nothing below `.git` (the source or the target), no root itself, rename and move within one
//! root, and nothing is overwritten (`renameat` with `RENAME_EXCL`, `clonefile`, exclusive
//! creation).

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use hutan_protocol::fs::{
    COPY_BYTES_MAX, COPY_ENTRIES_MAX, FsCopyItem, FsCreate, FsCreateKind, FsEntry, FsKind,
};
use rustix::fs::{AtFlags, CWD, Mode, OFlags, RenameFlags, Stat};
use rustix::io::Errno;

use super::confine::{
    FsError, Resolved, confine, entry_at, fstat, kind_of, mode_of, open_below, open_parent,
    require_absolute,
};
use super::trash::Trash;
use crate::context_file::{open_beneath, open_component};

/// Told, before a change, which path it will touch and whether everything below that path
/// changes with it, so that the watcher can attribute the events to the connection.
pub type Announce<'a> = &'a dyn Fn(&Path, bool);

/// An entry the request named, which may not exist: the folder that holds it, checked.
#[derive(Debug)]
pub struct Located {
    pub resolved: Resolved,
    parent: File,
    name: OsString,
}

impl Located {
    fn shown(&self) -> String {
        self.resolved.canonical.display().to_string()
    }

    fn parent_path(&self) -> &Path {
        self.resolved
            .canonical
            .parent()
            .unwrap_or_else(|| Path::new("/"))
    }

    /// What the entry is now (a link itself, not what it points to); `None` when it is not
    /// there.
    fn stat(&self) -> Result<Option<Stat>, FsError> {
        match rustix::fs::statat(&self.parent, &self.name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(stat)),
            Err(Errno::NOENT) => Ok(None),
            Err(e) => Err(io_error(&self.shown(), e)),
        }
    }

    fn existing(&self) -> Result<Stat, FsError> {
        self.stat()?
            .ok_or_else(|| FsError::NotFound(format!("{}: not found", self.shown())))
    }

    fn entry(&self) -> Result<FsEntry, FsError> {
        let name = self
            .name
            .to_str()
            .ok_or_else(|| FsError::bad("a name must be UTF-8"))?;
        entry_at(&self.parent, self.parent_path(), name).map_err(|e| FsError::io(self.shown(), e))
    }

    fn exists_error(&self) -> FsError {
        FsError::Conflict {
            reason: "exists",
            current: None,
            message: format!("{} exists already", self.shown()),
        }
    }
}

/// An error number of a file operation as the error a client should see.
fn io_error(what: &str, errno: Errno) -> FsError {
    match errno {
        Errno::EXIST | Errno::NOTEMPTY => FsError::Conflict {
            reason: "exists",
            current: None,
            message: format!("{what} exists already"),
        },
        Errno::NAMETOOLONG => FsError::bad(format!("{what}: the name is too long")),
        Errno::NOTDIR => FsError::bad(format!("{what}: a part of the path is not a folder")),
        Errno::XDEV => FsError::unsupported(
            "cross_device",
            format!("{what}: the paths are on different volumes"),
        ),
        other => FsError::io(what, other.into()),
    }
}

/// The folder and the name of `path`: the name must be one plain component.
fn split(path: &Path) -> Result<(&Path, &OsStr), FsError> {
    require_absolute(path)?;
    let (Some(parent), Some(Component::Normal(name))) =
        (path.parent(), path.components().next_back())
    else {
        return Err(FsError::bad(format!(
            "{} does not name a file or folder",
            path.display()
        )));
    };
    if name.as_bytes().contains(&0) {
        return Err(FsError::bad("a name must not contain a NUL byte"));
    }
    Ok((parent, name))
}

/// The checks every operation makes of a path whose folder is canonical: below a root (or
/// `picked`), not a root itself, not inside `.git`.
fn check(
    asked: &Path,
    directory: &Path,
    name: &OsStr,
    roots: &[PathBuf],
    picked: bool,
) -> Result<Resolved, FsError> {
    let resolved = confine(asked, directory.join(name), roots, picked)?;
    if roots.contains(&resolved.canonical) {
        return Err(FsError::forbidden(
            "root",
            format!(
                "{} is a workspace root; the file service does not change a root itself",
                resolved.canonical.display()
            ),
        ));
    }
    if resolved.in_git_dir() {
        return Err(FsError::forbidden(
            "git_dir",
            format!(
                "{} is inside a .git directory; the file service does not change it",
                resolved.canonical.display()
            ),
        ));
    }
    Ok(resolved)
}

fn require_folder(directory: &File, what: &Path) -> Result<(), FsError> {
    if kind_of(&fstat(directory, what)?) == FsKind::Dir {
        Ok(())
    } else {
        Err(FsError::bad(format!("{} is not a folder", what.display())))
    }
}

/// Finds the entry `path` names; the folder that holds it must exist.
pub fn locate(path: &Path, roots: &[PathBuf], picked: bool) -> Result<Located, FsError> {
    let (parent, name) = split(path)?;
    let directory = parent
        .canonicalize()
        .map_err(|e| FsError::io(parent.display().to_string(), e))?;
    let resolved = check(path, &directory, name, roots, picked)?;
    let parent = open_parent(&resolved)?;
    require_folder(&parent, &directory)?;
    Ok(Located {
        resolved,
        parent,
        name: name.to_owned(),
    })
}

/// Like [`locate`], and makes the folders of `path` that are missing, below a root, none of them
/// `.git`.
fn locate_creating(
    path: &Path,
    roots: &[PathBuf],
    announce: Announce<'_>,
) -> Result<Located, FsError> {
    let (parent, name) = split(path)?;
    let mut existing = parent;
    let mut missing: Vec<&OsStr> = Vec::new();
    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(canonical) => break canonical,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let (Some(folder), Some(above)) = (existing.file_name(), existing.parent()) else {
                    return Err(FsError::bad(format!("{} cannot be made", path.display())));
                };
                missing.push(folder);
                existing = above;
            }
            Err(e) => return Err(FsError::io(existing.display().to_string(), e)),
        }
    };
    missing.reverse();
    let mut directory = canonical_existing.clone();
    directory.extend(&missing);
    let resolved = check(path, &directory, name, roots, false)?;
    let below_root = canonical_existing
        .strip_prefix(&resolved.root)
        .map_err(|_| {
            FsError::forbidden(
                "outside_roots",
                format!("{} is outside the workspace roots", path.display()),
            )
        })?
        .to_owned();
    let mut folder = open_beneath(&resolved.root, &below_root)
        .map_err(|e| FsError::io(canonical_existing.display().to_string(), e))?;
    let mut made = canonical_existing;
    for component in missing {
        require_folder(&folder, &made)?;
        made.push(component);
        announce(&made, true);
        match rustix::fs::mkdirat(&folder, component, Mode::from_bits_truncate(0o777)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(io_error(&made.display().to_string(), e)),
        }
        folder = open_component(&folder, component, libc::O_DIRECTORY)
            .map_err(|e| FsError::io(made.display().to_string(), e))?;
    }
    require_folder(&folder, &directory)?;
    Ok(Located {
        resolved,
        parent: folder,
        name: name.to_owned(),
    })
}

// create ---------------------------------------------------------------------------------------

/// `fs.create`.
pub fn create(
    roots: &[PathBuf],
    request: &FsCreate,
    announce: Announce<'_>,
) -> Result<FsEntry, FsError> {
    let located = if request.parents {
        locate_creating(&request.path, roots, announce)?
    } else {
        locate(&request.path, roots, false)?
    };
    announce(
        &located.resolved.canonical,
        request.kind == FsCreateKind::Dir,
    );
    let made = match request.kind {
        FsCreateKind::File => rustix::fs::openat(
            &located.parent,
            &located.name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )
        .map(drop),
        FsCreateKind::Dir => rustix::fs::mkdirat(
            &located.parent,
            &located.name,
            Mode::from_bits_truncate(0o777),
        ),
    };
    match made {
        Ok(()) => located.entry(),
        Err(Errno::EXIST) => Err(located.exists_error()),
        Err(e) => Err(io_error(&located.shown(), e)),
    }
}

// rename and move ------------------------------------------------------------------------------

/// `fs.rename`, and an item of `fs.move`: `from` gets the path `to`, in the same root.
pub fn rename(
    roots: &[PathBuf],
    from: &Path,
    to: &Path,
    announce: Announce<'_>,
) -> Result<FsEntry, FsError> {
    let source = locate(from, roots, false)?;
    let source_stat = source.existing()?;
    let target = locate(to, roots, false)?;
    if source.resolved.root != target.resolved.root {
        return Err(FsError::unsupported(
            "cross_root",
            format!(
                "{} and {} are in different roots; copy, then trash the original",
                source.shown(),
                target.shown()
            ),
        ));
    }
    if source.resolved.canonical == target.resolved.canonical {
        return Err(FsError::bad(format!(
            "{} is renamed to itself",
            source.shown()
        )));
    }
    if target
        .resolved
        .canonical
        .starts_with(&source.resolved.canonical)
    {
        return Err(FsError::bad(format!(
            "{} cannot be moved into itself",
            source.shown()
        )));
    }
    let is_dir = kind_of(&source_stat) == FsKind::Dir;
    announce(&source.resolved.canonical, is_dir);
    announce(&target.resolved.canonical, is_dir);
    let exclusive = rustix::fs::renameat_with(
        &source.parent,
        &source.name,
        &target.parent,
        &target.name,
        RenameFlags::NOREPLACE,
    );
    match exclusive {
        Ok(()) => {}
        Err(Errno::EXIST) if case_only(&source, &source_stat, &target)? => {
            // The same file under the same name but for its case (a case-insensitive volume
            // sees the target as taken): a plain rename changes nothing else.
            rustix::fs::renameat(&source.parent, &source.name, &target.parent, &target.name)
                .map_err(|e| io_error(&target.shown(), e))?;
        }
        Err(Errno::EXIST) => return Err(target.exists_error()),
        // The volume cannot rename exclusively: look first, then rename.
        Err(Errno::NOTSUP | Errno::NOSYS | Errno::INVAL) => {
            if target.stat()?.is_some() {
                return Err(target.exists_error());
            }
            rustix::fs::renameat(&source.parent, &source.name, &target.parent, &target.name)
                .map_err(|e| io_error(&target.shown(), e))?;
        }
        Err(e) => return Err(io_error(&target.shown(), e)),
    }
    target.entry()
}

/// Whether `target` is `source` under a name that differs from its own only in case.
fn case_only(source: &Located, source_stat: &Stat, target: &Located) -> Result<bool, FsError> {
    if source.parent_path() != target.parent_path() || source.name == target.name {
        return Ok(false);
    }
    let (a, b) = (
        source.name.to_string_lossy().to_lowercase(),
        target.name.to_string_lossy().to_lowercase(),
    );
    if a != b {
        return Ok(false);
    }
    Ok(target
        .stat()?
        .is_some_and(|stat| stat.st_ino == source_stat.st_ino && stat.st_dev == source_stat.st_dev))
}

// copy -----------------------------------------------------------------------------------------

/// `fs.copy`, one item. `trusted`: the request is not an agent's, so `picked_from` counts.
pub fn copy(
    roots: &[PathBuf],
    item: &FsCopyItem,
    trusted: bool,
    announce: Announce<'_>,
) -> Result<FsEntry, FsError> {
    let source = locate(&item.from, roots, item.picked_from && trusted)?;
    let stat = source.existing()?;
    let target = locate(&item.to, roots, false)?;
    if source.resolved.canonical == target.resolved.canonical || target.stat()?.is_some() {
        return Err(target.exists_error());
    }
    if target
        .resolved
        .canonical
        .starts_with(&source.resolved.canonical)
    {
        return Err(FsError::bad(format!(
            "{} cannot be copied into itself",
            source.shown()
        )));
    }
    let kind = kind_of(&stat);
    match kind {
        FsKind::Symlink => {
            announce(&target.resolved.canonical, false);
            copy_link(&source, &target)?;
        }
        FsKind::File | FsKind::Dir => {
            let is_dir = kind == FsKind::Dir;
            let source_fd = open_component(
                &source.parent,
                &source.name,
                if is_dir {
                    libc::O_DIRECTORY
                } else {
                    libc::O_NONBLOCK
                },
            )
            .map_err(|e| FsError::io(source.shown(), e))?;
            if is_dir {
                survey(&source_fd, &source.shown())?;
            } else {
                check_size(1, u64::try_from(stat.st_size).unwrap_or(0))?;
            }
            announce(&target.resolved.canonical, is_dir);
            match clone(&source_fd, &target) {
                Ok(()) => {}
                Err(Errno::EXIST) => return Err(target.exists_error()),
                Err(Errno::NOTSUP | Errno::XDEV | Errno::NOSYS | Errno::INVAL) => {
                    copy_by_hand(&source_fd, &stat, &source, &target)?;
                }
                Err(e) => return Err(io_error(&target.shown(), e)),
            }
        }
        FsKind::Other => {
            return Err(FsError::unsupported(
                "not_a_file",
                format!("{} is neither a file, a folder nor a link", source.shown()),
            ));
        }
    }
    target.entry()
}

fn check_size(entries: u64, bytes: u64) -> Result<(), FsError> {
    if entries > COPY_ENTRIES_MAX || bytes > COPY_BYTES_MAX {
        return Err(FsError::Unsupported {
            reason: "too_large",
            size: Some(bytes),
            message: format!(
                "{entries} entries and {bytes} bytes are more than a copy takes \
                 ({COPY_ENTRIES_MAX} entries, {COPY_BYTES_MAX} bytes)"
            ),
        });
    }
    Ok(())
}

/// `clonefile` of the open source to the target's place: copy-on-write, atomic for a whole
/// tree, never over an existing entry.
#[cfg(target_os = "macos")]
fn clone(source: &File, target: &Located) -> Result<(), Errno> {
    rustix::fs::fclonefileat(
        source,
        &target.parent,
        &target.name,
        rustix::fs::CloneFlags::empty(),
    )
}

#[cfg(not(target_os = "macos"))]
fn clone(_source: &File, _target: &Located) -> Result<(), Errno> {
    Err(Errno::NOTSUP)
}

/// The names in a folder, at most `max` of them.
fn names(directory: &File, shown: &str, max: u64) -> Result<Vec<OsString>, FsError> {
    // Its own open file description: a clone would share the read position with the caller's.
    let handle = rustix::fs::openat(
        directory,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| FsError::io(shown.to_owned(), e.into()))?;
    let mut reader =
        rustix::fs::Dir::new(handle).map_err(|e| FsError::io(shown.to_owned(), e.into()))?;
    let mut names = Vec::new();
    while let Some(item) = reader.read() {
        let item = item.map_err(|e| FsError::io(shown.to_owned(), e.into()))?;
        let bytes = item.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        names.push(OsString::from_vec(bytes.to_vec()));
        if names.len() as u64 > max {
            return Err(FsError::Unsupported {
                reason: "too_large",
                size: None,
                message: format!(
                    "{shown} holds more than a copy takes ({COPY_ENTRIES_MAX} entries)"
                ),
            });
        }
    }
    Ok(names)
}

/// Counts the entries and bytes below the open folder `root` and refuses what is more than a
/// copy takes.
fn survey(root: &File, shown: &str) -> Result<(), FsError> {
    let (mut entries, mut bytes) = (1_u64, 0_u64);
    let mut pending: Vec<PathBuf> = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let folder = open_below(root, &relative).map_err(|e| FsError::io(shown.to_owned(), e))?;
        for name in names(&folder, shown, COPY_ENTRIES_MAX.saturating_sub(entries))? {
            let stat = rustix::fs::statat(&folder, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(shown, e))?;
            entries += 1;
            match kind_of(&stat) {
                FsKind::Dir => pending.push(relative.join(&name)),
                FsKind::File => {
                    bytes = bytes.saturating_add(u64::try_from(stat.st_size).unwrap_or(0))
                }
                FsKind::Symlink => {}
                FsKind::Other => {
                    return Err(FsError::unsupported(
                        "not_a_file",
                        format!(
                            "{shown}/{}: neither a file, a folder nor a link",
                            relative.join(&name).display()
                        ),
                    ));
                }
            }
            check_size(entries, bytes)?;
        }
    }
    Ok(())
}

/// Copies a link as a link.
fn copy_link(source: &Located, target: &Located) -> Result<(), FsError> {
    let destination = rustix::fs::readlinkat(&source.parent, &source.name, Vec::new())
        .map_err(|e| io_error(&source.shown(), e))?;
    rustix::fs::symlinkat(destination.as_c_str(), &target.parent, &target.name)
        .map_err(|e| io_error(&target.shown(), e))
}

/// The copy where cloning is not possible: files and links one by one, a folder at a time
/// (reopened from the root of the copy, so the number of open descriptors stays small). What
/// was made is removed again when the copy fails.
fn copy_by_hand(
    source_fd: &File,
    stat: &Stat,
    source: &Located,
    target: &Located,
) -> Result<(), FsError> {
    let shown = target.shown();
    if kind_of(stat) == FsKind::File {
        return copy_file(
            &source.parent,
            &target.parent,
            &source.name,
            &target.name,
            stat,
        );
    }
    // A folder: made writable for us first, given its own mode when it is full.
    rustix::fs::mkdirat(&target.parent, &target.name, mode_of(stat) | Mode::RWXU)
        .map_err(|e| io_error(&shown, e))?;
    let outcome = copy_tree(source_fd, target, stat, &shown);
    if outcome.is_err() {
        remove_tree(&target.parent, &target.name);
    }
    outcome
}

fn copy_tree(source_fd: &File, target: &Located, stat: &Stat, shown: &str) -> Result<(), FsError> {
    let destination = open_component(&target.parent, &target.name, libc::O_DIRECTORY)
        .map_err(|e| FsError::io(shown.to_owned(), e))?;
    let mut pending: Vec<PathBuf> = vec![PathBuf::new()];
    let mut finish: Vec<(PathBuf, Mode)> = vec![(PathBuf::new(), mode_of(stat))];
    while let Some(relative) = pending.pop() {
        let from =
            open_below(source_fd, &relative).map_err(|e| FsError::io(shown.to_owned(), e))?;
        let to =
            open_below(&destination, &relative).map_err(|e| FsError::io(shown.to_owned(), e))?;
        for name in names(&from, shown, COPY_ENTRIES_MAX)? {
            let entry = rustix::fs::statat(&from, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(shown, e))?;
            match kind_of(&entry) {
                FsKind::File => copy_file(&from, &to, &name, &name, &entry)?,
                FsKind::Dir => {
                    rustix::fs::mkdirat(&to, &name, mode_of(&entry) | Mode::RWXU)
                        .map_err(|e| io_error(shown, e))?;
                    pending.push(relative.join(&name));
                    finish.push((relative.join(&name), mode_of(&entry)));
                }
                FsKind::Symlink => {
                    let link = rustix::fs::readlinkat(&from, &name, Vec::new())
                        .map_err(|e| io_error(shown, e))?;
                    rustix::fs::symlinkat(link.as_c_str(), &to, &name)
                        .map_err(|e| io_error(shown, e))?;
                }
                FsKind::Other => {
                    return Err(FsError::unsupported(
                        "not_a_file",
                        format!("{shown}: a file of an unsupported kind"),
                    ));
                }
            }
        }
    }
    // Deepest first: a folder that is not writable is closed last.
    for (relative, mode) in finish.into_iter().rev() {
        let folder =
            open_below(&destination, &relative).map_err(|e| FsError::io(shown.to_owned(), e))?;
        rustix::fs::fchmod(&folder, mode).map_err(|e| io_error(shown, e))?;
    }
    Ok(())
}

/// Copies one regular file (`name` in `from`) to `to_name` in `to`, exclusively, with its mode
/// and modification time.
fn copy_file(
    from: &File,
    to: &File,
    name: &OsStr,
    to_name: &OsStr,
    stat: &Stat,
) -> Result<(), FsError> {
    let what = name.to_string_lossy().into_owned();
    let mut reader =
        open_component(from, name, libc::O_NONBLOCK).map_err(|e| FsError::io(what.clone(), e))?;
    let writer = rustix::fs::openat(
        to,
        to_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode_of(stat) | Mode::RUSR | Mode::WUSR,
    )
    .map_err(|e| io_error(&what, e))?;
    let mut writer = File::from(writer);
    let filled = io::copy(&mut reader, &mut writer)
        .map_err(|e| FsError::io(what.clone(), e))
        .and_then(|_| rustix::fs::fchmod(&writer, mode_of(stat)).map_err(|e| io_error(&what, e)));
    if let Err(error) = filled {
        // The file is ours: made just above, exclusively.
        let _ = rustix::fs::unlinkat(to, to_name, AtFlags::empty());
        return Err(error);
    }
    let modified = rustix::fs::Timespec {
        tv_sec: stat.st_mtime,
        tv_nsec: stat.st_mtime_nsec,
    };
    let unchanged = rustix::fs::Timespec {
        tv_sec: 0,
        tv_nsec: rustix::fs::UTIME_OMIT,
    };
    let _ = rustix::fs::futimens(
        &writer,
        &rustix::fs::Timestamps {
            last_access: unchanged,
            last_modification: modified,
        },
    );
    Ok(())
}

/// Removes what a failed copy made: the entry `name` of `directory`, folders with everything
/// in them, links not followed.
fn remove_tree(directory: &File, name: &OsStr) {
    let mut pending: Vec<PathBuf> = vec![PathBuf::from(name)];
    let mut folders: Vec<PathBuf> = Vec::new();
    while let Some(relative) = pending.pop() {
        let Some(leaf) = relative.file_name().map(ToOwned::to_owned) else {
            continue;
        };
        let above = relative.parent().unwrap_or_else(|| Path::new(""));
        let Ok(parent) = open_below(directory, above) else {
            continue;
        };
        match rustix::fs::statat(&parent, &leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if kind_of(&stat) == FsKind::Dir => {
                let _ = rustix::fs::chmodat(&parent, &leaf, Mode::RWXU, AtFlags::SYMLINK_NOFOLLOW);
                if let Ok(folder) = open_component(&parent, &leaf, libc::O_DIRECTORY)
                    && let Ok(listing) = names(&folder, "", u64::MAX)
                {
                    pending.extend(listing.into_iter().map(|child| relative.join(child)));
                }
                folders.push(relative);
            }
            Ok(_) => {
                let _ = rustix::fs::unlinkat(&parent, &leaf, AtFlags::empty());
            }
            Err(_) => {}
        }
    }
    for relative in folders.into_iter().rev() {
        let Some(leaf) = relative.file_name().map(ToOwned::to_owned) else {
            continue;
        };
        if let Ok(parent) = open_below(
            directory,
            relative.parent().unwrap_or_else(|| Path::new("")),
        ) {
            let _ = rustix::fs::unlinkat(&parent, &leaf, AtFlags::REMOVEDIR);
        }
    }
}

// trash and restore ----------------------------------------------------------------------------

/// What `fs.trash` did with one path: the entry as it was, and where it was and is.
#[derive(Debug, Clone)]
pub struct Trashed {
    pub entry: FsEntry,
    pub original: PathBuf,
    pub trashed: PathBuf,
}

/// `fs.trash`, one path.
pub fn trash(
    roots: &[PathBuf],
    path: &Path,
    backend: &dyn Trash,
    announce: Announce<'_>,
) -> Result<Trashed, FsError> {
    let located = locate(path, roots, false)?;
    let stat = located.existing()?;
    let entry = located.entry()?;
    announce(&located.resolved.canonical, kind_of(&stat) == FsKind::Dir);
    let trashed = backend.trash(&located.resolved.canonical)?;
    Ok(Trashed {
        entry,
        original: located.resolved.canonical.clone(),
        trashed,
    })
}

/// `fs.restore`, one item: puts `trashed` back at `original`, never over something.
pub fn restore(
    roots: &[PathBuf],
    original: &Path,
    trashed: &Path,
    announce: Announce<'_>,
) -> Result<FsEntry, FsError> {
    let target = locate(original, roots, false)?;
    let is_dir = match std::fs::symlink_metadata(trashed) {
        Ok(meta) => meta.is_dir(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(FsError::NotFound(format!(
                "{} is no longer in the Trash",
                target.shown()
            )));
        }
        Err(e) => return Err(FsError::io(trashed.display().to_string(), e)),
    };
    if target.stat()?.is_some() {
        return Err(target.exists_error());
    }
    announce(&target.resolved.canonical, is_dir);
    match rustix::fs::renameat_with(
        CWD,
        trashed,
        &target.parent,
        &target.name,
        RenameFlags::NOREPLACE,
    ) {
        Ok(()) => target.entry(),
        Err(Errno::EXIST) => Err(target.exists_error()),
        Err(e) => Err(io_error(&target.shown(), e)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    /// A canonical temporary root.
    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    /// `copy_by_hand` of `from` to `to`, the way `copy` falls back to it.
    fn by_hand(root: &Path, from: &str, to: &str) -> Result<(), FsError> {
        let roots = [root.to_path_buf()];
        let source = locate(&root.join(from), &roots, false)?;
        let stat = source.existing()?;
        let target = locate(&root.join(to), &roots, false)?;
        let is_dir = kind_of(&stat) == FsKind::Dir;
        let fd = open_component(
            &source.parent,
            &source.name,
            if is_dir {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            },
        )
        .unwrap();
        copy_by_hand(&fd, &stat, &source, &target)
    }

    #[test]
    fn copying_by_hand_copies_files_links_and_folders_with_their_modes_and_times() {
        let (_dir, root) = root();
        std::fs::create_dir_all(root.join("src/deep/er")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a").unwrap();
        std::fs::write(root.join("src/deep/er/c.sh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            root.join("src/deep/er/c.sh"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        symlink("a.txt", root.join("src/link")).unwrap();
        symlink("/nowhere", root.join("src/dangling")).unwrap();
        // A folder that is not writable: the copy must still fill it.
        std::fs::set_permissions(
            root.join("src/deep"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        by_hand(&root, "src", "copy").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("copy/a.txt")).unwrap(),
            "a"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("copy/deep/er/c.sh")).unwrap(),
            "#!/bin/sh\n"
        );
        assert_eq!(
            std::fs::metadata(root.join("copy/deep/er/c.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::metadata(root.join("copy/deep"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert_eq!(
            std::fs::read_link(root.join("copy/link")).unwrap(),
            PathBuf::from("a.txt")
        );
        assert_eq!(
            std::fs::read_link(root.join("copy/dangling")).unwrap(),
            PathBuf::from("/nowhere")
        );
        assert_eq!(
            std::fs::metadata(root.join("copy/a.txt")).unwrap().mtime(),
            std::fs::metadata(root.join("src/a.txt")).unwrap().mtime()
        );
        // One file, too.
        by_hand(&root, "src/a.txt", "one.txt").unwrap();
        assert_eq!(std::fs::read_to_string(root.join("one.txt")).unwrap(), "a");
        // Never over something that is there.
        assert!(matches!(
            by_hand(&root, "src/a.txt", "one.txt"),
            Err(FsError::Conflict {
                reason: "exists",
                ..
            })
        ));
        assert_eq!(std::fs::read_to_string(root.join("one.txt")).unwrap(), "a");

        for folder in ["src/deep", "copy/deep"] {
            std::fs::set_permissions(root.join(folder), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
    }

    #[test]
    fn a_copy_by_hand_that_fails_removes_what_it_made() {
        let (_dir, root) = root();
        std::fs::create_dir_all(root.join("src/a/b")).unwrap();
        std::fs::write(root.join("src/a/b/ok.txt"), "ok").unwrap();
        std::fs::write(root.join("src/first.txt"), "first").unwrap();
        // A FIFO is neither a file, a folder nor a link: the copy stops there.
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("src/a/b/pipe"))
                .status()
                .unwrap()
                .success()
        );
        // A read-only folder made on the way is removed as well.
        std::fs::set_permissions(root.join("src/a"), std::fs::Permissions::from_mode(0o555))
            .unwrap();

        let error = by_hand(&root, "src", "copy").unwrap_err();
        assert!(
            matches!(
                error,
                FsError::Unsupported {
                    reason: "not_a_file",
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(
            root.join("copy").symlink_metadata().is_err(),
            "the half-made copy is gone"
        );
        std::fs::set_permissions(root.join("src/a"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }

    #[test]
    fn a_survey_counts_entries_and_bytes_and_stops_at_the_bound() {
        let (_dir, root) = root();
        std::fs::create_dir_all(root.join("t/x")).unwrap();
        std::fs::write(root.join("t/x/f"), "12345").unwrap();
        let roots = [root.clone()];
        let located = locate(&root.join("t"), &roots, false).unwrap();
        let fd = open_component(&located.parent, &located.name, libc::O_DIRECTORY).unwrap();
        survey(&fd, "t").unwrap();
        std::fs::File::create(root.join("t/x/sparse"))
            .unwrap()
            .set_len(COPY_BYTES_MAX + 1)
            .unwrap();
        assert!(matches!(
            survey(&fd, "t"),
            Err(FsError::Unsupported {
                reason: "too_large",
                ..
            })
        ));
    }
}
