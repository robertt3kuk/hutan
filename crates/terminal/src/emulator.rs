//! Headless VT engine: OSC sniffer → `alacritty_terminal` → render DTOs.
//!
//! This is the whole terminal minus the PTY. [`crate::Terminal`] drives one from its reader
//! thread; the conformance corpus, the recorder and the flood bench drive it directly.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::grid::Grid;
use alacritty_terminal::index::Column;
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    CursorShape as EngineCursorShape, Processor, Rgb as EngineRgb,
};
use hutan_protocol::blocks::{Block, BlockPart};
use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::{
    Cursor, CursorShape, Link, Modes, MouseMode, Palette, Row, TerminalFrame, TerminalSnapshot,
};
use parking_lot::Mutex;

use crate::blocks::{BlockIndex, BlockText, BlockTextError, MAX_COMMAND_BYTES, now_ms};
use crate::error::TerminalError;
use crate::queries::{Rgb, xterm_color};
use crate::render::{LinkTable, RenderTracker, RowScratch, row_text};
use crate::scrollback::ScrollbackStore;
use crate::sniffer::{PromptMark, Scan, Signal, StreamScanner, TerminalMeta};

/// Replies produced by one `feed` call beyond this are dropped (a hostile stream of queries
/// must not grow memory without bound).
const MAX_PENDING_REPLY_BYTES: usize = 64 * 1024;
/// Same bound, in queued engine events, for replies that are resolved after the engine ran.
const MAX_PENDING_REQUESTS: usize = 4096;
/// Length of the DEC 2026 begin marker `ESC [ ? 2 0 2 6 h`.
const BSU_LEN: usize = 8;
/// Same bound for queued metadata events.
const MAX_PENDING_META: usize = 4096;
/// History rows kept in the engine; older rows live in the cold store (ADR 0013).
const HOT_MARGIN: usize = 256;
/// Bytes handed to the parser at a time. Bounds how many rows one call can scroll before they
/// are spilled, i.e. the transient size of the grid (and of the process's resident set, which
/// the allocator does not give back).
const SLICE_BYTES: usize = 2048;
/// Rows the engine may hold beyond `scrollback_lines` between two settles. The engine must never
/// reach its own cap, or it would recycle rows unseen and the absolute line numbers of the
/// block index could not be counted: one 2 KiB slice scrolls at most ~2 000 lines with plain
/// line feeds (a hostile stream of `CSI n S` can exceed it; the numbering is then too low by
/// the rows the cap swallowed).
const ENGINE_HEADROOM: usize = 4096;
/// Marks held back while a DEC 2026 batch hides the engine's progress; later ones are dropped.
const MAX_DEFERRED_SIGNALS: usize = 1024;
/// Most rows read for one command text.
const MAX_COMMAND_ROWS: u64 = 64;
/// Cell flag (bit 15, unused by the engine) set on the oldest hot history row while the cold
/// store is not empty: its disappearance means the engine cleared its history.
const MARK: Flags = Flags::from_bits_retain(1 << 15);

/// Colours the terminal reports for OSC 10/11/12 queries until the application sets its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultColors {
    pub foreground: (u8, u8, u8),
    pub background: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
}

impl Default for DefaultColors {
    fn default() -> Self {
        Self {
            foreground: (0xd4, 0xd4, 0xd4),
            background: (0x1e, 0x1e, 0x1e),
            cursor: (0xd4, 0xd4, 0xd4),
        }
    }
}

/// Colour slots of the engine's table that OSC 10/11/12 address.
const FOREGROUND: usize = 256;
const BACKGROUND: usize = 257;
const CURSOR: usize = 258;

/// A reply the engine asked for, in request order. Colour and size replies need the state of
/// the emulator, so they are completed right after the engine handled them
/// ([`Emulator::resolve_requests`]).
enum Request {
    Text(String),
    Color(usize, Arc<dyn Fn(EngineRgb) -> String + Send + Sync>),
    TextArea(Arc<dyn Fn(WindowSize) -> String + Send + Sync>),
}

#[derive(Default)]
struct SinkInner {
    requests: Vec<Request>,
    bells: u32,
}

/// Engine event listener: collects the replies the engine wants written back to the PTY and
/// bell notifications. Everything else the engine reports is intentionally ignored (clipboard,
/// mouse-cursor hints).
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<SinkInner>>);

impl EventListener for Sink {
    fn send_event(&self, event: Event) {
        let mut inner = self.0.lock();
        let request = match event {
            Event::PtyWrite(text) => Request::Text(text),
            Event::ColorRequest(index, format) => Request::Color(index, format),
            Event::TextAreaSizeRequest(format) => Request::TextArea(format),
            Event::Bell => {
                inner.bells = inner.bells.saturating_add(1);
                return;
            }
            _ => return,
        };
        if inner.requests.len() < MAX_PENDING_REQUESTS {
            inner.requests.push(request);
        }
    }
}

struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn validate(size: TerminalSize) -> Result<Dims, TerminalError> {
    if size.cols == 0 || size.rows == 0 {
        return Err(TerminalError::InvalidSize {
            cols: size.cols,
            rows: size.rows,
        });
    }
    Ok(Dims {
        cols: usize::from(size.cols),
        rows: usize::from(size.rows),
    })
}

