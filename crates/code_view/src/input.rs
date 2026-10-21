//! The platform's text input (`EntityInputHandler`): what the keyboard types arrives here as
//! text, in UTF-16 offsets of the whole file (the rope's UTF-16 metric makes every conversion
//! `O(log n)`). Input methods compose *marked* text in place: every update replaces the marked
//! ranges (one per caret) and the commit replaces them for good; the candidate window is put
//! under [`bounds_for_range`](gpui::EntityInputHandler::bounds_for_range). Keys are matched by
//! the character they produce, so Cyrillic or Dvorak layouts type what they show, and a
//! press-and-hold accent replaces the character before the caret through a replacement range.

use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, size};
use hutan_buffer::{Selection, Selections, Text};

use crate::buffer::Coalesce;
use crate::editing::Change;
use crate::view::CodeView;

/// A UTF-16 range of the file as bytes (clamped, on character boundaries).
fn bytes_of(text: &Text, range: &Range<usize>) -> Range<usize> {
    let to_byte = |utf16: usize| {
        let utf16 = utf16.min(text.utf16_len());
        text.utf16_to_byte(utf16)
            .or_else(|_| text.utf16_to_byte(utf16.saturating_sub(1)))
            .unwrap_or(text.len())
    };
    let start = to_byte(range.start);
    start..to_byte(range.end).max(start)
}

fn utf16_of(text: &Text, range: &Range<usize>) -> Range<usize> {
    let to = |byte: usize| text.byte_to_utf16(byte.min(text.len())).unwrap_or(0);
    to(range.start)..to(range.end)
}

/// Byte offset of UTF-16 offset `utf16` inside `s`.
fn utf16_in(s: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (at, c) in s.char_indices() {
        if units >= utf16 {
            return at;
        }
        units += c.len_utf16();
    }
    s.len()
}

impl CodeView {
    /// Ends an input method's composition: the marked text stays as it is. Returns whether
    /// there was one (a key that ends it does nothing else).
    pub(crate) fn end_composition(&mut self, cx: &mut Context<Self>) -> bool {
        if self.marked.is_empty() {
            return false;
        }
        self.marked.clear();
        self.buffer().update(cx, |b, _| b.break_group());
        cx.notify();
        true
    }

    /// The marked range that belongs to the primary caret.
    fn primary_marked(&self) -> Option<Range<usize>> {
        let head = self.selections.primary().head;
        self.marked
            .iter()
            .find(|r| r.start <= head && head <= r.end)
            .or_else(|| self.marked.first())
            .cloned()
    }

    /// Replaces `targets` (one per caret, `primary` among them) with `new_text`, then puts the
    /// carets `selected` (bytes into `new_text`) inside each and, when `mark`, marks the new
    /// text as being composed.
    fn compose(
        &mut self,
        targets: Vec<Range<usize>>,
        primary: usize,
        new_text: &str,
        selected: Option<Range<usize>>,
        mark: bool,
        cx: &mut Context<Self>,
    ) {
        let len = new_text.len();
        let select = selected.unwrap_or(len..len);
        let changes: Vec<Change> = targets
            .iter()
            .enumerate()
            .map(|(i, range)| Change {
                range: range.clone(),
                text: new_text.to_owned(),
                select: select.clone(),
                reversed: false,
                primary: i == primary,
            })
            .collect();
        // Where each replacement lands, for the marks.
        let mut sorted = targets;
        sorted.sort_by_key(|r| r.start);
        let mut shift = 0isize;
        let marks: Vec<Range<usize>> = sorted
            .iter()
            .map(|r| {
                let start = r.start.saturating_add_signed(shift);
                shift += len as isize - r.len() as isize;
                start..start + len
            })
            .collect();
        if self.apply_changes(changes, Coalesce::Typing, cx) && mark && len > 0 {
            self.marked = marks;
            cx.notify();
        }
    }

