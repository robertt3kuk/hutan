//! What a restore reads from the repository, and how it writes the checkout (ADR 0019
//! Decision 8, spike §5.2).
//!
//! Reads: what a turn changed (`diff-tree --raw` of its start and end commits: both entries of
//! each path), the files of a tree at some paths (`ls-tree`), the HEAD a snapshot saw (its
//! `Hutan-Head` trailer) and raw blobs (`cat-file --batch`: no filter runs, so a file comes
//! back byte for byte as it was hashed).
//!
//! Writes ([`Writer`], blocking): confined to the checkout, every directory opened with
//! `O_NOFOLLOW` below the one before (a symbolic link anywhere on the way fails the write), never
//! a path with a `.git` component, never over a symbolic link or a folder. A file is written to
//! a temporary file beside it (created exclusively, executable when its tree entry is), flushed
//! and renamed over the target; a deleted file's folders go when the deletion emptied them.
//! Every path is announced before it changes, so `fs.changed` can say who changed it.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use hutan_core::{Change, FileMode, TreeFile};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::git::{Blobs, nul_records, os_args};
use super::{CheckpointError, Checkpoints, HEAD_TRAILER};

/// Total bytes of paths per `ls-tree` command line (macOS `ARG_MAX` is 1 MiB).
const ARGS_BYTES: usize = 256 * 1024;

/// What changed between two commits, path by path.
#[derive(Debug, Default)]
pub struct TreeChanges {
    /// Relative to the checkout, `/`-separated.
    pub files: BTreeMap<String, Change>,
    /// Paths that are not UTF-8 (lossily printed): a restore cannot name them.
    pub unnamed: Vec<String>,
}

impl Checkpoints {
    /// What differs between the trees of commits `from` and `to` (no rename detection).
    pub async fn tree_changes(
        &self,
        common_dir: &Path,
        from: &str,
        to: &str,
    ) -> Result<TreeChanges, CheckpointError> {
        let output = self
            .repo_git(common_dir)
            .read_ok(&[
                "diff-tree",
                "-r",
                "-z",
                "--raw",
                "--no-renames",
                "--no-abbrev",
                from,
                to,
            ])
            .await?;
        Ok(parse_raw_diff(&output.stdout))
    }

    /// The files of `tree` at `paths` (a directory's files too).
    pub async fn tree_files(
        &self,
        common_dir: &Path,
        tree: &str,
        paths: &[&str],
    ) -> Result<BTreeMap<String, TreeFile>, CheckpointError> {
        let mut files = BTreeMap::new();
        let git = self.repo_git(common_dir);
        let mut start = 0;
        while start < paths.len() {
            let mut end = start;
            let mut size = 0;
            while end < paths.len() && (end == start || size + paths[end].len() < ARGS_BYTES) {
                size += paths[end].len() + 1;
                end += 1;
            }
            let mut args = vec![
                "--literal-pathspecs",
                "ls-tree",
                "-r",
                "-z",
                "--full-tree",
                tree,
                "--",
            ];
            args.extend_from_slice(&paths[start..end]);
            let output = git.read_ok(&args).await?;
            files.extend(parse_ls_tree(&output.stdout));
            start = end;
        }
        Ok(files)
    }

    /// The user's HEAD when the snapshot of `commit` was taken (its `Hutan-Head` trailer);
    /// `None` when it does not say (no commit yet, or a checkpoint of an older daemon).
    pub async fn head_at(
        &self,
        common_dir: &Path,
        commit: &str,
    ) -> Result<Option<String>, CheckpointError> {
        let output = self
            .repo_git(common_dir)
            .read(&os_args(&["cat-file", "commit", commit]), None)
            .await?;
        if !output.success {
            return Ok(None);
        }
        let prefix = format!("{HEAD_TRAILER}: ");
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
            .map(|head| head.trim().to_owned())
            .find(|head| super::is_object_id(head)))
    }

    /// A reader of the repository's blobs.
    pub(crate) fn blobs(&self, common_dir: &Path) -> Result<Blobs, CheckpointError> {
        self.repo_git(common_dir).blobs()
    }
}

