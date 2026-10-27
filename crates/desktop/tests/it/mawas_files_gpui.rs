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
use common::chat::{ChatEnv, held_turns};
use common::{TestDaemon, draw, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::sessions::{MawasFocus, ScopeKind, ScopeLabel};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::{
    AssistantChoice, AssistantPanel, ChangesCheckouts, ChangesPanel, FilesPanel, ProblemsPanel,
    SearchPanel,
};
use hutan_desktop::workbench::rewind::{Phase, SheetFocus};
use hutan_desktop::workbench::{FileOpen, Region, Workbench};
use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::checkpoint::{CheckpointList, CheckpointListRequest};
use hutan_protocol::messages::{
    Request, SessionResult, WorkspaceCreate, WorkspaceInfo, WorkspaceKind,
};

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
        working: None,
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

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let mut bounds = None;
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        bounds = view.read_with(cx, |wb, _| wb.region_bounds(region));
        bounds.is_some()
    });
    cx.simulate_click(bounds.unwrap().center(), Modifiers::none());
    draw(cx);
}

/// Opens Mawas Home's ⋯ menu and lists its entries. A click that lands while the layout is
/// still moving (a dock opening) misses the button: it is made again on the next frame.
fn more_menu(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    wait_until(cx, "the ⋯ menu to open", |cx| {
        if view.read_with(cx, |wb, _| wb.context_menu_entries().is_some()) {
            return true;
        }
        if let Some(bounds) =
            view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("more")))
        {
            cx.simulate_click(bounds.center(), Modifiers::none());
        }
        false
    });
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the ⋯ menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, prefix: &str) {
    let labels = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the ⋯ menu is open");
    let index = labels
        .iter()
        .position(|(label, _)| label.starts_with(prefix))
        .unwrap_or_else(|| panic!("no entry `{prefix}` in {labels:?}"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

fn can_track(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> bool {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).can_track_home())
}

#[gpui::test]
fn mawas_more_menu_has_where_mawas_works_for_both_scopes(cx: &mut TestAppContext) {
    let env = started();
    let home_dir = home_folder(&env);
    let lab = env.project("lab");
    let (view, cx) = open(cx, &env.daemon);
    let revealed = std::rc::Rc::new(std::cell::RefCell::new(Vec::<PathBuf>::new()));
    let sink = revealed.clone();
    view.update(cx, |wb, _| {
        wb.set_reveal_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });

    // Global (the window is at Home): the Home folder, with the way to version it.
    let home = home_id(&view, cx);
    cx.dispatch_action(actions::OpenWorkspace(home.to_string()));
    wait_until(cx, "Home to be the active container", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(home))
    });
    open_assistant(&view, cx);
    wait_until(cx, "the Home folder not to be a repository yet", |cx| {
        can_track(&view, cx)
    });
    let labels = more_menu(&view, cx);
    let at = labels
        .iter()
        .position(|l| l.starts_with("Backend: "))
        .expect("the backend entry");
    let tail = &labels[at + 1..];
    assert!(
        tail[0].starts_with("Where Mawas works: ") && tail[0].ends_with("Hutan"),
        "{labels:?}"
    );
    assert_eq!(
        tail[1..],
        [
            "Show Folder in Files",
            "Open Folder in Finder",
            "Open Terminal Here",
            "Track changes in this folder…"
        ],
        "{labels:?}"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_disabled())
            .iter()
            .any(|l| l.starts_with("Where Mawas works: ")),
        "the folder line is greyed out"
    );

    // Open Folder in Finder reveals it.
    run_entry(&view, cx, "Open Folder in Finder");
    assert_eq!(*revealed.borrow(), std::slice::from_ref(&home_dir));

    // Open Terminal Here: a terminal of Home, started in the folder.
    more_menu(&view, cx);
    run_entry(&view, cx, "Open Terminal Here");
    wait_until(cx, "a terminal in the Home folder", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().iter().any(|s| {
                s.workspace_id == home
                    && s.cwd.as_deref().and_then(|c| c.canonicalize().ok())
                        == Some(home_dir.clone())
            })
        })
    });

    // The terminal is the tab shown.
    wait_until(cx, "the terminal's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_center_item() != Some(ItemRef::panel("assistant"))
        })
    });

    // Show Folder in Files: the Files panel comes up, on the Home folder.
    // The right body opens on Files; another panel is shown so the menu has to bring it up.
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("changes".into())
    );
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    draw(cx);
    more_menu(&view, cx);
    run_entry(&view, cx, "Show Folder in Files");
    wait_until(cx, "the Files panel", |cx| {
        draw(cx);
        view.read_with(cx, |wb, cx| {
            wb.state().docks.right.active == Some("files".into())
                && wb
                    .panel::<FilesPanel>("files")
                    .is_some_and(|p| p.read(cx).root() == Some(home_dir.as_path()))
        })
    });

    // Track changes in this folder… opens the confirm sheet (nothing runs before OK).
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    draw(cx);
    more_menu(&view, cx);
    run_entry(&view, cx, "Track changes in this folder");
    let (title, message) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().expect("the confirm sheet");
        (modal.title.to_string(), modal.message.to_string())
    });
    assert_eq!(title, "Track changes in this folder?");
    for words in [
        "git init",
        ".gitignore",
        "first commit",
        "git identity",
        "Nothing leaves this machine",
    ] {
        assert!(message.contains(words), "{words}: {message}");
    }
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(!home_dir.join(".git").exists(), "Cancel changes nothing");

    // A workspace's Mawas works in its project: no Track entry, same folder entries.
    let lab_id = project(&view, cx, &lab);
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    draw(cx);
    wait_until(cx, "Mawas's workspace scope", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Workspace(lab_id))
    });
    let labels = more_menu(&view, cx);
    let at = labels
        .iter()
        .position(|l| l.starts_with("Where Mawas works: "))
        .expect("the folder line");
    assert!(labels[at].ends_with("lab"), "{labels:?}");
    assert_eq!(
        labels[at + 1..],
        [
            "Show Folder in Files",
            "Open Folder in Finder",
            "Open Terminal Here"
        ],
        "{labels:?}"
    );
    run_entry(&view, cx, "Open Folder in Finder");
    assert_eq!(revealed.borrow().last(), Some(&lab));
}

