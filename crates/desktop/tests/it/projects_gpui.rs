#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests against an in-process daemon for the sidebar of workspaces, projects and sessions
//! (M18-B): both organize modes, session rows with folder and placement badge, managing
//! workspaces (new, open a folder into one, drag a project in and out, new from a selection,
//! inline rename, delete keeping the projects), one layout per container, the project settings
//! layer, ⌘J. Real git repositories in temporary directories.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext,
    point, px,
};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::sessions::tree::folder_label;
use hutan_desktop::settings::{SidebarOrganize, SidebarSettings};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::panels::{RowKind, SessionsPanel, SidebarRow};
use hutan_desktop::workbench::{BadgeSite, Region, Workbench};
use hutan_protocol::messages::{
    Empty, Request, SessionCreate, SessionInfo, SessionResult, SessionSpec, TerminalSpec,
    WorkspaceCreate, WorkspaceInfo, WorkspaceKind, WorkspaceList, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::project::WorkspaceAssign;
use hutan_settings::{SettingsPaths, SettingsStore};
use tempfile::TempDir;

// ---- fixtures -------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Folders named after their projects: repositories `api` (with a worktree `api-wt/topic`) and
/// `web`, plain folders `notes` and `docs`.
struct Folders {
    _dir: TempDir,
    api: PathBuf,
    api_wt: PathBuf,
    web: PathBuf,
    notes: PathBuf,
    docs: PathBuf,
}

impl Folders {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let repo = |name: &str| {
            let root = base.join(name);
            fs::create_dir(&root).unwrap();
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
            root
        };
        let api = repo("api");
        let web = repo("web");
        let api_wt = base.join("api-wt").join("topic");
        git(
            &api,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "topic",
                api_wt.to_str().unwrap(),
            ],
        );
        let plain = |name: &str| {
            let root = base.join(name);
            fs::create_dir(&root).unwrap();
            root
        };
        let (notes, docs) = (plain("notes"), plain("docs"));
        Self {
            _dir: dir,
            api,
            api_wt,
            web,
            notes,
            docs,
        }
    }

    /// Another plain folder.
    fn extra(&self, name: &str) -> PathBuf {
        let root = self._dir.path().canonicalize().unwrap().join(name);
        fs::create_dir(&root).unwrap();
        root
    }
}

fn open_project(daemon: &TestDaemon, path: &Path) -> WorkspaceInfo {
    let opened: WorkspaceOpened = daemon.call(Request::WorkspaceOpen(WorkspaceOpen {
        path: path.to_owned(),
        created_by: None,
    }));
    opened.workspace
}

fn create_workspace(daemon: &TestDaemon, name: &str, projects: &[&WorkspaceInfo]) -> WorkspaceInfo {
    daemon.call(Request::WorkspaceCreate(WorkspaceCreate {
        name: name.to_owned(),
        paths: Vec::new(),
        project_ids: projects.iter().map(|p| p.id).collect(),
        created_by: None,
    }))
}

fn assign(daemon: &TestDaemon, project: &WorkspaceInfo, workspace: Option<&WorkspaceInfo>) {
    let _: WorkspaceInfo = daemon.call(Request::WorkspaceAssign(WorkspaceAssign {
        project_id: project.id,
        workspace_id: workspace.map(|w| w.id),
    }));
}

fn all(daemon: &TestDaemon) -> Vec<WorkspaceInfo> {
    let list: WorkspaceList = daemon.call(Request::WorkspaceList(Empty {}));
    list.workspaces
}

fn find(daemon: &TestDaemon, name: &str) -> Option<WorkspaceInfo> {
    all(daemon).into_iter().find(|w| w.name == name)
}

fn by_name(daemon: &TestDaemon, name: &str) -> WorkspaceInfo {
    find(daemon, name).unwrap_or_else(|| panic!("no workspace named {name}"))
}

/// A sleeping terminal named `title` that starts in `dir`; the daemon puts it into the project of
/// the folder.
fn session_at(daemon: &TestDaemon, title: &str, dir: &Path) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), "sleep 600".into()],
            cwd: Some(dir.to_owned()),
            title: Some(title.to_owned()),
            ..TerminalSpec::default()
        }),
    }));
    // Session creation order is the order of the rows.
    std::thread::sleep(Duration::from_millis(15));
    created.session
}

// ---- the desktop ----------------------------------------------------------------------------

/// A desktop that lists every container's sessions (`sidebar.show_all_sessions`), as the tree
/// was before SB-ATTACH, unless the test chose its own settings file.
fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = daemon.home().join("settings.toml");
    if !settings.exists() {
        fs::write(&settings, "[sidebar]\nshow_all_sessions = true\n").unwrap();
    }
    open_attached(cx, daemon, state_path)
}

/// A desktop with the default sidebar: only the container you work in is expanded.
fn open_attached<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        state_path,
    )
}

/// Waits until the desktop knows `workspaces` containers (Home included) and `sessions`
/// sessions, and has resolved the active container.
fn wait_synced(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    workspaces: usize,
    sessions: usize,
) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced()
                && store.workspaces().len() == workspaces
                && store.sessions().len() == sessions
        })
    });
    wait_until(cx, "the active container", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace().is_some())
    });
}

fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<SessionsPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock")
}

fn rows(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext) -> Vec<SidebarRow> {
    panel.read_with(cx, |p, cx| p.visible_rows(cx))
}

/// The tree as text: `W` workspace, `P` project, `H` Home, `S` session, `#` section, two spaces
/// per level.
fn outline(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext) -> Vec<String> {
    rows(panel, cx)
        .iter()
        .map(|row| {
            let tag = match row.kind {
                RowKind::Section => "#",
                RowKind::Workspace => "W",
                RowKind::Project => "P",
                RowKind::Home => "H",
                RowKind::NoWorkspace => "-",
                RowKind::Session => "S",
                RowKind::Worktree => "T",
            };
            format!("{}{tag} {}", "  ".repeat(row.depth), row.title)
        })
        .collect()
}

