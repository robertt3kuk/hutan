//! Terminal hub: the daemon's table of live [`Terminal`]s and everything clients do to them
//! outside the session lifecycle — attach/detach with ack-paced rendering (ADR 0004), resize,
//! history, and the bounded input path.
//!
//! The registry actor owns session state and registers/removes terminals here. Request handlers
//! reach terminals directly (a `Terminal` is a cheap, thread-safe handle), so keystrokes and
//! acks never queue behind session commands in the registry mailbox.
//!
//! Memory bounds: an attachment holds one [`RenderTracker`] (O(screen)) and renders only while
//! it has credit, so a client that never acks costs one screen and never fills its outbound
//! queue; the input queue holds at most [`INPUT_QUEUE`] slices of [`INPUT_SLICE`] bytes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use hutan_core::TerminalId;
use hutan_protocol::blocks::{
    BLOCKS_MAX_LIMIT, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks,
    TerminalBlocksResult,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    TerminalAttach, TerminalAttached, TerminalHistory, TerminalHistoryRows, TerminalResize,
    TerminalSetColors,
};
use hutan_protocol::render::TerminalRender;
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchResult};
use hutan_protocol::{Frame, MAX_PAYLOAD};
use hutan_terminal::{BlockTextError, DefaultColors, RenderTracker, Terminal, TerminalError};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::conn::{ConnHandle, ConnId, Outbound};
use crate::terminal_search::{self, Searches};

/// An attachment renders at most once per this interval (ADR 0004).
const COALESCE: Duration = Duration::from_millis(8);
/// Most rows one `terminal.history` call returns, whatever `count` asks for.
const HISTORY_MAX_ROWS: u32 = 5_000;
/// `terminal.history` stops adding rows once their JSON encoding would exceed this: half the
/// frame limit, leaving room for the envelope.
const HISTORY_MAX_BYTES: usize = MAX_PAYLOAD / 2;
/// `terminal.blocks` stops adding blocks once their JSON encoding would exceed this (same
/// budget as history rows).
const BLOCKS_MAX_BYTES: usize = HISTORY_MAX_BYTES;
/// Longest `text` of `terminal.block_text`, whatever `max_bytes` asks for: JSON escaping can
/// multiply it by up to six, which must still fit one frame.
const BLOCK_TEXT_MAX_BYTES: usize = MAX_PAYLOAD / 8;
/// Input slices waiting per terminal before the sending connection's reader waits.
const INPUT_QUEUE: usize = 32;
/// Input frames are split to this size when queued, bounding the queue to
/// `INPUT_QUEUE * INPUT_SLICE` bytes per terminal.
const INPUT_SLICE: usize = 64 * 1024;

/// What [`TerminalHub::recent_text`] read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentText {
    pub text: String,
    /// Lines in `text`.
    pub lines: usize,
    /// The terminal showed a full-screen program: only its visible screen was read.
    pub alternate_screen: bool,
}

/// A registered terminal. Obtained from [`TerminalHub::register`].
#[derive(Debug)]
pub struct TerminalEntry {
    id: TerminalId,
    terminal: Terminal,
    input: mpsc::Sender<Bytes>,
    ended: Arc<AtomicBool>,
    /// When the registry last heard about input to this terminal (ms since the epoch).
    activity_notified_ms: AtomicU64,
}

impl TerminalEntry {
    /// True at most once per [`ACTIVITY_NOTIFY_INTERVAL`](crate::registry::ACTIVITY_NOTIFY_INTERVAL):
    /// the registry should hear about this input.
    fn activity_notice_due(&self) -> bool {
        let now = crate::registry::now_ms();
        let interval =
            u64::try_from(crate::registry::ACTIVITY_NOTIFY_INTERVAL.as_millis()).unwrap_or(0);
        let last = self.activity_notified_ms.load(Ordering::Relaxed);
        now.saturating_sub(last) >= interval
            && self
                .activity_notified_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    pub fn id(&self) -> TerminalId {
        self.id
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
    }
}

/// Credit/ack state shared between the request handlers and one attachment's render task.
#[derive(Debug)]
struct Attachment {
    /// Latest frame number the client acked; the render task needs it to equal the last frame
    /// it sent before it renders again.
    acks: watch::Sender<u64>,
    /// Closed under the lock before a replacement snapshot is queued, so a stale task can never
    /// enqueue behind it.
    gate: Arc<Mutex<bool>>,
    cancel: CancellationToken,
}

impl Attachment {
    fn stop(&self) {
        *lock(&self.gate) = false;
        self.cancel.cancel();
    }
}

#[derive(Debug, Default)]
struct Inner {
    terminals: HashMap<TerminalId, Arc<TerminalEntry>>,
    attachments: HashMap<(ConnId, TerminalId), Attachment>,
    /// Running `terminal.search` calls.
    searches: Searches,
}

#[derive(Debug)]
pub struct TerminalHub {
    next_id: AtomicU64,
    inner: Mutex<Inner>,
}

impl Default for TerminalHub {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves its data half-updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn not_found(id: TerminalId) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("terminal {id} not found"))
}

