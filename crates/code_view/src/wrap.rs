//! Soft wrap (EDITOR-WRAP): the display map between buffer lines and display rows.
//!
//! A buffer line is drawn as one or more *rows* of at most `cols` cells. Columns stay the
//! line's own (a tab runs to the next stop of the *line*, not of its row), so a row is a window
//! `start_col..end_col` of the line and everything in [`layout`](crate::layout) works on it with
//! a base column. Rows after the first are drawn indented by the line's leading whitespace (and
//! [`WrapParams::extra_indent`]); a line whose indentation takes more than half of the width
//! wraps flush left.
//!
//! Where a line breaks ([`wrap_line`]): after a space or tab, after `, ; ) ] } ? !` and after
//! `- / \ |` that follow a letter or digit, and after any wide character (CJK breaks anywhere,
//! except before closing punctuation); trailing whitespace hangs at the end of its row instead of
//! starting one; a token that fits no row is cut at the edge. A row always holds at least one
//! character.
//!
//! [`WrapMap`] keeps only the number of rows of every line (and the prefix sums), a few bytes
//! per line; the breaks of the lines in view are computed from the text when asked and cached
//! (the last 256 lines). An edit re-wraps the lines it touched ([`WrapMap::apply`]); a new width
//! re-wraps every line. Above [`MAX_WRAP_BYTES`] a buffer does not wrap.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use hutan_buffer::{Text, Transaction};

use crate::layout::{
    TAB_SIZE, byte_at_col_from, byte_of_col_at, cells, col_of_byte_at, line_columns_at,
};

/// Narrowest row, in cells: a view narrower than this wraps as if it had this much room.
pub const MIN_WRAP_COLS: usize = 8;

/// Texts above this many bytes are not wrapped (the map would cost a frame at open).
pub const MAX_WRAP_BYTES: usize = 4 * 1024 * 1024;

/// `editor.wrap_column` by default.
pub const DEFAULT_WRAP_COLUMN: usize = 100;

/// Breaks of this many lines are kept between frames.
const CACHE_LINES: usize = 256;

/// Edits kept for the map between two frames; past it the map is built again.
const MAX_PENDING: usize = 256;

/// How a view wraps its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// One row per line; long lines scroll sideways.
    #[default]
    Off,
    /// Rows are as wide as the view.
    Viewport,
    /// Rows are at most this many cells wide, or as wide as the view when that is narrower.
    Column(usize),
}

impl WrapMode {
    /// The row width for a view `viewport` cells wide; `None` when the view does not wrap.
    pub fn cols(self, viewport: usize) -> Option<usize> {
        match self {
            WrapMode::Off => None,
            WrapMode::Viewport => Some(viewport.max(MIN_WRAP_COLS)),
            WrapMode::Column(n) => Some(n.max(MIN_WRAP_COLS).min(viewport.max(MIN_WRAP_COLS))),
        }
    }
}

/// What a line wraps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapParams {
    /// Cells of a row.
    pub cols: usize,
    /// Cells added to the indent of the rows after the first.
    pub extra_indent: usize,
}

/// Where a row after the first starts: a byte of the line's content and the column of the line
/// there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Break {
    pub byte: usize,
    pub col: usize,
}

/// The columns of the leading spaces and tabs of `line`.
pub fn indent_cols(line: &str) -> usize {
    let mut col = 0;
    for c in line.chars() {
        if c != ' ' && c != '\t' {
            break;
        }
        col += cells(c, col);
    }
    col
}

/// The cells rows after the first of `line` are indented by.
fn continuation_indent(line: &str, cols: usize, extra: usize) -> usize {
    let indent = indent_cols(line) + extra;
    if indent * 2 > cols { 0 } else { indent }
}

/// Whether a row may break right after `c` (`prev` is the character before it).
fn breaks_after(c: char, prev: Option<char>) -> bool {
    match c {
        ' ' | '\t' | ',' | ';' | ')' | ']' | '}' | '?' | '!' => true,
        '-' | '/' | '\\' | '|' => prev.is_some_and(char::is_alphanumeric),
        c => cells(c, 0) == 2,
    }
}

/// Characters that never start a row (they stay with what comes before).
fn no_break_before(c: char) -> bool {
    matches!(
        c,
        ')' | ']'
            | '}'
            | ','
            | '.'
            | ';'
            | ':'
            | '!'
            | '?'
            | '，'
            | '。'
            | '、'
            | '．'
            | '）'
            | '」'
            | '』'
            | '】'
            | '〉'
            | '》'
            | '！'
            | '？'
            | '；'
            | '：'
            | '…'
    )
}