fn row<'a>(rows: &'a [SidebarRow], title: &str) -> &'a SidebarRow {
    rows.iter()
        .find(|r| r.title == title)
        .unwrap_or_else(|| panic!("no row `{title}` in {rows:?}"))
}

fn active(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
    view.read_with(cx, |wb, _| wb.active_workspace())
}

fn active_project(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<WorkspaceId> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).active_project_id())
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn pump(cx: &mut VisualTestContext, span: Duration) {
    let until = std::time::Instant::now() + span;
    while std::time::Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn center_of(b: Bounds<Pixels>) -> Point<Pixels> {
    point(
        b.origin.x + b.size.width / 2.0,
        b.origin.y + b.size.height / 2.0,
    )
}

/// Press at `from`, move in small steps with the button held, release at `to`.
fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
}

fn center_of_region(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> Point<Pixels> {
    draw(cx);
    center_of(
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .unwrap_or_else(|| panic!("{region:?} is drawn")),
    )
}

fn row_center(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    container: WorkspaceId,
) -> Point<Pixels> {
    center_of_region(view, cx, Region::WorkspaceRow(container))
}

fn menu_labels(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label_prefix: &str) {
    let index = menu_labels(view, cx)
        .iter()
        .position(|l| l.starts_with(label_prefix))
        .unwrap_or_else(|| panic!("no menu entry `{label_prefix}`"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

fn daemon_parent(daemon: &TestDaemon, project: &WorkspaceInfo) -> Option<WorkspaceId> {
    all(daemon)
        .into_iter()
        .find(|w| w.id == project.id)
        .and_then(|w| w.parent_id)
}

/// A daemon with the containers most tests start from: `shop` = {`api`, `web`}, an empty `lab`,
/// the loose projects `notes` and `docs`, and one session in each of `api` (in the main checkout
/// and in the worktree), `web` and `notes`.
struct World {
    daemon: TestDaemon,
    folders: Folders,
    api: WorkspaceInfo,
    web: WorkspaceInfo,
    notes: WorkspaceInfo,
    docs: WorkspaceInfo,
    shop: WorkspaceInfo,
    lab: WorkspaceInfo,
    s_main: SessionInfo,
    s_wt: SessionInfo,
    s_web: SessionInfo,
    s_notes: SessionInfo,
}

impl World {
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let folders = Folders::new();
        let api = open_project(&daemon, &folders.api);
        let web = open_project(&daemon, &folders.web);
        let notes = open_project(&daemon, &folders.notes);
        let docs = open_project(&daemon, &folders.docs);
        let shop = create_workspace(&daemon, "shop", &[&api, &web]);
        let lab = create_workspace(&daemon, "lab", &[]);
        let s_main = session_at(&daemon, "s-main", &folders.api);
        let s_wt = session_at(&daemon, "s-wt", &folders.api_wt);
        let s_web = session_at(&daemon, "s-web", &folders.web);
        let s_notes = session_at(&daemon, "s-notes", &folders.notes);
        assert_eq!(
            s_wt.workspace_id, api.id,
            "a worktree belongs to its repository's project"
        );
        Self {
            daemon,
            folders,
            api,
            web,
            notes,
            docs,
            shop,
            lab,
            s_main,
            s_wt,
            s_web,
            s_notes,
        }
    }

    /// Home, the four projects, the two workspaces; the four sessions.
    fn synced<'a>(
        &self,
        cx: &'a mut TestAppContext,
        state_path: Option<PathBuf>,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        self.synced_with(cx, state_path, true)
    }

    /// [`synced`](Self::synced) with the default sidebar: only the active container expanded.
    fn synced_attached<'a>(
        &self,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        self.synced_with(cx, None, false)
    }

    fn synced_with<'a>(
        &self,
        cx: &'a mut TestAppContext,
        state_path: Option<PathBuf>,
        show_all: bool,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        let (view, cx) = if show_all {
            open(cx, &self.daemon, state_path)
        } else {
            open_attached(cx, &self.daemon, state_path)
        };
        wait_synced(&view, cx, 7, 4);
        // The placements arrive with the events (a plain folder has none).
        let git_sessions = [self.s_main.id, self.s_wt.id, self.s_web.id];
        wait_until(cx, "every placement", |cx| {
            view.read_with(cx, |wb, cx| {
                let store = wb.sessions().read(cx);
                git_sessions
                    .iter()
                    .all(|id| store.session(*id).is_some_and(|s| s.placement.is_some()))
            })
        });
        (view, cx)
    }
}

// ---- the tree ---------------------------------------------------------------------------------