fn terminal_error(error: &TerminalError) -> ErrorBody {
    let code = match error {
        TerminalError::InvalidSize { .. } => ErrorCode::BadRequest,
        TerminalError::Closed => ErrorCode::InvalidState,
        _ => ErrorCode::Internal,
    };
    ErrorBody::new(code, error.to_string())
}

impl TerminalHub {
    pub fn new() -> Self {
        Self {
            // Ids start at 1 and only grow within one daemon epoch.
            next_id: AtomicU64::new(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    /// Adds `terminal` under a fresh id and starts its input drain. Needs a Tokio runtime.
    pub fn register(&self, terminal: Terminal) -> Arc<TerminalEntry> {
        let id = TerminalId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (input, input_rx) = mpsc::channel(INPUT_QUEUE);
        let ended = Arc::new(AtomicBool::new(false));
        tokio::spawn(drain_input(
            id,
            terminal.clone(),
            input_rx,
            Arc::clone(&ended),
        ));
        let entry = Arc::new(TerminalEntry {
            id,
            terminal,
            input,
            ended,
            activity_notified_ms: AtomicU64::new(0),
        });
        lock(&self.inner).terminals.insert(id, Arc::clone(&entry));
        entry
    }

    /// Forgets a terminal: its attachments stop and later requests for it get `not_found`.
    pub fn remove(&self, id: TerminalId) {
        let mut inner = lock(&self.inner);
        inner.terminals.remove(&id);
        inner.searches.cancel_terminal(id);
        inner.attachments.retain(|(_, terminal), attachment| {
            let keep = *terminal != id;
            if !keep {
                attachment.stop();
            }
            keep
        });
    }

    /// The child exited: later input is dropped instead of queued.
    pub fn mark_ended(&self, id: TerminalId) {
        if let Some(entry) = lock(&self.inner).terminals.get(&id) {
            entry.ended.store(true, Ordering::Release);
        }
    }

    fn get(&self, id: TerminalId) -> Result<Arc<TerminalEntry>, ErrorBody> {
        lock(&self.inner)
            .terminals
            .get(&id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    /// Stops every attachment of a closed connection.
    pub fn connection_closed(&self, conn: ConnId) {
        let mut inner = lock(&self.inner);
        inner.searches.cancel_connection(conn);
        inner.attachments.retain(|(c, _), attachment| {
            let keep = *c != conn;
            if !keep {
                attachment.stop();
            }
            keep
        });
    }

    /// Queues the snapshot frame on `conn` (the caller queues the response after it), then
    /// keeps the client current by rendering whenever it has acked the previous frame.
    pub fn attach(
        &self,
        conn: &ConnHandle,
        req: TerminalAttach,
    ) -> Result<TerminalAttached, ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        let id = entry.id;
        if let Some(size) = req.size {
            match entry.terminal.resize(size) {
                Ok(()) => {}
                // An ended terminal keeps its final screen; its size can no longer change.
                Err(TerminalError::Closed) => {
                    debug!(terminal = %id, "resize on attach ignored: terminal ended")
                }
                Err(error) => return Err(terminal_error(&error)),
            }
        }

        // Subscribe before the snapshot so damage racing with it is not lost.
        let damage = entry.terminal.damage();
        let mut tracker = RenderTracker::new();
        let render = TerminalRender::Snapshot(entry.terminal.snapshot(&mut tracker));
        let frame = encode_render(id, &render).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::Internal,
                "terminal snapshot exceeds the frame limit",
            )
        })?;
        let first = tracker.frame_number();

        let (acks, ack_rx) = watch::channel(0u64);
        let attachment = Attachment {
            acks,
            gate: Arc::new(Mutex::new(true)),
            cancel: CancellationToken::new(),
        };
        let gate = Arc::clone(&attachment.gate);
        let cancel = attachment.cancel.clone();
        {
            let mut inner = lock(&self.inner);
            // The session may have been restarted between `get` and here.
            if !inner.terminals.contains_key(&id) {
                return Err(not_found(id));
            }
            if let Some(previous) = inner.attachments.insert((conn.id(), id), attachment) {
                previous.stop();
            }
            if !conn.enqueue(Outbound::Frame(frame)) {
                if let Some(failed) = inner.attachments.remove(&(conn.id(), id)) {
                    failed.stop();
                }
                return Err(ErrorBody::new(ErrorCode::Internal, "connection is closing"));
            }
        }
        tokio::spawn(render_loop(RenderTask {
            id,
            terminal: entry.terminal.clone(),
            conn: conn.clone(),
            tracker,
            damage,
            acks: ack_rx,
            gate,
            cancel,
        }));
        Ok(TerminalAttached { frame: first })
    }

    pub fn detach(&self, conn: ConnId, id: TerminalId) -> Result<(), ErrorBody> {
        self.get(id)?;
        if let Some(attachment) = lock(&self.inner).attachments.remove(&(conn, id)) {
            attachment.stop();
        }
        Ok(())
    }

    /// Records the client's ack. Acks for frames of an attachment that no longer exists (after
    /// detach, restart, replacement) are normal races and ignored.
    pub fn ack(&self, conn: ConnId, id: TerminalId, frame: u64) -> Result<(), ErrorBody> {
        self.get(id)?;
        match lock(&self.inner).attachments.get(&(conn, id)) {
            Some(attachment) => {
                attachment.acks.send_replace(frame);
            }
            None => debug!(%conn, terminal = %id, frame, "ack without attachment ignored"),
        }
        Ok(())
    }

    /// One PTY size for everyone: the last writer wins. Attachments get a snapshot through
    /// the terminal's damage notification.
    pub fn resize(&self, req: TerminalResize) -> Result<(), ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        entry
            .terminal
            .resize(req.size)
            .map_err(|e| terminal_error(&e))
    }

