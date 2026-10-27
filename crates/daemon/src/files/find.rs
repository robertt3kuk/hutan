//! `fs.find`: a fuzzy file finder over a bounded per-root index. Blocking work.
//!
//! The index is the list of files below a root (hidden files in; `.git`, `target` and
//! `node_modules` out; `.gitignore` rules honoured), at most [`FIND_INDEX_MAX_PATHS`] of them.
//! It is built on first use and kept while the root's watch generation is unchanged (or, when
//! nobody watches the root, for [`UNWATCHED_TTL`]). Matching scores a query as a subsequence of
//! the path with the bonuses fzy gives (after `/`, word starts, camel-case humps, runs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_protocol::fs::{FIND_INDEX_MAX_PATHS, FIND_MAX, FsFindMatch};
use parking_lot::Mutex;

/// How long an index of an unwatched root is trusted.
pub const UNWATCHED_TTL: Duration = Duration::from_secs(5);
/// Longest query looked at, in characters.
const QUERY_MAX_CHARS: usize = 128;
/// Longest path scored; a longer one cannot be a sensible hit.
const PATH_MAX_CHARS: usize = 1024;

/// Directory names the walks of the file service never enter.
pub fn is_noise_dir(name: &std::ffi::OsStr) -> bool {
    name == ".git" || name == "target" || name == "node_modules"
}

/// The files below one root, as `/`-separated paths relative to it.
#[derive(Debug)]
pub struct Index {
    built: Instant,
    /// The watch generation the index was built at; `None` when the root was not watched.
    generation: Option<u64>,
    paths: Vec<String>,
    truncated: bool,
}

impl Index {
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Walks `root`.
    pub fn build(root: &Path, generation: Option<u64>) -> Self {
        let mut paths = Vec::new();
        let mut truncated = false;
        let walk = ignore::WalkBuilder::new(root)
            .hidden(false)
            .follow_links(false)
            .filter_entry(|entry| {
                !(entry.depth() > 0
                    && entry.file_type().is_some_and(|t| t.is_dir())
                    && is_noise_dir(entry.file_name()))
            })
            .build();
        for entry in walk.filter_map(Result::ok) {
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(root) else {
                continue;
            };
            let Some(relative) = relative.to_str() else {
                continue;
            };
            if paths.len() >= FIND_INDEX_MAX_PATHS {
                truncated = true;
                break;
            }
            paths.push(relative.to_owned());
        }
        paths.sort();
        Self {
            built: Instant::now(),
            generation,
            paths,
            truncated,
        }
    }

    fn fresh(&self, generation: Option<u64>) -> bool {
        match (generation, self.generation) {
            (Some(now), Some(then)) => now == then,
            (None, None) => self.built.elapsed() < UNWATCHED_TTL,
            _ => false,
        }
    }

    /// The best `limit` paths for `query`. Words separated by spaces must all match (each as a
    /// subsequence of the path, in any order, like Zed and VS Code): `searcher glue` finds
    /// `crates/searcher/src/searcher/glue.rs`. A path's score is the sum of its words'.
    pub fn find(&self, query: &str, limit: u32) -> Vec<FsFindMatch> {
        let limit = limit.clamp(1, FIND_MAX) as usize;
        let query: String = query.chars().take(QUERY_MAX_CHARS).collect();
        let words: Vec<Vec<char>> = query
            .split_whitespace()
            .map(|word| word.chars().collect())
            .collect();
        if words.is_empty() {
            return self
                .paths
                .iter()
                .take(limit)
                .map(|path| FsFindMatch {
                    path: path.clone(),
                    score: 0,
                    matched: Vec::new(),
                })
                .collect();
        }
        // Smart case, per word: a word with an upper-case letter is exact about case.
        let mut scorers: Vec<Scorer> = words
            .iter()
            .map(|word| Scorer::new(word, word.iter().any(|c| c.is_uppercase())))
            .collect();
        let mut scored: Vec<(u32, usize)> = Vec::new();
        'paths: for (index, path) in self.paths.iter().enumerate() {
            let mut total = 0_u32;
            for scorer in &mut scorers {
                match scorer.score(path) {
                    Some(score) => total = total.saturating_add(score),
                    None => continue 'paths,
                }
            }
            scored.push((total, index));
        }
        // Best first; a shorter path, then the alphabet, break ties.
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| self.paths[a.1].len().cmp(&self.paths[b.1].len()))
                .then_with(|| self.paths[a.1].cmp(&self.paths[b.1]))
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, index)| {
                let path = &self.paths[index];
                let mut matched: Vec<[u32; 2]> = scorers
                    .iter_mut()
                    .flat_map(|scorer| scorer.ranges(path))
                    .collect();
                FsFindMatch {
                    path: path.clone(),
                    score,
                    matched: merge_ranges(&mut matched),
                }
            })
            .collect()
    }
}

