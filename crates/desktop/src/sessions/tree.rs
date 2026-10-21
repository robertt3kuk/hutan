//! The sidebar's tree (M18-B): user workspaces → their projects → sessions, the projects that
//! belong to no workspace, then Home; or a flat list of projects. Pure data: the daemon lists
//! containers and sessions, the modes, filters and the search are the client's business
//! (`docs/design.md` § Projects and workspaces).

use std::collections::HashMap;
use std::path::Path;

use hutan_core::WorkspaceId;
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo, WorkspaceKind};

use super::placement::{PlacementBadge, row_placement_badge};
use super::workspaces as order;
use crate::settings::{SidebarOrganize, WorkspaceSort, WorkspacesSettings};

/// What the tree is built from.
pub struct Inputs<'a> {
    /// Every container the daemon knows: Home, projects and workspaces.
    pub workspaces: &'a [WorkspaceInfo],
    /// The sessions the tree may list (a session that "Needs you" took out is not one of them).
    pub sessions: &'a [&'a SessionInfo],
    pub organize: SidebarOrganize,
    pub sort: WorkspaceSort,
    pub filters: &'a WorkspacesSettings,
    /// The filter box: words in any order, matched against names, folders and sessions.
    pub query: &'a str,
    /// The container and the project on screen: the filters never hide them.
    pub active: [Option<WorkspaceId>; 2],
    /// Sessions that need the user, per container they live in.
    pub needs: &'a HashMap<WorkspaceId, usize>,
    /// Whether a session matches the query by itself (only asked while searching).
    pub session_matches: &'a dyn Fn(&SessionInfo) -> bool,
}

impl std::fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inputs")
            .field("organize", &self.organize)
            .field("sort", &self.sort)
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

/// A project (or Home) with the sessions listed under it.
#[derive(Debug, Clone)]
pub struct ProjectNode {
    pub info: WorkspaceInfo,
    pub sessions: Vec<SessionInfo>,
    /// Sessions of the project that need the user, listed or not.
    pub needs: usize,
}

/// A user workspace with its member projects.
#[derive(Debug, Clone)]
pub struct WorkspaceNode {
    pub info: WorkspaceInfo,
    pub projects: Vec<ProjectNode>,
    /// Sessions that were started straight into the workspace (`workspace_id` of the workspace
    /// itself): listed after its projects.
    pub sessions: Vec<SessionInfo>,
    /// Sessions of the workspace and of its projects that need the user.
    pub needs: usize,
}

#[derive(Debug, Clone)]
pub enum Node {
    Workspace(WorkspaceNode),
    /// A project at the top level: in no workspace, or any project in the flat mode.
    Project(ProjectNode),
    /// The label above the projects that belong to no workspace; also where a project is
    /// dropped to leave its workspace.
    NoWorkspace,
    Home(ProjectNode),
}

/// The tree in display order.
#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

/// `workspaces` aggregated up: every container's count plus its member projects'.
fn aggregate(
    workspaces: &[WorkspaceInfo],
    needs: &HashMap<WorkspaceId, usize>,
) -> HashMap<WorkspaceId, usize> {
    let mut total = needs.clone();
    for (id, count) in needs {
        let parent = workspaces
            .iter()
            .find(|w| w.id == *id)
            .and_then(|w| w.parent_id);
        if let Some(parent) = parent {
            *total.entry(parent).or_insert(0) += count;
        }
    }
    total
}