#[gpui::test]
fn both_organize_modes_render_the_right_tree(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);

    // Workspaces first (their projects below), then "No workspace" with the loose projects that
    // have sessions (`docs` is empty and hidden), then Home.
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<SidebarSettings>()
            .organize),
        SidebarOrganize::Workspaces,
        "the default"
    );
    assert_eq!(
        outline(&panel, cx),
        [
            "W shop",
            "  P api",
            "    S s-main",
            "    T topic",
            "      S s-wt",
            "  P web",
            "    S s-web",
            "W lab",
            "- No workspace",
            "P notes",
            "  S s-notes",
            "H Home",
        ]
    );
    let listed = rows(&panel, cx);
    assert_eq!(row(&listed, "shop").count.as_deref(), Some("3"));
    assert_eq!(row(&listed, "api").count.as_deref(), Some("2"));
    assert_eq!(
        row(&listed, "lab").count,
        None,
        "an empty workspace has no counter"
    );
    assert_eq!(
        row(&listed, "api").folder,
        Some(folder_label(&w.folders.api)),
        "a project row names its folder"
    );
    assert!(row(&listed, "api").needs == 0);

    // The flat list: projects (the daemon's order), Home last.
    cx.dispatch_action(actions::SetSidebarOrganize(SidebarOrganize::Projects));
    assert_eq!(
        outline(&panel, cx),
        [
            "P api",
            "  S s-main",
            "  T topic",
            "    S s-wt",
            "P web",
            "  S s-web",
            "P notes",
            "  S s-notes",
            "H Home",
        ]
    );
    let text = std::fs::read_to_string(w.daemon.home().join("settings.toml")).unwrap();
    assert!(text.contains("organize = \"projects\""), "{text}");
    // The sort menu offers both, the current one checked.
    cx.dispatch_action(actions::ShowSortMenu(point(px(60.0), px(60.0))));
    let menu = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .unwrap();
    assert!(
        menu.iter()
            .any(|(label, on)| *on && label.as_ref() == "Organize: Projects"),
        "{menu:?}"
    );
    cx.dispatch_action(actions::SetSidebarOrganize(SidebarOrganize::Workspaces));
    assert_eq!(outline(&panel, cx)[0], "W shop");

    // Pins lead within their level; a pinned empty project shows too.
    let _: WorkspaceInfo = w.daemon.call(Request::WorkspacePin(
        hutan_protocol::messages::WorkspacePin {
            workspace_id: w.docs.id,
            pinned: true,
        },
    ));
    let _: WorkspaceInfo = w.daemon.call(Request::WorkspacePin(
        hutan_protocol::messages::WorkspacePin {
            workspace_id: w.web.id,
            pinned: true,
        },
    ));
    wait_until(cx, "the pins", |cx| {
        let listed = rows(&panel, cx);
        listed.iter().any(|r| r.title == "docs")
            && listed
                .iter()
                .find(|r| r.title == "web")
                .is_some_and(|r| r.pinned)
    });
    let list = outline(&panel, cx);
    let at = |name: &str| list.iter().position(|l| l.trim() == name).unwrap();
    assert!(
        at("P web") < at("P api"),
        "a pinned member leads its workspace: {list:?}"
    );
    assert!(
        at("P docs") < at("P notes"),
        "a pinned loose project leads: {list:?}"
    );
}

/// `sidebar.organize` is a setting of the Projects Sidebar page, with its own wording, and
/// changing it there changes the sidebar and `settings.toml` like the sidebar's own menu does.
#[gpui::test]
fn sidebar_organize_is_on_the_projects_sidebar_page_and_changes_the_sidebar(
    cx: &mut TestAppContext,
) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);
    assert_eq!(outline(&panel, cx)[0], "W shop");

    // The page of the sidebar shows both namespaces; `sidebar` opens it too.
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("sidebar"), window, cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("panels.sessions".to_owned())
    );
    let fields: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.key_path())
            .collect()
    });
    for expected in [
        "panels.sessions.group_by",
        "panels.sessions.row",
        "sidebar.organize",
    ] {
        assert!(
            fields.iter().any(|f| f == expected),
            "{expected} in {fields:?}"
        );
    }
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("organize")),
        Some(serde_json::json!("workspaces"))
    );
    assert_eq!(
        hutan_desktop::workbench::settings_labels::field_title("sidebar", "organize"),
        "Organize the sidebar"
    );
    draw(cx);

    view.update(cx, |wb, cx| wb.sheet_choose("organize", "projects", cx));
    wait_until(cx, "the flat list", |cx| outline(&panel, cx)[0] == "P api");
    let text = fs::read_to_string(w.daemon.home().join("settings.toml")).unwrap();
    assert!(text.contains("organize = \"projects\""), "{text}");
    assert!(!outline(&panel, cx).iter().any(|l| l.starts_with("W ")));

    // Reset removes the key: the default (workspaces first) is back.
    view.update(cx, |wb, cx| wb.sheet_reset("organize", cx));
    wait_until(cx, "the workspaces", |cx| {
        outline(&panel, cx)[0] == "W shop"
    });
    let text = fs::read_to_string(w.daemon.home().join("settings.toml")).unwrap();
    assert!(!text.contains("organize"), "{text}");
}

#[gpui::test]
fn session_rows_are_one_line_and_only_a_checkout_that_is_news_adds_a_second(
    cx: &mut TestAppContext,
) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);
    let listed = rows(&panel, cx);

    // The main checkout is where a project's sessions are: nothing to say.
    let main = row(&listed, "s-main");
    assert_eq!(
        (main.folder.as_deref(), main.badge.as_deref()),
        (None, None)
    );
    assert!(main.subtitle.is_none(), "one line");
    assert!(!main.shared);
    // A linked worktree is news: its badge, and only that (its folder is in the tooltip).
    let wt = row(&listed, "s-wt");
    assert_eq!(wt.badge.as_deref(), Some("wt topic"));
    assert_eq!(
        wt.folder, None,
        "the folder is inside the checkout it belongs to"
    );
    assert!(wt.subtitle.is_some());
    // A folder outside git has no placement and sits in its project's folder.
    let notes = row(&listed, "s-notes");
    assert!(notes.subtitle.is_none() && notes.badge.is_none());
    // Only the worktree draws a badge.
    wait_until(cx, "the worktree's badge", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::PlacementBadge(BadgeSite::Row, w.s_wt.id))
                .is_some()
        })
    });
    for id in [w.s_main.id, w.s_notes.id] {
        assert!(view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::PlacementBadge(BadgeSite::Row, id))
                .is_none()
        }));
    }

    // Two agents in one checkout: the badge turns into a warning.
    let omp = Reporter::new(w.s_main.id, "omp-extension", "omp");
    let claude = Reporter::new(w.s_main.id, "claude-hooks", "claude");
    let other = session_at(&w.daemon, "s-other", &w.folders.api);
    let second = Reporter::new(other.id, "claude-hooks", "claude");
    omp.report(&w.daemon, Node::main("root").working().name("omp"));
    second.report(&w.daemon, Node::main("root").working().name("claude"));
    let _ = claude;
    wait_until(cx, "the warning", |cx| {
        rows(&panel, cx)
            .iter()
            .filter(|r| r.kind == RowKind::Session && r.title.starts_with("s-"))
            .any(|r| r.title == "s-main" && r.shared)
    });
    let listed = rows(&panel, cx);
    assert!(row(&listed, "s-other").shared);
    // Sharing is a 16 px warning glyph in the row's trailing slot, not a pill on a second line:
    // the row keeps its one-line height.
    for title in ["s-main", "s-other"] {
        let shared = row(&listed, title);
        assert!(
            shared.badge.is_none() && shared.subtitle.is_none(),
            "{title} stays one line: {shared:?}"
        );
    }
    wait_until(cx, "the warning glyph", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::SharedCheckout(w.s_main.id))
                .is_some_and(|b| (f32::from(b.size.width) - 16.0).abs() <= 1.0)
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::SharedCheckout(w.s_wt.id))
            .is_none()),
        "a session alone in its worktree has no marker"
    );
    let one_line = view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::SessionRow(w.s_main.id))
            .map(|b| f32::from(b.size.height))
    });
    let plain = view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::SessionRow(w.s_notes.id))
            .map(|b| f32::from(b.size.height))
    });
    assert_eq!(one_line, plain, "a shared checkout does not grow its row");
    assert!(!row(&listed, "s-wt").shared, "another checkout");
}