fn tree_file(mode: &str, oid: &str) -> Option<TreeFile> {
    let mode = FileMode::from_octal(mode)?;
    Some(TreeFile {
        mode,
        oid: oid.to_owned(),
    })
}

/// `diff-tree -r -z --raw`: `:<mode> <mode> <oid> <oid> <status>\0<path>\0`.
fn parse_raw_diff(bytes: &[u8]) -> TreeChanges {
    let mut changes = TreeChanges::default();
    let mut records = bytes.split(|b| *b == 0);
    while let Some(head) = records.next() {
        let Some(head) = head.strip_prefix(b":") else {
            continue;
        };
        let Some(path) = records.next() else { break };
        let head = String::from_utf8_lossy(head);
        let fields: Vec<&str> = head.split(' ').collect();
        let [old_mode, new_mode, old_oid, new_oid, _status] = fields.as_slice() else {
            continue;
        };
        match std::str::from_utf8(path) {
            Ok(path) => {
                changes.files.insert(
                    path.to_owned(),
                    Change {
                        start: tree_file(old_mode, old_oid),
                        end: tree_file(new_mode, new_oid),
                    },
                );
            }
            Err(_) => changes
                .unnamed
                .push(String::from_utf8_lossy(path).into_owned()),
        }
    }
    changes
}

/// `ls-tree -r -z`: `<mode> <type> <oid>\t<path>\0`; paths that are not UTF-8 are left out.
fn parse_ls_tree(bytes: &[u8]) -> Vec<(String, TreeFile)> {
    nul_records(bytes)
        .filter_map(|record| {
            let tab = record.iter().position(|b| *b == b'\t')?;
            let head = std::str::from_utf8(&record[..tab]).ok()?;
            let path = std::str::from_utf8(&record[tab + 1..]).ok()?;
            let mut fields = head.split(' ');
            let (mode, _kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
            Some((path.to_owned(), tree_file(mode, oid)?))
        })
        .collect()
}

// Writing ------------------------------------------------------------------------------------

/// The components of `path` (relative, `/`-separated): no empty, `.`, `..` or `.git` one.
fn components(path: &str) -> Result<Vec<&OsStr>, String> {
    let parts: Vec<&str> = path.split('/').collect();
    let plain = !path.contains('\0')
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                // Case-insensitive: `.GIT` is the same folder on a default macOS volume.
                && !part.eq_ignore_ascii_case(".git")
        });
    if plain {
        Ok(parts.into_iter().map(OsStr::new).collect())
    } else {
        Err(format!(
            "`{path}` is not a file of the checkout a restore may write (no `.git`, no `..`)"
        ))
    }
}

fn describe(error: impl Into<io::Error>) -> String {
    let error: io::Error = error.into();
    match error.raw_os_error() {
        Some(code)
            if code == Errno::LOOP.raw_os_error() || code == Errno::NOTDIR.raw_os_error() =>
        {
            "a folder on its way is a symbolic link or a file".to_owned()
        }
        _ => error.to_string(),
    }
}

/// The checkout a restore writes, opened once without following links.
#[derive(Debug)]
pub struct Writer {
    root: PathBuf,
    dir: OwnedFd,
}

