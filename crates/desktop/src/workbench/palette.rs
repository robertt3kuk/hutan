//! Command-palette matching (pure) and state.

/// Fuzzy-match `query` against `text`. `None` when `query` is not a case-insensitive subsequence
/// of `text`; otherwise a score where higher is better: consecutive characters and matches at
/// word starts score more, long gaps and a late first match score less.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = text.chars().collect();
    let lower: Vec<char> = chars.iter().flat_map(|c| c.to_lowercase()).collect();
    // `to_lowercase` can change the length for exotic characters; fall back to a plain
    // positional match so indices stay aligned.
    let lower = if lower.len() == chars.len() {
        lower
    } else {
        chars
            .iter()
            .map(|c| c.to_lowercase().next().unwrap_or(*c))
            .collect()
    };

    let mut score = 0i32;
    let mut qi = 0;
    let mut prev_match: Option<usize> = None;
    for (i, c) in lower.iter().enumerate() {
        if qi == query.len() {
            break;
        }
        if *c != query[qi] {
            continue;
        }
        let word_start = i == 0
            || !chars[i - 1].is_alphanumeric()
            || (chars[i - 1].is_lowercase() && chars[i].is_uppercase());
        score += 10;
        if word_start {
            score += 8;
        }
        match prev_match {
            Some(p) if p + 1 == i => score += 12,
            Some(p) => score -= ((i - p - 1) as i32).min(6),
            None => score -= (i as i32).min(10),
        }
        prev_match = Some(i);
        qi += 1;
    }
    (qi == query.len()).then_some(score)
}

/// A command as the palette ranks it: its title (`Category: Name`) and whether a key runs it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub title: &'a str,
    pub has_key: bool,
}

/// What a command's name starts with, past its `Category: ` prefix.
fn label_of(title: &str) -> &str {
    title.split_once(": ").map_or(title, |(_, label)| label)
}

/// How much better a command ranks for `query` than its subsequence score says: its name
/// starts with the query (`new` → `Session: New Terminal`) most, a word of it starts with the
/// query next, and a command with a key (the ones people use) a little on top of either.
fn rank_bonus(query: &str, candidate: &Candidate<'_>) -> i32 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 0;
    }
    let title = candidate.title.to_lowercase();
    let label = label_of(&title);
    let mut bonus = 0;
    if label.starts_with(&query) {
        bonus += 80;
    } else if title.match_indices(&query).any(|(at, _)| {
        title[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    }) {
        bonus += 40;
    }
    if candidate.has_key {
        bonus += 30;
    }
    bonus
}

/// How the query sits in a command's title: 2 the name starts with it or a word of the title
/// does, 1 it is inside a word, 0 only its letters are found one after another.
fn match_tier(query: &str, title: &str) -> u8 {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 2;
    }
    let title = title.to_lowercase();
    if label_of(&title).starts_with(&query)
        || title.match_indices(&query).any(|(at, _)| {
            title[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric())
        })
    {
        2
    } else if title.contains(&query) {
        1
    } else {
        0
    }
}

/// Indices into `candidates` that match `query`, best first; ties keep the original order. The
/// subsequence score of [`fuzzy_score`] is lifted by [`rank_bonus`]: names that start with
/// the query and commands with a key come first.
pub fn rank_commands<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = Candidate<'a>>,
) -> Vec<usize> {
    let mut scored: Vec<(u8, i32, usize)> = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| {
            // The letters of `Category: ` must not steal the match from the name's own
            // (`Session: New Terminal` for `new` matches the `n` of Session first).
            let title = fuzzy_score(query, c.title);
            let label = fuzzy_score(query, label_of(c.title));
            title
                .max(label)
                .map(|s| (match_tier(query, c.title), s + rank_bonus(query, &c), i))
        })
        .collect();
    // The kind of match first, the score within it: letters picked out of several words
    // (`fork` in `Add Folder to Active Workspace…`) never stand above a name that has the word.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, i)| i).collect()
}

/// An open palette: the query, the filtered entries (indices into the command list) and the
/// highlighted row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteState {
    pub query: String,
    pub matches: Vec<usize>,
    pub selected: usize,
}

