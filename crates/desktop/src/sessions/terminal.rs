//! [`TerminalSession`]: one attachment to one daemon terminal, shared by every view of it.
//!
//! The session owns the [`TerminalHandle`] (one attachment per terminal per connection), pumps
//! its updates into the [`SharedTerminal`] model that pane tabs draw, and
//! turns what happens to the terminal (reconnect, restart, the session ending) into the
//! [`ViewStatus`] all views show. Dropping the session detaches.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::task::Poll;

use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Task, WeakEntity};
use hutan_client::{
    BlockList, ClientError, Connection, GoneReason, TerminalHandle, TerminalUpdate,
};
use hutan_core::{Epoch, SessionId, TerminalId};
use hutan_protocol::blocks::{Block, BlockPart};
use hutan_protocol::messages::{
    Request, SessionInfo, TerminalColors, TerminalHistory, TerminalHistoryRows, TerminalSize,
};
use hutan_protocol::render::TerminalRender;
use hutan_terminal_view::{DefaultColors, SharedTerminal, TerminalBackend, ViewStatus};

use super::attention::ended_text;

type SharedHandle = Rc<RefCell<Option<TerminalHandle>>>;

pub(crate) fn terminal_colors(colors: DefaultColors) -> TerminalColors {
    let tuple = |c: hutan_terminal_view::Rgb| (c.0, c.1, c.2);
    TerminalColors {
        foreground: tuple(colors.foreground),
        background: tuple(colors.background),
        cursor: tuple(colors.cursor),
    }
}

/// The folder the shell is in now: its OSC 7 folder, else the one it started in.
fn info_folder(info: &SessionInfo) -> Option<PathBuf> {
    info.folder.clone().or_else(|| info.cwd.clone())
}

/// What views of a terminal ask of the daemon, through the session's handle.
struct SessionBackend {
    handle: SharedHandle,
    connection: Connection,
    shared: WeakEntity<SharedTerminal>,
    cx: AsyncApp,
    /// Where the shell is (the session's `folder`, else the folder it started in): relative
    /// paths in output that no block places are looked for there.
    folder: RefCell<Option<PathBuf>>,
}

impl TerminalBackend for SessionBackend {
    fn input(&self, bytes: Vec<u8>) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.input(bytes);
        }
    }

    fn resize(&self, size: TerminalSize) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.resize(size);
        }
    }

    fn ack(&self, frame: u64) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.ack(frame);
        }
    }

    fn request_history(&self, from_line: u32, count: u32) {
        let Some(terminal) = self.handle.borrow().as_ref().map(TerminalHandle::terminal) else {
            return;
        };
        let connection = self.connection.clone();
        let shared = self.shared.clone();
        self.cx
            .spawn(async move |cx| {
                let request = Request::TerminalHistory(TerminalHistory {
                    terminal_id: terminal,
                    from_line,
                    count,
                });
                match connection.call::<TerminalHistoryRows>(request).await {
                    Ok(page) => {
                        shared
                            .update(cx, |shared, cx| {
                                shared.apply_history(page.from_line, page.rows, cx);
                            })
                            .ok();
                    }
                    Err(err) => tracing::debug!("terminal.history failed: {err}"),
                }
            })
            .detach();
    }

    fn set_default_colors(&self, colors: DefaultColors) {
        if let Some(handle) = self.handle.borrow().as_ref() {
            handle.set_colors(terminal_colors(colors));
        }
    }

    fn working_dir(&self) -> Option<PathBuf> {
        self.folder.borrow().clone()
    }

    fn block_text(&self, block_id: u64, part: BlockPart) -> Task<Option<String>> {
        let Some(terminal) = self.handle.borrow().as_ref().map(TerminalHandle::terminal) else {
            return Task::ready(None);
        };
        let connection = self.connection.clone();
        // The daemon clamps the limit to its own (512 KiB).
        self.cx.spawn(async move |_| {
            match connection
                .terminal_block_text(terminal, block_id, part, u32::MAX)
                .await
            {
                Ok(answer) => Some(answer.text),
                Err(err) => {
                    tracing::debug!("terminal.block_text failed: {err}");
                    None
                }
            }
        })
    }
}

