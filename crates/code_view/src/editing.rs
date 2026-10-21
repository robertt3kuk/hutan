//! Changes to the text from a view: typing (with auto-pairs and the outdent of a closing
//! bracket), line breaks with auto-indent, indentation, deletion, line commands, comments, the
//! clipboard and undo. Every command works on every caret and is **one** buffer transaction, so
//! a multi-caret edit is one undo step; typing joins the typing before it
//! ([`Coalesce::Typing`]).

use std::ops::Range;

use gpui::{ClipboardItem, Context};
use hutan_buffer::{Edit, Selection, Selections, Text, Transaction, normalize_newlines};

use crate::buffer::Coalesce;
use crate::motion::{next_boundary, prev_boundary, word_end, word_start};
use crate::rules::{
    IndentUnit, closing_pair, comment_style, is_closer, is_quote, is_word, opens_block,
};
use crate::view::{CodeView, CodeViewEvent};

/// The clipboard metadata of a copy from a code view: `hutan-code:line` for whole lines (a copy
/// without a selection), `hutan-code:sel:3,5` for the lengths of the pieces of a multi-caret
/// copy (joined with `\n`).
const CLIPBOARD_TAG: &str = "hutan-code:";

/// One caret's change: replace `range` with `text`, then select `select` (relative to where the
/// new text starts).
#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub range: Range<usize>,
    pub text: String,
    pub select: Range<usize>,
    pub reversed: bool,
    pub primary: bool,
}

impl Change {
    /// `range` becomes `text` with the caret after it.
    fn replace(range: Range<usize>, text: String, primary: bool) -> Change {
        let len = text.len();
        Change {
            range,
            text,
            select: len..len,
            reversed: false,
            primary,
        }
    }

    /// No change to the text; the caret goes to `to`.
    fn caret(to: usize, primary: bool) -> Change {
        Change {
            range: to..to,
            text: String::new(),
            select: 0..0,
            reversed: false,
            primary,
        }
    }
}

/// The leading spaces and tabs of the line that holds `at` (up to `at` at most), and where the
/// line starts.
fn leading_space(text: &Text, at: usize) -> (usize, String) {
    let line = text.line_of_byte(at).unwrap_or(0);
    let start = text.line_start(line).unwrap_or(0);
    let mut end = start;
    while end < at && matches!(text.char_at(end), Some(' ' | '\t')) {
        end += 1;
    }
    (start, text.slice(start..end).unwrap_or_default())
}

/// The display width of an indentation.
fn indent_columns(indent: &str) -> usize {
    crate::layout::line_columns(indent)
}

/// An indentation `columns` wide in the file's own unit.
fn indent_of(unit: IndentUnit, columns: usize) -> String {
    match unit {
        IndentUnit::Tab => {
            let tabs = columns / crate::layout::TAB_SIZE;
            let rest = columns % crate::layout::TAB_SIZE;
            format!("{}{}", "\t".repeat(tabs), " ".repeat(rest))
        }
        IndentUnit::Spaces(_) => " ".repeat(columns),
    }
}

/// The last character before `at` on its line that is not a space.
fn last_non_blank_before(text: &Text, at: usize) -> Option<char> {
    let (start, _) = leading_space(text, at);
    let mut i = at;
    while i > start {
        let c = text.char_before(i)?;
        if c != ' ' && c != '\t' {
            return Some(c);
        }
        i -= c.len_utf8();
    }
    None
}

impl CodeView {
    /// Applies one change per caret as one transaction (changes that overlap an earlier one are
    /// dropped). Returns whether the buffer took it.
    pub(crate) fn apply_changes(
        &mut self,
        mut changes: Vec<Change>,
        coalesce: Coalesce,
        cx: &mut Context<Self>,
    ) -> bool {
        changes.sort_by_key(|c| (c.range.start, c.range.end));
        let mut kept: Vec<Change> = Vec::with_capacity(changes.len());
        for change in changes {
            if let Some(prev) = kept.last()
                && (change.range.start < prev.range.end
                    || (change.range.start == prev.range.start && change.range.is_empty()))
            {
                continue;
            }
            kept.push(change);
        }
        let mut shift = 0isize;
        let mut primary = 0;
        let mut ranges = Vec::with_capacity(kept.len());
        for (i, change) in kept.iter().enumerate() {
            let start = change.range.start.saturating_add_signed(shift);
            let (a, b) = (start + change.select.start, start + change.select.end);
            ranges.push(if change.reversed {
                Selection::new(b, a)
            } else {
                Selection::new(a, b)
            });
            if change.primary {
                primary = i;
            }
            shift += change.text.len() as isize - change.range.len() as isize;
        }
        let Ok(after) = Selections::new(ranges, primary) else {
            return false;
        };
        let edits = kept
            .into_iter()
            .map(|c| Edit::new(c.range, c.text))
            .collect::<Vec<_>>();
        self.commit(edits, after, coalesce, cx)
    }

