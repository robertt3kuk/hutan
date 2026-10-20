//! `TextProvider` for the editor's rope and the conversion of a buffer transaction into the edits
//! the parser needs (`buffer` feature).

use hutan_buffer::{Text, Transaction};

use crate::error::SyntaxError;
use crate::text::{Point, TextEdit, TextProvider};

impl TextProvider for Text {
    fn len(&self) -> usize {
        Text::len(self)
    }

    fn chunk(&self, offset: usize) -> &[u8] {
        if offset >= Text::len(self) {
            return &[];
        }
        // `chunks_in` wants a character boundary; a mid-character offset starts at the boundary
        // before it and skips the bytes already consumed.
        let boundary = self.floor_char_boundary(offset);
        let skip = offset - boundary;
        match self.chunks_in(boundary..Text::len(self)) {
            Ok(mut chunks) => chunks
                .next()
                .map(|chunk| &chunk.as_bytes()[skip.min(chunk.len())..])
                .unwrap_or_default(),
            Err(_) => &[],
        }
    }

    fn point_at(&self, offset: usize) -> Point {
        let offset = offset.min(Text::len(self));
        // `offset` is in range, so the rope answers; the default keeps the trait infallible.
        self.line_of_byte(offset)
            .and_then(|row| {
                let start = Text::line_start(self, row)?;
                Ok(Point {
                    row,
                    column: offset - start,
                })
            })
            .unwrap_or_default()
    }

    fn line_start(&self, row: usize) -> usize {
        Text::line_start(self, row).unwrap_or(Text::len(self))
    }
}

/// The edits of `transaction` (of the buffer whose text *before* it is `old`), in the order to
/// feed [`Syntax::edit`](crate::Syntax::edit): last to first, so every edit's coordinates are
/// valid after the ones before it were applied.
pub fn edits_of_transaction(
    old: &Text,
    transaction: &Transaction,
) -> Result<Vec<TextEdit>, SyntaxError> {
    transaction
        .edits()
        .iter()
        .rev()
        .map(|edit| TextEdit::new(old, edit.range.clone(), &edit.text))
        .collect()
}
