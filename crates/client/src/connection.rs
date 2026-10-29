//! [`Connection`]: the reconnecting, executor-independent client that UIs build on.
//!
//! GPUI drives its own executor, so a `Connection` owns one OS thread (`hutan-client`) running
//! a current-thread Tokio runtime with the connection task. Everything public is a channel, a
//! `watch` or a `broadcast` handle; none of them needs a Tokio context, so they can be used from
//! any executor or plain thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use hutan_core::{ApprovalId, QuestionId, QuestionResponse, SessionId, TerminalId, WorkspaceId};
use hutan_protocol::account::{
    AccountInfo, AccountList, AccountRemoved, AccountSession, ManagedAccount,
};
use hutan_protocol::assistant::{
    AssistantConfig, AssistantConfigure, AssistantEntry, AssistantListRequest, AssistantOpen,
    AssistantScope, Attachment,
};
use hutan_protocol::blocks::{
    BlockPart, TerminalBlockText, TerminalBlockTextResult, TerminalBlocks, TerminalBlocksResult,
};
use hutan_protocol::chat::{
    ApprovalInfo, ChatCreate, ChatFork, ChatForked, ChatHistory, ChatModels, ChatQueue, ChatSent,
    ChatSentNow, QuestionInfo, QueuedMessage,
};
use hutan_protocol::daemon_settings::DaemonSettings;
use hutan_protocol::history::{HistoryHits, HistorySearch, HistorySetEnabled, HistoryStatusInfo};
use hutan_protocol::messages::{
    AgentReleaseRequest, AgentReportRequest, Empty, Request, SessionInfo, TerminalHistoryRows,
    TerminalSize,
};
use hutan_protocol::resources::{DEFAULT_INTERVAL_MS, ResourcesSubscribed};
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchResult};
use serde::de::DeserializeOwned;
use tokio::sync::{Notify, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::blocks::BlockList;
use crate::chat::ChatUpdate;
use crate::client::{Client, ConnectOptions};
use crate::endpoint::Endpoint;
use crate::error::ClientError;
use crate::store::{ConnectionState, Notice, StoreState};
use crate::supervisor::Supervisor;
use crate::terminal::{Command, TerminalHandle, UPDATE_BUFFER, send_command};

/// Name of the connection's thread; tests look for it by this prefix.
const THREAD_NAME: &str = "hutan-client";
/// Notices buffered per receiver before it starts lagging.
const NOTICE_BUFFER: usize = 256;
/// Chat updates buffered per receiver before it starts lagging (streaming deltas are many).
const CHAT_BUFFER: usize = 4096;
/// Terminal commands queued for the connection task.
const COMMAND_BUFFER: usize = 1024;

/// State shared between the public handles and the connection task.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) state: watch::Sender<Arc<StoreState>>,
    pub(crate) notices: broadcast::Sender<Notice>,
    pub(crate) chat: broadcast::Sender<ChatUpdate>,
    /// The live, synchronised connection; `None` whenever disconnected.
    client: Mutex<Option<Client>>,
    /// Counts the connections made so far: bumped, under the `client` lock, each time a
    /// synchronised client is installed. Lets a caller tell whether something it did on "the
    /// connection" was done on the one that is current now.
    generation: AtomicU64,
    commands: mpsc::Sender<Command>,
    next_handle: AtomicU64,
    /// Whether a missing daemon may be started (see [`Connection::set_autostart`]).
    pub(crate) autostart: AtomicBool,
    /// Wakes a backoff wait so a reconnect happens now.
    pub(crate) kick: Notify,
    /// The interval of the resource samples somebody asked for
    /// ([`Connection::resources_subscribe`]); a reconnect subscribes again while it is set.
    resources: Mutex<Option<u32>>,
    /// The files somebody watches ([`Connection::fs_watch`]) and whether each was picked; a
    /// reconnect watches them again.
    watched: Mutex<std::collections::BTreeMap<PathBuf, bool>>,
}

impl Shared {
    fn client(&self) -> Option<Client> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The live client together with its generation, read under one lock.
    fn client_generation(&self) -> Option<(u64, Client)> {
        let guard = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        let client = guard.clone()?;
        Some((self.generation.load(Ordering::Acquire), client))
    }

    pub(crate) fn resources_interval(&self) -> Option<u32> {
        *self
            .resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn set_resources_interval(&self, interval: Option<u32>) {
        *self
            .resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = interval;
    }

    /// The watched files, as `(path, picked)`.
    pub(crate) fn watched_files(&self) -> Vec<(PathBuf, bool)> {
        self.watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(path, picked)| (path.clone(), *picked))
            .collect()
    }

