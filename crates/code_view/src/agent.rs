//! What an agent changed in a file (slice E-10), as hunks of the text a buffer holds now.
//!
//! An agent's turn starts from a **base** (the file before the turn's first write) and leaves
//! the **agent text** (the file after its last write). The buffer may differ from that agent
//! text afterwards: the user typed, rejected a hunk, saved, or something else wrote the file.
//! The hunks shown are therefore never "base against buffer" (that would call the user's own
//! typing the agent's): the user's changes that lie outside the agent's hunks are first carried
//! into the base ([`rebase`]), and only then is the base diffed against the buffer
//! ([`agent_hunks`]). A rejected hunk is text equal to the base again, so it disappears; undoing
//! the Reject brings the agent's text and so the hunk back.
//!
//! Everything works on whole lines (`\n`-terminated, as `str::split_inclusive` cuts them) of
//! normalized text (`\n` line breaks, no byte-order mark: what the buffer draws). The diffs are
//! Myers' (`similar`), with the common first and last lines cut away first and a deadline after
//! which `similar` answers with a correct but less minimal script. GPUI-free.

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

/// How long one diff of [`agent_hunks`] may take before it settles for a less minimal answer.
pub const DIFF_DEADLINE: Duration = Duration::from_millis(250);

/// A file larger than this, or with a line longer than [`MINIFIED_LINE_BYTES`], is diffed again
/// at most every [`SLOW_DIFF_DELAY`] while it is edited (a large or minified file: Zed #65894).
pub const LARGE_DIFF_BYTES: usize = 1024 * 1024;
/// A line this long marks a minified file.
pub const MINIFIED_LINE_BYTES: usize = 4 * 1024;
/// The pause after an edit before the hunks are computed again.
pub const DIFF_DELAY: Duration = Duration::from_millis(150);
/// [`DIFF_DELAY`] for a large or minified file.
pub const SLOW_DIFF_DELAY: Duration = Duration::from_millis(1500);

/// Who made the changes, for the bar over the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLabel {
    /// The session's id (`hutan_core::SessionId` as text).
    pub session: String,
    /// What the session is called (its title), when it is known.
    pub title: Option<String>,
    /// The agent's own name (`OMP`, `Claude Code`), when it is one Hutan knows.
    pub agent: Option<String>,
}

/// The two texts that define an agent's changes to one file. Both are normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChanges {
    /// The file before the agent's turn changed it.
    pub base: Arc<str>,
    /// The file as the agent left it.
    pub agent: Arc<str>,
    pub label: AgentLabel,
}

/// What a hunk did to the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    /// Lines the base did not have.
    Added,
    /// Lines that replaced lines of the base.
    Modified,
    /// Lines of the base that are gone (drawn as a mark above `lines.start`).
    Removed,
}

/// One change of the agent, in the current text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHunk {
    /// The bytes of the current text the hunk covers: whole lines, their line breaks included.
    /// Empty for removed lines: the position they were at.
    pub bytes: Range<usize>,
    /// The base's text in their place (whole lines): what Reject puts back.
    pub base_text: Arc<str>,
}

impl AgentHunk {
    pub fn kind(&self) -> HunkKind {
        match (self.bytes.is_empty(), self.base_text.is_empty()) {
            (false, true) => HunkKind::Added,
            (true, _) => HunkKind::Removed,
            (false, false) => HunkKind::Modified,
        }
    }
}

/// The text as a buffer holds it: `\r\n` becomes `\n`, a leading byte-order mark goes.
pub fn normalize(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    hutan_buffer::normalize_newlines(text).into_owned()
}

/// Whether `text` is large or minified enough to be diffed less often.
pub fn is_heavy(text: &str) -> bool {
    text.len() > LARGE_DIFF_BYTES
        || text
            .split('\n')
            .any(|line| line.len() > MINIFIED_LINE_BYTES)
}

fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The changed line ranges between `a` and `b` (`(range in a, range in b)`, in order, adjacent
/// changes merged into one).
fn line_hunks(
    a: &[&str],
    b: &[&str],
    deadline: Option<Instant>,
) -> Vec<(Range<usize>, Range<usize>)> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let max_suffix = a.len().min(b.len()) - prefix;
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    let a_mid = &a[prefix..a.len() - suffix];
    let b_mid = &b[prefix..b.len() - suffix];
    let mut out: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    if a_mid.is_empty() && b_mid.is_empty() {
        return out;
    }
    if a_mid.is_empty() || b_mid.is_empty() {
        out.push((prefix..prefix + a_mid.len(), prefix..prefix + b_mid.len()));
        return out;
    }
    for op in capture_diff_slices_deadline(Algorithm::Myers, a_mid, b_mid, deadline) {
        let (old, new) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index..old_index + old_len, new_index..new_index),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index..old_index, new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (
                old_index..old_index + old_len,
                new_index..new_index + new_len,
            ),
        };
        let (old, new) = (
            prefix + old.start..prefix + old.end,
            prefix + new.start..prefix + new.end,
        );
        match out.last_mut() {
            Some((o, n)) if o.end == old.start && n.end == new.start => {
                o.end = old.end;
                n.end = new.end;
            }
            _ => out.push((old, new)),
        }
    }
    out
}

/// Whether two line ranges share a line, or touch where one of them is empty (an insertion at
/// the edge of a change belongs to it).
fn touches(a: &Range<usize>, b: &Range<usize>) -> bool {
    if a.is_empty() || b.is_empty() {
        a.start <= b.end && b.start <= a.end
    } else {
        a.start < b.end && b.start < a.end
    }
}

/// `base` with the changes that turned `from` into `to` carried over, except those that touch
/// a change between `base` and `from` (those stay the agent's). `from` is the text the agent
/// left; `to` is the text now.
pub fn rebase(base: &str, from: &str, to: &str) -> String {
    if from == to {
        return base.to_owned();
    }
    let deadline = Instant::now().checked_add(DIFF_DEADLINE);
    let base_lines = lines(base);
    let from_lines = lines(from);
    let to_lines = lines(to);
    // (range in base, range in from)
    let agent = line_hunks(&base_lines, &from_lines, deadline);
    // (range in from, range in to)
    let since = line_hunks(&from_lines, &to_lines, deadline);
    let mut out = String::with_capacity(base.len());
    let mut copied = 0usize; // lines of `base` already in `out`
    let mut passed = 0usize; // agent hunks entirely before the current edit
    let mut shift: isize = 0; // base line = from line + shift, outside the agent's hunks
    for (from_range, to_range) in since {
        if agent.iter().any(|(_, f)| touches(f, &from_range)) {
            continue;
        }
        while passed < agent.len() && agent[passed].1.end <= from_range.start {
            let (b, f) = &agent[passed];
            shift += b.len() as isize - f.len() as isize;
            passed += 1;
        }
        let start = from_range
            .start
            .saturating_add_signed(shift)
            .clamp(copied, base_lines.len());
        let end = (start + from_range.len()).min(base_lines.len());
        for line in &base_lines[copied..start] {
            out.push_str(line);
        }
        for line in &to_lines[to_range] {
            out.push_str(line);
        }
        copied = end;
    }
    for line in &base_lines[copied..] {
        out.push_str(line);
    }
    out
}

/// The agent's hunks in `current`: `changes.base` rebased over what changed since the agent
/// wrote (see [`rebase`]), diffed against `current`.
pub fn agent_hunks(base: &str, agent: &str, current: &str) -> Vec<AgentHunk> {
    let rebased = rebase(base, agent, current);
    hunks_between(&rebased, current)
}

/// The changes that turn `base` into `current`, as hunks of `current` (whole lines): what the
/// git gutter draws (`base` is the file in the index or at `HEAD`) and what an agent's hunks
/// are once rebased. Both texts are normalized.
pub fn hunks_between(base: &str, current: &str) -> Vec<AgentHunk> {
    if base == current {
        return Vec::new();
    }
    let deadline = Instant::now().checked_add(DIFF_DEADLINE);
    let old = lines(base);
    let new = lines(current);
    let mut starts = Vec::with_capacity(new.len() + 1);
    let mut at = 0usize;
    starts.push(0);
    for line in &new {
        at += line.len();
        starts.push(at);
    }
    line_hunks(&old, &new, deadline)
        .into_iter()
        .map(|(old_range, new_range)| AgentHunk {
            bytes: starts[new_range.start]..starts[new_range.end],
            base_text: Arc::from(old[old_range].concat()),
        })
        .collect()
}

