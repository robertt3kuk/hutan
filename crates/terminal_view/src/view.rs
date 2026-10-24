//! The terminal view entity: state, input handling, IME, selection, scrollback viewing.

use std::cell::Ref;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    KeyDownEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, Task, UTF16Selection, Window, actions, div,
    point, px,
};
use hutan_protocol::blocks::{Block, BlockPart, BlockState};
use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::{CursorShape, MouseMode, Palette, Row, TerminalRender};

use crate::TerminalBackend;
use crate::blocks::{self, DisplayRow};
use crate::convert;
use crate::element::TerminalElement;
use crate::find::{FindHighlights, FindSpan};
use crate::keys::{KeyOptions, Mods, encode_key};
use crate::links::{self, Candidate, LinkTarget};
use crate::model::{Applied, HISTORY_PAGE, TerminalModel, row_cells, row_width};
use crate::mouse::{
    Button, MouseAction, MouseEvent, WheelDirection, alternate_scroll, encode_mouse,
};
use crate::paste::{self, paste_bytes};
use crate::selection::{Point as GridPoint, Selection, SelectionKind, selected_text};
use crate::settings::{CursorStyle, TerminalViewSettings};
use crate::shape::{RowCache, hsla};
use crate::shared::{ModelRef, SharedEvent, SharedTerminal};
use crate::theme::{DefaultColors, Theme};

actions!(
    hutan_terminal_view,
    [
        /// Copies the selection to the clipboard.
        Copy,
        /// Pastes the clipboard into the terminal.
        Paste,
        /// Scrolls to the previous prompt and highlights its block.
        JumpToPreviousPrompt,
        /// Scrolls to the next prompt and highlights its block.
        JumpToNextPrompt,
        /// Selects the output of the current block.
        SelectBlockOutput,
        /// Copies the output of the current block (works beyond the loaded scrollback).
        CopyBlockOutput,
        /// Copies the command of the current block.
        CopyBlockCommand,
        /// Folds or unfolds the output of the current block.
        ToggleBlockCollapse,
        /// Types the current block's command into the session again and runs it.
        RerunBlock,
        /// Sends the current block to an agent chat: the view emits
        /// [`TerminalViewEvent::SendBlockToAgent`] and its owner does the rest.
        SendBlockToAgent
    ]
);

/// Key context of the view's root element, for keymap bindings.
pub const KEY_CONTEXT: &str = "HutanTerminal";

/// Binds ⌘C and ⌘V to [`Copy`] and [`Paste`], and the block keys (⌘↑ ⌘↓ ⌘⇧A ⌘⇧C) in the view's
/// key context. Optional: the view also handles these chords itself when no binding claims them.
pub fn bind_default_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-up", JumpToPreviousPrompt, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-down", JumpToNextPrompt, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-a", SelectBlockOutput, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-c", CopyBlockOutput, Some(KEY_CONTEXT)),
    ]);
}

const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);
/// How long *Terminal: Redraw* keeps the application at the nudged size before the real one is
/// sent back: long enough for a shell to see the change and repaint.
const REDRAW_NUDGE: Duration = Duration::from_millis(150);
const BLINK_INTERVAL: Duration = Duration::from_millis(530);
/// Blink phases a focused cursor goes through after the last activity (typing, output, focus),
/// about 5 s like Alacritty's `blinking_timeout`; then it stays solid. An unbounded blink would
/// repaint the window twice a second for as long as the app is open, which is the whole idle CPU
/// budget (docs/perf.md, B3).
const BLINK_TICKS: u32 = 10;
/// Time between two phases of the running-block indicator.
const ANIMATION_TICK: Duration = Duration::from_millis(140);
/// Phases of one cycle of the running-block indicator.
pub(crate) const ANIMATION_PHASES: u8 = 8;
/// Height of the "N new lines ↓" pill.
pub(crate) const JUMP_PILL_HEIGHT: f32 = 24.0;
/// Lines one wheel event may scroll.
const MAX_WHEEL_LINES: f32 = 64.0;
/// Group name of the drop overlay (`group_drag_over`).
const DROP_GROUP: &str = "hutan-terminal-drop";
/// What a pasted image says: a terminal takes text.
pub const IMAGE_PASTE_NOTICE: &str =
    "Images can't be pasted into a terminal — attach them in a chat";
/// Lines of a pending paste the confirmation previews.
const PASTE_PREVIEW_LINES: usize = 6;
/// Hyperlink schemes ⌘-click opens; anything else in an OSC 8 target is untrusted.
const OPENABLE_SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

/// Connection state the owner reports; non-live states disable input and show a banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewStatus {
    Live,
    Reconnecting,
    /// The session ended; the text says how.
    Ended(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The terminal title changed (`None` = cleared).
    TitleChanged(Option<String>),
    /// The user asked to send block `block` (its command and output) to an agent chat. The
    /// view has no chat to send to: the owner fetches the text (`terminal.block_text`) and
    /// attaches it.
    SendBlockToAgent { block: u64 },
    /// ⌘-click on a link: an OSC 8 hyperlink (`http(s)` and `mailto` only) or a URL or an
    /// existing `path[:line[:col]]` found in the text. The view opens nothing itself: its owner
    /// decides what opens it (the browser, the system, an editor).
    OpenLink(LinkTarget),
    /// Something the user should be told that the view cannot show itself (a paste of an image):
    /// the owner shows it as a notice.
    Notice(String),
}

/// How the user answered the multi-line paste confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteChoice {
    /// Paste the text as it is: every line runs as soon as it arrives.
    Paste,
    /// Paste it as one line (line breaks become spaces): nothing runs until Enter.
    OneLine,
    /// Paste nothing.
    Cancel,
}

/// What the tooltip over a link in the text says.
pub const LINK_TOOLTIP: &str = "⌘-click to open";

/// Rows of one soft-wrapped line the link detector reads on each side of the hovered row.
const LINK_SCAN_ROWS: i64 = 16;

/// The columns `(absolute line, first column, end column)` a link covers on one row.
pub type Segment = (i64, u16, u16);

/// A link found in plain text under the pointer: what ⌘-click opens and the cells it underlines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Detected {
    pub target: LinkTarget,
    /// `(absolute line, first column, end column)`; one entry per row the text spans.
    pub segments: Vec<Segment>,
}

impl Detected {
    /// The underlined columns on `line`.
    pub fn columns_on(&self, line: i64) -> Option<(u16, u16)> {
        self.segments
            .iter()
            .find(|(l, ..)| *l == line)
            .map(|(_, from, to)| (*from, *to))
    }

    fn covers(&self, line: i64, col: u16) -> bool {
        self.columns_on(line)
            .is_some_and(|(from, to)| from <= col && col < to)
    }
}

/// Grid geometry from the last layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Layout {
    pub bounds: Bounds<Pixels>,
    pub cell: Size<Pixels>,
    pub cols: u16,
    pub rows: u16,
    pub scale: f32,
}

impl Layout {
    /// Cell under `position` (window coordinates), clamped to the grid.
    pub fn cell_at(&self, position: Point<Pixels>) -> (u16, u16) {
        let x = (position.x - self.bounds.origin.x) / self.cell.width;
        let y = (position.y - self.bounds.origin.y) / self.cell.height;
        let clamp =
            |v: f32, n: u16| (v.floor().max(0.0) as u32).min(u32::from(n.max(1)) - 1) as u16;
        (clamp(y, self.rows), clamp(x, self.cols))
    }
}

#[derive(Debug, Clone)]
struct Marked {
    text: String,
    /// Selected range inside `text`, in UTF-16 units.
    selected: Range<usize>,
}

/// How a view relates to its terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Takes focus and input, reports its size to the terminal.
    Interactive,
    /// A read-only miniature (overview card): never resizes the terminal, ignores keys and
    /// mouse, and shows the newest rows that fit.
    Preview,
}

pub struct TerminalView {
    pub(crate) backend: Rc<dyn TerminalBackend>,
    pub(crate) settings: TerminalViewSettings,
    shared: Entity<SharedTerminal>,
    pub(crate) model: ModelRef,
    pub(crate) mode: ViewMode,
    pub(crate) theme: Theme,
    /// Mirror of the shared status (no context needed to read it).
    pub(crate) status: ViewStatus,
    focus_handle: FocusHandle,

    /// Lines the viewport is scrolled back into history; 0 follows the live screen.
    pub(crate) scroll_offset: u32,
    /// Lines that arrived below the viewport while it was scrolled back (the "N new lines ↓"
    /// pill). Only ever lowered by scrolling towards the live screen: the lines the reader has
    /// scrolled past are seen. See [`Self::set_scroll_offset`].
    unseen_lines: u32,
    scroll_accum: f32,
    history_in_flight: HashSet<u32>,
    /// History lines from this one on do not exist (an empty page came back).
    history_end: Option<u32>,

    pub(crate) selection: Option<Selection>,
    selecting: bool,
    /// Button held while the application receives mouse reports.
    reporting: Option<Button>,
    last_motion_cell: Option<(u16, u16)>,
    pub(crate) hover_cell: Option<(u16, u16)>,
    pub(crate) cmd_held: bool,
    /// The link in plain text under the pointer while ⌘ is held (found on hover, never per
    /// frame; dropped when the output changes).
    pub(crate) hover_detected: Option<Detected>,
    /// A multi-line paste waiting for the user's answer (the clipboard text as it was).
    pending_paste: Option<String>,

    marked: Option<Marked>,

    pub(crate) layout: Option<Layout>,
    reported_size: Option<TerminalSize>,
    wanted_size: Option<TerminalSize>,
    resize_task: Option<Task<()>>,
    /// The second half of [`Self::redraw`].
    redraw_task: Option<Task<()>>,

