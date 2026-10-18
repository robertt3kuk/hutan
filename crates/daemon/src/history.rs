//! The session history index inside the daemon (roadmap M17, `docs/history-index.md`): starts
//! and stops the `hutan-index` indexer according to `[history]` in `daemon.toml`, feeds it
//! Hutan's own sessions (finished terminal blocks, agent state changes), and answers the
//! `history.*` requests.
//!
//! Opt-in: while disabled nothing is opened, watched, or read, and `index.sqlite` is not even
//! created.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hutan_core::agent::MessageRole;
use hutan_core::{AgentEvent, AgentEventKind, SessionId, TerminalId};
use hutan_index::{
    BlockRef, HutanRecord, IndexConfig, IndexError, IndexHandle, Indexer, Role, SearchQuery,
    SessionText, SessionTextQuery, Source,
};
use hutan_protocol::blocks::{BlockPart, BlockState, TerminalBlockText};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::history::{
    HistoryHit, HistoryHits, HistoryRole, HistorySearch, HistorySessions, HistorySessionsRequest,
    HistorySource, HistorySourceStatus, HistoryStatusInfo, SEARCH_MAX_LIMIT,
};
use hutan_protocol::messages::{AgentState, Event, SessionInfo};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::agent::validate_resume_argv;
use crate::bus::EventBus;
use crate::config::{self, HistorySettings};
use crate::history_catalog;
use crate::paths::Paths;
use crate::registry::RegistryHandle;
use crate::terminals::TerminalHub;

/// Events waiting for the tap consumer before the bus drops them.
const TAP_QUEUE: usize = 1024;
/// Most bytes of a block's output the index reads from the terminal.
const BLOCK_OUTPUT_BYTES: u32 = 16 * 1024;
/// How long `history.sessions` with `refresh` waits for the rescan before it answers with the
/// scan still running.
const REFRESH_WAIT: Duration = Duration::from_secs(10);

struct Running {
    indexer: Indexer,
    handle: IndexHandle,
    tap: Option<JoinHandle<()>>,
}

struct State {
    settings: HistorySettings,
    running: Option<Running>,
}

pub struct HistoryService {
    paths: Paths,
    bus: Arc<EventBus>,
    hub: Arc<TerminalHub>,
    registry: RegistryHandle,
    state: Mutex<State>,
}

impl std::fmt::Debug for HistoryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryService").finish_non_exhaustive()
    }
}

fn internal(error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, error.to_string())
}

fn disabled() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        "history indexing is disabled; enable it with `hutan index enable`",
    )
}

fn index_error(error: IndexError) -> ErrorBody {
    match error {
        IndexError::BadQuery(message) => ErrorBody::new(ErrorCode::BadRequest, message),
        other => internal(other),
    }
}

impl HistoryService {
    pub fn new(
        paths: Paths,
        settings: HistorySettings,
        bus: Arc<EventBus>,
        hub: Arc<TerminalHub>,
        registry: RegistryHandle,
    ) -> Arc<Self> {
        Arc::new(Self {
            paths,
            bus,
            hub,
            registry,
            state: Mutex::new(State {
                settings,
                running: None,
            }),
        })
    }

    /// Starts indexing when `[history] enabled` is true. A failure is logged and leaves the
    /// index off: the daemon does not depend on it.
    pub async fn start(&self) {
        let mut state = self.state.lock().await;
        if state.settings.enabled
            && let Err(error) = self.launch(&mut state).await
        {
            warn!(message = %error.message, "history indexing could not start");
            state.settings.enabled = false;
        }
    }

    /// Stops the indexer (after it applies what is queued) and the tap. The index stays on disk.
    pub async fn shutdown(&self) {
        let mut state = self.state.lock().await;
        self.halt(&mut state).await;
    }

    async fn launch(&self, state: &mut State) -> Result<(), ErrorBody> {
        if state.running.is_some() {
            return Ok(());
        }
        let settings = state.settings.clone();
        let config = IndexConfig {
            db_path: self.paths.index_database.clone(),
            range_days: settings.range_days,
            roots: source_roots(&settings),
            hutan: settings.hutan,
        };
        let sources: Vec<_> = config.roots.iter().map(|(s, p)| (*s, p.clone())).collect();
        let indexer = tokio::task::spawn_blocking(move || Indexer::start(config))
            .await
            .map_err(internal)?
            .map_err(index_error)?;
        let handle = indexer.handle();
        let tap = settings.hutan.then(|| {
            let (tx, rx) = mpsc::channel(TAP_QUEUE);
            // Subscribe before reading the snapshot so no session update falls in between.
            self.bus.set_tap(Some(tx));
            tokio::spawn(consume(
                rx,
                handle.clone(),
                Arc::clone(&self.hub),
                self.registry.clone(),
            ))
        });
        info!(
            range_days = settings.range_days,
            hutan = settings.hutan,
            sources = ?sources,
            "history indexing started"
        );
        state.running = Some(Running {
            indexer,
            handle,
            tap,
        });
        state.settings.enabled = true;
        Ok(())
    }