#[gpui::test]
fn a_list_has_one_selected_fill_the_session_and_never_its_project_row_too(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);

    // Looking at a project (no session of it focused): the project row is the selection.
    cx.dispatch_action(actions::OpenWorkspace(w.api.id.to_string()));
    wait_until(cx, "the project to be active", |cx| {
        active_project(&view, cx) == Some(w.api.id)
    });
    let listed = rows(&panel, cx);
    assert!(row(&listed, "api").active);
    assert!(
        listed.iter().filter(|r| r.filled).count() <= 1,
        "{listed:?}"
    );

    // A session under it focused: it takes the fill, the project row keeps none.
    open_session(&view, cx, w.s_main.id);
    wait_until(cx, "the session's fill", |cx| {
        rows(&panel, cx)
            .iter()
            .any(|r| r.title == "s-main" && r.filled)
    });
    let listed = rows(&panel, cx);
    let api_row = row(&listed, "api");
    assert!(
        api_row.active && !api_row.filled,
        "the parent row of the selected session has no fill: {api_row:?}"
    );
    assert_eq!(listed.iter().filter(|r| r.filled).count(), 1, "{listed:?}");
}

#[gpui::test]
fn the_filter_matches_workspaces_projects_folders_and_sessions(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);

    // A session.
    panel.update(cx, |p, cx| p.set_query("s-web", cx));
    assert_eq!(outline(&panel, cx), ["W shop", "  P web", "    S s-web"]);
    // The folder a session is in now.
    panel.update(cx, |p, cx| p.set_query("topic", cx));
    assert_eq!(
        outline(&panel, cx),
        ["W shop", "  P api", "    T topic", "      S s-wt"],
        "the worktree's folder"
    );
    // A project lists its sessions.
    panel.update(cx, |p, cx| p.set_query("notes", cx));
    assert_eq!(
        outline(&panel, cx),
        ["- No workspace", "P notes", "  S s-notes"]
    );
    // A workspace lists everything in it.
    panel.update(cx, |p, cx| p.set_query("shop", cx));
    assert_eq!(
        outline(&panel, cx),
        [
            "W shop",
            "  P api",
            "    S s-main",
            "    T topic",
            "      S s-wt",
            "  P web",
            "    S s-web"
        ]
    );
    // A workspace without sessions is found by its name, too.
    panel.update(cx, |p, cx| p.set_query("lab", cx));
    assert_eq!(outline(&panel, cx), ["W lab"]);
    panel.update(cx, |p, cx| p.set_query("no such thing", cx));
    assert!(outline(&panel, cx).is_empty());
    panel.update(cx, |p, cx| p.set_query("", cx));
}

// ---- managing workspaces ----------------------------------------------------------------------

#[gpui::test]
fn dragging_a_project_into_a_workspace_and_out_again(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);

    // notes → lab: `workspace.assign`.
    let (from, to) = (
        row_center(&view, cx, w.notes.id),
        row_center(&view, cx, w.lab.id),
    );
    drag(cx, from, to);
    wait_until(cx, "notes to join lab", |cx| {
        outline(&panel, cx)
            .windows(2)
            .any(|p| p[0] == "W lab" && p[1] == "  P notes")
    });
    assert_eq!(daemon_parent(&w.daemon, &w.notes), Some(w.lab.id));
    assert_eq!(
        outline(&panel, cx),
        [
            "W shop",
            "  P api",
            "    S s-main",
            "    T topic",
            "      S s-wt",
            "  P web",
            "    S s-web",
            "W lab",
            "  P notes",
            "    S s-notes",
            "- No workspace",
            "H Home",
        ]
    );

    // Dragged onto a project of another workspace it moves there.
    let (from, to) = (
        row_center(&view, cx, w.notes.id),
        row_center(&view, cx, w.web.id),
    );
    drag(cx, from, to);
    wait_until(cx, "notes to join shop", |_| {
        daemon_parent(&w.daemon, &w.notes) == Some(w.shop.id)
    });
    wait_until(cx, "the sidebar", |cx| {
        outline(&panel, cx).iter().position(|l| l == "  P notes")
            > outline(&panel, cx).iter().position(|l| l == "W shop")
    });

    // And out again: onto the "No workspace" label.
    let (from, to) = (
        row_center(&view, cx, w.notes.id),
        center_of_region(&view, cx, Region::NoWorkspace),
    );
    drag(cx, from, to);
    wait_until(cx, "notes to leave", |_| {
        daemon_parent(&w.daemon, &w.notes).is_none()
    });
    wait_until(cx, "the sidebar", |cx| {
        let list = outline(&panel, cx);
        let at = list.iter().position(|l| l == "- No workspace");
        at.is_some_and(|at| list.get(at + 1).is_some_and(|l| l == "P notes"))
    });

    // A project dropped on the label while in none changes nothing.
    let before = outline(&panel, cx);
    let (from, to) = (
        row_center(&view, cx, w.notes.id),
        center_of_region(&view, cx, Region::NoWorkspace),
    );
    drag(cx, from, to);
    pump(cx, Duration::from_millis(300));
    assert_eq!(outline(&panel, cx), before);
}

