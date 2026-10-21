//! `fs.search`: text search below a root. Blocking work; the caller runs it on the blocking
//! pool and ends it through the cancel flag.

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use hutan_protocol::fs::{SEARCH_MAX, SEARCH_TEXT_MAX_CHARS, SearchHit};
use regex::bytes::{Regex, RegexBuilder};

use super::confine::{FsError, fstat, is_regular, open_below, open_root};
use super::find::is_noise_dir;

/// Files above this size are not searched.
const FILE_MAX_BYTES: u64 = 4 << 20;
/// How much of a file's start is looked at for NUL bytes: binary files are skipped.
const BINARY_PROBE: usize = 8 << 10;
/// Shortest time between two progress reports.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// Regular expressions compile to at most this much.
const REGEX_SIZE_LIMIT: usize = 4 << 20;

#[derive(Debug)]
pub struct Params {
    /// Canonical.
    pub root: PathBuf,
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: Option<bool>,
    pub globs: Vec<String>,
    pub limit: u32,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub hits: Vec<SearchHit>,
    pub files_searched: u64,
    pub truncated: bool,
    pub cancelled: bool,
}

/// Whether a pattern asks for case: an upper-case letter that is not an escape such as `\W`.
fn has_upper_case(pattern: &str) -> bool {
    let mut escaped = false;
    for c in pattern.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

fn compile(params: &Params) -> Result<Regex, FsError> {
    if params.pattern.is_empty() {
        return Err(FsError::bad("the pattern is empty"));
    }
    let source = if params.regex {
        params.pattern.clone()
    } else {
        regex::escape(&params.pattern)
    };
    let sensitive = params
        .case_sensitive
        .unwrap_or_else(|| has_upper_case(&params.pattern));
    RegexBuilder::new(&source)
        .case_insensitive(!sensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| {
            FsError::bad(format!(
                "the pattern is not a valid regular expression: {e}"
            ))
        })
}

/// Runs the search. `progress(files, hits)` is called a few times a second.
pub fn search(
    params: &Params,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u32),
) -> Result<Outcome, FsError> {
    let regex = compile(params)?;
    let limit = params.limit.clamp(1, SEARCH_MAX) as usize;
    let root_file = open_root(&params.root)?;
    let mut builder = ignore::WalkBuilder::new(&params.root);
    builder
        .hidden(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|entry| {
            !(entry.depth() > 0
                && entry.file_type().is_some_and(|t| t.is_dir())
                && is_noise_dir(entry.file_name()))
        });
    if !params.globs.is_empty() {
        let mut overrides = ignore::overrides::OverrideBuilder::new(&params.root);
        for glob in &params.globs {
            overrides
                .add(glob)
                .map_err(|e| FsError::bad(format!("glob `{glob}`: {e}")))?;
        }
        builder.overrides(
            overrides
                .build()
                .map_err(|e| FsError::bad(format!("globs: {e}")))?,
        );
    }
    let mut outcome = Outcome::default();
    let mut last_report = Instant::now();
    let mut buffer = Vec::new();
    for entry in builder.build().filter_map(Result::ok) {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(&params.root) else {
            continue;
        };
        let Some(shown) = relative.to_str() else {
            continue;
        };
        outcome.files_searched += 1;
        if last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            progress(outcome.files_searched, outcome.hits.len() as u32);
        }
        // A file that cannot be opened (a race, a permission) is skipped, like a binary one.
        let Ok(mut file) = open_below(&root_file, relative) else {
            continue;
        };
        let Ok(stat) = fstat(&file, relative) else {
            continue;
        };
        if !is_regular(&stat) || u64::try_from(stat.st_size).unwrap_or(u64::MAX) > FILE_MAX_BYTES {
            continue;
        }
        buffer.clear();
        if file.read_to_end(&mut buffer).is_err() {
            continue;
        }
        if buffer[..buffer.len().min(BINARY_PROBE)].contains(&0) {
            continue;
        }
        let full = scan(&buffer, &regex, shown, limit, &mut outcome, cancel);
        if full {
            break;
        }
    }
    progress(outcome.files_searched, outcome.hits.len() as u32);
    Ok(outcome)
}

/// Collects the hits of one file into `outcome`; `true` when the limit is reached.
fn scan(
    content: &[u8],
    regex: &Regex,
    path: &str,
    limit: usize,
    outcome: &mut Outcome,
    cancel: &AtomicBool,
) -> bool {
    for (index, raw) in content.split(|byte| *byte == b'\n').enumerate() {
        if index % 4096 == 4095 && cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return true;
        }
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if !regex.is_match(raw) {
            continue;
        }
        if outcome.hits.len() >= limit {
            outcome.truncated = true;
            return true;
        }
        // Matched on the text as shown, so that the ranges are offsets into it.
        let text = String::from_utf8_lossy(raw);
        let ranges: Vec<(usize, usize)> = regex
            .find_iter(text.as_bytes())
            .map(|m| (m.start(), m.end()))
            .filter(|(start, end)| end > start)
            .collect();
        let (text, cut) = clip(&text);
        outcome.hits.push(SearchHit {
            path: path.to_owned(),
            line: u32::try_from(index + 1).unwrap_or(u32::MAX),
            matched: ranges
                .into_iter()
                .filter(|(start, _)| *start < cut)
                .map(|(start, end)| [start as u32, end.min(cut) as u32])
                .collect(),
            text,
        });
    }
    false
}

