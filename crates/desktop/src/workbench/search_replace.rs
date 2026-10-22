//! The Search panel's query as `fs.search` takes it, and its *Replace All* (E-13b).
//!
//! - **One transaction per file.** Every file gets its replacements as one edit of its buffer
//!   (`Coalesce::Step`): ⌘Z in that file's tab takes them back, and only them. A file without a
//!   tab gets one in the background (the focused tab and the keyboard stay where they are).
//! - **Unsaved buffers are edited, not the disk.** A dirty buffer takes the replacements and
//!   stays dirty; nothing is written for it.
//! - **Clean files are written expecting the version the search saw.** The panel records each
//!   file's version (`fs.stat`) as the search answers; a clean buffer whose file is another
//!   version now (read again for the replace, or reloaded since) is skipped and listed, and the
//!   save itself expects the version the buffer was read as, so a change in between is refused
//!   by the daemon and shows the tab's conflict bar instead of being overwritten.
//! - **Every matched line is checked again.** A line that is not what the search showed any
//!   more (in the buffer: the disk's text or the unsaved one) skips the whole file.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui::Context;
use hutan_buffer::{Edit, Text, Transaction};
use hutan_code_view::{Coalesce, LoadState};
use hutan_protocol::fs::{FileVersion, SEARCH_TEXT_MAX_CHARS};
use regex::{Regex, RegexBuilder};

use super::model::{ItemRef, ModelError};
use super::view::Workbench;

/// Regular expressions compile to at most this much (the daemon's bound).
const REGEX_SIZE_LIMIT: usize = 4 << 20;
/// Skipped files the notice names before it says "and N more".
const NOTICE_NAMES: usize = 3;

/// What the Search panel looks for, as the user set it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchSpec {
    pub query: String,
    /// Match case exactly; off is smart case (exact once the query has an upper-case letter).
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// The query is a regular expression; `$1` in the replacement names its first group.
    pub regex: bool,
}

/// `pattern`, `regex` and `case_sensitive` of an `fs.search` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPattern {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: Option<bool>,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether a pattern asks for case: an upper-case letter that is not an escape such as `\W`
/// (the daemon's smart case, `files/search.rs`).
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

impl SearchSpec {
    /// The request: whole words are a regular expression with `\b` where the query starts or
    /// ends with a word character (`\b` next to `-` or `.` would ask for a word there).
    pub fn pattern(&self) -> SearchPattern {
        let case_sensitive = self.case_sensitive.then_some(true);
        if !self.whole_word {
            return SearchPattern {
                pattern: self.query.clone(),
                regex: self.regex,
                case_sensitive,
            };
        }
        let pattern = if self.regex {
            format!(r"\b(?:{})\b", self.query)
        } else {
            let first = self.query.chars().next().is_some_and(is_word_char);
            let last = self.query.chars().last().is_some_and(is_word_char);
            format!(
                "{}{}{}",
                if first { r"\b" } else { "" },
                regex::escape(&self.query),
                if last { r"\b" } else { "" }
            )
        };
        SearchPattern {
            pattern,
            regex: true,
            case_sensitive,
        }
    }

    /// The expression the daemon matches with: the same source, case rule and bound, so a
    /// match it reported is found again at the same place.
    pub fn compile(&self) -> Result<Regex, String> {
        let request = self.pattern();
        if request.pattern.is_empty() {
            return Err("the query is empty".to_owned());
        }
        let source = if request.regex {
            request.pattern.clone()
        } else {
            regex::escape(&request.pattern)
        };
        let sensitive = request
            .case_sensitive
            .unwrap_or_else(|| has_upper_case(&request.pattern));
        RegexBuilder::new(&source)
            .case_insensitive(!sensitive)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|e| format!("not a valid regular expression: {e}"))
    }
}

/// What the match of `regex` that starts at byte `start` of `line` becomes, and where it ends;
/// `None` when no match starts there (the line is not the one searched). `expand`: `$1`,
/// `${name}` and `$$` in `replacement` are the match's groups (a regular-expression query);
/// otherwise the replacement is literal.
pub fn replacement_at(
    regex: &Regex,
    line: &str,
    start: usize,
    replacement: &str,
    expand: bool,
) -> Option<(usize, String)> {
    if start > line.len() || !line.is_char_boundary(start) {
        return None;
    }
    let captures = regex.captures_at(line, start)?;
    let whole = captures.get(0)?;
    if whole.start() != start || whole.end() == start {
        return None;
    }
    let text = if expand {
        let mut text = String::new();
        captures.expand(replacement, &mut text);
        text
    } else {
        replacement.to_owned()
    };
    Some((whole.end(), text))
}