    async fn halt(&self, state: &mut State) {
        let Some(mut running) = state.running.take() else {
            return;
        };
        self.bus.set_tap(None);
        if let Some(tap) = running.tap.take() {
            // The sender is gone, so the consumer drains what is queued and ends.
            let _ = tap.await;
        }
        if let Err(error) = tokio::task::spawn_blocking(move || running.indexer.stop()).await {
            warn!(%error, "stopping the history indexer failed");
        }
        info!("history indexing stopped");
    }

    async fn handle(&self) -> Result<IndexHandle, ErrorBody> {
        let state = self.state.lock().await;
        state
            .running
            .as_ref()
            .map(|running| running.handle.clone())
            .ok_or_else(disabled)
    }

    pub async fn search(&self, req: HistorySearch) -> Result<HistoryHits, ErrorBody> {
        if req.query.trim().is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "query must not be empty",
            ));
        }
        if req.limit == 0 || req.limit > SEARCH_MAX_LIMIT {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("limit must be between 1 and {SEARCH_MAX_LIMIT}"),
            ));
        }
        if let (Some(since), Some(until)) = (req.since_ms, req.until_ms)
            && since > until
        {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "since_ms is after until_ms",
            ));
        }
        let handle = self.handle().await?;
        let query = SearchQuery {
            text: req.query,
            limit: req.limit as usize,
            sources: req.sources.iter().map(|s| source_in(*s)).collect(),
            roles: req.roles.iter().map(|r| role_in(*r)).collect(),
            project: req.project,
            since_ms: req.since_ms,
            until_ms: req.until_ms,
        };
        let hits = tokio::task::spawn_blocking(move || handle.search(&query))
            .await
            .map_err(internal)?
            .map_err(index_error)?;
        Ok(HistoryHits {
            hits: hits.into_iter().map(hit_out).collect(),
        })
    }

    /// The session catalog (`history.sessions`): the sessions of the enabled sources with their
    /// summaries, attributed to Hutan projects, narrowed by scope and filter, sorted, and cut to
    /// the limit. With `refresh` the transcript files are looked at again first (waiting at most
    /// [`REFRESH_WAIT`]; then the answer says the scan is still running).
    pub async fn sessions(
        &self,
        req: HistorySessionsRequest,
    ) -> Result<HistorySessions, ErrorBody> {
        let limit = history_catalog::check_request(&req)?;
        let handle = self.handle().await?;
        let refresh = req.refresh;
        let read = {
            let handle = handle.clone();
            tokio::task::spawn_blocking(move || {
                let finished = if refresh {
                    handle.refresh(REFRESH_WAIT)?
                } else {
                    true
                };
                let rows = handle.catalog()?;
                let mut scan = handle.scan_info();
                scan.scanning |= !finished;
                Ok::<_, IndexError>((rows, scan))
            })
        };
        let (rows, scan) = read.await.map_err(internal)?.map_err(index_error)?;
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        // Only Hutan rows need the live sessions (their title, folder and workspace).
        let sessions = if rows.iter().any(|row| row.source == Source::Hutan) {
            self.registry.snapshot().await?.sessions
        } else {
            Vec::new()
        };
        // Attribution reads the file system (canonical paths, git worktrees).
        tokio::task::spawn_blocking(move || {
            history_catalog::answer(&req, limit, rows, scan, &workspaces, &sessions)
        })
        .await
        .map_err(internal)?
    }

    /// The newest rows of one indexed session, oldest first (what an `agent_transcript`
    /// attachment reads for a transcript of another tool).
    pub async fn session_text(
        &self,
        query: SessionTextQuery,
    ) -> Result<Vec<SessionText>, ErrorBody> {
        let handle = self.handle().await?;
        tokio::task::spawn_blocking(move || handle.session_text(&query))
            .await
            .map_err(internal)?
            .map_err(index_error)
    }

    pub async fn status(&self) -> Result<HistoryStatusInfo, ErrorBody> {
        let handle = {
            let state = self.state.lock().await;
            state.running.as_ref().map(|running| running.handle.clone())
        };
        let Some(handle) = handle else {
            return Ok(HistoryStatusInfo {
                enabled: false,
                sources: Vec::new(),
                db_bytes: hutan_index::store::database_bytes(&self.paths.index_database),
            });
        };
        let status = tokio::task::spawn_blocking(move || handle.status())
            .await
            .map_err(internal)?
            .map_err(index_error)?;
        Ok(HistoryStatusInfo {
            enabled: true,
            sources: status
                .sources
                .into_iter()
                .map(|s| HistorySourceStatus {
                    name: s.source.as_str().to_owned(),
                    files: s.files,
                    rows: s.rows,
                    last_indexed_ms: s.last_indexed_ms,
                    errors: s.errors,
                })
                .collect(),
            db_bytes: status.db_bytes,
        })
    }

    /// Turns indexing on or off and records the choice in `daemon.toml`. Turning it off keeps the
    /// index on disk; `clear` deletes it.
    pub async fn set_enabled(&self, enabled: bool) -> Result<HistoryStatusInfo, ErrorBody> {
        {
            let mut state = self.state.lock().await;
            // Settings are read again so edits made to the file by hand take effect.
            let mut settings = match config::DaemonSettings::load(
                &self.paths.config_file,
                self.paths.user_home.as_deref(),
            ) {
                Ok(settings) => settings.history,
                Err(error) => return Err(internal(error)),
            };
            settings.enabled = enabled;
            if enabled {
                state.settings = settings;
                self.launch(&mut state).await?;
            } else {
                self.halt(&mut state).await;
                state.settings = settings;
            }
            config::set_history_enabled(&self.paths.config_file, enabled).map_err(internal)?;
        }
        self.status().await
    }

    /// The `[history]` settings indexing runs by (or would run by, when it is off).
    pub async fn settings(&self) -> HistorySettings {
        self.state.lock().await.settings.clone()
    }

    /// Indexing follows `settings` from now on (`daemon.settings.set` changed `[history]`):
    /// on or off, and a running indexer starts again when its sources or range changed. The
    /// index on disk is kept. Does not write `daemon.toml`.
    pub async fn apply_settings(&self, settings: HistorySettings) -> Result<(), ErrorBody> {
        let mut state = self.state.lock().await;
        if state.settings == settings && state.running.is_some() == settings.enabled {
            return Ok(());
        }
        self.halt(&mut state).await;
        state.settings = settings;
        if state.settings.enabled
            && let Err(error) = self.launch(&mut state).await
        {
            state.settings.enabled = false;
            return Err(error);
        }
        Ok(())
    }

    /// Deletes the index (rows, sources table, file) and, when indexing runs, starts an empty one.
    pub async fn clear(&self) -> Result<(), ErrorBody> {
        let state = self.state.lock().await;
        match &state.running {
            Some(running) => {
                let handle = running.handle.clone();
                tokio::task::spawn_blocking(move || handle.clear())
                    .await
                    .map_err(internal)?
                    .map_err(index_error)
            }
            None => {
                let path = self.paths.index_database.clone();
                tokio::task::spawn_blocking(move || hutan_index::store::remove_database(&path))
                    .await
                    .map_err(internal)?
                    .map_err(index_error)
            }
        }
    }

    /// Reads every transcript again, in the background.
    pub async fn rebuild(&self) -> Result<(), ErrorBody> {
        self.handle().await?.rebuild().map_err(index_error)
    }
}