#[gpui::test]
fn a_new_workspace_from_the_selected_projects(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);
    let selected = |cx: &mut VisualTestContext| panel.read_with(cx, |p, _| p.selected_projects());
    // `docs` is empty and hidden; make it visible.
    cx.dispatch_action(actions::ToggleHideEmptyWorkspaces);
    wait_until(cx, "docs to be listed", |cx| {
        outline(&panel, cx).iter().any(|l| l == "P docs")
    });

    // ⌘-click toggles (and does not open the project), ⇧-click selects the range from the
    // project clicked last.
    let shown = active(&view, cx);
    let notes_at = row_center(&view, cx, w.notes.id);
    cx.simulate_click(notes_at, Modifiers::command());
    assert_eq!(selected(cx), [w.notes.id]);
    assert_eq!(active(&view, cx), shown, "selecting does not switch");
    let docs_at = row_center(&view, cx, w.docs.id);
    cx.simulate_click(docs_at, Modifiers::command());
    assert_eq!(selected(cx).len(), 2);
    cx.simulate_click(docs_at, Modifiers::command());
    assert_eq!(selected(cx), [w.notes.id]);
    cx.simulate_click(notes_at, Modifiers::command());
    assert!(selected(cx).is_empty());
    cx.simulate_click(notes_at, Modifiers::command());
    cx.simulate_click(docs_at, Modifiers::shift());
    assert_eq!(selected(cx).len(), 2, "the range from notes to docs");
    let listed = rows(&panel, cx);
    assert!(row(&listed, "notes").selected && row(&listed, "docs").selected);

    // The context menu of a selected project works on the selection.
    cx.simulate_mouse_down(docs_at, MouseButton::Right, Modifiers::none());
    assert_eq!(
        menu_labels(&view, cx),
        [
            "New Workspace from Selection…",
            "Move Selection to Workspace",
            "Remove Selection from Workspace"
        ]
    );
    run_entry(&view, cx, "New Workspace from Selection");
    let modal = view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()));
    assert_eq!(modal.as_deref(), Some("New workspace"));
    view.update(cx, |wb, cx| wb.set_modal_input("team", cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the workspace", |_| {
        find(&w.daemon, "team").is_some()
            && daemon_parent(&w.daemon, &w.notes).is_some()
            && daemon_parent(&w.daemon, &w.docs).is_some()
    });
    let team = by_name(&w.daemon, "team");
    assert_eq!(team.kind, WorkspaceKind::Workspace);
    assert_eq!(daemon_parent(&w.daemon, &w.notes), Some(team.id));
    assert_eq!(daemon_parent(&w.daemon, &w.docs), Some(team.id));
    wait_until(cx, "the tree", |cx| {
        let list = outline(&panel, cx);
        list.windows(2)
            .any(|p| p[0] == "W team" && p[1].starts_with("  P "))
    });
    assert!(selected(cx).is_empty(), "the selection is spent");
    assert_ne!(
        active(&view, cx),
        Some(team.id),
        "making a workspace from a selection does not move the window"
    );

    // One project alone has its own menu; several in different places offer the move submenu.
    let notes_at = row_center(&view, cx, w.notes.id);
    cx.simulate_click(notes_at, Modifiers::command());
    let web_at = row_center(&view, cx, w.web.id);
    cx.simulate_click(web_at, Modifiers::command());
    cx.simulate_mouse_down(web_at, MouseButton::Right, Modifiers::none());
    run_entry(&view, cx, "Move Selection to Workspace");
    assert_eq!(
        menu_labels(&view, cx),
        ["shop", "lab", "team", "New Workspace…"],
        "the second level of the menu"
    );
    run_entry(&view, cx, "lab");
    wait_until(cx, "both to move into lab", |_| {
        daemon_parent(&w.daemon, &w.notes) == Some(w.lab.id)
            && daemon_parent(&w.daemon, &w.web) == Some(w.lab.id)
    });
}

