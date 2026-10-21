//! [`CodeView`]: one view of a [`FileBuffer`] — scroll position, selections (several carets),
//! the input method's marked text, find matches and the caches of what was drawn. Several views
//! (panes) share one buffer; an edit made in one is carried into the selections of the others.
//!
//! The editing commands are in `motion.rs` (carets and selections), `editing.rs` (changes to
//! the text) and `input.rs` (the platform's text input: typing, input methods).

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyContext, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollWheelEvent,
    ShapedLine, Styled as _, Subscription, Task, Window, div,
};
use hutan_buffer::{Bias, Selection, Selections, Text, Transaction};

use crate::buffer::{BufferEdited, FileBuffer};
use crate::element::CodeElement;
use crate::find::{self, FindQuery, FindState};
use crate::layout;
use crate::rules::is_word;
use crate::style::CodeStyle;

/// Lines shaped above and below the ones that intersect the view. None: a frame shapes exactly
/// what it shows (a partly visible line at each edge included).
pub const OVERSCAN_LINES: usize = 0;

/// Most lines one column selection (⌥⇧-drag) spans.
pub const MAX_COLUMN_LINES: usize = 10_000;

/// What happened to a view that its owner may show (the status bar's position).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeViewEvent {
    /// The selections, the Vim state or the find state changed.
    Changed,
    /// The view lost the keyboard, or its window stopped being the active one (autosave on
    /// focus change).
    FocusLost,
    /// `:` in Vim's Normal or Visual mode: the owner opens the command line (`visual`: with
    /// the range `'<,'>` of the selection that just ended).
    CommandLine { visual: bool },
}

/// What a drag extends: characters, words (a double click), lines (a triple click) or a block of
/// columns (⌥⇧), from the unit the press landed on. `others` are the selections an ⌥-click
/// keeps next to the one being dragged.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Drag {
    Chars {
        anchor: usize,
        others: Option<Selections>,
    },
    Words {
        origin: Range<usize>,
        others: Option<Selections>,
    },
    Lines {
        origin: Range<usize>,
        others: Option<Selections>,
    },
    /// Line and display column where the press landed.
    Column { line: usize, col: usize },
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

/// Shaped lines by key, so scrolling back and frames without changes cost a hash.
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

/// How a search answer is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchMode {
    /// A new query or a new option: the first match from the top of the view becomes current,
    /// is selected and scrolled to.
    Query,
    /// The text changed: the current match stays the one nearest to where it was; nothing moves.
    Refresh,
    /// A match was replaced: the next one from `offset` becomes current and is selected.
    AfterReplace(usize),
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
    /// Blank pixels the view keeps above its first line (the scroll is negative by this much):
    /// what is left of the room a bar above the view gave up. Scrolling up to the top of the
    /// file takes it back.
    pub(crate) inset: f32,
    /// Keep the text where it is on screen when the view's top moves at the next frame
    /// ([`keep_text_in_place`](CodeView::keep_text_in_place)).
    pub(crate) keep_in_place: bool,
    /// Pixels of content left of the text area's edge.
    pub(crate) scroll_x: f32,
    /// The carets and selections; always at least one.
    pub(crate) selections: Selections,
    /// The text an input method is composing, one range per caret (empty: none).
    pub(crate) marked: Vec<Range<usize>>,
    /// The display column each caret keeps while moving up and down.
    pub(crate) goal: Option<Vec<usize>>,
    /// ⌘D started from a word under the caret: the next occurrences are whole words too.
    pub(crate) occurrence_whole_word: bool,
    /// The text version the last search was started for: an edit's refresh is not needed when
    /// a search of its text already runs (Replace started one).
    searched_version: u64,
    drag: Option<Drag>,
    pub(crate) find: FindState,
    find_task: Option<Task<()>>,
    /// A line to bring into view as soon as the text and the geometry are known (1-based line,
    /// optional 1-based column).
    reveal: Option<(usize, Option<usize>)>,
    /// Bring the primary caret into view at the next frame (it moved before a layout existed).
    pub(crate) autoscroll: bool,
    pub(crate) layout: Option<Layout>,
    /// Shaped lines by (line, text version, columns, style, tree): the cost of a frame where
    /// nothing changed.
    pub(crate) lines: LineCache,
    /// Shaped lines by what they show (text and colors): an edit reshapes the lines it changed,
    /// not every line below it.
    pub(crate) shapes: LineCache,
    pub(crate) numbers: LineCache,
    pub(crate) stats: ViewStats,
    pub(crate) times: PaintTimes,
    /// The widest line drawn so far, in columns (how far the view scrolls right).
    pub(crate) widest: usize,
    /// Vim's state while `editor.vim_mode` is on (`modal.rs`).
    pub(crate) vim: Option<crate::modal::VimState>,
    /// `:set number`: the gutter shows line numbers.
    pub(crate) show_numbers: bool,
    /// The `:s` / `:g` that is searching, and its cancel flag.
    pub(crate) ex_task: Option<Task<()>>,
    pub(crate) ex_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    _observe: Subscription,
    _edits: Subscription,
    /// Focus-out and window-activation watchers, made at the first render (they need the
    /// window).
    focus_watch: Vec<Subscription>,
}