/// `ranges` sorted, with the ones that overlap or touch joined.
fn merge_ranges(ranges: &mut [[u32; 2]]) -> Vec<[u32; 2]> {
    ranges.sort_unstable();
    let mut merged: Vec<[u32; 2]> = Vec::with_capacity(ranges.len());
    for range in ranges.iter() {
        match merged.last_mut() {
            Some(last) if range[0] <= last[1] => last[1] = last[1].max(range[1]),
            _ => merged.push(*range),
        }
    }
    merged
}

/// Indexes of the roots, kept between requests.
#[derive(Debug, Default)]
pub struct Finder {
    indexes: Mutex<HashMap<PathBuf, Arc<Index>>>,
}

/// Roots whose index is kept at once; the least recently built goes first.
const INDEXES_MAX: usize = 8;

impl Finder {
    /// The index of `root` (canonical): the kept one while it is fresh, else a new one.
    /// `generation` is the root's watch generation when somebody watches it.
    pub fn index(&self, root: &Path, generation: Option<u64>) -> Arc<Index> {
        if let Some(kept) = self.indexes.lock().get(root)
            && kept.fresh(generation)
        {
            return kept.clone();
        }
        // Built without the lock: a walk takes a while, and a second request for the same
        // root may build it twice, which only costs time.
        let built = Arc::new(Index::build(root, generation));
        let mut indexes = self.indexes.lock();
        if indexes.len() >= INDEXES_MAX
            && !indexes.contains_key(root)
            && let Some(oldest) = indexes
                .iter()
                .min_by_key(|(_, index)| index.built)
                .map(|(path, _)| path.clone())
        {
            indexes.remove(&oldest);
        }
        indexes.insert(root.to_owned(), built.clone());
        built
    }
}

// Scoring ------------------------------------------------------------------------------------

const SCORE_GAP_LEADING: f64 = -0.005;
const SCORE_GAP_TRAILING: f64 = -0.005;
const SCORE_GAP_INNER: f64 = -0.01;
const SCORE_MATCH_CONSECUTIVE: f64 = 1.0;
const SCORE_MATCH_SLASH: f64 = 0.9;
const SCORE_MATCH_WORD: f64 = 0.8;
const SCORE_MATCH_CAPITAL: f64 = 0.7;
const SCORE_MATCH_DOT: f64 = 0.6;
/// Added when the whole query matches inside the file name.
const SCORE_IN_NAME: f64 = 1.5;

/// fzy's subsequence scorer with reusable buffers.
struct Scorer {
    query: Vec<char>,
    case_sensitive: bool,
    /// `d[i][j]`: best score of a match of `query[..=i]` that ends with `path[j]` matched.
    d: Vec<f64>,
    /// `m[i][j]`: best score of `query[..=i]` within `path[..=j]`.
    m: Vec<f64>,
    bonus: Vec<f64>,
    chars: Vec<char>,
    /// Byte offset of each of `chars`.
    offsets: Vec<usize>,
}