    pub(crate) blink_on: bool,
    /// The block the last prompt jump (or output selection) landed on.
    pub(crate) highlight: Option<u64>,
    /// Phase of the running-block indicator, `0..ANIMATION_PHASES`.
    pub(crate) animation_phase: u8,
    animation_task: Option<Task<()>>,
    /// What the last paint drew of the overlay.
    pub(crate) painted_overlay: blocks::OverlayReport,
    /// The find matches to draw (see [`Self::set_find_highlights`]).
    pub(crate) find: Option<FindHighlights>,
    /// What the last paint drew of them: `(grid row, from, to, current)`.
    pub(crate) painted_find: Vec<(u16, u16, u16, bool)>,
    pub(crate) rows: RowCache,
    /// Keystroke → paint correlation, fed only while `HUTAN_TRACE_LATENCY` is on.
    latency: crate::trace::LatencyTracker,
    /// How many times this view was painted (tests: a cached view must repaint when output
    /// arrives).
    paints: u64,
    /// Cursor position after the previous applied frame (tracing only).
    traced_cursor: Option<(u16, u16)>,

    _subscriptions: Vec<Subscription>,
    /// The running blink timer, while there is one (see [`BLINK_TICKS`]).
    blink_task: Option<Task<()>>,
    /// Phases left before the cursor goes solid; reset by activity.
    blink_ticks_left: u32,
    /// Whether the view's focus handle has focus (mirrors the focus events).
    focused: bool,
}

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView")
            .field("model", &*self.model.get())
            .field("status", &self.status)
            .field("mode", &self.mode)
            .field("scroll_offset", &self.scroll_offset)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    /// A view with a terminal model of its own: for owners that have one view per terminal.
    pub fn new(
        backend: Rc<dyn TerminalBackend>,
        settings: TerminalViewSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shared = cx.new(|_| SharedTerminal::new());
        Self::with_shared(shared, backend, settings, ViewMode::Interactive, window, cx)
    }

    /// A view that draws `shared`; any number of views can draw one terminal. `backend` carries
    /// the acks and history requests (and, for [`ViewMode::Interactive`], input and resizes).
    pub fn with_shared(
        shared: Entity<SharedTerminal>,
        backend: Rc<dyn TerminalBackend>,
        settings: TerminalViewSettings,
        mode: ViewMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let interactive = mode == ViewMode::Interactive;
        if interactive {
            backend.set_default_colors(DefaultColors::from(&settings.palette));
        }
        let mut theme = Theme::new(settings.palette);
        let (model, status) = {
            let shared = shared.read(cx);
            (shared.model_ref(), shared.status().clone())
        };
        if model.get().is_ready() {
            theme.set_overrides(model.get().palette());
        }
        let mut subscriptions = vec![cx.subscribe(&shared, Self::on_shared_event)];
        if interactive {
            subscriptions.extend([
                cx.on_focus_in(&focus_handle, window, |this, _window, cx| {
                    this.focus_changed(true, cx);
                }),
                cx.on_focus_out(&focus_handle, window, |this, _event, _window, cx| {
                    this.focus_changed(false, cx);
                }),
                cx.observe_window_activation(window, |this, _window, cx| {
                    this.blink_on = true;
                    this.keep_blinking(cx);
                    cx.notify();
                }),
            ]);
        }
        Self {
            backend,
            settings,
            shared,
            model,
            mode,
            theme,
            status,
            focus_handle,
            scroll_offset: 0,
            unseen_lines: 0,
            scroll_accum: 0.0,
            history_in_flight: HashSet::new(),
            history_end: None,
            selection: None,
            selecting: false,
            reporting: None,
            last_motion_cell: None,
            hover_cell: None,
            cmd_held: false,
            hover_detected: None,
            pending_paste: None,
            marked: None,
            layout: None,
            reported_size: None,
            wanted_size: None,
            resize_task: None,
            redraw_task: None,
            blink_on: true,
            highlight: None,
            animation_phase: 0,
            animation_task: None,
            painted_overlay: blocks::OverlayReport::default(),
            find: None,
            painted_find: Vec::new(),
            rows: RowCache::default(),
            latency: crate::trace::LatencyTracker::default(),
            paints: 0,
            traced_cursor: None,
            _subscriptions: subscriptions,
            blink_task: None,
            blink_ticks_left: 0,
            focused: false,
        }
    }

    // ---- owner API -------------------------------------------------------------------------

    /// The terminal this view draws.
    pub fn shared(&self) -> &Entity<SharedTerminal> {
        &self.shared
    }

    /// Applies a snapshot or frame from the daemon to the shared model. The frame is acked once a
    /// view has painted it.
    pub fn apply_render(&mut self, render: TerminalRender, cx: &mut Context<Self>) {
        self.shared
            .update(cx, |shared, cx| shared.apply_render(render, cx));
    }

    /// Delivers a `terminal.history` answer for a [`TerminalBackend::request_history`] call:
    /// `from_line` 0 is the line directly above the viewport, `rows` are newest first.
    pub fn apply_history(&mut self, from_line: u32, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.shared
            .update(cx, |shared, cx| shared.apply_history(from_line, rows, cx));
    }

    /// Sets the status every view of this terminal shows.
    pub fn set_status(&mut self, status: ViewStatus, cx: &mut Context<Self>) {
        self.shared
            .update(cx, |shared, cx| shared.set_status(status, cx));
    }

    pub fn update_settings(&mut self, settings: TerminalViewSettings, cx: &mut Context<Self>) {
        if settings == self.settings {
            return;
        }
        if settings.palette != self.settings.palette {
            self.theme.set_base(settings.palette);
            if self.mode == ViewMode::Interactive {
                self.backend
                    .set_default_colors(DefaultColors::from(&settings.palette));
            }
        }
        self.settings = settings;
        self.blink_on = true;
        self.keep_blinking(cx);
        self.sync_animation(cx);
        cx.notify();
    }

    fn on_shared_event(
        &mut self,
        _shared: Entity<SharedTerminal>,
        event: &SharedEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SharedEvent::Applied(applied) => {
                self.hover_detected = None;
                if applied.snapshot {
                    self.theme.set_overrides(self.model.get().palette());
                }
                self.after_apply(applied);
                if applied.title_changed {
                    cx.emit(TerminalViewEvent::TitleChanged(
                        self.model.get().title().map(str::to_owned),
                    ));
                }
                self.blink_on = true;
                self.keep_blinking(cx);
                if crate::trace::enabled() {
                    // An echo is visible as a replaced cursor row or a moved cursor (Enter).
                    let cursor = self.model.get().cursor();
                    let position = (cursor.row, cursor.col);
                    let moved = self.traced_cursor.replace(position) != Some(position);
                    self.latency.frame_applied(
                        std::time::Instant::now(),
                        applied.snapshot || moved || applied.dirty.contains(&cursor.row),
                    );
                }
            }
            SharedEvent::History { from_line, empty } => {
                self.history_in_flight.remove(from_line);
                if *empty {
                    self.history_end =
                        Some(self.history_end.map_or(*from_line, |e| e.min(*from_line)));
                }
                // A short page leaves lines missing: ask for the rest.
                self.ensure_history();
            }
            SharedEvent::Status => {
                let status = self.shared.read(cx).status().clone();
                if status == self.status {
                    return;
                }
                if status == ViewStatus::Live {
                    // Requests that were in flight when the connection dropped never answer.
                    self.history_in_flight.clear();
                    // The new connection has not seen our size.
                    self.reported_size = None;
                    self.wanted_size = None;
                }
                self.status = status;
                if self.status != ViewStatus::Live {
                    self.pending_paste = None;
                }
                self.reporting = None;
                self.selecting = false;
            }
            SharedEvent::Reset => {
                self.reset_history_view();
                self.highlight = None;
                self.theme.set_overrides(&Palette::default());
            }
            SharedEvent::Blocks => {
                if self
                    .highlight
                    .is_some_and(|id| self.model.get().blocks().get(id).is_none())
                {
                    self.highlight = None;
                }
                self.ensure_history();
            }
            SharedEvent::Collapsed => {}
        }
        self.sync_animation(cx);
        cx.notify();
    }

    fn after_apply(&mut self, applied: &Applied) {
        if applied.history_reset {
            let anchor = (applied.snapshot && applied.resized && self.scroll_offset > 0)
                .then(|| {
                    applied
                        .prev_viewport_top
                        .checked_sub(u64::from(self.scroll_offset))
                })
                .flatten();
            let unseen = self.unseen_lines;
            self.reset_history_view();
            if let Some(anchor) = anchor {
                // A resize reflows the daemon's hot margin but keeps its line numbers: the
                // line that was at the top of the view keeps its number (exactly so for lines
                // that do not wrap and for scrollback older than the margin, nearby otherwise)
                // and the view returns to it instead of the live screen. A line the daemon no
                // longer retains lands on the oldest one.
                let (top, max) = {
                    let model = self.model.get();
                    (model.viewport_top(), model.history_len())
                };
                let offset = top.saturating_sub(anchor).min(u64::from(max));
                self.scroll_offset = u32::try_from(offset).unwrap_or(max);
                self.unseen_lines = unseen;
            }
        } else if applied.history_grew > 0 && self.scroll_offset > 0 {
            // Stay on the same lines while output scrolls into history.
            self.scroll_offset =
                (self.scroll_offset + applied.history_grew).min(self.model.get().history_len());
            self.unseen_lines = self.unseen_lines.saturating_add(applied.history_grew);
        }
        let limit = self.model.get().history_len();
        self.set_scroll_offset(self.scroll_offset.min(limit));
        if applied.snapshot && applied.resized {
            self.selection = None;
            self.selecting = false;
        } else if !applied.snapshot && applied.history_grew == 0 {
            // Output rewrote lines under the selection: it no longer describes this text.
            let touched = {
                let model = self.model.get();
                self.selection
                    .and_then(|s| s.span(&model))
                    .is_some_and(|span| {
                        applied.dirty.iter().any(|row| {
                            let line = model.viewport_line(*row);
                            span.start.line <= line && line <= span.end.line
                        })
                    })
            };
            if touched {
                self.selection = None;
                self.selecting = false;
            }
        }
        self.ensure_history();
    }

    fn reset_history_view(&mut self) {
        self.scroll_offset = 0;
        self.unseen_lines = 0;
        self.scroll_accum = 0.0;
        self.history_in_flight.clear();
        self.history_end = None;
        self.selection = None;
        self.selecting = false;
    }

    // ---- accessors -------------------------------------------------------------------------

    /// The shared model. Do not hold the borrow across calls that apply renders.
    pub fn model(&self) -> Ref<'_, TerminalModel> {
        self.model.get()
    }

    pub fn status(&self) -> &ViewStatus {
        &self.status
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    pub fn title(&self) -> Option<String> {
        self.model.get().title().map(str::to_owned)
    }

    /// Columns and rows that fit the last laid-out bounds.
    pub fn grid_size(&self) -> Option<(u16, u16)> {
        self.layout.map(|l| (l.cols, l.rows))
    }

    /// Cell size in pixels from the last layout.
    pub fn cell_size(&self) -> Option<Size<Pixels>> {
        self.layout.map(|l| l.cell)
    }

    /// Lines scrolled back from the live screen.
    pub fn scroll_offset(&self) -> u32 {
        self.scroll_offset
    }

    /// Lines of output that arrived below the viewport since the reader scrolled back from the
    /// live screen and that they have not scrolled to yet; 0 at the live screen. The pill reads
    /// this.
    pub fn new_lines_below(&self) -> u32 {
        self.unseen_lines.min(self.scroll_offset)
    }

    /// Moves the viewport to `offset` lines back; lines scrolled past stop counting as new.
    fn set_scroll_offset(&mut self, offset: u32) {
        self.scroll_offset = offset;
        self.unseen_lines = self.unseen_lines.min(offset);
    }

    /// Whether the cursor is in the visible phase of its blink cycle.
    pub fn cursor_blink_visible(&self) -> bool {
        self.blink_on
    }

    pub fn marked_text(&self) -> Option<&str> {
        self.marked.as_ref().map(|m| m.text.as_str())
    }

    /// The selected text, if anything is selected.
    pub fn selection_text(&self) -> Option<String> {
        let model = self.model.get();
        let span = self.selection?.span(&model)?;
        let text = selected_text(&model, span);
        (!text.is_empty()).then_some(text)
    }

    // ---- internals shared with the element ----------------------------------------------------

    pub(crate) fn accepts_input(&self) -> bool {
        self.mode == ViewMode::Interactive && self.status == ViewStatus::Live
    }

    pub(crate) fn has_focus(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Absolute line shown in grid row `row` (a folded block's summary row: its first hidden
    /// line).
    pub(crate) fn line_for_row(&self, row: u16) -> i64 {
        match self.display().get(usize::from(row)) {
            Some(shown) => shown.line(),
            None => {
                self.model.get().viewport_line(0) + i64::from(row + self.row_offset())
                    - i64::from(self.scroll_offset)
            }
        }
    }

    /// Whether the block overlay is drawn: enabled, a render has arrived and the primary screen
    /// shows (vim, htop and other alternate-screen programs hide it).
    pub fn blocks_visible(&self) -> bool {
        let model = self.model.get();
        self.settings.blocks.enabled && model.is_ready() && !model.modes().alt_screen
    }

    /// Whether folds apply to this view: only an interactive view folds, and not while the
    /// application receives mouse reports (their coordinates are grid rows).
    fn folds_active(&self) -> bool {
        self.mode == ViewMode::Interactive
            && self.blocks_visible()
            && self.model.get().modes().mouse == MouseMode::None
            && !self.model.get().collapsed().is_empty()
    }

    /// What each grid row shows, top to bottom. The last row shows the line the scroll position
    /// names; folded blocks take one summary row each and older lines fill the rest.
    pub(crate) fn display(&self) -> Vec<DisplayRow> {
        let rows = usize::from(
            self.layout
                .map_or(self.model.get().screen_rows(), |l| l.rows),
        );
        let offset = self.row_offset();
        let model = self.model.get();
        let last = model.viewport_line(0) + i64::from(offset) + rows as i64
            - 1
            - i64::from(self.scroll_offset);
        let folds = if self.folds_active() {
            blocks::folds(&model)
        } else {
            Vec::new()
        };
        blocks::layout_rows(model.oldest_line(), &folds, last, rows)
    }

    /// The grid row the cursor is on, if it is on the screen.
    pub(crate) fn cursor_row(&self) -> Option<u16> {
        let cursor = self.model.get().cursor();
        if !self.folds_active() {
            return Some(cursor.row);
        }
        let line = self.model.get().viewport_line(cursor.row);
        self.display()
            .iter()
            .position(|shown| *shown == DisplayRow::Line(line))
            .and_then(|row| u16::try_from(row).ok())
    }

    /// Rows of the screen above the first one shown. A preview too short for the whole screen
    /// ends at the last row that has anything on it (content or the cursor), so a shell prompt
    /// near the top of a tall terminal shows its text instead of blank rows below it.
    pub(crate) fn row_offset(&self) -> u16 {
        match (self.mode, self.layout) {
            (ViewMode::Preview, Some(layout)) => {
                let model = self.model.get();
                let screen = model.screen_rows();
                if screen <= layout.rows {
                    return 0;
                }
                let blank = |row: &hutan_protocol::render::Row| {
                    row.runs.iter().all(|run| {
                        run.text.trim().is_empty()
                            && run.bg == hutan_protocol::render::Color::Default
                    })
                };
                let last_content = model
                    .viewport()
                    .iter()
                    .rposition(|row| !blank(row))
                    .and_then(|i| u16::try_from(i).ok());
                let cursor = model.cursor();
                let last_cursor = cursor.visible.then_some(cursor.row);
                let last = last_content
                    .into_iter()
                    .chain(last_cursor)
                    .max()
                    .unwrap_or(0)
                    .min(screen - 1);
                (last + 1).saturating_sub(layout.rows)
            }
            _ => 0,
        }
    }

    /// The text of the rows as laid out (what the view shows), top to bottom. A folded block
    /// shows its summary row. Empty before the first layout.
    pub fn visible_text(&self) -> Vec<String> {
        if self.layout.is_none() {
            return Vec::new();
        }
        let model = self.model.get();
        self.display()
            .iter()
            .map(|shown| match shown {
                DisplayRow::Line(line) => model
                    .row_at(*line)
                    .map(TerminalModel::row_text)
                    .unwrap_or_default(),
                DisplayRow::Summary(fold) => fold.summary(),
            })
            .collect()
    }

    /// Cursor shape and whether it blinks: an application's explicit choice wins over the
    /// user's preference, which applies while the application keeps the default steady block.
    pub(crate) fn cursor_appearance(&self) -> (CursorShape, bool) {
        let cursor = self.model.get().cursor();
        let app_default = cursor.shape == CursorShape::Block && !cursor.blinking;
        if app_default {
            let shape = match self.settings.cursor.style {
                CursorStyle::Block => CursorShape::Block,
                CursorStyle::Bar => CursorShape::Bar,
                CursorStyle::Underline => CursorShape::Underline,
            };
            (shape, self.settings.cursor.blink)
        } else {
            (cursor.shape, cursor.blinking)
        }
    }

    pub(crate) fn link_at(&self, row: u16, col: u16) -> Option<u32> {
        let model = self.model.get();
        let line = match self.display().get(usize::from(row)) {
            Some(DisplayRow::Summary(_)) => return None,
            Some(DisplayRow::Line(line)) => *line,
            None => self.line_for_row(row),
        };
        let row = model.row_at(line)?;
        row_cells(row)
            .iter()
            .find(|c| c.col <= col && col < c.end())
            .and_then(|c| c.run.link)
    }

    /// The link in plain text under the pointer while ⌘ is held, and the cells it underlines as
    /// `(absolute line, first column, end column)` per row.
    pub fn hovered_link(&self) -> Option<(&LinkTarget, &[Segment])> {
        self.hover_detected
            .as_ref()
            .map(|d| (&d.target, d.segments.as_slice()))
    }

    pub(crate) fn hover_link(&self) -> Option<u32> {
        let (row, col) = self.hover_cell?;
        self.link_at(row, col)
    }

    /// The link in plain text at `(row, col)`: a URL, or a `path[:line[:col]]` that exists
    /// (relative paths against the folder of the block the line belongs to, else the shell's
    /// current folder). Reads the logical line around the cell; call on hover and click only.
    pub(crate) fn detect_link(&self, row: u16, col: u16) -> Option<Detected> {
        if self.mode != ViewMode::Interactive {
            return None;
        }
        let line = match self.display().get(usize::from(row)) {
            Some(DisplayRow::Summary(_)) => return None,
            Some(DisplayRow::Line(line)) => *line,
            None => self.line_for_row(row),
        };
        let model = self.model.get();
        let mut first = line;
        while line - first < LINK_SCAN_ROWS && model.row_at(first - 1).is_some_and(|r| r.wrapped) {
            first -= 1;
        }
        let mut last = line;
        while last - line < LINK_SCAN_ROWS
            && model.row_at(last).is_some_and(|r| r.wrapped)
            && model.row_at(last + 1).is_some()
        {
            last += 1;
        }
        // The logical line as text, and where each cell sits in it: (byte, line, from, to).
        let mut text = String::new();
        let mut cells: Vec<(usize, i64, u16, u16)> = Vec::new();
        let mut at = None;
        for l in first..=last {
            let row = model.row_at(l)?;
            for cell in row_cells(row) {
                if l == line && cell.col <= col && col < cell.end() {
                    at = Some(text.len());
                }
                cells.push((text.len(), l, cell.col, cell.end()));
                match cell.text.chars().next() {
                    Some(c) if !c.is_control() => text.push_str(cell.text),
                    _ => text.push(' '),
                }
            }
            if row.wrapped {
                // A wrapped row is full: blanks the daemon trimmed are blanks of the text.
                let width = row_width(row);
                if width < model.cols() {
                    cells.push((text.len(), l, width, model.cols()));
                    text.push(' ');
                }
            }
        }
        let found = links::candidate_at(&text, at?)?;
        let target = match found.candidate {
            Candidate::Url(url) => LinkTarget::Url(url),
            Candidate::Path {
                path,
                line: at_line,
                column,
            } => {
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let relative = !path.starts_with(['/', '~']);
                let cwd = if relative { self.link_cwd(line) } else { None };
                let path = links::resolve(&path, cwd.as_deref(), home.as_deref())?;
                std::fs::metadata(&path).ok()?;
                LinkTarget::File {
                    path,
                    line: at_line,
                    column,
                }
            }
        };
        let mut segments: Vec<Segment> = Vec::new();
        for (byte, l, from, to) in &cells {
            if !found.range.contains(byte) {
                continue;
            }
            match segments.last_mut() {
                Some((last_line, _, end)) if last_line == l => *end = *to,
                _ => segments.push((*l, *from, *to)),
            }
        }
        Some(Detected { target, segments })
    }

    /// The folder relative paths on `line` are relative to: where the command of the block that
    /// owns the line ran, else the shell's current folder.
    fn link_cwd(&self, line: i64) -> Option<std::path::PathBuf> {
        let model = self.model.get();
        if !model.modes().alt_screen {
            let spans = blocks::spans(&model, self.cursor_line());
            let cwd = blocks::block_at(&spans, line)
                .and_then(|id| model.blocks().get(id))
                .and_then(|block| block.cwd.clone());
            if cwd.is_some() {
                return cwd;
            }
        }
        self.backend.working_dir()
    }

    /// Whether a plain hover (no ⌘) may mark the links in the text: not while the application
    /// takes the mouse or draws its own screen (vim, htop), where the text is its own.
    fn quiet_links(&self) -> bool {
        let model = self.model.get();
        let modes = model.modes();
        modes.mouse == MouseMode::None && !modes.alt_screen
    }

    /// What the tooltip over a link in the text says, while the pointer is on one and ⌘ is not
    /// held (with ⌘ it is underlined solid and opens on a click: the hint has done its job).
    pub fn link_tooltip(&self) -> Option<&'static str> {
        (self.hover_detected.is_some() && !self.cmd_held).then_some(LINK_TOOLTIP)
    }

    /// Finds the plain-text link under the pointer. Returns whether what is underlined
    /// changed.
    fn refresh_detected(&mut self) -> bool {
        let wanted = match self.hover_cell {
            Some((row, col))
                if (self.cmd_held || self.quiet_links()) && self.link_at(row, col).is_none() =>
            {
                if self.hover_detected.as_ref().is_some_and(|d| {
                    let line = self.line_for_row(row);
                    d.covers(line, col)
                }) {
                    return false;
                }
                self.detect_link(row, col)
            }
            _ => None,
        };
        if wanted == self.hover_detected {
            return false;
        }
        self.hover_detected = wanted;
        true
    }

    pub(crate) fn marked_for_paint(&self) -> Option<&str> {
        self.marked.as_ref().map(|m| m.text.as_str())
    }

    /// How many times this view was painted.
    pub fn paints(&self) -> u64 {
        self.paints
    }

    pub(crate) fn send_ack(&self, frame: u64) {
        self.backend.ack(frame);
    }

    /// Called when a paint of this view finished: completes the keystrokes its frame echoed and
    /// reports the first paint of a terminal that shows text (cold start trace).
    pub(crate) fn painted(&mut self) {
        self.paints += 1;
        if !crate::trace::enabled() {
            return;
        }
        self.latency.painted_and_emit(std::time::Instant::now());
        if self.mode == ViewMode::Interactive && self.model.get().visible_has_text() {
            crate::trace::note_text_painted();
            if !crate::trace::startup_seen("first_terminal_painted") {
                crate::trace::startup("first_terminal_painted");
            }
        }
    }

    /// Records the laid-out grid and reports its size to the backend: at once the first time,
    /// debounced afterwards.
    pub(crate) fn update_layout(&mut self, layout: Layout, cx: &mut Context<Self>) {
        if crate::trace::enabled() {
            self.latency.prepaint(std::time::Instant::now());
        }
        self.layout = Some(layout);
        if self.mode == ViewMode::Preview {
            return;
        }
        let pixels = |v: Pixels| {
            (f32::from(v) * layout.scale)
                .round()
                .clamp(0.0, f32::from(u16::MAX)) as u16
        };
        let size = TerminalSize {
            cols: layout.cols,
            rows: layout.rows,
            pixel_width: pixels(layout.bounds.size.width),
            pixel_height: pixels(layout.bounds.size.height),
        };
        if self.wanted_size == Some(size) {
            return;
        }
        self.wanted_size = Some(size);
        if self.reported_size.is_none() {
            self.resize_task = None;
            self.report_size(size);
        } else if self.reported_size == Some(size) {
            self.resize_task = None;
        } else {
            self.resize_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RESIZE_DEBOUNCE).await;
                this.update(cx, |view, _| {
                    view.resize_task = None;
                    view.report_size(size);
                })
                .ok();
            }));
        }
    }

    /// *Terminal: Redraw*: asks the application to repaint at the current size. The PTY only
    /// signals SIGWINCH when its size changes, so the size is sent with one row less (never one
    /// column less: that would reflow) and, after [`REDRAW_NUDGE`], as it is. Does nothing
    /// before the first layout or for a view that does not take input.
    pub fn redraw(&mut self, cx: &mut Context<Self>) {
        let Some(size) = self.wanted_size else {
            return;
        };
        if !self.accepts_input() {
            return;
        }
        let nudged = TerminalSize {
            rows: if size.rows > 1 { size.rows - 1 } else { 2 },
            ..size
        };
        self.backend.resize(nudged);
        self.redraw_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REDRAW_NUDGE).await;
            this.update(cx, |view, _| {
                view.redraw_task = None;
                // The layout may have changed meanwhile: its size is the one to restore.
                if let Some(size) = view.wanted_size {
                    view.reported_size = Some(size);
                    view.backend.resize(size);
                }
            })
            .ok();
        }));
    }

    fn report_size(&mut self, size: TerminalSize) {
        if self.reported_size == Some(size) {
            return;
        }
        self.reported_size = Some(size);
        self.backend.resize(size);
    }

    // ---- focus and blink -------------------------------------------------------------------

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        self.blink_on = true;
        if focused {
            self.keep_blinking(cx);
        } else {
            self.blink_task = None;
            self.blink_ticks_left = 0;
        }
        if self.model.get().modes().focus_events && self.accepts_input() {
            self.backend.input(if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            });
        }
        cx.notify();
    }

    /// Activity: the cursor blinks for another [`BLINK_TICKS`] phases. The timer exists only
    /// while the focused view has a blinking cursor; an idle or unfocused terminal sets none, so
    /// it never wakes the process.
    fn keep_blinking(&mut self, cx: &mut Context<Self>) {
        if self.mode != ViewMode::Interactive || !self.focused {
            return;
        }
        self.blink_ticks_left = BLINK_TICKS;
        if self.blink_task.is_some() || !self.cursor_appearance().1 {
            return;
        }
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let more = this.update(cx, |view, cx| view.blink_tick(cx));
                if !more.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// One blink phase. Returns whether the timer keeps running.
    fn blink_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let blinks = self.focused && self.cursor_appearance().1;
        if blinks && self.blink_ticks_left > 0 {
            self.blink_ticks_left -= 1;
            self.blink_on = !self.blink_on;
            cx.notify();
            if self.blink_ticks_left > 0 {
                return true;
            }
        }
        // Solid from here on (and visible: the last phase may have been the dark one).
        self.blink_task = None;
        if !self.blink_on {
            self.blink_on = true;
            cx.notify();
        }
        false
    }

    /// Whether a cursor blink timer is running (false when idle: nothing is scheduled).
    pub fn blink_timer_active(&self) -> bool {
        self.blink_task.is_some()
    }

    // ---- keyboard --------------------------------------------------------------------------

    fn key_options(&self) -> KeyOptions {
        let modes = self.model.get().modes();
        KeyOptions {
            app_cursor: modes.app_cursor,
            app_keypad: modes.app_keypad,
            kitty_flags: modes.kitty_keyboard,
            option_as_meta: self.settings.option_as_meta.enabled(),
        }
    }

    /// Sends bytes the user produced (typing, paste) to the application.
    fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if !self.accepts_input() || bytes.is_empty() || self.pending_paste.is_some() {
            self.latency.key_unsent();
            return;
        }
        if self.settings.scroll_to_bottom_on_input && self.scroll_offset != 0 {
            self.set_scroll_offset(0);
            self.scroll_accum = 0.0;
        }
        self.selection = None;
        self.selecting = false;
        self.highlight = None;
        self.blink_on = true;
        self.keep_blinking(cx);
        self.backend.input(bytes);
        if crate::trace::enabled() {
            self.latency.key_sent(std::time::Instant::now());
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key_at = crate::trace::enabled().then(std::time::Instant::now);
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if self.pending_paste.is_some() && self.answer_paste_with_key(ks, cx) {
            cx.stop_propagation();
            return;
        }
        if m.platform {
            if !m.control && !m.alt {
                match ks.key.as_str() {
                    "c" if !m.shift => {
                        self.copy(cx);
                        cx.stop_propagation();
                    }
                    "c" => {
                        if self.copy_current_block_output(cx) {
                            cx.stop_propagation();
                        }
                    }
                    "a" if m.shift => {
                        if self.select_current_block_output(cx) {
                            cx.stop_propagation();
                        }
                    }
                    "up" | "down" if !m.shift => {
                        if self.jump_to_prompt(ks.key == "up", cx) {
                            cx.stop_propagation();
                        }
                    }
                    "v" => {
                        self.paste(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }
            return;
        }
        if !self.accepts_input() {
            return;
        }
        // Printable keys reach the PTY through the input handler after this returns, so the
        // timer starts here and `send_input` stops it for either path.
        if let Some(at) = key_at {
            self.latency.key_started(at);
        }
        if m.shift && !m.control && !m.alt && !self.model.get().modes().alt_screen {
            let page = i32::from(
                self.layout
                    .map_or(self.model.get().screen_rows(), |l| l.rows),
            )
            .max(2)
                - 1;
            let handled = match ks.key.as_str() {
                "pageup" => Some(page),
                "pagedown" => Some(-page),
                "home" => Some(i32::MAX / 2),
                "end" => Some(i32::MIN / 2),
                _ => None,
            };
            if let Some(lines) = handled {
                self.scroll_lines(lines, cx);
                cx.stop_propagation();
                return;
            }
        }
        let Some(input) = convert::key_input(ks) else {
            return;
        };
        if let Some(bytes) = encode_key(&input, self.key_options()) {
            self.send_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    // ---- clipboard -------------------------------------------------------------------------

    /// Copies the selection; returns whether there was one. Never touches the PTY.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        match self.selection_text() {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    /// ⌘V. Files copied in Finder type their quoted paths (as a drop does); text is pasted,
    /// bracketed when the application enabled it, and a multi-line text for an application
    /// that did not asks first (`terminal.confirm_multiline_paste`); an image cannot be pasted
    /// into a terminal and says so.
    pub fn paste(&mut self, cx: &mut Context<Self>) {
        if self.pending_paste.is_some() {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let paths: Vec<std::path::PathBuf> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !paths.is_empty() {
            self.insert_paths(&paths, cx);
        } else if let Some(text) = item.text() {
            self.paste_text(text, cx);
        } else if item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
        {
            cx.emit(TerminalViewEvent::Notice(IMAGE_PASTE_NOTICE.to_owned()));
        }
    }

    fn paste_text(&mut self, text: String, cx: &mut Context<Self>) {
        let bracketed = self.model.get().modes().bracketed_paste;
        if !bracketed
            && self.settings.confirm_multiline_paste
            && self.accepts_input()
            && paste::is_multiline(&text)
        {
            self.pending_paste = Some(text);
            cx.notify();
            return;
        }
        self.send_input(paste_bytes(&text, bracketed), cx);
    }

    /// Types `paths` at the cursor, quoted for the shell (a drop, or ⌘V of files copied in
    /// Finder).
    pub fn insert_paths<P: AsRef<std::path::Path>>(&mut self, paths: &[P], cx: &mut Context<Self>) {
        let Some(text) = paste::dropped_paths_text(paths) else {
            return;
        };
        let bracketed = self.model.get().modes().bracketed_paste;
        self.send_input(paste_bytes(&text, bracketed), cx);
    }

    /// The multi-line paste waiting for an answer, if any.
    pub fn pending_paste(&self) -> Option<&str> {
        self.pending_paste.as_deref()
    }

    /// Answers the multi-line paste confirmation; does nothing when none is open.
    pub fn answer_paste(&mut self, choice: PasteChoice, cx: &mut Context<Self>) {
        let Some(text) = self.pending_paste.take() else {
            return;
        };
        let bracketed = self.model.get().modes().bracketed_paste;
        match choice {
            PasteChoice::Paste => self.send_input(paste_bytes(&text, bracketed), cx),
            PasteChoice::OneLine => {
                self.send_input(paste_bytes(&paste::as_one_line(&text), bracketed), cx);
            }
            PasteChoice::Cancel => {}
        }
        cx.notify();
    }

    /// Enter pastes as one line, ⌘Enter as it is, Escape cancels; every other key is held back
    /// while the question is open (nothing may type into the prompt behind it). Returns whether
    /// the key was consumed: ⌘ shortcuts of the app still work.
    fn answer_paste_with_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) -> bool {
        let m = ks.modifiers;
        let choice = match ks.key.as_str() {
            "enter" if m.platform => Some(PasteChoice::Paste),
            "enter" if !m.control && !m.alt && !m.shift => Some(PasteChoice::OneLine),
            "escape" => Some(PasteChoice::Cancel),
            _ => None,
        };
        match choice {
            Some(choice) => {
                self.answer_paste(choice, cx);
                true
            }
            None => !m.platform,
        }
    }

    // ---- scrollback ------------------------------------------------------------------------

    /// Scrolls the viewport by `lines` (positive = towards older output).
    pub fn scroll_lines(&mut self, lines: i32, cx: &mut Context<Self>) {
        let max = i64::from(self.model.get().history_len());
        let new = (i64::from(self.scroll_offset) + i64::from(lines)).clamp(0, max);
        let new = u32::try_from(new).unwrap_or(0);
        if new != self.scroll_offset {
            self.set_scroll_offset(new);
            self.ensure_history();
            cx.notify();
        }
    }

    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.scroll_offset != 0 {
            self.set_scroll_offset(0);
            self.scroll_accum = 0.0;
            cx.notify();
        }
    }

    // ---- find ------------------------------------------------------------------------------

    /// Draws `highlights` over the output (every match tinted, the current one stronger), or
    /// nothing for `None`. The owner runs the search; see [`crate::find`].
    pub fn set_find_highlights(
        &mut self,
        highlights: Option<FindHighlights>,
        cx: &mut Context<Self>,
    ) {
        if self.find != highlights {
            self.find = highlights;
            cx.notify();
        }
    }

    pub fn find_highlights(&self) -> Option<&FindHighlights> {
        self.find.as_ref()
    }

    /// What the last paint drew of the find matches: `(grid row, first cell, cell after the
    /// last, current)`, top to bottom.
    pub fn painted_find(&self) -> &[(u16, u16, u16, bool)] {
        &self.painted_find
    }

    /// Makes the match visible: nothing moves when it already is, otherwise it is scrolled to
    /// the middle of the view. Scrolling into the scrollback asks the daemon for the rows
    /// that are not loaded yet (`terminal.history`).
    pub fn reveal_find_match(&mut self, span: FindSpan, cx: &mut Context<Self>) {
        let line = self.model.get().line_of(span.row);
        if self.display().iter().any(|row| shows(row, line)) {
            return;
        }
        let rows = i64::from(
            self.layout
                .map_or(self.model.get().screen_rows(), |l| l.rows),
        );
        let (top, max) = {
            let model = self.model.get();
            (model.viewport_line(0), model.history_len())
        };
        let offset = (top + rows - 1 - line - rows / 2).clamp(0, i64::from(max));
        self.set_scroll_offset(u32::try_from(offset).unwrap_or(0));
        self.scroll_accum = 0.0;
        self.ensure_history();
        cx.notify();
    }

    /// Requests the first missing history page the viewport needs.
    fn ensure_history(&mut self) {
        if self.scroll_offset == 0 && !self.folds_active() {
            return;
        }
        let missing = {
            let model = self.model.get();
            let total = model.history_total();
            self.display()
                .iter()
                .filter_map(|shown| match shown {
                    DisplayRow::Line(line) => u64::try_from(*line).ok().filter(|l| *l < total),
                    DisplayRow::Summary(_) => None,
                })
                .map(|line| u32::try_from(total - 1 - line).unwrap_or(u32::MAX))
                .filter(|from_line| {
                    *from_line < model.history_len() && !model.has_history(*from_line)
                })
                .min()
        };
        let Some(missing) = missing else { return };
        if self.history_end.is_some_and(|end| missing >= end)
            || !self.history_in_flight.insert(missing)
        {
            return;
        }
        self.backend.request_history(missing, HISTORY_PAGE);
    }

    // ---- mouse -----------------------------------------------------------------------------

    fn report_mouse(&mut self, action: MouseAction, mods: Mods, row: u16, col: u16) -> bool {
        if self.pending_paste.is_some() {
            return false;
        }
        let modes = self.model.get().modes();
        let event = MouseEvent {
            action,
            mods,
            col,
            row,
        };
        match encode_mouse(&event, modes.mouse, modes.mouse_sgr) {
            Some(bytes) => {
                self.backend.input(bytes);
                true
            }
            None => false,
        }
    }

    fn captures_mouse(&self, mods: gpui::Modifiers) -> bool {
        self.model.get().modes().mouse != MouseMode::None && !mods.shift && self.accepts_input()
    }

    /// Whether a mouse press with `mods` goes to the program in the terminal (it turned on
    /// mouse reporting; ⇧ always keeps the press for the terminal). Its owner leaves such a
    /// press alone: no context menu over a program that handles the mouse itself.
    pub fn reports_mouse(&self, mods: gpui::Modifiers) -> bool {
        self.captures_mouse(mods)
    }

    pub(crate) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout else { return };
        window.focus(&self.focus_handle, cx);
        if self.pending_paste.is_some() {
            return;
        }
        let (row, col) = layout.cell_at(event.position);
        self.highlight = None;
        if event.button == gpui::MouseButton::Left && event.modifiers.platform {
            let hyperlink = self
                .link_at(row, col)
                .and_then(|id| self.model.get().link(id).map(str::to_owned))
                .filter(|uri| OPENABLE_SCHEMES.iter().any(|s| uri.starts_with(s)));
            if let Some(uri) = hyperlink {
                cx.emit(TerminalViewEvent::OpenLink(LinkTarget::Url(uri)));
                return;
            }
            if let Some(detected) = self.detect_link(row, col) {
                cx.emit(TerminalViewEvent::OpenLink(detected.target));
                return;
            }
        }
        if self.captures_mouse(event.modifiers) {
            if let Some(button) = convert::button(event.button) {
                self.reporting = Some(button);
                self.last_motion_cell = Some((row, col));
                self.report_mouse(
                    MouseAction::Press(button),
                    convert::mods(event.modifiers),
                    row,
                    col,
                );
            }
            return;
        }
        if event.button != gpui::MouseButton::Left {
            return;
        }
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            _ => SelectionKind::Line,
        };
        let at = GridPoint {
            line: self.line_for_row(row),
            col,
        };
        self.selection = Some(Selection::new(kind, at, &self.model.get()));
        self.selecting = true;
        cx.notify();
    }

    /// `inside`: the pointer is over the terminal (drags continue outside it).
    pub(crate) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        inside: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout else { return };
        let (row, col) = layout.cell_at(event.position);
        let hover = inside.then_some((row, col));
        let cmd = event.modifiers.platform;
        if hover != self.hover_cell || cmd != self.cmd_held {
            let had_link = self.hover_link().is_some();
            let had_block = self.hover_block();
            // ⌘ turns the dotted underline of a link solid.
            let restyled = cmd != self.cmd_held && self.hover_detected.is_some();
            self.hover_cell = hover;
            self.cmd_held = cmd;
            let detected_changed = self.refresh_detected();
            if detected_changed
                || restyled
                || had_link
                || self.hover_link().is_some()
                || had_block != self.hover_block()
            {
                cx.notify();
            }
        }
        let mods = convert::mods(event.modifiers);
        if let Some(button) = self.reporting {
            if self.last_motion_cell != Some((row, col)) {
                self.last_motion_cell = Some((row, col));
                self.report_mouse(MouseAction::Motion(Some(button)), mods, row, col);
            }
            return;
        }
        if self.selecting && event.pressed_button == Some(gpui::MouseButton::Left) {
            let top = layout.bounds.origin.y;
            let bottom = top + layout.bounds.size.height;
            if event.position.y < top {
                self.scroll_lines(1, cx);
            } else if event.position.y > bottom {
                self.scroll_lines(-1, cx);
            }
            let at = GridPoint {
                line: self.line_for_row(row),
                col,
            };
            if let Some(selection) = &mut self.selection {
                selection.extend(at);
                cx.notify();
            }
            return;
        }
        if inside
            && event.pressed_button.is_none()
            && self.model.get().modes().mouse == MouseMode::Motion
            && self.captures_mouse(event.modifiers)
            && self.last_motion_cell != Some((row, col))
        {
            self.last_motion_cell = Some((row, col));
            self.report_mouse(MouseAction::Motion(None), mods, row, col);
        }
    }

    pub(crate) fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let (row, col) = layout.cell_at(event.position);
        if let Some(button) = self.reporting.take() {
            self.last_motion_cell = None;
            self.report_mouse(
                MouseAction::Release(button),
                convert::mods(event.modifiers),
                row,
                col,
            );
            return;
        }
        if !self.selecting {
            return;
        }
        self.selecting = false;
        if self
            .selection
            .and_then(|s| s.span(&self.model.get()))
            .is_none()
        {
            self.selection = None;
        } else if self.settings.copy_on_select {
            self.copy(cx);
        }
        cx.notify();
    }

    pub(crate) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let delta = match event.delta {
            ScrollDelta::Pixels(p) => p.y / layout.cell.height,
            ScrollDelta::Lines(l) => l.y,
        };
        if delta == 0.0 {
            return;
        }
        if (delta > 0.0) != (self.scroll_accum > 0.0) {
            self.scroll_accum = 0.0;
        }
        self.scroll_accum = (self.scroll_accum + delta).clamp(-MAX_WHEEL_LINES, MAX_WHEEL_LINES);
        let lines = self.scroll_accum.trunc();
        if lines == 0.0 {
            return;
        }
        self.scroll_accum -= lines;
        let count = lines.abs() as usize;
        let up = lines > 0.0;
        let modes = self.model.get().modes();
        let (row, col) = layout.cell_at(event.position);

        if self.captures_mouse(event.modifiers) && self.scroll_offset == 0 {
            let direction = if up {
                WheelDirection::Up
            } else {
                WheelDirection::Down
            };
            let mods = convert::mods(event.modifiers);
            for _ in 0..count {
                self.report_mouse(MouseAction::Wheel(direction), mods, row, col);
            }
            return;
        }
        if modes.alt_screen {
            if modes.alternate_scroll && !event.modifiers.shift && self.accepts_input() {
                self.backend
                    .input(alternate_scroll(up, count, modes.app_cursor));
            }
            return;
        }
        let signed = i32::try_from(count).unwrap_or(i32::MAX);
        self.scroll_lines(if up { signed } else { -signed }, cx);
    }

    pub(crate) fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        cx: &mut Context<Self>,
    ) {
        if event.modifiers.platform != self.cmd_held {
            let restyled = self.hover_detected.is_some();
            self.cmd_held = event.modifiers.platform;
            if self.refresh_detected() || restyled || self.hover_link().is_some() {
                cx.notify();
            }
        }
    }
}