/// Whether `line` (the whole line now) is the line a hit showed as `shown` (cut at
/// [`SEARCH_TEXT_MAX_CHARS`] characters).
pub fn same_line(line: &str, shown: &str) -> bool {
    if shown.chars().count() >= SEARCH_TEXT_MAX_CHARS {
        line.starts_with(shown)
    } else {
        line == shown
    }
}

/// One matched line to change: the matches chosen in it, by their byte offset in the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceLine {
    /// One-based, as `fs.search` counts.
    pub line: u32,
    /// The line as the search showed it.
    pub text: String,
    /// Byte offsets where the chosen matches start, ascending.
    pub starts: Vec<u32>,
}

/// One file of a *Replace All*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceFile {
    /// Absolute and canonical (`fs.stat`'s): the identity of its tab.
    pub path: String,
    /// Relative to the searched folder, as listed.
    pub relative: String,
    /// The file's version when the search answered.
    pub seen: FileVersion,
    pub lines: Vec<ReplaceLine>,
}

/// The Search panel's *Replace All*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceRequest {
    /// The panel's number for it (the outcome names it).
    pub id: u64,
    pub spec: SearchSpec,
    pub replacement: String,
    pub files: Vec<ReplaceFile>,
}

/// Why a file was left as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The file (or its unsaved text) is not what the search saw.
    Changed,
    /// It could not be read (the message says why).
    Unreadable(String),
    /// It is open read-only (large-file mode), or is being saved.
    NotEditable,
    /// It changed on disk while its replacements were written: its tab shows the conflict, the
    /// replacements stay in the buffer.
    SaveConflict,
    /// The write failed (the message says why); the replacements stay in the buffer.
    SaveFailed(String),
}

impl SkipReason {
    pub fn text(&self) -> String {
        match self {
            Self::Changed => "changed since the search".to_owned(),
            Self::Unreadable(message) => format!("cannot be read: {message}"),
            Self::NotEditable => "cannot be edited now".to_owned(),
            Self::SaveConflict => "changed on disk while saving: see its tab".to_owned(),
            Self::SaveFailed(message) => format!("not saved: {message}"),
        }
    }
}

/// A file a *Replace All* did not change on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub relative: String,
    pub reason: SkipReason,
}

/// What a *Replace All* did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplaceOutcome {
    pub id: u64,
    /// Matches replaced (in buffers that were saved and in unsaved ones).
    pub matches: usize,
    /// Files changed.
    pub files: usize,
    /// Of those, the files whose unsaved buffer took the replacements (not written).
    pub in_buffer: usize,
    pub skipped: Vec<Skipped>,
}

impl ReplaceOutcome {
    /// One line for the notice area.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "Replaced {} in {}",
            plural(self.matches, "match", "matches"),
            plural(self.files, "file", "files")
        );
        if self.in_buffer > 0 {
            text.push_str(&format!(" ({} unsaved)", self.in_buffer));
        }
        if !self.skipped.is_empty() {
            let names: Vec<&str> = self
                .skipped
                .iter()
                .take(NOTICE_NAMES)
                .map(|s| s.relative.as_str())
                .collect();
            text.push_str(&format!(
                " · skipped {}: {}",
                plural(self.skipped.len(), "file", "files"),
                names.join(", ")
            ));
            if self.skipped.len() > NOTICE_NAMES {
                text.push_str(&format!(" and {} more", self.skipped.len() - NOTICE_NAMES));
            }
        }
        text
    }
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The edits of one file: `None` when a line is not the one searched any more.
pub fn plan_edits(
    text: &Text,
    lines: &[ReplaceLine],
    regex: &Regex,
    replacement: &str,
    expand: bool,
) -> Option<(Vec<Edit>, usize)> {
    let mut edits = Vec::new();
    for line in lines {
        let index = (line.line as usize).checked_sub(1)?;
        let range: Range<usize> = text.line_content_range(index).ok()?;
        let content = text.slice(range.clone()).ok()?;
        if !same_line(&content, &line.text) {
            return None;
        }
        for &start in &line.starts {
            let start = start as usize;
            let (end, new) = replacement_at(regex, &content, start, replacement, expand)?;
            edits.push(Edit::new(range.start + start..range.start + end, new));
        }
    }
    let count = edits.len();
    Some((edits, count))
}