impl Scorer {
    fn new(query: &[char], case_sensitive: bool) -> Self {
        let query = if case_sensitive {
            query.to_vec()
        } else {
            query.iter().map(|c| fold(*c)).collect()
        };
        Self {
            query,
            case_sensitive,
            d: Vec::new(),
            m: Vec::new(),
            bonus: Vec::new(),
            chars: Vec::new(),
            offsets: Vec::new(),
        }
    }

    fn load(&mut self, path: &str) -> bool {
        self.chars.clear();
        self.offsets.clear();
        for (offset, c) in path.char_indices().take(PATH_MAX_CHARS + 1) {
            self.chars.push(c);
            self.offsets.push(offset);
        }
        self.chars.len() <= PATH_MAX_CHARS && self.chars.len() >= self.query.len()
    }

    fn is_subsequence(&self) -> bool {
        let mut wanted = self.query.iter();
        let mut next = wanted.next();
        for c in &self.chars {
            let Some(want) = next else { return true };
            let c = if self.case_sensitive { *c } else { fold(*c) };
            if c == *want {
                next = wanted.next();
            }
        }
        next.is_none()
    }

    /// The score of `path` in thousandths, or `None` when the query is not a subsequence.
    fn score(&mut self, path: &str) -> Option<u32> {
        if !self.load(path) || !self.is_subsequence() {
            return None;
        }
        let (n, m) = (self.chars.len(), self.query.len());
        self.fill_bonus();
        self.d.clear();
        self.d.resize(n * m, f64::NEG_INFINITY);
        self.m.clear();
        self.m.resize(n * m, f64::NEG_INFINITY);
        for i in 0..m {
            let mut previous_best = f64::NEG_INFINITY;
            let gap = if i == m - 1 {
                SCORE_GAP_TRAILING
            } else {
                SCORE_GAP_INNER
            };
            for j in 0..n {
                let c = if self.case_sensitive {
                    self.chars[j]
                } else {
                    fold(self.chars[j])
                };
                let score = if c == self.query[i] {
                    if i == 0 {
                        j as f64 * SCORE_GAP_LEADING + self.bonus[j]
                    } else if j > 0 {
                        let diagonal = self.m[(i - 1) * n + (j - 1)];
                        let run = self.d[(i - 1) * n + (j - 1)];
                        (diagonal + self.bonus[j]).max(run + SCORE_MATCH_CONSECUTIVE)
                    } else {
                        f64::NEG_INFINITY
                    }
                } else {
                    f64::NEG_INFINITY
                };
                self.d[i * n + j] = score;
                previous_best = score.max(previous_best + gap);
                self.m[i * n + j] = previous_best;
            }
        }
        let best = self.m[(m - 1) * n + (n - 1)];
        if !best.is_finite() {
            return None;
        }
        let mut score = best;
        if self.matches_inside_name() {
            score += SCORE_IN_NAME;
        }
        // Negative scores (long paths, big gaps) still rank; shift them above zero.
        Some(((score + 10.0).max(0.0) * 1000.0) as u32)
    }

    fn fill_bonus(&mut self) {
        self.bonus.clear();
        let mut last = '/';
        for c in &self.chars {
            self.bonus.push(match last {
                '/' => SCORE_MATCH_SLASH,
                '-' | '_' | ' ' => SCORE_MATCH_WORD,
                '.' => SCORE_MATCH_DOT,
                l if l.is_lowercase() && c.is_uppercase() => SCORE_MATCH_CAPITAL,
                _ => 0.0,
            });
            last = *c;
        }
    }

