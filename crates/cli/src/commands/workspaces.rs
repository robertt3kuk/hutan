//! Workspace commands: `ws ls|new|add|remove|rename|rm|move|pin|unpin|use`, the target
//! resolution every command that takes a workspace shares, and the tree `ws ls` prints.
//! Projects are `commands/projects.rs`.

use std::collections::HashMap;
use std::path::PathBuf;

use hutan_client::Client;
use hutan_core::{SessionState, WorkspaceId};
use hutan_protocol::messages::{
    AgentState, CreatedBy, Empty, Request, SessionInfo, WorkspaceCreate, WorkspaceInfo,
    WorkspaceKind, WorkspaceList, WorkspaceMove, WorkspacePin, WorkspaceRename, WorkspaceRoot,
};
use hutan_protocol::project::{WorkspaceAssign, WorkspaceDelete};
use serde_json::Value;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{WsCommand, WsLsArgs, WsMoveArgs, WsSort};
use crate::error::CliError;

/// Shortest workspace id prefix accepted as a target.
const MIN_PREFIX: usize = 4;

/// Environment variable naming the workspace `hutan new` uses when `--ws` is absent.
pub(super) const WORKSPACE_ENV: &str = "HUTAN_WORKSPACE";

impl Ctx {
    pub(super) async fn ws(&self, command: WsCommand) -> Result<(), CliError> {
        match command {
            WsCommand::Ls(args) => self.ws_ls(args).await,
            WsCommand::New { name, paths } => self.ws_new(name, paths).await,
            WsCommand::Add { workspace, what } => self.ws_add(&workspace, &what).await,
            WsCommand::Remove { workspace, project } => self.ws_remove(&workspace, &project).await,
            WsCommand::Rename { target, name } => {
                self.rename_workspace(&target, name, Kind::Container).await
            }
            WsCommand::Rm { target } => self.ws_rm(&target).await,
            WsCommand::Move(args) => self.ws_move(args).await,
            WsCommand::Pin { target } => self.pin_workspace(&target, true, Kind::Container).await,
            WsCommand::Unpin { target } => {
                self.pin_workspace(&target, false, Kind::Container).await
            }
            WsCommand::Use { target } => self.ws_use(&target).await,
        }
    }

    async fn ws_ls(&self, args: WsLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let list = self.workspace_list(&client).await?;
        // Only the attention order needs to look at the sessions.
        let attention = if args.sort == WsSort::Attention {
            let sessions = self.session_list(&client, None).await?;
            attention_counts(&sessions.sessions)
        } else {
            HashMap::new()
        };
        let tree = workspace_tree(&list.workspaces, args.sort, &attention, args.all);
        // `--json` carries the same rows in the same order; `parent_id` rebuilds the tree.
        let shown = WorkspaceList {
            workspaces: tree.iter().map(|row| row.workspace.clone()).collect(),
        };
        self.print(&shown, || tree_table(&tree, now_ms()))
    }

