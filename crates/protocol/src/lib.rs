//! Hutan IPC protocol (ADR 0002, ADR 0004, `docs/protocol.md`).
//!
//! - [`review`], [`assistant`]: `agents.review` and the `turn_review` attachment — what an agent did
//!   in one turn, from the journal, the checkpoint and the language servers, with named heuristic
//!   signals and what is not recorded (protocol 0.67, capability `agents.review`, ADR 0025).
//! - [`stats`]: `agents.stats` — where the time goes, what agents wait on and what fails again,
//!   with the coverage of every number (protocol 0.66, capability `agents.stats`, ADR 0025).
//! - [`activity`]: `agents.activity` — what happened across agents in a window, from what the
//!   daemon already records: turns, requests, checkpoints, tasks, sessions; bounded, read-only and
//!   redacted (protocol 0.65, capability `agents.activity`, ADR 0025).
//! - [`board`]: `agents.board` — every agent across workspaces in one bounded, read-only,
//!   redacted answer: state and group of the shared reducer, what each waits for, where its facts
//!   come from (protocol 0.63, capability `agents.board`, ADR 0025).
//! - [`suggest`], [`chat`]: `ask.suggest` and the `suggestion` of a pending approval or question
//!   — an advisory, unsent answer from Mawas that the user may *Use* or dismiss — with the
//!   `ask.suggestion` event (protocol 0.64, capability `ask.suggest`, ADR 0025 R2).
//! - [`grants`] and [`chat`] also carry the target-bound grants of Mawas acting on another agent:
//!   `ControlGrant.target_session_id`, `ApprovalInfo.target` (who is addressed and the whole
//!   text), `QueuedMessage.origin` and the `origin` of a user message's payload (protocol 0.62,
//!   capability `control.grants.target`, ADR 0025 decision 3).
//! - [`assistant`] also carries `assistant.focus` and `ChatInfo.focus` — the workspace a Global
//!   Mawas chat looks at, a filter and a source of notes and project instructions that changes
//!   no permission (protocol 0.61, capability `assistant.focus`, ADR 0025).
//! - [`update`]: `update.status`, `update.check`, `update.install` and the events
//!   `update.available` and `update.install` — the daemon's check for a newer release and the
//!   Homebrew update it runs for the user (protocol 0.60, capability `update`, ADR 0024).
//! - [`insight`]: `agent.insight`, `agent.measure_context` and the `agent.insight_updated` event —
//!   what a chat's context holds, what each turn used, tools, files and time, with the source of
//!   every block (protocol 0.59, capability `agent.insight`); `TokenCounts` gains
//!   `cache_write_tokens`.
//! - [`chat`] also carries where a fork works — `ChatFork.where: new_worktree` (a git worktree
//!   whose files are the checkpoint of the turn), `branch`, `restore_files`, `archive_parent`,
//!   and `ForkDone.worktree` / `restored` / `parent_archived` / `notes` (protocol 0.58,
//!   capability `chat.fork.worktree`, ADR 0023 decision 3).
//! - [`chat`] also carries session folders — `ChatCreate.extra_dirs`, `TerminalSpec.extra_dirs`,
//!   `chat.add_dir`, `chat.remove_dir`, `ChatInfo.dirs` and `SessionInfo.extra_dirs` (protocol
//!   0.57, capability `session.dirs`, ADR 0023).
//! - [`messages`] also carries `SessionInfo.terminal_agent` — the coding agent recognised in a
//!   terminal session, from OSC 7501 or its program (protocol 0.55, capability `terminal.agent`).
//! - [`chat`] also carries `chat.fork` — a new chat that starts with a chat's conversation up to
//!   a turn (`exact`, `head` or `summary`; journal copied, parent untouched), `ChatInfo.fork`
//!   and `SessionInfo.forked_from` (protocol 0.56, capability `chat.fork`, ADR 0023).
//! - [`fs`] also carries file operations — `fs.create`, `fs.rename`, `fs.move`, `fs.copy`,
//!   `fs.trash` (undo tickets) and `fs.restore` (protocol 0.54, capability `fs.ops`, ADR 0012
//!   amendment 2026-10-27).
//! - [`worktree`] also carries staging — `worktree.hunks`, `worktree.stage`, `worktree.unstage`,
//!   `worktree.discard` (all user-only but the read `hunks`), `WorktreeCommit.staged_only` and
//!   `WorktreeFinish.staged_only` (protocol 0.53, capability `worktree.stage`, ADR 0020).
//! - [`hooks`]: user hooks — `hooks.list`, `hooks.log`, `hooks.test` and the payload a hook
//!   receives (protocol 0.48, capability `hooks`).
//! - [`worktree`] also carries setup and retention — `worktree.setup`, `worktree.setup_trust`
//!   (user-only), `worktree.cleanup`, `worktree.prune` (user-only), `WorktreeCreate.no_setup` and
//!   `WorktreeInfo.setup` (protocol 0.49, capability `worktree.setup`).
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
pub mod activity;
pub mod assistant;
pub mod blocks;
pub mod board;
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
pub mod insight;
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
pub mod review;
pub mod stats;
pub mod suggest;
pub mod task;
pub mod terminal_search;
pub mod tree;
pub mod ui;
pub mod update;
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
    minor: 67,
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
