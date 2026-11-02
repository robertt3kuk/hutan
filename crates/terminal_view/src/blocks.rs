//! The block overlay without GPUI: which block owns which model line, the status chip, folds
//! and the mapping of display rows to lines.
//!
//! Blocks come from the daemon in absolute line numbers (`terminal.blocks`); the model maps them
//! onto its own lines with the `viewport_top` of the last render ([`TerminalModel::line_of`]).
//! Folding is a view concern: a folded block's output lines are skipped and one summary row
//! stands for them, so every other row moves up and older lines fill the bottom-anchored view.
//! The daemon's screen, the scrollback and other clients never see it.

use hutan_protocol::blocks::{Block, BlockState};

use crate::model::TerminalModel;

/// The color family of a block's gutter bar and chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Success,
    Error,
    /// Running, or finished without an exit code.
    Neutral,
}

/// The tone of `block`'s gutter bar; `None` while only its prompt is shown (nothing ran yet).
pub fn tone(block: &Block) -> Option<Tone> {
    match block.state {
        BlockState::Prompt => None,
        BlockState::Running => Some(Tone::Neutral),
        BlockState::Finished | BlockState::Truncated => Some(match block.exit_code {
            Some(0) => Tone::Success,
            Some(_) => Tone::Error,
            None => Tone::Neutral,
        }),
    }
}

/// Wall time of a finished block in milliseconds.
pub fn duration_ms(block: &Block) -> Option<u64> {
    Some(block.finished_at_ms?.saturating_sub(block.started_at_ms))
}

/// `450 ms`, `1.2 s`, `12 s`, `3 m`, `3 m 20 s`, `1 h 5 m`: two significant units at most, whole
/// units above ten seconds.
pub fn format_duration(ms: u64) -> String {
    if ms < 1_000 {
        return format!("{ms} ms");
    }
    if ms < 10_000 {
        // One decimal, truncated: 9.96 s must not read 10.0 s.
        let tenths = ms / 100;
        return format!("{}.{} s", tenths / 10, tenths % 10);
    }
    let seconds = ms / 1_000;
    if seconds < 60 {
        return format!("{seconds} s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return match seconds % 60 {
            0 => format!("{minutes} m"),
            rest => format!("{minutes} m {rest} s"),
        };
    }
    match minutes % 60 {
        0 => format!("{} h", minutes / 60),
        rest => format!("{} h {rest} m", minutes / 60),
    }
}

/// The right-aligned chip of a block's prompt line: `✓ 1.2 s`, `✗ 2 1.2 s`; `None` while the
/// command has not finished (or no exit status and no time is known).
pub fn chip_text(block: &Block) -> Option<String> {
    if !matches!(block.state, BlockState::Finished | BlockState::Truncated) {
        return None;
    }
    let status = match block.exit_code {
        Some(0) => Some("✓".to_owned()),
        Some(code) => Some(format!("✗ {code}")),
        None => None,
    };
    let duration = duration_ms(block).map(format_duration);
    match (status, duration) {
        (Some(status), Some(duration)) => Some(format!("{status} {duration}")),
        (Some(status), None) => Some(status),
        (None, Some(duration)) => Some(duration),
        (None, None) => None,
    }
}

/// The chip of a failed command cut down to what matters, `✗ 128`: the exit status without the
/// duration. `None` for a command that did not fail with a code.
pub fn compact_chip_text(block: &Block) -> Option<String> {
    if !matches!(block.state, BlockState::Finished | BlockState::Truncated) {
        return None;
    }
    match block.exit_code {
        Some(code) if code != 0 => Some(format!("✗ {code}")),
        _ => None,
    }
}

/// Which form of a block's chip is drawn, and on which of the block's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipSpot {
    /// Index into the block's rows (0 = its prompt row).
    pub row: usize,
    /// The compact form (`✗ 128`) instead of the full one (`✗ 128 32 ms`).
    pub compact: bool,
}

/// Whether a chip `cols` wide fits right-aligned on a row `used` columns wide in a terminal
/// `model_cols` wide: one blank column after the text and two before it.
fn chip_fits(model_cols: u16, cols: u16, used: u16) -> bool {
    model_cols
        .checked_sub(cols.saturating_add(1))
        .is_some_and(|col| col >= 2 && used < col)
}