/// The tree for `inputs`.
pub fn build(inputs: &Inputs<'_>) -> Tree {
    let searching = !inputs.query.trim().is_empty();
    let workspaces = inputs.workspaces;
    let matches = |w: &WorkspaceInfo| order::matches_query(w, inputs.query);
    let parent_matches = |w: &WorkspaceInfo| {
        w.parent_id
            .and_then(|id| workspaces.iter().find(|p| p.id == id))
            .is_some_and(matches)
    };
    let hidden = |w: &WorkspaceInfo| {
        order::hidden(w, inputs.filters, inputs.active[0])
            && order::hidden(w, inputs.filters, inputs.active[1])
    };
    // Sessions of `container` that the search lists: all of them when the search is off or
    // matches the container (or `inherited` says an ancestor does), else the ones that match.
    let sessions_of = |container: &WorkspaceInfo, inherited: bool| -> (Vec<SessionInfo>, bool) {
        let whole = !searching || inherited || matches(container);
        let listed: Vec<SessionInfo> = inputs
            .sessions
            .iter()
            .filter(|s| s.workspace_id == container.id)
            .filter(|s| whole || (inputs.session_matches)(s))
            .map(|s| (*s).clone())
            .collect();
        (listed, whole)
    };
    let project_node = |project: &WorkspaceInfo, inherited: bool| -> Option<ProjectNode> {
        let (sessions, whole) = sessions_of(project, inherited);
        if searching && !whole && sessions.is_empty() {
            return None;
        }
        Some(ProjectNode {
            info: project.clone(),
            needs: inputs.needs.get(&project.id).copied().unwrap_or(0),
            sessions,
        })
    };
    let sorted = |group: Vec<WorkspaceInfo>| -> Vec<WorkspaceInfo> {
        order::sort(&group, inputs.sort, inputs.needs)
            .into_iter()
            .cloned()
            .collect()
    };
    let of_kind = |kind: WorkspaceKind| -> Vec<WorkspaceInfo> {
        workspaces
            .iter()
            .filter(|w| w.kind == kind)
            .cloned()
            .collect()
    };
    let is_member = |w: &WorkspaceInfo| {
        w.parent_id
            .is_some_and(|id| workspaces.iter().any(|p| p.id == id))
    };

    let aggregated = aggregate(workspaces, inputs.needs);
    let mut nodes = Vec::new();
    let user_workspaces = of_kind(WorkspaceKind::Workspace);
    match inputs.organize {
        SidebarOrganize::Workspaces => {
            let attention = aggregated.clone();
            let ordered: Vec<WorkspaceInfo> =
                order::sort(&user_workspaces, inputs.sort, &attention)
                    .into_iter()
                    .cloned()
                    .collect();
            for workspace in ordered {
                let inherited = matches(&workspace);
                let members: Vec<WorkspaceInfo> = workspaces
                    .iter()
                    .filter(|w| {
                        w.kind == WorkspaceKind::Project && w.parent_id == Some(workspace.id)
                    })
                    .cloned()
                    .collect();
                let projects: Vec<ProjectNode> = sorted(members)
                    .iter()
                    .filter_map(|member| project_node(member, inherited))
                    .collect();
                let (direct, _) = sessions_of(&workspace, false);
                if searching && !inherited && projects.is_empty() && direct.is_empty() {
                    continue;
                }
                nodes.push(Node::Workspace(WorkspaceNode {
                    needs: aggregated.get(&workspace.id).copied().unwrap_or(0),
                    info: workspace,
                    projects,
                    sessions: direct,
                }));
            }
            let loose: Vec<ProjectNode> = sorted(
                workspaces
                    .iter()
                    .filter(|w| w.kind == WorkspaceKind::Project && !is_member(w))
                    .filter(|w| !hidden(w))
                    .cloned()
                    .collect(),
            )
            .iter()
            .filter_map(|project| project_node(project, false))
            .collect();
            if !user_workspaces.is_empty() && (!loose.is_empty() || !searching) {
                nodes.push(Node::NoWorkspace);
            }
            nodes.extend(loose.into_iter().map(Node::Project));
        }
        SidebarOrganize::Projects => {
            let all: Vec<WorkspaceInfo> = workspaces
                .iter()
                .filter(|w| w.kind == WorkspaceKind::Project)
                .filter(|w| !hidden(w))
                .cloned()
                .collect();
            for project in sorted(all) {
                if let Some(node) = project_node(&project, parent_matches(&project)) {
                    nodes.push(Node::Project(node));
                }
            }
            // A session started straight into a workspace has no project to sit under.
            for workspace in order::sort(&user_workspaces, inputs.sort, &aggregated) {
                let (direct, _) = sessions_of(workspace, false);
                if !direct.is_empty() {
                    nodes.push(Node::Workspace(WorkspaceNode {
                        info: workspace.clone(),
                        projects: Vec::new(),
                        sessions: direct,
                        needs: inputs.needs.get(&workspace.id).copied().unwrap_or(0),
                    }));
                }
            }
        }
    }
    if let Some(home) = workspaces.iter().find(|w| w.kind == WorkspaceKind::Home)
        && let Some(node) = project_node(home, false)
    {
        nodes.push(Node::Home(node));
    }
    Tree { nodes }
}

