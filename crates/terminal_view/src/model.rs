//! GPUI-free terminal state: the local copy of the daemon's screen plus the scrollback cache.
//!
//! The daemon is the only VT parser (ADR 0004). This model only applies its snapshots and frames
//! and remembers history pages the view asked for.
//!
//! # Line numbering
//! Selection and the scrollback cache use *absolute* lines: history lines are numbered from the
//! oldest retained one upwards and viewport row `r` is `history_total + r`, where `history_total`
//! only grows. Content that scrolls into history keeps its number, so a selection (or a cached
//! page) stays attached to its text while output continues.

use std::collections::{BTreeSet, HashMap};

use hutan_client::BlockList;
use hutan_protocol::blocks::Block;
use hutan_protocol::render::{
    CellFlags, Cursor, CursorShape, Modes, Palette, Row, Run, TerminalFrame, TerminalRender,
    TerminalSnapshot,
};
use unicode_width::UnicodeWidthChar;

/// Rows requested per `terminal.history` call when scrolling back.
pub const HISTORY_PAGE: u32 = 200;

/// One grid cell of a row: a base character plus any combining characters after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell<'a> {
    pub col: u16,
    /// Columns covered: 2 for wide characters, else 1.
    pub width: u8,
    pub text: &'a str,
    pub run: &'a Run,
}

impl Cell<'_> {
    /// First column after the cell.
    pub fn end(&self) -> u16 {
        self.col + u16::from(self.width)
    }
}

/// Columns one character of a run occupies; zero for combining marks. Control characters count
/// as one column (they are drawn as blanks).
pub(crate) fn char_width(c: char, wide: bool) -> u8 {
    match c.width() {
        Some(0) => 0,
        _ if wide => 2,
        _ => 1,
    }
}

/// The cells of `row` in column order. Runs are contiguous from column 0; columns after the last
/// run are blank.
pub fn row_cells(row: &Row) -> Vec<Cell<'_>> {
    let mut cells: Vec<Cell<'_>> = Vec::new();
    let mut col: u16 = 0;
    for run in &row.runs {
        let wide = run.flags.contains(CellFlags::WIDE);
        let text = run.text.as_str();
        // Byte start and width of the cell being collected.
        let mut open: Option<(usize, u8)> = None;
        let mut push = |start: usize, end: usize, width: u8| {
            cells.push(Cell {
                col,
                width,
                text: &text[start..end],
                run,
            });
            col = col.saturating_add(u16::from(width));
        };
        for (i, c) in text.char_indices() {
            let w = char_width(c, wide);
            if w == 0 && open.is_some() {
                continue; // combining mark: stays inside the open cell
            }
            if let Some((start, width)) = open.replace((i, w.max(1))) {
                push(start, i, width);
            }
        }
        if let Some((start, width)) = open {
            push(start, text.len(), width);
        }
    }
    cells
}

/// Width of the row's content in columns.
pub fn row_width(row: &Row) -> u16 {
    row_cells(row).last().map_or(0, |c| c.end())
}

/// What an applied render changed, for the view's bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub frame: u64,
    pub snapshot: bool,
    /// The grid size changed (always true for the first snapshot).
    pub resized: bool,
    /// Lines that entered history with this render.
    pub history_grew: u32,
    /// History was discarded (cleared, or reflowed by a resize).
    pub history_reset: bool,
    pub title_changed: bool,
    pub modes_changed: bool,
    /// Viewport rows whose content was replaced (after `scrolled` was applied).
    pub dirty: Vec<u16>,
    /// The daemon's absolute line number of viewport row 0 *before* this render (the daemon's
    /// numbering survives a resize, the model's `history_total` does not): a view that was
    /// scrolled back finds its reading place again with it, see `TerminalView::after_apply`.
    pub prev_viewport_top: u64,
}

#[derive(Debug)]
pub struct TerminalModel {
    ready: bool,
    frame: u64,
    cols: u16,
    rows: Vec<Row>,
    cursor: Cursor,
    modes: Modes,
    palette: Palette,
    title: Option<String>,
    links: HashMap<u32, String>,
    history_len: u32,
    history_total: u64,
    /// Fetched scrollback by absolute line.
    history: HashMap<u64, Row>,
    /// The daemon's absolute line number of viewport row 0 (`viewport_top` of the last render):
    /// block lines map to rows with it, see [`Self::line_of`].
    viewport_top: u64,
    /// Blocks of this terminal (protocol 0.5), kept current by the owner.
    blocks: BlockList,
    /// Ids of blocks whose output the views fold into one summary row.
    collapsed: BTreeSet<u64>,
}