/// Where the chip of a block is drawn. `used` is the width of each of the block's rows that are
/// on screen (the prompt row first), `full` and `compact` the widths of the two forms
/// (`compact` is `None` for a command that did not fail).
///
/// A success chip is drawn in full on the prompt row when it fits there, and not at all
/// otherwise. A failure is never left without its exit status (the red bar alone does not say
/// *which* code): the full chip on the prompt row, else the compact one there, else the compact
/// one on the last row of the block that has room, and when every row is full, over the end of
/// the block's last row.
pub fn place_chip(
    model_cols: u16,
    full: u16,
    compact: Option<u16>,
    used: &[u16],
) -> Option<ChipSpot> {
    let first = *used.first()?;
    if chip_fits(model_cols, full, first) {
        return Some(ChipSpot {
            row: 0,
            compact: false,
        });
    }
    let compact = compact?;
    if chip_fits(model_cols, compact, first) {
        return Some(ChipSpot {
            row: 0,
            compact: true,
        });
    }
    if let Some(row) = (1..used.len())
        .rev()
        .find(|row| chip_fits(model_cols, compact, used[*row]))
    {
        return Some(ChipSpot { row, compact: true });
    }
    // Nothing has room: cover the end of the block's last row.
    (model_cols > compact).then_some(ChipSpot {
        row: used.len() - 1,
        compact: true,
    })
}

/// A folded block: its output lines (model lines `start..end`) shown as one summary row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    pub block_id: u64,
    pub start: i64,
    /// Exclusive.
    pub end: i64,
    pub exit_code: Option<i32>,
}

impl Fold {
    /// `… 241 lines, exit 0`.
    pub fn summary(&self) -> String {
        let lines = self.end - self.start;
        let noun = if lines == 1 { "line" } else { "lines" };
        match self.exit_code {
            Some(code) => format!("… {lines} {noun}, exit {code}"),
            None => format!("… {lines} {noun}"),
        }
    }
}

/// Whether the output of `block` can be folded: a finished command with at least one output line.
pub fn foldable(block: &Block) -> bool {
    block.state == BlockState::Finished
        && matches!((block.output_start, block.end_line), (Some(start), Some(end)) if start < end)
}

/// The folds in effect, in ascending line order.
pub fn folds(model: &TerminalModel) -> Vec<Fold> {
    let mut out: Vec<Fold> = model
        .blocks()
        .iter()
        .filter(|block| foldable(block) && model.is_collapsed(block.id))
        .filter_map(|block| {
            Some(Fold {
                block_id: block.id,
                start: model.line_of(block.output_start?),
                end: model.line_of(block.end_line?),
                exit_code: block.exit_code,
            })
        })
        .collect();
    out.sort_by_key(|fold| fold.start);
    // Blocks never overlap; a broken list must not make the layout loop.
    let mut previous_end = i64::MIN;
    out.retain(|fold| {
        let keep = fold.start >= previous_end && fold.start < fold.end;
        if keep {
            previous_end = fold.end;
        }
        keep
    });
    out
}

/// What the last paint drew of the overlay, in grid rows; for tests and inspection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayReport {
    /// Gutter bars: rows `from..to` in a tone.
    pub bars: Vec<(u16, u16, Tone)>,
    /// Rows with a separator line above them.
    pub separators: Vec<u16>,
    /// Status chips: row and text.
    pub chips: Vec<(u16, String)>,
    /// Rows tinted as the highlighted block.
    pub highlighted: Vec<u16>,
    /// Rows that show the summary of a folded block.
    pub summaries: Vec<u16>,
}

/// What one row of the view shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayRow {
    /// A model line.
    Line(i64),
    /// The summary of a folded block; `line` is its first hidden line.
    Summary(Fold),
}

impl DisplayRow {
    /// The model line the row stands for (a summary: its first hidden line).
    pub fn line(&self) -> i64 {
        match self {
            DisplayRow::Line(line) => *line,
            DisplayRow::Summary(fold) => fold.start,
        }
    }
}

