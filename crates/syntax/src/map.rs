//! Moving offsets across edits: how stale highlights follow typing until the new tree lands.

use std::ops::Range;

use crate::snapshot::HighlightSpan;
use crate::text::TextEdit;

/// Which side of an insertion at exactly the mapped offset the result lands on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// Stay before text inserted at the offset.
    Before,
    /// Move after it.
    After,
}

/// Maps `offset` of the text before `edit` to the text after it.
fn forward(offset: usize, (start, old_end, new_end): (usize, usize, usize), side: Side) -> usize {
    if offset < start {
        offset
    } else if offset == start {
        // A pure insertion at the offset is before or after it; a replacement keeps its start.
        if old_end == start && side == Side::After {
            new_end
        } else {
            start
        }
    } else if offset < old_end {
        match side {
            Side::Before => start,
            Side::After => new_end,
        }
    } else {
        // Both an offset at the end of the replaced range and one past it follow the shift.
        offset - old_end + new_end
    }
}

fn parts(edit: &TextEdit) -> (usize, usize, usize) {
    (edit.start, edit.old_end, edit.new_end)
}

/// Offset before `edits` → offset after all of them.
pub(crate) fn map_forward(edits: &[TextEdit], offset: usize, side: Side) -> usize {
    edits
        .iter()
        .fold(offset, |at, edit| forward(at, parts(edit), side))
}

/// Offset after `edits` → the offset it came from before them (the edits applied backwards).
pub(crate) fn map_backward(edits: &[TextEdit], offset: usize, side: Side) -> usize {
    edits.iter().rev().fold(offset, |at, edit| {
        // The inverse of "replace start..old_end by start..new_end" swaps the two ends.
        forward(at, (edit.start, edit.new_end, edit.old_end), side)
    })
}

/// Highlights of the old text carried onto the new one: a span follows its text, grows when text
/// is typed inside it, covers the replacement when its text was replaced exactly, and disappears
/// when a larger deletion swallowed it. Text typed exactly at the start or the end of a span is not
/// part of it.
pub(crate) fn map_spans(
    spans: Vec<HighlightSpan>,
    edits: &[TextEdit],
    clip: &Range<usize>,
) -> Vec<HighlightSpan> {
    let mut out: Vec<HighlightSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        let start = map_forward(edits, span.range.start, Side::After).max(clip.start);
        let end = map_forward(edits, span.range.end, Side::Before).min(clip.end);
        if start >= end {
            continue;
        }
        // Mapping is monotonic, so spans stay sorted; `max` only guards equal boundaries.
        let start = out.last().map_or(start, |last| start.max(last.range.end));
        if start >= end {
            continue;
        }
        out.push(HighlightSpan {
            range: start..end,
            token: span.token,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Point;

    fn edit(start: usize, old_end: usize, new_end: usize) -> TextEdit {
        TextEdit {
            start,
            old_end,
            new_end,
            start_point: Point::default(),
            old_end_point: Point::default(),
            new_end_point: Point::default(),
        }
    }

    #[test]
    fn insertion_shifts_what_follows() {
        let edits = [edit(5, 5, 8)];
        assert_eq!(map_forward(&edits, 3, Side::Before), 3);
        assert_eq!(map_forward(&edits, 5, Side::Before), 5);
        assert_eq!(map_forward(&edits, 5, Side::After), 8);
        assert_eq!(map_forward(&edits, 6, Side::Before), 9);
    }

    #[test]
    fn replacement_collapses_inside() {
        let edits = [edit(2, 6, 4)];
        assert_eq!(map_forward(&edits, 2, Side::After), 2);
        assert_eq!(map_forward(&edits, 4, Side::Before), 2);
        assert_eq!(map_forward(&edits, 4, Side::After), 4);
        assert_eq!(map_forward(&edits, 6, Side::Before), 4);
        assert_eq!(map_forward(&edits, 9, Side::Before), 7);
    }

    #[test]
    fn backward_inverts_forward_outside_edits() {
        let edits = [edit(2, 4, 7), edit(10, 12, 11)];
        for offset in [0, 1, 4, 5, 6, 20] {
            let there = map_forward(&edits, offset, Side::Before);
            assert_eq!(map_backward(&edits, there, Side::Before), offset);
        }
    }

    #[test]
    fn typing_inside_a_span_grows_it_and_at_its_edges_does_not() {
        use crate::token::Token;
        let spans = vec![HighlightSpan {
            range: 4..9,
            token: Token::String,
        }];
        let inside = map_spans(spans.clone(), &[edit(6, 6, 7)], &(0..30));
        assert_eq!(inside[0].range, 4..10);
        let at_end = map_spans(spans.clone(), &[edit(9, 9, 10)], &(0..30));
        assert_eq!(at_end[0].range, 4..9);
        let at_start = map_spans(spans.clone(), &[edit(4, 4, 5)], &(0..30));
        assert_eq!(at_start[0].range, 5..10);
        let replaced = map_spans(spans, &[edit(4, 9, 6)], &(0..30));
        assert_eq!(replaced[0].range, 4..6);
    }
}