impl PaletteState {
    pub fn refilter<'a>(&mut self, candidates: impl IntoIterator<Item = Candidate<'a>>) {
        self.matches = rank_commands(&self.query, candidates);
        self.selected = 0;
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }

    pub fn selected_command(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_score("sr", "Split Right").is_some());
        assert!(fuzzy_score("xyz", "Split Right").is_none());
        assert!(fuzzy_score("tr", "Split Right").is_some());
        assert!(
            fuzzy_score("rs", "Split Right").is_none(),
            "order matters: no `s` after the `r`"
        );
    }

    fn plain<'a>(titles: impl IntoIterator<Item = &'a str>) -> Vec<Candidate<'a>> {
        titles
            .into_iter()
            .map(|title| Candidate {
                title,
                has_key: false,
            })
            .collect()
    }

    #[test]
    fn letters_found_in_several_words_never_outrank_a_name_with_the_word() {
        // FX-UI-B: `fork` is a subsequence of `Folder … Workspace`, and that command has a key.
        let candidates = [
            Candidate {
                title: "Workspace: Add Folder to Active Workspace…",
                has_key: true,
            },
            Candidate {
                title: "Chat: Fork from Here",
                has_key: false,
            },
            Candidate {
                title: "Session: Fork Session",
                has_key: false,
            },
        ];
        let order = rank_commands("fork", candidates);
        assert_eq!(order.len(), 3, "it still matches, further down");
        assert_eq!(order[2], 0, "{order:?}");
        // A name starting with the word is still first among the real matches.
        assert_eq!(
            rank_commands("fork s", candidates)[0],
            2,
            "the longer query narrows to the one that has it"
        );
    }

    #[test]
    fn empty_query_matches_everything_in_order() {
        assert_eq!(rank_commands("", plain(["b", "a", "c"])), vec![0, 1, 2]);
    }

    #[test]
    fn case_insensitive_and_ignores_spaces() {
        assert!(fuzzy_score("SPLIT R", "split right").is_some());
    }

    #[test]
    fn prefers_consecutive_and_word_start_matches() {
        let titles = [
            "View: Toggle Left Dock",
            "Layout: Split Right",
            "Panel: Open Sessions",
        ];
        assert_eq!(rank_commands("split", plain(titles))[0], 1);
        assert_eq!(rank_commands("tld", plain(titles))[0], 0);
        let ranked = rank_commands("sess", plain(titles));
        assert_eq!(ranked[0], 2);
    }

    /// The review's case (CL-POLISH-4): `new` listed `Chat: New Direct Chat…`, `Layout: Use New
    /// Default` and `Move Tab to New Split Right…` before `Session: New Terminal ⌘T`.
    #[test]
    fn a_name_that_starts_with_the_query_and_a_command_with_a_key_come_first() {
        let titles = [
            "Layout: Use New Default",
            "Move Tab to New Split Right…",
            "Chat: New Direct Chat…",
            "Session: New Terminal",
            "Session: Rename…",
            "Workspace: New Workspace…",
        ];
        let with_keys = [false, false, false, true, false, true];
        let candidates = || {
            titles
                .iter()
                .zip(with_keys)
                .map(|(title, has_key)| Candidate { title, has_key })
        };
        let ranked = rank_commands("new", candidates());
        // A name that starts with `new` and has a key beats one that only starts with it, and
        // both beat the ones where `new` is somewhere inside; `Rename…` has the letters but is
        // not a word start.
        assert_eq!(ranked[..2], [3, 5], "the two with a key lead: {ranked:?}");
        assert_eq!(ranked[2], 2);
        assert!(!ranked.contains(&4), "`Rename…` has no `new` in it");
        // Without the keys the names that start with the query still lead.
        let bare = rank_commands("new", plain(titles));
        assert!(bare[..3].iter().all(|i| [2, 3, 5].contains(i)), "{bare:?}");
        // A key lifts a command among equals only.
        let tie = [
            Candidate {
                title: "A: Open Logs",
                has_key: false,
            },
            Candidate {
                title: "B: Open Settings",
                has_key: true,
            },
        ];
        assert_eq!(rank_commands("open", tie), vec![1, 0]);
    }

    #[test]
    fn selection_wraps() {
        let mut p = PaletteState::default();
        p.refilter(plain(["a", "b", "c"]));
        p.select_prev();
        assert_eq!(p.selected, 2);
        p.select_next();
        assert_eq!(p.selected, 0);
        let mut empty = PaletteState {
            query: "zzz".into(),
            ..PaletteState::default()
        };
        empty.refilter(plain(["a"]));
        empty.select_next();
        assert_eq!(empty.selected_command(), None);
    }
}