/// A VT engine instance with render-state extraction. Not thread-safe by itself.
pub struct Emulator {
    term: Term<Sink>,
    parser: Processor,
    scanner: StreamScanner,
    sink: Sink,
    links: LinkTable,
    meta: Vec<TerminalMeta>,
    replies: Vec<u8>,
    default_colors: DefaultColors,
    title: Option<String>,
    size: TerminalSize,
    /// Bumped whenever the visible state may have changed.
    generation: u64,
    /// History rows older than the engine's hot window (ADR 0013).
    cold: ScrollbackStore,
    scrollback_lines: usize,
    /// History rows the engine keeps; older ones are spilled to `cold`.
    hot_margin: usize,
    scratch: RowScratch,
    /// The viewport as protocol rows for the generation and size they were made at.
    rows_cache: Option<((u64, TerminalSize), Vec<Row>)>,
    /// The terminal was resized while the alternate screen hid the primary grid.
    resized_in_alt: bool,
    /// The engine's own history cap (`scrollback_lines` + [`ENGINE_HEADROOM`]).
    engine_cap: usize,
    /// Prompt, command and output units (M10).
    blocks: BlockIndex,
    /// Absolute line number of the viewport's first row on the primary screen: the rows ever
    /// scrolled into the history. Only grows, except by a resize that pulls history rows into
    /// a taller viewport.
    abs_top: u64,
    /// Rows in the engine's history when `abs_top` was last brought up to date.
    acct_hot: usize,
    /// Marks seen while a synchronized batch hid what the engine had executed; applied once it
    /// ended, at the cursor it left.
    deferred: Vec<Signal>,
}

/// Why the engine is run up to an offset of the fed bytes.
enum Break {
    /// A colour query that is answered now.
    Reply,
    /// `CSI 16 t`.
    CellSize,
    /// A prompt mark or working directory for the block index.
    Signal(Signal),
    /// Just before `CSI 3 J` / `ESC c`: rows scrolled so far must be counted first.
    EraseStart,
    /// Just after them.
    Erased { reset: bool },
}