/// The rows of a view `rows` tall whose last row shows `last_line`, filled upwards: lines of
/// folded blocks are skipped (one [`DisplayRow::Summary`] each), and the walk stops at the oldest
/// retained line, in which case the result is shorter than `rows` (the view shows it from its
/// top). Without folds this is exactly `last_line - rows + 1 ..= last_line`.
pub fn layout_rows(oldest: i64, folds: &[Fold], last_line: i64, rows: usize) -> Vec<DisplayRow> {
    let count = i64::try_from(rows).unwrap_or(i64::MAX);
    if folds.is_empty() {
        let first = last_line
            .saturating_sub(count - 1)
            .max(oldest.min(last_line));
        return (first..=last_line).map(DisplayRow::Line).collect();
    }
    let mut out = Vec::with_capacity(rows);
    let mut line = last_line;
    while out.len() < rows && line >= oldest {
        let at = folds.partition_point(|fold| fold.end <= line);
        match folds.get(at).filter(|fold| fold.start <= line) {
            Some(fold) => {
                out.push(DisplayRow::Summary(fold.clone()));
                line = fold.start - 1;
            }
            None => {
                out.push(DisplayRow::Line(line));
                line -= 1;
            }
        }
    }
    out.reverse();
    out
}

/// Model lines `first..end` a block covers, from its prompt to where the next one starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSpan {
    pub id: u64,
    pub first: i64,
    /// Exclusive.
    pub end: i64,
}

impl BlockSpan {
    pub fn contains(&self, line: i64) -> bool {
        self.first <= line && line < self.end
    }
}

/// The span of every block, oldest first. `cursor_line` is the model line of the cursor: the last
/// block runs up to it while its command runs.
pub fn spans(model: &TerminalModel, cursor_line: i64) -> Vec<BlockSpan> {
    let blocks = model.blocks();
    let count = blocks.len();
    let mut out = Vec::with_capacity(count);
    let mut iter = blocks.iter().peekable();
    while let Some(block) = iter.next() {
        let first = model.line_of(block.prompt_line);
        let end = match iter.peek() {
            Some(next) => model.line_of(next.prompt_line),
            None => match block.state {
                BlockState::Prompt => first + 1,
                BlockState::Running => cursor_line + 1,
                BlockState::Finished | BlockState::Truncated => {
                    block.end_line.map_or(first + 1, |end| model.line_of(end))
                }
            },
        };
        out.push(BlockSpan {
            id: block.id,
            first,
            end: end.max(first + 1),
        });
    }
    out
}

/// The block whose span holds `line`.
pub fn block_at(spans: &[BlockSpan], line: i64) -> Option<u64> {
    let at = spans.partition_point(|span| span.end <= line);
    spans
        .get(at)
        .filter(|span| span.contains(line))
        .map(|span| span.id)
}

