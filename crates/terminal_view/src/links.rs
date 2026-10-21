//! Links in terminal text (GPUI-free): URLs and `path[:line[:col]]` found in a logical line.
//!
//! Nothing here runs per frame. The view asks [`candidate_at`] only while ⌘ is held over a cell
//! (hover) and once on a ⌘-click, and decides with the file system whether a path candidate is a
//! real file ([`resolve`]); text that merely looks like a path (`e.g.`, `1.5`) is not a link.

use std::ops::Range;
use std::path::{Path, PathBuf};

/// What ⌘-click opens. A path is already resolved (absolute) and exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An `http(s)` or `mailto` URL: the browser (or mail client).
    Url(String),
    /// A file or folder, with the position the text named (`src/lib.rs:42:7`).
    File {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// A link-shaped piece of a logical line, before any file system check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    Url(String),
    Path {
        /// As written, without `:line:col`.
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// The candidate under byte offset `at`, and the byte range of its text (what gets underlined).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub range: Range<usize>,
    pub candidate: Candidate,
}

/// Longest URL or path the detector looks at, in bytes.
const MAX_TOKEN: usize = 2048;

/// Characters that end a token: whitespace and what encloses a path or URL in prose and logs.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|')
}

/// The candidate whose text covers byte offset `at` of `text`, if any. `text` is one logical line
/// (soft-wrapped rows joined); a hard line break never joins two matches.
pub fn candidate_at(text: &str, at: usize) -> Option<Found> {
    let here = text.get(at..)?.chars().next()?;
    if is_delimiter(here) {
        return None;
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .find(|(_, c)| is_delimiter(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let end = at
        + text[at..]
            .char_indices()
            .find(|(_, c)| is_delimiter(*c))
            .map_or(text.len() - at, |(i, _)| i);
    if end - start > MAX_TOKEN {
        return None;
    }
    let found = match url_in(&text[start..end]) {
        Some(Some((from, len))) => Found {
            range: start + from..start + from + len,
            candidate: Candidate::Url(text[start + from..start + from + len].to_owned()),
        },
        Some(None) => return None,
        None => path_in(text, start, end)?,
    };
    found.range.contains(&at).then_some(found)
}

/// `None`: the token holds no URL. `Some(None)`: it holds something that starts like one but is
/// not (`http://`, nothing after it). `Some(Some((offset, length)))`: the URL inside the token.
fn url_in(token: &str) -> Option<Option<(usize, usize)>> {
    let (from, scheme) = ["https://", "http://"].iter().find_map(|scheme| {
        token
            .char_indices()
            .find(|(i, _)| {
                let rest = &token[*i..];
                rest.len() >= scheme.len()
                    && rest.is_char_boundary(scheme.len())
                    && rest[..scheme.len()].eq_ignore_ascii_case(scheme)
                    && token[..*i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_alphanumeric() && !"+-.".contains(c))
            })
            .map(|(i, _)| (i, scheme.len()))
    })?;
    let len = trim_end(&token[from..]);
    let url = &token[from..from + len];
    let host_ok = url[scheme..]
        .chars()
        .next()
        .is_some_and(char::is_alphanumeric);
    Some(host_ok.then_some((from, len)))
}

/// Length of `s` without the punctuation that ends a sentence around a link, and without
/// closing brackets that have no opening one inside.
fn trim_end(s: &str) -> usize {
    let mut end = s.len();
    loop {
        let rest = &s[..end];
        let Some(last) = rest.chars().next_back() else {
            return end;
        };
        let unmatched = |open: char| {
            rest.chars().filter(|c| *c == last).count()
                > rest.chars().filter(|c| *c == open).count()
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' => true,
            ')' => unmatched('('),
            ']' => unmatched('['),
            '}' => unmatched('{'),
            _ => false,
        };
        if !strip {
            return end;
        }
        end -= last.len_utf8();
    }
}

/// The path candidate of the token `text[start..end]`.
fn path_in(text: &str, start: usize, end: usize) -> Option<Found> {
    let token = &text[start..end];
    // `(`, `[`, `{` that wrap the path, and the `--flag=` of an option.
    let mut from = token.len() - token.trim_start_matches(['(', '[', '{']).len();
    if token[from..].starts_with("--")
        && let Some(eq) = token[from..].find('=')
    {
        from += eq + 1;
    }
    let len = trim_end(&token[from..]);
    let body = &token[from..from + len];
    let (path, line, column) = split_position(body);
    if path.is_empty() || path.contains(':') || !path.chars().any(char::is_alphanumeric) {
        return None;
    }
    let has_position = line.is_some();
    let looks_like_a_path = path.starts_with('/')
        || path.starts_with('~')
        || path.contains('/')
        || path.contains('.')
        || has_position;
    if !looks_like_a_path {
        return None;
    }
    let (mut line, mut column) = (line, column);
    // Python tracebacks: `File "/a/b.py", line 12`.
    if line.is_none()
        && let Some(n) = quoted_line_after(&text[end..])
    {
        line = Some(n);
        column = None;
    }
    Some(Found {
        range: start + from..start + from + len,
        candidate: Candidate::Path {
            path: path.to_owned(),
            line,
            column,
        },
    })
}

/// A run of ASCII digits as a number.
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Splits `body` into the path, the line and the column: `path:12:5`, `path:12`, `path(12,5)`,
/// `path(12)`.
fn split_position(body: &str) -> (&str, Option<u32>, Option<u32>) {
    if let Some(inner) = body.strip_suffix(')')
        && let Some(open) = inner.rfind('(')
    {
        let numbers: Vec<Option<u32>> = inner[open + 1..]
            .split(',')
            .map(|n| number(n.trim()))
            .collect();
        match numbers.as_slice() {
            [Some(line)] => return (&inner[..open], Some(*line), None),
            [Some(line), Some(column)] => return (&inner[..open], Some(*line), Some(*column)),
            _ => {}
        }
    }
    if let Some((rest, last)) = body.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, middle)) = rest.rsplit_once(':')
            && let Some(line) = number(middle)
        {
            return (path, Some(line), Some(last));
        }
        return (rest, Some(last), None);
    }
    (body, None, None)
}