/// The breaks of `line` (the content of one buffer line): where each row after the first
/// starts. Empty when the line fits one row.
pub fn wrap_line(line: &str, params: WrapParams) -> Vec<Break> {
    let cols = params.cols.max(MIN_WRAP_COLS);
    let mut out = Vec::new();
    // Every byte is at most `TAB_SIZE` cells (a tab), so such a line fits.
    if line.len().saturating_mul(TAB_SIZE) <= cols {
        return out;
    }
    let continuation = cols - continuation_indent(line, cols, params.extra_indent);
    let (mut row_start, mut row_col, mut cap) = (0usize, 0usize, cols);
    let (mut at, mut col) = (0usize, 0usize);
    let mut seen_text = false;
    let mut prev: Option<char> = None;
    // The break after the character just read, and the latest one that is settled (the
    // character after it was read too).
    let mut pending: Option<(usize, usize)> = None;
    let mut last: Option<(usize, usize)> = None;
    while let Some(c) = line[at..].chars().next() {
        let width = cells(c, col);
        let len = c.len_utf8();
        if width > 0 {
            if let Some(opportunity) = pending.take()
                && !no_break_before(c)
            {
                last = Some(opportunity);
            }
        } else if let Some(opportunity) = pending.as_mut() {
            // A mark rides on its base: the break goes after both.
            *opportunity = (at + len, col);
        }
        let space = c == ' ' || c == '\t';
        if width > 0 && !space && col + width - row_col > cap && at > row_start {
            let (byte, break_col) = last.filter(|(b, _)| *b > row_start).unwrap_or((at, col));
            out.push(Break {
                byte,
                col: break_col,
            });
            row_start = byte;
            row_col = break_col;
            cap = continuation;
            at = byte;
            col = break_col;
            seen_text = false;
            prev = line[..byte].chars().next_back();
            pending = None;
            last = None;
            continue;
        }
        col += width;
        at += len;
        if width > 0 {
            if !space {
                seen_text = true;
            }
            if seen_text && breaks_after(c, prev) {
                pending = Some((at, col));
            }
            prev = Some(c);
        }
    }
    out
}

/// One display row of a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowGeom {
    pub line: usize,
    /// Which row of the line (0 = the first).
    pub sub: usize,
    /// The bytes of the row, in the buffer: a row ends where the next starts; the last one ends
    /// at the end of the line's content.
    pub bytes: Range<usize>,
    /// The column of the line the row starts at.
    pub start_col: usize,
    /// Cells the row is drawn indented by.
    pub indent: usize,
    pub first: bool,
    pub last: bool,
    /// The column the row ends at, known for every row but the last.
    end_col: Option<usize>,
}

impl RowGeom {
    /// `line` drawn as one row (a view that does not wrap).
    pub fn whole_line(text: &Text, line: usize) -> Self {
        Self {
            line,
            sub: 0,
            bytes: text.line_content_range(line).unwrap_or(0..0),
            start_col: 0,
            indent: 0,
            first: true,
            last: true,
            end_col: None,
        }
    }

    /// The column of the line the row ends at.
    pub fn end_col(&self, text: &Text) -> usize {
        self.end_col.unwrap_or_else(|| {
            let row = text.slice(self.bytes.clone()).unwrap_or_default();
            line_columns_at(&row, self.start_col)
        })
    }

    /// The row's text.
    pub fn text(&self, text: &Text) -> String {
        text.slice(self.bytes.clone()).unwrap_or_default()
    }

    /// The column of the line where byte `at` (inside the row, or its end) is.
    pub fn col_of(&self, text: &Text, at: usize) -> usize {
        let row = self.text(text);
        col_of_byte_at(&row, self.start_col, at.saturating_sub(self.bytes.start))
    }

    /// The cell, counted from the left of the text area, where column `col` of the line is
    /// drawn in this row.
    pub fn x_of_col(&self, col: usize) -> f32 {
        (col.saturating_sub(self.start_col) + self.indent) as f32
    }
}

/// The rows of every line of a text for one [`WrapParams`].
pub struct WrapMap {
    params: WrapParams,
    /// Rows of every line (at least one).
    rows: Vec<u32>,
    /// The first display row of every line, then the total.
    starts: Vec<u32>,
    /// The version of the buffer the map is of, and its text then (to find the lines an edit
    /// touched).
    version: u64,
    text: Text,
    cache: RefCell<HashMap<usize, Rc<[Break]>>>,
    /// Lines wrapped by a scan since the map was built (tests, and what an edit costs).
    rewrapped: u64,
}

impl std::fmt::Debug for WrapMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrapMap")
            .field("params", &self.params)
            .field("lines", &self.rows.len())
            .field("rows", &self.total_rows())
            .finish_non_exhaustive()
    }
}