/// `proj/hutan` for `/Users/me/proj/hutan`: the last two parts of a path (`~` for the home
/// folder).
pub fn folder_label(path: &Path) -> String {
    if std::env::home_dir().is_some_and(|home| home == path) {
        return "~".to_owned();
    }
    let parts: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [] => path.display().to_string(),
        [only] => only.clone(),
        [.., parent, last] => format!("{parent}/{last}"),
    }
}

/// Where a session is now: its live folder (`SessionInfo.folder`, the shell's OSC 7 directory),
/// else the folder it started in.
pub fn session_folder(session: &SessionInfo) -> Option<&Path> {
    session.folder.as_deref().or(session.cwd.as_deref())
}

/// The second line of a session row. A row is one line (state lane, kind icon, title, age); this
/// line exists only when it says something the line above and the parent project row do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitle {
    /// The container the session sits in: only where the row is away from its parent project
    /// row (Needs you, a flat list).
    pub container: Option<String>,
    /// The live folder's last two parts, only when the session has left the checkout it belongs
    /// to (its placement's root, else its project's).
    pub folder: Option<String>,
    /// The placement badge, only when the session is not in the main checkout (or shares it).
    pub badge: Option<PlacementBadge>,
}

/// What [`subtitle`] decides from.
#[derive(Debug, Clone, Copy)]
pub struct SubtitleInputs<'a> {
    /// The row is not under its project row (the Needs you group, a flat list), so the project
    /// is part of what it says.
    pub away_from_parent: bool,
    /// The name of the container the session sits in.
    pub container_name: Option<&'a str>,
    /// The root folder of the project the session sits in.
    pub project_root: Option<&'a Path>,
    /// `panels.sessions.row` lists `folder`.
    pub show_folder: bool,
    /// `panels.sessions.row` lists `placement`.
    pub show_placement: bool,
}

/// The second line of `session`'s row, `None` for a one-line row.
pub fn subtitle(session: &SessionInfo, inputs: SubtitleInputs<'_>) -> Option<Subtitle> {
    let badge = inputs
        .show_placement
        .then(|| row_placement_badge(session))
        .flatten();
    let folder = inputs
        .show_folder
        .then(|| {
            let live = session_folder(session)?;
            let home = session
                .placement
                .as_ref()
                .map(|p| p.root.as_path())
                .or(inputs.project_root)?;
            (!live.starts_with(home)).then(|| folder_label(live))
        })
        .flatten();
    let container = inputs
        .away_from_parent
        .then(|| inputs.container_name.map(str::to_owned))
        .flatten();
    (badge.is_some() || folder.is_some() || container.is_some()).then_some(Subtitle {
        container,
        folder,
        badge,
    })
}

#[cfg(test)]
mod tests {
    use hutan_protocol::messages::CreatedBy;

    use super::*;