    /// What a replacement range of the platform (or none) means: the primary caret's piece
    /// alone for an explicit range; else the marked ranges, else every selection.
    fn targets(
        &self,
        text: &Text,
        range_utf16: Option<Range<usize>>,
    ) -> (Vec<Range<usize>>, usize) {
        if let Some(range) = range_utf16 {
            return (vec![bytes_of(text, &range)], 0);
        }
        if !self.marked.is_empty() {
            let head = self.selections.primary().head;
            let primary = self
                .marked
                .iter()
                .position(|r| r.start <= head && head <= r.end)
                .unwrap_or(0);
            return (self.marked.clone(), primary);
        }
        (
            self.selections.iter().map(Selection::range).collect(),
            self.selections.primary_index(),
        )
    }
}

impl EntityInputHandler for CodeView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.buffer().read(cx).text();
        let bytes = bytes_of(text, &range_utf16);
        *adjusted_range = Some(utf16_of(text, &bytes));
        text.slice(bytes).ok()
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.buffer().read(cx).text();
        let primary = self.selections.primary();
        Some(UTF16Selection {
            range: utf16_of(text, &primary.range()),
            reversed: primary.is_reversed(),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.primary_marked()?;
        Some(utf16_of(self.buffer().read(cx).text(), &marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.marked.is_empty() {
            self.marked.clear();
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Vim's Normal and Visual never take text, whatever the platform sends.
        if !self.is_editable(cx) || !self.vim_accepts_text() {
            return;
        }
        // Return and Tab are keys (actions), not text: a platform that also reports them as
        // text must not insert them a second time.
        if range_utf16.is_none()
            && self.marked.is_empty()
            && matches!(new_text, "\n" | "\r" | "\r\n" | "\t")
        {
            return;
        }
        if range_utf16.is_none() && self.marked.is_empty() {
            // Plain typing: the auto-pair rules apply.
            self.type_text(new_text, cx);
            return;
        }
        // The end of a composition, or a replacement (a press-and-hold accent).
        let text = self.buffer().read(cx).text().clone();
        let (targets, primary) = self.targets(&text, range_utf16);
        self.compose(targets, primary, new_text, None, false, cx);
        self.marked.clear();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable(cx) || !self.vim_accepts_text() {
            return;
        }
        let text = self.buffer().read(cx).text().clone();
        let (targets, primary) = self.targets(&text, range_utf16);
        let selected =
            new_selected_range.map(|r| utf16_in(new_text, r.start)..utf16_in(new_text, r.end));
        self.marked.clear();
        self.compose(targets, primary, new_text, selected, true, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout?;
        let bytes = bytes_of(self.buffer().read(cx).text(), &range_utf16);
        let start = self.point_of(bytes.start, cx)?;
        let end = self.point_of(bytes.end, cx)?;
        // The rectangle of the range on its first line (an input method puts its window under
        // it); at least one cell wide.
        let width = if end.y == start.y && end.x > start.x {
            end.x - start.x
        } else {
            layout.cell_width
        };
        Some(Bounds::new(start, size(width, layout.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let at = self.offset_at(point, cx)?;
        self.buffer().read(cx).text().byte_to_utf16(at).ok()
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.buffer().read(cx).text().clone();
        let bytes = bytes_of(&text, &range_utf16);
        self.selections = Selections::single(Selection::new(bytes.start, bytes.end));
        cx.notify();
    }

    fn text_length_utf16(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        Some(self.buffer().read(cx).text().utf16_len())
    }

    fn accepts_text_input(&self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.buffer().read(cx).is_editable() && self.vim_accepts_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_become_byte_ranges_on_boundaries() {
        let text = Text::from("a😀b\nдв");
        // 😀 is two UTF-16 units and four bytes.
        assert_eq!(bytes_of(&text, &(1..3)), 1..5);
        assert_eq!(utf16_of(&text, &(1..5)), 1..3);
        // Inside the surrogate pair: the start of the character.
        assert_eq!(bytes_of(&text, &(2..3)).start, 1);
        // Past the end: clamped.
        assert_eq!(bytes_of(&text, &(0..100)), 0..text.len());
        assert_eq!(utf16_in("かな", 1), 3);
    }
}
