//! Project commands: `project ls|open|rename|pin|unpin|close|recent|git-init`. Projects are the
//! repositories and folders the daemon makes from the folders sessions start in; they are
//! `WorkspaceInfo` with `kind: project` on the wire.

use std::collections::HashMap;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;

use hutan_protocol::messages::{
    CreatedBy, RecentProjects, Request, WorkspaceInfo, WorkspaceKind, WorkspaceOpen,
    WorkspaceOpened, WorkspaceRecent, WorkspaceRef,
};
use hutan_protocol::project::{ProjectGitInit, ProjectGitInitialized};
use serde_json::Value;

use super::Ctx;
use super::sessions::now_ms;
use super::workspaces::{
    Kind, active_text, attention_counts, is_listed_by_default, resolve_of, sort_workspaces, table,
};
use crate::args::{ProjectCommand, ProjectLsArgs, WsSort};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn project(&self, command: ProjectCommand) -> Result<(), CliError> {
        match command {
            ProjectCommand::Ls(args) => self.project_ls(args).await,
            ProjectCommand::Open { path } => self.project_open(path).await,
            ProjectCommand::Rename { target, name } => {
                self.rename_workspace(&target, name, Kind::Project).await
            }
            ProjectCommand::Pin { target } => {
                self.pin_workspace(&target, true, Kind::Project).await
            }
            ProjectCommand::Unpin { target } => {
                self.pin_workspace(&target, false, Kind::Project).await
            }
            ProjectCommand::Close { target } => self.project_close(&target).await,
            ProjectCommand::Recent { limit } => self.project_recent(limit).await,
            ProjectCommand::GitInit { target, home, yes } => {
                self.project_git_init(target.as_deref(), home, yes).await
            }
        }
    }

    /// `project git-init`: asks (`--yes` without a terminal), then `project.git_init`; the
    /// daemon's refusals reach the user as it words them.
    async fn project_git_init(
        &self,
        target: Option<&str>,
        home: bool,
        yes: bool,
    ) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let workspace = match (home, target) {
            (true, _) => all
                .iter()
                .find(|w| w.kind == WorkspaceKind::Home)
                .ok_or_else(|| CliError::Failed("the daemon lists no Home workspace".to_owned()))?,
            (false, Some(target)) => resolve_of(&all, target, Kind::Project)?,
            (false, None) => {
                return Err(CliError::Usage(
                    "name a project, or pass --home for the Home folder".to_owned(),
                ));
            }
        };
        let folder = workspace
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .or_else(|| workspace.roots.first().cloned())
            .map_or_else(|| workspace.name.clone(), |root| root.display().to_string());
        if !yes {
            confirm_git_init(&folder)?;
        }
        let done: ProjectGitInitialized = self
            .call(
                &client,
                Request::ProjectGitInit(ProjectGitInit {
                    project_id: workspace.id,
                }),
            )
            .await?;
        self.print(&done, || {
            format!(
                "tracking changes in {}: branch {}, first commit {} with {}{}",
                done.root.display(),
                done.branch,
                done.commit.get(..8).unwrap_or(&done.commit),
                if done.files == 1 {
                    "1 file".to_owned()
                } else {
                    format!("{} files", done.files)
                },
                if done.gitignore_written {
                    " (.gitignore written)"
                } else {
                    ""
                }
            )
        })
    }

    async fn project_ls(&self, args: ProjectLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let list = self.workspace_list(&client).await?;
        let attention = if args.sort == WsSort::Attention {
            let sessions = self.session_list(&client, None).await?;
            attention_counts(&sessions.sessions)
        } else {
            HashMap::new()
        };
        let mut projects: Vec<WorkspaceInfo> = list
            .workspaces
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Project)
            .filter(|w| args.all || is_listed_by_default(w))
            .cloned()
            .collect();
        sort_workspaces(&mut projects, args.sort, &attention);
        let workspaces = list.workspaces;
        let shown = serde_json::json!({ "projects": projects });
        self.print(&shown, || project_table(&projects, &workspaces, now_ms()))
    }

    async fn project_open(&self, path: PathBuf) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let path = super::paths::absolute(path)?;
        let client = self.connect().await?;
        let opened: WorkspaceOpened = self
            .call(
                &client,
                Request::WorkspaceOpen(WorkspaceOpen {
                    path,
                    created_by: Some(CreatedBy::Cli),
                }),
            )
            .await?;
        self.print(&opened, || {
            let workspace = &opened.workspace;
            match workspace.kind {
                WorkspaceKind::Home => {
                    format!(
                        "that folder is Home ({}), which is no project",
                        workspace.id
                    )
                }
                _ => format!(
                    "{} {} ({})",
                    if opened.created { "created" } else { "opened" },
                    workspace.id,
                    workspace.name
                ),
            }
        })
    }

    async fn project_close(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let project = resolve_of(&all, target, Kind::Project)?;
        let _: Value = self
            .call(
                &client,
                Request::WorkspaceClose(WorkspaceRef {
                    workspace_id: project.id,
                }),
            )
            .await?;
        self.print(
            &serde_json::json!({"workspace_id": project.id, "closed": true}),
            || format!("closed {} ({})", project.id, project.name),
        )
    }

    async fn project_recent(&self, limit: u32) -> Result<(), CliError> {
        let client = self.connect().await?;
        let recent: RecentProjects = self
            .call(
                &client,
                Request::WorkspaceRecent(WorkspaceRecent { limit: Some(limit) }),
            )
            .await?;
        self.print(&recent, || recent_table(&recent, now_ms()))
    }
}