    async fn ws_new(&self, name: String, paths: Vec<PathBuf>) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let paths = paths
            .iter()
            .map(super::paths::absolute)
            .collect::<Result<Vec<_>, _>>()?;
        let client = self.connect().await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceCreate(WorkspaceCreate {
                    name,
                    paths,
                    project_ids: Vec::new(),
                    created_by: Some(CreatedBy::Cli),
                }),
            )
            .await?;
        self.print(&ws, || {
            format!(
                "created {} ({}) with {} project(s)",
                ws.id,
                ws.name,
                ws.roots.len()
            )
        })
    }

    /// `ws add WS PATH|PROJECT`: a path (it exists, and no project is called that) adds the
    /// project of the folder, anything else names an existing project.
    async fn ws_add(&self, workspace: &str, what: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let ws = resolve_of(&all, workspace, Kind::Workspace)?;
        let looks_like_path = what.contains('/') || what.starts_with('.') || what.starts_with('~');
        let project = if looks_like_path {
            None
        } else {
            match resolve_of(&all, what, Kind::Project) {
                Ok(project) => Some(project.id),
                Err(CliError::Failed(_)) if std::path::Path::new(what).is_dir() => None,
                Err(error) => return Err(error),
            }
        };
        let updated: WorkspaceInfo = match project {
            Some(project_id) => {
                let _: WorkspaceInfo = self
                    .call(
                        &client,
                        Request::WorkspaceAssign(WorkspaceAssign {
                            project_id,
                            workspace_id: Some(ws.id),
                        }),
                    )
                    .await?;
                // The workspace as it is now: its roots changed.
                self.workspace_list(&client)
                    .await?
                    .workspaces
                    .into_iter()
                    .find(|w| w.id == ws.id)
                    .ok_or_else(|| CliError::Failed(format!("workspace {} disappeared", ws.id)))?
            }
            None => {
                let path = super::paths::absolute(what)?;
                self.call(
                    &client,
                    Request::WorkspaceAddRoot(WorkspaceRoot {
                        workspace_id: ws.id,
                        path,
                    }),
                )
                .await?
            }
        };
        self.print(&updated, || {
            format!(
                "added to {} ({}); it has {} project(s)",
                updated.id,
                updated.name,
                updated.roots.len()
            )
        })
    }

    async fn ws_remove(&self, workspace: &str, project: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let ws = resolve_of(&all, workspace, Kind::Workspace)?;
        let project = resolve_of(&all, project, Kind::Project)?;
        if project.parent_id != Some(ws.id) {
            return Err(CliError::Failed(format!(
                "project {} ({}) is not in workspace {} ({})",
                project.id, project.name, ws.id, ws.name
            )));
        }
        let updated: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceAssign(WorkspaceAssign {
                    project_id: project.id,
                    workspace_id: None,
                }),
            )
            .await?;
        self.print(&updated, || {
            format!(
                "removed {} ({}) from {} ({}); it stays a project",
                project.id, project.name, ws.id, ws.name
            )
        })
    }

    async fn ws_rm(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let ws = resolve_of(&all, target, Kind::Workspace)?;
        let _: Value = self
            .call(
                &client,
                Request::WorkspaceDelete(WorkspaceDelete {
                    workspace_id: ws.id,
                }),
            )
            .await?;
        self.print(
            &serde_json::json!({"workspace_id": ws.id, "deleted": true}),
            || format!("deleted {} ({}); its projects stay", ws.id, ws.name),
        )
    }

    async fn ws_move(&self, args: WsMoveArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let moved = resolve_workspace(&all, &args.target)?;
        let before = match &args.before {
            Some(target) => Some(resolve_workspace(&all, target)?),
            None => None,
        };
        let list: WorkspaceList = self
            .call(
                &client,
                Request::WorkspaceMove(WorkspaceMove {
                    workspace_id: moved.id,
                    before: before.map(|w| w.id),
                }),
            )
            .await?;
        self.print(&list, || match before {
            Some(before) => format!("moved {} ({}) before {}", moved.id, moved.name, before.id),
            None => format!("moved {} ({}) to the end", moved.id, moved.name),
        })
    }

    /// `ws rename` and `project rename`.
    pub(super) async fn rename_workspace(
        &self,
        target: &str,
        name: String,
        kind: Kind,
    ) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let id = resolve_of(&all, target, kind)?.id;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceRename(WorkspaceRename {
                    workspace_id: id,
                    name,
                }),
            )
            .await?;
        self.print(&ws, || format!("renamed {} to {}", ws.id, ws.name))
    }

    /// `ws pin|unpin` and `project pin|unpin`.
    pub(super) async fn pin_workspace(
        &self,
        target: &str,
        pinned: bool,
        kind: Kind,
    ) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let id = resolve_of(&all, target, kind)?.id;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspacePin(WorkspacePin {
                    workspace_id: id,
                    pinned,
                }),
            )
            .await?;
        self.print(&ws, || {
            let verb = if pinned { "pinned" } else { "unpinned" };
            format!("{verb} {} ({})", ws.id, ws.name)
        })
    }

    async fn ws_use(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let ws = resolve_workspace(&all, target)?;
        let export = format!("export {WORKSPACE_ENV}={}", ws.id);
        self.print(
            &serde_json::json!({"workspace_id": ws.id, "name": ws.name, "export": export}),
            || export.clone(),
        )
    }

    // ---- helpers shared with the session commands ------------------------------------------

    pub(super) async fn workspace_list(&self, client: &Client) -> Result<WorkspaceList, CliError> {
        self.call(client, Request::WorkspaceList(Empty {})).await
    }

    /// The id of the workspace `target` names (see [`resolve_workspace`]).
    pub(super) async fn resolve_workspace_id(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<WorkspaceId, CliError> {
        let all = self.workspace_list(client).await?.workspaces;
        resolve_workspace(&all, target).map(|w| w.id)
    }
}