/// The transcript directories to index for `settings`: configured roots, else the agents' own
/// (`$CLAUDE_CONFIG_DIR`, `$CODEX_HOME`, `$PI_CODING_AGENT_DIR`, else under the home directory).
fn source_roots(settings: &HistorySettings) -> Vec<(Source, PathBuf)> {
    let home = std::env::home_dir();
    let default = |variable: &str, under_home: &[&str], sub: &str| -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os(variable).filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(dir).join(sub));
        }
        let mut dir = home.clone()?;
        dir.extend(under_home);
        Some(dir.join(sub))
    };
    let mut roots = Vec::new();
    if settings.claude
        && let Some(root) = settings
            .roots
            .claude
            .clone()
            .or_else(|| default("CLAUDE_CONFIG_DIR", &[".claude"], "projects"))
    {
        roots.push((Source::Claude, root));
    }
    if settings.codex
        && let Some(root) = settings
            .roots
            .codex
            .clone()
            .or_else(|| default("CODEX_HOME", &[".codex"], "sessions"))
    {
        roots.push((Source::Codex, root));
    }
    if settings.omp
        && let Some(root) = settings
            .roots
            .omp
            .clone()
            .or_else(|| default("PI_CODING_AGENT_DIR", &[".omp", "agent"], "sessions"))
    {
        roots.push((Source::Omp, root));
    }
    roots
}