/// Asks on the terminal; anything but `y` or `yes` is No.
fn confirm_git_init(folder: &str) -> Result<(), CliError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(CliError::Usage(format!(
            "nothing was changed: stdin is not a terminal; pass --yes to track changes in {folder}"
        )));
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "This runs git init in {folder}, writes a .gitignore with Hutan's scratch files and \
         commits what is there now with your git identity. Nothing leaves this machine."
    )?;
    write!(stderr, "Track changes in {folder}? [y/N] ")?;
    stderr.flush()?;
    let mut answer = String::new();
    stdin.lock().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::Failed("nothing was changed".to_owned()))
    }
}

fn project_table(projects: &[WorkspaceInfo], all: &[WorkspaceInfo], now_ms: u64) -> String {
    if projects.is_empty() {
        return "no projects".to_owned();
    }
    let rows = projects
        .iter()
        .map(|p| {
            vec![
                p.id.to_string(),
                p.name.clone(),
                p.parent_id
                    .and_then(|parent| all.iter().find(|w| w.id == parent))
                    .map_or_else(|| "-".to_owned(), |w| w.name.clone()),
                if p.pinned { "pinned" } else { "" }.to_owned(),
                format!("{}/{}", p.live_sessions, p.total_sessions),
                active_text(p.last_active_ms, now_ms),
                p.project
                    .as_ref()
                    .map(|project| project.root.clone())
                    .or_else(|| p.roots.first().cloned())
                    .map_or_else(String::new, |root| root.display().to_string()),
            ]
        })
        .collect();
    table(
        &[
            "ID",
            "NAME",
            "WORKSPACE",
            "PIN",
            "SESSIONS",
            "ACTIVE",
            "FOLDER",
        ],
        rows,
    )
}

fn recent_table(recent: &RecentProjects, now_ms: u64) -> String {
    if recent.projects.is_empty() {
        return "no recent projects".to_owned();
    }
    let rows = recent
        .projects
        .iter()
        .map(|p| {
            vec![
                p.name.clone(),
                p.workspace_id
                    .map_or_else(|| "closed".to_owned(), |id| id.to_string()),
                active_text(p.last_opened_ms, now_ms),
                p.path.display().to_string(),
            ]
        })
        .collect();
    table(&["NAME", "PROJECT", "OPENED", "PATH"], rows)
}