    /// The colors OSC 10/11/12 queries are answered with; the last writer wins.
    pub fn set_colors(&self, req: TerminalSetColors) -> Result<(), ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        entry.terminal.set_default_colors(DefaultColors {
            foreground: req.colors.foreground,
            background: req.colors.background,
            cursor: req.colors.cursor,
        });
        Ok(())
    }

    /// Scrollback rows for a client. One call answers at most [`HISTORY_MAX_ROWS`] rows and stops
    /// before the row that would take the encoded rows past [`HISTORY_MAX_BYTES`] (the first
    /// row is always included so paging makes progress). The answer may therefore be shorter
    /// than requested although more history exists; clients page with `from_line`.
    pub fn history(&self, req: TerminalHistory) -> Result<TerminalHistoryRows, ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        let mut encoded = 0usize;
        let mut first = true;
        let rows =
            entry
                .terminal
                .history_while(req.from_line, req.count.min(HISTORY_MAX_ROWS), |row| {
                    // +1 for the separating comma.
                    encoded = encoded.saturating_add(encoded_len(row).saturating_add(1));
                    let fits = first || encoded <= HISTORY_MAX_BYTES;
                    first = false;
                    fits
                });
        Ok(TerminalHistoryRows {
            from_line: req.from_line,
            rows,
        })
    }

    /// Blocks newest first. One call answers at most [`BLOCKS_MAX_LIMIT`] blocks and stops
    /// before the block that would take the encoded blocks past [`BLOCKS_MAX_BYTES`] (the
    /// first is always included); page with `before_id` of the last block returned.
    pub fn blocks(&self, req: TerminalBlocks) -> Result<TerminalBlocksResult, ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        let limit = usize::try_from(req.limit.min(BLOCKS_MAX_LIMIT)).unwrap_or(0);
        let (mut blocks, viewport_top) = entry.terminal.blocks(req.before_id, limit);
        let mut encoded = 0usize;
        let mut keep = 0;
        for block in &blocks {
            encoded = encoded.saturating_add(encoded_len(block).saturating_add(1));
            if keep > 0 && encoded > BLOCKS_MAX_BYTES {
                break;
            }
            keep += 1;
        }
        blocks.truncate(keep);
        Ok(TerminalBlocksResult {
            blocks,
            viewport_top,
        })
    }

    /// Plain text of a block's command or output, cut at `max_bytes` (clamped to
    /// [`BLOCK_TEXT_MAX_BYTES`]).
    pub fn block_text(&self, req: TerminalBlockText) -> Result<TerminalBlockTextResult, ErrorBody> {
        let entry = self.get(req.terminal_id)?;
        let max_bytes = usize::try_from(req.max_bytes)
            .unwrap_or(usize::MAX)
            .min(BLOCK_TEXT_MAX_BYTES);
        match entry.terminal.block_text(req.block_id, req.part, max_bytes) {
            Ok(text) => Ok(TerminalBlockTextResult {
                text: text.text,
                truncated: text.truncated,
            }),
            Err(BlockTextError::NotFound) => Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "block {} of terminal {} not found",
                    req.block_id, req.terminal_id
                ),
            )),
            Err(error @ BlockTextError::Unavailable) => {
                Err(ErrorBody::new(ErrorCode::InvalidState, error.to_string()))
            }
        }
    }

    /// `terminal.search`: the matches of a query in the scrollback and screen, oldest first (see
    /// [`terminal_search`]). A newer search of the same connection for the same terminal
    /// cancels this one (`conflict`, `data.reason = "superseded"`). Runs on the blocking pool,
    /// in slices that lock the terminal's engine one at a time.
    pub async fn search(
        &self,
        conn: ConnId,
        req: TerminalSearch,
    ) -> Result<TerminalSearchResult, ErrorBody> {
        let id = req.terminal_id;
        let entry = self.get(id)?;
        let params = terminal_search::Params::new(&req)?;
        let (handle, cancel) = lock(&self.inner).searches.begin(conn, id);
        let terminal = entry.terminal.clone();
        let worker = cancel.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            terminal_search::run(
                &terminal,
                &params,
                &terminal_search::Bounds::default(),
                &worker,
                || {},
            )
        })
        .await;
        lock(&self.inner).searches.end(conn, id, handle);
        match outcome {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(error.into_body(id)),
            Err(error) => {
                warn!(terminal = %id, %error, "terminal search task failed");
                Err(ErrorBody::new(ErrorCode::Internal, "the search failed"))
            }
        }
    }

    /// The last `lines` lines of what the terminal shows and has scrolled away, as plain text
    /// (soft wraps joined, trailing blank lines dropped), and how many lines that is. A
    /// full-screen program (the alternate screen) has no scrollback to speak of: then only the
    /// visible screen counts.
    pub fn recent_text(&self, id: TerminalId, lines: usize) -> Result<RecentText, ErrorBody> {
        let entry = self.get(id)?;
        let snapshot = entry.terminal.snapshot(&mut RenderTracker::new());
        let alternate_screen = snapshot.modes.alt_screen;
        let row_text = |row: &hutan_protocol::render::Row| {
            let text: String = row.runs.iter().map(|run| run.text.as_str()).collect();
            (text.trim_end_matches(' ').to_owned(), row.wrapped)
        };
        // Rows above the viewport, newest first; a logical line may take several rows, so ask
        // for a multiple of the lines wanted (bounded).
        let scrolled = if alternate_screen {
            Vec::new()
        } else {
            let wanted = u32::try_from(lines.saturating_mul(3).saturating_add(16))
                .unwrap_or(HISTORY_MAX_ROWS)
                .min(HISTORY_MAX_ROWS);
            entry.terminal.history(0, wanted)
        };
        let rows: Vec<(String, bool)> = scrolled
            .iter()
            .rev()
            .chain(snapshot.lines.iter())
            .map(row_text)
            .collect();
        let (text, count) = crate::context::last_lines(
            rows.iter().map(|(text, wrapped)| (text.as_str(), *wrapped)),
            lines,
        );
        Ok(RecentText {
            text,
            lines: count,
            alternate_screen,
        })
    }

    /// Queues a kind=2 frame body for the terminal. Waits while the terminal's input queue is
    /// full (the child is not reading): that stalls the sending connection's reader only.
    /// Input for unknown or ended terminals is dropped.
    ///
    /// Returns whether the registry should be told about session activity: input was queued
    /// and no notice for this terminal went out in the last second.
    pub async fn input(&self, id: TerminalId, bytes: Bytes) -> bool {
        let entry = match self.get(id) {
            Ok(entry) => entry,
            Err(_) => {
                debug!(terminal = %id, len = bytes.len(), "input for an unknown terminal dropped");
                return false;
            }
        };
        let mut rest = bytes;
        let mut queued = false;
        while !rest.is_empty() {
            if entry.ended.load(Ordering::Acquire) {
                debug!(terminal = %id, len = rest.len(), "input for an ended terminal dropped");
                break;
            }
            let slice = rest.split_to(rest.len().min(INPUT_SLICE));
            if entry.input.send(slice).await.is_err() {
                debug!(terminal = %id, "input drain gone; input dropped");
                break;
            }
            queued = true;
        }
        queued && entry.activity_notice_due()
    }
}