impl std::fmt::Debug for CodeView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeView")
            .field("scroll_y", &self.scroll_y)
            .field("selections", &self.selections)
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
            // A new text (a load) may leave the carets past its end.
            let text = buffer.read(cx).text();
            if !this.selections.is_valid_for(text) {
                this.selections = this.selections.clamp(text);
                this.marked.clear();
                this.goal = None;
            }
            cx.notify();
        });
        let edits = cx.subscribe(&buffer, |this, _, event: &BufferEdited, cx| {
            this.on_buffer_edited(event, cx);
        });
        Self {
            buffer,
            style,
            style_generation: 0,
            focus: cx.focus_handle(),
            scroll_y: 0.0,
            inset: 0.0,
            keep_in_place: false,
            scroll_x: 0.0,
            selections: Selections::cursor(0),
            marked: Vec::new(),
            goal: None,
            occurrence_whole_word: false,
            searched_version: 0,
            drag: None,
            find: FindState::default(),
            find_task: None,
            reveal: None,
            autoscroll: false,
            layout: None,
            lines: LineCache::default(),
            shapes: LineCache::default(),
            numbers: LineCache::default(),
            stats: ViewStats::default(),
            times: PaintTimes::default(),
            widest: 0,
            vim: None,
            show_numbers: true,
            ex_task: None,
            ex_cancel: None,
            _observe: observe,
            _edits: edits,
            focus_watch: Vec::new(),
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
            self.shapes.clear();
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

    /// The carets and selections.
    pub fn selections(&self) -> &Selections {
        &self.selections
    }

    /// The ranges an input method is composing (empty when none).
    pub fn marked_ranges(&self) -> &[Range<usize>] {
        &self.marked
    }

    /// The text of the primary selection, if it is not empty.
    pub fn selected_text(&self, cx: &App) -> Option<String> {
        let primary = self.selections.primary();
        if primary.is_empty() {
            return None;
        }
        self.buffer.read(cx).text().slice(primary.range()).ok()
    }

    /// Whether the text may be changed from this view.
    pub fn is_editable(&self, cx: &App) -> bool {
        self.buffer.read(cx).is_editable()
    }

    /// The first line drawn (0-based).
    pub fn top_line(&self) -> usize {
        self.layout.map_or(0, |l| {
            (self.scroll_y / f32::from(l.line_height)).floor() as usize
        })
    }

    /// The 1-based line and column (in characters) of the primary caret: what the status bar
    /// shows.
    pub fn position(&self, cx: &App) -> (usize, usize) {
        let text = self.buffer.read(cx).text();
        let head = self.selections.primary().head.min(text.len());
        let line = text.line_of_byte(head).unwrap_or(0);
        let start = text.line_start(line).unwrap_or(0);
        let before = text.slice(start..head).unwrap_or_default();
        (line + 1, before.chars().count() + 1)
    }

    /// Replaces the selections (the primary one is scrolled into view).
    pub fn set_selections(&mut self, selections: Selections, cx: &mut Context<Self>) {
        let text = self.buffer.read(cx).text().clone();
        self.selections = selections.clamp(&text);
        self.marked.clear();
        self.goal = None;
        self.buffer.update(cx, |b, _| b.break_group());
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// Another view (or the disk) changed the text: the carets, the marked text and the find
    /// matches follow it.
    fn on_buffer_edited(&mut self, event: &BufferEdited, cx: &mut Context<Self>) {
        let mine = event.origin == Some(cx.entity_id());
        if !mine {
            self.selections = self.selections.map(&event.transaction);
            self.marked = self
                .marked
                .iter()
                .map(|r| {
                    event.transaction.map_offset(r.start, Bias::Left)
                        ..event.transaction.map_offset(r.end, Bias::Right)
                })
                .collect();
            self.goal = None;
        }
        // Drawn matches follow the edit at once; the search runs again in the background.
        let matches = std::mem::take(&mut self.find.result.matches);
        self.find.result.matches = matches
            .into_iter()
            .map(|m| {
                event.transaction.map_offset(m.start, Bias::Right)
                    ..event.transaction.map_offset(m.end, Bias::Left)
            })
            .filter(|m| m.start < m.end)
            .collect();
        if !self.find.query.text.is_empty() && self.searched_version < event.version {
            self.search(SearchMode::Refresh, cx);
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    // ---- scrolling ---------------------------------------------------------------------------

    /// Brings 1-based `line` (and `column`) into view, about a third from the top, once the text
    /// is in and the view has been laid out, and puts the caret there.
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
        if let Ok(content) = text.line_content_range(index) {
            let at = match column {
                Some(column) => {
                    let line_text = text.slice(content.clone()).unwrap_or_default();
                    let byte = line_text
                        .char_indices()
                        .nth(column.saturating_sub(1))
                        .map_or(line_text.len(), |(at, _)| at);
                    content.start + byte
                }
                None => content.start,
            };
            self.selections = Selections::cursor(at);
            self.marked.clear();
            self.goal = None;
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
        self.scroll_y = self.scroll_y.clamp(-self.inset, max_y.max(0.0));
        self.inset = (-self.scroll_y).max(0.0);
        let visible_w = f32::from(layout.bounds.right() - layout.text_left).max(0.0);
        let max_x = (self.widest as f32 * f32::from(layout.cell_width) - visible_w * 0.5).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);
    }

    /// Asks the view to keep its text where it is on the screen if its top moves up at the
    /// next frame (a bar above it went away): the scroll follows, and when the file is already
    /// at its top the first line stays put under a blank band that the next scroll takes back.
    pub fn keep_text_in_place(&mut self) {
        self.keep_in_place = true;
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

    /// Starts where `other` (a view of the same file) is: its primary caret and its scroll; the
    /// caret is brought into view at the first frame, at the size this view is drawn. Vim's
    /// `:sp`, a file opened in a split.
    pub fn start_like(&mut self, other: &Entity<CodeView>, cx: &mut Context<Self>) {
        let (head, scroll_y) = {
            let other = other.read(cx);
            (other.selections.primary().head, other.scroll_y)
        };
        let text = self.buffer.read(cx).text().clone();
        self.selections = Selections::cursor(head).clamp(&text);
        self.marked.clear();
        self.goal = None;
        self.scroll_y = scroll_y;
        self.autoscroll = true;
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    /// Brings the primary caret into view at the next frame, at the size the view is drawn then
    /// (a split that just made the view smaller).
    pub fn reveal_caret_next_frame(&mut self, cx: &mut Context<Self>) {
        self.autoscroll = true;
        cx.notify();
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

    /// Lines one page moves.
    pub(crate) fn page(&self) -> usize {
        self.layout.map_or(20, |l| l.rows.saturating_sub(2).max(1))
    }

    /// Scrolls as little as it takes to show the primary caret (with a line and a few columns
    /// of margin); waits for the next frame when there is no layout yet.
    pub(crate) fn reveal_head(&mut self, cx: &App) {
        let Some(layout) = self.layout else {
            self.autoscroll = true;
            return;
        };
        self.autoscroll = false;
        let text = self.buffer.read(cx).text();
        let head = self.selections.primary().head.min(text.len());
        let Ok(line) = text.line_of_byte(head) else {
            return;
        };
        let line_h = f32::from(layout.line_height);
        let view_h = f32::from(layout.bounds.size.height);
        let top = line as f32 * line_h;
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if top + line_h > self.scroll_y + view_h {
            self.scroll_y = top + line_h - view_h;
        }
        let start = text.line_start(line).unwrap_or(0);
        let window = line_window(text, start..head, usize::MAX);
        let col = layout::line_columns(&window);
        let cell = f32::from(layout.cell_width);
        let x = col as f32 * cell;
        let visible_w = f32::from(layout.bounds.right() - layout.text_left);
        self.widest = self.widest.max(col + 1);
        let margin = 4.0 * cell;
        if x < self.scroll_x + margin {
            self.scroll_x = (x - margin).max(0.0);
        } else if x > self.scroll_x + visible_w - margin {
            self.scroll_x = x - visible_w + margin;
        }
        self.clamp_scroll(cx);
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

    // ---- geometry -----------------------------------------------------------------------------

    /// The line and fractional display column under window position `position` (the line
    /// clamped into the text).
    fn cell_at(&self, position: Point<Pixels>, text: &Text) -> Option<(usize, f32)> {
        let layout = self.layout?;
        let line_h = f32::from(layout.line_height);
        let y = f32::from(position.y - layout.bounds.top()) + self.scroll_y;
        let line = if y < 0.0 {
            0
        } else {
            ((y / line_h).floor() as usize).min(text.line_count().saturating_sub(1))
        };
        let x = f32::from(position.x - layout.text_left) + self.scroll_x;
        Some((line, (x / f32::from(layout.cell_width)).max(0.0)))
    }

    /// The byte offset under window position `position` (clamped into the text).
    pub(crate) fn offset_at(&self, position: Point<Pixels>, cx: &App) -> Option<usize> {
        let layout = self.layout?;
        let text = self.buffer.read(cx).text();
        let y = f32::from(position.y - layout.bounds.top()) + self.scroll_y;
        if y < 0.0 {
            return Some(0);
        }
        if (y / f32::from(layout.line_height)).floor() as usize >= text.line_count() {
            return Some(text.len());
        }
        let (line, x_cols) = self.cell_at(position, text)?;
        let content = text.line_content_range(line).ok()?;
        let window = line_window(text, content.clone(), x_cols as usize + 2);
        Some(content.start + layout::byte_of_col(&window, x_cols))
    }

    /// The window position of the top left corner of the cell where byte `offset` is drawn.
    pub fn point_of(&self, offset: usize, cx: &App) -> Option<Point<Pixels>> {
        let layout = self.layout?;
        let text = self.buffer.read(cx).text();
        let offset = text.floor_char_boundary(offset.min(text.len()));
        let line = text.line_of_byte(offset).ok()?;
        let start = text.line_start(line).ok()?;
        let col = layout::line_columns(&line_window(text, start..offset, usize::MAX));
        let x = layout.text_left + layout.cell_width * col as f32 - gpui::px(self.scroll_x);
        let y = layout.bounds.top() + layout.line_height * line as f32 - gpui::px(self.scroll_y);
        Some(gpui::point(x, y))
    }

    // ---- the mouse ----------------------------------------------------------------------------

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
        self.buffer.update(cx, |b, _| b.break_group());
        self.marked.clear();
        self.goal = None;
        let modifiers = event.modifiers;
        if modifiers.alt && modifiers.shift {
            // A block of columns from here.
            if let Some((line, col)) = self.cell_at(event.position, &text) {
                let col = col.round() as usize;
                self.drag = Some(Drag::Column { line, col });
                self.selections = column_selections(&text, (line, col), (line, col));
            }
        } else if modifiers.alt {
            // ⌥-click adds a caret, or takes away the one that is there.
            let existing = self
                .selections
                .iter()
                .position(|s| s.range().contains(&at) || s.head == at);
            match existing {
                Some(index) if self.selections.len() > 1 => {
                    self.selections.remove(index);
                    self.drag = None;
                }
                _ => {
                    let others = self.selections.clone();
                    self.selections = with_dragged(Some(&others), Selection::cursor(at));
                    self.drag = Some(Drag::Chars {
                        anchor: at,
                        others: Some(others),
                    });
                }
            }
        } else {
            match event.click_count {
                2 => {
                    let word = word_at(&text, at);
                    self.selections = Selections::single(Selection::new(word.start, word.end));
                    self.drag = Some(Drag::Words {
                        origin: word,
                        others: None,
                    });
                }
                n if n >= 3 => {
                    let line = line_at(&text, at);
                    self.selections = Selections::single(Selection::new(line.start, line.end));
                    self.drag = Some(Drag::Lines {
                        origin: line,
                        others: None,
                    });
                }
                _ => {
                    let (anchor, at) = if modifiers.shift {
                        (self.selections.primary().anchor, at)
                    } else {
                        let at = self.vim_click_target(&text, at);
                        (at, at)
                    };
                    self.selections = Selections::single(Selection::new(anchor, at));
                    self.drag = Some(Drag::Chars {
                        anchor,
                        others: None,
                    });
                }
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
        let selections = match drag {
            Drag::Chars { anchor, others } => {
                with_dragged(others.as_ref(), Selection::new(anchor, at))
            }
            Drag::Words { origin, others } => {
                with_dragged(others.as_ref(), extend(origin, word_at(&text, at)))
            }
            Drag::Lines { origin, others } => {
                with_dragged(others.as_ref(), extend(origin, line_at(&text, at)))
            }
            Drag::Column { line, col } => {
                let Some((now_line, now_col)) = self.cell_at(event.position, &text) else {
                    return;
                };
                column_selections(&text, (line, col), (now_line, now_col.round() as usize))
            }
        };
        if self.selections != selections {
            self.selections = selections;
            self.reveal_head(cx);
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
        }
    }

    pub(crate) fn mouse_up(&mut self, _event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.drag = None;
    }

    /// Selects `range` (byte offsets) and scrolls it into view.
    pub fn select(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.selections = Selections::single(Selection::new(range.start, range.end));
        self.marked.clear();
        self.goal = None;
        self.buffer.update(cx, |b, _| b.break_group());
        self.scroll_into_view(range.start, cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    // ---- an agent's changes (E-10) -----------------------------------------------------------

    /// The agent's hunk the primary caret is in.
    pub fn agent_hunk_at_caret(&self, cx: &App) -> Option<usize> {
        let head = self.selections.primary().head;
        self.buffer.read(cx).agent_hunk_at(head)
    }

    /// *Reject* on the hunk at the caret.
    pub fn reject_agent_hunk_at_caret(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.agent_hunk_at_caret(cx) {
            self.reject_agent_hunk(index, cx);
        }
    }

    /// *Reject* on hunk `index`: the base's text goes back in its place as one undo step (undo
    /// brings the agent's text and this view's carets back). Returns whether it did.
    pub fn reject_agent_hunk(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let id = cx.entity_id();
        let before = self.selections.clone();
        let applied = self
            .buffer
            .update(cx, |b, cx| b.reject_agent_hunk(index, id, Some(before), cx));
        let Some(applied) = applied else {
            return false;
        };
        let text = self.buffer.read(cx).text().clone();
        if let Some(after) = applied.transaction.selection_after() {
            self.selections = after.clamp(&text);
        }
        self.marked.clear();
        self.goal = None;
        self.reveal_head(cx);
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        true
    }

    /// Moves the caret to the start of the agent's next hunk (wrapping around).
    pub fn next_agent_hunk(&mut self, cx: &mut Context<Self>) {
        self.step_agent_hunk(true, cx);
    }

    /// Moves the caret to the start of the agent's previous hunk (wrapping around).
    pub fn previous_agent_hunk(&mut self, cx: &mut Context<Self>) {
        self.step_agent_hunk(false, cx);
    }

    fn step_agent_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let hunks = self.buffer.read(cx).agent_hunks();
        if hunks.is_empty() {
            return;
        }
        let head = self.selections.primary().head;
        let index = if forward {
            hunks.iter().position(|h| h.bytes.start > head).unwrap_or(0)
        } else {
            hunks
                .iter()
                .rposition(|h| h.bytes.start < head && !h.bytes.contains(&head))
                .unwrap_or(hunks.len() - 1)
        };
        self.reveal_agent_hunk(index, cx);
    }

    /// Puts the caret at the start of hunk `index` and scrolls it about a third from the top
    /// (*Follow agent*, the hunk arrows).
    pub fn reveal_agent_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let start = {
            let buffer = self.buffer.read(cx);
            let Some(hunk) = buffer.agent_hunks().get(index).cloned() else {
                return;
            };
            buffer
                .text()
                .line_of_byte(hunk.bytes.start.min(buffer.text().len()))
        };
        let Ok(line) = start else {
            return;
        };
        self.buffer.update(cx, |b, _| b.break_group());
        self.reveal_line(line + 1, None, cx);
        cx.emit(CodeViewEvent::Changed);
    }

    // ---- find and replace -----------------------------------------------------------------

    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    /// Searches the file for `query` (in the background); the first match at or after the top
    /// of the view becomes current, is selected and scrolled into view.
    pub fn set_find_query(&mut self, query: FindQuery, cx: &mut Context<Self>) {
        if query == self.find.query && !self.find.searching && self.find.generation > 0 {
            return;
        }
        self.find.query = query;
        self.find.current = None;
        self.search(SearchMode::Query, cx);
    }

    pub(crate) fn search(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        self.find.generation += 1;
        let generation = self.find.generation;
        let query = self.find.query.clone();
        if query.text.is_empty() {
            self.find.result = find::FindResult::default();
            self.find.searching = false;
            self.find_task = None;
            cx.emit(CodeViewEvent::Changed);
            cx.notify();
            return;
        }
        self.find.searching = true;
        let (text, version) = {
            let buffer = self.buffer.read(cx);
            (buffer.text().clone(), buffer.version())
        };
        self.searched_version = version;
        let search = cx.background_spawn(async move { find::run(&text, &query) });
        self.find_task = Some(cx.spawn(async move |this, cx| {
            let result = search.await;
            this.update(cx, |this, cx| {
                if this.find.generation != generation {
                    return;
                }
                this.take_search(result, version, mode, cx);
            })
            .ok();
        }));
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    fn take_search(
        &mut self,
        result: find::FindResult,
        version: u64,
        mode: SearchMode,
        cx: &mut Context<Self>,
    ) {
        let previous = self.find.current_match().map(|m| m.start);
        self.find.searching = false;
        self.find.result = result;
        self.find.version = version;
        match mode {
            SearchMode::Query => {
                let top = self.top_offset(cx);
                self.find.current = self.find.first_from(top);
                if let Some(current) = self.find.current_match() {
                    self.select_match(current, cx);
                }
            }
            SearchMode::Refresh => {
                self.find.current = previous.and_then(|at| self.find.first_from(at));
            }
            SearchMode::AfterReplace(offset) => {
                self.find.current = self.find.first_from(offset);
                if let Some(current) = self.find.current_match() {
                    self.select_match(current, cx);
                }
            }
        }
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
    }

    fn select_match(&mut self, found: Range<usize>, cx: &mut Context<Self>) {
        self.selections = Selections::single(Selection::new(found.start, found.end));
        self.marked.clear();
        self.goal = None;
        self.scroll_into_view(found.start, cx);
    }

    /// Whether a search is running (tests wait on it).
    pub fn is_searching(&self) -> bool {
        self.find.searching
    }

    /// Moves to the next (`forward`) or previous match, selects it and scrolls it into view.
    pub fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(found) = self.find.step(forward) {
            self.buffer.update(cx, |b, _| b.break_group());
            self.select_match(found, cx);
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

    /// Replaces the current match with `replacement` (one undo step) and moves to the next one.
    /// Returns whether something was replaced.
    pub fn replace_current(&mut self, replacement: &str, cx: &mut Context<Self>) -> bool {
        let version = self.buffer.read(cx).version();
        let Some(current) = self.find.current_match() else {
            return false;
        };
        if self.find.version != version || self.find.searching {
            // The matches are of an older text; the next answer is current.
            return false;
        }
        let Ok(transaction) = Transaction::replace(current.clone(), replacement) else {
            return false;
        };
        let before = self.selections.clone();
        let after = Selections::cursor(current.start + replacement.len());
        let transaction = transaction.with_selections(Some(before), Some(after.clone()));
        let id = cx.entity_id();
        let applied = self.buffer.update(cx, |b, cx| {
            b.edit(transaction, crate::buffer::Coalesce::Step, id, cx)
        });
        if applied.is_none() {
            return false;
        }
        self.selections = after;
        self.marked.clear();
        // The edit started a refresh; this one replaces it and goes on from the replacement.
        self.search(
            SearchMode::AfterReplace(current.start + replacement.len()),
            cx,
        );
        true
    }

    /// Replaces every match of the find query with `replacement` as **one** transaction (one
    /// undo step). The text is searched again first, so the matches are those of the text as it
    /// is now. Returns how many were replaced.
    pub fn replace_all(&mut self, replacement: &str, cx: &mut Context<Self>) -> usize {
        let query = self.find.query.clone();
        if query.text.is_empty() {
            return 0;
        }
        let text = self.buffer.read(cx).text().clone();
        let found = find::run_all(&text, &query);
        if found.is_empty() {
            return 0;
        }
        let edits = found
            .iter()
            .map(|range| hutan_buffer::Edit::new(range.clone(), replacement));
        let Ok(transaction) = Transaction::new(edits) else {
            return 0;
        };
        let before = self.selections.clone();
        let after = before.map(&transaction);
        let transaction = transaction.with_selections(Some(before), Some(after.clone()));
        let id = cx.entity_id();
        let applied = self.buffer.update(cx, |b, cx| {
            b.edit(transaction, crate::buffer::Coalesce::Step, id, cx)
        });
        if applied.is_none() {
            return 0;
        }
        self.selections = after;
        self.marked.clear();
        cx.emit(CodeViewEvent::Changed);
        cx.notify();
        found.len()
    }

    /// Emits [`CodeViewEvent::FocusLost`] when the view loses the keyboard or its window stops
    /// being active (made once, at the first render).
    fn watch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_watch.is_empty() {
            return;
        }
        let focus = self.focus.clone();
        self.focus_watch
            .push(cx.on_focus_out(&focus, window, |_, _, _, cx| {
                cx.emit(CodeViewEvent::FocusLost)
            }));
        self.focus_watch.push(
            cx.observe_window_activation(window, move |this, window, cx| {
                if !window.is_window_active() && this.focus.is_focused(window) {
                    cx.emit(CodeViewEvent::FocusLost);
                }
            }),
        );
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

/// `others` with `dragged` added as the primary selection (or `dragged` alone).
fn with_dragged(others: Option<&Selections>, dragged: Selection) -> Selections {
    match others {
        Some(others) => {
            let mut all = others.clone();
            if all.push(dragged, true).is_err() {
                return others.clone();
            }
            all
        }
        None => Selections::single(dragged),
    }
}

/// One selection per line from `from` to `to` (line, display column), each covering the columns
/// between the two: a column (block) selection. Lines shorter than the left column get a caret
/// at their end.
pub(crate) fn column_selections(
    text: &Text,
    from: (usize, usize),
    to: (usize, usize),
) -> Selections {
    let (first, last) = (from.0.min(to.0), from.0.max(to.0));
    let last = last.min(first + MAX_COLUMN_LINES - 1);
    let ranges: Vec<Selection> = (first..=last)
        .filter_map(|line| {
            let content = text.line_content_range(line).ok()?;
            let window = line_window(text, content.clone(), from.1.max(to.1) + 2);
            let anchor = content.start + layout::byte_at_col(&window, from.1);
            let head = content.start + layout::byte_at_col(&window, to.1);
            Some(Selection::new(anchor, head))
        })
        .collect();
    // The line the pointer is on is the primary one.
    let primary = if to.0 >= from.0 {
        ranges.len().saturating_sub(1)
    } else {
        0
    };
    Selections::new(ranges, primary).unwrap_or_else(|_| Selections::cursor(0))
}

/// The selection a drag makes from the unit `origin` to the unit `now`.
fn extend(origin: Range<usize>, now: Range<usize>) -> Selection {
    if now.start < origin.start {
        Selection::new(origin.end, now.start)
    } else {
        Selection::new(origin.start, now.end.max(origin.end))
    }
}

/// The word (or run of other characters) around byte `at`.
pub(crate) fn word_at(text: &Text, at: usize) -> Range<usize> {
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.watch_focus(window, cx);
        let root = div()
            .id("code-view")
            .key_context(self.key_context())
            .track_focus(&self.focus)
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.vim_key_down(event, cx) {
                    cx.stop_propagation();
                }
            }));
        crate::actions::register(root, cx).child(CodeElement::new(cx.entity()))
    }
}

impl CodeView {
    /// `CodeView Editor`, and `vim_mode = normal | insert | visual` while Vim is on: the
    /// editor's own bindings are bound outside Normal and Visual so they never shadow Vim.
    fn key_context(&self) -> KeyContext {
        let mut context = KeyContext::default();
        context.add(crate::KEY_CONTEXT);
        context.add(crate::EDITOR_CONTEXT);
        if let Some(mode) = self.vim_mode() {
            context.set(
                crate::VIM_MODE_KEY,
                match mode {
                    hutan_vim::Mode::Normal => "normal",
                    hutan_vim::Mode::Insert => "insert",
                    hutan_vim::Mode::Visual
                    | hutan_vim::Mode::VisualLine
                    | hutan_vim::Mode::VisualBlock => "visual",
                },
            );
        }
        context
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
        assert_eq!(extend(4..7, 10..12), Selection::new(4, 12));
        assert_eq!(extend(4..7, 0..2), Selection::new(7, 0));
    }

    #[test]
    fn a_huge_line_is_cut_for_drawing() {
        let long = "x".repeat(10_000);
        let text = Text::from(long.as_str());
        assert_eq!(line_window(&text, 0..10_000, 100).len(), 464);
        assert_eq!(line_window(&text, 0..10, 100).len(), 10);
    }

    #[test]
    fn a_column_selection_takes_the_same_columns_of_every_line() {
        let text = Text::from("abcdef\nab\nabcdef\n");
        let block = column_selections(&text, (0, 1), (2, 4));
        let ranges: Vec<_> = block.iter().map(|s| s.range()).collect();
        // Line 1 is shorter: it gets the part of the columns it has.
        assert_eq!(ranges, vec![1..4, 8..9, 11..14]);
        assert_eq!(block.primary().range(), 11..14);
    }
}