/// The text a unified diff (`patch`) was made from, given the text it produced (`after`): each
/// hunk's new lines are found at the place its header names and swapped for its old lines.
/// `None` when the patch does not match `after` (it is cut short, or the file changed since).
pub fn reverse_patch(after: &str, patch: &str) -> Option<String> {
    struct Hunk {
        new_start: usize,
        old: Vec<String>,
        new: Vec<String>,
        old_no_eol: bool,
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut last_side = ' ';
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let new_part = rest.split_whitespace().find(|p| p.starts_with('+'))?;
            let start: usize = new_part
                .trim_start_matches('+')
                .split(',')
                .next()?
                .parse()
                .ok()?;
            let count_zero = new_part.ends_with(",0");
            // A hunk that adds to an empty region names the line before it.
            let new_start = if count_zero {
                start
            } else {
                start.saturating_sub(1)
            };
            hunks.push(Hunk {
                new_start,
                old: Vec::new(),
                new: Vec::new(),
                old_no_eol: false,
            });
            continue;
        }
        let Some(hunk) = hunks.last_mut() else {
            continue; // headers before the first hunk
        };
        if line.starts_with('\\') {
            if last_side != '+' {
                hunk.old_no_eol = true;
            }
            continue;
        }
        let (side, text) = line.split_at(line.chars().next().map_or(0, char::len_utf8));
        match side {
            " " => {
                hunk.old.push(text.to_owned());
                hunk.new.push(text.to_owned());
            }
            "-" => hunk.old.push(text.to_owned()),
            "+" => hunk.new.push(text.to_owned()),
            "" => {
                // An empty context line whose leading space was trimmed.
                hunk.old.push(String::new());
                hunk.new.push(String::new());
            }
            _ => return None,
        }
        last_side = side.chars().next().unwrap_or(' ');
    }
    if hunks.is_empty() {
        return None;
    }
    let after_lines: Vec<&str> = after.split_inclusive('\n').collect();
    let ends_with_newline = after.is_empty() || after.ends_with('\n');
    let mut out: Vec<String> = Vec::with_capacity(after_lines.len());
    let mut at = 0usize;
    let mut old_no_eol = None;
    for hunk in &hunks {
        if hunk.new_start < at || hunk.new_start + hunk.new.len() > after_lines.len() {
            return None;
        }
        for line in &after_lines[at..hunk.new_start] {
            out.push((*line).to_owned());
        }
        for (expected, actual) in hunk.new.iter().zip(&after_lines[hunk.new_start..]) {
            if expected != actual.strip_suffix('\n').unwrap_or(actual) {
                return None;
            }
        }
        for line in &hunk.old {
            out.push(format!("{line}\n"));
        }
        at = hunk.new_start + hunk.new.len();
        if at == after_lines.len() {
            old_no_eol = Some(hunk.old_no_eol);
        }
    }
    for line in &after_lines[at..] {
        out.push((*line).to_owned());
    }
    let mut text = out.concat();
    let no_eol = old_no_eol.unwrap_or(!ends_with_newline);
    if no_eol && text.ends_with('\n') {
        text.pop();
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply_hunks(current: &str, hunks: &[AgentHunk], reject: &[usize]) -> String {
        let mut out = current.to_owned();
        for &i in reject.iter().rev() {
            let hunk = &hunks[i];
            out.replace_range(hunk.bytes.clone(), &hunk.base_text);
        }
        out
    }

    #[test]
    fn hunks_of_an_agent_edit_are_its_lines() {
        let base = "a\nb\nc\nd\ne\n";
        let agent = "a\nB\nc\nd\ne\nf\n";
        let hunks = agent_hunks(base, agent, agent);
        assert_eq!(hunks.len(), 2);
        assert_eq!(&agent[hunks[0].bytes.clone()], "B\n");
        assert_eq!(&*hunks[0].base_text, "b\n");
        assert_eq!(hunks[0].kind(), HunkKind::Modified);
        assert_eq!(&agent[hunks[1].bytes.clone()], "f\n");
        assert_eq!(hunks[1].kind(), HunkKind::Added);
    }

    #[test]
    fn removed_lines_are_an_empty_hunk_with_the_base_text() {
        let base = "a\nb\nc\n";
        let agent = "a\nc\n";
        let hunks = agent_hunks(base, agent, agent);
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].bytes, 2..2);
        assert_eq!(&*hunks[0].base_text, "b\n");
        assert_eq!(hunks[0].kind(), HunkKind::Removed);
    }

    #[test]
    fn rejecting_one_hunk_restores_only_that_hunk() {
        let base = "fn a() {}\n\nfn b() {}\n\nfn c() {}\n";
        let agent = "fn a() { 1 }\n\nfn b() {}\n\nfn c() { 3 }\n";
        let hunks = agent_hunks(base, agent, agent);
        assert_eq!(hunks.len(), 2, "two functions, two hunks");
        let rejected = apply_hunks(agent, &hunks, &[1]);
        assert_eq!(rejected, "fn a() { 1 }\n\nfn b() {}\n\nfn c() {}\n");
        let left = agent_hunks(base, agent, &rejected);
        assert_eq!(left.len(), 1, "the other hunk stays");
        assert_eq!(&rejected[left[0].bytes.clone()], "fn a() { 1 }\n");
    }

    #[test]
    fn the_users_own_edits_outside_hunks_are_not_the_agents() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let agent = "one\nTWO\nthree\nfour\nfive\n";
        // The user adds a line far from the agent's change and edits another.
        let current = "zero\none\nTWO\nthree\nfour\nFIVE!\n";
        let hunks = agent_hunks(base, agent, current);
        assert_eq!(hunks.len(), 1);
        assert_eq!(&current[hunks[0].bytes.clone()], "TWO\n");
        assert_eq!(&*hunks[0].base_text, "two\n");
    }

    #[test]
    fn an_edit_inside_a_hunk_stays_part_of_it() {
        let base = "a\nb\nc\n";
        let agent = "a\nB\nc\n";
        let current = "a\nB2\nc\n";
        let hunks = agent_hunks(base, agent, current);
        assert_eq!(hunks.len(), 1);
        assert_eq!(&current[hunks[0].bytes.clone()], "B2\n");
        assert_eq!(&*hunks[0].base_text, "b\n");
    }

    #[test]
    fn a_text_back_at_the_base_has_no_hunks_and_an_undo_brings_them_back() {
        let base = "x\ny\n";
        let agent = "x\nY\n";
        assert!(agent_hunks(base, agent, base).is_empty());
        assert_eq!(agent_hunks(base, agent, agent).len(), 1);
    }

    #[test]
    fn rebase_carries_edits_outside_the_hunks_only() {
        let base = "a\nb\nc\nd\n";
        let agent = "a\nB\nc\nd\n";
        let now = "a\nB!\nc\nd\ne\n";
        assert_eq!(rebase(base, agent, now), "a\nb\nc\nd\ne\n");
        assert_eq!(rebase(base, agent, agent), base);
    }

    #[test]
    fn a_file_the_agent_created_rejects_to_empty_never_to_nothing() {
        let hunks = agent_hunks("", "hi\n", "hi\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), HunkKind::Added);
        assert_eq!(apply_hunks("hi\n", &hunks, &[0]), "");
    }

    #[test]
    fn lines_without_a_final_line_break() {
        let base = "a\nb";
        let agent = "a\nc";
        let hunks = agent_hunks(base, agent, agent);
        assert_eq!(hunks.len(), 1);
        assert_eq!(apply_hunks(agent, &hunks, &[0]), base);
    }

    #[test]
    fn normalize_drops_crlf_and_the_bom() {
        assert_eq!(normalize("\u{feff}a\r\nb\r\n"), "a\nb\n");
    }

    #[test]
    fn heavy_files_are_large_or_minified() {
        assert!(!is_heavy("short\nlines\n"));
        assert!(is_heavy(&"x".repeat(MINIFIED_LINE_BYTES + 1)));
    }

    #[test]
    fn reverse_patch_recovers_the_text_before() {
        let before = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
        let after = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\n";
        let patch = "--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -6,2 +6,3 @@\n six\n seven\n+eight\n";
        assert_eq!(reverse_patch(after, patch).as_deref(), Some(before));
    }

    #[test]
    fn reverse_patch_of_a_creation_is_empty() {
        let patch = "@@ -0,0 +1,2 @@\n+a\n+b\n";
        assert_eq!(reverse_patch("a\nb\n", patch).as_deref(), Some(""));
    }

    #[test]
    fn reverse_patch_refuses_what_does_not_match() {
        let patch = "@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(reverse_patch("c\n", patch), None);
        assert_eq!(reverse_patch("b\n", "no hunks here"), None);
    }

    #[test]
    fn reverse_patch_keeps_a_missing_final_line_break() {
        let patch =
            "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n";
        assert_eq!(reverse_patch("b", patch).as_deref(), Some("a"));
    }
}