/// Feeds queued input to the PTY. `write_input` blocks while the child is not reading, so it
/// runs on the blocking pool; one chunk at a time keeps the bytes in order.
async fn drain_input(
    id: TerminalId,
    terminal: Terminal,
    mut rx: mpsc::Receiver<Bytes>,
    ended: Arc<AtomicBool>,
) {
    while let Some(chunk) = rx.recv().await {
        let target = terminal.clone();
        match tokio::task::spawn_blocking(move || target.write_input(&chunk)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                debug!(terminal = %id, %error, "PTY input closed");
                break;
            }
            Err(error) => {
                warn!(terminal = %id, %error, "input write task failed");
                break;
            }
        }
    }
    ended.store(true, Ordering::Release);
}

/// Length of `value` as JSON, without building it.
fn encoded_len(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    // A row that cannot be serialized would fail the response anyway; count it as oversized.
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

fn encode_render(id: TerminalId, render: &TerminalRender) -> Option<Frame> {
    match Frame::terminal_render(id, render) {
        Ok(frame) if frame.payload.len() <= MAX_PAYLOAD => Some(frame),
        Ok(frame) => {
            warn!(terminal = %id, len = frame.payload.len(), "render exceeds the frame limit");
            None
        }
        Err(error) => {
            warn!(terminal = %id, %error, "failed to encode a render");
            None
        }
    }
}

struct RenderTask {
    id: TerminalId,
    terminal: Terminal,
    conn: ConnHandle,
    tracker: RenderTracker,
    damage: watch::Receiver<u64>,
    acks: watch::Receiver<u64>,
    gate: Arc<Mutex<bool>>,
    cancel: CancellationToken,
}

/// One attachment: credit 1. After a render is sent the loop waits for its ack, then for
/// damage (immediately if some happened meanwhile), then for the coalescing interval, and
/// renders the latest state. No render is produced while credit is 0.
async fn render_loop(task: RenderTask) {
    let RenderTask {
        id,
        terminal,
        conn,
        mut tracker,
        mut damage,
        mut acks,
        gate,
        cancel,
    } = task;
    let mut last_sent = tracker.frame_number();
    let mut last_sent_at = Instant::now();
    loop {
        let acked = async { acks.wait_for(|ack| *ack == last_sent).await.is_ok() };
        tokio::select! {
            _ = cancel.cancelled() => return,
            acked = acked => if !acked { return },
        }

        let damaged = async { damage.changed().await.is_ok() };
        tokio::select! {
            _ = cancel.cancelled() => return,
            damaged = damaged => if !damaged { return },
        }

        tokio::select! {
            _ = cancel.cancelled() => return,
            () = sleep_until(last_sent_at + COALESCE) => {}
        }

        // Everything up to here is covered by the render below; later damage wakes the loop.
        damage.borrow_and_update();
        let Some(render) = terminal.render(&mut tracker) else {
            continue;
        };
        let Some(frame) = encode_render(id, &render) else {
            return;
        };
        let queued = {
            let live = lock(&gate);
            *live && conn.enqueue(Outbound::Frame(frame))
        };
        if !queued {
            return;
        }
        last_sent = tracker.frame_number();
        last_sent_at = Instant::now();
    }
}
