//! How well a path answers a typed query (FX-WORK-2, N-02 and N-12): one scorer for the Files
//! panel's filter and ⌘P, so the same query orders the same way in both.
//!
//! A word of the query is judged against the path in classes, best first:
//!
//! 0. the whole name of the file, or the name without its extension (`glue` → `glue.rs`);
//! 1. the start of the name (`glu` → `glue.rs`);
//! 2. somewhere in the name (`lue` → `glue.rs`);
//! 3. the start of the path, or of one of its folders (`src/ch04` → `src/ch04-01.md`);
//! 4. somewhere in the path, as it was typed (`rc/ch04`);
//! 5. only as scattered letters (`gle` → `.github/ISSUE_TEMPLATE/…`).
//!
//! The query's class is its worst word's: `searcher glue` is a path match until both words are in
//! the name. Within a class a shorter path wins, then the alphabet. Case is ignored: the daemon's
//! smart case decides what matches, this only orders what matched.

use std::cmp::Ordering;

/// How well one path answers a query; lower is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rank {
    /// 0 (the file's own name) to 5 (scattered letters), see the module docs.
    pub class: u8,
    /// Characters of the path: shorter first within a class.
    pub len: usize,
}

/// The class of one word of the query against `path` (module docs).
fn word_class(word: &str, lower: &str) -> u8 {
    let word = word.to_lowercase();
    let (folder, name) = match lower.rfind('/') {
        Some(at) => (&lower[..=at], &lower[at + 1..]),
        None => ("", lower),
    };
    let stem = match name.rfind('.') {
        Some(at) if at > 0 => &name[..at],
        _ => name,
    };
    if name == word || stem == word {
        0
    } else if name.starts_with(&word) {
        1
    } else if name.contains(&word) {
        2
    } else if lower.starts_with(&word)
        || folder
            .split('/')
            .any(|segment| !segment.is_empty() && segment.starts_with(&word))
        || folder.contains(&format!("/{word}"))
    {
        3
    } else if lower.contains(&word) {
        4
    } else {
        5
    }
}

/// How well `path` answers `query` (words separated by white space).
pub fn rank(query: &str, path: &str) -> Rank {
    let lower = path.to_lowercase();
    let class = query
        .split_whitespace()
        .map(|word| word_class(word, &lower))
        .max()
        .unwrap_or(5);
    Rank {
        class,
        len: path.chars().count(),
    }
}

/// Orders two paths by [`rank`], then the alphabet.
pub fn compare(query: &str, a: &str, b: &str) -> Ordering {
    rank(query, a).cmp(&rank(query, b)).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(query: &str, path: &str) -> u8 {
        rank(query, path).class
    }

    #[test]
    fn the_file_name_beats_a_path_match() {
        assert_eq!(class("glue", "crates/searcher/src/glue.rs"), 0);
        assert_eq!(class("glue.rs", "crates/searcher/src/glue.rs"), 0);
        assert_eq!(class("glu", "crates/searcher/src/glue.rs"), 1);
        assert_eq!(class("lue", "crates/searcher/src/glue.rs"), 2);
        assert_eq!(class("searcher", "crates/searcher/src/glue.rs"), 3);
        assert_eq!(class("earch", "crates/searcher/src/glue.rs"), 4);
        assert_eq!(class("gle", "crates/searcher/src/glue.rs"), 5);
        assert_eq!(class("gue", ".github/ISSUE_TEMPLATE/bug_report.yml"), 5);
        // The N-02 order: the file first, the scattered matches after it.
        let mut paths = [
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            "crates/ignore/src/glue.rs",
            "UNLICENSE",
            "pkg/deb/prelude.fish",
        ];
        paths.sort_by(|a, b| compare("glue", a, b));
        assert_eq!(paths[0], "crates/ignore/src/glue.rs");
    }

    #[test]
    fn the_worst_word_decides_and_case_is_ignored() {
        assert_eq!(class("searcher glue", "crates/searcher/src/glue.rs"), 3);
        assert_eq!(class("Glue", "crates/searcher/src/GLUE.rs"), 0);
        assert_eq!(class("", "a"), 5, "no words, no relevance");
    }

    #[test]
    fn a_path_that_starts_with_the_query_beats_one_that_only_holds_it() {
        // N-12: `src/…` before `second-edition/src/…`.
        let query = "src/ch04-01-what";
        assert_eq!(class(query, "src/ch04-01-what-is-ownership.md"), 3);
        assert_eq!(
            class(query, "second-edition/src/ch04-01-what-is-ownership.md"),
            4
        );
        let mut paths = [
            "second-edition/src/ch04-01-what-is-ownership.md",
            "src/ch04-01-what-is-ownership.md",
        ];
        paths.sort_by(|a, b| compare(query, a, b));
        assert_eq!(paths[0], "src/ch04-01-what-is-ownership.md");
    }

    #[test]
    fn within_a_class_the_shorter_path_wins() {
        let mut paths = ["a/b/c/main.rs", "xmain.rs", "src/main.rs"];
        paths.sort_by(|a, b| compare("main.rs", a, b));
        // The class first (the file's own name, then a name that holds it), then the length.
        assert_eq!(paths, ["src/main.rs", "a/b/c/main.rs", "xmain.rs"]);
    }
}