/// The workspace `hutan new` and `ls` were told to use: `--ws`, else `$HUTAN_WORKSPACE`.
pub(super) fn workspace_target(flag: Option<String>) -> Option<(String, &'static str)> {
    match flag {
        Some(target) => Some((target, "--ws")),
        // `$HUTAN_WORKSPACE` names a workspace of the local daemon.
        None if super::paths::is_remote() => None,
        None => std::env::var(WORKSPACE_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| (value, WORKSPACE_ENV)),
    }
}

/// What a target may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A workspace (the user's container of projects).
    Workspace,
    /// A workspace or Home: what `ws rename` and `ws pin` take.
    Container,
    /// A project.
    Project,
    /// Any of them, Home included.
    Any,
}

impl Kind {
    fn accepts(self, workspace: &WorkspaceInfo) -> bool {
        match self {
            Kind::Workspace => workspace.kind == WorkspaceKind::Workspace,
            Kind::Container => workspace.kind != WorkspaceKind::Project,
            Kind::Project => workspace.kind == WorkspaceKind::Project,
            Kind::Any => true,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::Workspace | Kind::Container => "workspace",
            Kind::Project => "project",
            Kind::Any => "workspace or project",
        }
    }

    fn listing(self) -> &'static str {
        match self {
            Kind::Workspace | Kind::Container => "hutan ws ls --all",
            Kind::Project => "hutan project ls --all",
            Kind::Any => "hutan ws ls --all",
        }
    }
}

/// Picks one workspace or project (or Home) by [`resolve_of`].
pub(super) fn resolve_workspace<'a>(
    workspaces: &'a [WorkspaceInfo],
    target: &str,
) -> Result<&'a WorkspaceInfo, CliError> {
    resolve_of(workspaces, target, Kind::Any)
}

/// Picks one workspace of `kind`. Tried in order: full id, exact name, name ignoring case, id
/// prefix of at least [`MIN_PREFIX`] characters. A name or prefix matching several is a usage
/// error that lists the candidates; no match is a plain failure, which says so when the target
/// names something of another kind.
pub(super) fn resolve_of<'a>(
    workspaces: &'a [WorkspaceInfo],
    target: &str,
    kind: Kind,
) -> Result<&'a WorkspaceInfo, CliError> {
    let pool: Vec<&WorkspaceInfo> = workspaces.iter().filter(|w| kind.accepts(w)).collect();
    if let Ok(id) = target.parse::<WorkspaceId>()
        && let Some(workspace) = pool.iter().find(|w| w.id == id)
    {
        return Ok(workspace);
    }
    let by_name: Vec<_> = pool.iter().copied().filter(|w| w.name == target).collect();
    if !by_name.is_empty() {
        return unique(target, by_name, kind);
    }
    let by_name_folded: Vec<_> = pool
        .iter()
        .copied()
        .filter(|w| w.name.to_lowercase() == target.to_lowercase())
        .collect();
    if !by_name_folded.is_empty() {
        return unique(target, by_name_folded, kind);
    }
    if target.len() >= MIN_PREFIX {
        let prefix = target.to_ascii_uppercase();
        let by_prefix: Vec<_> = pool
            .iter()
            .copied()
            .filter(|w| w.id.to_string().starts_with(&prefix))
            .collect();
        if !by_prefix.is_empty() {
            return unique(target, by_prefix, kind);
        }
    }
    if kind != Kind::Any
        && let Ok(other) = resolve_of(workspaces, target, Kind::Any)
    {
        return Err(CliError::Failed(format!(
            "`{target}` is {}, not a {}",
            match other.kind {
                WorkspaceKind::Home => "the Home workspace",
                WorkspaceKind::Project => "a project",
                WorkspaceKind::Workspace => "a workspace",
            },
            kind.noun()
        )));
    }
    let hint = if target.len() < MIN_PREFIX {
        format!(" (an id prefix needs at least {MIN_PREFIX} characters)")
    } else {
        String::new()
    };
    Err(CliError::Failed(format!(
        "no {} matches `{target}`{hint}; `{}` lists them",
        kind.noun(),
        kind.listing()
    )))
}

