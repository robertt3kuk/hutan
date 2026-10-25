//! Hutan IPC protocol (ADR 0002, ADR 0004, `docs/protocol.md`).
//!
//! - [`hooks`]: user hooks — `hooks.list`, `hooks.log`, `hooks.test` and the payload a hook
//!   receives (protocol 0.49, capability `hooks`).
//! - [`memory_files`]: Mawas's memory as Markdown files — the format of the mirror, the export
//!   and the import — and `memory.import` / `memory.files.status` (protocol 0.46, capability
//!   `memory.files`); [`memory`]'s provenance names the origin of an edit made in a file.
//! - [`git`]: read-only git history — `git.log`, `git.blame`, `git.show` and `git.file` (protocol
//!   0.47, capability `git.history`).
//! - [`terminal_search`]: find in terminal output — `terminal.search` (protocol 0.43, capability
//!   `terminal.search`).
//! - [`messages`] also carries which daemon answers — `Welcome.daemon_build` (protocol 0.42,
//!   capability `daemon.identity`) — and a stop someone asked for — the `daemon.stopping` event
//!   and `daemon.shutdown`'s `restart` (protocol 0.42, capability `daemon.stopping`).
//! - [`lsp`]: language servers in the daemon — `doc.open|change|save|close` (capability
//!   `doc.sync`), `lsp.status|diagnostics|hover|definition|restart|stop|trust` and the events
//!   `lsp.state` and `lsp.diagnostics` (protocol 0.39, capability `lsp`), completion and
//!   signature help — `lsp.completion|completion_resolve|signature_help` (protocol 0.41,
//!   capability `lsp.completion`); [`resources`] lists the servers as `services`.
//! - [`checkpoint`]: checkpoints of agent edits on the wire — `checkpoint.list|diff|remove`, the
//!   events `checkpoint.created|removed|state` and `ChatInfo.checkpoints` (protocol 0.37,
//!   capability `checkpoint`, ADR 0019); restoring files to before turns —
//!   `checkpoint.preview|restore` and `fs.changed` origin `restore` (protocol 0.38, capability
//!   `checkpoint.restore`); what a restore restored, `CheckpointInfo.restores` and
//!   `restores_through` (protocol 0.40, capability `checkpoint.restores`).
//! - [`chat`] also carries the daemon-owned queue of a chat: `chat.queue.add|edit|remove|clear|list`,
//!   the `chat.queue` event, `chat.send_now` (steer where the agent can, else interrupt and send)
//!   and `chat.interrupt`'s `keep_queue` (protocol 0.36, capability `chat.queue`).
//! - [`worktree`] also carries `worktree.finish`: the user's Finish — commit, merge a worktree,
//!   push to the upstream, step by step, user-only — and [`ui`]'s `PaneItem.path` names a file
//!   tab's file (protocol 0.35, capability `finish`).
//! - [`notify`] also carries `notify.mute` and `SessionInfo.muted`; a focused session raises no
//!   intent and focusing one withdraws its intents (protocol 0.34, capability `notify.mute`).
//! - [`chat`] also carries `chat.retry`: the last user message again, journaled once
//!   (protocol 0.34, capability `chat.retry`).
//! - [`usage`]: `usage.summary` and the `usage.changed` event: the limit windows harnesses report
//!   and the tokens and cost of the turns Hutan ran (protocol 0.45, capability `usage`).
//! - [`fs`]: the daemon's file service: `fs.read`, chunked atomic `fs.write.*`, `fs.stat`,
//!   `fs.list`, `fs.find`, `fs.search`, `fs.watch` and the `fs.changed` event (protocol 0.33,
//!   capabilities `fs`, `fs.watch`).
//! - [`memory`]: Mawas's curated memory: entries, revisions, proposals, receipts (protocol 0.32,
//!   capability `memory`).
//! - [`grants`]: `control.grants.list` / `.revoke`, the grants Mawas's approvals store (protocol
//!   0.31, capability `control.grants`).
//! - [`daemon_settings`]: `daemon.toml` as a list of settings, `daemon.settings.get` / `.set`
//!   and `assistant.reset` (protocol 0.28, capability `daemon.settings`).
//! - [`account`]: harness accounts (protocol 0.17, capability `accounts`); [`chat`] also takes
//!   `chat.create` for the ACP harnesses `omp` and `claude` (capability `harness.acp`).
//! - [`resources`]: process trees, usage, stop/kill/interrupt (protocol 0.18, capability
//!   `resources`).
//! - [`project`]: automatic projects and user workspaces (protocol 0.19, capability
//!   `projects.auto`).
//! - [`ui`]: the agent control surface: action catalog, layout, `ui.run` (protocol 0.21,
//!   capability `control`).
//! - [`task`] also carries the delivery facet (`delivery`, `delivery_history`, `after_policy`),
//!   `task.review` and `worktree.commit`, and [`worktree`]'s `worktree.diff` says what is
//!   committed and what is not (protocol 0.25, capability `task.review`).
//! - [`recovery`]: what a daemon restart did to each session, `session.recovery_ack` (protocol
//!   0.26, capability `session.recovery`).
//! - [`assistant`] also carries rich input: `image` and `pasted_text` attachments, `file` for
//!   every chat, `ChatInfo.images`, error code `unsupported` (protocol 0.23, capability
//!   `chat.rich_input`).
//! - [`worktree`] also carries `worktree.diff`, `.file_diff` and `.merge` (protocol 0.22, capability
//!   `worktree.review`).
//! - [`task`]: tasks with dependencies (protocol 0.16, capability `task`).
//! - [`chat`] also carries provenance and mid-stream partials (protocol 0.15, capability
//!   `chat.provenance`); [`history`] hits carry their origin (path, terminal and block).
//! - [`assistant`]: assistant chat DTOs and context attachments (protocol 0.12); backends per
//!   scope, `assistant.configure` (protocol 0.20, capability `assistant.backend`).
//! - [`notify`]: notification intents, `client.focus` (protocol 0.13).
//! - [`blocks`]: terminal block DTOs (protocol 0.5).
//! - [`chat`]: agent chat and approval DTOs (protocol 0.9).
//! - [`provider`]: model provider and direct chat DTOs (protocol 0.11).
//! - [`history`]: session history search DTOs (protocol 0.8).
//! - [`frame`]: length-prefixed frames and the tokio codec.
//! - [`control`]: JSON control envelope (requests, responses, events).
//! - [`messages`]: typed request params, results, events, and DTOs.
//! - [`worktree`]: worktrees and session placement (protocol 0.14).
//! - [`tree`]: agent observation tree DTOs (protocol 0.10).
//! - [`render`]: terminal render snapshot/frame DTOs carried in binary frames.

pub mod account;
pub mod assistant;
pub mod blocks;
pub mod chat;
pub mod checkpoint;
pub mod control;
pub mod daemon_settings;
pub mod error;
pub mod frame;
pub mod fs;
pub mod git;
pub mod grants;
pub mod history;
pub mod hooks;
pub mod lsp;
pub mod memory;
pub mod memory_files;
pub mod messages;
pub mod notify;
pub mod project;
pub mod provider;
pub mod recovery;
pub mod render;
pub mod resources;
pub mod task;
pub mod terminal_search;
pub mod tree;
pub mod ui;
pub mod usage;
pub mod worktree;

pub use control::{ControlMessage, ErrorBody, ErrorCode};
pub use error::ProtocolError;
pub use frame::{Frame, FrameCodec, FrameKind, MAX_PAYLOAD};
pub use messages::{Event, Request};

use serde::{Deserialize, Serialize};

/// Version spoken by this build. Minor = additive, major = breaking (ADR 0002).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 49,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Peers interoperate iff majors match; capabilities gate minor additions.
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}