// ---- blocks ---------------------------------------------------------------------------------

/// What the hover actions bar offers for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockActions {
    pub block: u64,
    pub copy_command: bool,
    pub copy_output: bool,
    /// `Some(true)` when the output is folded (the button unfolds), `Some(false)` when it can
    /// be folded, `None` when there is nothing to fold.
    pub folded: Option<bool>,
    pub rerun: bool,
    /// The block has a command or output to send to an agent.
    pub send_to_agent: bool,
}

fn shows(row: &DisplayRow, line: i64) -> bool {
    match row {
        DisplayRow::Line(l) => *l == line,
        DisplayRow::Summary(fold) => fold.start <= line && line < fold.end,
    }
}

impl TerminalView {
    fn cursor_line(&self) -> i64 {
        let model = self.model.get();
        model.viewport_line(model.cursor().row)
    }

    pub(crate) fn block_spans(&self) -> Vec<blocks::BlockSpan> {
        blocks::spans(&self.model.get(), self.cursor_line())
    }

    /// The block under the pointer (interactive views only; none while selecting or while the
    /// application receives mouse reports).
    pub fn hover_block(&self) -> Option<u64> {
        if !self.can_hover_blocks() {
            return None;
        }
        self.hover_block_in(&self.display(), &self.block_spans())
    }