    /// Applies `edits` (coordinates of the current text) as one transaction and shows `after`.
    pub(crate) fn commit(
        &mut self,
        edits: Vec<Edit>,
        after: Selections,
        coalesce: Coalesce,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_editable(cx) {
            return false;
        }
        let Ok(transaction) = Transaction::new(edits) else {
            return false;
        };
        if !transaction.is_empty() {
            let transaction =
                transaction.with_selections(Some(self.selections.clone()), Some(after.clone()));
            let id = cx.entity_id();
            let applied = self
                .buffer()
                .clone()
                .update(cx, |b, cx| b.edit(transaction, coalesce, id, cx));
            if applied.is_none() {
                return false;
            }
        }
        let text = self.buffer().read(cx).text().clone();
        self.selections = after.clamp(&text);
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        true
    }

    /// Applies `edits` and carries the selections through them.
    fn commit_mapped(&mut self, edits: Vec<Edit>, coalesce: Coalesce, cx: &mut Context<Self>) {
        let Ok(transaction) = Transaction::new(edits.clone()) else {
            return;
        };
        let after = self.selections.map(&transaction);
        self.commit(edits, after, coalesce, cx);
    }

    /// One change per caret from `f(text, selection, is_primary)`.
    fn per_caret(
        &mut self,
        coalesce: Coalesce,
        cx: &mut Context<Self>,
        mut f: impl FnMut(&Text, Selection, bool) -> Change,
    ) -> bool {
        let text = self.buffer().read(cx).text().clone();
        let primary = self.selections.primary_index();
        let changes = self
            .selections
            .iter()
            .enumerate()
            .map(|(i, s)| f(&text, *s, i == primary))
            .collect();
        self.apply_changes(changes, coalesce, cx)
    }

    /// Replaces every selection with `text` (a paste, a committed composition, a typed string).
    pub(crate) fn insert(
        &mut self,
        text: &str,
        coalesce: Coalesce,
        cx: &mut Context<Self>,
    ) -> bool {
        let text = normalize_newlines(text).into_owned();
        self.per_caret(coalesce, cx, |_, s, primary| {
            Change::replace(s.range(), text.clone(), primary)
        })
    }