fn count_rows(text: &Text, params: WrapParams, line: usize, scanned: &mut u64) -> u32 {
    let Ok(content) = text.line_content_range(line) else {
        return 1;
    };
    if content.len().saturating_mul(TAB_SIZE) <= params.cols.max(MIN_WRAP_COLS) {
        return 1;
    }
    *scanned += 1;
    let line_text = text.slice(content).unwrap_or_default();
    u32::try_from(wrap_line(&line_text, params).len() + 1).unwrap_or(u32::MAX)
}

impl WrapMap {
    /// The map of `text` (the buffer's `version`): every line is wrapped.
    pub fn new(text: &Text, version: u64, params: WrapParams) -> Self {
        let mut scanned = 0;
        let rows: Vec<u32> = (0..text.line_count())
            .map(|line| count_rows(text, params, line, &mut scanned))
            .collect();
        let mut map = Self {
            params,
            starts: Vec::new(),
            rows,
            version,
            text: text.clone(),
            cache: RefCell::new(HashMap::new()),
            rewrapped: scanned,
        };
        map.recount(0);
        map
    }

    /// Fills `starts` from line `from` on.
    fn recount(&mut self, from: usize) {
        self.starts.truncate(from + 1);
        if self.starts.is_empty() {
            self.starts.push(0);
        }
        let mut total = self.starts[from.min(self.starts.len() - 1)];
        for rows in &self.rows[from.min(self.rows.len())..] {
            total = total.saturating_add(*rows);
            self.starts.push(total);
        }
    }

    pub fn params(&self) -> WrapParams {
        self.params
    }

    /// The buffer version the map is of.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// How many lines were wrapped by a scan since the map was built.
    pub fn rewrapped(&self) -> u64 {
        self.rewrapped
    }

    pub fn line_count(&self) -> usize {
        self.rows.len()
    }

    /// Display rows of the whole text.
    pub fn total_rows(&self) -> usize {
        self.starts.last().copied().unwrap_or(0) as usize
    }

    /// Rows of `line`.
    pub fn rows_in_line(&self, line: usize) -> usize {
        self.rows.get(line).map_or(1, |r| *r as usize)
    }

    /// The display row `line` starts at (the total for a line past the end).
    pub fn first_row_of_line(&self, line: usize) -> usize {
        self.starts
            .get(line)
            .or(self.starts.last())
            .copied()
            .unwrap_or(0) as usize
    }

    /// The line and row of the line that display row `row` is (the last row for a row past the
    /// end).
    pub fn locate_row(&self, row: usize) -> (usize, usize) {
        let last_line = self.rows.len().saturating_sub(1);
        let row = u32::try_from(row).unwrap_or(u32::MAX);
        let line = self
            .starts
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
            .min(last_line);
        let sub = (row.saturating_sub(self.starts[line]) as usize).min(self.rows_in_line(line) - 1);
        (line, sub)
    }

    /// Where the rows after the first of `line` start (cached).
    pub fn breaks(&self, text: &Text, line: usize) -> Rc<[Break]> {
        if self.rows_in_line(line) <= 1 {
            return Rc::from(Vec::new());
        }
        if let Some(hit) = self.cache.borrow().get(&line) {
            return hit.clone();
        }
        let content = text.line_content_range(line).unwrap_or(0..0);
        let line_text = text.slice(content).unwrap_or_default();
        let breaks: Rc<[Break]> = wrap_line(&line_text, self.params).into();
        let mut cache = self.cache.borrow_mut();
        if cache.len() >= CACHE_LINES {
            cache.clear();
        }
        cache.insert(line, breaks.clone());
        breaks
    }

    /// Row `sub` of `line`.
    pub fn row(&self, text: &Text, line: usize, sub: usize) -> RowGeom {
        let content = text.line_content_range(line).unwrap_or(0..0);
        let breaks = self.breaks(text, line);
        let sub = sub.min(breaks.len());
        let (start, start_col) = match sub.checked_sub(1) {
            None => (content.start, 0),
            Some(i) => (content.start + breaks[i].byte, breaks[i].col),
        };
        let (end, end_col) = match breaks.get(sub) {
            Some(b) => (content.start + b.byte, Some(b.col)),
            None => (content.end, None),
        };
        let indent = if sub == 0 {
            0
        } else {
            let line_text = text.slice(content.clone()).unwrap_or_default();
            continuation_indent(
                &line_text,
                self.params.cols.max(MIN_WRAP_COLS),
                self.params.extra_indent,
            )
        };
        RowGeom {
            line,
            sub,
            bytes: start..end,
            start_col,
            indent,
            first: sub == 0,
            last: sub == breaks.len(),
            end_col,
        }
    }

