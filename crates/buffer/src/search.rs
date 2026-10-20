use std::ops::Range;

use regex_cursor::engines::meta::Regex;
use regex_cursor::regex_automata::util::syntax;
use regex_cursor::{Cursor, Input};

use crate::error::BufferError;
use crate::text::Text;

/// Longest search pattern accepted (bytes).
pub const MAX_PATTERN_BYTES: usize = 4 * 1024;
/// Hard cap on the matches one search returns, whatever limit the caller passes.
pub const MAX_SEARCH_MATCHES: usize = 100_000;
/// Compiled regex programs are refused above this size (bytes).
const REGEX_SIZE_LIMIT: usize = 8 << 20;

/// What to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    pattern: String,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
}

impl SearchQuery {
    /// Finds `text` literally, case-sensitively.
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            pattern: text.into(),
            regex: false,
            case_sensitive: true,
            whole_word: false,
        }
    }

    /// Finds matches of a regular expression (Rust `regex` syntax, Unicode, `^`/`$` match at line
    /// boundaries), case-sensitively. Matching time is linear in the searched text.
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            regex: true,
            case_sensitive: true,
            whole_word: false,
        }
    }

    /// Sets case sensitivity (default: sensitive).
    #[must_use]
    pub fn case_sensitive(mut self, yes: bool) -> Self {
        self.case_sensitive = yes;
        self
    }

    /// Only matches delimited by word boundaries (`\b`).
    #[must_use]
    pub fn whole_word(mut self, yes: bool) -> Self {
        self.whole_word = yes;
        self
    }

    fn compile(&self) -> Result<Regex, BufferError> {
        if self.pattern.is_empty() {
            return Err(BufferError::Pattern("the pattern is empty".into()));
        }
        if self.pattern.len() > MAX_PATTERN_BYTES {
            return Err(BufferError::Pattern(format!(
                "the pattern is longer than {MAX_PATTERN_BYTES} bytes"
            )));
        }
        let mut pattern = if self.regex {
            self.pattern.clone()
        } else {
            escape(&self.pattern)
        };
        if self.whole_word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        // The literal prefilter of regex-cursor 0.1.5 loses matches that straddle a chunk
        // boundary inside a multi-byte sequence (a 3-byte literal split after its first byte),
        // so it stays off; the lazy DFA still scans 10 MB in a few milliseconds.
        Regex::builder()
            .syntax(
                syntax::Config::new()
                    .case_insensitive(!self.case_sensitive)
                    .multi_line(true),
            )
            .configure(
                Regex::config()
                    .auto_prefilter(false)
                    .nfa_size_limit(Some(REGEX_SIZE_LIMIT))
                    .hybrid_cache_capacity(2 << 20),
            )
            .build(&pattern)
            .map_err(|e| BufferError::Pattern(e.to_string()))
    }
}

/// Escapes the characters that are special in the regex syntax (`regex_syntax::escape`).
fn escape(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len() + 8);
    for c in literal.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The result of a search: byte ranges in order, and whether more matches exist past the limit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchMatches {
    /// Matches in document order (non-overlapping).
    pub matches: Vec<Range<usize>>,
    /// `true` when the search stopped at the limit with text left to search.
    pub truncated: bool,
}

/// A cursor over a text's chunks (crop's chunks never split a character, and the list allows
/// stepping back as the regex engine needs).
struct ChunkCursor<'a> {
    chunks: Vec<&'a [u8]>,
    starts: Vec<usize>,
    index: usize,
    total: usize,
}

impl<'a> ChunkCursor<'a> {
    fn new(text: &'a Text) -> Self {
        let mut chunks = Vec::new();
        let mut starts = Vec::new();
        let mut at = 0;
        for chunk in text.chunks().filter(|c| !c.is_empty()) {
            chunks.push(chunk.as_bytes());
            starts.push(at);
            at += chunk.len();
        }
        Self {
            chunks,
            starts,
            index: 0,
            total: at,
        }
    }
}