    /// Whether the query is a subsequence of the part after the last `/`.
    fn matches_inside_name(&self) -> bool {
        let start = self
            .chars
            .iter()
            .rposition(|c| *c == '/')
            .map_or(0, |slash| slash + 1);
        let mut wanted = self.query.iter();
        let mut next = wanted.next();
        for c in &self.chars[start..] {
            let Some(want) = next else { return true };
            let c = if self.case_sensitive { *c } else { fold(*c) };
            if c == *want {
                next = wanted.next();
            }
        }
        next.is_none()
    }

    /// The byte ranges of `path` the best match uses, merged where they touch.
    fn ranges(&mut self, path: &str) -> Vec<[u32; 2]> {
        if self.score(path).is_none() {
            return Vec::new();
        }
        let (n, m) = (self.chars.len(), self.query.len());
        // Walk the matrices back: from the best end, take a match at (i, j) when `d` is what
        // `m` holds, else step left.
        let mut positions = vec![0_usize; m];
        let mut j = n - 1;
        let mut match_required = false;
        for i in (0..m).rev() {
            loop {
                let d = self.d[i * n + j];
                let best = self.m[i * n + j];
                if d.is_finite() && (match_required || (d - best).abs() < 1e-9) {
                    match_required = i > 0
                        && j > 0
                        && (best - (self.d[(i - 1) * n + (j - 1)] + SCORE_MATCH_CONSECUTIVE)).abs()
                            < 1e-9;
                    positions[i] = j;
                    j = j.saturating_sub(1);
                    break;
                }
                if j == 0 {
                    break;
                }
                j -= 1;
            }
        }
        let mut ranges: Vec<[u32; 2]> = Vec::new();
        for position in positions {
            let start = self.offsets[position];
            let end = self
                .offsets
                .get(position + 1)
                .copied()
                .unwrap_or(path.len());
            match ranges.last_mut() {
                Some(last) if last[1] as usize == start => last[1] = end as u32,
                _ => ranges.push([start as u32, end as u32]),
            }
        }
        ranges
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(paths: &[&str]) -> Index {
        let mut paths: Vec<String> = paths.iter().map(|p| (*p).to_owned()).collect();
        paths.sort();
        Index {
            built: Instant::now(),
            generation: None,
            paths,
            truncated: false,
        }
    }

    fn first(found: &[FsFindMatch]) -> &str {
        &found[0].path
    }

    #[test]
    fn a_query_is_a_subsequence_and_an_upper_case_letter_makes_it_exact_about_case() {
        let index = index(&[
            "crates/daemon/src/router.rs",
            "crates/daemon/src/registry.rs",
            "crates/protocol/src/messages.rs",
            "docs/router-notes.md",
            "README.md",
        ]);
        let mut found: Vec<String> = index.find("rout", 10).into_iter().map(|m| m.path).collect();
        found.sort();
        assert_eq!(
            found,
            ["crates/daemon/src/router.rs", "docs/router-notes.md"]
        );
        assert_eq!(first(&index.find("READ", 10)), "README.md");
        assert_eq!(first(&index.find("readme", 10)), "README.md");
        assert!(index.find("Router", 10).is_empty());
        assert!(index.find("zzz", 10).is_empty());
        // Contiguous runs and word starts rank higher than scattered letters.
        let ranked = index.find("msg", 10);
        assert_eq!(first(&ranked), "crates/protocol/src/messages.rs");
    }

    /// F-13: words separated by spaces are AND-matched, in any order.
    #[test]
    fn words_separated_by_spaces_must_all_match() {
        let index = index(&[
            "crates/searcher/src/searcher/glue.rs",
            "crates/searcher/src/lib.rs",
            "crates/printer/src/glue.rs",
            "docs/glue-notes.md",
        ]);
        let found = index.find("searcher glue", 10);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(first(&found), "crates/searcher/src/searcher/glue.rs");
        // Order does not matter, extra spaces do not either.
        assert_eq!(
            first(&index.find("  glue   searcher ", 10)),
            "crates/searcher/src/searcher/glue.rs"
        );
        // Each word alone is as before; one that matches nothing empties the result.
        assert_eq!(index.find("glue", 10).len(), 3);
        assert!(index.find("searcher zzz", 10).is_empty());
        // Smart case is per word.
        assert!(index.find("Searcher glue", 10).is_empty());
        // The ranges of both words are marked, in order, without overlap (the word `searcher`
        // may match either of the two segments of that name).
        let path = first(&found);
        let shown: Vec<&str> = found[0]
            .matched
            .iter()
            .map(|[start, end]| &path[*start as usize..*end as usize])
            .collect();
        assert_eq!(shown.first(), Some(&"searcher"), "{shown:?}");
        assert_eq!(shown.last(), Some(&"glue"), "{shown:?}");
        assert!(
            found[0]
                .matched
                .windows(2)
                .all(|pair| pair[0][1] <= pair[1][0]),
            "sorted and apart: {:?}",
            found[0].matched
        );
        // Only spaces: the empty query.
        assert_eq!(index.find("   ", 2).len(), 2);
    }

    #[test]
    fn matched_ranges_point_at_the_characters_of_the_query() {
        let index = index(&["src/main.rs", "src/other/cargo.toml"]);
        let found = index.find("main", 5);
        assert_eq!(first(&found), "src/main.rs");
        let path = first(&found);
        let shown: String = found[0]
            .matched
            .iter()
            .map(|[start, end]| &path[*start as usize..*end as usize])
            .collect();
        assert_eq!(shown, "main");
        assert_eq!(found[0].matched, [[4, 8]]);

        let found = index.find("sct", 5);
        let path = first(&found);
        let shown: String = found[0]
            .matched
            .iter()
            .map(|[start, end]| &path[*start as usize..*end as usize])
            .collect();
        assert_eq!(shown, "sct");
    }

    #[test]
    fn multibyte_paths_give_byte_ranges_on_character_boundaries() {
        let index = index(&["docs/übersicht.md"]);
        let found = index.find("uber", 5);
        assert!(found.is_empty(), "ü is not u");
        let found = index.find("ü", 5);
        assert_eq!(found[0].matched, [[5, 7]]);
    }

    #[test]
    fn limits_and_the_empty_query() {
        let paths: Vec<String> = (0..500).map(|i| format!("dir/file{i:03}.rs")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let index = index(&refs);
        assert_eq!(index.find("file", 7).len(), 7);
        assert_eq!(index.find("file", 10_000).len(), FIND_MAX as usize);
        assert_eq!(index.find("", 3).len(), 3);
    }

    #[test]
    fn the_walk_skips_noise_directories_honours_gitignore_and_is_kept_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        for sub in [
            ".git",
            "target/debug",
            "node_modules/x",
            "src",
            "build",
            ".github",
        ] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        for file in [
            ".git/config",
            "target/debug/out",
            "node_modules/x/i.js",
            "src/lib.rs",
            "build/gen.rs",
            ".github/ci.yml",
            "Cargo.toml",
        ] {
            std::fs::write(dir.path().join(file), "").unwrap();
        }
        std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        let root = dir.path().canonicalize().unwrap();
        let finder = Finder::default();
        let built = finder.index(&root, None);
        assert_eq!(
            built.paths,
            [".github/ci.yml", ".gitignore", "Cargo.toml", "src/lib.rs"]
        );
        assert!(!built.truncated());
        // Kept while fresh; rebuilt when the watch generation moved.
        std::fs::write(dir.path().join("added.rs"), "").unwrap();
        assert_eq!(finder.index(&root, None).len(), 4);
        assert_eq!(finder.index(&root, Some(1)).len(), 5);
        assert_eq!(finder.index(&root, Some(1)).len(), 5);
        std::fs::write(dir.path().join("more.rs"), "").unwrap();
        assert_eq!(finder.index(&root, Some(1)).len(), 5);
        assert_eq!(finder.index(&root, Some(2)).len(), 6);
    }
}
