//! Path confinement for pack files: relative normalized UTF-8 paths only, no symlink anywhere
//! below the pack root, regular files only, bounded reads.
//!
//! Descriptor-relative `openat(O_NOFOLLOW)` opens every component, so concurrent path swaps
//! cannot redirect a read through a symlink. Identity checks also reject a moved/replaced pack.

use std::fs::{self, File, Metadata};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Whether `path` is a normalized relative UTF-8 path that stays inside its root.
pub fn check_relative(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path is empty".to_owned());
    }
    if path.contains('\0') {
        return Err("path contains a NUL byte".to_owned());
    }
    if path.contains('\\') {
        return Err("path contains a backslash; use `/`".to_owned());
    }
    if path.starts_with('/') {
        return Err("path is absolute; it must be relative to the pack".to_owned());
    }
    if path.starts_with('~') {
        return Err("`~` is not expanded; the path must be relative to the pack".to_owned());
    }
    if path.contains(':') {
        return Err("path contains `:` (URL schemes and drive letters are refused)".to_owned());
    }
    for component in path.split('/') {
        match component {
            "" => return Err("path has an empty component".to_owned()),
            "." | ".." => return Err(format!("path component `{component}` is not allowed")),
            _ => {}
        }
    }
    Ok(())
}

/// Identity of one path component.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
}

fn identity(meta: &Metadata) -> Identity {
    Identity {
        dev: meta.dev(),
        ino: meta.ino(),
    }
}

/// `lstat`s every component of `rel` below `root`; the last one must be a regular file and
/// none may be a symlink. Returns the identities, root first.
fn walk(root: &Path, rel: &str) -> Result<(PathBuf, Vec<Identity>), String> {
    let mut current = root.to_path_buf();
    let mut identities = Vec::new();
    let root_meta = fs::symlink_metadata(&current).map_err(|e| format!("cannot read pack: {e}"))?;
    if root_meta.file_type().is_symlink() {
        return Err("symlink component refused".to_owned());
    }
    identities.push(identity(&root_meta));
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        let meta =
            fs::symlink_metadata(&current).map_err(|e| format!("cannot read `{part}`: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err("symlink component refused".to_owned());
        }
        if parts.peek().is_some() {
            if !meta.is_dir() {
                return Err(format!("`{part}` is not a directory"));
            }
        } else if !meta.is_file() {
            return Err(format!("`{part}` is not a regular file"));
        }
        identities.push(identity(&meta));
    }
    Ok((current, identities))
}

/// Reads at most `max` bytes of `rel` below `root`.
pub fn read_confined(root: &Path, rel: &str, max: u64) -> Result<Vec<u8>, String> {
    read_confined_with(root, rel, max, &mut || {})
}

/// [`read_confined`] with a hook that runs between the checks and the open (tests swap path
/// components there).
pub(super) fn read_confined_with(
    root: &Path,
    rel: &str,
    max: u64,
    between: &mut dyn FnMut(),
) -> Result<Vec<u8>, String> {
    check_relative(rel)?;
    let (_, before) = walk(root, rel)?;
    between();
    let file = open_confined(root, rel)?;
    let meta = file.metadata().map_err(|e| format!("cannot stat: {e}"))?;
    if !meta.is_file() {
        return Err("not a regular file".to_owned());
    }
    if meta.len() > max {
        return Err(format!("{} bytes; the limit is {max}", meta.len()));
    }
    if before.last() != Some(&identity(&meta)) {
        return Err("changed while it was being opened".to_owned());
    }
    let mut bytes = Vec::new();
    // One byte more than the limit tells a file that grew while being read.
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read: {e}"))?;
    if bytes.len() as u64 > max {
        return Err(format!(
            "grew past the limit of {max} bytes while being read"
        ));
    }
    let (_, after) =
        walk(root, rel).map_err(|why| format!("changed while it was being read: {why}"))?;
    if before != after {
        return Err("changed while it was being read".to_owned());
    }
    Ok(bytes)
}

/// UTF-8 text of `rel` below `root`.
pub fn read_text(root: &Path, rel: &str, max: u64) -> Result<String, String> {
    let bytes = read_confined(root, rel, max)?;
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_owned())
}

#[cfg(test)]
pub(super) fn read_text_with(
    root: &Path,
    rel: &str,
    max: u64,
    between: &mut dyn FnMut(),
) -> Result<String, String> {
    let bytes = read_confined_with(root, rel, max, between)?;
    String::from_utf8(bytes).map_err(|_| "not valid UTF-8".to_owned())
}

