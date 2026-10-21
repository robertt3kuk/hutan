//! `fs.read`, `fs.stat` and `fs.list`. Blocking I/O.

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::FileExt;
use std::path::Path;

use hutan_protocol::fs::{
    CHUNK_MAX_BYTES, FILE_MAX_BYTES, FsEntry, FsFile, FsKind, FsListing, LIST_MAX, LineEnding,
};

use super::confine::{
    FsError, Resolved, entry_at, fstat, into_fd, is_regular, open, open_parent, version_of,
};

/// The most bytes of a chunk's JSON form: a frame holds 4 MiB, and a text full of control
/// characters escapes to up to six times its size.
const ESCAPED_MAX: usize = 3 << 20;
/// The most directory entries looked at: a bound for a folder with millions.
const SCAN_MAX: usize = 100_000;

/// Reads up to `max` bytes of text at `offset`.
pub fn read(resolved: &Resolved, offset: u64, max: usize) -> Result<FsFile, FsError> {
    let file = open(resolved)?;
    let stat = fstat(&file, &resolved.canonical)?;
    let shown = resolved.canonical.display();
    if !is_regular(&stat) {
        return Err(FsError::unsupported(
            "not_a_file",
            format!("{shown} is not a regular file"),
        ));
    }
    let version = version_of(&stat);
    let size = version.size;
    if size > FILE_MAX_BYTES {
        return Err(FsError::Unsupported {
            reason: "too_large",
            size: Some(size),
            message: format!(
                "{shown} is {size} bytes; the file service handles at most {FILE_MAX_BYTES}"
            ),
        });
    }
    let max = max.clamp(1, CHUNK_MAX_BYTES);
    let wanted = usize::try_from(size.saturating_sub(offset)).map_or(max, |left| left.min(max));
    let mut buffer = vec![0_u8; wanted];
    let mut filled = 0;
    while filled < wanted {
        let read = file
            .read_at(&mut buffer[filled..], offset + filled as u64)
            .map_err(|e| FsError::io(shown.to_string(), e))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buffer.truncate(filled);
    if offset > 0 && buffer.first().is_some_and(|byte| byte & 0xC0 == 0x80) {
        return Err(FsError::bad(format!(
            "offset {offset} is inside a character of {shown}"
        )));
    }
    if buffer.contains(&0) {
        return Err(FsError::unsupported(
            "binary",
            format!("{shown} is a binary file"),
        ));
    }
    let mut text = decode(buffer, shown.to_string().as_str())?;
    shrink_to_frame(&mut text);
    let bom = if offset == 0 {
        text.starts_with('\u{feff}')
    } else {
        let mut head = [0_u8; 3];
        file.read_at(&mut head, 0).is_ok_and(|n| n == 3) && head == [0xEF, 0xBB, 0xBF]
    };
    let end = offset + text.len() as u64;
    Ok(FsFile {
        path: resolved.canonical.clone(),
        line_ending: line_ending(&text),
        eof: end >= size,
        text,
        offset,
        size,
        version,
        bom,
    })
}

/// The longest prefix of `bytes` that is UTF-8; a character cut by the end of the chunk is left
/// for the next one, anything else invalid is an error.
fn decode(mut bytes: Vec<u8>, shown: &str) -> Result<String, FsError> {
    match std::str::from_utf8(&bytes) {
        Ok(_) => {}
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            bytes.truncate(error.valid_up_to());
        }
        Err(_) => {
            return Err(FsError::unsupported(
                "not_utf8",
                format!("{shown} is not valid UTF-8"),
            ));
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| FsError::unsupported("not_utf8", format!("{shown} is not valid UTF-8")))
}

/// Cuts `text` at a character boundary until its JSON form fits [`ESCAPED_MAX`].
fn shrink_to_frame(text: &mut String) {
    while escaped_len(text) > ESCAPED_MAX {
        let mut cut = text.len() / 2;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
}

/// The length of `text` as a JSON string, quotes excluded.
pub fn escaped_len(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

fn line_ending(text: &str) -> LineEnding {
    let bytes = text.as_bytes();
    let (mut crlf, mut lf) = (0_usize, 0_usize);
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            if index > 0 && bytes[index - 1] == b'\r' {
                crlf += 1;
            } else {
                lf += 1;
            }
        }
    }
    match (crlf, lf) {
        (0, _) => LineEnding::Lf,
        (_, 0) => LineEnding::Crlf,
        _ => LineEnding::Mixed,
    }
}

/// `fs.stat`: what the path is. A link is described rather than followed, so the path is looked
/// at through its directory.
pub fn stat(resolved: &Resolved) -> Result<FsEntry, FsError> {
    if resolved.relative.as_os_str().is_empty() {
        // The root itself.
        let directory = open(resolved)?;
        let stat = fstat(&directory, &resolved.canonical)?;
        return Ok(FsEntry {
            name: resolved
                .canonical
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            path: resolved.canonical.clone(),
            kind: super::confine::kind_of(&stat),
            size: u64::try_from(stat.st_size).unwrap_or(0),
            mtime_ns: super::confine::mtime_ns(&stat),
            ino: stat.st_ino,
            ignored: false,
        });
    }
    let parent = open_parent(resolved)?;
    let name = resolved
        .canonical
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let dir_path = resolved.canonical.parent().unwrap_or(Path::new("/"));
    let mut entry = entry_at(&parent, dir_path, &name)
        .map_err(|e| FsError::io(resolved.canonical.display().to_string(), e))?;
    entry.path = resolved.canonical.clone();
    Ok(entry)
}

/// `fs.list`: the entries of a directory, directories first.
pub fn list(resolved: &Resolved, ignore: bool, limit: u32) -> Result<FsListing, FsError> {
    let directory = open(resolved)?;
    let stat = fstat(&directory, &resolved.canonical)?;
    if super::confine::kind_of(&stat) != FsKind::Dir {
        return Err(FsError::unsupported(
            "not_a_file",
            format!("{} is not a directory", resolved.canonical.display()),
        ));
    }
    let shown = resolved.canonical.display().to_string();
    // The entry listing walks the same descriptor `entry_at` looks things up in.
    let handle = directory
        .try_clone()
        .map_err(|e| FsError::io(shown.clone(), e))?;
    let mut reader =
        rustix::fs::Dir::new(into_fd(handle)).map_err(|e| FsError::io(shown.clone(), e.into()))?;
    let mut entries = Vec::new();
    let mut scanned = 0;
    let mut truncated = false;
    while let Some(item) = reader.read() {
        let item = item.map_err(|e| FsError::io(shown.clone(), e.into()))?;
        let bytes = item.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        scanned += 1;
        if scanned > SCAN_MAX {
            truncated = true;
            break;
        }
        let name = OsString::from_vec(bytes.to_vec())
            .to_string_lossy()
            .into_owned();
        // An entry that vanished meanwhile is simply not there.
        if let Ok(entry) = entry_at(&directory, &resolved.canonical, &name) {
            entries.push(entry);
        }
    }
    if ignore {
        mark_ignored(&resolved.canonical, &mut entries);
    }
    entries.sort_by(|a, b| {
        (a.kind != FsKind::Dir, a.name.to_lowercase(), &a.name).cmp(&(
            b.kind != FsKind::Dir,
            b.name.to_lowercase(),
            &b.name,
        ))
    });
    let limit = limit.clamp(1, LIST_MAX) as usize;
    if entries.len() > limit {
        entries.truncate(limit);
        truncated = true;
    }
    Ok(FsListing {
        dir: resolved.canonical.clone(),
        entries,
        truncated,
    })
}

/// Flags the entries the `.gitignore` / `.ignore` rules of the folder and its parents match:
/// those a one-level walk does not yield. `.git` itself counts as ignored.
fn mark_ignored(dir: &Path, entries: &mut [FsEntry]) {
    let kept: HashSet<OsString> = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| entry.file_name().to_owned())
        .collect();
    for entry in entries {
        entry.ignored =
            entry.name == ".git" || !kept.contains(OsString::from(&entry.name).as_os_str());
    }
}