fn unique<'a>(
    target: &str,
    mut matches: Vec<&'a WorkspaceInfo>,
    kind: Kind,
) -> Result<&'a WorkspaceInfo, CliError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => {
            let mut message = format!("`{target}` matches {n} {}s; use a longer id:", kind.noun());
            for workspace in matches {
                message.push_str(&format!("\n  {}  {}", workspace.id, workspace.name));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// Without `--all`: Home, workspaces, pinned projects, projects in a workspace, and projects
/// that have sessions.
pub(super) fn is_listed_by_default(workspace: &WorkspaceInfo) -> bool {
    workspace.kind != WorkspaceKind::Project
        || workspace.pinned
        || workspace.parent_id.is_some()
        || workspace.total_sessions > 0
}

/// Sessions per workspace that wait for the user: a pending approval, or an agent that reports
/// being blocked on one.
pub(super) fn attention_counts(sessions: &[SessionInfo]) -> HashMap<WorkspaceId, u32> {
    let mut counts = HashMap::new();
    for session in sessions {
        let waiting = session.state == SessionState::WaitingApproval
            || (session.state.is_live()
                && session
                    .agent
                    .as_ref()
                    .is_some_and(|report| report.state == AgentState::Blocked));
        if waiting {
            *counts.entry(session.workspace_id).or_insert(0) += 1;
        }
    }
    counts
}

/// Pinned first, then by `mode`; the daemon's manual order breaks every tie.
pub(super) fn sort_workspaces(
    workspaces: &mut [WorkspaceInfo],
    mode: WsSort,
    attention: &HashMap<WorkspaceId, u32>,
) {
    workspaces.sort_by(|a, b| {
        let waiting = |w: &WorkspaceInfo| attention.get(&w.id).copied().unwrap_or(0);
        b.pinned.cmp(&a.pinned).then_with(|| {
            match mode {
                WsSort::Manual => std::cmp::Ordering::Equal,
                WsSort::Recent => b.last_active_ms.cmp(&a.last_active_ms),
                WsSort::Attention => waiting(b)
                    .cmp(&waiting(a))
                    .then_with(|| b.last_active_ms.cmp(&a.last_active_ms)),
                WsSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
            .then_with(|| a.position.cmp(&b.position))
        })
    });
}

/// One line of `ws ls`: a workspace or project and how deep it sits.
#[derive(Debug, PartialEq)]
pub(super) struct TreeRow {
    pub depth: usize,
    pub workspace: WorkspaceInfo,
}

/// The listing of `ws ls`: every workspace followed by its projects, then the projects in no
/// workspace, then Home. Each level follows `mode`; empty projects that are in no workspace
/// are left out unless `all` (or pinned).
pub(super) fn workspace_tree(
    workspaces: &[WorkspaceInfo],
    mode: WsSort,
    attention: &HashMap<WorkspaceId, u32>,
    all: bool,
) -> Vec<TreeRow> {
    // A workspace waits for the user when one of its projects does.
    let mut attention = attention.clone();
    for project in workspaces {
        if let Some(parent) = project.parent_id
            && let Some(waiting) = attention.get(&project.id).copied()
        {
            *attention.entry(parent).or_insert(0) += waiting;
        }
    }
    let of_kind = |kind: WorkspaceKind, parent: Option<WorkspaceId>| {
        let mut found: Vec<WorkspaceInfo> = workspaces
            .iter()
            .filter(|w| w.kind == kind && w.parent_id == parent)
            .filter(|w| all || is_listed_by_default(w))
            .cloned()
            .collect();
        sort_workspaces(&mut found, mode, &attention);
        found
    };
    let mut rows = Vec::new();
    for container in of_kind(WorkspaceKind::Workspace, None) {
        let id = container.id;
        rows.push(TreeRow {
            depth: 0,
            workspace: container,
        });
        // Members are always shown: someone put them there.
        let mut members: Vec<WorkspaceInfo> = workspaces
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Project && w.parent_id == Some(id))
            .cloned()
            .collect();
        sort_workspaces(&mut members, mode, &attention);
        rows.extend(members.into_iter().map(|workspace| TreeRow {
            depth: 1,
            workspace,
        }));
    }
    rows.extend(
        of_kind(WorkspaceKind::Project, None)
            .into_iter()
            .map(|workspace| TreeRow {
                depth: 0,
                workspace,
            }),
    );
    rows.extend(
        workspaces
            .iter()
            .filter(|w| w.kind == WorkspaceKind::Home)
            .cloned()
            .map(|workspace| TreeRow {
                depth: 0,
                workspace,
            }),
    );
    rows
}

fn kind_text(workspace: &WorkspaceInfo) -> &'static str {
    match workspace.kind {
        WorkspaceKind::Home => "home",
        WorkspaceKind::Project => "project",
        WorkspaceKind::Workspace => "workspace",
    }
}