impl std::fmt::Debug for Emulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emulator")
            .field("size", &self.size)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Emulator {
    /// `scrollback_lines` bounds the retained history exactly.
    pub fn new(size: TerminalSize, scrollback_lines: usize) -> Result<Self, TerminalError> {
        Self::with_hot_margin(size, scrollback_lines, HOT_MARGIN)
    }

    /// [`Self::new`] with an explicit hot window size (tests; `usize::MAX` keeps the whole
    /// history in the engine, the reference the cold store is checked against).
    pub(crate) fn with_hot_margin(
        size: TerminalSize,
        scrollback_lines: usize,
        hot_margin: usize,
    ) -> Result<Self, TerminalError> {
        let dims = validate(size)?;
        let sink = Sink::default();
        let config = Config {
            kitty_keyboard: true,
            // The engine's cap is above the line bound (`settle` enforces that one) so it
            // never recycles a row unseen (ADR 0013, `ENGINE_HEADROOM`).
            scrolling_history: scrollback_lines.saturating_add(ENGINE_HEADROOM),
            ..Config::default()
        };
        Ok(Self {
            term: Term::new(config, &dims, sink.clone()),
            parser: Processor::new(),
            scanner: StreamScanner::new(),
            sink,
            links: LinkTable::default(),
            meta: Vec::new(),
            replies: Vec::new(),
            default_colors: DefaultColors::default(),
            title: None,
            size,
            generation: 1,
            cold: ScrollbackStore::default(),
            scrollback_lines,
            hot_margin,
            scratch: RowScratch::default(),
            rows_cache: None,
            resized_in_alt: false,
            engine_cap: scrollback_lines.saturating_add(ENGINE_HEADROOM),
            blocks: BlockIndex::new(),
            abs_top: 0,
            acct_hot: 0,
            deferred: Vec::new(),
        })
    }

    /// Sets the colours reported for OSC 10/11/12 queries while the application has not
    /// overridden them.
    #[must_use]
    pub fn with_default_colors(mut self, colors: DefaultColors) -> Self {
        self.default_colors = colors;
        self
    }

    /// Changes the colours reported for OSC 10/11/12 queries from now on; colours the
    /// application set itself still win.
    pub fn set_default_colors(&mut self, colors: DefaultColors) {
        self.default_colors = colors;
    }

    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// Counter that changes whenever the visible state may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Runs `bytes` through the sniffer and the engine. Returns `true` when visible state may
    /// have changed; `false` while every byte went into an open DEC 2026 synchronized batch
    /// (the batch is committed by its end marker, [`Self::stop_sync`] or a buffer overflow).
    pub fn feed(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let meta_start = self.meta.len();
        let was_pending = self.sync_deadline().is_some();
        // Queries are answered in stream order: the engine runs up to the end of each one (so a
        // colour query sees the colours as they were at that point) and its reply is queued
        // before the next query is handled. CSI 16 t is not known to the engine; its reply is
        // produced here. One pass over the bytes finds all of it.
        let mut scan = Scan::default();
        self.scanner.feed(bytes, &mut self.meta, &mut scan);
        let mut breaks: Vec<(usize, Break)> = Vec::new();
        breaks.extend(
            scan.color_queries
                .into_iter()
                .map(|end| (end, Break::Reply)),
        );
        breaks.extend(
            scan.signals
                .into_iter()
                .map(|(end, signal)| (end, Break::Signal(signal))),
        );
        breaks.extend(
            scan.cell_sizes
                .into_iter()
                .map(|end| (end, Break::CellSize)),
        );
        for erase in scan.erases {
            breaks.push((erase.start, Break::EraseStart));
            breaks.push((erase.end, Break::Erased { reset: erase.reset }));
        }
        // Equal offsets: whatever ended there first, then the start of an erase.
        breaks.sort_by_key(|(end, brk)| (*end, matches!(brk, Break::EraseStart)));
        // The engine ignores OSC 7 and OSC 133 but formats a log line for each (vte's
        // `unhandled`, a String per string even with logging off): the sniffer has them, so the
        // engine runs over the bytes around them. Their marks stay where they were in the
        // stream (`breaks` are re-aimed at the shortened input). Not while a synchronized batch
        // is open: its byte accounting is of what was read.
        let kept;
        let bytes = if was_pending || scan.skips.is_empty() {
            bytes
        } else {
            let mut shortened = Vec::with_capacity(bytes.len());
            let mut from = 0;
            for &(start, end) in &scan.skips {
                shortened.extend_from_slice(&bytes[from..start]);
                from = end;
            }
            shortened.extend_from_slice(&bytes[from..]);
            let (mut removed, mut next) = (0, 0);
            for (at, _) in &mut breaks {
                while next < scan.skips.len() && scan.skips[next].1 <= *at {
                    removed += scan.skips[next].1 - scan.skips[next].0;
                    next += 1;
                }
                // A break inside a skipped string (the signal of an ST-terminated one) lands
                // where the string began.
                let inside = scan
                    .skips
                    .get(next)
                    .filter(|(start, _)| *start < *at)
                    .map_or(0, |(start, _)| *at - start);
                *at -= removed + inside;
            }
            kept = shortened;
            &kept[..]
        };
        let mut done = 0;
        for (end, brk) in breaks {
            self.advance(&bytes[done..end]);
            self.resolve_requests();
            match brk {
                Break::Reply | Break::EraseStart => {}
                Break::CellSize => self.push_reply(&cell_size_report(self.size)),
                Break::Signal(signal) => self.signal(signal),
                Break::Erased { reset } => {
                    // `ESC c` also wipes the screen, which no scrolling accounts for.
                    if reset && !self.sync_pending() && !self.on_alt_screen() {
                        self.blocks.truncate_all();
                    }
                }
            }
            done = end;
        }
        self.advance(&bytes[done..]);
        self.resolve_requests();
        self.flush_deferred();
        self.flush_blocks();
        self.collect_bell();
        self.apply_titles(meta_start);
        let withheld = self.parser.sync_bytes_count();
        let visible = if was_pending {
            // Everything went into the batch unless the batch ended inside this chunk.
            withheld < bytes.len()
        } else if self.sync_deadline().is_some() {
            // A batch opened inside this chunk: only bytes before its begin marker count.
            bytes.len() > withheld + BSU_LEN
        } else {
            true
        };
        if visible {
            self.generation += 1;
        }
        visible
    }

    /// When the open synchronized-output batch must be force-closed (150 ms after it began).
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// True while a synchronized-output batch is open and its bytes are withheld.
    pub fn sync_pending(&self) -> bool {
        self.parser.sync_bytes_count() > 0 || self.sync_deadline().is_some()
    }

    /// Commits an open synchronized-output batch.
    pub fn stop_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.settle();
        self.resolve_requests();
        self.flush_deferred();
        self.flush_blocks();
        self.collect_bell();
        self.generation += 1;
    }

    /// Takes the bytes to write back to the PTY: DA, DSR, DECRQM, kitty keyboard, colour
    /// (OSC 4/10/11/12) and window size (CSI 14/16/18 t) replies, in query order.
    pub fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Takes pending metadata events in stream order (bell, at most once per `feed`, last).
    pub fn drain_meta(&mut self) -> Vec<TerminalMeta> {
        std::mem::take(&mut self.meta)
    }

    pub fn resize(&mut self, size: TerminalSize) -> Result<(), TerminalError> {
        let dims = validate(size)?;
        // The first engine row keeps its line number through a resize; rows are reflowed from
        // there on (cold rows and their numbers are untouched).
        let first_hot = self.abs_top.saturating_sub(self.acct_hot as u64);
        let reflowed = size.cols != self.size.cols;
        let on_alt = self.on_alt_screen();
        if reflowed && !on_alt {
            self.clear_open_prompt();
        }
        let wraps_before =
            (reflowed && !on_alt && self.blocks.reaches(first_hot)).then(|| self.region_wraps());
        // Only the hot margin reflows (it already is the only history in the engine).
        self.term.resize(dims);
        self.size = size;
        self.generation += 1;
        if on_alt {
            self.resized_in_alt = true;
            if reflowed {
                // The hidden rows were reflowed and cannot be read to follow the blocks.
                self.blocks.invalidate_from(first_hot);
            }
        } else {
            // The reflow may have moved the marker; restore it before `settle` looks for it. A
            // window enlarged by more rows than the hot window holds pulled all of it into the
            // viewport, which leaves no row to carry the marker: the cold store goes.
            let hot = self.term.history_size();
            if hot == 0 {
                self.cold.clear();
            } else if !self.cold.is_empty() {
                self.mark_oldest_hot(hot);
            }
            self.abs_top = first_hot + hot as u64;
            self.acct_hot = hot;
            if let Some(before) = wraps_before {
                let after = self.region_wraps();
                self.blocks.remap(first_hot, |line| {
                    remap_reflowed(line - first_hot, &before, &after) + first_hot
                });
            }
            // `settle` also spills what narrowing pushed past the margin.
            self.settle();
        }
        self.flush_blocks();
        Ok(())
    }

    /// A width change while the shell sits at a prompt (`OSC 133;A` seen, no command started):
    /// the rows from the prompt's first row down are erased instead of reflowed, and the cursor
    /// keeps its distance from the prompt's first row. The shell repaints the prompt after
    /// SIGWINCH by moving up the rows it believes its prompt took and cannot know how a reflow
    /// broke the old one into rows.
    fn clear_open_prompt(&mut self) {
        let Some(line) = self.blocks.open_prompt_line() else {
            return;
        };
        // A prompt that starts above the screen (or, stale, below it) is left to the reflow.
        let Some(row) = line.checked_sub(self.viewport_top()) else {
            return;
        };
        let (cursor_line, _) = self.cursor_abs();
        let offset = cursor_line.saturating_sub(line);
        let rows = u64::from(self.size.rows);
        if row >= rows {
            return;
        }
        let cursor_row = (row + offset).min(rows - 1);
        let erase = format!("\x1b[{};1H\x1b[J\x1b[{};1H", row + 1, cursor_row + 1);
        self.parser.advance(&mut self.term, erase.as_bytes());
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Rows in the cold store (tests).
    #[cfg(test)]
    pub(crate) fn cold_len(&self) -> usize {
        self.cold.len()
    }

    /// Lines of scrollback currently retained (0 on the alternate screen).
    pub fn history_len(&self) -> u32 {
        if self.on_alt_screen() {
            return 0;
        }
        u32::try_from(self.term.history_size() + self.cold.len()).unwrap_or(u32::MAX)
    }

    /// True when `tracker` cannot be advanced with a [`TerminalFrame`] and needs a fresh
    /// [`TerminalSnapshot`]: it never had one, the size changed, or the palette changed
    /// (frames carry neither dimensions nor palette).
    pub fn needs_snapshot(&self, tracker: &RenderTracker) -> bool {
        !tracker.synced
            || tracker.cols != self.size.cols
            || tracker.rows != self.size.rows
            || tracker.palette != self.palette()
    }

    /// Full visible state; resets `tracker` to it.
    pub fn snapshot(&mut self, tracker: &mut RenderTracker) -> TerminalSnapshot {
        let lines = self.viewport_rows();
        let cursor = self.cursor();
        let modes = self.modes();
        let palette = self.palette();
        let title = self.title.clone();
        let history_len = self.history_len();
        let viewport_top = self.viewport_top();
        let links = self.links.all();
        tracker.frame += 1;
        tracker.synced = true;
        tracker.cols = self.size.cols;
        tracker.rows = self.size.rows;
        tracker.lines = lines.clone();
        tracker.cursor = Some(cursor);
        tracker.modes = modes;
        tracker.palette = palette.clone();
        tracker.title = title.clone();
        tracker.links_sent = self.links.len();
        tracker.history_len = history_len;
        tracker.viewport_top = viewport_top;
        tracker.generation = self.generation;
        TerminalSnapshot {
            frame: tracker.frame,
            cols: self.size.cols,
            rows: self.size.rows,
            cursor,
            modes,
            palette,
            title,
            lines,
            links,
            history_len,
            viewport_top,
        }
    }

    /// Changes since `tracker`'s state, or `None` when nothing changed — and also when the
    /// tracker [needs a snapshot](Self::needs_snapshot) instead.
    pub fn frame(&mut self, tracker: &mut RenderTracker) -> Option<TerminalFrame> {
        if self.needs_snapshot(tracker) || tracker.generation == self.generation {
            return None;
        }
        let lines = self.viewport_rows();
        let cursor = self.cursor();
        let modes = self.modes();
        let history_len = self.history_len();
        let viewport_top = self.viewport_top();
        let dirty: Vec<(u16, Row)> = lines
            .iter()
            .zip(&tracker.lines)
            .enumerate()
            .filter(|(_, (now, before))| now != before)
            .map(|(index, (now, _))| (index as u16, now.clone()))
            .collect();
        let title_changed = self.title != tracker.title;
        let modes_changed = modes != tracker.modes;
        tracker.generation = self.generation;
        if dirty.is_empty()
            && !title_changed
            && !modes_changed
            && tracker.cursor == Some(cursor)
            && tracker.history_len == history_len
            && tracker.viewport_top == viewport_top
        {
            return None;
        }
        let links = self.links.since(tracker.links_sent);
        tracker.links_sent = self.links.len();
        tracker.frame += 1;
        tracker.lines = lines;
        tracker.cursor = Some(cursor);
        tracker.modes = modes;
        tracker.history_len = history_len;
        tracker.viewport_top = viewport_top;
        if title_changed {
            tracker.title.clone_from(&self.title);
        }
        Some(TerminalFrame {
            frame: tracker.frame,
            scrolled: 0,
            dirty,
            cursor,
            modes: modes_changed.then_some(modes),
            title: title_changed.then(|| self.title.clone()),
            links,
            history_len,
            viewport_top,
        })
    }

    /// Scrollback rows, newest first: `from_line` 0 is the line directly above the viewport,
    /// larger is older. Shorter than `count` at the top of history.
    pub fn history(&mut self, from_line: u32, count: u32) -> Vec<Row> {
        self.history_while(from_line, count, |_| true)
    }

    /// Like [`Self::history`], but stops before the first row `keep` rejects. `keep` sees every
    /// row in order, once, and can account for what it has accepted (e.g. an encoded size
    /// budget).
    pub fn history_while(
        &mut self,
        from_line: u32,
        count: u32,
        mut keep: impl FnMut(&Row) -> bool,
    ) -> Vec<Row> {
        let available = self.history_len();
        let cols = self.term.columns();
        let end = from_line.saturating_add(count).min(available);
        // The engine's rows are the newest; `available > 0` implies the primary screen.
        let hot = u32::try_from(self.term.history_size()).unwrap_or(u32::MAX);
        let mut rows = Vec::new();
        for index in from_line..end {
            let row = if index < hot {
                let line = Line(-(index as i32) - 1);
                self.scratch
                    .fill(&self.term.grid()[line], cols, &mut self.links);
                self.scratch.to_row()
            } else {
                // Cold rows are indexed oldest first.
                let newest_first = (index - hot) as usize;
                match self
                    .cold
                    .len()
                    .checked_sub(newest_first + 1)
                    .and_then(|oldest_first| self.cold.get(oldest_first))
                {
                    Some(cold) => cold.to_row(),
                    None => break,
                }
            };
            if !keep(&row) {
                break;
            }
            rows.push(row);
        }
        rows
    }

    /// Visible rows as plain text, one line per row, trailing spaces trimmed (for tests and
    /// corpus `.grid` files).
    pub fn grid_text(&mut self) -> String {
        let rows = self.viewport_rows();
        rows.iter().map(row_text).collect::<Vec<_>>().join("\n")
    }

    /// Hyperlink table so far (`id == index`).
    pub fn links(&self) -> Vec<Link> {
        self.links.all()
    }

    pub fn cursor(&self) -> Cursor {
        let content = self.term.renderable_content();
        let style = self.term.cursor_style();
        let shape = match style.shape {
            EngineCursorShape::Block | EngineCursorShape::Hidden => CursorShape::Block,
            EngineCursorShape::Underline => CursorShape::Underline,
            EngineCursorShape::Beam => CursorShape::Bar,
            EngineCursorShape::HollowBlock => CursorShape::HollowBlock,
        };
        let point = content.cursor.point;
        Cursor {
            col: u16::try_from(point.column.0).unwrap_or(u16::MAX),
            row: u16::try_from(point.line.0.max(0)).unwrap_or(u16::MAX),
            shape,
            visible: content.cursor.shape != EngineCursorShape::Hidden,
            blinking: style.blinking,
        }
    }

    pub fn modes(&self) -> Modes {
        let mode = *self.term.mode();
        let mouse = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseMode::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseMode::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::Click
        } else {
            MouseMode::None
        };
        const KITTY: [(TermMode, u8); 5] = [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ];
        let kitty_keyboard = KITTY
            .iter()
            .filter(|(flag, _)| mode.contains(*flag))
            .fold(0, |bits, (_, bit)| bits | bit);
        Modes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse,
            mouse_sgr: mode.contains(TermMode::SGR_MOUSE),
            focus_events: mode.contains(TermMode::FOCUS_IN_OUT),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
            kitty_keyboard,
        }
    }

    /// Colours the application changed with OSC 4/10/11/12.
    pub fn palette(&self) -> Palette {
        let colors = self.term.colors();
        let rgb = |index: usize| colors[index].map(|c| (c.r, c.g, c.b));
        Palette {
            indexed: (0..256usize)
                .filter_map(|index| rgb(index).map(|color| (index as u8, color)))
                .collect(),
            foreground: rgb(FOREGROUND),
            background: rgb(BACKGROUND),
            cursor: rgb(CURSOR),
        }
    }

    /// The viewport as protocol rows. Converted once per generation: every attached client asks
    /// for them after each change, and the grid does not change in between.
    fn viewport_rows(&mut self) -> Vec<Row> {
        let key = (self.generation, self.size);
        if let Some((cached, rows)) = &self.rows_cache
            && *cached == key
        {
            return rows.clone();
        }
        let cols = self.term.columns();
        let lines = self.term.screen_lines();
        let mut rows = Vec::with_capacity(lines);
        for line in 0..lines {
            self.scratch
                .fill(&self.term.grid()[Line(line as i32)], cols, &mut self.links);
            rows.push(self.scratch.to_row());
        }
        self.rows_cache = Some((key, rows.clone()));
        rows
    }

    /// Runs `bytes` through the engine in slices and settles the scrollback when the slice left
    /// more history in the grid than the hot window keeps, and once at the end (a read has to
    /// be settled before anything that looks at the grid: a block mark, a query reply, a
    /// frame). Settling every slice that scrolled keeps the grid at the size of the hot window:
    /// letting rows pile up for a few slices measured 2–5× slower on a flood (the spilled rows
    /// are no longer in cache when they are converted).
    fn advance(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let limit = self.hot_margin.min(self.scrollback_lines);
        let mut settled = true;
        for slice in bytes.chunks(SLICE_BYTES) {
            self.parser.advance(&mut self.term, slice);
            if self.term.history_size() > limit {
                self.settle();
                settled = true;
            } else {
                settled = false;
            }
        }
        if !settled {
            self.settle();
        }
    }

    fn on_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    // Blocks ----------------------------------------------------------------------------------

    /// Blocks newest first: at most `limit`, only ids below `before_id` when it is given.
    pub fn blocks(&self, before_id: Option<u64>, limit: usize) -> Vec<Block> {
        self.blocks.page(before_id, limit)
    }

    /// Absolute line number of the viewport's first row: rows ever scrolled into the history.
    /// The `from_line` row of [`Self::history`] has the number `viewport_top() - from_line - 1`.
    pub fn viewport_top(&self) -> u64 {
        self.abs_top
    }

    /// Text of a block's command or output as plain text (soft wraps joined, trailing spaces
    /// and trailing empty lines removed), at most `max_bytes` long (cut on a character
    /// boundary, the start is kept). The output of a running block reaches the cursor's row.
    pub fn block_text(
        &mut self,
        id: u64,
        part: BlockPart,
        max_bytes: usize,
    ) -> Result<BlockText, BlockTextError> {
        let block = self.blocks.get(id).ok_or(BlockTextError::NotFound)?.clone();
        let (mut text, mut truncated) = match part {
            BlockPart::Command => (block.command.unwrap_or_default(), false),
            BlockPart::Output => {
                let Some(start) = block.output_start else {
                    return Ok(BlockText {
                        text: String::new(),
                        truncated: false,
                    });
                };
                if self.on_alt_screen() {
                    return Err(BlockTextError::Unavailable);
                }
                let end = match block.end_line {
                    Some(end) => end,
                    None => self.cursor_abs().0 + 1,
                };
                self.output_text(start, end, max_bytes)
            }
        };
        if text.len() > max_bytes {
            let mut cut = max_bytes;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            truncated = true;
        }
        Ok(BlockText { text, truncated })
    }

    /// The lines `start..end` that are still retained, as text; the flag says the first ones
    /// are gone or the text was cut at `max_bytes`.
    fn output_text(&mut self, start: u64, end: u64, max_bytes: usize) -> (String, bool) {
        let hot = self.term.history_size();
        let retained = (hot + self.cold.len()) as u64;
        let first_retained = self.abs_top.saturating_sub(retained);
        let lost = start < first_retained;
        let from = start.max(first_retained) - first_retained;
        let to = end.saturating_sub(first_retained);
        let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else {
            return (String::new(), lost);
        };
        let mut text = String::new();
        let mut cut = false;
        for (index, line) in self.logical_range(from, to).enumerate() {
            if index > 0 {
                text.push('\n');
            }
            text.push_str(&line.text);
            if text.len() > max_bytes {
                cut = true;
                break;
            }
        }
        let kept = text.trim_end_matches('\n').len();
        text.truncate(kept);
        (text, lost || cut)
    }

    /// Absolute line and column of the cursor on the primary screen.
    fn cursor_abs(&self) -> (u64, u16) {
        let point = self.term.grid().cursor.point;
        (
            self.abs_top + u64::try_from(point.line.0.max(0)).unwrap_or(0),
            u16::try_from(point.column.0).unwrap_or(u16::MAX),
        )
    }

    /// Grid line holding absolute line `abs`, if the engine still has that row.
    fn grid_line(&self, abs: u64) -> Option<Line> {
        let relative = i64::try_from(abs).ok()? - i64::try_from(self.abs_top).ok()?;
        let hot = i64::try_from(self.term.history_size()).ok()?;
        let lines = i64::try_from(self.term.screen_lines()).ok()?;
        (relative >= -hot && relative < lines).then_some(Line(relative as i32))
    }

    /// The text typed between the end of the prompt (`from`) and the cursor (`end_line`,
    /// `end_col`): rows joined along soft wraps, hard line breaks kept.
    fn command_text(&self, from: (u64, u16), end_line: u64, end_col: u16) -> Option<String> {
        let last = if end_col == 0 {
            end_line.checked_sub(1)?
        } else {
            end_line
        };
        if last < from.0 {
            return None;
        }
        let cols = self.term.columns();
        let grid = self.term.grid();
        let mut text = String::new();
        let mut line_text = String::new();
        for abs in from.0..=last.min(from.0 + MAX_COMMAND_ROWS) {
            let Some(line) = self.grid_line(abs) else {
                if abs == from.0 {
                    return None;
                }
                break;
            };
            let row = &grid[line];
            let first = if abs == from.0 {
                usize::from(from.1)
            } else {
                0
            };
            for cell in &row[Column(first.min(cols))..Column(cols.min(row.len()))] {
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line_text.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(extra) = cell.zerowidth() {
                    line_text.extend(extra);
                }
            }
            let wrapped = cols > 0
                && row.len() >= cols
                && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            if !wrapped {
                text.push_str(line_text.trim_end_matches(' '));
                text.push('\n');
                line_text.clear();
            }
        }
        text.push_str(line_text.trim_end_matches(' '));
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut end = text.len().min(MAX_COMMAND_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Some(text[..end].to_owned())
    }

    /// Soft-wrap flag of every engine row, history rows (oldest first) then the viewport.
    fn region_wraps(&self) -> Vec<bool> {
        let cols = self.term.columns();
        let grid = self.term.grid();
        let hot = i32::try_from(self.term.history_size()).unwrap_or(i32::MAX);
        let lines = i32::try_from(self.term.screen_lines()).unwrap_or(i32::MAX);
        (-hot..lines)
            .map(|line| {
                let row = &grid[Line(line)];
                cols > 0
                    && row.len() >= cols
                    && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE)
            })
            .collect()
    }

    /// Hands a mark to the block index once the engine ran every byte before it; while a
    /// synchronized batch hides that, it waits for the batch to end.
    fn signal(&mut self, signal: Signal) {
        self.flush_deferred();
        if self.sync_pending() {
            if self.deferred.len() < MAX_DEFERRED_SIGNALS {
                self.deferred.push(signal);
            }
        } else {
            self.apply_signal(signal);
        }
    }

    fn flush_deferred(&mut self) {
        if !self.deferred.is_empty() && !self.sync_pending() {
            for signal in std::mem::take(&mut self.deferred) {
                self.apply_signal(signal);
            }
        }
    }

    fn apply_signal(&mut self, signal: Signal) {
        let mark = match signal {
            Signal::Cwd(cwd) => {
                self.blocks.set_cwd(cwd);
                return;
            }
            // The alternate screen is a program drawing, not the shell's prompt.
            Signal::Mark(_) if self.on_alt_screen() => return,
            Signal::Mark(mark) => mark,
        };
        let (line, col) = self.cursor_abs();
        // Output that ends mid-line includes that line.
        let end = if col > 0 { line + 1 } else { line };
        let now = now_ms();
        match mark {
            PromptMark::A => self.blocks.prompt(line, end, now),
            PromptMark::B => self.blocks.input(line, col, now),
            PromptMark::C => {
                let command = self
                    .blocks
                    .input_at()
                    .and_then(|from| self.command_text(from, line, col));
                self.blocks.run(line, command, now);
            }
            PromptMark::D { exit_code } => self.blocks.finish(exit_code, end, now),
        }
    }

    /// Queues the blocks that changed as metadata events.
    fn flush_blocks(&mut self) {
        let changed = self.blocks.take_events();
        self.meta
            .extend(changed.into_iter().map(TerminalMeta::Block));
    }

    /// Brings the cold store in line with the engine after it ran: notices a cleared history
    /// (`ESC[3J`, `ESC c`), counts the rows that scrolled, spills the rows beyond the hot
    /// margin, enforces the line bound and retires the blocks whose lines are gone.
    fn settle(&mut self) {
        if self.on_alt_screen() {
            // The primary grid (hot window) is out of reach; nothing is exposed meanwhile.
            return;
        }
        let hot = self.term.history_size();
        let mut cleared = false;
        if !self.cold.is_empty() && (hot == 0 || !self.oldest_hot_is_marked(hot)) {
            if hot > 0 && self.resized_in_alt {
                // A resize inside the alternate screen reflowed the hidden grid and lost the
                // marker; the history is intact.
                self.mark_oldest_hot(hot);
            } else {
                // The engine cleared its history (or recycled all of it at its cap).
                self.cold.clear();
                cleared = true;
            }
        } else if hot < self.acct_hot && !self.resized_in_alt {
            cleared = true;
        }
        self.resized_in_alt = false;
        // Rows that entered the history since the last count; after an erase, the rows that
        // scrolled once it was done (the erase itself is run on its own, see `feed`).
        let scrolled = if cleared {
            hot as i64
        } else {
            hot as i64 - self.acct_hot as i64
        };
        self.abs_top = self.abs_top.saturating_add_signed(scrolled);
        if hot > self.scrollback_lines {
            let grid = self.term.grid_mut();
            grid.update_history(self.scrollback_lines);
            grid.update_history(self.engine_cap);
        }
        self.spill();
        self.acct_hot = self.term.history_size();
        let retained = (self.acct_hot + self.cold.len()) as u64;
        self.blocks
            .truncate_below(self.abs_top.saturating_sub(retained));
    }

    fn oldest_hot_is_marked(&self, hot: usize) -> bool {
        let grid = self.term.grid();
        grid[Line(-(hot as i32))][Column(0)].flags.contains(MARK)
    }

    fn mark_oldest_hot(&mut self, hot: usize) {
        self.term.grid_mut()[Line(-(hot as i32))][Column(0)]
            .flags
            .insert(MARK);
    }

    /// Moves history rows beyond the hot margin into the cold store and frees their grid
    /// memory, marks the oldest remaining hot row and trims the store so that hot + cold never
    /// exceed `scrollback_lines`.
    fn spill(&mut self) {
        let hot = self.term.history_size();
        let keep = self.hot_margin.max(1);
        if hot > keep {
            let cols = self.term.columns();
            for index in 0..hot - keep {
                let line = Line(index as i32 - hot as i32);
                self.scratch
                    .fill(&self.term.grid()[line], cols, &mut self.links);
                self.cold.push(&self.scratch);
            }
            // Shrinking drops the oldest rows; the second call restores the engine's own cap.
            let grid = self.term.grid_mut();
            grid.update_history(keep);
            grid.update_history(self.engine_cap);
        }
        let hot = self.term.history_size();
        self.cold
            .truncate_to(self.scrollback_lines.saturating_sub(hot));
        if !self.cold.is_empty() && hot > 0 {
            self.mark_oldest_hot(hot);
        }
    }

    /// Iterates logical lines (rows joined along soft wraps), oldest first, as plain text:
    /// cold history, hot history and optionally the viewport. A line that wraps across the
    /// end of the scope is cut there; one that wraps across the oldest retained row starts
    /// mid-line. Blank rows yield empty lines.
    pub fn logical_lines(&mut self, scope: LineScope) -> LogicalLines<'_> {
        let on_alt = self.on_alt_screen();
        let hot = self.term.history_size();
        let cold_len = if on_alt { 0 } else { self.cold.len() };
        let viewport = match scope {
            LineScope::History => 0,
            LineScope::HistoryAndViewport => self.term.screen_lines(),
        };
        LogicalLines {
            grid: self.term.grid(),
            cols: self.term.columns(),
            cold: &self.cold,
            links: &mut self.links,
            scratch: &mut self.scratch,
            cold_len,
            hot,
            total: cold_len + hot + viewport,
            next: 0,
        }
    }

    /// Logical lines of the rows `from..to`, counted from the oldest retained row through the
    /// history and on into the viewport.
    fn logical_range(&mut self, from: usize, to: usize) -> LogicalLines<'_> {
        let mut lines = self.logical_lines(LineScope::HistoryAndViewport);
        lines.next = from.min(lines.total);
        lines.total = to.min(lines.total);
        lines
    }

    fn collect_bell(&mut self) {
        let bells = std::mem::take(&mut self.sink.0.lock().bells);
        if bells > 0 {
            self.meta.push(TerminalMeta::Bell);
        }
        if self.meta.len() > MAX_PENDING_META {
            self.meta.truncate(MAX_PENDING_META);
        }
    }

    /// Turns the engine's queued reply requests into bytes.
    fn resolve_requests(&mut self) {
        let requests = std::mem::take(&mut self.sink.0.lock().requests);
        for request in requests {
            let text = match request {
                Request::Text(text) => text,
                Request::Color(index, format) => format(self.color_value(index)),
                // The engine's format is `height = lines * cell_height`, `width = cols *
                // cell_width`; one "cell" spanning the window yields the exact pixel size.
                Request::TextArea(format) => format(WindowSize {
                    num_lines: 1,
                    num_cols: 1,
                    cell_width: self.size.pixel_width,
                    cell_height: self.size.pixel_height,
                }),
            };
            self.push_reply(text.as_bytes());
        }
    }

    fn push_reply(&mut self, bytes: &[u8]) {
        if self.replies.len() + bytes.len() <= MAX_PENDING_REPLY_BYTES {
            self.replies.extend_from_slice(bytes);
        }
    }

    /// Colour for an OSC 4 (palette index) or OSC 10/11/12 (slots 256–258) query: what the
    /// application set, else the configured default / standard xterm palette.
    fn color_value(&self, index: usize) -> EngineRgb {
        let defaults = self.default_colors;
        let fallback: Rgb = match index {
            0..=255 => xterm_color(u8::try_from(index).unwrap_or(0)),
            BACKGROUND => defaults.background,
            CURSOR => defaults.cursor,
            _ => defaults.foreground,
        };
        let overridden = if index <= CURSOR {
            self.term.colors()[index]
        } else {
            None
        };
        overridden.unwrap_or(EngineRgb {
            r: fallback.0,
            g: fallback.1,
            b: fallback.2,
        })
    }

    fn apply_titles(&mut self, from: usize) {
        for meta in self.meta.iter().skip(from) {
            if let TerminalMeta::Title(title) = meta {
                self.title = (!title.is_empty()).then(|| title.clone());
            }
        }
    }
}