#[cfg(test)]
mod tests {
    use super::super::confine::{canonical_roots, resolve};
    use super::*;

    fn roots(dir: &Path) -> Vec<std::path::PathBuf> {
        canonical_roots(&[dir.to_path_buf()])
    }

    #[test]
    fn text_is_cut_on_character_boundaries_and_reads_resume_where_they_ended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let text = "héllo wörld — ünïcode\n".repeat(10);
        std::fs::write(&path, &text).unwrap();
        let roots = roots(dir.path());
        let resolved = resolve(&path, &roots, false).unwrap();

        let mut collected = String::new();
        let mut offset = 0;
        loop {
            let chunk = read(&resolved, offset, 7).unwrap();
            assert!(chunk.text.len() <= 7);
            assert_eq!(chunk.offset, offset);
            offset += chunk.text.len() as u64;
            collected.push_str(&chunk.text);
            if chunk.eof {
                break;
            }
        }
        assert_eq!(collected, text);
        // An offset in the middle of a character is the client's mistake.
        let error = read(&resolved, 2, 7).unwrap_err();
        assert!(matches!(error, FsError::BadRequest(_)), "{error:?}");
    }

    #[test]
    fn binary_and_non_utf8_files_are_unsupported_and_the_rest_reports_its_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bin"), b"abc\0def").unwrap();
        std::fs::write(dir.path().join("latin"), b"caf\xe9\n").unwrap();
        std::fs::write(dir.path().join("crlf"), b"\xef\xbb\xbfa\r\nb\r\n").unwrap();
        std::fs::write(dir.path().join("mixed"), b"a\r\nb\nc\n").unwrap();
        let roots = roots(dir.path());
        let get = |name: &str| {
            read(
                &resolve(&dir.path().join(name), &roots, false).unwrap(),
                0,
                100,
            )
        };
        let reason = |error: FsError| match error {
            FsError::Unsupported { reason, .. } => reason,
            other => panic!("{other:?}"),
        };
        assert_eq!(reason(get("bin").unwrap_err()), "binary");
        assert_eq!(reason(get("latin").unwrap_err()), "not_utf8");
        let crlf = get("crlf").unwrap();
        assert_eq!((crlf.line_ending, crlf.bom), (LineEnding::Crlf, true));
        assert!(crlf.text.starts_with('\u{feff}'));
        assert_eq!(get("mixed").unwrap().line_ending, LineEnding::Mixed);
    }

    #[test]
    fn a_text_full_of_control_characters_is_cut_to_fit_a_frame() {
        let mut text = "\u{1}".repeat(CHUNK_MAX_BYTES);
        shrink_to_frame(&mut text);
        assert!(escaped_len(&text) <= ESCAPED_MAX);
        assert!(!text.is_empty());
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(json.len() - 2, escaped_len(&text));
    }

    #[test]
    fn listings_put_directories_first_and_flag_what_gitignore_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(dir.path().join("Zebra.txt"), "z").unwrap();
        std::fs::write(dir.path().join("apple.txt"), "a").unwrap();
        std::fs::write(dir.path().join("build.log"), "l").unwrap();
        let roots = roots(dir.path());
        let resolved = resolve(dir.path(), &roots, false).unwrap();

        let listing = list(&resolved, true, 100).unwrap();
        let names: Vec<(&str, bool)> = listing
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.ignored))
            .collect();
        assert_eq!(
            names,
            [
                (".git", true),
                ("src", false),
                ("target", true),
                (".gitignore", false),
                ("apple.txt", false),
                ("build.log", true),
                ("Zebra.txt", false),
            ]
        );
        assert!(!listing.truncated);

        let plain = list(&resolved, false, 100).unwrap();
        assert!(plain.entries.iter().all(|e| !e.ignored));
        let short = list(&resolved, false, 2).unwrap();
        assert_eq!(short.entries.len(), 2);
        assert!(short.truncated);
    }

    #[test]
    fn stat_follows_a_link_that_stays_inside_the_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "12345").unwrap();
        std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("l")).unwrap();
        let roots = roots(dir.path());
        // The link inside the root resolves to its target before it is looked at, so a stat of
        // the link path describes the file (`list` shows links as links); the root is a
        // directory.
        let file = stat(&resolve(&dir.path().join("l"), &roots, false).unwrap()).unwrap();
        assert_eq!(
            (file.kind, file.size, file.name.as_str()),
            (FsKind::File, 5, "a")
        );
        let root = stat(&resolve(dir.path(), &roots, false).unwrap()).unwrap();
        assert_eq!(root.kind, FsKind::Dir);
    }
}