#[gpui::test]
fn the_plus_menu_makes_workspaces_and_opens_folders_into_them(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let _panel = panel(&view, cx);

    // Open Folder into Workspace asks for a folder and puts its project into the workspace.
    let plus = center_of_region(&view, cx, Region::SidebarControl("plus"));
    cx.simulate_click(plus, Modifiers::none());
    assert_eq!(
        menu_labels(&view, cx),
        [
            "New Workspace…",
            "Open Folder…",
            "Open Folder into Workspace"
        ]
    );
    run_entry(&view, cx, "Open Folder into Workspace");
    assert_eq!(menu_labels(&view, cx), ["shop", "lab"]);
    run_entry(&view, cx, "lab");
    assert!(cx.did_prompt_for_paths());
    let fresh = w.folders.extra("fresh");
    let chosen = fresh.clone();
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![chosen])
    });
    wait_until(cx, "fresh to join lab", |_| {
        find(&w.daemon, "fresh").is_some_and(|p| p.parent_id == Some(w.lab.id))
    });
    let fresh_info = by_name(&w.daemon, "fresh");
    wait_until(cx, "lab to be shown with its new project", |cx| {
        active(&view, cx) == Some(w.lab.id) && active_project(&view, cx) == Some(fresh_info.id)
    });

    // New Workspace… with a name only.
    cx.dispatch_action(actions::ShowPlusMenu(point(px(60.0), px(60.0))));
    run_entry(&view, cx, "New Workspace");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .modal()
            .and_then(|m| m.choice_label().map(str::to_owned))),
        Some("Choose folders…".to_owned()),
        "folders are optional"
    );
    view.update(cx, |wb, cx| wb.set_modal_input("blank", cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the empty workspace", |cx| {
        find(&w.daemon, "blank").is_some()
            && active(&view, cx) == find(&w.daemon, "blank").map(|b| b.id)
    });
    assert!(
        all(&w.daemon)
            .iter()
            .all(|p| p.parent_id != find(&w.daemon, "blank").map(|b| b.id))
    );

    // New Workspace… with folders: the second button asks for them.
    cx.dispatch_action(actions::NewWorkspace);
    view.update(cx, |wb, cx| wb.set_modal_input("studio", cx));
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    assert!(cx.did_prompt_for_paths());
    let (one, two) = (w.folders.extra("one"), w.folders.extra("two"));
    let (first, second) = (one.clone(), two.clone());
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && options.multiple);
        Some(vec![first, second])
    });
    wait_until(cx, "studio with two projects", |_| {
        let studio = find(&w.daemon, "studio");
        studio.is_some_and(|s| {
            all(&w.daemon)
                .iter()
                .filter(|p| p.parent_id == Some(s.id))
                .count()
                == 2
        })
    });
    let studio = by_name(&w.daemon, "studio");
    wait_until(cx, "studio to be shown", |cx| {
        active(&view, cx) == Some(studio.id)
    });
}

#[gpui::test]
fn rename_opens_with_the_whole_name_selected_so_typing_and_pasting_replace_it(
    cx: &mut TestAppContext,
) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);
    let text = |cx: &mut VisualTestContext| {
        panel.read_with(cx, |p, _| p.renaming().map(|(_, text)| text.to_owned()))
    };

    // Inline: the old name is selected, typing replaces all of it.
    view.update_in(cx, |wb, window, cx| {
        wb.rename_workspace(w.shop.id, window, cx)
    });
    draw(cx);
    assert_eq!(text(cx).as_deref(), Some("shop"));
    assert!(panel.read_with(cx, |p, _| p.rename_selected()));
    cx.simulate_input("store");
    assert_eq!(text(cx).as_deref(), Some("store"), "replaced, not appended");
    assert!(!panel.read_with(cx, |p, _| p.rename_selected()));
    cx.simulate_input("s");
    assert_eq!(
        text(cx).as_deref(),
        Some("stores"),
        "then it edits as usual"
    );
    cx.simulate_keystrokes("escape");

    // A paste replaces it too.
    view.update_in(cx, |wb, window, cx| {
        wb.rename_workspace(w.shop.id, window, cx)
    });
    draw(cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("Orchard review".to_owned()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(text(cx).as_deref(), Some("Orchard review"));
    cx.simulate_keystrokes("escape");

    // Backspace clears the selection; ⌘A selects the text again after an edit.
    view.update_in(cx, |wb, window, cx| {
        wb.rename_workspace(w.shop.id, window, cx)
    });
    draw(cx);
    cx.simulate_keystrokes("backspace");
    assert_eq!(text(cx).as_deref(), Some(""));
    cx.simulate_input("ab");
    cx.simulate_keystrokes("cmd-a");
    assert!(panel.read_with(cx, |p, _| p.rename_selected()));
    cx.simulate_input("c");
    assert_eq!(text(cx).as_deref(), Some("c"));
    cx.simulate_keystrokes("escape");
}

#[gpui::test]
fn a_workspace_is_renamed_in_place_and_deleting_it_keeps_its_projects(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let panel = panel(&view, cx);
    let renaming = |cx: &mut VisualTestContext| {
        panel.read_with(cx, |p, _| {
            p.renaming().map(|(id, text)| (id, text.to_owned()))
        })
    };

    // The workspace menu, then Rename… edits the row itself: no dialog.
    cx.dispatch_action(actions::ShowWorkspaceMenu(actions::WorkspaceMenuAt {
        workspace: w.shop.id.to_string(),
        position: point(px(50.0), px(50.0)),
    }));
    assert_eq!(
        menu_labels(&view, cx),
        [
            "Pin",
            "Rename…",
            "Add Folder…",
            "Delete Workspace…",
            "Reveal in Finder"
        ]
    );
    run_entry(&view, cx, "Rename");
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert_eq!(renaming(cx), Some((w.shop.id, "shop".to_owned())));
    cx.simulate_keystrokes("backspace backspace backspace backspace");
    cx.simulate_input("store");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new name", |_| find(&w.daemon, "store").is_some());
    assert_eq!(renaming(cx), None);
    wait_until(cx, "the row", |cx| outline(&panel, cx)[0] == "W store");
    // Its projects are still in it.
    assert_eq!(daemon_parent(&w.daemon, &w.api), Some(w.shop.id));

    // Clicking elsewhere while editing confirms (the way Finder does).
    view.update_in(cx, |wb, window, cx| {
        wb.rename_workspace(w.lab.id, window, cx)
    });
    draw(cx);
    cx.simulate_keystrokes("backspace backspace backspace");
    cx.simulate_input("garage");
    let elsewhere = row_center(&view, cx, w.notes.id);
    cx.simulate_click(elsewhere, Modifiers::none());
    wait_until(cx, "the name to be sent", |_| {
        find(&w.daemon, "garage").is_some()
    });
    assert_eq!(renaming(cx), None);

    // Delete Workspace… asks first and says what stays.
    open_session(&view, cx, w.s_web.id);
    assert_eq!(active(&view, cx), Some(w.shop.id));
    cx.dispatch_action(actions::ShowWorkspaceMenu(actions::WorkspaceMenuAt {
        workspace: w.shop.id.to_string(),
        position: point(px(50.0), px(50.0)),
    }));
    run_entry(&view, cx, "Delete Workspace");
    let (title, message) = view
        .read_with(cx, |wb, _| {
            wb.modal()
                .map(|m| (m.title.to_string(), m.message.to_string()))
        })
        .expect("a confirmation");
    assert_eq!(title, "Delete workspace?");
    assert!(
        message.contains("store") && message.contains("2 projects") && message.contains("kept"),
        "{message}"
    );
    // Cancelling changes nothing.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(find(&w.daemon, "store").is_some());
    cx.dispatch_action(actions::ShowWorkspaceMenu(actions::WorkspaceMenuAt {
        workspace: w.shop.id.to_string(),
        position: point(px(50.0), px(50.0)),
    }));
    run_entry(&view, cx, "Delete Workspace");
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the workspace to go", |_| {
        find(&w.daemon, "store").is_none()
    });

    // The projects and their sessions stay, now under "No workspace".
    for project in [&w.api, &w.web] {
        assert_eq!(daemon_parent(&w.daemon, project), None);
        assert!(all(&w.daemon).iter().any(|p| p.id == project.id));
    }
    for session in [&w.s_main, &w.s_wt, &w.s_web] {
        assert!(w.daemon.session(session.id).state.is_live());
    }
    wait_until(cx, "the sidebar", |cx| {
        let list = outline(&panel, cx);
        let at = list.iter().position(|l| l == "- No workspace");
        at.is_some_and(|at| {
            list[at..].contains(&"P api".to_owned()) && list[at..].contains(&"P web".to_owned())
        })
    });
    assert!(!outline(&panel, cx).iter().any(|l| l == "W store"));
}

