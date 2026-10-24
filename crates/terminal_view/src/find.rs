//! Find in terminal output: what the view draws of a search (`terminal.search`, protocol 0.43).
//!
//! The owner of the view runs the search (the daemon holds the whole scrollback) and hands the
//! view the matches as [`FindSpan`]s; the view paints the cells of the ones on screen and scrolls
//! to the current one. Spans use the daemon's absolute rows, so they stay where the text is
//! while output scrolls the screen.

/// One match: the daemon's absolute rows and cell columns (`end_col` exclusive); `end_row` is
/// below `row` when the match continues onto the next row of a wrapped line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindSpan {
    pub row: u64,
    pub col: u16,
    pub end_row: u64,
    pub end_col: u16,
}

/// The matches to draw, oldest first and not overlapping, and which one is current.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindHighlights {
    spans: Vec<FindSpan>,
    current: Option<usize>,
}

/// Cells of one row that a match covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FoundCells {
    pub from: u16,
    /// Exclusive.
    pub to: u16,
    pub current: bool,
}

impl FindHighlights {
    /// `spans` must be sorted by position and not overlap (what the daemon answers).
    pub fn new(spans: Vec<FindSpan>, current: Option<usize>) -> Self {
        let current = current.filter(|index| *index < spans.len());
        Self { spans, current }
    }

    pub fn spans(&self) -> &[FindSpan] {
        &self.spans
    }

    /// Index of the current match.
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn current_span(&self) -> Option<FindSpan> {
        self.current
            .and_then(|index| self.spans.get(index).copied())
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The cells the matches cover on absolute row `row` of a grid `cols` wide, left to right.
    pub fn cells_on(&self, row: u64, cols: u16) -> Vec<FoundCells> {
        // Spans do not overlap, so their end rows grow with their start rows.
        let first = self.spans.partition_point(|span| span.end_row < row);
        let mut cells = Vec::new();
        for (index, span) in self.spans.iter().enumerate().skip(first) {
            if span.row > row {
                break;
            }
            let from = if span.row == row { span.col } else { 0 };
            let to = if span.end_row == row {
                span.end_col
            } else {
                cols
            };
            let (from, to) = (from.min(cols), to.min(cols));
            if from < to {
                cells.push(FoundCells {
                    from,
                    to,
                    current: self.current == Some(index),
                });
            }
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(row: u64, col: u16, end_row: u64, end_col: u16) -> FindSpan {
        FindSpan {
            row,
            col,
            end_row,
            end_col,
        }
    }

    #[test]
    fn a_match_inside_one_row_covers_its_columns_there_only() {
        let found = FindHighlights::new(vec![span(5, 3, 5, 7)], Some(0));
        assert_eq!(
            found.cells_on(5, 80),
            [FoundCells {
                from: 3,
                to: 7,
                current: true
            }]
        );
        assert!(found.cells_on(4, 80).is_empty());
        assert!(found.cells_on(6, 80).is_empty());
    }

    #[test]
    fn a_match_across_a_wrap_covers_the_end_of_one_row_and_the_start_of_the_next() {
        let found = FindHighlights::new(vec![span(2, 76, 4, 3)], None);
        let cells = |row| found.cells_on(row, 80);
        assert_eq!(
            cells(2),
            [FoundCells {
                from: 76,
                to: 80,
                current: false
            }]
        );
        // A match of three rows covers the whole middle one.
        assert_eq!(
            cells(3),
            [FoundCells {
                from: 0,
                to: 80,
                current: false
            }]
        );
        assert_eq!(
            cells(4),
            [FoundCells {
                from: 0,
                to: 3,
                current: false
            }]
        );
        assert!(cells(5).is_empty());
    }

    #[test]
    fn several_matches_on_a_row_and_only_the_current_one_is_marked() {
        let found = FindHighlights::new(
            vec![span(1, 0, 1, 2), span(1, 5, 1, 7), span(2, 0, 2, 1)],
            Some(1),
        );
        assert_eq!(
            found
                .cells_on(1, 80)
                .iter()
                .map(|c| (c.from, c.to, c.current))
                .collect::<Vec<_>>(),
            [(0, 2, false), (5, 7, true)]
        );
        assert_eq!(found.current_span(), Some(span(1, 5, 1, 7)));
    }

    #[test]
    fn a_current_index_beyond_the_matches_is_dropped_and_columns_stay_in_the_grid() {
        let found = FindHighlights::new(vec![span(0, 70, 0, 90)], Some(4));
        assert_eq!(found.current(), None);
        assert_eq!(found.cells_on(0, 80)[0].to, 80);
        assert!(FindHighlights::default().cells_on(0, 80).is_empty());
    }
}
