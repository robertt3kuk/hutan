//! Carets and selections: moving them by characters, words, lines, pages and the whole file
//! (extending with ⇧), and the multi-cursor commands (⌘D, select all occurrences, Escape).

use gpui::Context;
use hutan_buffer::{MAX_SELECTIONS, SearchQuery, Selection, Selections, Text};

use crate::layout;
use crate::rules::is_word;
use crate::view::{CodeView, CodeViewEvent, line_window, word_at};
use crate::wrap::WrapMap;

/// Where a motion takes each caret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Left,
    Right,
    Up(usize),
    Down(usize),
    WordLeft,
    WordRight,
    /// The first character of the line that is not a space, or the start of the line when the
    /// caret is already there.
    LineStart,
    LineEnd,
    Start,
    End,
    PageUp,
    PageDown,
}

/// A motion: where to, and whether the selections extend (⇧) or collapse to carets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Motion {
    pub target: Target,
    pub select: bool,
}

/// The kind of character a word motion stops between.
fn class(c: char) -> u8 {
    if is_word(c) {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

pub(crate) fn prev_boundary(text: &Text, at: usize) -> usize {
    text.char_before(at).map_or(at, |c| at - c.len_utf8())
}

pub(crate) fn next_boundary(text: &Text, at: usize) -> usize {
    text.char_at(at).map_or(at, |c| at + c.len_utf8())
}

/// The start of the word before `at` (spaces and line breaks before it skipped).
pub(crate) fn word_start(text: &Text, mut at: usize) -> usize {
    while let Some(c) = text.char_before(at).filter(|c| c.is_whitespace()) {
        at -= c.len_utf8();
    }
    if let Some(kind) = text.char_before(at).map(class) {
        while let Some(c) = text.char_before(at).filter(|c| class(*c) == kind) {
            at -= c.len_utf8();
        }
    }
    at
}

/// The end of the word after `at` (spaces and line breaks after it skipped).
pub(crate) fn word_end(text: &Text, mut at: usize) -> usize {
    while let Some(c) = text.char_at(at).filter(|c| c.is_whitespace()) {
        at += c.len_utf8();
    }
    if let Some(kind) = text.char_at(at).map(class) {
        while let Some(c) = text.char_at(at).filter(|c| class(*c) == kind) {
            at += c.len_utf8();
        }
    }
    at
}

/// The display column of byte `at` on its line.
pub(crate) fn column_of(text: &Text, at: usize) -> usize {
    let line = text.line_of_byte(at).unwrap_or(0);
    let start = text.line_start(line).unwrap_or(0);
    layout::line_columns(&line_window(text, start..at, usize::MAX))
}

/// The byte of `line` at display column `goal` (the end of a shorter line).
pub(crate) fn offset_at_column(text: &Text, line: usize, goal: usize) -> usize {
    let Ok(content) = text.line_content_range(line) else {
        return text.len();
    };
    let window = line_window(text, content.clone(), goal + 2);
    content.start + layout::byte_at_col(&window, goal)
}

/// The first byte of `line` that is not a space or a tab.
pub(crate) fn first_non_blank(text: &Text, line: usize) -> usize {
    let Ok(content) = text.line_content_range(line) else {
        return text.len();
    };
    let mut at = content.start;
    while at < content.end && matches!(text.char_at(at), Some(' ' | '\t')) {
        at += 1;
    }
    at
}

impl CodeView {
    /// Runs `motion` for every caret.
    pub(crate) fn motion(&mut self, motion: Motion, cx: &mut Context<Self>) {
        self.sync_wrap(cx);
        let text = self.buffer().read(cx).text().clone();
        let vertical = matches!(
            motion.target,
            Target::Up(_) | Target::Down(_) | Target::PageUp | Target::PageDown
        );
        let page = self.page();
        let (moved, goals, primary_upstream) = {
            let wrap = self.wrap.borrow();
            let map = wrap.map.as_ref();
            let goals: Vec<usize> = match &self.goal {
                Some(goals) if vertical && goals.len() == self.selections.len() => goals.clone(),
                _ => self
                    .selections
                    .iter()
                    .map(|s| match map {
                        // The cell the caret is drawn in, counted from the left of the text.
                        Some(map) => map
                            .x_of_byte(&text, s.head, wrap.upstream == Some(s.head))
                            .1
                            .round() as usize,
                        None => column_of(&text, s.head),
                    })
                    .collect(),
            };
            let primary = self.selections.primary_index();
            let mut moved = Vec::with_capacity(self.selections.len());
            let mut primary_upstream = false;
            for (index, (selection, goal)) in self.selections.iter().zip(&goals).enumerate() {
                let upstream = index == primary && wrap.upstream == Some(selection.head);
                let (to, ends_row) =
                    self.move_one(&text, map, *selection, upstream, motion, *goal, page);
                if index == primary {
                    primary_upstream = ends_row;
                }
                moved.push(to);
            }
            (moved, goals, primary_upstream)
        };
        let primary = self.selections.primary_index();
        let selections = match motion.target {
            // To the start or end of the file: one caret.
            Target::Start | Target::End if !motion.select => Selections::single(moved[primary]),
            _ => Selections::new(moved, primary).unwrap_or_else(|_| self.selections.clone()),
        };
        if matches!(motion.target, Target::PageUp | Target::PageDown)
            && let Some(layout) = self.layout
        {
            let delta = page as f32 * f32::from(layout.line_height);
            self.scroll_y += if motion.target == Target::PageUp {
                -delta
            } else {
                delta
            };
            self.clamp_scroll(cx);
        }
        let head = selections.primary().head;
        self.selections = selections;
        self.set_upstream(primary_upstream.then_some(head));
        self.goal = vertical.then_some(goals);
        self.marked.clear();
        self.buffer().update(cx, |b, _| b.break_group());
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// Where `motion` takes one caret, and whether the caret then stands at the end of a wrapped
    /// row (drawn there, not at the start of the next one). `map`: the view's rows while it
    /// wraps; `upstream`: the caret is drawn at the end of its row now.
    #[allow(clippy::too_many_arguments)]
    fn move_one(
        &self,
        text: &Text,
        map: Option<&WrapMap>,
        selection: Selection,
        upstream: bool,
        motion: Motion,
        goal: usize,
        page: usize,
    ) -> (Selection, bool) {
        let collapsing = !motion.select && !selection.is_empty();
        let head = selection.head;
        let last_line = text.line_count().saturating_sub(1);
        // Up and down: a line, or a row of a wrapped view.
        let vertical = |from: usize, up: bool, steps: usize| -> (usize, bool) {
            if let Some(map) = map {
                let row = map.row_of_byte(text, from, upstream && from == head);
                let target = if up {
                    if row == 0 {
                        return (0, false);
                    }
                    row.saturating_sub(steps)
                } else {
                    if row + 1 >= map.total_rows() {
                        return (text.len(), false);
                    }
                    (row + steps).min(map.total_rows() - 1)
                };
                return map.byte_at_goal(text, target, goal);
            }
            let line = text.line_of_byte(from).unwrap_or(0);
            let to = if up {
                if line == 0 {
                    0
                } else {
                    offset_at_column(text, line.saturating_sub(steps), goal)
                }
            } else if line >= last_line {
                text.len()
            } else {
                offset_at_column(text, (line + steps).min(last_line), goal)
            };
            (to, false)
        };
        // The row the caret is drawn in, while wrapping.
        let row = map.map(|map| {
            let (line, sub) = map.locate_byte(text, head, upstream);
            map.row(text, line, sub)
        });
        let mut ends_row = false;
        let to = match motion.target {
            Target::Left if collapsing => selection.start(),
            Target::Right if collapsing => selection.end(),
            Target::Left => prev_boundary(text, head),
            Target::Right => next_boundary(text, head),
            Target::Up(n) => {
                let (to, end) =
                    vertical(if collapsing { selection.start() } else { head }, true, n);
                ends_row = end;
                to
            }
            Target::Down(n) => {
                let (to, end) = vertical(if collapsing { selection.end() } else { head }, false, n);
                ends_row = end;
                to
            }
            Target::PageUp => {
                let (to, end) = vertical(head, true, page);
                ends_row = end;
                to
            }
            Target::PageDown => {
                let (to, end) = vertical(head, false, page);
                ends_row = end;
                to
            }
            Target::WordLeft => word_start(text, head),
            Target::WordRight => word_end(text, head),
            Target::LineStart => {
                let line = text.line_of_byte(head).unwrap_or(0);
                let first = first_non_blank(text, line);
                let start = text.line_start(line).unwrap_or(0);
                match &row {
                    // A row after the first: its own start, then the line's.
                    Some(row) if !row.first && head != row.bytes.start => row.bytes.start,
                    _ if head == first => start,
                    _ => first,
                }
            }
            Target::LineEnd => {
                let line = text.line_of_byte(head).unwrap_or(0);
                let line_end = text.line_content_range(line).map_or(head, |r| r.end);
                match &row {
                    // A row before the last: its own end (drawn there), then the line's.
                    Some(row) if !row.last && head != row.bytes.end => {
                        ends_row = true;
                        row.bytes.end
                    }
                    _ => line_end,
                }
            }
            Target::Start => 0,
            Target::End => text.len(),
        };
        let moved = if motion.select {
            Selection::new(selection.anchor, to)
        } else {
            Selection::cursor(to)
        };
        (moved, ends_row)
    }

    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        let len = self.buffer().read(cx).text().len();
        self.selections = Selections::single(Selection::new(0, len));
        self.after_selecting(cx);
    }

    /// Escape: ends a composition first, then closes the popups (completion, signature help,
    /// hover) and a snippet's tab stops; else one caret (the primary) without a selection.
    pub(crate) fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.end_composition(cx) || (self.dismiss_popups(cx) | self.dismiss_hover(cx)) {
            return;
        }
        self.snippet = None;
        let primary = self.selections.primary();
        self.selections = if self.selections.len() > 1 {
            Selections::single(primary)
        } else {
            Selections::cursor(primary.head)
        };
        self.after_selecting(cx);
    }

    fn after_selecting(&mut self, cx: &mut Context<Self>) {
        self.goal = None;
        self.marked.clear();
        self.buffer().update(cx, |b, _| b.break_group());
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// Every empty caret selects the word it is in. Returns whether there was one.
    fn select_words(&mut self, text: &Text) -> bool {
        let primary = self.selections.primary_index();
        let ranges: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                if !s.is_empty() {
                    return *s;
                }
                let word = word_at(text, s.head);
                let is_word_here = text
                    .slice(word.clone())
                    .is_ok_and(|w| w.chars().next().is_some_and(is_word));
                if is_word_here {
                    Selection::new(word.start, word.end)
                } else {
                    *s
                }
            })
            .collect();
        let changed = ranges
            .iter()
            .zip(self.selections.iter())
            .any(|(a, b)| a != b);
        if let Ok(selections) = Selections::new(ranges, primary) {
            self.selections = selections;
        }
        changed
    }

    /// ⌘D: the word under the caret, then the next occurrence of the primary selection after the
    /// last selection (wrapping around) as a new primary selection.
    pub(crate) fn select_next_occurrence(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        if self.selections.primary().is_empty() {
            self.occurrence_whole_word = self.select_words(&text);
            self.after_selecting(cx);
            return;
        }
        let primary = self.selections.primary();
        let Ok(needle) = text.slice(primary.range()) else {
            return;
        };
        let query = SearchQuery::literal(needle)
            .case_sensitive(true)
            .whole_word(self.occurrence_whole_word);
        let from = self
            .selections
            .iter()
            .map(Selection::end)
            .max()
            .unwrap_or(0);
        let taken =
            |range: &std::ops::Range<usize>| self.selections.iter().any(|s| s.range() == *range);
        let mut next = None;
        for window in [from..text.len(), 0..from] {
            let mut at = window.start;
            while at < window.end {
                let Ok(found) = text.search(&query, at..window.end, 1) else {
                    break;
                };
                let Some(range) = found.matches.into_iter().next() else {
                    break;
                };
                if !taken(&range) {
                    next = Some(range);
                    break;
                }
                at = range.end.max(at + 1);
            }
            if next.is_some() {
                break;
            }
        }
        let Some(range) = next else {
            return;
        };
        let reversed = primary.is_reversed();
        let added = if reversed {
            Selection::new(range.end, range.start)
        } else {
            Selection::new(range.start, range.end)
        };
        if self.selections.push(added, true).is_ok() {
            self.after_selecting(cx);
        }
    }

    /// ⌘⇧L: every occurrence of the primary selection (of the word under the caret when it is
    /// empty), the one that was primary staying primary.
    pub(crate) fn select_all_occurrences(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        if self.selections.primary().is_empty() {
            self.occurrence_whole_word = self.select_words(&text);
        }
        let primary = self.selections.primary();
        if primary.is_empty() {
            return;
        }
        let Ok(needle) = text.slice(primary.range()) else {
            return;
        };
        let query = SearchQuery::literal(needle)
            .case_sensitive(true)
            .whole_word(self.occurrence_whole_word);
        let Ok(found) = text.search_all(&query, MAX_SELECTIONS) else {
            return;
        };
        let index = found
            .matches
            .iter()
            .position(|m| *m == primary.range())
            .unwrap_or(0);
        let ranges = found
            .matches
            .into_iter()
            .map(|m| Selection::new(m.start, m.end));
        if let Ok(selections) = Selections::new(ranges, index) {
            self.selections = selections;
            self.after_selecting(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_stop_at_their_class() {
        let text = Text::from("let foo_bar = x.y;\n  next");
        assert_eq!(word_end(&text, 0), 3);
        assert_eq!(word_end(&text, 3), 11, "spaces first, then the word");
        assert_eq!(word_start(&text, 11), 4);
        assert_eq!(word_end(&text, 11), 13, "punctuation is a word of its own");
        assert_eq!(word_start(&text, 21), 17, "across the line break");
        assert_eq!(word_end(&text, 18), 25);
    }

    #[test]
    fn columns_survive_tabs_and_wide_characters() {
        let text = Text::from("\tab\n日本x\nshort\n");
        assert_eq!(column_of(&text, 2), 5);
        assert_eq!(offset_at_column(&text, 1, 4), 4 + 6, "after 本");
        assert_eq!(offset_at_column(&text, 1, 3), 4 + 3, "inside 本: before it");
        assert_eq!(
            offset_at_column(&text, 2, 40),
            17,
            "the end of a short line"
        );
        assert_eq!(first_non_blank(&text, 0), 1);
    }
}