    pub(crate) fn set_client(&self, client: Option<Client>) {
        let mut guard = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if client.is_some() {
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
        *guard = client;
    }
}

/// Stops the connection thread when the last [`Connection`] clone goes away (or on shutdown).
#[derive(Debug)]
struct Owner {
    cancel: CancellationToken,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Owner {
    /// Cancels the task and waits for the thread to end. Idempotent.
    fn stop(&self) {
        self.cancel.cancel();
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
            && thread.join().is_err()
        {
            error!("the hutan-client thread panicked");
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A connection to `hutand` that reconnects by itself and keeps a [`StoreState`] current.
///
/// Clones share the connection; it ends when the last clone is dropped or
/// [`shutdown`](Self::shutdown) is called on any clone.
#[derive(Debug, Clone)]
pub struct Connection {
    shared: Arc<Shared>,
    owner: Arc<Owner>,
    endpoint: Option<Endpoint>,
}

impl Connection {
    /// Returns immediately. Connects (starting `hutand` per `opts.autostart`; after a start that
    /// failed the next waits 1 s, doubling to 60 s, and after [`crate::AUTOSTART_GIVE_UP`]
    /// failures of one kind in a row none follows until [`start_daemon_now`](Self::start_daemon_now))
    /// and reconnects with exponential backoff (100 ms doubling to 5 s, ±20 % jitter) until
    /// dropped or shut down. A daemon that announces a stop someone asked for
    /// ([`StoreState::stopped_by`]) turns autostart off.
    pub fn start(opts: ConnectOptions) -> Connection {
        let autostart = opts.autostart.is_some();
        Self::start_with(opts, autostart)
    }

    /// Like [`start`](Self::start), but a missing daemon is **not** started until
    /// [`set_autostart`](Self::set_autostart) allows it: the connection only dials. For a
    /// daemon that something else (launchd, "keep running") starts; there is no window in which
    /// the first dial could still autostart, as there would be with `start` followed by
    /// `set_autostart(false)`.
    pub fn start_held(opts: ConnectOptions) -> Connection {
        Self::start_with(opts, false)
    }

    fn start_with(opts: ConnectOptions, autostart: bool) -> Connection {
        let endpoint = opts.endpoint.clone().or_else(Endpoint::default_local);
        let (state, _) = watch::channel(Arc::new(StoreState::initial()));
        let (notices, _) = broadcast::channel(NOTICE_BUFFER);
        let (chat, _) = broadcast::channel(CHAT_BUFFER);
        let (commands, command_rx) = mpsc::channel(COMMAND_BUFFER);
        let shared = Arc::new(Shared {
            state,
            notices,
            chat,
            client: Mutex::new(None),
            generation: AtomicU64::new(0),
            commands,
            next_handle: AtomicU64::new(1),
            autostart: AtomicBool::new(autostart),
            kick: Notify::new(),
            resources: Mutex::new(None),
            watched: Mutex::new(std::collections::BTreeMap::new()),
        });
        let cancel = CancellationToken::new();
        let supervisor = Supervisor::new(opts, shared.clone(), command_rx, cancel.clone());
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || run_thread(supervisor));
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(e) => {
                error!(error = %e, "cannot start the hutan-client thread");
                shared.state.send_modify(|state| {
                    Arc::make_mut(state).connection = ConnectionState::Stopped;
                });
                None
            }
        };
        Connection {
            shared,
            owner: Arc::new(Owner {
                cancel,
                thread: Mutex::new(thread),
            }),
            endpoint,
        }
    }

    /// The latest full state. Coalesced: a receiver that is slow sees the newest state, not
    /// every intermediate one.
    pub fn state(&self) -> watch::Receiver<Arc<StoreState>> {
        self.shared.state.subscribe()
    }

    /// Transient events (bell, exit, restart). A receiver that falls behind misses the oldest.
    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.shared.notices.subscribe()
    }

    /// Chat events of every session, in order, as they arrive; see [`ChatUpdate`] for how a
    /// consumer keeps a transcript exact across reconnects. A receiver that falls behind gets
    /// `RecvError::Lagged` and must reload the history.
    pub fn chat_updates(&self) -> broadcast::Receiver<ChatUpdate> {
        self.shared.chat.subscribe()
    }

    /// What this connection dials (`ConnectOptions::endpoint`, else the local default).
    pub fn endpoint(&self) -> Option<&Endpoint> {
        self.endpoint.as_ref()
    }

    /// The Unix socket this connection dials, when its endpoint is local.
    pub fn socket_path(&self) -> Option<&Path> {
        self.endpoint.as_ref().and_then(Endpoint::local_socket)
    }

    /// Allows or forbids starting `hutand` when nothing listens (it has no effect without
    /// [`ConnectOptions::autostart`]). Forbidding keeps the connection retrying quietly, so a
    /// daemon the user stopped is not brought back; allowing it again reconnects at once and
    /// starts the daemon without waiting out the backoff of earlier failed starts.
    pub fn set_autostart(&self, enabled: bool) {
        let was = self.shared.autostart.swap(enabled, Ordering::AcqRel);
        if enabled && !was {
            self.shared.kick.notify_one();
        }
    }

    /// Allows autostart and dials now, starting `hutand` from scratch (no backoff, a halted
    /// autostart and an announced stop forgotten): what *Start* and *Retry* do.
    pub fn start_daemon_now(&self) {
        self.shared.autostart.store(true, Ordering::Release);
        self.shared.kick.notify_one();
    }

    /// Whether a missing daemon is started ([`set_autostart`](Self::set_autostart)).
    pub fn autostart(&self) -> bool {
        self.shared.autostart.load(Ordering::Acquire)
    }

    /// Sends `req` on the current connection. Fails with [`ClientError::Disconnected`] when
    /// not connected, and when the connection drops mid-call; a call is never queued across a
    /// reconnect.
    pub async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        req: Request,
    ) -> Result<T, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.call(req).await
    }