    fn can_hover_blocks(&self) -> bool {
        self.mode == ViewMode::Interactive
            && self.blocks_visible()
            && !self.selecting
            && self.reporting.is_none()
    }

    /// [`Self::hover_block`] over a display and spans the caller already has (one paint).
    pub(crate) fn hover_block_in(
        &self,
        display: &[DisplayRow],
        spans: &[blocks::BlockSpan],
    ) -> Option<u64> {
        if !self.can_hover_blocks() {
            return None;
        }
        let (row, _) = self.hover_cell?;
        let line = display.get(usize::from(row))?.line();
        blocks::block_at(spans, line)
    }

    /// The block the last prompt jump or output selection landed on.
    pub fn highlighted_block(&self) -> Option<u64> {
        self.highlight
    }

    /// The block the keyboard actions act on: the highlighted one when it qualifies, else the
    /// newest block that does.
    pub fn current_block(&self, wanted: impl Fn(&Block) -> bool) -> Option<u64> {
        let model = self.model.get();
        let blocks = model.blocks();
        self.highlight
            .and_then(|id| blocks.get(id))
            .filter(|block| wanted(block))
            .or_else(|| {
                blocks
                    .iter()
                    .rev()
                    .find(|block| block.state != BlockState::Prompt && wanted(block))
            })
            .map(|block| block.id)
    }

