//! Find in file: the query, its matches and which one is current. The search itself runs on a
//! background thread over an `O(1)` snapshot of the rope ([`run`]); this module only keeps the
//! answer and steps through it.

use std::ops::Range;

use hutan_buffer::{MAX_SEARCH_MATCHES, SearchQuery, Text};

/// Most matches one search keeps (the count says "10000+" past it).
pub const MAX_FIND_MATCHES: usize = 10_000;

/// What to look for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindQuery {
    pub text: String,
    /// Match case (off: `foo` finds `Foo`).
    pub case_sensitive: bool,
    /// `text` is a regular expression (Rust `regex` syntax).
    pub regex: bool,
}

impl FindQuery {
    pub fn literal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }
}

/// The answer to a [`FindQuery`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindResult {
    pub matches: Vec<Range<usize>>,
    /// More matches exist than [`MAX_FIND_MATCHES`].
    pub truncated: bool,
    /// Why the pattern was refused (a regular expression that does not compile).
    pub error: Option<String>,
}

fn search_query(query: &FindQuery) -> SearchQuery {
    if query.regex {
        SearchQuery::regex(query.text.clone())
    } else {
        SearchQuery::literal(query.text.clone())
    }
    .case_sensitive(query.case_sensitive)
}

/// Searches `text` for `query` (blocking: call it off the main thread).
pub fn run(text: &Text, query: &FindQuery) -> FindResult {
    if query.text.is_empty() {
        return FindResult::default();
    }
    match text.search_all(&search_query(query), MAX_FIND_MATCHES) {
        Ok(found) => FindResult {
            matches: found.matches,
            truncated: found.truncated,
            error: None,
        },
        Err(error) => FindResult {
            error: Some(error.to_string()),
            ..FindResult::default()
        },
    }
}

/// Every match of `query` in `text`, up to [`MAX_SEARCH_MATCHES`] (what Replace All replaces);
/// empty for an empty query or a pattern that does not compile.
pub fn run_all(text: &Text, query: &FindQuery) -> Vec<Range<usize>> {
    if query.text.is_empty() {
        return Vec::new();
    }
    text.search_all(&search_query(query), MAX_SEARCH_MATCHES)
        .map(|found| found.matches)
        .unwrap_or_default()
}

/// The find state of one view.
#[derive(Debug, Clone, Default)]
pub struct FindState {
    pub query: FindQuery,
    pub result: FindResult,
    /// Index into `result.matches`.
    pub current: Option<usize>,
    /// A search for `query` is running.
    pub searching: bool,
    /// Bumped by every new query: a search that finishes for an older one is dropped.
    pub generation: u64,
    /// The text version `result` was searched in.
    pub version: u64,
}

impl FindState {
    /// The match to show first: the first one at or after `from` (the top of the view), else
    /// the first.
    pub fn first_from(&self, from: usize) -> Option<usize> {
        if self.result.matches.is_empty() {
            return None;
        }
        let at = self.result.matches.partition_point(|m| m.start < from);
        Some(if at < self.result.matches.len() {
            at
        } else {
            0
        })
    }

    /// Steps to the next match (`forward`) or the previous one, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<Range<usize>> {
        let count = self.result.matches.len();
        if count == 0 {
            return None;
        }
        let next = match self.current {
            None => 0,
            Some(i) if forward => (i + 1) % count,
            Some(i) => (i + count - 1) % count,
        };
        self.current = Some(next);
        self.result.matches.get(next).cloned()
    }

    /// The current match.
    pub fn current_match(&self) -> Option<Range<usize>> {
        self.result.matches.get(self.current?).cloned()
    }

    /// `3 of 41`, `No matches`, `10000+ matches`, the pattern error.
    pub fn summary(&self) -> String {
        if let Some(error) = &self.result.error {
            return error.clone();
        }
        if self.query.text.is_empty() {
            return String::new();
        }
        if self.searching && self.result.matches.is_empty() {
            return "Searching…".to_owned();
        }
        let count = self.result.matches.len();
        let total = if self.result.truncated {
            format!("{count}+")
        } else {
            count.to_string()
        };
        match (count, self.current) {
            (0, _) => "No matches".to_owned(),
            (_, Some(i)) => format!("{} of {total}", i + 1),
            (_, None) => format!("{total} matches"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_search_ignores_case_unless_asked() {
        let text = Text::from("Foo foo FOO\nbar");
        assert_eq!(run(&text, &FindQuery::literal("foo")).matches.len(), 3);
        let strict = FindQuery {
            case_sensitive: true,
            ..FindQuery::literal("foo")
        };
        assert_eq!(run(&text, &strict).matches, vec![4..7]);
        let regex = FindQuery {
            regex: true,
            ..FindQuery::literal("^bar$")
        };
        assert_eq!(run(&text, &regex).matches, vec![12..15]);
        let broken = FindQuery {
            regex: true,
            ..FindQuery::literal("(")
        };
        assert!(run(&text, &broken).error.is_some());
    }

    #[test]
    fn stepping_wraps_and_starts_at_the_view() {
        let mut state = FindState {
            result: FindResult {
                matches: vec![0..1, 10..11, 20..21],
                ..FindResult::default()
            },
            query: FindQuery::literal("x"),
            ..FindState::default()
        };
        assert_eq!(state.first_from(5), Some(1));
        assert_eq!(state.first_from(25), Some(0));
        state.current = Some(2);
        assert_eq!(state.step(true), Some(0..1));
        assert_eq!(state.step(false), Some(20..21));
        assert_eq!(state.summary(), "3 of 3");
    }
}