/// `CSI 6 ; cell_height ; cell_width t`, the answer to `CSI 16 t`; `0` when the window's pixel
/// size is unknown.
fn cell_size_report(size: TerminalSize) -> Vec<u8> {
    let cell_height = size.pixel_height.checked_div(size.rows).unwrap_or(0);
    let cell_width = size.pixel_width.checked_div(size.cols).unwrap_or(0);
    format!("\x1b[6;{cell_height};{cell_width}t").into_bytes()
}

/// Row index inside the engine's rows after a reflow for the row `index` before it: the same
/// row of the same logical line (clamped to the line's new length). `before` and `after` are the
/// soft-wrap flags of the rows (see `Emulator::region_wraps`).
fn remap_reflowed(index: u64, before: &[bool], after: &[bool]) -> u64 {
    let index = usize::try_from(index).unwrap_or(usize::MAX);
    if index >= before.len() {
        // Past the last row (the line after the cursor's): keep the distance to the end.
        return (after.len() + (index - before.len())) as u64;
    }
    let starts = |wraps: &[bool]| -> Vec<usize> {
        (0..wraps.len())
            .filter(|&row| row == 0 || !wraps[row - 1])
            .collect()
    };
    let old = starts(before);
    let new = starts(after);
    let line = old.partition_point(|&start| start <= index) - 1;
    let offset = index - old[line];
    let Some(&start) = new.get(line) else {
        return after.len().saturating_sub(1) as u64;
    };
    let end = new.get(line + 1).copied().unwrap_or(after.len());
    (start + offset.min((end - start).saturating_sub(1))) as u64
}