/// The folder column: a project's folder, Home's folder; a workspace has none of its own.
fn folder_text(workspace: &WorkspaceInfo) -> String {
    match workspace.kind {
        WorkspaceKind::Workspace => String::new(),
        _ => workspace
            .roots
            .first()
            .map_or_else(String::new, |root| root.display().to_string()),
    }
}

fn tree_table(rows: &[TreeRow], now_ms: u64) -> String {
    if rows.is_empty() {
        return "no workspaces".to_owned();
    }
    let cells = rows
        .iter()
        .map(|row| {
            let w = &row.workspace;
            vec![
                w.id.to_string(),
                format!("{}{}", "  ".repeat(row.depth), w.name),
                kind_text(w).to_owned(),
                if w.pinned { "pinned" } else { "" }.to_owned(),
                format!("{}/{}", w.live_sessions, w.total_sessions),
                active_text(w.last_active_ms, now_ms),
                folder_text(w),
            ]
        })
        .collect();
    table(
        &["ID", "NAME", "KIND", "PIN", "SESSIONS", "ACTIVE", "FOLDER"],
        cells,
    )
}

/// `5s`, `3m`, ... since `at_ms`; `-` when it never happened.
pub(super) fn active_text(at_ms: u64, now_ms: u64) -> String {
    if at_ms == 0 {
        "-".to_owned()
    } else {
        age_text(now_ms.saturating_sub(at_ms) / 1000)
    }
}