    fn has_output(&self, block: &Block) -> bool {
        blocks::output_lines(&self.model.get(), block, self.cursor_line()).is_some()
    }

    /// What the actions bar offers for `id` right now.
    pub fn block_actions(&self, id: u64) -> Option<BlockActions> {
        let model = self.model.get();
        let block = model.blocks().get(id)?;
        if block.state == BlockState::Prompt {
            return None;
        }
        let running = model
            .blocks()
            .iter()
            .any(|block| block.state == BlockState::Running);
        let folded = if model.is_collapsed(id) {
            Some(true)
        } else {
            blocks::foldable(block).then_some(false)
        };
        let copy_output = self.has_output(block);
        Some(BlockActions {
            block: id,
            copy_command: block.command.is_some(),
            copy_output,
            send_to_agent: block.command.is_some() || copy_output,
            folded,
            rerun: self.accepts_input()
                && block
                    .command
                    .as_deref()
                    .is_some_and(|c| !c.trim().is_empty())
                && !running
                && !model.modes().alt_screen,
        })
    }

    /// Makes sure `line` is on screen: scrolls it to the top of the view when it is not.
    fn reveal(&mut self, line: i64) {
        if self.display().iter().any(|row| shows(row, line)) {
            return;
        }
        let (top, max) = {
            let model = self.model.get();
            (model.viewport_line(0), model.history_len())
        };
        let offset = (top - line).clamp(0, i64::from(max));
        self.set_scroll_offset(u32::try_from(offset).unwrap_or(0));
        self.scroll_accum = 0.0;
        self.ensure_history();
    }

