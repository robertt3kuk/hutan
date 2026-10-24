#![allow(clippy::unwrap_used, clippy::expect_used)]
//! MAWAS-FILES: Mawas's folder is browsable and its changes can be versioned. Files, Search and
//! Problems show the Home folder at Home and follow Mawas's scope while Mawas Home is the
//! focused center tab; Mawas's ⋯ has the folder's actions; *Track changes in this folder…*
//! (`project.git_init`) turns Changes and checkpoints on at Home. A real in-process daemon, the
//! in-memory credential store and a loopback provider; nothing calls a model.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::assistant::{AssistantEnv, Options, open, open_assistant};
use common::{draw, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::sessions::{MawasFocus, ScopeKind, ScopeLabel};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::{
    AssistantChoice, AssistantPanel, FilesPanel, ProblemsPanel, SearchPanel,
};
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_protocol::messages::{Request, WorkspaceCreate, WorkspaceInfo, WorkspaceKind};

fn started() -> AssistantEnv {
    AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    })
}

/// The Home folder of the daemon (canonical), with a file and a folder in it.
fn home_folder(env: &AssistantEnv) -> PathBuf {
    let folder = &env.daemon.paths.default_home_folder;
    fs::create_dir_all(folder.join("projects")).unwrap();
    fs::write(folder.join("notes.md"), "# notes\n").unwrap();
    folder.canonicalize().unwrap()
}

fn label(name: &str, kind: ScopeKind) -> Option<ScopeLabel> {
    Some(ScopeLabel {
        name: name.to_owned(),
        kind,
        branch: None,
    })
}

/// Files, Search and Problems, drawn once each (the views stay cached afterwards).
struct Scoped {
    files: Entity<FilesPanel>,
    search: Entity<SearchPanel>,
    problems: Entity<ProblemsPanel>,
}

impl Scoped {
    fn open(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Self {
        let settle = |cx: &mut VisualTestContext| {
            draw(cx);
            draw(cx);
            view.update_in(cx, |wb, window, cx| {
                use gpui::Focusable as _;
                window.focus(&wb.focus_handle(cx), cx);
            });
        };
        cx.dispatch_action(actions::ShowFiles);
        settle(cx);
        cx.dispatch_action(actions::ShowSearch);
        settle(cx);
        cx.dispatch_action(actions::ShowProblems);
        settle(cx);
        let (files, search, problems) = view.read_with(cx, |wb, _| {
            (
                wb.panel::<FilesPanel>("files"),
                wb.panel::<SearchPanel>("search"),
                wb.panel::<ProblemsPanel>("problems"),
            )
        });
        Self {
            files: files.expect("Files is drawn"),
            search: search.expect("Search is drawn"),
            problems: problems.expect("Problems is drawn"),
        }
    }

    /// What Files, Search and Problems say they are about.
    fn scopes(&self, cx: &mut VisualTestContext) -> [Option<ScopeLabel>; 3] {
        cx.read(|cx| {
            [
                self.files.read(cx).scope(cx),
                self.search.read(cx).scope(cx),
                self.problems.read(cx).scope(cx),
            ]
        })
    }

    fn wait_root(&self, cx: &mut VisualTestContext, when: &str, root: &Path) {
        wait_until(
            cx,
            &format!("{when}: Files to list {}", root.display()),
            |cx| {
                self.files
                    .read_with(cx, |p, _| p.root() == Some(root) && p.settled())
            },
        );
    }

    fn names(&self, cx: &mut VisualTestContext) -> Vec<String> {
        self.files.read_with(cx, |p, _| {
            p.rows()
                .iter()
                .map(|row| {
                    row.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect()
        })
    }
}

fn home_id(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> WorkspaceId {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .home_workspace()
            .map(|w| w.id)
            .expect("Home is listed")
    })
}

fn mawas_focus(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<MawasFocus> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).mawas_focus())
}

/// Opens `dir` as the project the window works in.
fn project(view: &Entity<Workbench>, cx: &mut VisualTestContext, dir: &Path) -> WorkspaceId {
    let dir = dir.to_owned();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(dir.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(dir.as_path())
        })
    });
    view.read_with(cx, |wb, _| wb.active_workspace()).unwrap()
}

/// A tab that is not Mawas Home, shown in the focused pane.
fn show_other_tab(view: &Entity<Workbench>, cx: &mut VisualTestContext, title: &str) {
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder(title)).unwrap();
        });
    });
    draw(cx);
}