fn open_confined(root: &Path, rel: &str) -> Result<File, String> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let fail = |e| format!("cannot open confined file (symlink component refused): {e}");
    let dirs = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut parent = open(root, dirs, Mode::empty()).map_err(fail)?;
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        let flags = if parts.peek().is_some() {
            dirs
        } else {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
        };
        parent = openat(&parent, part, flags, Mode::empty()).map_err(fail)?;
    }
    Ok(File::from(parent))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn relative_paths_are_normalized_and_confined() {
        for ok in ["a", "icons/a.svg", "a/b/c.txt", "LICENSE"] {
            assert_eq!(check_relative(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a/./b",
            "a\\b",
            "~/x",
            "C:/x",
            "file://x",
            "a\0b",
            "a/",
        ] {
            assert!(check_relative(bad).is_err(), "{bad:?}");
        }
    }

    fn pack() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("pack/sub")).unwrap();
        fs::write(dir.path().join("pack/theme.toml"), "ok").unwrap();
        fs::write(dir.path().join("pack/sub/LICENSE"), "mit").unwrap();
        fs::write(dir.path().join("secret.txt"), "secret").unwrap();
        dir
    }

    #[test]
    fn a_regular_file_reads() {
        let dir = pack();
        let root = dir.path().join("pack");
        assert_eq!(read_text(&root, "theme.toml", 16).unwrap(), "ok");
        assert_eq!(read_text(&root, "sub/LICENSE", 16).unwrap(), "mit");
    }

    #[test]
    fn symlinks_are_refused_at_every_component() {
        let dir = pack();
        let root = dir.path().join("pack");
        symlink(dir.path().join("secret.txt"), root.join("link.toml")).unwrap();
        symlink(dir.path(), root.join("up")).unwrap();
        for rel in ["link.toml", "up/secret.txt"] {
            let err = read_text(&root, rel, 64).unwrap_err();
            assert!(err.contains("symlink"), "{rel}: {err}");
        }
        // A symlinked root is refused too.
        let alias = dir.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(
            read_text(&alias, "theme.toml", 64)
                .unwrap_err()
                .contains("symlink")
        );
    }

    #[test]
    fn directories_and_missing_files_are_not_files() {
        let dir = pack();
        let root = dir.path().join("pack");
        assert!(
            read_text(&root, "sub", 64)
                .unwrap_err()
                .contains("not a regular file")
        );
        assert!(
            read_text(&root, "theme.toml/x", 64)
                .unwrap_err()
                .contains("not a directory")
        );
        assert!(read_text(&root, "absent", 64).is_err());
        assert!(read_text(&root, "../secret.txt", 64).is_err());
    }

    #[test]
    fn the_size_limit_holds_for_the_file_and_for_growth() {
        let dir = pack();
        let root = dir.path().join("pack");
        let err = read_text(&root, "theme.toml", 1).unwrap_err();
        assert!(err.contains("the limit is 1"), "{err}");
        // Grows between the stat and the read: still bounded.
        let big = root.join("big");
        fs::write(&big, "x").unwrap();
        let grow = big.clone();
        let err = read_text_with(&root, "big", 4, &mut move || {
            fs::write(&grow, "0123456789").unwrap();
        })
        .unwrap_err();
        assert!(err.contains("limit"), "{err}");
    }

    #[test]
    fn a_directory_swapped_for_a_symlink_mid_read_is_caught() {
        let dir = pack();
        let root = dir.path().join("pack");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("LICENSE"), "stolen").unwrap();
        let sub = root.join("sub");
        let result = read_text_with(&root, "sub/LICENSE", 64, &mut || {
            fs::remove_dir_all(&sub).unwrap();
            symlink(&outside, &sub).unwrap();
        });
        let err = result.unwrap_err();
        assert!(
            err.contains("symlink") || err.contains("changed"),
            "leaked or wrong error: {err}"
        );
    }

    #[test]
    fn a_file_replaced_by_another_between_check_and_open_is_caught() {
        let dir = pack();
        let root = dir.path().join("pack");
        let path = root.join("theme.toml");
        let other = root.join("other.toml");
        let result = read_text_with(&root, "theme.toml", 64, &mut || {
            // The replacement exists next to the original, so its inode cannot be reused.
            fs::write(&other, "swapped").unwrap();
            fs::rename(&other, &path).unwrap();
        });
        assert!(result.unwrap_err().contains("changed"), "inode must differ");
    }
}