    /// Scrolls to `id`'s prompt and highlights the block.
    pub fn jump_to_block(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        let Some(span) = self.block_spans().into_iter().find(|span| span.id == id) else {
            return false;
        };
        self.highlight = Some(id);
        self.reveal(span.first);
        cx.notify();
        true
    }

    /// ⌘↑ / ⌘↓: moves the highlight to the previous / next prompt and scrolls to it. Without a
    /// highlight the previous prompt is the one before the live one (or, scrolled back, the one
    /// above the first visible line). Past the newest block, `next` returns to the live screen.
    /// Returns whether the key was used.
    pub fn jump_to_prompt(&mut self, previous: bool, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        let spans = self.block_spans();
        if spans.is_empty() {
            return false;
        }
        let at = self
            .highlight
            .and_then(|id| spans.iter().position(|span| span.id == id));
        let target = match at {
            Some(index) if previous => index.checked_sub(1),
            Some(index) => (index + 1 < spans.len()).then_some(index + 1),
            None if self.scroll_offset == 0 => {
                if previous {
                    spans.len().checked_sub(2)
                } else {
                    None
                }
            }
            None => {
                let top = self.display().first().map(DisplayRow::line);
                top.and_then(|top| {
                    if previous {
                        spans.iter().rposition(|span| span.first < top)
                    } else {
                        spans.iter().position(|span| span.first > top)
                    }
                })
            }
        };
        match target {
            Some(index) => self.jump_to_block(spans[index].id, cx),
            None if !previous && (at.is_some() || self.scroll_offset > 0) => {
                self.highlight = None;
                self.scroll_to_bottom(cx);
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// Selects the output of block `id` with the regular selection (copy it with ⌘C).
    pub fn select_block_output(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        let range = {
            let model = self.model.get();
            model
                .blocks()
                .get(id)
                .and_then(|block| blocks::output_lines(&model, block, self.cursor_line()))
        };
        let Some((start, end)) = range else {
            return false;
        };
        self.highlight = Some(id);
        self.reveal(start);
        let selection = {
            let model = self.model.get();
            let mut selection = Selection::new(
                SelectionKind::Simple,
                GridPoint {
                    line: start,
                    col: 0,
                },
                &model,
            );
            selection.extend(GridPoint {
                line: end - 1,
                col: model.cols().max(1) - 1,
            });
            selection
        };
        self.selection = Some(selection);
        self.selecting = false;
        cx.notify();
        true
    }

    /// Copies the command or the output of block `id` to the clipboard. The text comes from the
    /// daemon (`terminal.block_text`), so it works for output that left the loaded scrollback.
    pub fn copy_block_part(&mut self, id: u64, part: BlockPart, cx: &mut Context<Self>) -> bool {
        if self.model.get().blocks().get(id).is_none() {
            return false;
        }
        let text = self.backend.block_text(id, part);
        cx.spawn(async move |this, cx| {
            if let Some(text) = text.await {
                this.update(cx, |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                })
                .ok();
            }
        })
        .detach();
        true
    }

    /// Folds `id`'s output into one summary row, or unfolds it. Only finished blocks with output
    /// fold. View-only: the daemon, the scrollback and other clients are untouched.
    pub fn toggle_block_collapsed(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        let collapsed = {
            let model = self.model.get();
            let Some(block) = model.blocks().get(id) else {
                return false;
            };
            if model.is_collapsed(id) {
                true
            } else if blocks::foldable(block) {
                false
            } else {
                return false;
            }
        };
        self.shared.update(cx, |shared, cx| {
            shared.set_block_collapsed(id, !collapsed, cx)
        })
    }

    /// Asks the owner to send block `id` to an agent chat ([`TerminalViewEvent::SendBlockToAgent`]).
    pub fn send_block_to_agent(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if !self
            .block_actions(id)
            .is_some_and(|actions| actions.send_to_agent)
        {
            return false;
        }
        cx.emit(TerminalViewEvent::SendBlockToAgent { block: id });
        true
    }

    /// Types block `id`'s command into the session and presses Enter.
    pub fn rerun_block(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if self.block_actions(id).is_none_or(|actions| !actions.rerun) {
            return false;
        }
        let (command, bracketed) = {
            let model = self.model.get();
            let command = model
                .blocks()
                .get(id)
                .and_then(|block| block.command.clone());
            (command, model.modes().bracketed_paste)
        };
        let Some(command) = command else {
            return false;
        };
        let mut bytes = if command.contains('\n') {
            paste_bytes(&command, bracketed)
        } else {
            command
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .into_bytes()
        };
        bytes.push(b'\r');
        self.send_input(bytes, cx);
        true
    }

    /// ⌘⇧C: copies the output of the current block.
    pub fn copy_current_block_output(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        match self.current_block(|block| self.has_output(block)) {
            Some(id) => self.copy_block_part(id, BlockPart::Output, cx),
            None => false,
        }
    }

    /// ⌘⇧A: selects the output of the current block.
    pub fn select_current_block_output(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.blocks_visible() {
            return false;
        }
        match self.current_block(|block| self.has_output(block)) {
            Some(id) => self.select_block_output(id, cx),
            None => false,
        }
    }

    /// Whether a running block's indicator should animate now.
    fn wants_animation(&self) -> bool {
        self.mode == ViewMode::Interactive
            && self.settings.blocks.animate
            && self.blocks_visible()
            && self
                .model
                .get()
                .blocks()
                .last()
                .is_some_and(|block| block.state == BlockState::Running)
    }

    /// Starts or stops the indicator timer; it exists only while a command runs in a visible
    /// overlay, so an idle terminal wakes nothing.
    fn sync_animation(&mut self, cx: &mut Context<Self>) {
        if !self.wants_animation() {
            self.animation_task = None;
            self.animation_phase = 0;
            return;
        }
        if self.animation_task.is_some() {
            return;
        }
        self.animation_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ANIMATION_TICK).await;
                let more = this.update(cx, |view, cx| {
                    if !view.wants_animation() {
                        view.animation_task = None;
                        view.animation_phase = 0;
                        cx.notify();
                        return false;
                    }
                    view.animation_phase = (view.animation_phase + 1) % ANIMATION_PHASES;
                    cx.notify();
                    true
                });
                if !more.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// What the last paint drew of the block overlay, in grid rows.
    pub fn painted_overlay(&self) -> &blocks::OverlayReport {
        &self.painted_overlay
    }

    /// Phase of the running-block indicator (`0` while it does not run).
    pub fn animation_phase_for_tests(&self) -> u8 {
        self.animation_phase
    }

    /// Whether the running-block indicator timer is active (tests).
    pub fn block_animation_active(&self) -> bool {
        self.animation_task.is_some()
    }
}

// ---- text input (IME) ---------------------------------------------------------------------

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Byte range of `s` for a UTF-16 range, clamped to the string.
fn byte_range(s: &str, range: &Range<usize>) -> Range<usize> {
    let mut units = 0;
    let mut start = s.len();
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        if units >= range.start && start == s.len() {
            start = i;
        }
        if units >= range.end {
            end = i;
            break;
        }
        units += c.len_utf16();
    }
    start..end.max(start)
}

impl gpui::EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let marked = self.marked.as_ref()?;
        let bytes = byte_range(&marked.text, &range);
        *adjusted_range =
            Some(utf16_len(&marked.text[..bytes.start])..utf16_len(&marked.text[..bytes.end]));
        Some(marked.text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked.as_ref().map_or(0..0, |m| m.selected.clone()),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.as_ref().map(|m| 0..utf16_len(&m.text))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The terminal has no document to edit: committing text (also the end of a composition)
        // sends it to the application.
        self.marked = None;
        self.send_input(text.as_bytes().to_vec(), cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.accepts_input() {
            return;
        }
        self.marked = (!new_text.is_empty()).then(|| {
            let len = utf16_len(new_text);
            Marked {
                text: new_text.to_owned(),
                selected: new_selected_range
                    .map(|r| r.start.min(len)..r.end.min(len))
                    .unwrap_or(len..len),
            }
        });
        self.blink_on = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout?;
        let cursor = self.model.get().cursor();
        // Columns the marked text covers before the requested range starts.
        let skipped = self.marked.as_ref().map_or(0, |m| {
            let bytes = byte_range(&m.text, &range_utf16);
            crate::shape::text_columns(&m.text[..bytes.start])
        });
        let col = f32::from(cursor.col) + skipped as f32;
        let origin = point(
            element_bounds.origin.x + layout.cell.width * col,
            element_bounds.origin.y
                + layout.cell.height * f32::from(self.cursor_row().unwrap_or(cursor.row)),
        );
        Some(Bounds::new(origin, layout.cell))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        self.accepts_input()
    }
}

// ---- rendering ----------------------------------------------------------------------------

impl TerminalView {
    /// The hover actions bar of the block under the pointer, positioned on the block's first
    /// visible row at the right edge.
    /// The display row the actions bar of block `id` sits on: over the output, at the block's
    /// top right, so the command line stays readable to its end and the exit badge stays
    /// visible. A block with no output has only the one row, and the bar is on it.
    pub fn block_actions_row(&self, id: u64) -> Option<usize> {
        let spans = self.block_spans();
        let display = self.display();
        let first = display
            .iter()
            .position(|shown| blocks::block_at(&spans, shown.line()) == Some(id))?;
        let has_output = display
            .get(first + 1)
            .is_some_and(|shown| blocks::block_at(&spans, shown.line()) == Some(id));
        Some(if has_output { first + 1 } else { first })
    }

