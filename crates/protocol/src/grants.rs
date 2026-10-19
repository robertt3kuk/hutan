//! Grants of the control gate (protocol 0.31, capability `control.grants`; ADR 0017 decision 4,
//! `docs/control.md` § Mawas).
//!
//! When Mawas (the assistant) asks Hutan to change something, the user may answer a `control`
//! approval with **Allow for this chat** or **Always allow this tool in this workspace**; the
//! daemon stores that answer as a grant and consults it before it asks again. These two
//! requests list and revoke grants. Both are the user's: an agent-originated connection is
//! refused with `forbidden` (`user_only`), Mawas included, so nothing an agent runs can read
//! the grants it holds, widen them or take them away from the user.
//!
//! The approval's options are plain ids (`allow_once`, `allow_chat`, `allow_workspace`,
//! `deny`) in the existing `ApprovalInfo.options` / `approval.respond`; no new wire shape is
//! involved in answering.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The option that grants a tool to one chat (`approval.respond`).
pub const OPTION_ALLOW_CHAT: &str = "allow_chat";
/// The option that grants a tool in the workspace the chat lives in.
pub const OPTION_ALLOW_WORKSPACE: &str = "allow_workspace";

/// One stored grant: `tool` (the request's method, narrowed for `ui.run` per action and
/// `ui.set_setting` per namespace) may run without asking where the grant applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlGrant {
    /// Stable id, for `control.grants.revoke` and the audit trail.
    pub id: u64,
    /// The tool: a wire method (`session.rename`, `task.create`), or `ui.run:<action>` /
    /// `ui.set_setting:<namespace>`.
    pub tool: String,
    /// The grant covers chats of this workspace (an "always allow here" grant). Absent for a
    /// grant of one chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// The grant covers this chat only ("allow for this chat"). Absent for a workspace grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<SessionId>,
    pub created_ms: u64,
}

/// `control.grants.list`: the stored grants, newest first. With `workspace_id` only the grants
/// that apply to that workspace's chats (its own and those of single chats in it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ControlGrantsList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// The answer of `control.grants.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlGrants {
    pub grants: Vec<ControlGrant>,
}

/// `control.grants.revoke`: removes one grant; answered with `Empty`. `not_found` when it does
/// not exist (already revoked). The next request the grant covered asks again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlGrantRevoke {
    pub id: u64,
}
