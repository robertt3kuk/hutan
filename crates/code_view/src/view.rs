//! [`CodeView`]: one view of a [`FileBuffer`] — scroll position, selection, find matches and
//! the caches of what was drawn. Several views (panes) share one buffer.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, ScrollWheelEvent, ShapedLine, Styled as _, Task,
    Window, div,
};
use hutan_buffer::Text;

use crate::buffer::FileBuffer;
use crate::element::CodeElement;
use crate::find::{self, FindQuery, FindState};
use crate::layout;
use crate::style::CodeStyle;
use crate::{Bottom, Copy, KEY_CONTEXT, LineDown, LineUp, PageDown, PageUp, SelectAll, Top};

/// Lines shaped above and below the ones that intersect the view. None: a frame shapes exactly
/// what it shows (a partly visible line at each edge included).
pub const OVERSCAN_LINES: usize = 0;

/// What happened to a view that its owner may show (the status bar's position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeViewEvent {
    /// The selection or the find state changed.
    Changed,
}

/// The selected text: `anchor` is where the mouse went down, `head` where it is now (byte
/// offsets; `head` may be before `anchor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// What a drag extends: characters, words (a double click) or lines (a triple click), from the
/// unit the press landed on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Drag {
    Chars,
    Words(Range<usize>),
    Lines(Range<usize>),
}

/// The geometry of the last frame, for mouse hit testing and scrolling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub bounds: gpui::Bounds<Pixels>,
    /// Left edge of the text area (right of the gutter).
    pub text_left: Pixels,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    /// Lines that fit the view (rounded up).
    pub rows: usize,
}

/// Counters of what frames did (the deterministic half of the performance budgets).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewStats {
    /// Frames prepared.
    pub frames: u64,
    /// Lines shaped since the view was made.
    pub lines_shaped: u64,
    /// Lines the last frame shaped (cache misses).
    pub last_shaped: usize,
    /// Lines the last frame drew.
    pub last_lines: Range<usize>,
}

/// When content first reached the screen (benchmarks B11a/c).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaintTimes {
    /// The first frame that drew text of the file.
    pub first_text: Option<Instant>,
    /// The first frame whose lines had colors from a finished tree of the current text.
    pub first_highlighted: Option<Instant>,
}

/// Shaped lines by content key, so scrolling back and frames without changes cost a hash.
#[derive(Default)]
pub(crate) struct LineCache {
    entries: HashMap<u64, (Rc<ShapedLine>, u64)>,
    generation: u64,
}

/// Entries kept once the cache outgrows this; the current frame's lines always survive.
const CACHE_SOFT_LIMIT: usize = 1024;

impl LineCache {
    pub(crate) fn begin_frame(&mut self) {
        self.generation += 1;
        if self.entries.len() > CACHE_SOFT_LIMIT {
            let keep_from = self.generation.saturating_sub(1);
            self.entries.retain(|_, (_, used)| *used >= keep_from);
        }
    }

    pub(crate) fn get(&mut self, key: u64) -> Option<Rc<ShapedLine>> {
        let generation = self.generation;
        self.entries.get_mut(&key).map(|(line, used)| {
            *used = generation;
            line.clone()
        })
    }