    fn actions_bar(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let id = self.hover_block()?;
        let actions = self.block_actions(id)?;
        let layout = self.layout?;
        let row = self.block_actions_row(id)?;
        let (background, foreground) = (self.theme.background(), self.theme.foreground());
        let surface = hsla(background.mix(foreground, 0.16));
        let text = hsla(foreground.mix(background, 0.2));
        let dim = hsla(foreground.mix(background, 0.6));
        let hover = hsla(background.mix(foreground, 0.28));
        let size = (self.settings.font_size * 0.85).max(8.0);
        let button =
            |key: &'static str,
             label: String,
             enabled: bool,
             run: fn(&mut TerminalView, u64, &mut Context<TerminalView>)| {
                div()
                    .id(key)
                    .px(px(6.))
                    .h_full()
                    .flex()
                    .items_center()
                    .whitespace_nowrap()
                    .text_size(px(size))
                    .text_color(if enabled { text } else { dim })
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |this, _, _, cx| run(this, id, cx)))
                    })
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(SharedString::from(label))
            };
        let fold_label = match actions.folded {
            Some(true) => "Expand",
            _ => "Collapse",
        };
        Some(
            div()
                .id("hutan-block-actions")
                .absolute()
                .top(px(self.settings.padding) + layout.cell.height * row as f32)
                .right(px(self.settings.padding))
                .h(layout.cell.height)
                .flex()
                .items_center()
                .rounded(px(4.))
                .bg(surface)
                .child(button(
                    "block-copy-command",
                    "Copy command".into(),
                    actions.copy_command,
                    |this, id, cx| {
                        this.copy_block_part(id, BlockPart::Command, cx);
                    },
                ))
                .child(button(
                    "block-copy-output",
                    "Copy output".into(),
                    actions.copy_output,
                    |this, id, cx| {
                        this.copy_block_part(id, BlockPart::Output, cx);
                    },
                ))
                .child(button(
                    "block-collapse",
                    fold_label.into(),
                    actions.folded.is_some(),
                    |this, id, cx| {
                        this.toggle_block_collapsed(id, cx);
                    },
                ))
                .child(button(
                    "block-rerun",
                    "Rerun".into(),
                    actions.rerun,
                    |this, id, cx| {
                        this.rerun_block(id, cx);
                    },
                ))
                .child(button(
                    "block-agent",
                    "Send to agent".into(),
                    actions.send_to_agent,
                    |this, id, cx| {
                        this.send_block_to_agent(id, cx);
                    },
                ))
                .into_any_element(),
        )
    }
}