    /// True while connected to a daemon that advertised capability `terminal.blocks`
    /// ([`Client::supports_blocks`]); false when disconnected.
    pub fn supports_blocks(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_blocks())
    }

    /// Every block the daemon keeps for `terminal` ([`BlockList::fetch`] on the current
    /// connection).
    pub async fn fetch_blocks(&self, terminal: TerminalId) -> Result<BlockList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        BlockList::fetch(&client, terminal).await
    }

    /// One page of `terminal.history` with the viewport top it was counted from (see
    /// [`Client::terminal_history_anchored`]).
    pub async fn terminal_history_anchored(
        &self,
        terminal: TerminalId,
        from_line: u32,
        count: u32,
    ) -> Result<(Option<u64>, TerminalHistoryRows), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client
            .terminal_history_anchored(terminal, from_line, count)
            .await
    }

    /// One page of `terminal`'s blocks, newest first (see [`Client::terminal_blocks`]).
    pub async fn terminal_blocks(
        &self,
        terminal: TerminalId,
        before_id: Option<u64>,
        limit: u32,
    ) -> Result<TerminalBlocksResult, ClientError> {
        self.call(Request::TerminalBlocks(TerminalBlocks {
            terminal_id: terminal,
            before_id,
            limit,
        }))
        .await
    }

    /// Plain text of a block's command or output (see [`Client::terminal_block_text`]).
    pub async fn terminal_block_text(
        &self,
        terminal: TerminalId,
        block_id: u64,
        part: BlockPart,
        max_bytes: u32,
    ) -> Result<TerminalBlockTextResult, ClientError> {
        self.call(Request::TerminalBlockText(TerminalBlockText {
            terminal_id: terminal,
            block_id,
            part,
            max_bytes,
        }))
        .await
    }

    /// `terminal.search` (see [`Client::terminal_search`]).
    pub async fn terminal_search(
        &self,
        req: TerminalSearch,
    ) -> Result<TerminalSearchResult, ClientError> {
        self.call(Request::TerminalSearch(req)).await
    }

    /// True while connected to a daemon that advertised capability `terminal.search`
    /// ([`Client::supports_terminal_search`]); false when disconnected.
    pub fn supports_terminal_search(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_terminal_search())
    }

    /// True while connected to a daemon that advertised capability `fs` ([`Client::supports_fs`],
    /// protocol 0.33); false when disconnected.
    pub fn supports_fs(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_fs())
    }

    /// True while connected to a daemon that advertised capability `fs.ops`
    /// ([`Client::supports_fs_ops`], protocol 0.54); false when disconnected.
    pub fn supports_fs_ops(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_fs_ops())
    }

    /// `fs.create` (see [`Client::fs_create`]): an empty file or a folder.
    pub async fn fs_create(
        &self,
        req: hutan_protocol::fs::FsCreate,
    ) -> Result<hutan_protocol::fs::FsEntry, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_create(req).await
    }

    /// `fs.rename` (see [`Client::fs_rename`]).
    pub async fn fs_rename(
        &self,
        req: hutan_protocol::fs::FsRename,
    ) -> Result<hutan_protocol::fs::FsEntry, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_rename(req).await
    }

    /// `fs.move` (see [`Client::fs_move`]): a batch of renames, each reported.
    pub async fn fs_move(
        &self,
        req: hutan_protocol::fs::FsMove,
    ) -> Result<hutan_protocol::fs::FsOpResults, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_move(req).await
    }

    /// `fs.copy` (see [`Client::fs_copy`]): a batch of copies, each reported.
    pub async fn fs_copy(
        &self,
        req: hutan_protocol::fs::FsCopy,
    ) -> Result<hutan_protocol::fs::FsOpResults, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_copy(req).await
    }

    /// `fs.trash` (see [`Client::fs_trash`]): to the system Trash, with an undo ticket.
    pub async fn fs_trash(
        &self,
        req: hutan_protocol::fs::FsTrash,
    ) -> Result<hutan_protocol::fs::FsTrashed, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_trash(req).await
    }

    /// `fs.restore` (see [`Client::fs_restore`]): puts back what a trash ticket holds.
    pub async fn fs_restore(
        &self,
        req: hutan_protocol::fs::FsRestore,
    ) -> Result<hutan_protocol::fs::FsOpResults, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_restore(req).await
    }

    /// `fs.stat` (see [`Client::fs_stat`]): the canonical path, kind and size of `path`.
    pub async fn fs_stat(
        &self,
        path: &Path,
        picked: bool,
    ) -> Result<hutan_protocol::fs::FsEntry, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_stat(path, picked).await
    }

    /// The whole file, read chunk by chunk (see [`Client::fs_read_all`]).
    pub async fn fs_read_all(
        &self,
        path: &Path,
        picked: bool,
    ) -> Result<crate::FsText, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_read_all(path, picked).await
    }

    /// Writes `text` as a file, chunk by chunk ([`Client::fs_write`]): with
    /// `req.expect` set, a file that is no longer that version is refused with `conflict`
    /// (`data.current`: its version now).
    pub async fn fs_write(
        &self,
        req: hutan_protocol::fs::FsWriteBegin,
        text: &str,
    ) -> Result<hutan_protocol::fs::FsWritten, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_write(req, text).await
    }

    /// `fs.list` (see [`Client::fs_list`]): the entries of one folder, `.gitignore` matches
    /// flagged.
    pub async fn fs_list(
        &self,
        req: hutan_protocol::fs::FsList,
    ) -> Result<hutan_protocol::fs::FsListing, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_list(req).await
    }

    /// `fs.find` (see [`Client::fs_find`]): the files below a root that match a fuzzy query,
    /// best first.
    pub async fn fs_find(
        &self,
        req: hutan_protocol::fs::FsFind,
    ) -> Result<hutan_protocol::fs::FsFound, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_find(req).await
    }

    /// `fs.search` (see [`Client::fs_search`]): the matching lines below a root; it answers
    /// when the search ends, [`Notice::FsSearchProgress`] reports on it meanwhile.
    pub async fn fs_search(
        &self,
        req: hutan_protocol::fs::FsSearch,
    ) -> Result<hutan_protocol::fs::FsSearchResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_search(req).await
    }

    /// `fs.search.cancel` (see [`Client::fs_search_cancel`]): ends the search named
    /// `search_id` with what it found.
    pub async fn fs_search_cancel(&self, search_id: &str) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.fs_search_cancel(search_id).await
    }

    /// True while connected to a daemon that advertised capability `fs.watch`
    /// ([`Client::supports_fs_watch`]); false when disconnected.
    pub fn supports_fs_watch(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_fs_watch())
    }

    /// Watches the file `path` (`fs.watch`): its changes arrive as [`Notice::FsChanged`]. The
    /// wish is remembered: after a reconnect the connection watches it again by itself. Fails
    /// with [`ClientError::Disconnected`] while disconnected, but the wish stays.
    pub async fn fs_watch(&self, path: &Path, picked: bool) -> Result<(), ClientError> {
        self.shared
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned(), picked);
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client
            .fs_watch(hutan_protocol::fs::FsWatch {
                paths: vec![hutan_protocol::fs::FsWatchPath {
                    path: path.to_owned(),
                    recursive: false,
                }],
                picked,
            })
            .await
            .map(drop)
    }

    /// Stops watching `path` ([`fs_watch`](Self::fs_watch)); never fails (the daemon drops the
    /// watch with the connection anyway).
    pub async fn fs_unwatch(&self, path: &Path) {
        self.shared
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(path);
        if let Some(client) = self.shared.client() {
            let _ = client
                .fs_unwatch(hutan_protocol::fs::FsUnwatch {
                    paths: vec![path.to_owned()],
                })
                .await;
        }
    }

    /// True while connected to a daemon that advertised capability `history.search`
    /// ([`Client::supports_history`]); false when disconnected.
    pub fn supports_history(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_history())
    }

    /// `history.search`: one hit per matching session, best first. Fails with the daemon's
    /// `invalid_state` while indexing is off.
    pub async fn history_search(&self, search: HistorySearch) -> Result<HistoryHits, ClientError> {
        self.call(Request::HistorySearch(search)).await
    }

    /// `history.status`: whether indexing is on, per-source counters, database size.
    pub async fn history_status(&self) -> Result<HistoryStatusInfo, ClientError> {
        self.call(Request::HistoryStatus(Empty {})).await
    }

    /// True while connected to a daemon that advertised capability `history.sessions`
    /// ([`Client::supports_history_sessions`]); false when disconnected.
    pub fn supports_history_sessions(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_history_sessions())
    }

    /// `history.sessions` ([`Client::history_sessions`]): the session catalog.
    pub async fn history_sessions(
        &self,
        request: hutan_protocol::history::HistorySessionsRequest,
    ) -> Result<hutan_protocol::history::HistorySessions, ClientError> {
        self.call(Request::HistorySessions(request)).await
    }

    /// True while connected to a daemon that advertised capability `session.close`
    /// ([`Client::supports_session_close`]); false when disconnected.
    pub fn supports_session_close(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_session_close())
    }

    /// `session.close` ([`Client::session_close`]). A refusal because something runs in the
    /// session carries [`ClientError::busy`].
    pub async fn session_close(
        &self,
        session: SessionId,
        stop_running: bool,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.session_close(session, stop_running).await
    }

    /// `session.reopen` ([`Client::session_reopen`]): brings a closed session back.
    pub async fn session_reopen(&self, session: SessionId) -> Result<SessionInfo, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.session_reopen(session).await
    }

    /// `history.set_enabled`: turns indexing on or off (recorded in `daemon.toml`); answers with
    /// the new status.
    pub async fn history_set_enabled(
        &self,
        enabled: bool,
    ) -> Result<HistoryStatusInfo, ClientError> {
        self.call(Request::HistorySetEnabled(HistorySetEnabled { enabled }))
            .await
    }

    /// `history.clear`: deletes the index and, while indexing is on, starts an empty one.
    pub async fn history_clear(&self) -> Result<(), ClientError> {
        self.call::<serde_json::Value>(Request::HistoryClear(Empty {}))
            .await
            .map(drop)
    }

    /// `history.rebuild`: reads every transcript again in the background. Fails with the
    /// daemon's `invalid_state` while indexing is off.
    pub async fn history_rebuild(&self) -> Result<(), ClientError> {
        self.call::<serde_json::Value>(Request::HistoryRebuild(Empty {}))
            .await
            .map(drop)
    }

    /// True while connected to a daemon that advertised capability `chat.codex`
    /// ([`Client::supports_chat`]); false when disconnected.
    pub fn supports_chat(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat())
    }

    /// True while connected to a daemon that advertised capability `agent.tree`
    /// ([`Client::supports_agent_tree`]): it reports subagents. False when disconnected.
    pub fn supports_agent_tree(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_agent_tree())
    }

    /// True while connected to a daemon that advertised capability `resources`
    /// ([`Client::supports_resources`]); false when disconnected.
    pub fn supports_resources(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_resources())
    }

    /// Asks for `resources.updated` samples every `interval_ms` (default 2000, at least 1000;
    /// [`Client::resources_subscribe`]). They arrive as [`Notice::ResourcesUpdated`]. The
    /// subscription is remembered: after a reconnect the connection subscribes again by itself
    /// and announces the first sample, until [`resources_unsubscribe`](Self::resources_unsubscribe).
    /// Disconnected, this fails with [`ClientError::Disconnected`] but the wish stays, so the
    /// first sample comes with the next connection. The daemon samples only while somebody is
    /// subscribed.
    pub async fn resources_subscribe(
        &self,
        interval_ms: Option<u32>,
    ) -> Result<ResourcesSubscribed, ClientError> {
        let interval = interval_ms.unwrap_or(DEFAULT_INTERVAL_MS);
        self.shared.set_resources_interval(Some(interval));
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        let result = client.resources_subscribe(Some(interval)).await;
        if matches!(result, Err(ClientError::Remote(_))) {
            // Refused (an interval below the floor, an old daemon): do not retry on reconnects.
            self.shared.set_resources_interval(None);
        }
        result
    }

    /// Ends [`resources_subscribe`](Self::resources_subscribe): forgets it for reconnects and
    /// tells the daemon, which stops sampling with its last subscriber. Never fails; a daemon
    /// that cannot be told drops the subscription with the connection.
    pub async fn resources_unsubscribe(&self) {
        self.shared.set_resources_interval(None);
        if let Some(client) = self.shared.client() {
            let _ = client.resources_unsubscribe().await;
        }
    }

    /// `agent.report` ([`Client::agent_report`]).
    pub async fn agent_report(&self, req: AgentReportRequest) -> Result<bool, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.agent_report(req).await
    }

    /// `agent.release` ([`Client::agent_release`]).
    pub async fn agent_release(&self, req: AgentReleaseRequest) -> Result<bool, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.agent_release(req).await
    }

    /// True while connected to a daemon that advertised capability `notify.intents`
    /// ([`Client::supports_notify`]); false when disconnected.
    pub fn supports_notify(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_notify())
    }

    /// True while connected to a daemon that advertised capability `notify.mute`
    /// ([`Client::supports_notify_mute`]); false when disconnected.
    pub fn supports_notify_mute(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_notify_mute())
    }

    /// True while connected to a daemon that advertised capability `worktree`
    /// ([`Client::supports_worktrees`]); false when disconnected.
    pub fn supports_worktrees(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_worktrees())
    }

    /// True while connected to a daemon that advertised capability `worktree.setup`
    /// ([`Client::supports_worktree_setup`]); false when disconnected.
    pub fn supports_worktree_setup(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_worktree_setup())
    }

    /// True while connected to a daemon that advertised capability `control`
    /// ([`Client::supports_control`]); false when disconnected.
    pub fn supports_control(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_control())
    }

    /// `ui.publish` on the current connection, answering the connection's
    /// [`generation`](Self::generation) it went out on (a window publishes again after a
    /// reconnect).
    pub async fn ui_publish(
        &self,
        publish: hutan_protocol::ui::UiPublish,
    ) -> Result<u64, ClientError> {
        let (generation, client) = self
            .shared
            .client_generation()
            .ok_or(ClientError::Disconnected)?;
        client.ui_publish(publish).await?;
        Ok(generation)
    }

    /// `ui.command_result` on the current connection.
    pub async fn ui_command_result(
        &self,
        result: hutan_protocol::ui::UiCommandResult,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.ui_command_result(result).await
    }

    /// True while connected to a daemon that advertised capability `task`
    /// ([`Client::supports_tasks`]); false when disconnected.
    pub fn supports_tasks(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_tasks())
    }

    /// True while connected to a daemon that advertised capability `worktree.review`
    /// ([`Client::supports_worktree_review`]); false when disconnected.
    pub fn supports_worktree_review(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_worktree_review())
    }

    /// True while connected to a daemon that advertised capability `chat.rich_input`
    /// ([`Client::supports_rich_input`]); false when disconnected.
    pub fn supports_rich_input(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_rich_input())
    }

    /// True while connected to a daemon that advertised capability `task.review`
    /// ([`Client::supports_task_review`]); false when disconnected.
    pub fn supports_task_review(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_task_review())
    }

    /// True while connected to a daemon that advertised capability `session.recovery`
    /// ([`Client::supports_recovery`]); false when disconnected.
    pub fn supports_recovery(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_recovery())
    }

    /// True while connected to a daemon that advertised capability `task.chat_model`
    /// ([`Client::supports_task_chat_model`]); false when disconnected.
    pub fn supports_task_chat_model(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_task_chat_model())
    }

    /// The number of the current connection: it grows each time the connection is made again
    /// (see [`client_focus`](Self::client_focus)); `0` before the first one.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// `client.focus` ([`Client::client_focus`]): the session the user is looking at. The daemon
    /// forgets it when the connection ends. Answers the [`generation`](Self::generation) of the
    /// connection it was reported on: a caller that finds the current one different reports
    /// again.
    pub async fn client_focus(&self, session: Option<SessionId>) -> Result<u64, ClientError> {
        let (generation, client) = self
            .shared
            .client_generation()
            .ok_or(ClientError::Disconnected)?;
        client.client_focus(session).await?;
        Ok(generation)
    }

    /// `chat.create`: opens an agent conversation as a new session.
    pub async fn chat_create(&self, req: ChatCreate) -> Result<SessionInfo, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_create(req).await
    }

    /// `chat.send`: the user's message; the turn's events follow on [`chat_updates`](Self::chat_updates).
    pub async fn chat_send(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<ChatSent, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_send(session, text).await
    }

    /// [`chat_send`](Self::chat_send) with read-only context attached (protocol 0.12, capability
    /// `assistant`): the daemon resolves, redacts, bounds and journals it with the message
    /// ([`Client::chat_send_with`]).
    pub async fn chat_send_with(
        &self,
        session: SessionId,
        text: impl Into<String>,
        attachments: Vec<Attachment>,
    ) -> Result<ChatSent, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_send_with(session, text, attachments).await
    }

    /// True while connected to a daemon that advertised capability `chat.retry`
    /// ([`Client::supports_chat_retry`]); false when disconnected.
    pub fn supports_chat_retry(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_retry())
    }

    /// True while connected to a daemon that advertised capability `chat.mode`
    /// ([`Client::supports_chat_mode`]); false when disconnected.
    pub fn supports_chat_mode(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_mode())
    }

    /// True while connected to a daemon that advertised capability `finish`
    /// ([`Client::supports_finish`]); false when disconnected.
    pub fn supports_finish(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_finish())
    }

    /// `chat.retry` ([`Client::chat_retry`]).
    pub async fn chat_retry(&self, session: SessionId) -> Result<ChatSent, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_retry(session).await
    }

    /// True while connected to a daemon that advertised capability `chat.fork`
    /// ([`Client::supports_chat_fork`]); false when disconnected.
    pub fn supports_chat_fork(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_fork())
    }

    /// True while connected to a daemon that advertised capability `chat.fork.worktree`
    /// ([`Client::supports_chat_fork_worktree`]); false when disconnected.
    pub fn supports_chat_fork_worktree(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_fork_worktree())
    }

    /// `chat.fork` ([`Client::chat_fork`]).
    pub async fn chat_fork(&self, fork: ChatFork) -> Result<ChatForked, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_fork(fork).await
    }

    /// True while connected to a daemon that advertised capability `assistant`
    /// ([`Client::supports_assistant`]); false when disconnected.
    pub fn supports_assistant(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_assistant())
    }

    /// `assistant.open`: the default assistant session of a scope, created on first use.
    pub async fn assistant_open(&self, req: AssistantOpen) -> Result<SessionInfo, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.assistant_open(req).await
    }

    /// `assistant.list`: the assistant sessions, oldest first.
    pub async fn assistant_list(
        &self,
        req: AssistantListRequest,
    ) -> Result<Vec<AssistantEntry>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.assistant_list(req).await
    }

    /// True while connected to a daemon that advertised capability `assistant.backend`
    /// ([`Client::supports_assistant_backend`]); false when disconnected.
    pub fn supports_assistant_backend(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_assistant_backend())
    }

    /// `assistant.configure`: chooses the backend (and model) assistants of a scope are created
    /// with from now on.
    pub async fn assistant_configure(
        &self,
        req: AssistantConfigure,
    ) -> Result<AssistantConfig, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.assistant_configure(req).await
    }

    /// True while connected to a daemon that advertised capability `daemon.settings`
    /// ([`Client::supports_daemon_settings`]); false when disconnected.
    pub fn supports_daemon_settings(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_daemon_settings())
    }

    /// `daemon.settings.get` ([`Client::daemon_settings`]): the options of `daemon.toml`.
    pub async fn daemon_settings(&self) -> Result<DaemonSettings, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.daemon_settings().await
    }

    /// `daemon.settings.set` ([`Client::daemon_setting_set`]): changes (`Some`) or removes
    /// (`None`) one option of `daemon.toml`.
    pub async fn daemon_setting_set(
        &self,
        key: &str,
        value: Option<serde_json::Value>,
    ) -> Result<DaemonSettings, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.daemon_setting_set(key, value).await
    }

    /// True while connected to a daemon that advertised capability `control.grants`
    /// ([`Client::supports_control_grants`]); false when disconnected.
    pub fn supports_control_grants(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_control_grants())
    }

    /// `control.grants.list` ([`Client::control_grants`]): what Mawas may do without asking.
    pub async fn control_grants(
        &self,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<Vec<hutan_protocol::grants::ControlGrant>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.control_grants(workspace_id).await
    }

    /// `control.grants.revoke` ([`Client::control_grant_revoke`]).
    pub async fn control_grant_revoke(&self, id: u64) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.control_grant_revoke(id).await
    }

    /// True while connected to a daemon that advertised capability `memory`
    /// ([`Client::supports_memory`]); false when disconnected.
    pub fn supports_memory(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_memory())
    }

    /// `memory.list` ([`Client::memory_list`]).
    pub async fn memory_list(
        &self,
        req: hutan_protocol::memory::MemoryList,
    ) -> Result<Vec<hutan_protocol::memory::MemoryEntry>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_list(req).await
    }

    /// `memory.history` ([`Client::memory_history`]).
    pub async fn memory_history(
        &self,
        entry_id: &str,
    ) -> Result<hutan_protocol::memory::MemoryHistory, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_history(entry_id).await
    }

    /// `memory.write` ([`Client::memory_write`]).
    pub async fn memory_write(
        &self,
        req: hutan_protocol::memory::MemoryWrite,
    ) -> Result<hutan_protocol::memory::MemoryReceipt, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_write(req).await
    }

    /// Whether the daemon keeps the memory in files (capability `memory.files`,
    /// [`Client::supports_memory_files`]); false when disconnected.
    pub fn supports_memory_files(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_memory_files())
    }

    /// `memory.import` ([`Client::memory_import`]).
    pub async fn memory_import(
        &self,
        req: hutan_protocol::memory::MemoryImport,
    ) -> Result<hutan_protocol::memory::MemoryImported, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_import(req).await
    }

    /// `memory.files.status` ([`Client::memory_files_status`]).
    pub async fn memory_files_status(
        &self,
        req: hutan_protocol::memory::MemoryFilesStatusRequest,
    ) -> Result<hutan_protocol::memory::MemoryFilesStatus, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_files_status(req).await
    }

    /// `memory.update` ([`Client::memory_update`]).
    pub async fn memory_update(
        &self,
        req: hutan_protocol::memory::MemoryUpdate,
    ) -> Result<hutan_protocol::memory::MemoryReceipt, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_update(req).await
    }

    /// `memory.archive` ([`Client::memory_archive`]).
    pub async fn memory_archive(
        &self,
        req: hutan_protocol::memory::MemoryTransition,
    ) -> Result<hutan_protocol::memory::MemoryReceipt, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_archive(req).await
    }

    /// `memory.restore` ([`Client::memory_restore`]).
    pub async fn memory_restore(
        &self,
        req: hutan_protocol::memory::MemoryTransition,
    ) -> Result<hutan_protocol::memory::MemoryReceipt, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_restore(req).await
    }

    /// `memory.proposals.list` ([`Client::memory_proposals`]).
    pub async fn memory_proposals(
        &self,
        include_resolved: bool,
    ) -> Result<Vec<hutan_protocol::memory::MemoryProposal>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_proposals(include_resolved).await
    }

    /// `memory.proposals.resolve` ([`Client::memory_resolve`]).
    pub async fn memory_resolve(
        &self,
        req: hutan_protocol::memory::MemoryProposalResolve,
    ) -> Result<hutan_protocol::memory::MemoryProposalResolved, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.memory_resolve(req).await
    }

    /// `assistant.reset` ([`Client::assistant_reset`]): the scope inherits its backend again.
    pub async fn assistant_reset(
        &self,
        scope: AssistantScope,
        workspace_id: Option<WorkspaceId>,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.assistant_reset(scope, workspace_id).await
    }

    /// True while connected to a daemon that advertised capability `harness.acp`
    /// ([`Client::supports_harness_chat`]); false when disconnected.
    pub fn supports_harness_chat(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_harness_chat())
    }

    /// True while connected to a daemon that advertised capability `accounts`
    /// ([`Client::supports_accounts`]); false when disconnected.
    pub fn supports_accounts(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_accounts())
    }

    /// `account.list`: the logins of the harnesses (names and plan only, never a token).
    pub async fn account_list(&self) -> Result<Vec<AccountInfo>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_list().await
    }

    /// True while connected to a daemon that advertised capability `usage`
    /// ([`Client::supports_usage`]); false when disconnected.
    pub fn supports_usage(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_usage())
    }

    /// True while connected to a daemon that advertised capability `hooks`; false when
    /// disconnected.
    pub fn supports_hooks(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_hooks())
    }

    /// True while connected to a daemon that advertised capability `update`; false when
    /// disconnected.
    pub fn supports_update(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_update())
    }

    /// `update.status` ([`Client::update_status`]).
    pub async fn update_status(&self) -> Result<hutan_protocol::update::UpdateStatus, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.update_status().await
    }

    /// `update.check` ([`Client::update_check`]).
    pub async fn update_check(&self) -> Result<hutan_protocol::update::UpdateStatus, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.update_check().await
    }

    /// `update.install` ([`Client::update_install`]).
    pub async fn update_install(
        &self,
        method: hutan_protocol::update::InstallMethod,
    ) -> Result<hutan_protocol::update::UpdateInstalling, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.update_install(method).await
    }

    /// `hooks.list` ([`Client::hooks_list`]).
    pub async fn hooks_list(&self) -> Result<hutan_protocol::hooks::HooksList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.hooks_list().await
    }

    /// `hooks.log` ([`Client::hooks_log`]).
    pub async fn hooks_log(
        &self,
        req: hutan_protocol::hooks::HooksLogRequest,
    ) -> Result<hutan_protocol::hooks::HooksLog, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.hooks_log(req).await
    }

    /// `hooks.test` ([`Client::hooks_test`]).
    pub async fn hooks_test(
        &self,
        hook: u32,
    ) -> Result<hutan_protocol::hooks::HooksTested, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.hooks_test(hook).await
    }

    /// True while connected to a daemon that advertised capability `agent.insight`
    /// ([`Client::supports_agent_insight`]); false when disconnected.
    pub fn supports_agent_insight(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_agent_insight())
    }

    /// `agent.insight` ([`Client::agent_insight`]).
    pub async fn agent_insight(
        &self,
        session_id: SessionId,
        node_id: Option<String>,
    ) -> Result<hutan_protocol::insight::AgentInsightResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.agent_insight(session_id, node_id).await
    }

    /// `agent.measure_context` ([`Client::agent_measure_context`]).
    pub async fn agent_measure_context(
        &self,
        session_id: SessionId,
    ) -> Result<hutan_protocol::insight::AgentContextMeasured, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.agent_measure_context(session_id).await
    }

    /// `usage.summary`: the limit windows the harnesses report and what the turns used;
    /// `refresh` asks the harnesses first (it takes seconds).
    pub async fn usage_summary(
        &self,
        refresh: bool,
    ) -> Result<hutan_protocol::usage::UsageSummary, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.usage_summary(refresh).await
    }

    /// `account.login`: the harness's own login in a terminal session.
    pub async fn account_login(
        &self,
        harness: &str,
        provider: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_login(harness, provider).await
    }

    /// `account.logout`: the harness's own logout in a terminal session.
    pub async fn account_logout(
        &self,
        harness: &str,
        provider: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_logout(harness, provider).await
    }

    /// True while connected to a daemon that advertised capability `chat.safety`.
    pub fn supports_chat_safety(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_safety())
    }

    /// True while connected to a daemon that advertised capability `sandbox` (Contain).
    pub fn supports_sandbox(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_sandbox())
    }

    /// True while connected to a daemon that advertised capability `session.dirs` (protocol
    /// 0.57): a session can hold extra folders.
    pub fn supports_session_dirs(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_session_dirs())
    }

    /// True while connected to a daemon that advertised capability `worktree.stage` (protocol
    /// 0.53): files and hunks can be staged, unstaged and discarded, and the staged ones
    /// committed alone.
    pub fn supports_worktree_stage(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_worktree_stage())
    }

    /// True while connected to a daemon that advertised capability `accounts.multi`.
    pub fn supports_accounts_multi(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_accounts_multi())
    }

    /// `account.list` with everything it says: own logins, managed accounts, defaults.
    pub async fn account_overview(&self) -> Result<AccountList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_overview().await
    }

    /// `account.login` for a managed account (`None`: the harness's own login).
    pub async fn account_login_as(
        &self,
        harness: &str,
        provider: Option<String>,
        account: Option<String>,
    ) -> Result<AccountSession, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_login_as(harness, provider, account).await
    }

    /// `account.add`: a managed account with an empty home.
    pub async fn account_add(
        &self,
        harness: &str,
        name: &str,
        label: Option<String>,
    ) -> Result<ManagedAccount, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_add(harness, name, label).await
    }

    /// `account.remove`: logs the account out, deletes its home, forgets it.
    pub async fn account_remove(
        &self,
        harness: &str,
        name: &str,
    ) -> Result<AccountRemoved, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_remove(harness, name).await
    }

    /// `account.default`: sets or clears the default account of a harness or of a project.
    pub async fn account_default(
        &self,
        harness: &str,
        account: Option<String>,
        project: Option<String>,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.account_default(harness, account, project).await
    }

    /// `chat.set_model`: switches the model (and for a direct chat the provider) of a chat for
    /// its next turn.
    pub async fn chat_set_model(
        &self,
        session: SessionId,
        provider_id: Option<String>,
        model: &str,
    ) -> Result<SessionInfo, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_set_model(session, provider_id, model).await
    }

    /// `chat.interrupt`: stops the turn in progress (and clears the queue).
    pub async fn chat_interrupt(&self, session: SessionId) -> Result<(), ClientError> {
        self.chat_stop(session, false).await
    }

    /// True while connected to a daemon that advertised capability `chat.queue`
    /// ([`Client::supports_chat_queue`]); false when disconnected.
    pub fn supports_chat_queue(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_chat_queue())
    }

    /// `chat.interrupt` with `keep_queue` ([`Client::chat_stop`]).
    pub async fn chat_stop(&self, session: SessionId, keep_queue: bool) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_stop(session, keep_queue).await
    }

    /// `chat.queue.list` ([`Client::chat_queue`]).
    pub async fn chat_queue(&self, session: SessionId) -> Result<ChatQueue, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_queue(session).await
    }

    /// `chat.queue.add` ([`Client::chat_queue_add`]).
    pub async fn chat_queue_add(
        &self,
        session: SessionId,
        text: String,
    ) -> Result<QueuedMessage, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_queue_add(session, text).await
    }

    /// `chat.queue.edit` ([`Client::chat_queue_edit`]).
    pub async fn chat_queue_edit(
        &self,
        session: SessionId,
        item: u64,
        text: String,
    ) -> Result<QueuedMessage, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_queue_edit(session, item, text).await
    }

    /// `chat.queue.remove` ([`Client::chat_queue_remove`]).
    pub async fn chat_queue_remove(
        &self,
        session: SessionId,
        item: u64,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_queue_remove(session, item).await
    }

    /// `chat.queue.clear` ([`Client::chat_queue_clear`]).
    pub async fn chat_queue_clear(&self, session: SessionId) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_queue_clear(session).await
    }

    /// `chat.send_now` ([`Client::chat_send_now`]).
    pub async fn chat_send_now(
        &self,
        session: SessionId,
        text: Option<String>,
        item: Option<u64>,
    ) -> Result<ChatSentNow, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_send_now(session, text, item).await
    }

    /// One page of a chat's journal, oldest first ([`Client::chat_history`]).
    pub async fn chat_history(
        &self,
        session: SessionId,
        before_seq: Option<u64>,
        limit: u32,
    ) -> Result<ChatHistory, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_history(session, before_seq, limit).await
    }

    /// Whether the daemon has checkpoints of agent turns (capability `checkpoint`,
    /// [`Client::supports_checkpoints`]); false when disconnected.
    pub fn supports_checkpoints(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_checkpoints())
    }

    /// Whether the daemon restores files to before a checkpoint's turn (capability
    /// `checkpoint.restore`, [`Client::supports_checkpoint_restore`]); false when disconnected.
    pub fn supports_checkpoint_restore(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_checkpoint_restore())
    }

    /// Whether the daemon puts folders under version control (capability `project.git_init`,
    /// [`Client::supports_project_git_init`]); false when disconnected.
    pub fn supports_project_git_init(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_project_git_init())
    }

    /// Whether the daemon answers git history queries (capability `git.history`,
    /// [`Client::supports_git_history`]); false when disconnected.
    pub fn supports_git_history(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_git_history())
    }

    /// `git.log` ([`Client::git_log`]).
    pub async fn git_log(
        &self,
        req: hutan_protocol::git::GitLog,
    ) -> Result<hutan_protocol::git::GitLogResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.git_log(req).await
    }

    /// `git.blame` ([`Client::git_blame`]).
    pub async fn git_blame(
        &self,
        req: hutan_protocol::git::GitBlame,
    ) -> Result<hutan_protocol::git::GitBlameResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.git_blame(req).await
    }

    /// `git.show` ([`Client::git_show`]).
    pub async fn git_show(
        &self,
        req: hutan_protocol::git::GitShow,
    ) -> Result<hutan_protocol::git::GitShowResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.git_show(req).await
    }

    /// `git.file` ([`Client::git_file`]).
    pub async fn git_file(
        &self,
        req: hutan_protocol::git::GitFile,
    ) -> Result<hutan_protocol::git::GitFileResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.git_file(req).await
    }

    /// `project.git_init` ([`Client::project_git_init`]).
    pub async fn project_git_init(
        &self,
        project_id: hutan_core::WorkspaceId,
    ) -> Result<hutan_protocol::project::ProjectGitInitialized, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.project_git_init(project_id).await
    }

    /// `checkpoint.list` ([`Client::checkpoint_list`]).
    pub async fn checkpoint_list(
        &self,
        request: hutan_protocol::checkpoint::CheckpointListRequest,
    ) -> Result<hutan_protocol::checkpoint::CheckpointList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.checkpoint_list(request).await
    }

    /// `checkpoint.diff` ([`Client::checkpoint_diff`]).
    pub async fn checkpoint_diff(
        &self,
        request: hutan_protocol::checkpoint::CheckpointDiffRequest,
    ) -> Result<hutan_protocol::checkpoint::CheckpointDiff, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.checkpoint_diff(request).await
    }

    /// `checkpoint.preview` ([`Client::checkpoint_preview`]).
    pub async fn checkpoint_preview(
        &self,
        request: hutan_protocol::checkpoint::CheckpointPreviewRequest,
    ) -> Result<hutan_protocol::checkpoint::CheckpointPreview, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.checkpoint_preview(request).await
    }

    /// `checkpoint.restore` ([`Client::checkpoint_restore`]): a stale preview answers
    /// `conflict` with the new preview as the error's `data`.
    pub async fn checkpoint_restore(
        &self,
        request: hutan_protocol::checkpoint::CheckpointRestore,
    ) -> Result<hutan_protocol::checkpoint::CheckpointRestored, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.checkpoint_restore(request).await
    }

    /// `chat.models`: the models an agent offers.
    pub async fn chat_models(&self, agent: &str) -> Result<ChatModels, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.chat_models(agent).await
    }

    /// `approval.list`: permission requests waiting for an answer (of one session, or of all).
    pub async fn approvals(
        &self,
        session: Option<SessionId>,
    ) -> Result<Vec<ApprovalInfo>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.approvals(session).await
    }

    /// `approval.respond`: answers a permission request with one of the options it offered.
    pub async fn approve(
        &self,
        approval: ApprovalId,
        option_id: impl Into<String>,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.approve(approval, option_id).await
    }

    /// `question.list`: questions of agents waiting for an answer (of one session, or of all).
    pub async fn questions(
        &self,
        session: Option<SessionId>,
    ) -> Result<Vec<QuestionInfo>, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.questions(session).await
    }

    /// `question.answer`: values by field id, a decline, or a cancel.
    pub async fn answer_question(
        &self,
        question: QuestionId,
        response: QuestionResponse,
    ) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.answer_question(question, response).await
    }

    /// A durable view of `terminal` that re-attaches across reconnects. Returns at once; the
    /// attach happens when (and whenever) the connection is up. At most one handle per
    /// terminal is active: attaching again ends the earlier handle's update stream.
    pub fn attach(&self, terminal: TerminalId, size: Option<TerminalSize>) -> TerminalHandle {
        let handle = self.shared.next_handle.fetch_add(1, Ordering::Relaxed);
        let (updates, updates_rx) = mpsc::channel(UPDATE_BUFFER);
        send_command(
            &self.shared.commands,
            Command::Attach {
                handle,
                terminal,
                size,
                updates,
            },
        );
        TerminalHandle::new(terminal, handle, updates_rx, self.shared.commands.clone())
    }

    /// Stops reconnecting, closes the connection and waits for the thread to finish. Affects
    /// every clone: their state becomes [`ConnectionState::Stopped`] and calls fail.
    pub fn shutdown(self) {
        self.owner.stop();
    }
}