pub struct TerminalSession {
    session: SessionId,
    shared: Entity<SharedTerminal>,
    handle: SharedHandle,
    backend: Rc<SessionBackend>,
    connection: Connection,
    info: Option<SessionInfo>,
    reconnecting: bool,
    /// Why the daemon-side terminal is unreachable for good.
    gone: Option<String>,
    /// The daemon restarted: its terminals are gone, but a session it respawned keeps its id and
    /// gets a new terminal, which the store's next word on the session names. While this is
    /// set, the first such word rebinds the session (and clears `gone`).
    awaiting_respawn: bool,
    /// The daemon (epoch) the attached terminal belongs to, and the one `info` is the word of.
    epoch: Option<Epoch>,
    info_epoch: Option<Epoch>,
    colors: Option<DefaultColors>,
    /// The user stopped the daemon from the app; nothing reconnects until they start it.
    daemon_stopped: bool,
    /// The block list must be fetched with the next snapshot: a fresh attachment, or the
    /// connection dropped in between and events were missed.
    blocks_stale: bool,
    /// While a fetch is under way: the `terminal.block` events that arrived meanwhile, applied
    /// on top of its answer (a plain replace would lose the ones newer than the answer).
    blocks_fetch: Option<Vec<Block>>,
    _blocks_task: Task<()>,
    _pump: Task<()>,
}

impl std::fmt::Debug for TerminalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalSession")
            .field("session", &self.session)
            .field("terminal", &self.terminal())
            .field("reconnecting", &self.reconnecting)
            .field("gone", &self.gone)
            .finish_non_exhaustive()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Views may still hold the backend; the attachment ends with the session.
        self.handle.borrow_mut().take();
    }
}

impl TerminalSession {
    /// Attaches to the terminal of `info` (the word of the daemon of `epoch`); `None` when the
    /// session has no terminal (yet).
    pub fn attach(
        info: SessionInfo,
        epoch: Option<Epoch>,
        connection: Connection,
        colors: Option<DefaultColors>,
        cx: &mut App,
    ) -> Option<Entity<Self>> {
        let terminal = info.terminal_id?;
        Some(cx.new(|cx| {
            let shared = cx.new(|_| SharedTerminal::new());
            let handle: SharedHandle = Rc::new(RefCell::new(None));
            let backend = Rc::new(SessionBackend {
                handle: Rc::clone(&handle),
                connection: connection.clone(),
                shared: shared.downgrade(),
                cx: cx.to_async(),
                folder: RefCell::new(info_folder(&info)),
            });
            let mut this = Self {
                session: info.id,
                shared,
                handle,
                backend,
                connection,
                info: Some(info),
                epoch,
                info_epoch: epoch,
                reconnecting: false,
                gone: None,
                awaiting_respawn: false,
                colors,
                daemon_stopped: false,
                blocks_stale: true,
                blocks_fetch: None,
                _blocks_task: Task::ready(()),
                _pump: Task::ready(()),
            };
            this.bind(terminal, cx);
            this
        }))
    }

