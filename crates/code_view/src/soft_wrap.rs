//! A view's soft wrap (EDITOR-WRAP): the mode it was asked for, the [`WrapMap`] of the last
//! frame's width kept in step with the text, and the rows every place that used to count lines
//! now counts (scrolling, the caret, the mouse). Without a map a row is a line.

use gpui::{App, Context};
use hutan_buffer::{Text, Transaction};

use crate::view::CodeView;
use crate::wrap::{RowGeom, WrapMap, WrapMode, WrapParams};

impl CodeView {
    /// How the view wraps lines.
    pub fn soft_wrap(&self) -> WrapMode {
        self.wrap.borrow().mode
    }

    /// Sets how the view wraps lines; the next frame wraps and keeps the first row in view where
    /// it was.
    pub fn set_soft_wrap(&mut self, mode: WrapMode, cx: &mut Context<Self>) {
        let state = self.wrap.get_mut();
        if state.mode != mode {
            state.mode = mode;
            state.upstream = None;
            cx.notify();
        }
    }

    /// Whether lines are wrapped in the last frame (the mode is on, the text is not too large
    /// and the view has been drawn).
    pub fn is_wrapped(&self) -> bool {
        self.wrap.borrow().map.is_some()
    }

    /// Whether the view wraps (or will, at its next frame): the mode is on and the text is
    /// small enough ([`MAX_WRAP_BYTES`](crate::MAX_WRAP_BYTES)). The status bar's `Wrap`.
    pub fn wraps_text(&self, cx: &App) -> bool {
        self.soft_wrap() != WrapMode::Off
            && self.buffer().read(cx).text().len() <= crate::wrap::MAX_WRAP_BYTES
    }

    /// How many lines the view's map wrapped by a scan since it was built (what an edit costs;
    /// tests and benchmarks).
    pub fn wrap_rewrapped(&self) -> u64 {
        self.wrap
            .borrow()
            .map
            .as_ref()
            .map_or(0, WrapMap::rewrapped)
    }

    /// The row width of the last frame, in cells, while wrapping.
    pub fn wrap_params(&self) -> Option<WrapParams> {
        self.wrap.borrow().map.as_ref().map(WrapMap::params)
    }

    /// Brings the map in step with the buffer's text (a no-op when it already is).
    pub(crate) fn sync_wrap(&self, cx: &App) {
        let buffer = self.buffer().read(cx);
        self.wrap
            .borrow_mut()
            .sync(buffer.text(), buffer.version(), |from| {
                buffer.edits_since(from)
            });
    }

    /// Runs `f` on the map and the text, `None` when the view does not wrap. `f` must not call
    /// the view's other wrap helpers.
    pub(crate) fn with_wrap<R>(&self, cx: &App, f: impl FnOnce(&WrapMap, &Text) -> R) -> Option<R> {
        self.sync_wrap(cx);
        let state = self.wrap.borrow();
        let map = state.map.as_ref()?;
        Some(f(map, self.buffer().read(cx).text()))
    }

    /// Whether the view wraps its lines right now (the map is in step with the text).
    pub(crate) fn is_wrapping(&self, cx: &App) -> bool {
        self.with_wrap(cx, |_, _| ()).is_some()
    }

    /// Display rows of the whole text: the lines, or more while wrapping.
    pub fn display_rows(&self, cx: &App) -> usize {
        self.with_wrap(cx, |map, _| map.total_rows())
            .unwrap_or_else(|| self.buffer().read(cx).text().line_count())
    }

    /// The caret that stands at byte `at` (the primary's) is drawn at the end of the row before.
    pub(crate) fn upstream_at(&self, at: usize) -> bool {
        self.wrap.borrow().upstream == Some(at) && self.selections.primary().head == at
    }

    /// Where the primary caret stands: drawn at the end of a row (`upstream`, see
    /// [`WrapState::upstream`](crate::wrap::WrapState)) or not.
    pub(crate) fn set_upstream(&mut self, at: Option<usize>) {
        self.wrap.get_mut().upstream = at;
    }

    /// The display row byte `at` is drawn in: its line when the view does not wrap.
    pub fn row_of_offset(&self, at: usize, cx: &App) -> usize {
        let upstream = self.upstream_at(at);
        self.with_wrap(cx, |map, text| map.row_of_byte(text, at, upstream))
            .unwrap_or_else(|| {
                let text = self.buffer().read(cx).text();
                text.line_of_byte(at.min(text.len())).unwrap_or(0)
            })
    }

    /// The display row line `line` starts at.
    pub fn first_row_of_line(&self, line: usize, cx: &App) -> usize {
        self.with_wrap(cx, |map, _| map.first_row_of_line(line))
            .unwrap_or(line)
    }

    /// The row of the line (and the line) at display row `row`.
    pub(crate) fn line_of_row(&self, row: usize, cx: &App) -> (usize, usize) {
        self.with_wrap(cx, |map, _| map.locate_row(row))
            .unwrap_or_else(|| {
                let last = self.buffer().read(cx).text().line_count().saturating_sub(1);
                (row.min(last), 0)
            })
    }

    /// The row geometry at display row `row` while wrapping.
    pub(crate) fn row_geom(&self, row: usize, cx: &App) -> Option<RowGeom> {
        self.with_wrap(cx, |map, text| map.row_at(text, row))
    }

    /// The text drawn on display row `row` (its line's text when the view does not wrap);
    /// `None` past the last row.
    pub fn display_row_text(&self, row: usize, cx: &App) -> Option<String> {
        if row >= self.display_rows(cx) {
            return None;
        }
        match self.row_geom(row, cx) {
            Some(geom) => Some(geom.text(self.buffer().read(cx).text())),
            None => self.buffer().read(cx).text().line_text(row).ok(),
        }
    }

    /// The line at the top of the view as of the last frame.
    pub(crate) fn top_row(&self) -> usize {
        self.layout.map_or(0, |l| {
            (self.scroll_y / f32::from(l.line_height)).floor().max(0.0) as usize
        })
    }

    /// Called by the frame: tells the state how wide the text area is, brings the map to the
    /// text and keeps the row at the top of the view where it was when the wrapping changed
    /// (the mode, the width).
    pub(crate) fn sync_wrap_frame(
        &mut self,
        text: &Text,
        version: u64,
        edits: impl FnOnce(u64) -> Option<Transaction>,
    ) {
        let Some(layout) = self.layout else {
            return;
        };
        let line_h = f32::from(layout.line_height).max(1.0);
        // One cell stays free at the right edge: the caret at the end of a row.
        let cols = (f32::from(layout.bounds.right() - layout.text_left)
            / f32::from(layout.cell_width))
        .floor()
        .max(0.0) as usize;
        let top = (self.scroll_y / line_h).floor().max(0.0) as usize;
        let state = self.wrap.get_mut();
        let before = state.map.as_ref().map(WrapMap::params);
        let anchor = match &state.map {
            Some(map) if map.version() == version => Some(map.row_at(text, top).bytes.start),
            Some(_) => None,
            None => text
                .line_start(top.min(text.line_count().saturating_sub(1)))
                .ok(),
        };
        state.viewport_cols = Some(cols.saturating_sub(1));
        state.sync(text, version, edits);
        let after = state.map.as_ref().map(WrapMap::params);
        if before != after
            && let Some(anchor) = anchor
        {
            let row = match &state.map {
                Some(map) => map.row_of_byte(text, anchor, false),
                None => text.line_of_byte(anchor).unwrap_or(0),
            };
            self.scroll_y = row as f32 * line_h;
        }
        if after.is_some() {
            self.scroll_x = 0.0;
        }
    }
}