fn source_in(source: HistorySource) -> Source {
    match source {
        HistorySource::Hutan => Source::Hutan,
        HistorySource::Claude => Source::Claude,
        HistorySource::Codex => Source::Codex,
        HistorySource::Omp => Source::Omp,
    }
}

fn source_out(source: Source) -> HistorySource {
    match source {
        Source::Hutan => HistorySource::Hutan,
        Source::Claude => HistorySource::Claude,
        Source::Codex => HistorySource::Codex,
        Source::Omp => HistorySource::Omp,
    }
}

fn role_in(role: HistoryRole) -> Role {
    match role {
        HistoryRole::User => Role::User,
        HistoryRole::Assistant => Role::Assistant,
        HistoryRole::Tool => Role::Tool,
        HistoryRole::Command => Role::Command,
        HistoryRole::Output => Role::Output,
    }
}

fn role_out(role: Role) -> HistoryRole {
    match role {
        Role::User => HistoryRole::User,
        Role::Assistant => HistoryRole::Assistant,
        Role::Tool => HistoryRole::Tool,
        Role::Command => HistoryRole::Command,
        Role::Output => HistoryRole::Output,
    }
}

fn hit_out(hit: hutan_index::Hit) -> HistoryHit {
    // The same rules as `agent.report` resume commands: an id that would not pass is not offered.
    let resume_argv = hutan_index::resume_argv(hit.source, &hit.session_key)
        .filter(|argv| validate_resume_argv(argv).is_ok());
    // A Hutan hit's key is the Hutan session id: a client can open the session.
    let session_id = (hit.source == Source::Hutan)
        .then(|| hit.session_key.parse::<SessionId>().ok())
        .flatten();
    HistoryHit {
        source: source_out(hit.source),
        session_key: hit.session_key,
        title: hit.title,
        cwd: hit.cwd,
        role: role_out(hit.role),
        ts_ms: hit.ts_ms,
        snippet: hit.snippet,
        score: hit.score,
        matches: hit.matches,
        resume_argv,
        session_id,
        path: hit.path,
        terminal_id: hit.block.map(|block| TerminalId(block.terminal_id)),
        block_id: hit.block.map(|block| block.block_id),
    }
}

// ---- Hutan's own sessions -----------------------------------------------------------------------

/// What the tap consumer remembers about a session.
struct SessionMeta {
    id: SessionId,
    title: String,
    cwd: Option<String>,
}

#[derive(Default)]
struct Consumer {
    by_terminal: HashMap<TerminalId, SessionMeta>,
    /// Chat sessions (Codex, direct, assistant): their journals are indexed as they are written.
    by_session: HashMap<SessionId, SessionMeta>,
    /// The last agent report seen per session, to index state changes once.
    agents: HashMap<SessionId, (AgentState, Option<String>, u64)>,
}

/// What the index keeps of a chat event: user and assistant messages, tool calls and their
/// output. The key makes a record replace itself when it is pushed again.
fn chat_row(event: &AgentEvent) -> Option<(Role, String, String)> {
    let key = format!("c:{}", event.seq);
    match &event.kind {
        AgentEventKind::MessageEnd { role, text, .. } if !text.trim().is_empty() => {
            let role = match role {
                MessageRole::User => Role::User,
                MessageRole::Assistant => Role::Assistant,
            };
            Some((role, text.clone(), key))
        }
        AgentEventKind::ToolStart { name, title, .. } => {
            Some((Role::Tool, format!("{name}: {title}"), key))
        }
        AgentEventKind::ToolEnd {
            output: Some(output),
            ..
        } if !output.trim().is_empty() => Some((Role::Tool, output.clone(), key)),
        _ => None,
    }
}