/// `text` cut at [`SEARCH_TEXT_MAX_CHARS`] characters, and the byte offset of the cut.
fn clip(text: &str) -> (String, usize) {
    match text.char_indices().nth(SEARCH_TEXT_MAX_CHARS) {
        Some((cut, _)) => (text[..cut].to_owned(), cut),
        None => (text.to_owned(), text.len()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for sub in ["src", "target", "node_modules/x", "docs", ".git"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"Hello\");\n}\n// TODO(ann): fix\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn hello() {}\r\npub fn other() {}\r\n",
        )
        .unwrap();
        std::fs::write(root.join("docs/a.md"), "hello docs\nTODO(bob): write\n").unwrap();
        std::fs::write(root.join("target/gen.rs"), "hello from target\n").unwrap();
        std::fs::write(root.join("node_modules/x/i.js"), "hello\n").unwrap();
        std::fs::write(root.join(".git/config"), "hello\n").unwrap();
        std::fs::write(root.join("blob.bin"), b"hello\0binary").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "hello ignored\n").unwrap();
        (dir, root)
    }

    fn run(root: &Path, pattern: &str, f: impl FnOnce(&mut Params)) -> Outcome {
        let mut params = Params {
            root: root.to_owned(),
            pattern: pattern.to_owned(),
            regex: false,
            case_sensitive: None,
            globs: Vec::new(),
            limit: 100,
        };
        f(&mut params);
        search(&params, &AtomicBool::new(false), &mut |_, _| {}).unwrap()
    }

    fn places(outcome: &Outcome) -> Vec<(String, u32)> {
        outcome
            .hits
            .iter()
            .map(|h| (h.path.clone(), h.line))
            .collect()
    }

    #[test]
    fn hits_come_in_path_order_and_skip_noise_binaries_and_ignored_files() {
        let (_dir, root) = tree();
        let found = run(&root, "hello", |_| {});
        assert_eq!(
            places(&found),
            [
                ("docs/a.md".to_owned(), 1),
                ("src/lib.rs".to_owned(), 1),
                ("src/main.rs".to_owned(), 2),
            ]
        );
        // The CR of a CRLF file is not part of the line.
        let lib = &found.hits[1];
        assert_eq!(lib.text, "pub fn hello() {}");
        assert_eq!(lib.matched, [[7, 12]]);
        // The file `blob.bin`, `.git`, `target`, `node_modules` and `ignored.txt` were left out.
        assert_eq!(found.files_searched, 5, "{found:?}");
        assert!(!found.truncated && !found.cancelled);
    }

    #[test]
    fn smart_case_regexes_globs_and_limits() {
        let (_dir, root) = tree();
        // Lower-case: any case. An upper-case letter: exactly.
        assert_eq!(run(&root, "hello", |_| {}).hits.len(), 3);
        assert_eq!(
            places(&run(&root, "Hello", |_| {})),
            [("src/main.rs".to_owned(), 2)]
        );
        assert_eq!(
            run(&root, "Hello", |p| p.case_sensitive = Some(false))
                .hits
                .len(),
            3
        );
        let todo = run(&root, r"TODO\(\w+\)", |p| p.regex = true);
        assert_eq!(todo.hits.len(), 2);
        assert_eq!(todo.hits[0].matched, [[0, 9]]);
        // A literal pattern is not a regex.
        assert!(run(&root, r"TODO\(", |_| {}).hits.is_empty());
        let only_rs = run(&root, "hello", |p| p.globs = vec!["*.rs".into()]);
        assert_eq!(places(&only_rs).len(), 2);
        let not_docs = run(&root, "hello", |p| p.globs = vec!["!docs/**".into()]);
        assert_eq!(places(&not_docs).len(), 2);
        let limited = run(&root, "hello", |p| p.limit = 2);
        assert_eq!((limited.hits.len(), limited.truncated), (2, true));
        assert!(matches!(
            search(
                &Params {
                    root,
                    pattern: "(".into(),
                    regex: true,
                    case_sensitive: None,
                    globs: Vec::new(),
                    limit: 5,
                },
                &AtomicBool::new(false),
                &mut |_, _| {}
            ),
            Err(FsError::BadRequest(_))
        ));
    }

    #[test]
    fn long_lines_are_cut_and_their_ranges_with_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let line = format!("{}needle{}needle", "é".repeat(600), "x".repeat(10));
        std::fs::write(root.join("long.txt"), &line).unwrap();
        std::fs::write(root.join("near.txt"), format!("needle{}", "y".repeat(2000))).unwrap();
        let found = run(&root, "needle", |_| {});
        let long = &found.hits[0];
        assert_eq!(long.path, "long.txt");
        assert_eq!(long.text.chars().count(), SEARCH_TEXT_MAX_CHARS);
        assert!(long.matched.is_empty(), "both matches lie behind the cut");
        let near = &found.hits[1];
        assert_eq!(near.text.chars().count(), SEARCH_TEXT_MAX_CHARS);
        assert_eq!(near.matched, [[0, 6]]);
    }

    #[test]
    fn a_cancelled_search_returns_what_it_has() {
        let (_dir, root) = tree();
        let cancel = AtomicBool::new(true);
        let params = Params {
            root,
            pattern: "hello".into(),
            regex: false,
            case_sensitive: None,
            globs: Vec::new(),
            limit: 10,
        };
        let outcome = search(&params, &cancel, &mut |_, _| {}).unwrap();
        assert!(outcome.cancelled);
        assert!(outcome.hits.is_empty());
    }
}
