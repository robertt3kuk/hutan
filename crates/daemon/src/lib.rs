//! `hutand` as a library so tests (and later tooling) can run the daemon in-process.
//!
//! - [`paths`]: `HUTAN_HOME` / `HUTAN_SOCKET` layout.
//! - [`config`]: `daemon.toml` (`restore_sessions`, `home_folder`).
//! - [`daemon`]: lock, socket, accept loop, shutdown ([`start`]).
//! - [`conn`]: per-client handshake, request loop, bounded outbound queue.
//! - [`bus`]: epoch + seq event ring and fan-out.
//! - [`router`]: typed request dispatch.
//! - [`registry`]: the `SessionRegistry` actor (workspaces and terminal sessions, written
//!   through to `hutan-storage` and restored from it at start).
//! - [`restore`]: loading durable state and the rules for what is stored.
//! - [`recovery`]: what a start did to each restored session (`SessionInfo.recovery`).
//! - [`placement`]: which git checkout a session works in and which agents share one.
//! - [`worktrees`]: git worktrees (`worktree.*`) on top of the git command line.
//! - [`checkpoints`]: per-turn snapshots of a checkout's files in hidden git refs, through a
//!   private index (ADR 0019; the engine, not yet wired to sessions).
//! - [`tasks`]: tasks with dependencies (`task.*`): finish detection, auto-start, restart.
//! - [`projects`]: the Home folder, project identity (git common directory), which project or
//!   Home a folder belongs to.
//! - [`project_migration`]: the one-time conversion of earlier workspaces into projects and
//!   groups.
//! - [`terminals`]: live terminals, ack-paced attachments, resize, history, and input.
//! - [`history`]: the opt-in session history index (`hutan-index`) and the `history.*` requests.
//! - [`history_catalog`]: `history.sessions`, the session catalog: attribution to projects, scopes,
//!   filter, sort.
//! - [`chat`]: chat sessions: agent adapters, direct model chats, the approval engine, the journal.
//! - [`direct`]: direct model chat: a session whose "agent" is the model gateway.
//! - [`providers`]: model providers: SQLite config, Keychain credentials, adapters.
//! - [`agent`]: validation of agent reports and the `HUTAN_BIN` lookup.
//! - [`tree`]: the per-session agent observation tree: event coalescing, rollup, storage.
//! - [`assistant`]: assistant sessions (direct chats with a scope) and their context providers.
//! - [`assistant_pick`]: the assistant's automatic model: one tier below a premium default.
//! - [`context`]: redaction, budget, headers and assembly of attached context.
//! - [`context_file`]: file attachments, read below the workspace roots without following symlinks.
//! - [`resources`]: process trees, CPU and memory while watched, `session.kill`, `process.signal`,
//!   `agent.interrupt`, and their audit trail.
//! - [`notify`]: notification intents derived from the event stream, `notify.*`, `client.focus`.
//! - [`partial`]: journaled snapshots of streaming chat text (protocol 0.15).
//! - [`notify_native`]: posting the intents to the system's notification centre from `Hutan.app`
//!   and routing the clicks back (ADR 0007).
//! - [`menu_bar`]: the daemon's own menu bar item while no desktop is connected (M6-D).
//! - [`control`]: who is asking (`ClientInfo.origin`), the `[control]` policy for agents, the
//!   `control` approvals and their audit (protocol 0.21, ADR 0016).
//! - [`uihub`]: the desktop's published catalog and layout, `ui.run` / `ui.set_setting` routing.
//! - [`files`]: the file service: confined `fs.read`/`fs.list`/`fs.stat`, atomic chunked
//!   `fs.write.*`, `fs.find`, `fs.search`, `fs.watch` with `fs.changed` and who made a change
//!   (protocol 0.33).
//! - [`mcp_wiring`]: how `hutan mcp` is registered with the agents Hutan starts.
//! - [`daemon_settings`]: `daemon.toml` as a list of settings: `daemon.settings.get` / `.set`
//!   validate, edit the file in place and apply what can change live (protocol 0.28).

pub mod accounts;
pub mod agent;
pub mod assistant;
pub mod assistant_pick;
pub mod bus;
pub mod chat;
pub mod chat_queue;
pub mod checkpoints;
pub mod config;
pub mod conn;
pub mod context;
pub mod context_file;
pub mod control;
pub mod daemon;
pub mod daemon_settings;
pub mod direct;
pub mod error;
pub mod files;
pub mod harness;
pub mod history;
pub mod history_catalog;
pub mod mcp_wiring;
pub mod memory;
pub mod menu_bar;
pub mod notify;
pub mod notify_native;
pub mod partial;
pub mod paths;
pub mod placement;
pub mod project_migration;
pub mod projects;
pub mod providers;
pub mod recovery;
pub mod registry;
pub mod resources;
pub mod restore;
pub mod router;
pub mod session_names;
pub mod tasks;
pub mod terminals;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod tree;
pub mod uihub;
pub mod worktrees;

pub use bus::RingLimits;
pub use config::{
    AgentPolicy, AssistantSettings, ControlSettings, DaemonSettings, HistorySettings,
    NotifySettings, ProjectsSettings, RestoreMode, TasksSettings,
};
pub use daemon::{DaemonConfig, RunningDaemon, start};
pub use error::DaemonError;
pub use paths::Paths;
pub use providers::GatewayConfig;