/// Model lines of a block's output: `output_start..end_line`, and for a running command up to and
/// including the cursor's line. `None` when the block has no output (yet).
pub fn output_lines(model: &TerminalModel, block: &Block, cursor_line: i64) -> Option<(i64, i64)> {
    let start = model.line_of(block.output_start?);
    let end = match block.state {
        BlockState::Running => cursor_line + 1,
        _ => model.line_of(block.end_line?),
    };
    (start < end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hutan_client::BlockList;
    use hutan_protocol::blocks::TerminalBlocksResult;

    use super::*;

    fn block(id: u64, prompt: u64, state: BlockState) -> Block {
        Block {
            id,
            prompt_line: prompt,
            command_line: None,
            output_start: None,
            end_line: None,
            command: None,
            exit_code: None,
            started_at_ms: 1_000,
            finished_at_ms: None,
            cwd: Some(PathBuf::from("/repo")),
            state,
        }
    }

    fn finished(id: u64, prompt: u64, out: (u64, u64), exit: i32, ms: u64) -> Block {
        Block {
            command_line: Some(prompt),
            output_start: Some(out.0),
            end_line: Some(out.1),
            command: Some(format!("cmd{id}")),
            exit_code: Some(exit),
            finished_at_ms: Some(1_000 + ms),
            ..block(id, prompt, BlockState::Finished)
        }
    }

    fn model_with(blocks: Vec<Block>) -> TerminalModel {
        let mut model = TerminalModel::new();
        let mut list = BlockList::new();
        list.replace(TerminalBlocksResult {
            blocks: blocks.into_iter().rev().collect(),
            viewport_top: 0,
        });
        model.set_blocks(list);
        model
    }

    #[test]
    fn durations_read_like_the_chip_wants() {
        for (ms, text) in [
            (0, "0 ms"),
            (450, "450 ms"),
            (999, "999 ms"),
            (1_000, "1.0 s"),
            (1_234, "1.2 s"),
            (9_999, "9.9 s"),
            (10_000, "10 s"),
            (59_999, "59 s"),
            (60_000, "1 m"),
            (180_000, "3 m"),
            (200_000, "3 m 20 s"),
            (3_600_000, "1 h"),
            (3_900_000, "1 h 5 m"),
        ] {
            assert_eq!(format_duration(ms), text, "{ms} ms");
        }
    }

    #[test]
    fn a_failure_keeps_its_code_where_the_full_chip_does_not_fit() {
        // `✗ 128 32 ms` is 11 columns, `✗ 128` 5; the pane is 80 columns wide.
        let place = |used: &[u16]| place_chip(80, 11, Some(5), used);
        let spot = |row, compact| Some(ChipSpot { row, compact });
        assert_eq!(place(&[40, 10]), spot(0, false), "plenty of room");
        // The full chip needs the prompt row to end before column 68, the compact one before 74.
        assert_eq!(place(&[69, 10]), spot(0, true), "the code without the time");
        assert_eq!(
            place(&[74, 10, 30]),
            spot(2, true),
            "the last row with room"
        );
        assert_eq!(place(&[74, 79, 30, 79]), spot(2, true));
        assert_eq!(place(&[80, 80, 80]), spot(2, true), "over the last row");
        assert_eq!(place(&[80]), spot(0, true), "a one-row block");
        // A success has no compact form: it is drawn in full on its prompt row or not at all.
        assert_eq!(place_chip(80, 6, None, &[40, 10]), spot(0, false));
        assert_eq!(place_chip(80, 6, None, &[75, 10]), None);
        // Too narrow for even the code: nothing is drawn.
        assert_eq!(place_chip(5, 11, Some(5), &[0, 0]), None);
        assert_eq!(place(&[]), None);
    }

    #[test]
    fn only_a_failure_has_a_compact_chip() {
        assert_eq!(
            compact_chip_text(&finished(1, 0, (1, 2), 128, 32)),
            Some("✗ 128".into())
        );
        assert_eq!(compact_chip_text(&finished(1, 0, (1, 2), 0, 32)), None);
        assert_eq!(compact_chip_text(&block(1, 0, BlockState::Running)), None);
        let mut lost = finished(1, 0, (1, 2), 0, 1);
        lost.exit_code = None;
        assert_eq!(compact_chip_text(&lost), None);
    }

    #[test]
    fn chips_and_tones_follow_the_state() {
        assert_eq!(tone(&block(1, 0, BlockState::Prompt)), None);
        assert_eq!(chip_text(&block(1, 0, BlockState::Prompt)), None);
        let running = block(1, 0, BlockState::Running);
        assert_eq!(tone(&running), Some(Tone::Neutral));
        assert_eq!(chip_text(&running), None);
        let ok = finished(1, 0, (1, 3), 0, 1_234);
        assert_eq!(
            (tone(&ok), chip_text(&ok)),
            (Some(Tone::Success), Some("✓ 1.2 s".into()))
        );
        let failed = finished(2, 3, (4, 4), 2, 180_000);
        assert_eq!(
            (tone(&failed), chip_text(&failed)),
            (Some(Tone::Error), Some("✗ 2 3 m".into()))
        );
        // The next prompt ended the command without a `D`: no status, no time.
        let lost = Block {
            exit_code: None,
            finished_at_ms: None,
            ..finished(3, 4, (5, 6), 0, 0)
        };
        assert_eq!((tone(&lost), chip_text(&lost)), (Some(Tone::Neutral), None));
    }

    #[test]
    fn folds_cover_the_output_and_summarize_it() {
        let mut model = model_with(vec![
            finished(1, 0, (1, 4), 0, 10),
            finished(2, 4, (5, 5), 1, 10),
            block(3, 5, BlockState::Prompt),
        ]);
        // Nothing is folded yet; a block without output cannot be folded at all.
        assert!(folds(&model).is_empty());
        assert!(model.set_block_collapsed(1, true));
        assert!(model.set_block_collapsed(2, true));
        let found = folds(&model);
        assert_eq!(
            found.len(),
            1,
            "an empty output has nothing to fold: {found:?}"
        );
        assert_eq!((found[0].start, found[0].end), (1, 4));
        assert_eq!(found[0].summary(), "… 3 lines, exit 0");
        assert!(
            !model.set_block_collapsed(99, true),
            "unknown blocks do not fold"
        );
        assert!(model.set_block_collapsed(1, false));
        assert!(folds(&model).is_empty());
    }

    #[test]
    fn rows_without_folds_are_a_plain_window_and_never_pass_the_oldest_line() {
        let rows = layout_rows(0, &[], 9, 4);
        assert_eq!(rows, (6..=9).map(DisplayRow::Line).collect::<Vec<_>>());
        // Fewer lines than rows: the view starts at the oldest line.
        let rows = layout_rows(5, &[], 7, 6);
        assert_eq!(rows, (5..=7).map(DisplayRow::Line).collect::<Vec<_>>());
    }

    #[test]
    fn a_fold_is_one_row_and_older_lines_fill_the_view_from_the_bottom() {
        let fold = Fold {
            block_id: 1,
            start: 3,
            end: 8,
            exit_code: Some(0),
        };
        let rows = layout_rows(0, std::slice::from_ref(&fold), 9, 5);
        assert_eq!(
            rows,
            vec![
                DisplayRow::Line(1),
                DisplayRow::Line(2),
                DisplayRow::Summary(fold.clone()),
                DisplayRow::Line(8),
                DisplayRow::Line(9),
            ]
        );
        // The bottom row inside the fold: the summary is the last row.
        let rows = layout_rows(0, std::slice::from_ref(&fold), 5, 3);
        assert_eq!(rows.last(), Some(&DisplayRow::Summary(fold.clone())));
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], DisplayRow::Line(1));
        // Not enough history above: the rows start at the oldest line and the view is short.
        let rows = layout_rows(2, std::slice::from_ref(&fold), 9, 8);
        assert_eq!(rows[0], DisplayRow::Line(2));
        assert_eq!(rows.len(), 4);
        assert_eq!(DisplayRow::Summary(fold).line(), 3);
    }

    #[test]
    fn spans_run_from_a_prompt_to_the_next_and_clip_a_running_block_at_the_cursor() {
        let model = model_with(vec![
            finished(1, 0, (1, 4), 0, 10),
            Block {
                output_start: Some(6),
                ..block(2, 4, BlockState::Running)
            },
        ]);
        let spans = spans(&model, 8);
        assert_eq!(
            spans,
            vec![
                BlockSpan {
                    id: 1,
                    first: 0,
                    end: 4
                },
                BlockSpan {
                    id: 2,
                    first: 4,
                    end: 9
                },
            ]
        );
        assert_eq!(block_at(&spans, 0), Some(1));
        assert_eq!(block_at(&spans, 3), Some(1));
        assert_eq!(block_at(&spans, 4), Some(2));
        assert_eq!(block_at(&spans, 8), Some(2));
        assert_eq!(block_at(&spans, 9), None, "below the cursor nothing runs");
        assert_eq!(block_at(&spans, -1), None);
        let running = model.blocks().get(2).unwrap();
        assert_eq!(output_lines(&model, running, 8), Some((6, 9)));
        assert_eq!(
            output_lines(&model, model.blocks().get(1).unwrap(), 8),
            Some((1, 4))
        );
    }
}