    /// Opens a fresh handle on `terminal` and starts pumping it.
    fn bind(&mut self, terminal: TerminalId, cx: &mut Context<Self>) {
        let handle = self.connection.attach(terminal, None);
        if let Some(colors) = self.colors {
            handle.set_colors(terminal_colors(colors));
        }
        *self.handle.borrow_mut() = Some(handle);
        let handle = Rc::clone(&self.handle);
        self._pump = cx.spawn(async move |this, cx| {
            loop {
                let update = std::future::poll_fn(|task_cx| match handle.borrow_mut().as_mut() {
                    Some(h) => h.updates().poll_recv(task_cx),
                    None => Poll::Ready(None),
                })
                .await;
                let Some(update) = update else { break };
                if this
                    .update(cx, |this, cx| this.on_update(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    fn on_update(&mut self, update: TerminalUpdate, cx: &mut Context<Self>) {
        match update {
            TerminalUpdate::Render(render) => {
                self.reconnecting = false;
                let snapshot = matches!(render, TerminalRender::Snapshot(_));
                self.shared
                    .update(cx, |shared, cx| shared.apply_render(render, cx));
                self.refresh_status(cx);
                if snapshot && self.blocks_stale {
                    self.fetch_blocks(cx);
                }
            }
            TerminalUpdate::Reconnecting => {
                self.reconnecting = true;
                // Events during the gap are lost: the next snapshot refetches the list.
                self.blocks_stale = true;
                self.refresh_status(cx);
            }
            TerminalUpdate::Gone { reason } => match reason {
                GoneReason::SessionRestarted { new_terminal } => self.rebind(new_terminal, cx),
                GoneReason::EpochChanged => {
                    self.awaiting_respawn = true;
                    self.gone = Some("The daemon restarted; this session is gone".to_owned());
                    // The store may already know the respawned terminal.
                    if !self.rebind_to_respawn(cx) {
                        self.refresh_status(cx);
                    }
                }
                GoneReason::NotFound => {
                    self.gone = Some("The daemon no longer knows this terminal".to_owned());
                    self.refresh_status(cx);
                }
            },
        }
    }

    /// The session restarted into another terminal: start over on it. The views are told
    /// `Reconnecting` until its first snapshot arrives, which makes them report their size.
    fn rebind(&mut self, new_terminal: TerminalId, cx: &mut Context<Self>) {
        self.shared.update(cx, |shared, cx| shared.reset(cx));
        self.blocks_stale = true;
        self.blocks_fetch = None;
        self._blocks_task = Task::ready(());
        self.reconnecting = true;
        self.bind(new_terminal, cx);
        self.refresh_status(cx);
    }

    /// After a daemon restart: attaches to the terminal the session was respawned with, once
    /// the store has told us about one of the new daemon (terminal ids start over with every
    /// daemon, so only the epoch tells its word from the old one). A session that really ended
    /// (or has no terminal) stays ended. Returns whether it rebound.
    fn rebind_to_respawn(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.awaiting_respawn || self.info_epoch.is_none() || self.info_epoch == self.epoch {
            return false;
        }
        let Some(new_terminal) = self
            .info
            .as_ref()
            .filter(|info| ended_text(&info.state).is_none())
            .and_then(|info| info.terminal_id)
        else {
            return false;
        };
        self.awaiting_respawn = false;
        self.gone = None;
        self.epoch = self.info_epoch;
        self.rebind(new_terminal, cx);
        true
    }

    /// Loads the terminal's blocks (the daemon keeps at most 2 000). A daemon without capability
    /// `terminal.blocks` has none; a failed fetch is retried with the next snapshot.
    fn fetch_blocks(&mut self, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal() else {
            return;
        };
        if !self.connection.supports_blocks() {
            // Not connected (the snapshot was a replay) or an older daemon: the next snapshot
            // after a reconnect asks again.
            return;
        }
        self.blocks_stale = false;
        self.blocks_fetch = Some(Vec::new());
        let connection = self.connection.clone();
        self._blocks_task = cx.spawn(async move |this, cx| {
            let result = connection.fetch_blocks(terminal).await;
            this.update(cx, |this, cx| this.finish_fetch(terminal, result, cx))
                .ok();
        });
    }

    fn finish_fetch(
        &mut self,
        terminal: TerminalId,
        result: Result<BlockList, ClientError>,
        cx: &mut Context<Self>,
    ) {
        if self.terminal() != Some(terminal) {
            return;
        }
        let buffered = self.blocks_fetch.take().unwrap_or_default();
        match result {
            Ok(mut list) => {
                for block in buffered {
                    list.apply(block);
                }
                self.shared
                    .update(cx, |shared, cx| shared.set_blocks(list, cx));
            }
            Err(err) => {
                tracing::debug!("terminal.blocks failed: {err}");
                self.blocks_stale = true;
            }
        }
    }

    /// A `terminal.block` event for `terminal`; ignored when this session shows another one.
    pub fn apply_block_event(
        &mut self,
        terminal: TerminalId,
        block: Block,
        cx: &mut Context<Self>,
    ) {
        if self.terminal() != Some(terminal) {
            return;
        }
        match &mut self.blocks_fetch {
            Some(buffered) => buffered.push(block),
            None => self
                .shared
                .update(cx, |shared, cx| shared.apply_block(block, cx)),
        }
    }

    fn compute_status(&self) -> ViewStatus {
        if let Some(reason) = &self.gone {
            // The restarted daemon's word on the session says how it really ended.
            let ended = self
                .info
                .as_ref()
                .filter(|_| self.awaiting_respawn)
                .and_then(|info| ended_text(&info.state));
            return ViewStatus::Ended(ended.unwrap_or_else(|| reason.clone()));
        }
        if self.daemon_stopped {
            return ViewStatus::Ended("The daemon is stopped".to_owned());
        }
        match &self.info {
            None => return ViewStatus::Ended("The session no longer exists".to_owned()),
            Some(info) => {
                if let Some(text) = ended_text(&info.state) {
                    return ViewStatus::Ended(text);
                }
            }
        }
        if self.reconnecting {
            ViewStatus::Reconnecting
        } else {
            ViewStatus::Live
        }
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        let status = self.compute_status();
        self.shared
            .update(cx, |shared, cx| shared.set_status(status, cx));
        cx.notify();
    }

    /// The store's latest word on the session (`None`: it is gone from the daemon), from the
    /// daemon of `epoch`.
    pub fn update_info(
        &mut self,
        info: Option<SessionInfo>,
        epoch: Option<Epoch>,
        cx: &mut Context<Self>,
    ) {
        if self.info == info && self.info_epoch == epoch {
            return;
        }
        *self.backend.folder.borrow_mut() = info.as_ref().and_then(info_folder);
        self.info = info;
        self.info_epoch = epoch;
        if !self.rebind_to_respawn(cx) {
            self.refresh_status(cx);
        }
    }

    /// The user stopped (or started again) the daemon from the app.
    pub fn set_daemon_stopped(&mut self, stopped: bool, cx: &mut Context<Self>) {
        if self.daemon_stopped != stopped {
            self.daemon_stopped = stopped;
            self.refresh_status(cx);
        }
    }

    /// The default colors the daemon answers OSC 10/11/12 queries with; remembered for rebinds.
    pub fn set_colors(&mut self, colors: DefaultColors) {
        if self.colors == Some(colors) {
            return;
        }
        self.colors = Some(colors);
        self.backend.set_default_colors(colors);
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn session_id(&self) -> SessionId {
        self.session
    }

    /// The terminal currently attached, if any.
    pub fn terminal(&self) -> Option<TerminalId> {
        self.handle.borrow().as_ref().map(TerminalHandle::terminal)
    }

    /// The daemon (epoch) the attached terminal belongs to.
    pub fn daemon_epoch(&self) -> Option<Epoch> {
        self.epoch
    }

    pub fn info(&self) -> Option<&SessionInfo> {
        self.info.as_ref()
    }

    /// The model every view of this terminal draws.
    pub fn shared(&self) -> &Entity<SharedTerminal> {
        &self.shared
    }

    /// What views send input, resizes, acks and history requests through.
    pub fn backend(&self) -> Rc<dyn TerminalBackend> {
        self.backend.clone()
    }

    pub fn status(&self) -> ViewStatus {
        self.compute_status()
    }
}