/// Which rows [`Emulator::logical_lines`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineScope {
    /// Scrollback only (cold + hot rows; nothing on the alternate screen).
    History,
    /// Scrollback followed by the viewport.
    HistoryAndViewport,
}

/// One logical line: consecutive rows joined along soft wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    /// Index of the line's first row, counted from the oldest retained row (0) through the
    /// history and, with [`LineScope::HistoryAndViewport`], on into the viewport.
    pub start: u32,
    /// Rows the line occupies.
    pub rows: u32,
    /// Text of the rows joined; trailing spaces of the last row are removed, blanks that ended
    /// a wrapped row are kept so the text matches the original line.
    pub text: String,
}

/// Iterator returned by [`Emulator::logical_lines`], oldest line first.
pub struct LogicalLines<'a> {
    grid: &'a Grid<Cell>,
    cols: usize,
    cold: &'a ScrollbackStore,
    links: &'a mut LinkTable,
    scratch: &'a mut RowScratch,
    cold_len: usize,
    hot: usize,
    total: usize,
    next: usize,
}

impl std::fmt::Debug for LogicalLines<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogicalLines")
            .field("next", &self.next)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl LogicalLines<'_> {
    /// Appends row `index`'s text to `out`; returns whether the row soft-wraps.
    fn push_row(&mut self, index: usize, out: &mut String) -> bool {
        if let Some(row) = (index < self.cold_len)
            .then(|| self.cold.get(index))
            .flatten()
        {
            out.push_str(row.text());
            let wrapped = row.wrapped();
            if wrapped {
                out.extend(std::iter::repeat_n(' ', usize::from(row.blank_tail())));
            }
            return wrapped;
        }
        let rest = index - self.cold_len;
        let line = if rest < self.hot {
            Line(rest as i32 - self.hot as i32)
        } else {
            Line((rest - self.hot) as i32)
        };
        self.scratch
            .fill(&self.grid[line], self.cols, &mut *self.links);
        out.push_str(&self.scratch.text);
        if self.scratch.wrapped {
            out.extend(std::iter::repeat_n(
                ' ',
                usize::from(self.scratch.blank_tail),
            ));
        }
        self.scratch.wrapped
    }
}

impl Iterator for LogicalLines<'_> {
    type Item = LogicalLine;

    fn next(&mut self) -> Option<LogicalLine> {
        if self.next >= self.total {
            return None;
        }
        let start = self.next;
        let mut text = String::new();
        loop {
            let wrapped = self.push_row(self.next, &mut text);
            self.next += 1;
            if !wrapped || self.next >= self.total {
                break;
            }
        }
        text.truncate(text.trim_end_matches(' ').len());
        Some(LogicalLine {
            start: u32::try_from(start).unwrap_or(u32::MAX),
            rows: u32::try_from(self.next - start).unwrap_or(u32::MAX),
            text,
        })
    }
}