    fn container(name: &str, kind: WorkspaceKind, position: i64) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.to_owned(),
            roots: vec![format!("/work/{name}").into()],
            kind,
            pinned: false,
            position,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 1,
            total_sessions: 1,
            parent_id: None,
            project: None,
        }
    }

    fn project(name: &str, position: i64) -> WorkspaceInfo {
        container(name, WorkspaceKind::Project, position)
    }

    fn member(name: &str, position: i64, of: &WorkspaceInfo) -> WorkspaceInfo {
        WorkspaceInfo {
            parent_id: Some(of.id),
            ..project(name, position)
        }
    }

    fn session(title: &str, of: &WorkspaceInfo) -> SessionInfo {
        SessionInfo {
            id: hutan_core::SessionId::new(),
            workspace_id: of.id,
            kind: hutan_core::SessionKind::Terminal,
            title: title.to_owned(),
            state: hutan_core::SessionState::Running,
            command: vec!["sh".into()],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    /// `(label, depth)` of every row, with `session:` for sessions.
    fn shape(tree: &Tree) -> Vec<String> {
        let mut out = Vec::new();
        let project = |out: &mut Vec<String>, p: &ProjectNode, depth: usize| {
            out.push(format!("{}{}", "  ".repeat(depth), p.info.name));
            for s in &p.sessions {
                out.push(format!("{}· {}", "  ".repeat(depth + 1), s.title));
            }
        };
        for node in &tree.nodes {
            match node {
                Node::Workspace(w) => {
                    out.push(format!("[{}]", w.info.name));
                    for p in &w.projects {
                        project(&mut out, p, 1);
                    }
                    for s in &w.sessions {
                        out.push(format!("  · {}", s.title));
                    }
                }
                Node::Project(p) | Node::Home(p) => project(&mut out, p, 0),
                Node::NoWorkspace => out.push("-- no workspace --".to_owned()),
            }
        }
        out
    }

    struct World {
        all: Vec<WorkspaceInfo>,
        sessions: Vec<SessionInfo>,
    }

    impl World {
        fn tree(&self, organize: SidebarOrganize, query: &str) -> Tree {
            self.tree_with(
                organize,
                WorkspaceSort::Manual,
                &WorkspacesSettings::default(),
                query,
            )
        }

        fn tree_with(
            &self,
            organize: SidebarOrganize,
            sort: WorkspaceSort,
            filters: &WorkspacesSettings,
            query: &str,
        ) -> Tree {
            let refs: Vec<&SessionInfo> = self.sessions.iter().collect();
            let needs = HashMap::new();
            let by_title = |s: &SessionInfo| {
                let q = query.trim().to_lowercase();
                q.split_whitespace()
                    .all(|w| s.title.to_lowercase().contains(w))
            };
            build(&Inputs {
                workspaces: &self.all,
                sessions: &refs,
                organize,
                sort,
                filters,
                query,
                active: [None, None],
                needs: &needs,
                session_matches: &by_title,
            })
        }
    }

    /// shop = {api, web}, lab = {}, loose projects `solo` and `idle` (empty), Home.
    fn world() -> World {
        let shop = container("shop", WorkspaceKind::Workspace, 0);
        let lab = container("lab", WorkspaceKind::Workspace, 1);
        let api = member("api", 2, &shop);
        let web = member("web", 3, &shop);
        let solo = project("solo", 4);
        let mut idle = project("idle", 5);
        idle.total_sessions = 0;
        idle.live_sessions = 0;
        let home = container("Home", WorkspaceKind::Home, 6);
        let sessions = vec![
            session("build", &api),
            session("deploy", &api),
            session("lint", &web),
            session("notes", &solo),
            session("scratch", &home),
        ];
        World {
            all: vec![shop, lab, api, web, solo, idle, home],
            sessions,
        }
    }

    #[test]
    fn workspaces_mode_lists_workspaces_their_projects_loose_projects_and_home() {
        let tree = world().tree(SidebarOrganize::Workspaces, "");
        assert_eq!(
            shape(&tree),
            [
                "[shop]",
                "  api",
                "    · build",
                "    · deploy",
                "  web",
                "    · lint",
                "[lab]",
                "-- no workspace --",
                "solo",
                "  · notes",
                "Home",
                "  · scratch",
            ],
            "empty loose projects are hidden, empty workspaces are not"
        );
    }

    #[test]
    fn projects_mode_is_a_flat_list_with_home_last() {
        let tree = world().tree(SidebarOrganize::Projects, "");
        assert_eq!(
            shape(&tree),
            [
                "api",
                "  · build",
                "  · deploy",
                "web",
                "  · lint",
                "solo",
                "  · notes",
                "Home",
                "  · scratch",
            ]
        );
    }

    #[test]
    fn without_workspaces_there_is_no_no_workspace_label() {
        let mut w = world();
        w.all.retain(|c| c.kind != WorkspaceKind::Workspace);
        for c in &mut w.all {
            c.parent_id = None;
        }
        let tree = w.tree(SidebarOrganize::Workspaces, "");
        assert!(!shape(&tree).contains(&"-- no workspace --".to_owned()));
        assert_eq!(shape(&tree)[0], "api");
    }

    #[test]
    fn pinned_projects_lead_and_sorting_applies_within_each_level() {
        let mut w = world();
        let web = w.all.iter_mut().find(|c| c.name == "web").unwrap();
        web.pinned = true;
        web.last_active_ms = 1;
        let by_name = |w: &World, organize| {
            shape(&w.tree_with(
                organize,
                WorkspaceSort::Name,
                &WorkspacesSettings::default(),
                "",
            ))
        };
        let tree = by_name(&w, SidebarOrganize::Workspaces);
        // workspaces by name (lab, shop), the pinned member first within shop.
        assert_eq!(
            tree[..5],
            ["[lab]", "[shop]", "  web", "    · lint", "  api"]
        );
        let flat = by_name(&w, SidebarOrganize::Projects);
        assert_eq!(flat[0], "web", "pinned first in the flat list");
    }

    #[test]
    fn filters_hide_loose_projects_only_and_never_pinned_or_active_ones() {
        let mut w = world();
        let strict = WorkspacesSettings {
            hide_empty: true,
            ..WorkspacesSettings::default()
        };
        let list = shape(&w.tree_with(
            SidebarOrganize::Workspaces,
            WorkspaceSort::Manual,
            &strict,
            "",
        ));
        assert!(!list.contains(&"idle".to_owned()));
        assert!(list.contains(&"[lab]".to_owned()), "workspaces always show");
        // Pinned: shown despite being empty.
        w.all.iter_mut().find(|c| c.name == "idle").unwrap().pinned = true;
        let list = shape(&w.tree_with(
            SidebarOrganize::Workspaces,
            WorkspaceSort::Manual,
            &strict,
            "",
        ));
        assert!(list.contains(&"idle".to_owned()));
        // The active project of a hidden kind stays.
        w.all.iter_mut().find(|c| c.name == "idle").unwrap().pinned = false;
        let idle = w.all.iter().find(|c| c.name == "idle").unwrap().id;
        let refs: Vec<&SessionInfo> = w.sessions.iter().collect();
        let tree = build(&Inputs {
            workspaces: &w.all,
            sessions: &refs,
            organize: SidebarOrganize::Workspaces,
            sort: WorkspaceSort::Manual,
            filters: &strict,
            query: "",
            active: [None, Some(idle)],
            needs: &HashMap::new(),
            session_matches: &|_| true,
        });
        assert!(shape(&tree).contains(&"idle".to_owned()));
    }

    #[test]
    fn the_search_matches_workspaces_projects_and_sessions() {
        let w = world();
        // A session: only the chain to it is listed.
        assert_eq!(
            shape(&w.tree(SidebarOrganize::Workspaces, "lint")),
            ["[shop]", "  web", "    · lint"]
        );
        // A project: all its sessions.
        assert_eq!(
            shape(&w.tree(SidebarOrganize::Workspaces, "api")),
            ["[shop]", "  api", "    · build", "    · deploy"]
        );
        // A workspace: everything in it.
        assert_eq!(
            shape(&w.tree(SidebarOrganize::Workspaces, "shop")),
            [
                "[shop]",
                "  api",
                "    · build",
                "    · deploy",
                "  web",
                "    · lint"
            ]
        );
        // The flat list finds a project through its workspace's name too.
        assert_eq!(
            shape(&w.tree(SidebarOrganize::Projects, "shop")),
            ["api", "  · build", "  · deploy", "web", "  · lint"]
        );
        // A folder.
        assert_eq!(
            shape(&w.tree(SidebarOrganize::Workspaces, "/work/solo")),
            ["-- no workspace --", "solo", "  · notes"]
        );
        assert!(w.tree(SidebarOrganize::Workspaces, "zzz").nodes.is_empty());
    }

    #[test]
    fn a_session_started_into_a_workspace_is_listed_under_it_or_flat() {
        let mut w = world();
        let lab = w.all.iter().find(|c| c.name == "lab").unwrap().clone();
        w.sessions.push(session("direct", &lab));
        let list = shape(&w.tree(SidebarOrganize::Workspaces, ""));
        let at = list.iter().position(|l| l == "[lab]").unwrap();
        assert_eq!(list[at + 1], "  · direct");
        let flat = shape(&w.tree(SidebarOrganize::Projects, ""));
        assert!(flat.contains(&"[lab]".to_owned()));
    }

    #[test]
    fn needs_you_counts_roll_up_to_the_workspace() {
        let w = world();
        let api = w.all.iter().find(|c| c.name == "api").unwrap().id;
        let web = w.all.iter().find(|c| c.name == "web").unwrap().id;
        let refs: Vec<&SessionInfo> = w.sessions.iter().collect();
        let needs = HashMap::from([(api, 2), (web, 1)]);
        let tree = build(&Inputs {
            workspaces: &w.all,
            sessions: &refs,
            organize: SidebarOrganize::Workspaces,
            sort: WorkspaceSort::Manual,
            filters: &WorkspacesSettings::default(),
            query: "",
            active: [None, None],
            needs: &needs,
            session_matches: &|_| true,
        });
        let Some(Node::Workspace(shop)) = tree.nodes.first() else {
            panic!("the first node is the workspace");
        };
        assert_eq!(shop.needs, 3);
        assert_eq!(shop.projects[0].needs, 2);
        assert_eq!(shop.projects[1].needs, 1);
    }

    #[test]
    fn attention_sort_ranks_workspaces_by_what_their_projects_need() {
        let mut w = world();
        let lab = w.all.iter().find(|c| c.name == "lab").unwrap().id;
        // lab has no needs; shop's projects do: shop first (it is first by position too), so
        // pin down the reverse order by giving lab the attention.
        let web = w.all.iter().find(|c| c.name == "web").unwrap().id;
        let member_of_lab = w.all.iter_mut().find(|c| c.name == "web").unwrap();
        member_of_lab.parent_id = Some(lab);
        let refs: Vec<&SessionInfo> = w.sessions.iter().collect();
        let needs = HashMap::from([(web, 1)]);
        let tree = build(&Inputs {
            workspaces: &w.all,
            sessions: &refs,
            organize: SidebarOrganize::Workspaces,
            sort: WorkspaceSort::Attention,
            filters: &WorkspacesSettings::default(),
            query: "",
            active: [None, None],
            needs: &needs,
            session_matches: &|_| true,
        });
        let Some(Node::Workspace(first)) = tree.nodes.first() else {
            panic!("a workspace leads");
        };
        assert_eq!(first.info.name, "lab");
    }

    #[test]
    fn folders_show_their_last_two_parts() {
        assert_eq!(
            folder_label(Path::new("/Users/me/proj/hutan")),
            "proj/hutan"
        );
        assert_eq!(folder_label(Path::new("/tmp")), "tmp");
        assert_eq!(folder_label(Path::new("/a/b/c/")), "b/c");
        assert_eq!(folder_label(Path::new("/")), "/");
        if let Some(home) = std::env::home_dir() {
            assert_eq!(folder_label(&home), "~");
        }
    }

    fn placed(
        mut info: SessionInfo,
        kind: hutan_protocol::worktree::PlacementKind,
        root: &str,
        shared: usize,
    ) -> SessionInfo {
        info.placement = Some(hutan_protocol::worktree::Placement {
            kind,
            root: root.into(),
            branch: Some("topic".to_owned()),
            shared_with: (0..shared).map(|_| hutan_core::SessionId::new()).collect(),
        });
        info
    }

    fn inputs<'a>(away: bool, root: Option<&'a Path>) -> SubtitleInputs<'a> {
        SubtitleInputs {
            away_from_parent: away,
            container_name: Some("api"),
            project_root: root,
            show_folder: true,
            show_placement: true,
        }
    }

    #[test]
    fn a_session_in_the_main_checkout_under_its_project_is_one_line() {
        use hutan_protocol::worktree::PlacementKind;
        let api = project("api", 0);
        let mut main = placed(session("s", &api), PlacementKind::Main, "/work/api", 0);
        main.folder = Some("/work/api/src".into());
        assert_eq!(
            subtitle(&main, inputs(false, Some(Path::new("/work/api")))),
            None
        );
        assert_eq!(subtitle(&session("plain", &api), inputs(false, None)), None);
    }

    #[test]
    fn a_worktree_session_adds_its_badge_and_nothing_else() {
        use hutan_protocol::worktree::PlacementKind;
        let api = project("api", 0);
        let mut wt = placed(session("s", &api), PlacementKind::Worktree, "/wt/topic", 0);
        wt.folder = Some("/wt/topic/src".into());
        let line = subtitle(&wt, inputs(false, Some(Path::new("/work/api")))).unwrap();
        assert_eq!(
            line.badge.as_ref().map(|b| b.text.as_str()),
            Some("wt topic")
        );
        assert_eq!((line.container, line.folder), (None, None));
    }

    #[test]
    fn a_shared_main_checkout_keeps_its_warning_on_the_second_line() {
        use hutan_protocol::worktree::PlacementKind;
        let api = project("api", 0);
        let shared = placed(session("s", &api), PlacementKind::Main, "/work/api", 2);
        let badge = subtitle(&shared, inputs(false, None))
            .unwrap()
            .badge
            .unwrap();
        assert!(badge.shared);
    }

    #[test]
    fn a_session_that_left_its_checkout_names_the_folder() {
        use hutan_protocol::worktree::PlacementKind;
        let api = project("api", 0);
        let mut away = placed(session("s", &api), PlacementKind::Main, "/work/api", 0);
        away.folder = Some("/tmp/scratch".into());
        let line = subtitle(&away, inputs(false, None)).unwrap();
        assert_eq!(line.folder.as_deref(), Some("tmp/scratch"));
        // Without a checkout, the project's root is where it belongs.
        let mut bare = session("b", &api);
        bare.cwd = Some("/work/api/tools".into());
        assert_eq!(
            subtitle(&bare, inputs(false, Some(Path::new("/work/api")))),
            None
        );
        bare.cwd = Some("/elsewhere/x".into());
        assert_eq!(
            subtitle(&bare, inputs(false, Some(Path::new("/work/api"))))
                .unwrap()
                .folder
                .as_deref(),
            Some("elsewhere/x")
        );
    }

    #[test]
    fn a_row_away_from_its_project_names_the_project() {
        let api = project("api", 0);
        let line = subtitle(&session("s", &api), inputs(true, None)).unwrap();
        assert_eq!(line.container.as_deref(), Some("api"));
        assert_eq!((line.folder, line.badge), (None, None));
    }

    #[test]
    fn the_row_fields_the_user_dropped_stay_dropped() {
        use hutan_protocol::worktree::PlacementKind;
        let api = project("api", 0);
        let mut wt = placed(session("s", &api), PlacementKind::Worktree, "/wt/topic", 0);
        wt.folder = Some("/tmp/x".into());
        let quiet = SubtitleInputs {
            show_folder: false,
            show_placement: false,
            ..inputs(false, None)
        };
        assert_eq!(subtitle(&wt, quiet), None);
    }

    #[test]
    fn no_workspace_means_no_workspace_header_and_no_label() {
        let solo = project("solo", 0);
        let home = container("Home", WorkspaceKind::Home, 1);
        let world = World {
            sessions: vec![session("a", &solo), session("b", &home)],
            all: vec![solo, home],
        };
        for organize in [SidebarOrganize::Workspaces, SidebarOrganize::Projects] {
            let tree = world.tree(organize, "");
            assert!(
                tree.nodes
                    .iter()
                    .all(|n| matches!(n, Node::Project(_) | Node::Home(_))),
                "{organize:?}: only projects and Home"
            );
        }
    }
}
