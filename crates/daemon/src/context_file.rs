//! File attachments: open beneath an allowed workspace root without following raced symlinks.
//! Blocking I/O; callers run this on the blocking pool and redact before journaling.

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

/// Bound work even for a range near the end of an enormous file.
const SCAN_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct FileContext {
    pub path: PathBuf,
    pub text: String,
    pub truncated: bool,
}

/// One-based, inclusive lines; absent means the whole file, subject to the character budget.
/// A range beyond the scan bound is an explicit error, never an empty success.
pub fn read_file(
    path: &Path,
    roots: &[PathBuf],
    range: Option<(u32, u32)>,
    max_chars: usize,
) -> io::Result<FileContext> {
    let (start, end) = range.unwrap_or((1, u32::MAX));
    if start == 0 || end < start || max_chars == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid file range or budget",
        ));
    }
    let canonical = path.canonicalize()?;
    let mut selected = None;
    for root in roots {
        let root = root.canonicalize()?;
        if let Ok(relative) = canonical.strip_prefix(&root) {
            selected = Some(open_beneath(&root, relative)?);
            break;
        }
    }
    let file = selected.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is outside the workspace roots",
        )
    })?;
    read_text(file, canonical, (start, end), max_chars)
}

/// Like [`read_file`] for a path the user named themselves (protocol 0.23, `picked`): any
/// regular file, symbolic links followed, not confined to the workspace roots.
pub fn read_picked_file(
    path: &Path,
    range: Option<(u32, u32)>,
    max_chars: usize,
) -> io::Result<FileContext> {
    let (start, end) = range.unwrap_or((1, u32::MAX));
    if start == 0 || end < start || max_chars == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid file range or budget",
        ));
    }
    let (file, canonical) = open_picked(path)?;
    read_text(file, canonical, (start, end), max_chars)
}

/// The bytes of an image the user named (protocol 0.23): any regular file of at most
/// `max_bytes`, symbolic links followed. Returns the canonical path with the bytes.
pub fn read_picked_bytes(path: &Path, max_bytes: usize) -> io::Result<(PathBuf, Vec<u8>)> {
    let (file, canonical) = open_picked(path)?;
    let limit = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if file.metadata()?.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "file is larger than the limit",
        ));
    }
    let mut bytes = Vec::new();
    // One byte more than the limit: a file that grew while it was read is still refused.
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "file is larger than the limit",
        ));
    }
    Ok((canonical, bytes))
}

/// Opens a regular file named by the user; directories and special files are `InvalidInput`.
fn open_picked(path: &Path) -> io::Result<(File, PathBuf)> {
    let canonical = path.canonicalize()?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&canonical)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment is not a regular file",
        ));
    }
    Ok((file, canonical))
}

/// The text of lines `start..=end` of `file`, at most `max_chars` characters.
fn read_text(
    file: File,
    canonical: PathBuf,
    (start, end): (u32, u32),
    max_chars: usize,
) -> io::Result<FileContext> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment is not a regular file",
        ));
    }
    let size = file.metadata()?.len();
    let mut reader = BufReader::new(file.take(SCAN_BYTES));
    let mut selected_bytes = Vec::new();
    let byte_limit = max_chars.saturating_add(1).saturating_mul(4);
    let mut line = 1u32;
    let mut found_start = false;
    let mut truncated = false;
    // fill_buf, rather than read_line: even a single enormous line stays bounded.
    while let bytes @ [_, ..] = reader.fill_buf()? {
        let used = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        let segment = &bytes[..used];
        if line >= start && line <= end {
            found_start = true;
            let count = segment.len().min(byte_limit - selected_bytes.len());
            selected_bytes.extend_from_slice(&segment[..count]);
            if count < segment.len() {
                truncated = true;
                break;
            }
        }
        let newline = segment.last() == Some(&b'\n');
        reader.consume(used);
        if newline {
            if line == end {
                break;
            }
            line = line.saturating_add(1);
        }
    }
    if !found_start && size >= SCAN_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file range exceeds the scan bound",
        ));
    }
    if reader.get_ref().limit() == 0 && size > SCAN_BYTES {
        truncated = true;
    }
    let valid = match std::str::from_utf8(&selected_bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            // The scan/byte bound cut the final UTF-8 character, not an interior buffer edge.
            std::str::from_utf8(&selected_bytes[..error.valid_up_to()])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        }
        Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    };
    let text = if let Some((boundary, _)) = valid.char_indices().nth(max_chars) {
        truncated = true;
        valid[..boundary].to_owned()
    } else {
        valid.to_owned()
    };
    Ok(FileContext {
        path: canonical,
        text,
        truncated,
    })
}