/// `", line 12"` right after a closing quote.
fn quoted_line_after(rest: &str) -> Option<u32> {
    let rest = rest.strip_prefix(['"', '\''])?;
    let rest = rest.strip_prefix(", line ")?;
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    rest[..digits].parse().ok()
}

/// The absolute path a written `path` names: `~/…` against `home`, a relative path against
/// `cwd`. `None` for a relative path with no `cwd`.
pub fn resolve(path: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if path == "~" {
        return home.map(Path::to_path_buf);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    cwd.map(|cwd| cwd.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidate under the first occurrence of `needle` in `text` (its first byte).
    type Hit = Option<(String, Candidate)>;
    type Case<'a> = (&'a str, &'a str, Option<(&'a str, Candidate)>);

    fn found(text: &str, needle: &str) -> Hit {
        let at = text.find(needle).expect("needle in text");
        candidate_at(text, at).map(|f| (text[f.range].to_owned(), f.candidate))
    }

    fn path(p: &str, line: Option<u32>, column: Option<u32>) -> Candidate {
        Candidate::Path {
            path: p.to_owned(),
            line,
            column,
        }
    }

    #[test]
    fn detector_table() {
        let cases: &[Case<'_>] = &[
            (
                "src/lib.rs:42:7: error",
                "src",
                Some(("src/lib.rs:42:7", path("src/lib.rs", Some(42), Some(7)))),
            ),
            (
                "error at (foo.rs:12) here",
                "foo",
                Some(("foo.rs:12", path("foo.rs", Some(12), None))),
            ),
            (
                "see [./a/b.txt] now",
                "a/b",
                Some(("./a/b.txt", path("./a/b.txt", None, None))),
            ),
            (
                "--manifest-path=crates/x/Cargo.toml",
                "crates",
                Some((
                    "crates/x/Cargo.toml",
                    path("crates/x/Cargo.toml", None, None),
                )),
            ),
            (
                "at ~/proj/main.go:9.",
                "~",
                Some(("~/proj/main.go:9", path("~/proj/main.go", Some(9), None))),
            ),
            (
                "Foo.cs(12,5): error",
                "Foo",
                Some(("Foo.cs(12,5)", path("Foo.cs", Some(12), Some(5)))),
            ),
            (
                "Foo.cs(12)",
                "Foo",
                Some(("Foo.cs(12)", path("Foo.cs", Some(12), None))),
            ),
            (
                "Makefile:3: recipe",
                "Makefile",
                Some(("Makefile:3", path("Makefile", Some(3), None))),
            ),
            (
                "File \"/tmp/x.py\", line 7, in <module>",
                "/tmp",
                Some(("/tmp/x.py", path("/tmp/x.py", Some(7), None))),
            ),
            (
                "visit https://example.com/a/b?q=1.",
                "https",
                Some((
                    "https://example.com/a/b?q=1",
                    Candidate::Url("https://example.com/a/b?q=1".into()),
                )),
            ),
            (
                "(https://example.com/x), and",
                "https",
                Some((
                    "https://example.com/x",
                    Candidate::Url("https://example.com/x".into()),
                )),
            ),
            (
                "https://en.wikipedia.org/wiki/Rust_(language).",
                "wiki",
                Some((
                    "https://en.wikipedia.org/wiki/Rust_(language)",
                    Candidate::Url("https://en.wikipedia.org/wiki/Rust_(language)".into()),
                )),
            ),
            (
                "<https://example.com/z>",
                "example",
                Some((
                    "https://example.com/z",
                    Candidate::Url("https://example.com/z".into()),
                )),
            ),
            (
                "url=http://localhost:3000/x;",
                "localhost",
                Some((
                    "http://localhost:3000/x",
                    Candidate::Url("http://localhost:3000/x".into()),
                )),
            ),
            // Not links.
            ("plain words here", "words", None),
            (
                "version 1:2 done",
                "1:2",
                Some(("1:2", path("1", Some(2), None))),
            ),
            ("http:// nothing", "http", None),
            ("e.g. this", "e.g", Some(("e.g", path("e.g", None, None)))),
            ("(((", "(", None),
        ];
        for (text, needle, want) in cases {
            let got = found(text, needle);
            let want = want.as_ref().map(|(t, c)| ((*t).to_owned(), c.clone()));
            assert_eq!(got, want, "{text:?} at {needle:?}");
        }
    }

    #[test]
    fn only_the_hovered_link_of_a_line_is_found() {
        let text = "a.rs:1 and https://x.io/y b.rs:2";
        let (t, _) = found(text, "x.io").unwrap();
        assert_eq!(t, "https://x.io/y");
        let (t, _) = found(text, "b.rs").unwrap();
        assert_eq!(t, "b.rs:2");
        // On the space between, and on the wrapping punctuation, there is none.
        assert_eq!(found(text, " and"), None);
    }

    #[test]
    fn a_url_is_found_from_any_of_its_characters() {
        let text = "go https://example.com/path now";
        let start = text.find("https").unwrap();
        for at in start..start + "https://example.com/path".len() {
            let f = candidate_at(text, at).expect("inside the url");
            assert_eq!(&text[f.range], "https://example.com/path");
        }
        assert_eq!(candidate_at(text, start - 1), None);
    }

    #[test]
    fn text_before_a_url_in_its_token_is_not_part_of_it() {
        // `url=` is not a link; only the URL after it is.
        assert_eq!(found("url=https://a.io/x", "url"), None);
    }

    #[test]
    fn multibyte_text_does_not_panic() {
        for text in [
            "héllo wörld.rs:3 ✓",
            "日本語/ファイル.rs:12:3",
            "https://例え.jp/パス。",
        ] {
            for (at, _) in text.char_indices() {
                let _ = candidate_at(text, at);
            }
        }
        let (t, c) = found("日本語/ファイル.rs:12:3", "日").unwrap();
        assert_eq!(t, "日本語/ファイル.rs:12:3");
        assert_eq!(c, path("日本語/ファイル.rs", Some(12), Some(3)));
    }

    #[test]
    fn resolve_expands_home_and_joins_relative_paths() {
        let home = Path::new("/Users/me");
        let cwd = Path::new("/repo");
        assert_eq!(
            resolve("~/a.rs", Some(cwd), Some(home)),
            Some("/Users/me/a.rs".into())
        );
        assert_eq!(
            resolve("~", Some(cwd), Some(home)),
            Some("/Users/me".into())
        );
        assert_eq!(resolve("/etc/hosts", None, None), Some("/etc/hosts".into()));
        assert_eq!(
            resolve("src/a.rs", Some(cwd), None),
            Some("/repo/src/a.rs".into())
        );
        assert_eq!(resolve("src/a.rs", None, Some(home)), None);
        assert_eq!(resolve("~/a.rs", Some(cwd), None), None);
    }
}