    /// The row with display index `row`.
    pub fn row_at(&self, text: &Text, row: usize) -> RowGeom {
        let (line, sub) = self.locate_row(row);
        self.row(text, line, sub)
    }

    /// The line and row of the line that byte `at` is drawn in. A byte where a row starts is in
    /// that row, unless `upstream`: the caret that stands at the end of the row before.
    pub fn locate_byte(&self, text: &Text, at: usize, upstream: bool) -> (usize, usize) {
        let at = at.min(text.len());
        let line = text.line_of_byte(at).unwrap_or(0);
        let start = text.line_start(line).unwrap_or(0);
        let rel = at - start;
        let breaks = self.breaks(text, line);
        let mut sub = breaks.partition_point(|b| b.byte <= rel);
        if upstream && sub > 0 && breaks[sub - 1].byte == rel {
            sub -= 1;
        }
        (line, sub)
    }

    /// The display row byte `at` is drawn in (see [`locate_byte`](Self::locate_byte)).
    pub fn row_of_byte(&self, text: &Text, at: usize, upstream: bool) -> usize {
        let (line, sub) = self.locate_byte(text, at, upstream);
        self.first_row_of_line(line) + sub
    }

    /// The byte on display row `row` that a pointer at cell `x` (counted from the left of the
    /// text area, in cells) is nearest to, and whether it is the end of a row that is not the
    /// line's last (where the caret draws before the next row's first character).
    pub fn byte_at_x(&self, text: &Text, row: usize, x: f32) -> (usize, bool) {
        let geom = self.row_at(text, row);
        let row_text = geom.text(text);
        let col = (x - geom.indent as f32).max(0.0) + geom.start_col as f32;
        let at = byte_of_col_at(&row_text, geom.start_col, col);
        self.in_row(&geom, &row_text, at)
    }

    /// The byte on display row `row` where a caret that wants cell `goal` goes: the last
    /// character boundary at or before it (the end of the row when it is shorter).
    pub fn byte_at_goal(&self, text: &Text, row: usize, goal: usize) -> (usize, bool) {
        let geom = self.row_at(text, row);
        let row_text = geom.text(text);
        let col = goal.saturating_sub(geom.indent) + geom.start_col;
        let at = byte_at_col_from(&row_text, geom.start_col, col);
        self.in_row(&geom, &row_text, at)
    }

    /// `at` bytes into the row: its byte, and whether it is past the end of a row that another
    /// follows (then the end of the row, shown there).
    fn in_row(&self, geom: &RowGeom, row_text: &str, at: usize) -> (usize, bool) {
        if !geom.last && at >= row_text.len() {
            (geom.bytes.end, true)
        } else {
            (geom.bytes.start + at, false)
        }
    }

    /// The cell where byte `at` is drawn (counted from the left of the text area): its display
    /// row and its x.
    pub fn x_of_byte(&self, text: &Text, at: usize, upstream: bool) -> (usize, f32) {
        let (line, sub) = self.locate_byte(text, at, upstream);
        let geom = self.row(text, line, sub);
        let col = geom.col_of(text, at.min(geom.bytes.end));
        (self.first_row_of_line(line) + sub, geom.x_of_col(col))
    }

    /// Takes the edit `transaction` (made to the text the map is of; the text now is `text`,
    /// buffer version `version`): only the lines it touched are wrapped again. `false` when it
    /// does not fit the map (it must be built again).
    pub fn apply(&mut self, text: &Text, version: u64, transaction: &Transaction) -> bool {
        // The old and new lines of every change, merged where they share a line.
        let mut spans: Vec<(usize, usize, usize, usize)> = Vec::new();
        for change in transaction.changes() {
            let (Ok(lo), Ok(hi), Ok(new_lo), Ok(new_hi)) = (
                self.text.line_of_byte(change.old.start),
                self.text.line_of_byte(change.old.end),
                text.line_of_byte(change.new.start),
                text.line_of_byte(change.new.end),
            ) else {
                return false;
            };
            match spans.last_mut() {
                Some(last) if lo <= last.1 => {
                    last.1 = last.1.max(hi);
                    last.3 = last.3.max(new_hi);
                }
                _ => spans.push((lo, hi, new_lo, new_hi)),
            }
        }
        let params = self.params;
        let mut scanned = 0;
        let mut first_changed = usize::MAX;
        for (lo, hi, new_lo, new_hi) in spans.into_iter().rev() {
            if hi >= self.rows.len() || new_lo > new_hi {
                return false;
            }
            let fresh: Vec<u32> = (new_lo..=new_hi)
                .map(|line| count_rows(text, params, line, &mut scanned))
                .collect();
            self.rows.splice(lo..=hi, fresh);
            first_changed = lo;
        }
        if self.rows.len() != text.line_count() {
            return false;
        }
        self.rewrapped += scanned;
        self.version = version;
        self.text = text.clone();
        self.cache.borrow_mut().clear();
        if first_changed != usize::MAX {
            self.recount(first_changed);
        }
        true
    }
}

