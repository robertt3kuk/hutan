//! Git blame in the editor (GIT-HISTORY): which commit last changed each line, kept in step with
//! the text.
//!
//! The owner asks the daemon for the blame of the buffer's text (`git.blame` with `contents`)
//! and hands the answer over as [`BlameData`]: the commits and, for each line of that text, the
//! commit that last changed it. The buffer keeps the answer's text and maps every line of its
//! current text to a line of it (whole-line matching, [`hutan_buffer::line_map`]) after each
//! edit, so a line that is typed or changed is *uncommitted* until the file is saved and blamed
//! again, and lines that only moved keep their commit. Undo brings a line back to its commit
//! because the match is made against the text, not accumulated from the edits.
//!
//! The view draws the result in a column of the gutter (the first line of each run of one
//! commit says *author · date · subject*, the rest are blank), and for the caret's line after
//! its end (`inline`).

use std::sync::Arc;

/// Texts larger than this are not blamed: the line match runs after every edit.
pub const MAX_BLAME_BYTES: usize = 2 * 1024 * 1024;

/// One commit a blame line points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    /// The full hash; all zeros for lines that are in no commit.
    pub id: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the Unix epoch.
    pub time: i64,
    /// The first line of the message.
    pub subject: String,
    /// The lines are in no commit yet.
    pub uncommitted: bool,
}

impl BlameCommit {
    /// The first seven characters of the hash.
    pub fn short(&self) -> &str {
        self.id.get(..7).unwrap_or(&self.id)
    }
}

/// What the owner got from `git.blame` for `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameData {
    pub commits: Vec<BlameCommit>,
    /// For each line of `text`, the index of its commit in `commits`; a text line past the end
    /// of this list is uncommitted.
    pub lines: Vec<u32>,
    /// The text that was blamed.
    pub text: Arc<str>,
}

/// What a line of the current text is blamed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlameLine<'a> {
    Commit(&'a BlameCommit),
    /// In no commit: typed since the blame, or edited on disk.
    Uncommitted,
}

impl<'a> BlameLine<'a> {
    /// Whether two lines belong to the same run: one commit, or both uncommitted.
    pub fn same_run(self, other: BlameLine<'_>) -> bool {
        match (self, other) {
            (Self::Commit(a), BlameLine::Commit(b)) => a.id == b.id,
            (Self::Uncommitted, BlameLine::Uncommitted) => true,
            _ => false,
        }
    }

    pub fn commit(self) -> Option<&'a BlameCommit> {
        match self {
            Self::Commit(commit) => Some(commit),
            Self::Uncommitted => None,
        }
    }
}

/// The blame the buffer holds and where it is in the text now.
#[derive(Debug)]
pub(crate) struct BlameState {
    data: BlameData,
    /// For each line of the current text, the index of its commit in `data.commits`.
    rows: Vec<Option<u32>>,
}

impl BlameState {
    /// The blame of `data`, matched to `current`.
    pub(crate) fn new(data: BlameData, current: &str) -> Self {
        let mut state = Self {
            data,
            rows: Vec::new(),
        };
        state.remap(current);
        state
    }

    /// Matches the lines of `current` to the lines that were blamed.
    pub(crate) fn remap(&mut self, current: &str) {
        let matched = hutan_buffer::line_map(&self.data.text, current);
        self.rows = matched
            .into_iter()
            .map(|old| {
                old.and_then(|old| self.data.lines.get(old).copied())
                    .filter(|commit| {
                        self.data
                            .commits
                            .get(*commit as usize)
                            .is_some_and(|c| !c.uncommitted)
                    })
            })
            .collect();
    }

    pub(crate) fn line(&self, row: usize) -> Option<BlameLine<'_>> {
        let slot = self.rows.get(row)?;
        Some(match slot.and_then(|c| self.data.commits.get(c as usize)) {
            Some(commit) => BlameLine::Commit(commit),
            None => BlameLine::Uncommitted,
        })
    }

    pub(crate) fn lines(&self) -> usize {
        self.rows.len()
    }
}