// ---- layouts ----------------------------------------------------------------------------------

#[gpui::test]
fn the_layout_belongs_to_the_container_switched_to(cx: &mut TestAppContext) {
    let w = World::new();
    let state_path = w.daemon.home().join("desktop-state.json");
    let (view, cx) = w.synced(cx, Some(state_path));
    let state = |cx: &mut VisualTestContext| view.read_with(cx, |wb, _| wb.state().clone());

    // A project in a workspace shows the workspace's layout: both projects share it.
    open_session(&view, cx, w.s_main.id);
    assert_eq!(active(&view, cx), Some(w.shop.id));
    assert_eq!(active_project(&view, cx), Some(w.api.id));
    open_session(&view, cx, w.s_web.id);
    assert_eq!(
        active(&view, cx),
        Some(w.shop.id),
        "same container, no switch"
    );
    assert_eq!(active_project(&view, cx), Some(w.web.id));
    let shop_layout = state(cx).center;
    assert_eq!(state(cx).panes().len(), 1);
    assert_eq!(
        state(cx).panes()[0].items.len(),
        2,
        "both tabs in one layout"
    );

    // A project in no workspace has its own; the workspace's is parked under the workspace's id.
    open_session(&view, cx, w.s_notes.id);
    assert_eq!(active(&view, cx), Some(w.notes.id));
    assert_eq!(active_project(&view, cx), Some(w.notes.id));
    let parked: Vec<String> = state(cx).parked.keys().cloned().collect();
    assert!(parked.contains(&w.shop.id.to_string()), "{parked:?}");
    assert!(
        !parked.contains(&w.api.id.to_string()) && !parked.contains(&w.web.id.to_string()),
        "layouts are never keyed by a project inside a workspace: {parked:?}"
    );
    assert_eq!(state(cx).panes()[0].items.len(), 1);

    // Back through a project row, through the other one, and through the workspace row.
    cx.dispatch_action(actions::OpenWorkspace(w.api.id.to_string()));
    assert_eq!(active(&view, cx), Some(w.shop.id));
    assert_eq!(active_project(&view, cx), Some(w.api.id));
    assert_eq!(state(cx).center, shop_layout, "restored exactly");
    assert!(state(cx).parked.contains_key(&w.notes.id.to_string()));
    cx.dispatch_action(actions::OpenWorkspace(w.web.id.to_string()));
    assert_eq!(active_project(&view, cx), Some(w.web.id));
    assert_eq!(state(cx).center, shop_layout, "the layout did not move");
    cx.dispatch_action(actions::OpenWorkspace(w.shop.id.to_string()));
    assert_eq!(active(&view, cx), Some(w.shop.id));
    assert_eq!(
        active_project(&view, cx),
        Some(w.web.id),
        "the workspace keeps its project"
    );

    // The sidebar tints the project the window shows, not the workspace.
    let listed = rows(&panel(&view, cx), cx);
    assert!(row(&listed, "web").active && !row(&listed, "shop").active);

    // Put the project the window shows into a workspace: it hands its layout over.
    cx.dispatch_action(actions::OpenWorkspace(w.notes.id.to_string()));
    assert_eq!(active(&view, cx), Some(w.notes.id));
    assign(&w.daemon, &w.notes, Some(&w.shop));
    wait_until(cx, "the window to follow notes into shop", |cx| {
        active(&view, cx) == Some(w.shop.id)
    });
    assert_eq!(
        state(cx).center,
        shop_layout,
        "the workspace's own layout wins"
    );
    assert!(
        !state(cx).parked.contains_key(&w.notes.id.to_string()),
        "the project's layout is gone"
    );

    // Take it out again: it is its own container once more, the workspace keeps its layout.
    assign(&w.daemon, &w.notes, None);
    wait_until(cx, "notes to leave", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(w.notes.id)
                .is_some_and(|p| p.parent_id.is_none())
        })
    });
    open_session(&view, cx, w.s_notes.id);
    assert_eq!(active(&view, cx), Some(w.notes.id));
    cx.dispatch_action(actions::OpenWorkspace(w.shop.id.to_string()));
    assert_eq!(state(cx).center, shop_layout);
}