#[gpui::test]
fn files_search_and_problems_show_the_home_folder_at_home(cx: &mut TestAppContext) {
    let env = started();
    let home_dir = home_folder(&env);
    let (view, cx) = open(cx, &env.daemon);
    let home = home_id(&view, cx);
    cx.dispatch_action(actions::OpenWorkspace(home.to_string()));
    wait_until(cx, "Home to be the active container", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(home))
    });
    let scoped = Scoped::open(&view, cx);

    scoped.wait_root(cx, "call 1", &home_dir);
    let names = scoped.names(cx);
    assert!(
        names.iter().any(|n| n == "notes.md") && names.iter().any(|n| n == "projects"),
        "{names:?}"
    );
    assert_eq!(
        scoped.scopes(cx),
        std::array::from_fn(|_| label("Home", ScopeKind::Home)),
        "every panel says Home"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).files_root()),
        Some(home_dir.clone())
    );

    // A file of it opens as a normal file tab.
    let notes = home_dir.join("notes.md");
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&notes), window, cx);
    });
    let key = notes.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() == Some(ItemRef::file(key.clone()))
        })
    });
    assert_eq!(mawas_focus(&view, cx), None);
}

#[gpui::test]
fn files_follow_mawass_scope_while_mawas_home_is_the_focused_tab(cx: &mut TestAppContext) {
    let env = started();
    let home_dir = home_folder(&env);
    let lab = env.project("lab");
    fs::write(lab.join("lab.txt"), "lab\n").unwrap();
    let (view, cx) = open(cx, &env.daemon);
    let lab_id = project(&view, cx, &lab);
    let scoped = Scoped::open(&view, cx);
    scoped.wait_root(cx, "call 2", &lab);
    assert_eq!(
        scoped.scopes(cx)[0],
        label("lab", ScopeKind::Folder),
        "the project the window works in"
    );
    assert_eq!(mawas_focus(&view, cx), None);

    // Mawas Home in the active (workspace) scope: Files, Search and Problems say so, and show
    // that workspace's project.
    let panel: Entity<AssistantPanel> = open_assistant(&view, cx);
    draw(cx);
    wait_until(cx, "Files, Search and Problems to follow Mawas", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Workspace(lab_id))
    });
    let workspace = label("Mawas · lab", ScopeKind::Mawas);
    assert_eq!(
        scoped.scopes(cx),
        std::array::from_fn(|_| workspace.clone())
    );
    scoped.wait_root(cx, "call 3", &lab);
    assert!(scoped.names(cx).iter().any(|n| n == "lab.txt"));

    // Global: the Home folder.
    panel.update(cx, |p, cx| p.choose(AssistantChoice::Global, cx));
    draw(cx);
    wait_until(cx, "Mawas's Global scope", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Global)
    });
    let global = label("Mawas · Global", ScopeKind::Mawas);
    assert_eq!(scoped.scopes(cx), std::array::from_fn(|_| global.clone()));
    scoped.wait_root(cx, "call 4", &home_dir);
    assert!(scoped.names(cx).iter().any(|n| n == "notes.md"));

    // A file from there opens as a normal file tab, and with Mawas no longer the tab shown the
    // panels are about the project again.
    let notes = home_dir.join("notes.md");
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&notes), window, cx);
    });
    let key = notes.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() == Some(ItemRef::file(key.clone()))
        })
    });
    draw(cx);
    wait_until(cx, "the panels to leave Mawas", |cx| {
        mawas_focus(&view, cx).is_none()
    });
    assert_eq!(scoped.scopes(cx)[0], label("lab", ScopeKind::Folder));
    scoped.wait_root(cx, "call 5", &lab);

    // Another tab: still the project's. Back on Mawas: her scope again.
    show_other_tab(&view, cx, "Elsewhere");
    assert_eq!(mawas_focus(&view, cx), None);
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    draw(cx);
    wait_until(cx, "Mawas's scope again", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Global)
    });
    scoped.wait_root(cx, "call 6", &home_dir);
}

#[gpui::test]
fn a_workspace_mawas_shows_the_workspaces_active_project(cx: &mut TestAppContext) {
    let env = started();
    let a = env.project("alpha");
    let b = env.project("beta");
    fs::write(a.join("a.txt"), "a\n").unwrap();
    fs::write(b.join("b.txt"), "b\n").unwrap();
    let shop: WorkspaceInfo = env.daemon.call(Request::WorkspaceCreate(WorkspaceCreate {
        name: "shop".into(),
        paths: vec![a.clone(), b.clone()],
        project_ids: Vec::new(),
        created_by: None,
    }));
    assert_eq!(shop.kind, WorkspaceKind::Workspace);
    let (view, cx) = open(cx, &env.daemon);
    cx.dispatch_action(actions::OpenWorkspace(shop.id.to_string()));
    wait_until(cx, "the workspace to be active", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(shop.id))
    });
    let scoped = Scoped::open(&view, cx);
    // The project Files shows without Mawas is the one she works in.
    let shown = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).files_root())
        .expect("a project of the workspace");
    assert!(shown == a || shown == b, "{shown:?}");
    scoped.wait_root(cx, "call 7", &shown);

    open_assistant(&view, cx);
    draw(cx);
    wait_until(cx, "Mawas's workspace scope", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Workspace(shop.id))
    });
    assert_eq!(
        scoped.scopes(cx),
        std::array::from_fn(|_| label("Mawas · shop", ScopeKind::Mawas))
    );
    scoped.wait_root(cx, "call 8", &shown);
}