/// `3 days ago`, `now`, `2 years ago`: how long ago `time` is at `now` (both Unix seconds).
pub fn relative_date(time: i64, now: i64) -> String {
    let age = now.saturating_sub(time).max(0);
    let (value, unit) = match age {
        0..=59 => return "now".to_owned(),
        60..=3_599 => (age / 60, "minute"),
        3_600..=86_399 => (age / 3_600, "hour"),
        86_400..=604_799 => (age / 86_400, "day"),
        604_800..=2_629_799 => (age / 604_800, "week"),
        2_629_800..=31_557_599 => (age / 2_629_800, "month"),
        _ => (age / 31_557_600, "year"),
    };
    format!("{value} {unit}{} ago", if value == 1 { "" } else { "s" })
}

/// The one line a gutter or the end of a line shows for `line`: `author · 3 days ago · subject`,
/// or `Uncommitted`.
pub fn label(line: BlameLine<'_>, now: i64) -> String {
    match line {
        BlameLine::Uncommitted => "Uncommitted".to_owned(),
        BlameLine::Commit(commit) => format!(
            "{} · {} · {}",
            commit.author,
            relative_date(commit.time, now),
            commit.subject
        ),
    }
}

/// `label` cut to `max_chars` characters with an ellipsis.
pub fn fit(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The lines of a hover over a blame line: the commit, who, when and the subject.
pub fn details(line: BlameLine<'_>, now: i64) -> Vec<String> {
    match line {
        BlameLine::Uncommitted => vec![
            "Uncommitted".to_owned(),
            "These lines are not in a commit yet.".to_owned(),
        ],
        BlameLine::Commit(commit) => vec![
            format!("{} · {}", commit.short(), commit.subject),
            format!(
                "{} <{}> · {}",
                commit.author,
                commit.email,
                relative_date(commit.time, now)
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, author: &str, subject: &str) -> BlameCommit {
        BlameCommit {
            id: id.to_owned(),
            author: author.to_owned(),
            email: format!("{author}@example.com"),
            time: 1_000_000,
            subject: subject.to_owned(),
            uncommitted: false,
        }
    }

    fn data() -> BlameData {
        BlameData {
            commits: vec![
                commit("aaaaaaaaaa", "Ada", "first"),
                commit("bbbbbbbbbb", "Grace", "second"),
            ],
            lines: vec![0, 1, 0],
            text: Arc::from("one\ntwo\nthree\n"),
        }
    }

    fn who(state: &BlameState, row: usize) -> String {
        match state.line(row) {
            Some(BlameLine::Commit(c)) => c.author.clone(),
            Some(BlameLine::Uncommitted) => "-".to_owned(),
            None => "?".to_owned(),
        }
    }

    #[test]
    fn lines_keep_their_commit_across_inserts_and_changes() {
        let mut state = BlameState::new(data(), "one\ntwo\nthree\n");
        assert_eq!(
            (0..3).map(|r| who(&state, r)).collect::<Vec<_>>(),
            ["Ada", "Grace", "Ada"]
        );
        // A new line above, one changed, one unchanged.
        state.remap("new\none\nTWO\nthree\n");
        assert_eq!(
            (0..5).map(|r| who(&state, r)).collect::<Vec<_>>(),
            ["-", "Ada", "-", "Ada", "?"]
        );
        // Back to the blamed text: the commits are back.
        state.remap("one\ntwo\nthree\n");
        assert_eq!(who(&state, 1), "Grace");
    }

    #[test]
    fn dates_read_like_the_status_bar() {
        assert_eq!(relative_date(100, 130), "now");
        assert_eq!(relative_date(0, 61), "1 minute ago");
        assert_eq!(relative_date(0, 3 * 86_400 + 5), "3 days ago");
        assert_eq!(relative_date(0, 2 * 31_557_600), "2 years ago");
        assert_eq!(relative_date(500, 100), "now");
    }

    #[test]
    fn labels_fit_a_column() {
        let line = BlameLine::Commit(&commit("c0ffee1234", "Ada", "fix the thing"));
        let text = label(line, 1_000_000 + 7_200);
        assert_eq!(text, "Ada · 2 hours ago · fix the thing");
        assert_eq!(fit(&text, 12), "Ada · 2 hou…");
        assert_eq!(fit(&text, 100), text);
        assert_eq!(label(BlameLine::Uncommitted, 0), "Uncommitted");
    }
}