    /// Typed text (not from an input method's composition): one character gets the auto-pair
    /// rules, a longer string goes in as it is.
    pub(crate) fn type_text(&mut self, typed: &str, cx: &mut Context<Self>) -> bool {
        let mut chars = typed.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return self.insert(typed, Coalesce::Typing, cx);
        };
        let language = self.buffer().read(cx).language();
        let unit = self.buffer().read(cx).indent_unit();
        self.per_caret(Coalesce::Typing, cx, |text, s, primary| {
            let len = c.len_utf8();
            if !s.is_empty() {
                // A bracket or quote typed over a selection surrounds it.
                if let Some(close) = closing_pair(language, c) {
                    let inner = text.slice(s.range()).unwrap_or_default();
                    let n = inner.len();
                    return Change {
                        range: s.range(),
                        text: format!("{c}{inner}{close}"),
                        select: len..len + n,
                        reversed: s.is_reversed(),
                        primary,
                    };
                }
                return Change::replace(s.range(), c.to_string(), primary);
            }
            let head = s.head;
            let next = text.char_at(head);
            let prev = text.char_before(head);
            // Typing the closer that is already there steps over it.
            if is_closer(c) && next == Some(c) {
                return Change::caret(head + len, primary);
            }
            if let Some(close) = closing_pair(language, c) {
                let next_free = next.is_none_or(|n| {
                    n.is_whitespace() || matches!(n, ')' | ']' | '}' | ',' | ';' | ':')
                });
                let prev_free = !is_quote(c) || prev.is_none_or(|p| !is_word(p) && p != c);
                if next_free && prev_free {
                    return Change {
                        range: head..head,
                        text: format!("{c}{close}"),
                        select: len..len,
                        reversed: false,
                        primary,
                    };
                }
            }
            // A closing bracket typed on a blank line goes back one level.
            if matches!(c, '}' | ')' | ']') {
                let (start, indent) = leading_space(text, head);
                if !indent.is_empty() && start + indent.len() == head {
                    let columns = indent_columns(&indent).saturating_sub(unit.columns());
                    let text = format!("{}{c}", indent_of(unit, columns));
                    return Change::replace(start..head, text, primary);
                }
            }
            Change::replace(head..head, c.to_string(), primary)
        })
    }

    /// Return: a line break, the new line indented as the code says (the syntax tree when it
    /// is current, else the line's own indentation, one level deeper after an opening bracket),
    /// and a caret between a bracket pair gets the closer on a line of its own.
    pub(crate) fn newline(&mut self, cx: &mut Context<Self>) {
        // A Return that ends an input method's composition only commits it.
        if self.end_composition(cx) {
            return;
        }
        let (language, unit, syntax) = {
            let buffer = self.buffer().read(cx);
            (
                buffer.language(),
                buffer.indent_unit(),
                buffer.syntax_view().filter(|v| v.is_current()),
            )
        };
        self.per_caret(Coalesce::Step, cx, |text, s, primary| {
            let at = s.start();
            let (_, own) = leading_space(text, at);
            let before = last_non_blank_before(text, at);
            let indent = match &syntax {
                Some(view) => {
                    let level = view.snapshot().indent_level_after(at);
                    // A tree that says less than the line has (a flat file, a parse error):
                    // keep the line's own indentation.
                    let structural = unit.text(level);
                    if level == 0 && before.is_some_and(|c| opens_block(language, c)) {
                        format!("{own}{}", unit.text(1))
                    } else {
                        structural
                    }
                }
                None => match before {
                    Some(c) if opens_block(language, c) => format!("{own}{}", unit.text(1)),
                    _ => own.clone(),
                },
            };
            let after = text.char_at(s.end());
            let between = matches!(
                (before, after),
                (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
            ) && text.char_before(at) == before;
            if between {
                let first = format!("\n{indent}");
                let caret = first.len();
                return Change {
                    range: s.range(),
                    text: format!("{first}\n{own}"),
                    select: caret..caret,
                    reversed: false,
                    primary,
                };
            }
            Change::replace(s.range(), format!("\n{indent}"), primary)
        });
    }

    /// Tab: indents the lines of a selection that spans lines; else inserts one level at every
    /// caret (spaces up to the next stop in a file indented with spaces).
    pub(crate) fn tab(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let spans_lines = self
            .selections
            .iter()
            .any(|s| text.line_of_byte(s.start()).ok() != text.line_of_byte(s.end()).ok());
        if spans_lines {
            self.indent(cx);
            return;
        }
        let unit = self.buffer().read(cx).indent_unit();
        self.per_caret(Coalesce::Typing, cx, |text, s, primary| {
            let insert = match unit {
                IndentUnit::Tab => "\t".to_owned(),
                IndentUnit::Spaces(n) => {
                    let col = crate::motion::column_of(text, s.start());
                    " ".repeat(n - col % n.max(1))
                }
            };
            Change::replace(s.range(), insert, primary)
        });
    }

    /// The lines the selections cover, as merged blocks of line indices (a selection that ends
    /// at the start of a line does not cover it).
    pub(crate) fn line_blocks(&self, text: &Text) -> Vec<Range<usize>> {
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for s in self.selections.iter() {
            let first = text.line_of_byte(s.start()).unwrap_or(0);
            let mut last = text.line_of_byte(s.end()).unwrap_or(first);
            if last > first && text.line_start(last).ok() == Some(s.end()) {
                last -= 1;
            }
            match blocks.last_mut() {
                Some(prev) if first <= prev.end => prev.end = prev.end.max(last + 1),
                _ => blocks.push(first..last + 1),
            }
        }
        blocks
    }

    /// ⌘]: one level more at the start of every non-blank line of the selections.
    pub(crate) fn indent(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let level = self.buffer().read(cx).indent_unit().text(1);
        let mut edits = Vec::new();
        for block in self.line_blocks(&text) {
            for line in block {
                let Ok(content) = text.line_content_range(line) else {
                    continue;
                };
                if !content.is_empty() {
                    edits.push(Edit::insert(content.start, level.as_str()));
                }
            }
        }
        self.commit_mapped(edits, Coalesce::Step, cx);
    }

    /// ⌘[ / ⇧Tab: one level less at the start of every line of the selections.
    pub(crate) fn outdent(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let unit = self.buffer().read(cx).indent_unit();
        let mut edits = Vec::new();
        for block in self.line_blocks(&text) {
            for line in block {
                let Ok(start) = text.line_start(line) else {
                    continue;
                };
                let take = match text.char_at(start) {
                    Some('\t') => 1,
                    Some(' ') => {
                        let mut n = 0;
                        while n < unit.columns() && text.char_at(start + n) == Some(' ') {
                            n += 1;
                        }
                        n
                    }
                    _ => 0,
                };
                if take > 0 {
                    edits.push(Edit::delete(start..start + take));
                }
            }
        }
        self.commit_mapped(edits, Coalesce::Step, cx);
    }

    /// Backspace: the selection, else the character before the caret — both halves of an empty
    /// bracket pair, or back to the previous indentation stop in leading spaces.
    pub(crate) fn delete_backward(&mut self, cx: &mut Context<Self>) {
        let language = self.buffer().read(cx).language();
        let unit = self.buffer().read(cx).indent_unit();
        self.per_caret(Coalesce::Typing, cx, |text, s, primary| {
            if !s.is_empty() {
                return Change::replace(s.range(), String::new(), primary);
            }
            let head = s.head;
            let Some(prev) = text.char_before(head) else {
                return Change::caret(head, primary);
            };
            let start = head - prev.len_utf8();
            if let Some(next) = text.char_at(head)
                && closing_pair(language, prev) == Some(next)
            {
                return Change::replace(start..head + next.len_utf8(), String::new(), primary);
            }
            if let IndentUnit::Spaces(n) = unit {
                let (line_start, indent) = leading_space(text, head);
                if prev == ' '
                    && line_start + indent.len() == head
                    && !indent.contains('\t')
                    && n > 1
                {
                    let cols = indent.len();
                    let back = match cols % n {
                        0 => n,
                        k => k,
                    };
                    return Change::replace(head - back.min(cols)..head, String::new(), primary);
                }
            }
            Change::replace(start..head, String::new(), primary)
        });
    }

    /// Delete (forward): the selection, else the character after the caret.
    pub(crate) fn delete_forward(&mut self, cx: &mut Context<Self>) {
        self.delete_to(cx, |text, head| head..next_boundary(text, head));
    }

    pub(crate) fn delete_word_backward(&mut self, cx: &mut Context<Self>) {
        self.delete_to(cx, |text, head| word_start(text, head)..head);
    }

    pub(crate) fn delete_word_forward(&mut self, cx: &mut Context<Self>) {
        self.delete_to(cx, |text, head| head..word_end(text, head));
    }

    /// ⌘⌫: to the start of the line (the line break before it at the start of a line).
    pub(crate) fn delete_to_line_start(&mut self, cx: &mut Context<Self>) {
        self.delete_to(cx, |text, head| {
            let line = text.line_of_byte(head).unwrap_or(0);
            let start = text.line_start(line).unwrap_or(0);
            if start == head {
                prev_boundary(text, head)..head
            } else {
                start..head
            }
        });
    }

    /// To the end of the line (the line break after it at the end of a line).
    pub(crate) fn delete_to_line_end(&mut self, cx: &mut Context<Self>) {
        self.delete_to(cx, |text, head| {
            let line = text.line_of_byte(head).unwrap_or(0);
            let end = text.line_content_range(line).map_or(head, |r| r.end);
            if end == head {
                head..next_boundary(text, head)
            } else {
                head..end
            }
        });
    }

    /// Deletes every selection, or for a caret the range `range(text, head)`.
    fn delete_to(&mut self, cx: &mut Context<Self>, range: impl Fn(&Text, usize) -> Range<usize>) {
        self.per_caret(Coalesce::Step, cx, |text, s, primary| {
            let range = if s.is_empty() {
                range(text, s.head)
            } else {
                s.range()
            };
            Change::replace(range, String::new(), primary)
        });
    }

    /// The byte range of the lines `block`, with the line break after them; the last line of
    /// the file takes the break before it instead.
    fn block_range(text: &Text, block: &Range<usize>) -> Range<usize> {
        let start = text.line_start(block.start).unwrap_or(0);
        let end = text.line_range(block.end - 1).map_or(text.len(), |r| r.end);
        let has_break = text
            .line_content_range(block.end - 1)
            .is_ok_and(|content| content.end < end);
        if !has_break && start > 0 {
            prev_boundary(text, start)..end
        } else {
            start..end
        }
    }

    /// ⌘⇧K: the lines of the selections go; the carets land where the next line starts.
    pub(crate) fn delete_line(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let primary_line = text
            .line_of_byte(self.selections.primary().head)
            .unwrap_or(0);
        let changes = self
            .line_blocks(&text)
            .iter()
            .map(|block| Change {
                range: Self::block_range(&text, block),
                text: String::new(),
                select: 0..0,
                reversed: false,
                primary: block.contains(&primary_line),
            })
            .collect();
        self.apply_changes(changes, Coalesce::Step, cx);
    }

    /// ⌥⇧↑ / ⌥⇧↓: copies the lines of the selections above (`down: false`) or below them; the
    /// selections go with the lower copy for ⌥⇧↓.
    fn duplicate_lines(&mut self, down: bool, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let blocks = self.line_blocks(&text);
        let mut edits = Vec::new();
        // (block start offset, block end offset, inserted length)
        let mut spans = Vec::new();
        for block in &blocks {
            let start = text.line_start(block.start).unwrap_or(0);
            let end = text
                .line_content_range(block.end - 1)
                .map_or(text.len(), |r| r.end);
            let body = text.slice(start..end).unwrap_or_default();
            let insert = format!("\n{body}");
            spans.push((start, end, insert.len()));
            edits.push(Edit::insert(end, insert));
        }
        let mut shifted = Vec::with_capacity(self.selections.len());
        for s in self.selections.iter() {
            let mut delta = 0usize;
            for (start, end, len) in &spans {
                if *end < s.start() {
                    // The copy of an earlier block went in before this selection.
                    delta += len;
                } else {
                    // Its own block: the selection goes with the lower copy for ⌥⇧↓.
                    if down && s.start() >= *start {
                        delta += len;
                    }
                    break;
                }
            }
            shifted.push(Selection::new(s.anchor + delta, s.head + delta));
        }
        let after = Selections::new(shifted, self.selections.primary_index())
            .unwrap_or_else(|_| self.selections.clone());
        self.commit(edits, after, Coalesce::Step, cx);
    }

    pub(crate) fn duplicate_line_up(&mut self, cx: &mut Context<Self>) {
        self.duplicate_lines(false, cx);
    }

    pub(crate) fn duplicate_line_down(&mut self, cx: &mut Context<Self>) {
        self.duplicate_lines(true, cx);
    }

    /// ⌥↑ / ⌥↓: the lines of the selections swap with the line above or below them.
    fn move_lines(&mut self, up: bool, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let last_line = text.line_count().saturating_sub(1);
        let blocks = self.line_blocks(&text);
        if (up && blocks.first().is_some_and(|b| b.start == 0))
            || (!up && blocks.last().is_some_and(|b| b.end > last_line))
        {
            return;
        }
        let content_end = |line: usize| text.line_content_range(line).map_or(text.len(), |r| r.end);
        let line_start = |line: usize| text.line_start(line).unwrap_or(0);
        let mut edits = Vec::new();
        // (block start, block end, how far its selections move)
        let mut moves: Vec<(usize, usize, isize)> = Vec::new();
        for block in &blocks {
            let (start, end) = (line_start(block.start), content_end(block.end - 1));
            let body = text.slice(start..end).unwrap_or_default();
            if up {
                let other = block.start - 1;
                let other_start = line_start(other);
                let other_text = text
                    .slice(other_start..content_end(other))
                    .unwrap_or_default();
                edits.push(Edit::new(other_start..end, format!("{body}\n{other_text}")));
                moves.push((start, end, -((other_text.len() + 1) as isize)));
            } else {
                let other = block.end;
                let other_text = text
                    .slice(line_start(other)..content_end(other))
                    .unwrap_or_default();
                edits.push(Edit::new(
                    start..content_end(other),
                    format!("{other_text}\n{body}"),
                ));
                moves.push((start, end, (other_text.len() + 1) as isize));
            }
        }
        let shifted: Vec<Selection> = self
            .selections
            .iter()
            .map(|s| {
                let delta = moves
                    .iter()
                    .find(|(start, end, _)| s.start() >= *start && s.start() <= *end)
                    .map_or(0, |m| m.2);
                Selection::new(
                    s.anchor.saturating_add_signed(delta),
                    s.head.saturating_add_signed(delta),
                )
            })
            .collect();
        let after = Selections::new(shifted, self.selections.primary_index())
            .unwrap_or_else(|_| self.selections.clone());
        self.commit(edits, after, Coalesce::Step, cx);
    }

    pub(crate) fn move_line_up(&mut self, cx: &mut Context<Self>) {
        self.move_lines(true, cx);
    }

    pub(crate) fn move_line_down(&mut self, cx: &mut Context<Self>) {
        self.move_lines(false, cx);
    }

    /// ⌘/: comments the lines of the selections out, or in again when every non-blank one is
    /// commented.
    pub(crate) fn toggle_comment(&mut self, cx: &mut Context<Self>) {
        let Some(style) = comment_style(self.buffer().read(cx).language()) else {
            return;
        };
        let text = self.buffer().read(cx).text().clone();
        let mut edits = Vec::new();
        for block in self.line_blocks(&text) {
            let lines: Vec<(Range<usize>, String)> = block
                .filter_map(|line| {
                    let content = text.line_content_range(line).ok()?;
                    let body = text.slice(content.clone()).ok()?;
                    (!body.trim().is_empty()).then_some((content, body))
                })
                .collect();
            if lines.is_empty() {
                continue;
            }
            let commented = lines
                .iter()
                .all(|(_, body)| body.trim_start().starts_with(style.prefix));
            if commented {
                for (content, body) in &lines {
                    let at = content.start + (body.len() - body.trim_start().len());
                    let rest = &body[at - content.start + style.prefix.len()..];
                    let take = style.prefix.len() + usize::from(rest.starts_with(' '));
                    edits.push(Edit::delete(at..at + take));
                    if !style.suffix.is_empty() {
                        let trimmed = body.trim_end();
                        if let Some(before) = trimmed.strip_suffix(style.suffix) {
                            let space = usize::from(before.ends_with(' '));
                            let end = content.start + trimmed.len();
                            let from = end - style.suffix.len() - space;
                            if from >= at + take {
                                edits.push(Edit::delete(from..end));
                            }
                        }
                    }
                }
            } else {
                let indent = lines
                    .iter()
                    .map(|(_, body)| body.len() - body.trim_start().len())
                    .min()
                    .unwrap_or(0);
                for (content, _) in &lines {
                    edits.push(Edit::insert(
                        content.start + indent,
                        format!("{} ", style.prefix),
                    ));
                    if !style.suffix.is_empty() {
                        edits.push(Edit::insert(content.end, format!(" {}", style.suffix)));
                    }
                }
            }
        }
        self.commit_mapped(edits, Coalesce::Step, cx);
    }

    // ---- the clipboard -----------------------------------------------------------------------

    /// What a copy takes: the selections joined with line breaks, or — when every caret is
    /// empty — the whole lines of the carets.
    fn copied(&self, text: &Text) -> (String, String) {
        if self.selections.iter().all(Selection::is_empty) {
            let mut lines: Vec<usize> = self
                .selections
                .iter()
                .filter_map(|s| text.line_of_byte(s.head).ok())
                .collect();
            lines.dedup();
            let mut out = String::new();
            for line in lines {
                let Ok(range) = text.line_range(line) else {
                    continue;
                };
                out.push_str(&text.slice(range).unwrap_or_default());
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            return (out, format!("{CLIPBOARD_TAG}line"));
        }
        let pieces: Vec<String> = self
            .selections
            .iter()
            .map(|s| text.slice(s.range()).unwrap_or_default())
            .collect();
        let lens: Vec<String> = pieces.iter().map(|p| p.len().to_string()).collect();
        (
            pieces.join("\n"),
            format!("{CLIPBOARD_TAG}sel:{}", lens.join(",")),
        )
    }

    pub(crate) fn copy(&mut self, cx: &mut Context<Self>) {
        let text = self.buffer().read(cx).text().clone();
        let (copied, tag) = self.copied(&text);
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(copied, tag));
    }

    /// ⌘X: copies, then deletes the selections (the whole lines, without a selection).
    pub(crate) fn cut(&mut self, cx: &mut Context<Self>) {
        self.copy(cx);
        if !self.is_editable(cx) {
            return;
        }
        if self.selections.iter().all(Selection::is_empty) {
            self.delete_line(cx);
        } else {
            self.per_caret(Coalesce::Step, cx, |_, s, primary| {
                Change::replace(s.range(), String::new(), primary)
            });
        }
    }

    /// ⌘V: one piece per caret when the clipboard holds as many from a multi-caret copy; whole
    /// lines copied without a selection go above the carets' lines; else the text at every caret.
    pub(crate) fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(raw) = item.text() else {
            return;
        };
        let pasted = normalize_newlines(&raw).into_owned();
        let tag = item
            .metadata()
            .and_then(|m| m.strip_prefix(CLIPBOARD_TAG))
            .map(str::to_owned);
        let carets = self.selections.len();
        if let Some(lens) = tag.as_deref().and_then(|t| t.strip_prefix("sel:")) {
            let lens: Vec<usize> = lens.split(',').filter_map(|n| n.parse().ok()).collect();
            let fits = lens.len() == carets
                && carets > 1
                && lens.iter().sum::<usize>() + carets - 1 == pasted.len();
            if fits {
                let mut pieces = Vec::with_capacity(carets);
                let mut at = 0;
                for len in &lens {
                    pieces.push(pasted.get(at..at + len).unwrap_or_default().to_owned());
                    at += len + 1;
                }
                let mut index = 0;
                self.per_caret(Coalesce::Step, cx, |_, s, primary| {
                    let piece = pieces.get(index).cloned().unwrap_or_default();
                    index += 1;
                    Change::replace(s.range(), piece, primary)
                });
                return;
            }
        }
        if tag.as_deref() == Some("line") && self.selections.iter().all(Selection::is_empty) {
            self.per_caret(Coalesce::Step, cx, |text, s, primary| {
                let line = text.line_of_byte(s.head).unwrap_or(0);
                let start = text.line_start(line).unwrap_or(0);
                let caret = pasted.len() + (s.head - start);
                Change {
                    range: start..start,
                    text: pasted.clone(),
                    select: caret..caret,
                    reversed: false,
                    primary,
                }
            });
            return;
        }
        self.insert(&pasted, Coalesce::Step, cx);
    }

    // ---- undo ------------------------------------------------------------------------------

    pub(crate) fn undo(&mut self, cx: &mut Context<Self>) {
        let id = cx.entity_id();
        let applied = self.buffer().clone().update(cx, |b, cx| b.undo(id, cx));
        self.after_history(applied, cx);
    }

    pub(crate) fn redo(&mut self, cx: &mut Context<Self>) {
        let id = cx.entity_id();
        let applied = self.buffer().clone().update(cx, |b, cx| b.redo(id, cx));
        self.after_history(applied, cx);
    }

    /// Restores the selections an undo or redo recorded (else carries the current ones across).
    fn after_history(&mut self, applied: Option<hutan_buffer::Applied>, cx: &mut Context<Self>) {
        let Some(applied) = applied else {
            return;
        };
        let text = self.buffer().read(cx).text().clone();
        self.selections = match applied.transaction.selection_after() {
            Some(selections) => selections.clamp(&text),
            None => self.selections.map(&applied.transaction).clamp(&text),
        };
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }
}
