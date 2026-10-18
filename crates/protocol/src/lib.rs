//! Hutan IPC protocol (ADR 0002, ADR 0004, `docs/protocol.md`).
//!
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
pub mod control;
pub mod daemon_settings;
pub mod error;
pub mod frame;
pub mod history;
pub mod messages;
pub mod notify;
pub mod project;
pub mod provider;
pub mod recovery;
pub mod render;
pub mod resources;
pub mod task;
pub mod tree;
pub mod ui;
pub mod worktree;

pub use control::{ControlMessage, ErrorBody, ErrorCode};
pub use error::ProtocolError;
pub use frame::{Frame, FrameCodec, FrameKind, MAX_PAYLOAD};
pub use messages::{Event, Request};

use serde::{Deserialize, Serialize};

/// Version spoken by this build. Minor = additive, major = breaking (ADR 0002).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion {
    major: 0,
    minor: 29,
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