impl Consumer {
    /// Learns a session; `emit` also turns a changed agent report into a row.
    fn learn(&mut self, session: &SessionInfo, emit: bool, handle: &IndexHandle) {
        if session.chat.is_some() {
            self.by_session.insert(
                session.id,
                SessionMeta {
                    id: session.id,
                    title: session.title.clone(),
                    cwd: session
                        .cwd
                        .as_ref()
                        .map(|c| c.to_string_lossy().into_owned()),
                },
            );
        }
        if let Some(terminal) = session.terminal_id {
            self.by_terminal.insert(
                terminal,
                SessionMeta {
                    id: session.id,
                    title: session.title.clone(),
                    cwd: session
                        .cwd
                        .as_ref()
                        .map(|c| c.to_string_lossy().into_owned()),
                },
            );
        }
        let Some(agent) = &session.agent else {
            self.agents.remove(&session.id);
            return;
        };
        let seen = (agent.state, agent.message.clone(), agent.updated_at_ms);
        if self.agents.get(&session.id) == Some(&seen) {
            return;
        }
        let first_sight = !self.agents.contains_key(&session.id);
        self.agents.insert(session.id, seen);
        if !emit || (first_sight && agent.updated_at_ms == 0) {
            return;
        }
        let state = match agent.state {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
        };
        let mut text = format!("{} {state}", agent.agent);
        if let Some(message) = agent.message.as_deref().filter(|m| !m.trim().is_empty()) {
            text.push_str(": ");
            text.push_str(message);
        }
        handle.push_hutan(HutanRecord {
            session_key: session.id.to_string(),
            title: Some(session.title.clone()),
            cwd: session
                .cwd
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned()),
            role: Role::Tool,
            ts_ms: agent.updated_at_ms,
            text,
            ext_key: format!("a:{}", agent.updated_at_ms),
            block: None,
        });
    }
}

/// Consumes session and block events until the bus drops the tap.
async fn consume(
    mut events: mpsc::Receiver<Event>,
    handle: IndexHandle,
    hub: Arc<TerminalHub>,
    registry: RegistryHandle,
) {
    let mut consumer = Consumer::default();
    match registry.snapshot().await {
        Ok(snapshot) => {
            for session in &snapshot.sessions {
                consumer.learn(session, false, &handle);
            }
        }
        Err(error) => warn!(message = %error.message, "history: no session snapshot"),
    }
    while let Some(event) = events.recv().await {
        match event {
            Event::SessionUpdated { session } => consumer.learn(&session, true, &handle),
            Event::TerminalBlock { terminal_id, block } if block.state == BlockState::Finished => {
                let Some(command) = block
                    .command
                    .as_deref()
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                else {
                    continue;
                };
                let Some(session) = consumer.by_terminal.get(&terminal_id) else {
                    debug!(%terminal_id, "history: block of an unknown terminal");
                    continue;
                };
                let cwd = block
                    .cwd
                    .as_ref()
                    .map(|c| c.to_string_lossy().into_owned())
                    .or_else(|| session.cwd.clone());
                let record = |role, text: &str, suffix: &str| HutanRecord {
                    session_key: session.id.to_string(),
                    title: Some(session.title.clone()),
                    cwd: cwd.clone(),
                    role,
                    ts_ms: block.started_at_ms,
                    text: text.to_owned(),
                    ext_key: format!("b:{}:{}{suffix}", block.id, block.started_at_ms),
                    block: Some(BlockRef {
                        terminal_id: terminal_id.0,
                        block_id: block.id,
                    }),
                };
                handle.push_hutan(record(Role::Command, command, ""));
                let output = hub.block_text(TerminalBlockText {
                    terminal_id,
                    block_id: block.id,
                    part: BlockPart::Output,
                    max_bytes: BLOCK_OUTPUT_BYTES,
                });
                if let Ok(output) = output
                    && !output.text.trim().is_empty()
                {
                    handle.push_hutan(record(Role::Output, &output.text, ":out"));
                }
            }
            Event::ChatEvent { session_id, event } => {
                let Some(session) = consumer.by_session.get(&session_id) else {
                    debug!(%session_id, "history: chat event of an unknown session");
                    continue;
                };
                if let Some((role, text, key)) = chat_row(&event) {
                    handle.push_hutan(HutanRecord {
                        session_key: session_id.to_string(),
                        title: Some(session.title.clone()),
                        cwd: session.cwd.clone(),
                        role,
                        ts_ms: event.ts_ms,
                        text,
                        ext_key: key,
                        block: None,
                    });
                }
            }
            _ => {}
        }
    }
    debug!("history tap closed");
}