// ---- settings: user < workspace < project -------------------------------------------------------

fn write_settings(root: &Path, text: &str) {
    fs::create_dir_all(root.join(".hutan")).unwrap();
    fs::write(root.join(".hutan/settings.toml"), text).unwrap();
}

#[gpui::test]
fn the_project_layer_applies_over_the_workspace_layer_and_hot_reloads(cx: &mut TestAppContext) {
    let w = World::new();
    // The workspace layer is its first project's file (`api`); each project may override it.
    write_settings(&w.folders.api, "[terminal]\nfont_size = 20\n");
    write_settings(&w.folders.web, "[terminal]\nfont_size = 18\n");
    write_settings(&w.folders.notes, "[terminal]\nfont_size = 24\n");
    let (view, cx) = w.synced(cx, None);
    let font = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size)
    };
    assert_eq!(font(&view, cx), 13.0, "Home: the user's value");

    // api: the workspace layer and the project layer read the same file.
    open_session(&view, cx, w.s_main.id);
    wait_until(cx, "the workspace layer", |cx| font(&view, cx) == 20.0);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        Some(w.folders.api.clone())
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.project_root().cloned()), None);

    // web: its own file wins over the workspace's.
    open_session(&view, cx, w.s_web.id);
    wait_until(cx, "the project layer", |cx| font(&view, cx) == 18.0);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.project_root().cloned()),
        Some(w.folders.web.clone())
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        Some(w.folders.api.clone())
    );

    // The workspace row alone: the workspace layer.
    cx.dispatch_action(actions::OpenWorkspace(w.lab.id.to_string()));
    wait_until(cx, "lab (no folders): the user's value", |cx| {
        font(&view, cx) == 13.0
    });
    cx.dispatch_action(actions::OpenWorkspace(w.shop.id.to_string()));
    // The workspace keeps the project the user chose in it.
    wait_until(cx, "web again", |cx| font(&view, cx) == 18.0);

    // A project in no workspace layers only its own file, and hot-reloads while it stays active.
    open_session(&view, cx, w.s_notes.id);
    wait_until(cx, "the loose project", |cx| font(&view, cx) == 24.0);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        None
    );
    write_settings(&w.folders.notes, "[terminal]\nfont_size = 26\n");
    wait_until(cx, "the edit", |cx| font(&view, cx) == 26.0);
    // Even a file that did not exist when the project was shown.
    write_settings(&w.folders.docs, "[terminal]\nfont_size = 15\n");
    cx.dispatch_action(actions::OpenWorkspace(w.docs.id.to_string()));
    wait_until(cx, "docs", |cx| font(&view, cx) == 15.0);

    // New terminals start in the project the user works in.
    open_session(&view, cx, w.s_web.id);
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|id| id != w.s_web.id)
    });
    let created = w
        .daemon
        .session(view.read_with(cx, |wb, _| wb.focused_session()).unwrap());
    assert_eq!(
        created.workspace_id, w.web.id,
        "the project of the focused session"
    );
    assert_eq!(
        fs::canonicalize(created.cwd.as_deref().unwrap()).unwrap(),
        w.folders.web
    );
    assert_eq!(active(&view, cx), Some(w.shop.id));
}

// ---- ⌘J ---------------------------------------------------------------------------------------

#[gpui::test]
fn the_jump_palette_lists_workspaces_projects_and_sessions(cx: &mut TestAppContext) {
    let w = World::new();
    let (view, cx) = w.synced(cx, None);
    let _panel = panel(&view, cx);

    cx.dispatch_action(actions::OpenJump);
    let entries = view
        .read_with(cx, |wb, _| wb.picker().map(|p| p.entries.clone()))
        .unwrap();
    let titles: Vec<String> = entries.iter().map(|e| e.title.to_string()).collect();
    assert_eq!(
        titles,
        [
            "shop", "api", "web", "lab", "notes", "Home", "s-main", "s-wt", "s-web", "s-notes"
        ],
        "workspaces with their projects, loose projects, Home, then the sessions — hidden ones \
         too, but not `docs`: a project that never had a session is stale (E-13a)"
    );
    let detail = |name: &str| {
        entries
            .iter()
            .find(|e| e.title == name)
            .map(|e| e.detail.to_string())
            .unwrap()
    };
    assert_eq!(detail("shop"), "2 projects");
    assert!(detail("api").starts_with("shop › "), "{}", detail("api"));
    assert!(detail("s-wt").contains("shop › api") && detail("s-wt").contains("wt topic"));
    assert!(detail("s-wt").contains("api-wt/topic"));
    assert!(
        detail("s-notes").starts_with("notes ·"),
        "{}",
        detail("s-notes")
    );

    // Enter on a workspace shows it; on a project of a workspace, that workspace; on a session,
    // its tab.
    view.update(cx, |wb, cx| wb.set_picker_query("lab", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&view, cx), Some(w.lab.id));
    cx.dispatch_action(actions::OpenJump);
    view.update(cx, |wb, cx| wb.set_picker_query("web", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&view, cx), Some(w.shop.id));
    assert_eq!(active_project(&view, cx), Some(w.web.id));
    cx.dispatch_action(actions::OpenJump);
    view.update(cx, |wb, cx| wb.set_picker_query("s-notes", cx));
    cx.simulate_keystrokes("enter");
    assert_eq!(active(&view, cx), Some(w.notes.id));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(w.s_notes.id)
    );
}