/// Columns padded to their widest cell, two spaces apart; the last column is not padded.
pub(super) fn table(header: &[&str], rows: Vec<Vec<String>>) -> String {
    let header: Vec<String> = header.iter().map(|h| (*h).to_owned()).collect();
    let mut widths = vec![0; header.len()];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let last = header.len() - 1;
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let mut line = String::new();
            for (index, (cell, width)) in row.iter().zip(&widths).enumerate() {
                if index == last {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, name: &str, position: i64) -> WorkspaceInfo {
        WorkspaceInfo {
            id: id.parse().unwrap(),
            name: name.to_owned(),
            roots: Vec::new(),
            kind: WorkspaceKind::Project,
            pinned: false,
            position,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: None,
            project: None,
        }
    }

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";
    const C: &str = "01K0000000000000000000000C";

    fn fixture() -> Vec<WorkspaceInfo> {
        vec![ws(A, "alpha", 0), ws(B, "Beta", 1), ws(C, "alpha-2", 2)]
    }

    #[test]
    fn resolves_full_id_name_folded_name_and_prefix() {
        let all = fixture();
        assert_eq!(resolve_workspace(&all, A).unwrap().name, "alpha");
        assert_eq!(resolve_workspace(&all, "Beta").unwrap().id.to_string(), B);
        assert_eq!(resolve_workspace(&all, "beta").unwrap().id.to_string(), B);
        assert_eq!(
            resolve_workspace(&all, "01k0").unwrap().id.to_string(),
            C,
            "prefixes are case-insensitive"
        );
    }

    #[test]
    fn ambiguous_targets_are_usage_errors_and_unmatched_ones_fail() {
        let all = fixture();
        let error = resolve_workspace(&all, "01J0").unwrap_err();
        assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        assert!(error.to_string().contains(A) && error.to_string().contains(B));
        for target in ["nope", "01J", "alp"] {
            let error = resolve_workspace(&all, target).unwrap_err();
            assert!(matches!(error, CliError::Failed(_)), "{target}: {error:?}");
        }
        let mut twins = fixture();
        twins[2].name = "alpha".to_owned();
        assert!(matches!(
            resolve_workspace(&twins, "alpha"),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn a_target_of_the_wrong_kind_says_what_it_is() {
        let mut all = fixture();
        all[0].kind = WorkspaceKind::Workspace;
        assert_eq!(
            resolve_of(&all, "alpha", Kind::Workspace)
                .unwrap()
                .id
                .to_string(),
            A
        );
        let error = resolve_of(&all, "Beta", Kind::Workspace).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`Beta` is a project, not a workspace"),
            "{error}"
        );
        let error = resolve_of(&all, "alpha", Kind::Project).unwrap_err();
        assert!(
            error.to_string().contains("is a workspace, not a project"),
            "{error}"
        );
        // The same name in two kinds is no ambiguity once the kind is known.
        all[1].name = "alpha".to_owned();
        assert_eq!(
            resolve_of(&all, "alpha", Kind::Project)
                .unwrap()
                .id
                .to_string(),
            B
        );
        assert!(matches!(
            resolve_of(&all, "alpha", Kind::Any),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn the_default_listing_hides_empty_projects_except_pinned_and_members() {
        let mut all = fixture();
        all[1].total_sessions = 1;
        all[2].pinned = true;
        let mut member = ws("01J0000000000000000000000M", "member", 3);
        member.parent_id = Some(all[0].id);
        all.push(member);
        let mut container = ws("01J0000000000000000000000W", "shop", 4);
        container.kind = WorkspaceKind::Workspace;
        all.push(container);
        let mut home = ws("01J0000000000000000000000H", "Home", -1);
        home.kind = WorkspaceKind::Home;
        all.push(home);
        let shown: Vec<_> = all
            .iter()
            .filter(|w| is_listed_by_default(w))
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(shown, ["Beta", "alpha-2", "member", "shop", "Home"]);
    }

    #[test]
    fn sorting_puts_pins_first_and_breaks_ties_by_position() {
        let mut all = fixture();
        all[0].last_active_ms = 10;
        all[1].last_active_ms = 30;
        all[2].last_active_ms = 30;
        let attention = HashMap::from([(all[0].id, 2)]);
        let order = |mode, all: &mut Vec<WorkspaceInfo>| {
            sort_workspaces(all, mode, &attention);
            all.iter().map(|w| w.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            order(WsSort::Manual, &mut all),
            ["alpha", "Beta", "alpha-2"]
        );
        assert_eq!(
            order(WsSort::Recent, &mut all),
            ["Beta", "alpha-2", "alpha"]
        );
        assert_eq!(
            order(WsSort::Attention, &mut all),
            ["alpha", "Beta", "alpha-2"]
        );
        assert_eq!(order(WsSort::Name, &mut all), ["alpha", "alpha-2", "Beta"]);
        all.iter_mut().find(|w| w.name == "alpha-2").unwrap().pinned = true;
        assert_eq!(
            order(WsSort::Manual, &mut all),
            ["alpha-2", "alpha", "Beta"]
        );
        assert_eq!(order(WsSort::Name, &mut all), ["alpha-2", "alpha", "Beta"]);
    }

    #[test]
    fn the_tree_lists_workspaces_with_their_projects_then_loose_projects_then_home() {
        let id = |n: u32| format!("01J00000000000000000000{n:03}");
        let mut home = ws(&id(1), "Home", 0);
        home.kind = WorkspaceKind::Home;
        let mut shop = ws(&id(2), "shop", 1);
        shop.kind = WorkspaceKind::Workspace;
        let mut lab = ws(&id(3), "lab", 2);
        lab.kind = WorkspaceKind::Workspace;
        let mut web = ws(&id(4), "web", 3);
        web.parent_id = Some(shop.id);
        let mut api = ws(&id(5), "api", 4);
        api.parent_id = Some(shop.id);
        api.pinned = true;
        let mut busy = ws(&id(6), "busy", 5);
        busy.total_sessions = 2;
        let idle = ws(&id(7), "idle", 6);
        let all = [home, web, shop, busy, idle, api, lab];

        let names = |tree: &[TreeRow]| {
            tree.iter()
                .map(|row| format!("{}{}", "-".repeat(row.depth), row.workspace.name))
                .collect::<Vec<_>>()
        };
        let tree = workspace_tree(&all, WsSort::Manual, &HashMap::new(), false);
        // Pinned members first inside their workspace; the empty loose project is hidden.
        assert_eq!(
            names(&tree),
            ["shop", "-api", "-web", "lab", "busy", "Home"]
        );
        let tree = workspace_tree(&all, WsSort::Name, &HashMap::new(), true);
        assert_eq!(
            names(&tree),
            ["lab", "shop", "-api", "-web", "busy", "idle", "Home"]
        );
        // A project that waits for the user brings its workspace up.
        let waiting = HashMap::from([(all[1].id, 1)]);
        let tree = workspace_tree(&all, WsSort::Attention, &waiting, false);
        assert_eq!(names(&tree)[0], "shop");
        let text = tree_table(&tree, 10_000);
        assert!(text.lines().next().unwrap().starts_with("ID"), "{text}");
        assert!(text.contains("  api"), "members are indented: {text}");
    }

    #[test]
    fn tables_pad_all_but_the_last_column() {
        let text = table(
            &["A", "BB", "C"],
            vec![vec!["x".into(), "yyy".into(), "z".into()]],
        );
        assert_eq!(text, "A  BB   C\nx  yyy  z");
    }
}