impl Writer {
    /// Opens the checkout at `root`.
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        let dir = crate::context_file::open_beneath(&root, Path::new(""))?;
        Ok(Self {
            root,
            dir: OwnedFd::from(dir),
        })
    }

    /// The directory `dirs` below the root; missing ones are created when `create`, else
    /// `None`. Created directories are announced.
    fn open_dir(
        &self,
        dirs: &[&OsStr],
        create: bool,
        announce: &dyn Fn(&Path),
    ) -> io::Result<Option<OwnedFd>> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut dir = rustix::io::dup(&self.dir)?;
        let mut walked = PathBuf::new();
        for name in dirs {
            walked.push(name);
            dir = match rustix::fs::openat(&dir, *name, flags, Mode::empty()) {
                Ok(next) => next,
                Err(Errno::NOENT) if create => {
                    announce(&self.root.join(&walked));
                    match rustix::fs::mkdirat(&dir, *name, Mode::from_bits_truncate(0o777)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(error) => return Err(error.into()),
                    }
                    rustix::fs::openat(&dir, *name, flags, Mode::empty())?
                }
                Err(Errno::NOENT) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        }
        Ok(Some(dir))
    }

    /// What is at `name` in `dir` now: `None` when nothing; an error naming what is there when
    /// a restore must not replace or delete it.
    fn existing(dir: &OwnedFd, name: &OsStr) -> Result<Option<()>, String> {
        match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => Ok(Some(())),
                FileType::Symlink => Err("it is a symbolic link now".to_owned()),
                FileType::Directory => Err("it is a folder now".to_owned()),
                _ => Err("it is not a regular file now".to_owned()),
            },
            Err(Errno::NOENT) => Ok(None),
            Err(error) => Err(describe(error)),
        }
    }

    /// Writes `bytes` to `path` atomically, executable when `mode` says so. The reason when it
    /// cannot.
    pub fn write(
        &self,
        path: &str,
        bytes: &[u8],
        mode: FileMode,
        announce: &dyn Fn(&Path),
    ) -> Result<(), String> {
        let parts = components(path)?;
        let Some((name, dirs)) = parts.split_last() else {
            return Err("the path names no file".to_owned());
        };
        if !mode.is_file() {
            return Err("not a regular file".to_owned());
        }
        let dir = self
            .open_dir(dirs, true, announce)
            .map_err(describe)?
            .ok_or_else(|| "its folder could not be made".to_owned())?;
        Self::existing(&dir, name)?;
        let temp = format!(".hutan-write-restore-{}.tmp", ulid::Ulid::generate());
        let permissions = if mode == FileMode::Executable {
            0o777
        } else {
            0o666
        };
        let fd = rustix::fs::openat(
            &dir,
            temp.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(permissions),
        )
        .map_err(describe)?;
        let mut file = File::from(fd);
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        let renamed = written.map_err(describe).and_then(|()| {
            announce(&self.root.join(path));
            rustix::fs::renameat(&dir, temp.as_str(), &dir, *name).map_err(describe)
        });
        if renamed.is_err() {
            let _ = rustix::fs::unlinkat(&dir, temp.as_str(), AtFlags::empty());
        }
        renamed?;
        // The rename is durable once the folder is.
        let _ = rustix::fs::fsync(&dir);
        Ok(())
    }

    /// Deletes the file at `path` (nothing to do when it is gone), then the folders the deletion
    /// emptied. The reason when it cannot.
    pub fn delete(&self, path: &str, announce: &dyn Fn(&Path)) -> Result<(), String> {
        let parts = components(path)?;
        let Some((name, dirs)) = parts.split_last() else {
            return Err("the path names no file".to_owned());
        };
        let Some(dir) = self.open_dir(dirs, false, announce).map_err(describe)? else {
            return Ok(());
        };
        if Self::existing(&dir, name)?.is_none() {
            return Ok(());
        }
        announce(&self.root.join(path));
        rustix::fs::unlinkat(&dir, *name, AtFlags::empty()).map_err(describe)?;
        drop(dir);
        for depth in (1..=dirs.len()).rev() {
            let Ok(Some(parent)) = self.open_dir(&dirs[..depth - 1], false, announce) else {
                break;
            };
            let folder: PathBuf = dirs[..depth].iter().collect();
            match rustix::fs::unlinkat(&parent, dirs[depth - 1], AtFlags::REMOVEDIR) {
                Ok(()) => announce(&self.root.join(folder)),
                // Not empty (something else lives there), or anything else: it stays.
                Err(_) => break,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    #[test]
    fn raw_diffs_and_tree_listings_parse_with_both_entries() {
        let z = "0".repeat(40);
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let raw = format!(
            ":100644 100755 {a} {b} M\0bin/run\0:000000 100644 {z} {b} A\0new.txt\0\
             :120000 000000 {a} {z} D\0link\0"
        );
        let mut bytes = raw.into_bytes();
        bytes.extend_from_slice(format!(":100644 100644 {a} {b} M\0").as_bytes());
        bytes.extend_from_slice(b"odd\xff\0");
        let changes = parse_raw_diff(&bytes);
        assert_eq!(changes.files.len(), 3);
        let run = &changes.files["bin/run"];
        assert_eq!(run.start.as_ref().unwrap().mode, FileMode::Regular);
        assert_eq!(run.end.as_ref().unwrap().mode, FileMode::Executable);
        assert_eq!(changes.files["new.txt"].start, None);
        assert_eq!(changes.files["link"].end, None);
        assert_eq!(changes.unnamed, ["odd\u{fffd}"]);

        let listing = format!("100644 blob {a}\tdir/a b\0120000 blob {b}\tl\0");
        let files = parse_ls_tree(listing.as_bytes());
        assert_eq!(files[0].0, "dir/a b");
        assert_eq!(files[1].1.mode, FileMode::Symlink);
    }

    #[test]
    fn only_plain_paths_outside_git_are_written() {
        for bad in [
            "",
            "a//b",
            "../a",
            "a/../b",
            ".git/config",
            "sub/.GIT/x",
            "./a",
            "/abs",
        ] {
            assert!(components(bad).is_err(), "{bad}");
        }
        assert_eq!(components("a/b.txt").unwrap().len(), 2);
        assert!(components(".gitignore").is_ok());
    }

    #[test]
    fn writes_are_atomic_confined_and_deletes_take_emptied_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let writer = Writer::open(&root).unwrap();
        let seen = RefCell::new(Vec::new());
        let announce = |path: &Path| seen.borrow_mut().push(path.to_owned());

        writer
            .write(
                "deep/er/run.sh",
                b"#!/bin/sh\n",
                FileMode::Executable,
                &announce,
            )
            .unwrap();
        let written = root.join("deep/er/run.sh");
        assert_eq!(std::fs::read(&written).unwrap(), b"#!/bin/sh\n");
        assert_ne!(
            std::fs::metadata(&written).unwrap().permissions().mode() & 0o111,
            0
        );
        assert_eq!(
            *seen.borrow(),
            [root.join("deep"), root.join("deep/er"), written.clone()]
        );
        // No temporary file is left beside it.
        let names: Vec<_> = std::fs::read_dir(root.join("deep/er")).unwrap().collect();
        assert_eq!(names.len(), 1);

        // A folder with something else in it stays.
        std::fs::write(root.join("deep/keep.txt"), "keep").unwrap();
        seen.borrow_mut().clear();
        writer.delete("deep/er/run.sh", &announce).unwrap();
        assert!(!root.join("deep/er").exists());
        assert!(root.join("deep/keep.txt").exists());
        assert_eq!(*seen.borrow(), [written, root.join("deep/er")]);
        writer.delete("deep/er/run.sh", &announce).unwrap();

        // Never through or over a link, never a folder, never under .git.
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join("out")).unwrap();
        assert!(
            writer
                .write("out/x.txt", b"x", FileMode::Regular, &announce)
                .is_err()
        );
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        symlink("deep/keep.txt", root.join("link")).unwrap();
        let refused = writer
            .write("link", b"x", FileMode::Regular, &announce)
            .unwrap_err();
        assert!(refused.contains("symbolic link"), "{refused}");
        assert!(writer.delete("link", &announce).is_err());
        assert!(writer.delete("deep", &announce).is_err());
        assert!(
            writer
                .write(".git/config", b"x", FileMode::Regular, &announce)
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(root.join("deep/keep.txt")).unwrap(),
            "keep"
        );
    }
}