/// A daemon whose Mawas is a Codex replay, whose Home folder (`<work>/Hutan`, one file in it) is
/// not a repository, and whose git has an identity.
struct TrackEnv {
    chat: ChatEnv,
    daemon: TestDaemon,
    home_dir: PathBuf,
    gate: PathBuf,
}

fn track_env() -> TrackEnv {
    let chat = ChatEnv::new();
    let base = chat.cwd();
    let home_dir = base.join("Hutan");
    fs::create_dir(&home_dir).unwrap();
    fs::write(home_dir.join("a.txt"), "alpha\n").unwrap();
    let identity = base.join("gitconfig");
    fs::write(
        &identity,
        "[user]\n\tname = Test\n\temail = test@example.com\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    let gate = base.join("gate-0");
    let transcript = held_turns(&base, std::slice::from_ref(&gate));
    let folder = home_dir.clone();
    let daemon = TestDaemon::start_with_codex_config(
        "[assistant]\nbackend = \"codex\"\n",
        chat.replay(&transcript),
        move |config| {
            config.checkpoints.git_env = std::collections::BTreeMap::from([
                (
                    "GIT_CONFIG_GLOBAL".to_owned(),
                    identity.display().to_string(),
                ),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ]);
            config.paths.default_home_folder = folder;
        },
    );
    TrackEnv {
        chat,
        daemon,
        home_dir,
        gate,
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[gpui::test]
fn track_changes_turns_changes_on_and_a_mawas_turn_gets_a_checkpoint(cx: &mut TestAppContext) {
    let env = track_env();
    let (view, cx) = open(cx, &env.daemon);
    let home = home_id(&view, cx);
    cx.dispatch_action(actions::OpenWorkspace(home.to_string()));
    wait_until(cx, "Home to be the active container", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(home))
    });
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    draw(cx);
    let changes: Entity<ChangesPanel> = view
        .read_with(cx, |wb, _| wb.panel("changes"))
        .expect("Changes is drawn");

    // Changes at Home: nothing to list, and the button that tracks the folder.
    wait_until(cx, "Changes to say Home has no checkout", |cx| {
        let empty = changes.read_with(
            cx,
            |p, _| matches!(p.checkouts(), ChangesCheckouts::Ready(list) if list.is_empty()),
        );
        empty && can_track(&view, cx)
    });
    click(&view, cx, Region::Changes("track-home"));
    let title = view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()));
    assert_eq!(title.as_deref(), Some("Track changes in this folder?"));
    assert!(!env.home_dir.join(".git").exists());

    // OK: the daemon runs it, and Changes lists the Home folder as a checkout.
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "Changes to list the Home folder", |cx| {
        draw(cx);
        changes.read_with(cx, |p, _| {
            p.rows().len() == 1 && p.rows()[0].main && p.rows()[0].path == env.home_dir
        })
    });
    wait_until(cx, "the store to know Home is a repository", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).home_is_repository())
    });
    assert!(!can_track(&view, cx));
    assert_eq!(git(&env.home_dir, &["rev-list", "--count", "HEAD"]), "1");
    assert!(env.home_dir.join(".gitignore").is_file());
    assert!(view.read_with(cx, |wb, _| {
        wb.notice().is_some_and(|n| n.contains("Tracking changes"))
    }));

    // Mawas's ⋯ no longer offers it.
    open_assistant(&view, cx);
    let labels = more_menu(&view, cx);
    assert!(
        labels.iter().any(|l| l == "Open Terminal Here")
            && !labels.iter().any(|l| l.starts_with("Track changes")),
        "{labels:?}"
    );

    // A Mawas turn that edits a file now has a checkpoint.
    let session: SessionResult = env.daemon.call(Request::AssistantOpen(AssistantOpen {
        scope: AssistantScope::Global,
        workspace_id: None,
        new: false,
    }));
    let session = session.session.id;
    let _: ChatSent = env.daemon.call(Request::ChatSend(ChatSend {
        session_id: session,
        text: "turn 0".into(),
        attachments: vec![],
    }));
    fs::write(env.home_dir.join("b.txt"), "written by Mawas\n").unwrap();
    fs::write(&env.gate, "").unwrap();
    wait_until(cx, "the turn's checkpoint", |_| {
        let list: CheckpointList =
            env.daemon
                .call(Request::CheckpointList(CheckpointListRequest {
                    session_id: Some(session),
                    ..CheckpointListRequest::default()
                }));
        list.checkpoints.iter().any(|c| c.files_changed == 1)
    });

    // Rewinding her turn puts the note for her in *her* composer (Mawas Home owns its model),
    // not in a model of a chat tab nobody sees.
    let list: CheckpointList = env
        .daemon
        .call(Request::CheckpointList(CheckpointListRequest {
            session_id: Some(session),
            ..CheckpointListRequest::default()
        }));
    let checkpoint = list
        .checkpoints
        .iter()
        .find(|c| c.files_changed == 1)
        .expect("the turn's checkpoint")
        .id;
    wait_until(cx, "the checkpoint in the window's store", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .checkpoints()
                .read(cx)
                .get(checkpoint)
                .is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_rewind(session, checkpoint, window, cx);
    });
    wait_until(cx, "the rewind preview", |cx| {
        view.read_with(cx, |wb, _| {
            wb.rewind_sheet()
                .is_some_and(|s| matches!(s.phase, Phase::Ready) && !s.working && !s.busy())
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.press_rewind(SheetFocus::Restore, window, cx);
    });
    wait_until(cx, "the restore", |cx| {
        view.read_with(cx, |wb, _| {
            wb.restore_toast().is_some() && wb.rewind_sheet().is_none()
        })
    });
    assert!(!env.home_dir.join("b.txt").exists(), "the file is gone");
    let assistant: Entity<AssistantPanel> = view
        .read_with(cx, |wb, _| wb.panel("assistant"))
        .expect("Mawas Home is open");
    let draft = assistant.read_with(cx, |panel, cx| {
        panel
            .chat_model(session)
            .expect("her chat model")
            .read(cx)
            .input()
            .text()
            .to_owned()
    });
    assert!(
        draft.starts_with("Hutan restored these files to before") && draft.contains("b.txt"),
        "{draft}"
    );
    let _ = &env.chat;
}

/// `git init` with one commit in `dir`.
fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "first",
        ],
    );
}