impl Default for TerminalModel {
    fn default() -> Self {
        Self {
            ready: false,
            frame: 0,
            cols: 0,
            rows: Vec::new(),
            cursor: Cursor {
                col: 0,
                row: 0,
                shape: CursorShape::Block,
                visible: false,
                blinking: false,
            },
            modes: Modes::default(),
            palette: Palette::default(),
            title: None,
            links: HashMap::new(),
            history_len: 0,
            history_total: 0,
            history: HashMap::new(),
            viewport_top: 0,
            blocks: BlockList::new(),
            collapsed: BTreeSet::new(),
        }
    }
}

impl TerminalModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// A snapshot has been applied.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn frame(&self) -> u64 {
        self.frame
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn screen_rows(&self) -> u16 {
        u16::try_from(self.rows.len()).unwrap_or(u16::MAX)
    }

    pub fn viewport(&self) -> &[Row] {
        &self.rows
    }

    /// Whether any viewport row holds non-blank text (a snapshot with a prompt on it).
    pub fn visible_has_text(&self) -> bool {
        self.ready
            && self
                .rows
                .iter()
                .any(|row| !Self::row_text(row).trim().is_empty())
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn modes(&self) -> Modes {
        self.modes
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn link(&self, id: u32) -> Option<&str> {
        self.links.get(&id).map(String::as_str)
    }

    /// Lines of scrollback the daemon retains.
    pub fn history_len(&self) -> u32 {
        self.history_len
    }

    /// Absolute number of the first viewport row.
    pub fn history_total(&self) -> u64 {
        self.history_total
    }

    /// The daemon's line number (`terminal.blocks` numbering) of viewport row 0.
    pub fn viewport_top(&self) -> u64 {
        self.viewport_top
    }

    /// The model line (see the module docs) of the daemon's absolute line `abs`. Both numberings
    /// advance together, so the offset from the viewport is all that links them; the model's
    /// own counts are never used to guess it.
    pub fn line_of(&self, abs: u64) -> i64 {
        let offset = i128::from(abs) - i128::from(self.viewport_top);
        let line = i128::from(self.history_total) + offset;
        i64::try_from(line).unwrap_or(if line < 0 { i64::MIN } else { i64::MAX })
    }

    /// The daemon's absolute line number of the model line `line`; `None` before the first line
    /// of the terminal.
    pub fn abs_of(&self, line: i64) -> Option<u64> {
        let offset = i128::from(line) - i128::from(self.history_total);
        u64::try_from(i128::from(self.viewport_top) + offset).ok()
    }

    /// The oldest model line the daemon still retains.
    pub fn oldest_line(&self) -> i64 {
        i64::try_from(
            self.history_total
                .saturating_sub(u64::from(self.history_len)),
        )
        .unwrap_or(i64::MAX)
    }

    // ---- blocks ----------------------------------------------------------------------------

    /// The blocks of this terminal, oldest first.
    pub fn blocks(&self) -> &BlockList {
        &self.blocks
    }

    /// Replaces the block list (an initial fetch, or one after a reconnect). Folds of blocks
    /// that are gone are forgotten.
    pub fn set_blocks(&mut self, blocks: BlockList) {
        self.blocks = blocks;
        let blocks = &self.blocks;
        self.collapsed.retain(|id| blocks.get(*id).is_some());
    }

    /// Folds one `terminal.block` event into the list.
    pub fn apply_block(&mut self, block: Block) {
        self.blocks.apply(block);
    }

    /// Ids of the folded blocks.
    pub fn collapsed(&self) -> &BTreeSet<u64> {
        &self.collapsed
    }

    pub fn is_collapsed(&self, id: u64) -> bool {
        self.collapsed.contains(&id)
    }

    /// Replaces the folded set (restoring what the desktop saved). Ids of blocks that do not
    /// exist fold nothing; the next [`Self::set_blocks`] forgets them.
    pub fn set_collapsed(&mut self, ids: BTreeSet<u64>) {
        self.collapsed = ids;
    }

    /// Folds or unfolds a block; returns whether the set changed (only known blocks can fold).
    pub fn set_block_collapsed(&mut self, id: u64, collapsed: bool) -> bool {
        if collapsed {
            self.blocks.get(id).is_some() && self.collapsed.insert(id)
        } else {
            self.collapsed.remove(&id)
        }
    }

    pub fn apply(&mut self, render: TerminalRender) -> Option<Applied> {
        match render {
            TerminalRender::Snapshot(s) => Some(self.apply_snapshot(s)),
            TerminalRender::Frame(f) => self.apply_frame(f),
        }
    }

    pub fn apply_snapshot(&mut self, s: TerminalSnapshot) -> Applied {
        let rows = usize::from(s.rows);
        let resized = !self.ready || s.cols != self.cols || rows != self.rows.len();
        let old_len = self.history_len;
        let was_ready = self.ready;

        let mut lines = s.lines;
        lines.resize_with(rows, Row::default);
        self.rows = lines;
        self.cols = s.cols;
        self.cursor = s.cursor;
        let modes_changed = self.modes != s.modes;
        self.modes = s.modes;
        self.palette = s.palette;
        let title_changed = self.title != s.title;
        self.title = s.title;
        self.links = s.links.into_iter().map(|l| (l.id, l.uri)).collect();
        self.frame = s.frame;
        self.ready = true;
        self.history_len = s.history_len;
        let old_top = std::mem::replace(&mut self.viewport_top, s.viewport_top);

        let mut history_grew = 0;
        let mut history_reset = false;
        if !was_ready {
            self.history_total = u64::from(s.history_len);
        } else if resized || s.history_len < old_len {
            // Reflow or a cleared history: absolute numbering restarts.
            history_reset = true;
            self.history.clear();
            self.history_total = u64::from(s.history_len);
        } else {
            history_grew = history_growth(old_len, s.history_len, old_top, s.viewport_top);
            self.history_total += u64::from(history_grew);
        }
        self.purge_history();
        Applied {
            frame: s.frame,
            snapshot: true,
            resized,
            history_grew,
            history_reset,
            title_changed,
            modes_changed,
            dirty: (0..self.screen_rows()).collect(),
            prev_viewport_top: old_top,
        }
    }

    /// Frames before the first snapshot are ignored (`None`).
    pub fn apply_frame(&mut self, f: TerminalFrame) -> Option<Applied> {
        if !self.ready {
            return None;
        }
        self.links
            .extend(f.links.into_iter().map(|l| (l.id, l.uri)));
        let len = self.rows.len();
        let mut dirty = Vec::new();
        let scrolled = usize::try_from(f.scrolled.unsigned_abs()).map_or(len, |n| n.min(len));
        if scrolled > 0 {
            if f.scrolled > 0 {
                self.rows.rotate_left(scrolled);
                self.rows[len - scrolled..].fill(Row::default());
                dirty.extend((len - scrolled..len).filter_map(|r| u16::try_from(r).ok()));
            } else {
                self.rows.rotate_right(scrolled);
                self.rows[..scrolled].fill(Row::default());
                dirty.extend((0..scrolled).filter_map(|r| u16::try_from(r).ok()));
            }
        }
        for (index, row) in f.dirty {
            if let Some(slot) = self.rows.get_mut(usize::from(index)) {
                *slot = row;
                if !dirty.contains(&index) {
                    dirty.push(index);
                }
            }
        }
        self.cursor = f.cursor;
        let mut modes_changed = false;
        if let Some(modes) = f.modes {
            modes_changed = modes != self.modes;
            self.modes = modes;
        }
        let mut title_changed = false;
        if let Some(title) = f.title {
            title_changed = title != self.title;
            self.title = title;
        }

        let old_len = self.history_len;
        self.history_len = f.history_len;
        let old_top = std::mem::replace(&mut self.viewport_top, f.viewport_top);
        let mut history_grew = 0;
        let mut history_reset = false;
        if f.history_len < old_len {
            history_reset = true;
            self.history.clear();
            self.history_total = u64::from(f.history_len);
        } else {
            history_grew = history_growth(old_len, f.history_len, old_top, f.viewport_top);
            self.history_total += u64::from(history_grew);
        }
        self.frame = f.frame;
        self.purge_history();
        dirty.sort_unstable();
        Some(Applied {
            frame: f.frame,
            snapshot: false,
            resized: false,
            history_grew,
            history_reset,
            title_changed,
            modes_changed,
            dirty,
            prev_viewport_top: old_top,
        })
    }

    /// Stores a `terminal.history` answer: `from_line` 0 is the line directly above the viewport
    /// as of now, `rows` are newest first. Lines beyond the retained history are ignored.
    pub fn apply_history(&mut self, from_line: u32, rows: Vec<Row>) {
        for (offset, row) in rows.into_iter().enumerate() {
            let Some(line) = u64::from(from_line).checked_add(offset as u64) else {
                break;
            };
            if line >= self.history_total || line >= u64::from(self.history_len) {
                break;
            }
            self.history.insert(self.history_total - 1 - line, row);
        }
    }

    /// Drops cached lines the daemon no longer retains.
    fn purge_history(&mut self) {
        let oldest = self
            .history_total
            .saturating_sub(u64::from(self.history_len));
        if self.history.len() > self.history_len as usize + 512 {
            self.history.retain(|line, _| *line >= oldest);
        }
    }

    /// The row at absolute line `line` (history or viewport), if known.
    pub fn row_at(&self, line: i64) -> Option<&Row> {
        let line = u64::try_from(line).ok()?;
        if line >= self.history_total {
            let row = usize::try_from(line - self.history_total).ok()?;
            return self.rows.get(row);
        }
        self.history.get(&line)
    }

    /// Absolute line of viewport row `row`.
    pub fn viewport_line(&self, row: u16) -> i64 {
        i64::try_from(self.history_total).unwrap_or(i64::MAX) + i64::from(row)
    }

    /// First `from_line` in `start..start + count` that exists in the daemon's history but is not
    /// cached yet.
    pub fn first_missing_history(&self, start: u32, count: u32) -> Option<u32> {
        let end = start.saturating_add(count).min(self.history_len);
        (start..end).find(|from_line| {
            let line = self.history_total - 1 - u64::from(*from_line);
            !self.history.contains_key(&line)
        })
    }

    /// Whether the line `from_line` above the viewport is cached.
    pub fn has_history(&self, from_line: u32) -> bool {
        u64::from(from_line) < self.history_total
            && self
                .history
                .contains_key(&(self.history_total - 1 - u64::from(from_line)))
    }

    /// Plain text of one row (cells joined, trailing blanks trimmed).
    pub fn row_text(row: &Row) -> String {
        row_cells(row)
            .iter()
            .map(|c| c.text)
            .collect::<String>()
            .trim_end_matches(' ')
            .to_owned()
    }
}

/// Lines that scrolled into the history between two updates: the history length's growth, or —
/// once the history is at its bound and its length stands still — the advance of the daemon's
/// viewport number.
fn history_growth(old_len: u32, new_len: u32, old_top: u64, new_top: u64) -> u32 {
    let by_top = u32::try_from(new_top.saturating_sub(old_top)).unwrap_or(u32::MAX);
    (new_len - old_len).max(by_top)
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use hutan_protocol::render::Color;

    pub fn run(text: &str) -> Run {
        Run {
            text: text.to_owned(),
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_color: None,
            link: None,
        }
    }

    pub fn wide_run(text: &str) -> Run {
        Run {
            flags: CellFlags::WIDE,
            ..run(text)
        }
    }

    pub fn row(text: &str) -> Row {
        Row {
            runs: if text.is_empty() {
                vec![]
            } else {
                vec![run(text)]
            },
            wrapped: false,
        }
    }

    pub fn cursor() -> Cursor {
        Cursor {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            blinking: false,
        }
    }

    pub fn snapshot(frame: u64, cols: u16, lines: &[&str], history_len: u32) -> TerminalSnapshot {
        TerminalSnapshot {
            frame,
            cols,
            rows: lines.len() as u16,
            cursor: cursor(),
            modes: Modes::default(),
            palette: Palette::default(),
            title: None,
            lines: lines.iter().map(|l| row(l)).collect(),
            links: vec![],
            history_len,
            viewport_top: 0,
        }
    }

    pub fn frame(frame: u64, dirty: &[(u16, &str)], history_len: u32) -> TerminalFrame {
        TerminalFrame {
            frame,
            scrolled: 0,
            dirty: dirty.iter().map(|(i, t)| (*i, row(t))).collect(),
            cursor: cursor(),
            modes: None,
            title: None,
            links: vec![],
            history_len,
            viewport_top: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use hutan_protocol::render::Link;

    fn text(model: &TerminalModel, row: usize) -> String {
        TerminalModel::row_text(&model.viewport()[row])
    }

    #[test]
    fn daemon_lines_map_onto_model_lines_through_the_viewport_top() {
        let mut m = TerminalModel::new();
        // 30 lines of history, the daemon says the first viewport row is its line 500.
        let mut s = snapshot(1, 10, &["a", "b"], 30);
        s.viewport_top = 500;
        m.apply_snapshot(s);
        assert_eq!(m.viewport_top(), 500);
        assert_eq!(
            m.line_of(500),
            30,
            "row 0 is the first model line of the viewport"
        );
        assert_eq!(m.line_of(499), 29);
        assert_eq!(m.line_of(470), 0, "the oldest retained line");
        assert_eq!(m.oldest_line(), 0);
        assert_eq!(m.abs_of(31), Some(501));
        assert_eq!(m.abs_of(0), Some(470));
        // The history is at its bound: lines scroll off and the history length stands still, but
        // the model numbering advances with the daemon's, so a line keeps its model number (and a
        // cached history row keyed by it keeps meaning the same text).
        let mut f = frame(2, &[], 30);
        f.viewport_top = 507;
        let applied = m.apply_frame(f).unwrap();
        assert_eq!(applied.history_grew, 7);
        assert_eq!(m.line_of(500), 30, "line 500 kept its model number");
        assert_eq!(m.line_of(507), 37);
        assert_eq!(m.oldest_line(), 7, "seven lines fell off the bound");
        // A terminal with fewer lines than the number says maps below zero.
        assert_eq!(m.line_of(0), 37 - 507);
        assert_eq!(m.abs_of(-1000), None);
    }

    #[test]
    fn collapsed_blocks_are_pruned_by_a_new_list_only() {
        use hutan_protocol::blocks::{Block, BlockState, TerminalBlocksResult};
        let block = |id| Block {
            id,
            prompt_line: id,
            command_line: None,
            output_start: None,
            end_line: None,
            command: None,
            exit_code: None,
            started_at_ms: 0,
            finished_at_ms: None,
            cwd: None,
            state: BlockState::Prompt,
        };
        let list = |ids: &[u64]| {
            let mut list = BlockList::new();
            list.replace(TerminalBlocksResult {
                blocks: ids.iter().rev().map(|id| block(*id)).collect(),
                viewport_top: 0,
            });
            list
        };
        let mut m = TerminalModel::new();
        // Restored before the first fetch: kept.
        m.set_collapsed([1, 2, 9].into());
        assert_eq!(m.collapsed().len(), 3);
        // Events do not prune; a fetch does.
        m.apply_block(block(1));
        assert_eq!(m.collapsed().len(), 3);
        m.set_blocks(list(&[1, 2, 3]));
        assert_eq!(m.collapsed().iter().copied().collect::<Vec<_>>(), [1, 2]);
        assert!(!m.set_block_collapsed(7, true), "unknown block");
        assert!(m.set_block_collapsed(3, true));
        assert!(!m.set_block_collapsed(3, true), "already folded");
        assert!(m.set_block_collapsed(3, false));
    }

    #[test]
    fn snapshot_replaces_everything() {
        let mut m = TerminalModel::new();
        assert!(!m.is_ready());
        let mut s = snapshot(1, 10, &["one", "two", ""], 5);
        s.title = Some("t".into());
        s.links = vec![Link {
            id: 0,
            uri: "https://a".into(),
        }];
        let applied = m.apply_snapshot(s);
        assert!(applied.snapshot && applied.resized);
        assert_eq!(applied.dirty, vec![0, 1, 2]);
        assert_eq!(m.cols(), 10);
        assert_eq!(m.screen_rows(), 3);
        assert_eq!(text(&m, 1), "two");
        assert_eq!(m.title(), Some("t"));
        assert_eq!(m.link(0), Some("https://a"));
        assert_eq!(m.history_len(), 5);
        assert_eq!(m.history_total(), 5);

        let applied = m.apply_snapshot(snapshot(7, 10, &["x", "y", "z"], 5));
        assert!(!applied.resized && !applied.history_reset);
        assert_eq!(m.frame(), 7);
        assert_eq!(m.title(), None);
        assert_eq!(m.link(0), None);
    }

    #[test]
    fn snapshot_pads_missing_lines() {
        let mut m = TerminalModel::new();
        let mut s = snapshot(1, 4, &["a", "b"], 0);
        s.rows = 4;
        m.apply_snapshot(s);
        assert_eq!(m.viewport().len(), 4);
        assert_eq!(text(&m, 3), "");
    }

    #[test]
    fn frames_before_a_snapshot_are_ignored() {
        let mut m = TerminalModel::new();
        assert!(m.apply_frame(frame(1, &[(0, "x")], 0)).is_none());
        assert!(!m.is_ready());
    }

    #[test]
    fn frame_updates_dirty_rows_cursor_modes_title_links() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["a", "b", "c"], 0));
        let mut f = frame(2, &[(2, "new")], 0);
        f.cursor.col = 3;
        f.modes = Some(Modes {
            bracketed_paste: true,
            ..Modes::default()
        });
        f.title = Some(Some("hello".into()));
        f.links = vec![Link {
            id: 4,
            uri: "https://b".into(),
        }];
        let applied = m.apply_frame(f).unwrap();
        assert_eq!(applied.dirty, vec![2]);
        assert!(applied.modes_changed && applied.title_changed);
        assert_eq!(text(&m, 0), "a");
        assert_eq!(text(&m, 2), "new");
        assert_eq!(m.cursor().col, 3);
        assert!(m.modes().bracketed_paste);
        assert_eq!(m.title(), Some("hello"));
        assert_eq!(m.link(4), Some("https://b"));
        assert_eq!(m.frame(), 2);
    }

    #[test]
    fn frame_without_modes_or_title_keeps_them() {
        let mut m = TerminalModel::new();
        let mut s = snapshot(1, 10, &["a"], 0);
        s.modes.app_cursor = true;
        s.title = Some("keep".into());
        m.apply_snapshot(s);
        let applied = m.apply_frame(frame(2, &[], 0)).unwrap();
        assert!(!applied.modes_changed && !applied.title_changed);
        assert!(m.modes().app_cursor);
        assert_eq!(m.title(), Some("keep"));
    }

    #[test]
    fn title_some_none_clears_it() {
        let mut m = TerminalModel::new();
        let mut s = snapshot(1, 10, &["a"], 0);
        s.title = Some("t".into());
        m.apply_snapshot(s);
        let mut f = frame(2, &[], 0);
        f.title = Some(None);
        let applied = m.apply_frame(f).unwrap();
        assert!(applied.title_changed);
        assert_eq!(m.title(), None);
    }

    #[test]
    fn scrolled_frames_shift_rows_before_dirty_applies() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["a", "b", "c", "d"], 0));
        let mut f = frame(2, &[(3, "e")], 1);
        f.scrolled = 1;
        let applied = m.apply_frame(f).unwrap();
        assert_eq!(applied.dirty, vec![3]);
        assert_eq!(
            (0..4).map(|i| text(&m, i)).collect::<Vec<_>>(),
            ["b", "c", "d", "e"]
        );

        let mut f = frame(3, &[(0, "z")], 1);
        f.scrolled = -2;
        m.apply_frame(f).unwrap();
        assert_eq!(
            (0..4).map(|i| text(&m, i)).collect::<Vec<_>>(),
            ["z", "", "b", "c"]
        );
    }

    #[test]
    fn scroll_larger_than_the_screen_clears_it() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["a", "b"], 0));
        let mut f = frame(2, &[], 0);
        f.scrolled = 99;
        m.apply_frame(f).unwrap();
        assert_eq!(text(&m, 0), "");
        assert_eq!(text(&m, 1), "");
    }

    #[test]
    fn out_of_range_dirty_rows_are_ignored() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["a"], 0));
        let applied = m.apply_frame(frame(2, &[(5, "x")], 0)).unwrap();
        assert!(applied.dirty.is_empty());
    }

    #[test]
    fn history_merges_by_absolute_line_and_survives_growth() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["v0", "v1"], 4));
        // Lines newest first: from_line 0 is the one above the viewport.
        m.apply_history(0, vec![row("h3"), row("h2")]);
        assert_eq!(
            m.row_at(3).map(TerminalModel::row_text).as_deref(),
            Some("h3")
        );
        assert_eq!(
            m.row_at(2).map(TerminalModel::row_text).as_deref(),
            Some("h2")
        );
        assert!(m.row_at(1).is_none());
        assert!(m.has_history(0) && m.has_history(1) && !m.has_history(2));

        // Two lines scroll into history: the cached lines keep their absolute numbers.
        let applied = m.apply_frame(frame(2, &[], 6)).unwrap();
        assert_eq!(applied.history_grew, 2);
        assert_eq!(m.history_total(), 6);
        assert_eq!(
            m.row_at(3).map(TerminalModel::row_text).as_deref(),
            Some("h3")
        );
        assert!(!m.has_history(0) && !m.has_history(1));
        assert!(m.has_history(2) && m.has_history(3));
        assert_eq!(m.first_missing_history(0, 200), Some(0));
        assert_eq!(m.first_missing_history(2, 2), None);

        // The second page fills in the older lines.
        m.apply_history(2, vec![row("h3"), row("h2"), row("h1"), row("h0")]);
        assert_eq!(m.first_missing_history(0, 200), Some(0));
        m.apply_history(0, vec![row("h5"), row("h4")]);
        assert_eq!(m.first_missing_history(0, 200), None);
        let top: Vec<_> = (0..6)
            .map(|l| TerminalModel::row_text(m.row_at(l).unwrap()))
            .collect();
        assert_eq!(top, ["h0", "h1", "h2", "h3", "h4", "h5"]);
    }

    #[test]
    fn history_rows_past_the_retained_length_are_dropped() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["v"], 2));
        m.apply_history(0, vec![row("a"), row("b"), row("c")]);
        assert!(m.has_history(1));
        assert!(!m.has_history(2));
    }

    #[test]
    fn viewport_rows_follow_history_in_absolute_numbering() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["v0", "v1"], 3));
        assert_eq!(m.viewport_line(0), 3);
        assert_eq!(
            m.row_at(4).map(TerminalModel::row_text).as_deref(),
            Some("v1")
        );
        assert!(m.row_at(5).is_none());
        assert!(m.row_at(-1).is_none());
    }

    #[test]
    fn shrinking_history_or_resizing_resets_the_cache() {
        let mut m = TerminalModel::new();
        m.apply_snapshot(snapshot(1, 10, &["v"], 3));
        m.apply_history(0, vec![row("a")]);
        let applied = m.apply_frame(frame(2, &[], 0)).unwrap();
        assert!(applied.history_reset);
        assert_eq!(m.history_total(), 0);
        assert!(!m.has_history(0));

        m.apply_snapshot(snapshot(3, 10, &["v"], 3));
        m.apply_history(0, vec![row("a")]);
        let applied = m.apply_snapshot(snapshot(4, 20, &["v"], 3));
        assert!(applied.resized && applied.history_reset);
        assert!(!m.has_history(0));
    }

    #[test]
    fn cells_split_wide_and_combining_characters() {
        let row = Row {
            runs: vec![run("ab"), wide_run("日本"), run("e\u{301}x")],
            wrapped: false,
        };
        let cells = row_cells(&row);
        let shape: Vec<_> = cells.iter().map(|c| (c.col, c.width, c.text)).collect();
        assert_eq!(
            shape,
            [
                (0, 1, "a"),
                (1, 1, "b"),
                (2, 2, "日"),
                (4, 2, "本"),
                (6, 1, "e\u{301}"),
                (7, 1, "x"),
            ]
        );
        assert_eq!(row_width(&row), 8);
        assert_eq!(TerminalModel::row_text(&row), "ab日本e\u{301}x");
    }
}