/// Language servers (protocol 0.39, capabilities `doc.sync` and `lsp`; `docs/protocol.md`
/// § LSP). A document belongs to the connection it was opened on: the daemon closes it when
/// that connection ends, so [`doc_open`](Connection::doc_open) answers the
/// [`generation`](Connection::generation) it was opened on and a caller that finds another one
/// opens it again. `lsp.state` and `lsp.diagnostics` arrive as [`Notice::LspState`] and
/// [`Notice::LspDiagnostics`].
impl Connection {
    /// True while connected to a daemon that advertised capability `doc.sync`
    /// ([`Client::supports_doc_sync`]); false when disconnected.
    pub fn supports_doc_sync(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_doc_sync())
    }

    /// True while connected to a daemon that advertised capability `lsp`
    /// ([`Client::supports_lsp`]); false when disconnected.
    pub fn supports_lsp(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_lsp())
    }

    /// True while connected to a daemon that advertised capability `lsp.completion`
    /// ([`Client::supports_lsp_completion`]).
    pub fn supports_lsp_completion(&self) -> bool {
        self.shared
            .client()
            .is_some_and(|client| client.supports_lsp_completion())
    }

    /// `doc.open` on the current connection, with the generation it was opened on.
    pub async fn doc_open(
        &self,
        req: hutan_protocol::lsp::DocOpen,
    ) -> Result<(u64, hutan_protocol::lsp::DocOpened), ClientError> {
        let (generation, client) = self
            .shared
            .client_generation()
            .ok_or(ClientError::Disconnected)?;
        let opened = client.doc_open(req).await?;
        Ok((generation, opened))
    }

    /// `doc.change`: the owner's edits of an open document.
    pub async fn doc_change(&self, req: hutan_protocol::lsp::DocChange) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.doc_change(req).await
    }

    /// `doc.save`.
    pub async fn doc_save(&self, doc_id: &str) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.doc_save(doc_id).await
    }

    /// `doc.close`.
    pub async fn doc_close(&self, doc_id: &str) -> Result<(), ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.doc_close(doc_id).await
    }

    /// `lsp.status`: the servers and the folder decisions.
    pub async fn lsp_status(
        &self,
        req: hutan_protocol::lsp::LspStatusRequest,
    ) -> Result<hutan_protocol::lsp::LspStatus, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_status(req).await
    }

    /// `lsp.diagnostics`: what the servers published.
    pub async fn lsp_diagnostics(
        &self,
        req: hutan_protocol::lsp::LspDiagnosticsRequest,
    ) -> Result<hutan_protocol::lsp::LspDiagnosticsResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_diagnostics(req).await
    }

    /// `lsp.hover` at a position of an open document.
    pub async fn lsp_hover(
        &self,
        req: hutan_protocol::lsp::LspPositionRequest,
    ) -> Result<hutan_protocol::lsp::LspHover, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_hover(req).await
    }

    /// `lsp.definition` of the symbol at a position of an open document.
    pub async fn lsp_definition(
        &self,
        req: hutan_protocol::lsp::LspPositionRequest,
    ) -> Result<hutan_protocol::lsp::LspLocations, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_definition(req).await
    }

    /// `lsp.completion` at a position of an open document.
    pub async fn lsp_completion(
        &self,
        req: hutan_protocol::lsp::LspCompletionRequest,
    ) -> Result<hutan_protocol::lsp::LspCompletionList, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_completion(req).await
    }

    /// `lsp.completion_resolve`: more about one item of a completion list.
    pub async fn lsp_completion_resolve(
        &self,
        req: hutan_protocol::lsp::LspCompletionResolve,
    ) -> Result<hutan_protocol::lsp::LspCompletionItem, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_completion_resolve(req).await
    }

    /// `lsp.signature_help` at a position of an open document.
    pub async fn lsp_signature_help(
        &self,
        req: hutan_protocol::lsp::LspSignatureHelpRequest,
    ) -> Result<hutan_protocol::lsp::LspSignatureHelp, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_signature_help(req).await
    }

    /// `lsp.trust`: the user's decision about a folder (user-only).
    pub async fn lsp_trust(
        &self,
        req: hutan_protocol::lsp::LspTrustRequest,
    ) -> Result<hutan_protocol::lsp::LspTrustResult, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_trust(req).await
    }

    /// `lsp.restart`: starts a server again (a failed or stopped one, too).
    pub async fn lsp_restart(
        &self,
        server_id: &str,
    ) -> Result<hutan_protocol::lsp::LspServerInfo, ClientError> {
        let client = self.shared.client().ok_or(ClientError::Disconnected)?;
        client.lsp_restart(server_id).await
    }
}

/// Body of the `hutan-client` thread.
fn run_thread(supervisor: Supervisor) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    match runtime {
        Ok(runtime) => runtime.block_on(supervisor.run()),
        Err(e) => {
            error!(error = %e, "cannot build the hutan-client runtime");
            supervisor.finish();
        }
    }
}