/// What a view keeps of its soft wrap: the mode it was asked for, the map of the last frame's
/// width, the edits since the map was made, and where a caret that sits at the end of a row is
/// drawn.
#[derive(Debug, Default)]
pub(crate) struct WrapState {
    pub mode: WrapMode,
    /// Cells the text area of the last frame holds (`None` before the first frame).
    pub viewport_cols: Option<usize>,
    pub map: Option<WrapMap>,
    pending: Vec<(u64, Transaction)>,
    /// The caret at this byte is drawn at the end of the row before (`End` on a wrapped row, a
    /// click past its end); the byte itself is also where the next row starts.
    pub upstream: Option<usize>,
}

impl WrapState {
    /// The params a text of `len` bytes wraps with now.
    fn params(&self, len: usize) -> Option<WrapParams> {
        if len > MAX_WRAP_BYTES {
            return None;
        }
        let cols = self.mode.cols(self.viewport_cols?)?;
        Some(WrapParams {
            cols,
            extra_indent: 0,
        })
    }

    /// An edit of the buffer (now at `version`), kept until the next [`sync`](Self::sync).
    pub fn note_edit(&mut self, version: u64, transaction: &Transaction) {
        if self.map.is_none() {
            return;
        }
        if self.pending.len() >= MAX_PENDING {
            self.map = None;
            self.pending.clear();
            return;
        }
        self.pending.push((version, transaction.clone()));
    }

    /// Brings the map to `text` (the buffer at `version`) and the current width: edits are
    /// carried in, a new width or a text that was replaced builds it again. No map when the
    /// view does not wrap.
    pub fn sync(&mut self, text: &Text, version: u64) {
        let Some(params) = self.params(text.len()) else {
            self.map = None;
            self.pending.clear();
            return;
        };
        if let Some(map) = &self.map
            && map.params == params
            && (map.version == version || self.advance(text, version))
        {
            self.pending.clear();
            return;
        }
        self.map = Some(WrapMap::new(text, version, params));
        self.pending.clear();
    }