#[gpui::test]
fn changes_follows_mawass_scope_like_files_while_mawas_home_is_the_focused_tab(
    cx: &mut TestAppContext,
) {
    let env = track_env();
    init_repo(&env.home_dir);
    let lab = env.chat.cwd().join("lab");
    fs::create_dir(&lab).unwrap();
    fs::write(lab.join("lab.txt"), "lab\n").unwrap();
    init_repo(&lab);
    let lab = lab.canonicalize().unwrap();
    let home_dir = env.home_dir.canonicalize().unwrap();
    let (view, cx) = open(cx, &env.daemon);
    project(&view, cx, &lab);
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    draw(cx);
    let changes: Entity<ChangesPanel> = view
        .read_with(cx, |wb, _| wb.panel("changes"))
        .expect("Changes is drawn");
    // The checkout shown is the one of the panel's rows that `shown` names first.
    let shown_path = |cx: &mut VisualTestContext| -> Option<PathBuf> {
        cx.read(|cx| {
            let panel = changes.read(cx);
            panel
                .shown(cx)
                .first()
                .map(|&index| panel.rows()[index].path.clone())
        })
    };
    wait_until(
        cx,
        "Changes to show the project the window works in",
        |cx| shown_path(cx).as_deref() == Some(lab.as_path()),
    );
    assert_eq!(
        cx.read(|cx| changes.read(cx).scope(cx)).map(|s| s.name),
        Some("lab".to_owned())
    );

    // Mawas Home, Global: Changes shows the Home checkout, with Mawas's chip.
    let panel: Entity<AssistantPanel> = open_assistant(&view, cx);
    panel.update(cx, |p, cx| p.choose(AssistantChoice::Global, cx));
    draw(cx);
    wait_until(cx, "Mawas's Global scope", |cx| {
        mawas_focus(&view, cx) == Some(MawasFocus::Global)
    });
    wait_until(cx, "Changes to show the Home checkout", |cx| {
        draw(cx);
        shown_path(cx).as_deref() == Some(home_dir.as_path())
    });
    assert_eq!(
        cx.read(|cx| changes.read(cx).scope(cx)),
        label("Mawas · Global", ScopeKind::Mawas)
    );

    // A memory file Mawas wrote into the tracked Home folder carries the hint; another file
    // of the folder does not.
    fs::create_dir_all(home_dir.join(".mawas/memory")).unwrap();
    fs::write(home_dir.join(".mawas/memory/global.md"), "# Memory\n").unwrap();
    fs::write(home_dir.join("notes.txt"), "notes\n").unwrap();
    changes.update(cx, |panel, cx| panel.refresh(cx));
    wait_until(cx, "the memory file in Home's changes", |cx| {
        draw(cx);
        cx.read(|cx| changes.read(cx).files_of(&home_dir).len()) == 2
    });
    let rows: Vec<String> = cx.read(|cx| {
        changes
            .read(cx)
            .files_of(&home_dir)
            .iter()
            .map(|f| f.path.clone())
            .collect()
    });
    for (index, path) in rows.iter().enumerate() {
        let hinted = view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChangesPanelMemoryHint(0, index))
                .is_some()
        });
        assert_eq!(hinted, path.starts_with(".mawas/memory/"), "{path}");
    }

    // Another tab: the project's again.
    show_other_tab(&view, cx, "Elsewhere");
    wait_until(cx, "Changes to leave Mawas", |cx| {
        draw(cx);
        shown_path(cx).as_deref() == Some(lab.as_path())
    });
    assert_eq!(mawas_focus(&view, cx), None);
}