impl TerminalView {
    /// The tooltip under a link in the text ([`Self::link_tooltip`]): a small label one row
    /// below the pointer's cell (above it on the last row), over no text it has not already hidden.
    fn link_tip_overlay(&self) -> Option<gpui::AnyElement> {
        let text = self.link_tooltip()?;
        let (row, col) = self.hover_cell?;
        let layout = self.layout?;
        let (background, foreground) = (self.theme.background(), self.theme.foreground());
        let below = row + 1 < layout.rows;
        let shown_row = if below {
            row + 1
        } else {
            row.saturating_sub(1)
        };
        Some(
            div()
                .id("hutan-link-tip")
                .absolute()
                .top(px(self.settings.padding) + layout.cell.height * shown_row as f32)
                .left(px(self.settings.padding) + layout.cell.width * f32::from(col))
                .h(layout.cell.height)
                .px(px(6.))
                .flex()
                .items_center()
                .whitespace_nowrap()
                .rounded(px(4.))
                .bg(hsla(background.mix(foreground, 0.16)))
                .text_color(hsla(foreground.mix(background, 0.2)))
                .text_size(px((self.settings.font_size * 0.85).max(8.0)))
                .child(SharedString::from(text))
                .into_any_element(),
        )
    }

    /// The "N new lines ↓" pill at the bottom edge, in the theme's cursor colors, while the
    /// view is scrolled back and output arrived below it. A click returns to the live screen.
    fn jump_pill(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let new = self.new_lines_below();
        if new == 0 {
            return None;
        }
        let text = match new {
            1 => "1 new line ↓".to_owned(),
            n => format!("{n} new lines ↓"),
        };
        let fill = hsla(self.theme.cursor());
        let size = (self.settings.font_size * 0.9).max(9.0);
        Some(
            div()
                .absolute()
                .bottom(px(self.settings.padding + 8.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("hutan-terminal-jump-latest")
                        .flex()
                        .items_center()
                        .h(px(JUMP_PILL_HEIGHT))
                        .px(px(12.))
                        .rounded(px(JUMP_PILL_HEIGHT / 2.0))
                        .bg(fill)
                        .text_color(hsla(self.theme.cursor_foreground()))
                        .text_size(px(size))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .cursor_pointer()
                        .hover(move |s| s.bg(fill.opacity(0.88)))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|this, _, _, cx| this.scroll_to_bottom(cx)))
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
        )
    }
}

impl TerminalView {
    /// The highlight that covers the view while files are dragged over it. Always present, and
    /// invisible until a drag of files is over the view (the chat's drop overlay does the same).
    fn drop_overlay(&self) -> Option<gpui::AnyElement> {
        if !self.accepts_input() {
            return None;
        }
        let accent = hsla(self.theme.cursor());
        let size = (self.settings.font_size * 1.1).max(11.0);
        Some(
            div()
                .absolute()
                .inset_0()
                .m(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .border_2()
                .border_color(accent)
                .bg(accent.opacity(0.14))
                .text_color(accent)
                .text_size(px(size))
                .font_weight(gpui::FontWeight::MEDIUM)
                .opacity(0.0)
                .group_drag_over::<ExternalPaths>(DROP_GROUP, |style| style.opacity(1.0))
                .child("Drop to insert path")
                .into_any_element(),
        )
    }

    /// The question a multi-line paste asks: what will happen, the first lines, and three
    /// answers. Enter pastes as one line (nothing runs), ⌘Enter as it is, Escape cancels.
    fn paste_card(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let text = self.pending_paste.as_deref()?;
        let theme = &self.theme;
        let (background, foreground) = (theme.background(), theme.foreground());
        let card = hsla(background.mix(foreground, 0.08));
        let border = hsla(background.mix(foreground, 0.28));
        let muted = hsla(foreground.mix(background, 0.35));
        let text_color = hsla(foreground);
        let accent = hsla(theme.cursor());
        let on_accent = hsla(theme.cursor_foreground());
        let size = (self.settings.font_size * 0.95).max(10.0);

        let all = paste::lines(text);
        let count = all.len();
        let preview: Vec<gpui::AnyElement> = all
            .iter()
            .take(PASTE_PREVIEW_LINES)
            .map(|line| {
                div()
                    .truncate()
                    .child(SharedString::from(if line.is_empty() {
                        " ".to_owned()
                    } else {
                        (*line).to_owned()
                    }))
                    .into_any_element()
            })
            .collect();
        let more = count.saturating_sub(PASTE_PREVIEW_LINES);
        let button =
            |id: &'static str, label: &'static str, hint: &'static str, choice, primary: bool| {
                let (fill, ink) = if primary {
                    (accent, on_accent)
                } else {
                    (
                        hsla(background.mix(foreground, 0.0)).opacity(0.0),
                        text_color,
                    )
                };
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if primary { accent } else { border })
                    .bg(fill)
                    .text_color(ink)
                    .text_size(px(size))
                    .cursor_pointer()
                    .hover(move |s| {
                        s.bg(if primary {
                            accent.opacity(0.88)
                        } else {
                            hsla(foreground).opacity(0.10)
                        })
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.answer_paste(choice, cx)))
                    .child(label)
                    .child(div().text_size(px(size * 0.85)).opacity(0.7).child(hint))
            };
        Some(
            div()
                .id("hutan-terminal-paste-confirm")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(background).opacity(0.6))
                .occlude()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .w_full()
                        .max_w(px(560.))
                        .m(px(16.))
                        .p(px(16.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(border)
                        .bg(card)
                        .text_color(text_color)
                        .text_size(px(size))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("Paste {count} lines into the terminal?"))),
                        )
                        .child(div().text_color(muted).child(
                            "This program did not turn on bracketed paste, so a shell runs every line as soon as it arrives.",
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .p(px(8.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(border)
                                .bg(hsla(background))
                                .font_family(self.settings.font_family.clone())
                                .text_size(px(size * 0.92))
                                .overflow_hidden()
                                .children(preview)
                                .when(more > 0, |d| {
                                    d.child(div().text_color(muted).child(SharedString::from(format!(
                                        "… and {more} more line{}",
                                        if more == 1 { "" } else { "s" }
                                    ))))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(button("hutan-paste-cancel", "Cancel", "esc", PasteChoice::Cancel, false))
                                .child(button("hutan-paste-all", "Paste", "⌘↩", PasteChoice::Paste, false))
                                .child(button(
                                    "hutan-paste-one-line",
                                    "Paste as one line",
                                    "↩",
                                    PasteChoice::OneLine,
                                    true,
                                )),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = hsla(self.theme.background());
        if self.mode == ViewMode::Preview {
            return div()
                .id("hutan-terminal-preview")
                .relative()
                .size_full()
                .overflow_hidden()
                .bg(background)
                .child(TerminalElement::new(cx.entity()))
                .into_any_element();
        }
        let banner = match &self.status {
            ViewStatus::Live => None,
            ViewStatus::Reconnecting => Some("Reconnecting…".to_owned()),
            ViewStatus::Ended(message) => Some(message.clone()),
        };
        let bar = self.actions_bar(cx);
        let link_tip = self.link_tip_overlay();
        let pill = self.jump_pill(cx);
        let drop_overlay = self.drop_overlay();
        let paste_card = self.paste_card(cx);
        let root = div()
            .id("hutan-terminal")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &Copy, _window, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _window, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &JumpToPreviousPrompt, _window, cx| {
                if !this.jump_to_prompt(true, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &JumpToNextPrompt, _window, cx| {
                if !this.jump_to_prompt(false, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SelectBlockOutput, _window, cx| {
                if !this.select_current_block_output(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CopyBlockOutput, _window, cx| {
                if !this.copy_current_block_output(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CopyBlockCommand, _window, cx| {
                let current = this.current_block(|block| block.command.is_some());
                if !current.is_some_and(|id| this.copy_block_part(id, BlockPart::Command, cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleBlockCollapse, _window, cx| {
                let current = this.current_block(|block| {
                    blocks::foldable(block) || this.model.get().is_collapsed(block.id)
                });
                if !current.is_some_and(|id| this.toggle_block_collapsed(id, cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &SendBlockToAgent, _window, cx| {
                let current =
                    this.current_block(|block| block.command.is_some() || this.has_output(block));
                if !current.is_some_and(|id| this.send_block_to_agent(id, cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &RerunBlock, _window, cx| {
                let current = this.current_block(|block| block.command.is_some());
                if !current.is_some_and(|id| this.rerun_block(id, cx)) {
                    cx.propagate();
                }
            }))
            .group(DROP_GROUP)
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if this.accepts_input() {
                    window.focus(&this.focus_handle, cx);
                    this.insert_paths(paths.paths(), cx);
                }
            }))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(background)
            .child(TerminalElement::new(cx.entity()))
            .children(bar)
            .children(link_tip)
            .children(pill)
            .children(drop_overlay)
            .children(paste_card);
        match banner {
            None => root.into_any_element(),
            Some(text) => root
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px(px(10.))
                        .py(px(4.))
                        .bg(hsla(self.theme.background()).opacity(0.85))
                        .text_color(hsla(self.theme.foreground()))
                        .text_size(px(12.))
                        .child(text),
                )
                .into_any_element(),
        }
    }
}