    /// Carries the pending edits into the map. `false` when they are not the versions between
    /// the map's and `version`.
    fn advance(&mut self, text: &Text, version: u64) -> bool {
        let Some(map) = self.map.as_mut() else {
            return false;
        };
        let mut next = map.version;
        let mut composed: Option<Transaction> = None;
        for (at, transaction) in &self.pending {
            if *at <= map.version {
                continue;
            }
            if *at != next + 1 {
                return false;
            }
            next = *at;
            composed = match composed {
                None => Some(transaction.clone()),
                Some(before) => match before.compose(transaction) {
                    Ok(both) => Some(both),
                    Err(_) => return false,
                },
            };
        }
        match composed {
            Some(transaction) if next == version => map.apply(text, version, &transaction),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(cols: usize) -> WrapParams {
        WrapParams {
            cols,
            extra_indent: 0,
        }
    }

    /// The rows of `line` as strings.
    fn rows_of(line: &str, p: WrapParams) -> Vec<String> {
        let mut at = 0;
        let mut out = Vec::new();
        for b in wrap_line(line, p) {
            out.push(line[at..b.byte].to_owned());
            at = b.byte;
        }
        out.push(line[at..].to_owned());
        out
    }

    #[test]
    fn a_line_that_fits_is_one_row() {
        assert!(wrap_line("short", params(20)).is_empty());
        assert!(wrap_line("", params(20)).is_empty());
        assert!(wrap_line("exactly eight", params(13)).is_empty());
    }

    #[test]
    fn rows_break_after_whitespace_and_keep_the_space_at_the_end() {
        assert_eq!(
            rows_of("aaa bbb ccc ddd", params(8)),
            vec!["aaa bbb ", "ccc ddd"]
        );
        // Trailing whitespace hangs: the space after "aaaaaaaa" stays on its row.
        assert_eq!(
            rows_of("aaaaaaaa bbbb", params(8)),
            vec!["aaaaaaaa ", "bbbb"]
        );
    }

    #[test]
    fn a_token_longer_than_a_row_is_cut_at_the_edge() {
        let line = "x".repeat(20);
        let breaks = wrap_line(&line, params(8));
        assert_eq!(
            breaks,
            vec![Break { byte: 8, col: 8 }, Break { byte: 16, col: 16 }]
        );
        // A long token after a word: the word's row ends at the space, the token is cut.
        assert_eq!(
            rows_of("ab cdefghijklmnopq", params(8)),
            vec!["ab ", "cdefghij", "klmnopq"]
        );
    }

    #[test]
    fn punctuation_and_slashes_are_places_to_break() {
        assert_eq!(
            rows_of("src/main/java/Hello.java", params(10)),
            vec!["src/main/", "java/", "Hello.java"]
        );
        assert_eq!(
            rows_of("ab --flag --other", params(8)),
            vec!["ab ", "--flag ", "--other"]
        );
    }

    #[test]
    fn wide_characters_count_two_cells_and_break_anywhere() {
        // Six wide characters are twelve cells; rows of eight cells hold four.
        let rows = rows_of("本本本本本本", params(8));
        assert_eq!(rows, vec!["本本本本", "本本"]);
        let breaks = wrap_line("本本本本本本", params(8));
        assert_eq!(breaks, vec![Break { byte: 12, col: 8 }]);
        // Closing punctuation never starts a row.
        assert_eq!(rows_of("本本本本。本", params(8)), vec!["本本本", "本。本"]);
    }

    #[test]
    fn a_wide_character_does_not_straddle_the_edge() {
        // Seven cells of ASCII, then a wide character: it needs two, so it starts the next row.
        assert_eq!(rows_of("abcdefg本本", params(8)), vec!["abcdefg", "本本"]);
    }

    #[test]
    fn tabs_are_counted_to_the_next_stop_of_the_line() {
        // "a" + tab (3 cells) + "bcd" = 7 cells; "e" is the 8th, then "f" overflows.
        let line = "a\tbcde fghi";
        let breaks = wrap_line(line, params(8));
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks[0], Break { byte: 7, col: 9 });
        // Every column is the line's own: the row's start column matches a scan of the line.
        for b in &breaks {
            assert_eq!(line_columns_at(&line[..b.byte], 0), b.col);
        }
    }

    #[test]
    fn continuation_rows_keep_the_indent_of_the_line() {
        let line = "    aaa bbb ccc ddd";
        let p = params(12);
        let rows = rows_of(line, p);
        assert_eq!(rows, vec!["    aaa bbb ", "ccc ddd"]);
        assert_eq!(continuation_indent(line, 12, 0), 4);
        assert_eq!(continuation_indent(line, 12, 2), 6);
        // An indent that takes more than half of the width is dropped.
        assert_eq!(continuation_indent(line, 7, 0), 0);
        // Rows after the first have indent cells less room.
        let rows = rows_of("  aaa bbb ccc ddd eee fff", params(12));
        assert_eq!(rows, vec!["  aaa bbb ", "ccc ddd ", "eee fff"]);
    }

    #[test]
    fn cyrillic_words_wrap_at_spaces() {
        assert_eq!(
            rows_of("привет мир как дела", params(12)),
            vec!["привет мир ", "как дела"]
        );
    }

    #[test]
    fn a_combining_mark_stays_with_its_base() {
        // "e" + U+0301 takes one cell; the break after "e\u{301} " keeps them together.
        let line = "ab cd e\u{301}f gh ij";
        for b in wrap_line(line, params(8)) {
            assert!(line.is_char_boundary(b.byte));
            assert_ne!(line[b.byte..].chars().next(), Some('\u{301}'));
        }
    }

    #[test]
    fn every_row_holds_a_character_and_stays_within_the_width() {
        let samples = [
            "the quick brown fox jumps over the lazy dog and keeps running far away",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "\t\tfn  long_name(argument_one: usize, argument_two: &str) -> Result<()>",
            "日本語のテキストは、どこでも折り返せます。English words too.",
            "   ",
            "a                                        b",
        ];
        for cols in [8, 9, 13, 40] {
            for sample in samples {
                let p = params(cols);
                let breaks = wrap_line(sample, p);
                let mut start = 0;
                let mut start_col = 0;
                let cont = cols - continuation_indent(sample, cols, 0);
                for (i, b) in breaks.iter().enumerate() {
                    assert!(b.byte > start, "{sample:?} at {cols}: empty row");
                    assert!(sample.is_char_boundary(b.byte));
                    assert_eq!(line_columns_at(&sample[..b.byte], 0), b.col);
                    let cap = if i == 0 { cols } else { cont };
                    let row = &sample[start..b.byte];
                    let trimmed = row.trim_end_matches([' ', '\t']);
                    let width = line_columns_at(trimmed, start_col) - start_col;
                    let one_char = trimmed.chars().count() <= 1;
                    assert!(
                        width <= cap || one_char,
                        "{sample:?} at {cols}: row {row:?} is {width} wide"
                    );
                    start = b.byte;
                    start_col = b.col;
                }
            }
        }
    }

    fn text(s: &str) -> Text {
        Text::from(s)
    }

    fn edit(map: &mut WrapMap, old: &str, range: Range<usize>, with: &str) -> (Text, bool) {
        let tx = Transaction::replace(range, with).expect("valid range");
        let new = text(&tx.apply_to_string(old).expect("applies"));
        let ok = map.apply(&new, map.version() + 1, &tx);
        (new, ok)
    }

    #[test]
    fn the_map_counts_rows_and_finds_them() {
        let t = text("aaa bbb ccc ddd\n\nshort\nxxxxxxxxxxxxxxxxxxxx");
        let map = WrapMap::new(&t, 1, params(8));
        assert_eq!(map.line_count(), 4);
        assert_eq!(map.rows_in_line(0), 2);
        assert_eq!(map.rows_in_line(1), 1, "an empty line is a row");
        assert_eq!(map.rows_in_line(2), 1);
        assert_eq!(map.rows_in_line(3), 3);
        assert_eq!(map.total_rows(), 7);
        assert_eq!(map.first_row_of_line(3), 4);
        assert_eq!(map.locate_row(0), (0, 0));
        assert_eq!(map.locate_row(1), (0, 1));
        assert_eq!(map.locate_row(2), (1, 0));
        assert_eq!(map.locate_row(5), (3, 1));
        assert_eq!(map.locate_row(99), (3, 2), "past the end: the last row");
        let row = map.row_at(&t, 1);
        assert_eq!(row.text(&t), "ccc ddd");
        assert!(row.last && !row.first);
        assert_eq!(row.start_col, 8);
    }

    #[test]
    fn a_byte_at_a_break_is_in_the_next_row_unless_the_caret_is_upstream() {
        let t = text("aaa bbb ccc ddd");
        let map = WrapMap::new(&t, 1, params(8));
        assert_eq!(map.locate_byte(&t, 7, false), (0, 0));
        assert_eq!(map.locate_byte(&t, 8, false), (0, 1));
        assert_eq!(map.locate_byte(&t, 8, true), (0, 0));
        assert_eq!(
            map.locate_byte(&t, 15, false),
            (0, 1),
            "the end of the line"
        );
        assert_eq!(map.row_of_byte(&t, 8, true), 0);
        let (row, x) = map.x_of_byte(&t, 8, true);
        assert_eq!((row, x), (0, 8.0), "drawn at the end of the first row");
        let (row, x) = map.x_of_byte(&t, 8, false);
        assert_eq!((row, x), (1, 0.0));
    }

    #[test]
    fn continuation_rows_are_drawn_indented() {
        let t = text("    aaa bbb ccc ddd");
        let map = WrapMap::new(&t, 1, params(12));
        let row = map.row_at(&t, 1);
        assert_eq!(row.indent, 4);
        let (r, x) = map.x_of_byte(&t, 13, false);
        assert_eq!((r, x), (1, 4.0 + 1.0), "one character into the second row");
    }

    #[test]
    fn hit_testing_maps_a_cell_back_to_a_byte() {
        let t = text("aaa bbb ccc ddd");
        let map = WrapMap::new(&t, 1, params(8));
        assert_eq!(map.byte_at_x(&t, 0, 2.2), (2, false));
        assert_eq!(map.byte_at_x(&t, 1, 0.0), (8, false));
        assert_eq!(map.byte_at_x(&t, 1, 3.6), (12, false));
        // Past the end of the first row: the end of the row, shown there.
        assert_eq!(map.byte_at_x(&t, 0, 30.0), (8, true));
        // Past the end of the last row: the end of the line.
        assert_eq!(map.byte_at_x(&t, 1, 30.0), (15, false));
        assert_eq!(map.byte_at_goal(&t, 1, 2), (10, false));
        assert_eq!(map.byte_at_goal(&t, 0, 30), (8, true));
    }

    #[test]
    fn an_edit_wraps_only_the_lines_it_touched() {
        let old = "aaa bbb ccc ddd\nxxx yyy zzz www\nshort\nlast line is long enough";
        let mut map = WrapMap::new(&text(old), 1, params(8));
        let before = map.rewrapped();
        assert_eq!(map.total_rows(), 2 + 2 + 1 + 4);
        // Typing in the second line (a long one) wraps that line only.
        let (new, ok) = edit(&mut map, old, 16..16, "q");
        assert!(ok);
        assert_eq!(map.rewrapped() - before, 1);
        assert_eq!(map.rows_in_line(1), 2);
        assert_eq!(map.total_rows(), 9);
        // The rows after the edit moved with their lines.
        assert_eq!(map.first_row_of_line(3), 5);
        assert_eq!(map.row_at(&new, 5).text(&new), "last ");
    }

    #[test]
    fn an_edit_at_a_wrap_boundary_moves_the_break() {
        let old = "aaa bbb ccc ddd";
        let mut map = WrapMap::new(&text(old), 1, params(8));
        assert_eq!(map.rows_in_line(0), 2);
        // Insert a long word at the start of the second row: the row wraps into two.
        let (new, ok) = edit(&mut map, old, 8..8, "eeeeeeee ");
        assert!(ok);
        assert_eq!(map.rows_in_line(0), 3);
        assert_eq!(map.row_at(&new, 1).text(&new), "eeeeeeee ");
        // Delete it again.
        let (back, ok) = edit(&mut map, "aaa bbb eeeeeeee ccc ddd", 8..17, "");
        assert!(ok);
        assert_eq!(back.slice(0..back.len()).unwrap(), old);
        assert_eq!(map.rows_in_line(0), 2);
        assert_eq!(map.total_rows(), 2);
    }

    #[test]
    fn lines_joined_and_split_keep_the_map_in_step() {
        let old = "aaa bbb ccc ddd\nshort\nxxxxxxxxxx";
        let mut map = WrapMap::new(&text(old), 1, params(8));
        // Delete the line break after the first line: two lines become one.
        let (joined, ok) = edit(&mut map, old, 15..16, " ");
        assert!(ok);
        assert_eq!(map.line_count(), joined.line_count());
        assert_eq!(map.rows_in_line(0), 3);
        // Split it again, twice at once.
        let (split, ok) = edit(&mut map, "aaa bbb ccc ddd short\nxxxxxxxxxx", 15..16, "\n");
        assert!(ok);
        assert_eq!(map.line_count(), split.line_count());
        assert_eq!(
            map.total_rows(),
            WrapMap::new(&split, 1, params(8)).total_rows()
        );
    }

    #[test]
    fn a_multi_edit_transaction_over_one_line_is_carried_in() {
        let old = "aaa bbb ccc ddd\nrest";
        let mut map = WrapMap::new(&text(old), 1, params(8));
        let tx = Transaction::new([
            hutan_buffer::Edit::insert(0, "A\nB "),
            hutan_buffer::Edit::insert(15, " eee fff ggg"),
        ])
        .expect("edits");
        let new = text(&tx.apply_to_string(old).expect("applies"));
        assert!(map.apply(&new, 2, &tx));
        let fresh = WrapMap::new(&new, 2, params(8));
        assert_eq!(map.total_rows(), fresh.total_rows());
        for line in 0..new.line_count() {
            assert_eq!(
                map.rows_in_line(line),
                fresh.rows_in_line(line),
                "line {line}"
            );
        }
    }

    #[test]
    fn the_state_carries_edits_and_rebuilds_on_a_new_width() {
        let old = "aaa bbb ccc ddd\nshort";
        let mut state = WrapState {
            mode: WrapMode::Viewport,
            viewport_cols: Some(8),
            ..WrapState::default()
        };
        state.sync(&text(old), 1);
        assert_eq!(state.map.as_ref().map(WrapMap::total_rows), Some(3));
        // An edit is noted and carried in at the next sync.
        let tx = Transaction::insert(21, " and more words");
        let new = text(&tx.apply_to_string(old).expect("applies"));
        state.note_edit(2, &tx);
        state.sync(&new, 2);
        let map = state.map.as_ref().expect("map");
        assert_eq!(map.version(), 2);
        assert_eq!(
            map.rewrapped(),
            3,
            "two lines scanned when built, one by the edit"
        );
        // A version nobody told it about: built again.
        state.sync(&new, 5);
        assert_eq!(state.map.as_ref().map(WrapMap::version), Some(5));
        // A new width.
        state.viewport_cols = Some(40);
        state.sync(&new, 5);
        assert_eq!(state.map.as_ref().map(|m| m.params().cols), Some(40));
        // Off: no map.
        state.mode = WrapMode::Off;
        state.sync(&new, 5);
        assert!(state.map.is_none());
    }

    #[test]
    fn column_mode_is_bounded_by_the_view() {
        assert_eq!(WrapMode::Off.cols(80), None);
        assert_eq!(WrapMode::Viewport.cols(80), Some(80));
        assert_eq!(WrapMode::Column(100).cols(80), Some(80));
        assert_eq!(WrapMode::Column(60).cols(80), Some(60));
        assert_eq!(WrapMode::Viewport.cols(3), Some(MIN_WRAP_COLS));
    }

    #[test]
    fn a_huge_text_is_not_wrapped() {
        let state = WrapState {
            mode: WrapMode::Viewport,
            viewport_cols: Some(80),
            ..WrapState::default()
        };
        assert!(state.params(MAX_WRAP_BYTES).is_some());
        assert!(state.params(MAX_WRAP_BYTES + 1).is_none());
    }
}
