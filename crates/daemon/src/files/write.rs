//! Atomic writes: `fs.write.begin` / `.chunk` / `.commit`. Blocking I/O.
//!
//! The text goes into a temporary file in the target's directory (created exclusively, never
//! through a link), is flushed to disk, and replaces the target with one `renameat`, so a
//! reader sees the old file or the new one, never half of it, and a crash leaves the old one.
//! The directory is held as a descriptor opened without following links, so the temporary
//! file and the target are looked up in the directory that was checked.

use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use hutan_protocol::fs::{CHUNK_MAX_BYTES, FILE_MAX_BYTES, FileVersion, FsWriteBegin};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::confine::{
    FsError, is_regular, kind_of, mode_of, open_parent, resolve_for_write, version_of,
};

/// An upload in progress. Dropping it without committing removes the temporary file.
#[derive(Debug)]
pub struct Upload {
    directory: File,
    name: OsString,
    temp_name: String,
    temp: File,
    /// The canonical path the commit writes.
    target: PathBuf,
    expect: Option<FileVersion>,
    create: bool,
    received: u64,
    next_seq: u32,
    /// The temporary file is gone (renamed over the target).
    finished: bool,
}

impl Upload {
    /// Checks the request and creates the temporary file. `roots` are canonical; `picked` is
    /// whether a picked path is allowed at all (not for an agent).
    pub fn begin(
        roots: &[PathBuf],
        request: &FsWriteBegin,
        picked_allowed: bool,
        id: &str,
    ) -> Result<Self, FsError> {
        if request.create && request.expect.is_some() {
            return Err(FsError::bad(
                "`create` and `expect` exclude each other: a new file has no version",
            ));
        }
        if let Some(size) = request.size_hint
            && size > FILE_MAX_BYTES
        {
            return Err(FsError::Unsupported {
                reason: "too_large",
                size: Some(size),
                message: format!(
                    "{size} bytes are more than the {FILE_MAX_BYTES} the file service writes"
                ),
            });
        }
        let resolved = resolve_for_write(&request.path, roots, request.picked && picked_allowed)?;
        if resolved.in_git_dir() {
            return Err(FsError::forbidden(
                "git_dir",
                format!(
                    "{} is inside a .git directory; the file service does not write there",
                    resolved.canonical.display()
                ),
            ));
        }
        let directory = open_parent(&resolved)?;
        let name = resolved
            .canonical
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| FsError::bad("the path names no file"))?;
        let temp_name = format!(".hutan-write-{id}.tmp");
        let temp = rustix::fs::openat(
            &directory,
            &temp_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o666),
        )
        .map_err(|e| FsError::io(resolved.canonical.display().to_string(), e.into()))?;
        let upload = Self {
            directory,
            name,
            temp_name,
            temp: File::from(temp),
            target: resolved.canonical,
            expect: request.expect,
            create: request.create,
            received: 0,
            next_seq: 0,
            finished: false,
        };
        // Checked before anything is sent, and again at the commit.
        upload.check_target()?;
        Ok(upload)
    }

    /// The resolved target of the upload.
    pub fn target(&self) -> &PathBuf {
        &self.target
    }

    /// What the target is now and whether the request's preconditions hold.
    fn check_target(&self) -> Result<Option<rustix::fs::Stat>, FsError> {
        let shown = self.target.display();
        match rustix::fs::statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                if kind_of(&stat) == hutan_protocol::fs::FsKind::Symlink {
                    return Err(FsError::forbidden(
                        "symlink",
                        format!("{shown} became a symbolic link"),
                    ));
                }
                if !is_regular(&stat) {
                    return Err(FsError::unsupported(
                        "not_a_file",
                        format!("{shown} is not a regular file"),
                    ));
                }
                let current = version_of(&stat);
                if self.create {
                    return Err(FsError::Conflict {
                        reason: "exists",
                        current: Some(current),
                        message: format!("{shown} exists already"),
                    });
                }
                if let Some(expect) = self.expect
                    && expect != current
                {
                    return Err(FsError::Conflict {
                        reason: "version",
                        current: Some(current),
                        message: format!("{shown} changed on disk since it was read"),
                    });
                }
                Ok(Some(stat))
            }
            Err(Errno::NOENT) => {
                if self.create {
                    Ok(None)
                } else if self.expect.is_some() {
                    Err(FsError::Conflict {
                        reason: "missing",
                        current: None,
                        message: format!("{shown} was deleted since it was read"),
                    })
                } else {
                    Err(FsError::NotFound(format!("{shown} does not exist")))
                }
            }
            Err(e) => Err(FsError::io(shown.to_string(), e.into())),
        }
    }

    /// Appends the next chunk.
    pub fn append(&mut self, seq: u32, text: &str) -> Result<(), FsError> {
        if seq != self.next_seq {
            return Err(FsError::bad(format!(
                "chunk {seq} arrived, chunk {} is next",
                self.next_seq
            )));
        }
        if text.len() > CHUNK_MAX_BYTES {
            return Err(FsError::bad(format!(
                "a chunk holds at most {CHUNK_MAX_BYTES} bytes, this one {}",
                text.len()
            )));
        }
        let total = self.received + text.len() as u64;
        if total > FILE_MAX_BYTES {
            return Err(FsError::Unsupported {
                reason: "too_large",
                size: Some(total),
                message: format!("a file of more than {FILE_MAX_BYTES} bytes cannot be written"),
            });
        }
        self.temp
            .write_all(text.as_bytes())
            .map_err(|e| FsError::io(self.target.display().to_string(), e))?;
        self.received = total;
        self.next_seq += 1;
        Ok(())
    }

    /// Makes the written text the file. Returns its path and version.
    pub fn commit(mut self, total_bytes: u64) -> Result<(PathBuf, FileVersion), FsError> {
        let shown = self.target.display().to_string();
        if total_bytes != self.received {
            return Err(FsError::bad(format!(
                "the commit says {total_bytes} bytes, the chunks add up to {}",
                self.received
            )));
        }
        self.temp
            .flush()
            .map_err(|e| FsError::io(shown.clone(), e))?;
        rustix::fs::fsync(&self.temp).map_err(|e| FsError::io(shown.clone(), e.into()))?;
        let existing = self.check_target()?;
        if let Some(stat) = &existing {
            // The file keeps its permissions (the temporary file starts with the umask's).
            rustix::fs::fchmod(&self.temp, mode_of(stat))
                .map_err(|e| FsError::io(shown.clone(), e.into()))?;
        }
        if self.create {
            // Never over a file that appeared meanwhile.
            match rustix::fs::linkat(
                &self.directory,
                &self.temp_name,
                &self.directory,
                &self.name,
                AtFlags::empty(),
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => {
                    let current =
                        rustix::fs::statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)
                            .ok()
                            .map(|stat| version_of(&stat));
                    return Err(FsError::Conflict {
                        reason: "exists",
                        current,
                        message: format!("{shown} exists already"),
                    });
                }
                Err(e) => return Err(FsError::io(shown, e.into())),
            }
        } else {
            rustix::fs::renameat(
                &self.directory,
                &self.temp_name,
                &self.directory,
                &self.name,
            )
            .map_err(|e| FsError::io(shown.clone(), e.into()))?;
            self.finished = true;
        }
        // The rename is durable once the directory is.
        rustix::fs::fsync(&self.directory).map_err(|e| FsError::io(shown.clone(), e.into()))?;
        let stat = rustix::fs::statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| FsError::io(shown, e.into()))?;
        Ok((self.target.clone(), version_of(&stat)))
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.finished {
            let _ = rustix::fs::unlinkat(&self.directory, &self.temp_name, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::super::confine::canonical_roots;
    use super::*;

    fn begin(
        dir: &Path,
        name: &str,
        expect: Option<FileVersion>,
        create: bool,
    ) -> Result<Upload, FsError> {
        let roots = canonical_roots(&[dir.to_path_buf()]);
        Upload::begin(
            &roots,
            &FsWriteBegin {
                path: dir.join(name),
                expect,
                create,
                size_hint: None,
                picked: false,
            },
            true,
            "t1",
        )
    }

    fn version(path: &Path) -> FileVersion {
        let stat = rustix::fs::stat(path).unwrap();
        version_of(&stat)
    }

    fn conflict(error: FsError) -> (&'static str, Option<FileVersion>) {
        match error {
            FsError::Conflict {
                reason, current, ..
            } => (reason, current),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_write_replaces_the_file_atomically_and_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        let before = version(&path);

        let mut upload = begin(dir.path(), "run.sh", Some(before), false).unwrap();
        upload.append(0, "new ").unwrap();
        upload.append(1, "text\n").unwrap();
        // Nothing is visible until the commit, and the temporary file sits next to the target.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert!(dir.path().join(".hutan-write-t1.tmp").exists());
        let (written, after) = upload.commit(9).unwrap();

        assert_eq!(written, path.canonicalize().unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new text\n");
        assert_eq!(after, version(&path));
        assert_ne!(after.ino, before.ino, "a new file replaced the old one");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o750
        );
        assert!(!dir.path().join(".hutan-write-t1.tmp").exists());
    }

    #[test]
    fn a_stale_version_is_a_conflict_that_names_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        let read = version(&path);
        // Somebody else saves meanwhile (a different size, a different file).
        std::fs::write(dir.path().join("a.tmp"), "other contents").unwrap();
        std::fs::rename(dir.path().join("a.tmp"), &path).unwrap();
        let current = version(&path);

        let (reason, got) = conflict(begin(dir.path(), "a.txt", Some(read), false).unwrap_err());
        assert_eq!((reason, got), ("version", Some(current)));

        // A change between the begin and the commit is caught by the commit.
        let mut upload = begin(dir.path(), "a.txt", Some(current), false).unwrap();
        upload.append(0, "mine").unwrap();
        std::fs::write(&path, "theirs, longer").unwrap();
        let (reason, _) = conflict(upload.commit(4).unwrap_err());
        assert_eq!(reason, "version");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs, longer");
        assert!(
            !dir.path().join(".hutan-write-t1.tmp").exists(),
            "a failed commit leaves no temporary file"
        );

        std::fs::remove_file(&path).unwrap();
        let (reason, got) = conflict(begin(dir.path(), "a.txt", Some(current), false).unwrap_err());
        assert_eq!((reason, got), ("missing", None));
    }

    #[test]
    fn create_never_overwrites_and_a_plain_write_needs_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("n.txt");
        assert!(matches!(
            begin(dir.path(), "n.txt", None, false),
            Err(FsError::NotFound(_))
        ));
        let mut upload = begin(dir.path(), "n.txt", None, true).unwrap();
        upload.append(0, "fresh").unwrap();
        // The name is taken between the begin and the commit.
        std::fs::write(&path, "taken").unwrap();
        let (reason, current) = conflict(upload.commit(5).unwrap_err());
        assert_eq!(reason, "exists");
        assert!(current.is_some());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "taken");

        let (reason, _) = conflict(begin(dir.path(), "n.txt", None, true).unwrap_err());
        assert_eq!(reason, "exists");
        std::fs::remove_file(&path).unwrap();
        let mut upload = begin(dir.path(), "n.txt", None, true).unwrap();
        upload.append(0, "fresh").unwrap();
        upload.commit(5).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh");
        assert!(!dir.path().join(".hutan-write-t1.tmp").exists());
        assert!(matches!(
            begin(dir.path(), "n.txt", Some(version(&path)), true),
            Err(FsError::BadRequest(_))
        ));
    }

    #[test]
    fn chunks_must_arrive_in_order_and_add_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "x").unwrap();
        let mut upload = begin(dir.path(), "a", None, false).unwrap();
        assert!(matches!(
            upload.append(1, "late"),
            Err(FsError::BadRequest(_))
        ));
        upload.append(0, "ab").unwrap();
        assert!(matches!(
            upload.append(0, "again"),
            Err(FsError::BadRequest(_))
        ));
        assert!(matches!(
            upload.append(1, &"x".repeat(CHUNK_MAX_BYTES + 1)),
            Err(FsError::BadRequest(_))
        ));
        assert!(matches!(upload.commit(3), Err(FsError::BadRequest(_))));
        assert_eq!(std::fs::read_to_string(dir.path().join("a")).unwrap(), "x");
        assert!(!dir.path().join(".hutan-write-t1.tmp").exists());
    }

    #[test]
    fn writing_below_git_and_through_escaping_links_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("out")).unwrap();
        let forbidden = |result: Result<Upload, FsError>| match result {
            Err(FsError::Forbidden { reason, .. }) => reason,
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("allowed"),
        };
        assert_eq!(
            forbidden(begin(dir.path(), ".git/hooks_new", None, true)),
            "git_dir"
        );
        assert_eq!(
            forbidden(begin(dir.path(), "out/x.txt", None, true)),
            "symlink"
        );
        assert!(!outside.path().join("x.txt").exists());
        // A picked path outside the roots is allowed to the user, never to an agent.
        let roots = canonical_roots(&[dir.path().to_path_buf()]);
        let request = FsWriteBegin {
            path: outside.path().join("saved.txt"),
            expect: None,
            create: true,
            size_hint: None,
            picked: true,
        };
        assert!(Upload::begin(&roots, &request, true, "p1").is_ok());
        assert_eq!(
            forbidden(Upload::begin(&roots, &request, false, "p2")),
            "outside_roots"
        );
    }
}