impl Cursor for ChunkCursor<'_> {
    fn chunk(&self) -> &[u8] {
        self.chunks.get(self.index).copied().unwrap_or(&[])
    }

    fn advance(&mut self) -> bool {
        if self.index + 1 < self.chunks.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn backtrack(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    fn total_bytes(&self) -> Option<usize> {
        Some(self.total)
    }

    fn offset(&self) -> usize {
        self.starts.get(self.index).copied().unwrap_or(0)
    }
}

impl Text {
    /// Searches `range` of the text for `query` without flattening the rope, returning at most
    /// `limit` matches (and never more than [`MAX_SEARCH_MATCHES`]). Context outside `range` is
    /// visible to `^`, `$` and `\b`, but matches lie inside it.
    pub fn search(
        &self,
        query: &SearchQuery,
        range: Range<usize>,
        limit: usize,
    ) -> Result<SearchMatches, BufferError> {
        self.check_range(&range)?;
        let regex = query.compile()?;
        let limit = limit.min(MAX_SEARCH_MATCHES);
        let input = Input::new(ChunkCursor::new(self)).range(range);
        let mut matches = Vec::new();
        let mut truncated = false;
        for m in regex.find_iter(input) {
            if matches.len() == limit {
                truncated = true;
                break;
            }
            matches.push(m.start()..m.end());
        }
        Ok(SearchMatches { matches, truncated })
    }

    /// Every match in the whole text, bounded like [`search`](Self::search).
    pub fn search_all(
        &self,
        query: &SearchQuery,
        limit: usize,
    ) -> Result<SearchMatches, BufferError> {
        self.search(query, 0..self.len(), limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str, q: &SearchQuery) -> Vec<Range<usize>> {
        Text::from(text).search_all(q, 100).unwrap().matches
    }

    #[test]
    fn literal_is_not_a_regex() {
        assert_eq!(
            all("a.b axb a.b", &SearchQuery::literal("a.b")),
            vec![0..3, 8..11]
        );
        assert_eq!(all("(x) [y]", &SearchQuery::literal("(x)")), vec![0..3]);
    }

    #[test]
    fn regex_case_and_words() {
        assert_eq!(
            all(
                "Foo foo FOO",
                &SearchQuery::literal("foo").case_sensitive(false)
            ),
            vec![0..3, 4..7, 8..11]
        );
        assert_eq!(
            all(
                "cat concat cat",
                &SearchQuery::literal("cat").whole_word(true)
            ),
            vec![0..3, 11..14]
        );
        assert_eq!(
            all("a1 b22 c333", &SearchQuery::regex(r"\d+")),
            vec![1..2, 4..6, 8..11]
        );
        assert_eq!(
            all("x\nfoo\nfoobar", &SearchQuery::regex(r"^foo$")),
            vec![2..5]
        );
    }

    #[test]
    fn limits_and_truncation() {
        let t = Text::from("aaaaaaaa");
        let r = t.search_all(&SearchQuery::literal("a"), 3).unwrap();
        assert_eq!(r.matches.len(), 3);
        assert!(r.truncated);
        let r = t.search_all(&SearchQuery::literal("a"), 8).unwrap();
        assert!(!r.truncated);
    }

    #[test]
    fn range_keeps_context_and_matches_inside() {
        let t = Text::from("foobar foo");
        let q = SearchQuery::literal("foo").whole_word(true);
        assert_eq!(t.search(&q, 3..10, 10).unwrap().matches, vec![7..10]);
        assert_eq!(
            t.search(&q, 0..6, 10).unwrap().matches,
            Vec::<Range<usize>>::new()
        );
    }

    #[test]
    fn matches_span_chunks_and_unicode() {
        let mut s = String::new();
        for i in 0..5000 {
            s.push_str(&format!("line {i} é🦀\n"));
        }
        let t = Text::from(s.as_str());
        let hits = t
            .search_all(&SearchQuery::literal("4999 é🦀"), 5)
            .unwrap()
            .matches;
        assert_eq!(hits.len(), 1);
        assert_eq!(t.slice(hits[0].clone()).unwrap(), "4999 é🦀");
        let all_lines = t.search_all(&SearchQuery::regex("🦀$"), 10_000).unwrap();
        assert_eq!(all_lines.matches.len(), 5000);
    }

    #[test]
    fn multibyte_literals_straddling_chunks_are_found() {
        // regex-cursor's literal prefilter loses matches that straddle a chunk boundary inside a
        // multi-byte sequence; the search runs with the prefilter off (see `compile`).
        let unit = "👩\u{200d}👩\u{200d}👧";
        for n in (50..70).chain(1000..1010) {
            let s = unit.repeat(n);
            let t = Text::from(s.as_str());
            assert_eq!(
                t.search_all(&SearchQuery::literal(unit), 10_000)
                    .unwrap()
                    .matches
                    .len(),
                n,
                "n = {n}"
            );
        }
    }

    #[test]
    fn a_match_at_the_far_end_of_a_big_text_is_found() {
        let mut s = String::with_capacity(5 << 20);
        while s.len() < 4 << 20 {
            s.push_str("    let value = compute(&state)?; // héllo 🦀\n");
        }
        let start = s.len();
        s.push_str("needle-at-the-end");
        let t = Text::from(s.as_str());
        assert_eq!(
            t.search_all(&SearchQuery::literal("needle-at-the-end"), 10)
                .unwrap()
                .matches,
            vec![start..start + 17]
        );
        assert_eq!(
            t.search_all(&SearchQuery::regex("end$"), 10)
                .unwrap()
                .matches,
            vec![start + 14..start + 17]
        );
    }

    #[test]
    fn bad_patterns_are_errors() {
        let t = Text::from("x");
        assert!(matches!(
            t.search_all(&SearchQuery::regex("("), 1),
            Err(BufferError::Pattern(_))
        ));
        assert!(matches!(
            t.search_all(&SearchQuery::literal(""), 1),
            Err(BufferError::Pattern(_))
        ));
        let long = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert!(matches!(
            t.search_all(&SearchQuery::literal(long), 1),
            Err(BufferError::Pattern(_))
        ));
        assert!(matches!(
            t.search(&SearchQuery::literal("x"), 0..9, 1),
            Err(BufferError::OutOfBounds { .. })
        ));
    }
}