    pub(crate) fn insert(&mut self, key: u64, line: Rc<ShapedLine>) {
        self.entries.insert(key, (line, self.generation));
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

/// One view of a file.
pub struct CodeView {
    buffer: Entity<FileBuffer>,
    pub(crate) style: CodeStyle,
    /// Bumped when the style changes: every shaped line is keyed by it.
    pub(crate) style_generation: u64,
    focus: FocusHandle,
    /// Pixels of content above the top of the view.
    pub(crate) scroll_y: f32,
    /// Pixels of content left of the text area's edge.
    pub(crate) scroll_x: f32,
    pub(crate) selection: Option<Selection>,
    drag: Option<Drag>,
    pub(crate) find: FindState,
    find_task: Option<Task<()>>,
    /// A line to bring into view as soon as the text and the geometry are known (1-based line,
    /// optional 1-based column).
    reveal: Option<(usize, Option<usize>)>,
    pub(crate) layout: Option<Layout>,
    pub(crate) lines: LineCache,
    pub(crate) numbers: LineCache,
    pub(crate) stats: ViewStats,
    pub(crate) times: PaintTimes,
    /// The widest line drawn so far, in columns (how far the view scrolls right).
    pub(crate) widest: usize,
    /// The text version the caches hold.
    drawn_version: u64,
    _observe: gpui::Subscription,
}

impl std::fmt::Debug for CodeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeView")
            .field("scroll_y", &self.scroll_y)
            .field("selection", &self.selection)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<CodeViewEvent> for CodeView {}

impl Focusable for CodeView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CodeView {
    pub fn new(buffer: Entity<FileBuffer>, style: CodeStyle, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&buffer, |this, buffer, cx| {
            // A new text: what was drawn and found no longer applies.
            if this.stats.frames > 0 && buffer.read(cx).version() != this.drawn_version {
                this.lines.clear();
            }
            cx.notify();
        });
        Self {
            buffer,
            style,
            style_generation: 0,
            focus: cx.focus_handle(),
            scroll_y: 0.0,
            scroll_x: 0.0,
            selection: None,
            drag: None,
            find: FindState::default(),
            find_task: None,
            reveal: None,
            layout: None,
            lines: LineCache::default(),
            numbers: LineCache::default(),
            stats: ViewStats::default(),
            times: PaintTimes::default(),
            widest: 0,
            drawn_version: 0,
            _observe: observe,
        }
    }

    pub fn buffer(&self) -> &Entity<FileBuffer> {
        &self.buffer
    }

    pub fn style(&self) -> &CodeStyle {
        &self.style
    }

    /// Restyles the view (a theme or font change); the text and its syntax are untouched.
    pub fn set_style(&mut self, style: CodeStyle, cx: &mut Context<Self>) {
        if self.style != style {
            self.style = style;
            self.style_generation += 1;
            self.lines.clear();
            self.numbers.clear();
            cx.notify();
        }
    }

    pub fn stats(&self) -> &ViewStats {
        &self.stats
    }

    pub fn paint_times(&self) -> PaintTimes {
        self.times
    }

    pub fn layout(&self) -> Option<Layout> {
        self.layout
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The selected text, if any.
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let selection = self.selection.filter(|s| !s.is_empty())?;
        self.buffer.read(cx).text().slice(selection.range()).ok()
    }

    /// The first line drawn (0-based).
    pub fn top_line(&self) -> usize {
        self.layout.map_or(0, |l| {
            (self.scroll_y / f32::from(l.line_height)).floor() as usize
        })
    }

    /// The 1-based line and column of the selection's head (where the mouse is), else of the
    /// top line: what the status bar shows.
    pub fn position(&self, cx: &App) -> (usize, usize) {
        let text = self.buffer.read(cx).text();
        match self.selection {
            Some(selection) => {
                let line = text.line_of_byte(selection.head).unwrap_or(0);
                let start = text.line_start(line).unwrap_or(0);
                let before = text.slice(start..selection.head).unwrap_or_default();
                (line + 1, before.chars().count() + 1)
            }
            None => (self.top_line() + 1, 1),
        }
    }

    // ---- scrolling ---------------------------------------------------------------------------

    /// Brings 1-based `line` (and `column`) into view, about a third from the top, once the text
    /// is in and the view has been laid out.
    pub fn reveal_line(&mut self, line: usize, column: Option<usize>, cx: &mut Context<Self>) {
        self.reveal = Some((line.max(1), column));
        self.apply_reveal(cx);
        cx.notify();
    }

    /// Applies a pending [`reveal_line`](Self::reveal_line) when it can be.
    pub(crate) fn apply_reveal(&mut self, cx: &App) {
        let (Some((line, column)), Some(layout)) = (self.reveal, self.layout) else {
            return;
        };
        let buffer = self.buffer.read(cx);
        if !buffer.is_ready() {
            return;
        }
        let text = buffer.text();
        let index = (line - 1).min(text.line_count().saturating_sub(1));
        let line_h = f32::from(layout.line_height);
        let third = (layout.rows / 3) as f32 * line_h;
        self.scroll_y = (index as f32 * line_h - third).max(0.0);
        if let (Some(column), Ok(start)) = (column, text.line_start(index)) {
            let content = text.line_content_range(index).unwrap_or(start..start);
            let line_text = text.slice(content.clone()).unwrap_or_default();
            let byte = line_text
                .char_indices()
                .nth(column.saturating_sub(1))
                .map_or(line_text.len(), |(at, _)| at);
            let at = content.start + byte;
            self.selection = Some(Selection {
                anchor: at,
                head: at,
            });
        }
        self.reveal = None;
        self.clamp_scroll(cx);
    }

    fn line_count(&self, cx: &App) -> usize {
        self.buffer.read(cx).text().line_count()
    }

    /// Keeps the scroll inside the content: the last line can reach the top, the widest line
    /// seen can reach the left edge.
    pub(crate) fn clamp_scroll(&mut self, cx: &App) {
        let Some(layout) = self.layout else {
            return;
        };
        let line_h = f32::from(layout.line_height);
        let max_y = (self.line_count(cx).saturating_sub(1)) as f32 * line_h;
        self.scroll_y = self.scroll_y.clamp(0.0, max_y.max(0.0));
        let visible_w = f32::from(layout.bounds.right() - layout.text_left).max(0.0);
        let max_x = (self.widest as f32 * f32::from(layout.cell_width) - visible_w * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Scrolls by `lines` (negative: up).
    pub fn scroll_lines(&mut self, lines: f32, cx: &mut Context<Self>) {
        let line_h = self.layout.map_or(16.0, |l| f32::from(l.line_height));
        self.scroll_y += lines * line_h;
        self.clamp_scroll(cx);
        cx.notify();
    }

    /// Scrolls to `y` pixels from the top (benchmarks, tests).
    pub fn scroll_to(&mut self, y: f32, cx: &mut Context<Self>) {
        self.scroll_y = y;
        self.clamp_scroll(cx);
        cx.notify();
    }

    pub fn scroll_y(&self) -> f32 {
        self.scroll_y
    }

    pub(crate) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else {
            return;
        };
        let delta = event.delta.pixel_delta(layout.line_height);
        // Positive y moves the content down, towards the start of the file.
        self.scroll_y -= f32::from(delta.y);
        self.scroll_x -= f32::from(delta.x);
        self.clamp_scroll(cx);
        cx.notify();
    }

    fn page(&self) -> f32 {
        self.layout
            .map_or(20.0, |l| l.rows.saturating_sub(2).max(1) as f32)
    }

    // ---- hit testing and selection ---------------------------------------------------------

    /// The byte offset under window position `position` (clamped into the text).
    pub(crate) fn offset_at(&self, position: Point<Pixels>, cx: &App) -> Option<usize> {
        let layout = self.layout?;
        let text = self.buffer.read(cx).text();
        let line_h = f32::from(layout.line_height);
        let y = f32::from(position.y - layout.bounds.top()) + self.scroll_y;
        if y < 0.0 {
            return Some(0);
        }
        let line = (y / line_h).floor() as usize;
        if line >= text.line_count() {
            return Some(text.len());
        }
        let content = text.line_content_range(line).ok()?;
        let x = f32::from(position.x - layout.text_left) + self.scroll_x;
        let x_cols = (x / f32::from(layout.cell_width)).max(0.0);
        let window = line_window(text, content.clone(), x_cols as usize + 2);
        Some(content.start + layout::byte_of_col(&window, x_cols))
    }

    pub(crate) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        window.focus(&self.focus, cx);
        let Some(at) = self.offset_at(event.position, cx) else {
            return;
        };
        let text = self.buffer.read(cx).text().clone();
        match event.click_count {
            2 => {
                let word = word_at(&text, at);
                self.selection = Some(Selection {
                    anchor: word.start,
                    head: word.end,
                });
                self.drag = Some(Drag::Words(word));
            }
            n if n >= 3 => {
                let line = line_at(&text, at);
                self.selection = Some(Selection {
                    anchor: line.start,
                    head: line.end,
                });
                self.drag = Some(Drag::Lines(line));
            }
            _ => {
                let anchor = match self.selection {
                    Some(selection) if event.modifiers.shift => selection.anchor,
                    _ => at,
                };
                self.selection = Some(Selection { anchor, head: at });
                self.drag = Some(Drag::Chars);
            }
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    pub(crate) fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(at) = self.offset_at(event.position, cx) else {
            return;
        };
        let text = self.buffer.read(cx).text().clone();
        let selection = match drag {
            Drag::Chars => Selection {
                anchor: self.selection.map_or(at, |s| s.anchor),
                head: at,
            },
            Drag::Words(origin) => extend(origin, word_at(&text, at)),
            Drag::Lines(origin) => extend(origin, line_at(&text, at)),
        };
        if self.selection != Some(selection) {
            self.selection = Some(selection);
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
        }
    }

    pub(crate) fn mouse_up(&mut self, _event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.drag = None;
    }

    /// Selects `range` (byte offsets) and scrolls it into view.
    pub fn select(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selection = Some(Selection {
            anchor: range.start,
            head: range.end,
        });
        self.scroll_into_view(range.start, cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let len = self.buffer.read(cx).text().len();
        self.selection = Some(Selection {
            anchor: 0,
            head: len,
        });
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selected_text(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Scrolls so that byte `offset` is on screen (centred vertically when it was not).
    fn scroll_into_view(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else {
            return;
        };
        let text = self.buffer.read(cx).text().clone();
        let Ok(line) = text.line_of_byte(offset) else {
            return;
        };
        let line_h = f32::from(layout.line_height);
        let top = line as f32 * line_h;
        let view_h = f32::from(layout.bounds.size.height);
        if top < self.scroll_y || top + line_h > self.scroll_y + view_h {
            self.scroll_y = top - (view_h / 2.0).floor();
        }
        if let Ok(start) = text.line_start(line) {
            let window = line_window(&text, start..offset, usize::MAX);
            let col = layout::line_columns(&window) as f32;
            let x = col * f32::from(layout.cell_width);
            let visible_w = f32::from(layout.bounds.right() - layout.text_left);
            self.widest = self.widest.max(col as usize + 1);
            if x < self.scroll_x || x > self.scroll_x + visible_w - f32::from(layout.cell_width) {
                self.scroll_x = (x - visible_w / 3.0).max(0.0);
            }
        }
        self.clamp_scroll(cx);
    }

    // ---- find --------------------------------------------------------------------------------

    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    /// Searches the file for `query` (in the background); the first match at or after the top
    /// of the view becomes current and is scrolled into view.
    pub fn set_find_query(&mut self, query: FindQuery, cx: &mut Context<Self>) {
        if query == self.find.query && !self.find.searching && self.find.generation > 0 {
            return;
        }
        self.find.generation += 1;
        let generation = self.find.generation;
        self.find.query = query.clone();
        self.find.current = None;
        if query.text.is_empty() {
            self.find.result = find::FindResult::default();
            self.find.searching = false;
            self.find_task = None;
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
            return;
        }
        self.find.searching = true;
        let text = self.buffer.read(cx).text().clone();
        let search = cx.background_spawn(async move { find::run(&text, &query) });
        self.find_task = Some(cx.spawn(async move |this, cx| {
            let result = search.await;
            this.update(cx, |this, cx| {
                if this.find.generation != generation {
                    return;
                }
                this.find.searching = false;
                this.find.result = result;
                let top = this.top_offset(cx);
                this.find.current = this.find.first_from(top);
                if let Some(current) = this.find.current_match() {
                    this.scroll_into_view(current.start, cx);
                }
                cx.emit(CodeViewEvent::Changed);
                cx.notify();
            })
            .ok();
        }));
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// Whether a search is running (tests wait on it).
    pub fn is_searching(&self) -> bool {
        self.find.searching
    }

    /// Moves to the next (`forward`) or previous match and scrolls it into view.
    pub fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(found) = self.find.step(forward) {
            self.scroll_into_view(found.start, cx);
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
        }
    }

    /// Ends find: the matches are no longer drawn.
    pub fn clear_find(&mut self, cx: &mut Context<Self>) {
        self.set_find_query(FindQuery::default(), cx);
    }

    /// The byte offset of the top line.
    fn top_offset(&self, cx: &App) -> usize {
        let text = self.buffer.read(cx).text();
        text.line_start(self.top_line().min(text.line_count().saturating_sub(1)))
            .unwrap_or(0)
    }
}

impl CodeView {
    /// The version of the text the caches hold.
    pub(crate) fn note_drawn(&mut self, version: u64) {
        self.drawn_version = version;
    }
}

/// The text of the content range `content` of a line, cut so that at least `cols` columns are
/// in it without copying a pathological single line of megabytes (a column is at least one
/// byte and at most four for every character but a zero-width mark).
pub(crate) fn line_window(text: &Text, content: Range<usize>, cols: usize) -> String {
    let budget = cols.saturating_mul(4).saturating_add(64);
    let end = if content.len() > budget {
        text.floor_char_boundary(content.start + budget)
    } else {
        content.end
    };
    text.slice(content.start..end).unwrap_or_default()
}

/// The selection a drag makes from the unit `origin` to the unit `now`.
fn extend(origin: Range<usize>, now: Range<usize>) -> Selection {
    if now.start < origin.start {
        Selection {
            anchor: origin.end,
            head: now.start,
        }
    } else {
        Selection {
            anchor: origin.start,
            head: now.end.max(origin.end),
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word (or run of other characters) around byte `at`.
fn word_at(text: &Text, at: usize) -> Range<usize> {
    let Ok(line) = text.line_of_byte(at) else {
        return at..at;
    };
    let Ok(content) = text.line_content_range(line) else {
        return at..at;
    };
    let line_text = text.slice(content.clone()).unwrap_or_default();
    let rel = at.saturating_sub(content.start).min(line_text.len());
    let class = |c: char| {
        if is_word(c) {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let Some(here) = line_text[rel..]
        .chars()
        .next()
        .or_else(|| line_text[..rel].chars().next_back())
    else {
        return at..at;
    };
    let kind = class(here);
    let start = line_text[..rel]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == kind)
        .last()
        .map_or(rel, |(i, _)| i);
    let end = line_text[rel..]
        .char_indices()
        .take_while(|(_, c)| class(*c) == kind)
        .last()
        .map_or(rel, |(i, c)| rel + i + c.len_utf8());
    content.start + start..content.start + end
}

/// The whole line around byte `at`, its line break included.
fn line_at(text: &Text, at: usize) -> Range<usize> {
    text.line_of_byte(at)
        .and_then(|line| text.line_range(line))
        .unwrap_or(at..at)
}

impl Render for CodeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("code-view")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &LineUp, _, cx| this.scroll_lines(-1.0, cx)))
            .on_action(cx.listener(|this, _: &LineDown, _, cx| this.scroll_lines(1.0, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let page = this.page();
                this.scroll_lines(-page, cx);
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let page = this.page();
                this.scroll_lines(page, cx);
            }))
            .on_action(cx.listener(|this, _: &Top, _, cx| this.scroll_to(0.0, cx)))
            .on_action(cx.listener(|this, _: &Bottom, _, cx| this.scroll_to(f32::MAX, cx)))
            .child(CodeElement::new(cx.entity()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_lines_around_an_offset() {
        let text = Text::from("let foo_bar = 1;\nnext");
        assert_eq!(word_at(&text, 5), 4..11);
        assert_eq!(word_at(&text, 11), 11..12, "a run of spaces is a unit");
        assert_eq!(
            word_at(&text, 16),
            15..16,
            "end of the line: the character before"
        );
        assert_eq!(line_at(&text, 3), 0..17);
        assert_eq!(line_at(&text, 18), 17..21);
    }

    #[test]
    fn a_drag_extends_whole_units_both_ways() {
        assert_eq!(
            extend(4..7, 10..12),
            Selection {
                anchor: 4,
                head: 12
            }
        );
        assert_eq!(extend(4..7, 0..2), Selection { anchor: 7, head: 0 });
    }

    #[test]
    fn a_huge_line_is_cut_for_drawing() {
        let long = "x".repeat(10_000);
        let text = Text::from(long.as_str());
        assert_eq!(line_window(&text, 0..10_000, 100).len(), 464);
        assert_eq!(line_window(&text, 0..10, 100).len(), 10);
    }
}