/// A *Replace All* under way.
pub(crate) struct ReplaceJob {
    regex: Regex,
    replacement: String,
    expand: bool,
    /// Files whose buffer is still being read.
    waiting: HashMap<String, ReplaceFile>,
    /// Files whose replacements are being written.
    saving: HashMap<String, String>,
    /// Tabs this job opened (closed again for a file it skipped before changing it).
    opened: HashSet<String>,
    outcome: ReplaceOutcome,
}

impl ReplaceJob {
    fn skip(&mut self, path: &str, relative: &str, reason: SkipReason) {
        self.outcome.skipped.push(Skipped {
            path: path.to_owned(),
            relative: relative.to_owned(),
            reason,
        });
    }
}

impl Workbench {
    /// ⌘⇧F / ⌘⇧H: the Search panel with the keyboard in its query (`replace`: the replace
    /// field shown); a one-line selection in the focused file tab becomes the query.
    pub(crate) fn show_search(
        &mut self,
        replace: bool,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let seed = matches!(self.state.focus, super::model::FocusTarget::Pane(_))
            .then(|| self.focused_file_view())
            .flatten()
            .and_then(|view| view.read(cx).code().read(cx).selected_text(cx))
            .filter(|text| !text.is_empty() && !text.contains('\n'));
        self.open_panel("search", cx);
        let panel = self
            .panel_view(&super::model::PanelId::new("search"), window, cx)
            .and_then(|view| view.downcast::<super::panels::SearchPanel>().ok());
        let Some(panel) = panel else {
            self.set_notice("The Search panel is not available", cx);
            return;
        };
        // The panel is drawn (and its query can take the keyboard) after this frame.
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| panel.reveal(seed, replace, window, cx));
        });
    }

    /// *Replace All* of the Search panel.
    pub(crate) fn replace_in_files(&mut self, request: ReplaceRequest, cx: &mut Context<Self>) {
        if self.replace_job.is_some() {
            self.set_notice("A replace is still running", cx);
            return;
        }
        let regex = match request.spec.compile() {
            Ok(regex) => regex,
            Err(error) => {
                self.set_notice(format!("Cannot replace: {error}"), cx);
                self.replace_finished(
                    ReplaceOutcome {
                        id: request.id,
                        ..ReplaceOutcome::default()
                    },
                    cx,
                );
                return;
            }
        };
        let mut job = ReplaceJob {
            regex,
            replacement: request.replacement,
            expand: request.spec.regex,
            waiting: HashMap::new(),
            saving: HashMap::new(),
            opened: HashSet::new(),
            outcome: ReplaceOutcome {
                id: request.id,
                ..ReplaceOutcome::default()
            },
        };
        let paths: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
        for file in request.files {
            if self.buffers.get(&file.path).is_none() {
                self.open_in_background(&file.path, cx);
                job.opened.insert(file.path.clone());
            }
            job.waiting.insert(file.path.clone(), file);
        }
        self.replace_job = Some(job);
        for path in paths {
            self.replace_buffer_ready(&path, cx);
        }
        self.replace_maybe_done(cx);
    }

    /// A tab for `path` in the focused pane, its buffer read; the active tab and the keyboard
    /// stay where they are.
    fn open_in_background(&mut self, path: &str, cx: &mut Context<Self>) {
        let item = ItemRef::file(path.to_owned());
        if self.state.find_item(&item).is_none() {
            let pane = self.state.active_pane_id();
            self.try_mutate(cx, |s| -> Result<(), ModelError> {
                let focus = s.focus;
                let active = s
                    .pane(pane)
                    .filter(|p| !p.items.is_empty())
                    .map(|p| p.active);
                s.add_item(pane, item)?;
                if let Some(active) = active {
                    s.set_active_item(pane, active)?;
                }
                s.focus = focus;
                Ok(())
            });
        }
        self.ensure_buffer(path, true, None, cx);
    }

    /// The buffer of `path` was read (or failed to be): its replacements go in when a job
    /// waits for it.
    pub(crate) fn replace_buffer_ready(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(job) = &mut self.replace_job else {
            return;
        };
        let Some(file) = job.waiting.remove(path) else {
            return;
        };
        let Some(stored) = self.buffers.buffers.get(path) else {
            job.skip(
                &file.path,
                &file.relative,
                SkipReason::Unreadable("its tab was closed".to_owned()),
            );
            return;
        };
        let buffer = stored.buffer.clone();
        let disk = stored.disk;
        let b = buffer.read(cx);
        match b.state() {
            LoadState::Loading => {
                job.waiting.insert(file.path.clone(), file);
                return;
            }
            LoadState::Failed { message, .. } => {
                let reason = SkipReason::Unreadable(message.clone());
                job.skip(&file.path, &file.relative, reason);
                return;
            }
            LoadState::NeedsConfirm { .. } => {
                job.skip(&file.path, &file.relative, SkipReason::NotEditable);
                return;
            }
            LoadState::Ready => {}
        }
        if !b.is_editable() || b.is_saving() {
            job.skip(&file.path, &file.relative, SkipReason::NotEditable);
            return;
        }
        let dirty = b.is_dirty();
        if !dirty && disk != Some(file.seen) {
            job.skip(&file.path, &file.relative, SkipReason::Changed);
            return;
        }
        let planned = plan_edits(
            b.text(),
            &file.lines,
            &job.regex,
            &job.replacement,
            job.expand,
        );
        let Some((edits, count)) = planned else {
            job.skip(&file.path, &file.relative, SkipReason::Changed);
            return;
        };
        let transaction = match Transaction::new(edits) {
            Ok(transaction) => transaction,
            Err(error) => {
                tracing::warn!(path, %error, "replace: the edits do not fit");
                job.skip(&file.path, &file.relative, SkipReason::Changed);
                return;
            }
        };
        let origin = cx.entity_id();
        let applied = buffer.update(cx, |b, cx| {
            b.edit(transaction, Coalesce::Step, origin, cx).is_some()
        });
        let Some(job) = &mut self.replace_job else {
            return;
        };
        if !applied {
            job.skip(&file.path, &file.relative, SkipReason::NotEditable);
            return;
        }
        // The tab stays: ⌘Z there takes the replacements back.
        job.opened.remove(&file.path);
        job.outcome.matches += count;
        job.outcome.files += 1;
        if dirty {
            job.outcome.in_buffer += 1;
            return;
        }
        job.saving.insert(file.path.clone(), file.relative.clone());
        self.save_file(&file.path, None, cx);
        let started = self
            .buffers
            .buffers
            .get(&file.path)
            .is_some_and(|s| s.save.is_some());
        if !started {
            self.replace_saved(
                &file.path,
                Err(SkipReason::SaveFailed("the save did not start".to_owned())),
                cx,
            );
        }
    }

    /// The write of a file a job changed answered.
    pub(crate) fn replace_saved(
        &mut self,
        path: &str,
        result: Result<(), SkipReason>,
        cx: &mut Context<Self>,
    ) {
        let Some(job) = &mut self.replace_job else {
            return;
        };
        let Some(relative) = job.saving.remove(path) else {
            return;
        };
        if let Err(reason) = result {
            job.skip(path, &relative, reason);
        }
        self.replace_maybe_done(cx);
    }

    /// Buffers that were dropped (their tab closed) while a job waited for them.
    pub(crate) fn replace_buffers_dropped(&mut self, paths: &[String], cx: &mut Context<Self>) {
        if self.replace_job.is_none() {
            return;
        }
        for path in paths {
            self.replace_buffer_ready(path, cx);
        }
        self.replace_maybe_done(cx);
    }

    /// Ends the job once nothing is read or written for it any more.
    pub(crate) fn replace_maybe_done(&mut self, cx: &mut Context<Self>) {
        let done = self
            .replace_job
            .as_ref()
            .is_some_and(|job| job.waiting.is_empty() && job.saving.is_empty());
        if !done {
            return;
        }
        let Some(job) = self.replace_job.take() else {
            return;
        };
        // Tabs opened for files that were skipped before anything changed go again.
        for path in &job.opened {
            let item = ItemRef::file(path.clone());
            if let Some((pane, index)) = self.state.find_item(&item) {
                self.try_mutate(cx, |s| s.close_item(pane, index));
            }
        }
        let outcome = job.outcome;
        self.set_notice(outcome.summary(), cx);
        self.replace_finished(outcome, cx);
    }

    fn replace_finished(&mut self, outcome: ReplaceOutcome, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel::<super::panels::SearchPanel>("search") {
            panel.update(cx, |panel, cx| panel.replace_finished(outcome, cx));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use hutan_buffer::Buffer;

    fn spec(query: &str) -> SearchSpec {
        SearchSpec {
            query: query.to_owned(),
            ..SearchSpec::default()
        }
    }

    #[test]
    fn whole_words_bound_only_the_word_ends_of_the_query() {
        let words = SearchSpec {
            whole_word: true,
            ..spec("foo.bar")
        };
        assert_eq!(words.pattern().pattern, r"\bfoo\.bar\b");
        assert!(words.pattern().regex);
        let dashed = SearchSpec {
            whole_word: true,
            ..spec("-x-")
        };
        assert_eq!(dashed.pattern().pattern, r"\-x\-");
        let regex = SearchSpec {
            whole_word: true,
            regex: true,
            ..spec("a|b")
        };
        assert_eq!(regex.pattern().pattern, r"\b(?:a|b)\b");
        let re = words.compile().unwrap();
        assert!(re.is_match("x foo.bar y"));
        assert!(!re.is_match("xfoo.bar"));
    }

    #[test]
    fn smart_case_unless_match_case_is_on() {
        assert!(spec("needle").compile().unwrap().is_match("NEEDLE"));
        assert!(!spec("Needle").compile().unwrap().is_match("needle"));
        let exact = SearchSpec {
            case_sensitive: true,
            ..spec("needle")
        };
        assert_eq!(exact.pattern().case_sensitive, Some(true));
        assert!(!exact.compile().unwrap().is_match("NEEDLE"));
        assert!(spec("(").compile().is_ok(), "literal text is escaped");
        let bad = SearchSpec {
            regex: true,
            ..spec("(")
        };
        assert!(bad.compile().is_err());
    }

    #[test]
    fn a_replacement_expands_groups_only_for_regular_expressions() {
        let regex = SearchSpec {
            regex: true,
            ..spec(r"(\w+)_id")
        };
        let re = regex.compile().unwrap();
        let line = "let user_id = order_id;";
        assert_eq!(
            replacement_at(&re, line, 4, "${1}Id", true),
            Some((11, "userId".to_owned()))
        );
        assert_eq!(
            replacement_at(&re, line, 4, "$1", false),
            Some((11, "$1".to_owned()))
        );
        // No match starts at 12 (the next one is at 14): the line is not the one searched.
        assert_eq!(replacement_at(&re, line, 12, "x", true), None);
        // A word boundary looks before the start.
        let word = SearchSpec {
            whole_word: true,
            ..spec("id")
        }
        .compile()
        .unwrap();
        assert_eq!(replacement_at(&word, "user_id id", 5, "x", false), None);
        assert_eq!(
            replacement_at(&word, "user_id id", 8, "x", false),
            Some((10, "x".to_owned()))
        );
    }

    #[test]
    fn edits_land_on_the_lines_searched_and_a_changed_line_stops_the_file() {
        let buffer = Buffer::from_content("alpha needle\r\nbeta\r\nneedle needle\r\n");
        let re = spec("needle").compile().unwrap();
        let lines = vec![
            ReplaceLine {
                line: 1,
                text: "alpha needle".into(),
                starts: vec![6],
            },
            ReplaceLine {
                line: 3,
                text: "needle needle".into(),
                starts: vec![7],
            },
        ];
        let (edits, count) = plan_edits(buffer.text(), &lines, &re, "pin", false).unwrap();
        assert_eq!(count, 2);
        let after = Transaction::new(edits)
            .unwrap()
            .apply_to_string(&buffer.text().slice(0..buffer.text().len()).unwrap())
            .unwrap();
        assert_eq!(after, "alpha pin\nbeta\nneedle pin\n");
        let stale = vec![ReplaceLine {
            line: 2,
            text: "alpha needle".into(),
            starts: vec![6],
        }];
        assert!(plan_edits(buffer.text(), &stale, &re, "pin", false).is_none());
        let gone = vec![ReplaceLine {
            line: 9,
            text: "x".into(),
            starts: vec![0],
        }];
        assert!(plan_edits(buffer.text(), &gone, &re, "pin", false).is_none());
    }

    #[test]
    fn a_cut_line_is_compared_by_its_start() {
        let shown: String = "x".repeat(SEARCH_TEXT_MAX_CHARS);
        let whole = format!("{shown}tail");
        assert!(same_line(&whole, &shown));
        assert!(!same_line("xx", "x"));
        assert!(same_line("x", "x"));
    }

    #[test]
    fn the_summary_names_the_skipped_files() {
        let outcome = ReplaceOutcome {
            id: 1,
            matches: 42,
            files: 7,
            in_buffer: 1,
            skipped: (0..5)
                .map(|n| Skipped {
                    path: format!("/p/{n}.rs"),
                    relative: format!("{n}.rs"),
                    reason: SkipReason::Changed,
                })
                .collect(),
        };
        assert_eq!(
            outcome.summary(),
            "Replaced 42 matches in 7 files (1 unsaved) · skipped 5 files: 0.rs, 1.rs, 2.rs and 2 more"
        );
    }
}
