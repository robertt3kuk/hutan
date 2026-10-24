//! Automatic projects and user workspaces (protocol 0.19, capability `projects.auto`): the
//! project identity carried by `WorkspaceInfo`, and the DTOs of `workspace.assign` and
//! `workspace.delete` (`docs/protocol.md` § Projects and workspaces, `docs/orchestration.md`
//! § Projects, ADR 0014).
//!
//! Both kinds of container are `WorkspaceInfo`. A **project** (`kind: project`) is a repository
//! or a folder, made by the daemon when a session needs it; its identity is the git common
//! directory of the repository. A **workspace** (`kind: workspace`) is the user's own: a name,
//! a pin, an order, and member projects (`WorkspaceInfo.parent_id` on the project). A project
//! is in at most one workspace.
//!
//! Errors, in short: `not_found` (an unknown workspace or project), `bad_request` (an empty or
//! too long name, a path that is no directory), `invalid_state` (the id names the wrong kind:
//! `project_id` must be a project, `workspace_id` a workspace; a workspace with live sessions
//! is deleted).

use std::path::PathBuf;

use hutan_core::WorkspaceId;
use serde::{Deserialize, Serialize};

/// What identifies a project (`WorkspaceInfo.project`; present for `kind: project` only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    /// The git common directory of the project's repository (canonical): shared by the main
    /// checkout and every linked worktree. Absent for a folder outside git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_dir: Option<PathBuf>,
    /// The project's folder: the main checkout of its repository, else the folder itself.
    /// `.hutan/settings.toml` is read from here.
    pub root: PathBuf,
}

/// `workspace.assign`: puts a project into a workspace, or out of its workspace
/// (`workspace_id` absent). Answered with the project's `WorkspaceInfo`; `workspace.updated`
/// follows for the project and for every workspace whose members changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAssign {
    pub project_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// `workspace.delete`: removes a workspace (`kind: workspace`). Its projects stay, ungrouped
/// (one `workspace.updated` each); `workspace.closed` announces the removal. A workspace with
/// live sessions of its own is `invalid_state`; its ended sessions and its tasks go with it.
/// Answered with `Empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDelete {
    pub workspace_id: WorkspaceId,
}

/// `project.git_init` (protocol 0.44, capability `project.git_init`; user-only, never an
/// agent's): turns a project's folder, or the Home folder (`project_id` names the Home
/// workspace), into a git repository so Changes, checkpoints and Finish work there. The daemon
/// writes a `.gitignore` with Hutan's own scratch entries (appended to an existing one), runs
/// `git init`, and commits what is there now with the user's own git identity. Nothing leaves
/// the machine.
///
/// Refused: `invalid_state` when the folder is the user's home directory or `/`, is a
/// repository already, lies inside another repository, has more files than checkpoints can
/// seed (`[checkpoints] max_untracked_files`), or when git has no identity to commit with
/// (the message says how to set one); `forbidden` for an agent-originated connection;
/// `not_found` for an unknown id; `invalid_state` for a workspace (it has no folder of its
/// own); `bad_request` when the folder is gone. A refusal leaves the folder as it was.
/// Answered with [`ProjectGitInitialized`]; `worktree.changed` follows for the workspaces
/// holding the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGitInit {
    pub project_id: WorkspaceId,
}

/// Result of `project.git_init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGitInitialized {
    /// The folder that is a repository now (canonical).
    pub root: PathBuf,
    /// The branch the initial commit is on.
    pub branch: String,
    /// The initial commit (full hash).
    pub commit: String,
    /// Files the initial commit holds.
    pub files: u32,
    /// Whether Hutan wrote `.gitignore` (created it, or added its entries to the existing one).
    pub gitignore_written: bool,
}