/// `openat` of one path component below `parent`, never following a symlink in it.
#[allow(unsafe_code)] // Isolated openat boundary, matching the terminal/index syscall wrappers.
pub(crate) fn open_component(
    parent: &File,
    name: &std::ffi::OsStr,
    flags: libc::c_int,
) -> io::Result<File> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in file path"))?;
    // SAFETY: the parent descriptor and NUL-terminated component remain valid for openat.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor; no other File owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Opens `root` (canonical, absolute) and then `relative` below it using descriptors only: every
/// component, the root's own ancestors included, is opened with `O_NOFOLLOW` relative to its
/// parent descriptor, so a symlink swapped in at any level after the path was checked fails the
/// open instead of being followed.
pub(crate) fn open_beneath(root: &Path, relative: &Path) -> io::Result<File> {
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open("/")?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory = open_component(&directory, name, libc::O_DIRECTORY)?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "workspace root is not a canonical absolute path",
                ));
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn reads_only_the_requested_lines_and_bounds_output() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes");
        std::fs::write(&path, "first\nsecond\nthird\n").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let result = read_file(&path, &roots, Some((2, 2)), 100).unwrap();
        assert_eq!(result.text, "second\n");
        assert!(!result.truncated);
        let result = read_file(&path, &roots, None, 8).unwrap();
        assert_eq!(result.text, "first\nse");
        assert!(result.truncated);
    }

    #[test]
    fn preserves_unicode_at_io_boundaries_and_cuts_at_characters() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unicode");
        let input = format!("{}😀é\nnext", "a".repeat(8191));
        std::fs::write(&path, &input).unwrap();
        let roots = vec![root.path().to_path_buf()];
        let result = read_file(&path, &roots, None, 9000).unwrap();
        assert_eq!(result.text, input);
        assert!(!result.truncated);
        let result = read_file(&path, &roots, None, 8192).unwrap();
        assert_eq!(result.text, format!("{}😀", "a".repeat(8191)));
        assert!(result.truncated);
    }

    #[test]
    fn refuses_invalid_ranges_and_non_text_attachments() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("binary");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(
            read_file(&path, &roots, Some((0, 2)), 100)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            read_file(&path, &roots, None, 100).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_file(root.path(), &roots, None, 100)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn confines_symlink_targets_and_parent_traversal() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("secret");
        std::fs::write(&path, "outside").unwrap();
        symlink(&path, root.path().join("escape")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert_eq!(
            read_file(&root.path().join("escape"), &roots, None, 100)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        symlink(root.path(), root.path().join("inside")).unwrap();
        std::fs::write(root.path().join("notes"), "inside").unwrap();
        assert_eq!(
            read_file(&root.path().join("inside/notes"), &roots, None, 100)
                .unwrap()
                .text,
            "inside"
        );
        // `open_beneath` takes a canonical root and walks it from `/` without following symlinks.
        let canonical = root.path().canonicalize().unwrap();
        assert_eq!(
            open_beneath(&canonical, Path::new("../secret"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(open_beneath(&canonical, Path::new("escape")).is_err());
        assert!(open_beneath(&canonical, Path::new("notes")).is_ok());
        // A symlink among the root's own ancestors (raced in after the path was checked) is
        // not followed either.
        let through = outside.path().join("via");
        symlink(&canonical, &through).unwrap();
        assert!(open_beneath(&through, Path::new("notes")).is_err());
    }
}
