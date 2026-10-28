#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! SESSION-DIRS-B: more than one folder in a session (protocol 0.57, ADR 0023 decision 4), seen
//! from the window against an in-process daemon whose harnesses replay recorded transcripts:
//! the folders chip of a chat header and of a terminal agent, the popover with each folder's
//! state in words (`applied`), *Add Folder…* and remove, the paste flow of a running terminal
//! agent (typed `/add-dir …` without Enter), *Restart with this folder*, the ＋ menu's two
//! entries, the session menu's *Folders…*, and Files showing a session's folders as roots.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_core::SessionId;
use hutan_desktop::workbench::chat::{ChatModel, LoadStatus};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::FilesPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::chat::{ChatCreate, ChatDirApplied};
use hutan_protocol::messages::{
    Request, SessionCreate, SessionInfo, SessionResult, SessionSpec, TerminalSpec, WorkspaceInfo,
    WorkspaceKind,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::Value;

// ---- harness ----------------------------------------------------------------------------------

/// Temporary folders with recognisable names, canonical, kept alive for the test.
struct Folders {
    root: tempfile::TempDir,
}

impl Folders {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn make(&self, name: &str) -> PathBuf {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn open_chat(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> Entity<ChatModel> {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(session).is_some()
        })
    });
    view.read_with(cx, |wb, _| wb.chat_model_of(session))
        .unwrap()
}

fn bounds_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let bounds = bounds_of(view, cx, region).unwrap_or_else(|| panic!("{region:?} is not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn wait_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        bounds_of(view, cx, region).is_some()
    });
}

fn session(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> SessionInfo {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(id).cloned())
        .expect("the session is listed")
}

/// The extra folders of a chat or a terminal with how each is held, as the store has them.
fn extras(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Vec<(PathBuf, ChatDirApplied)> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session_folders(id)
            .map(|folders| {
                folders
                    .extras
                    .iter()
                    .map(|folder| (folder.path.clone(), folder.applied))
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn wait_extras(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
    what: &str,
    mut wanted: impl FnMut(&[(PathBuf, ChatDirApplied)]) -> bool,
) {
    wait_until(cx, what, |cx| wanted(&extras(view, cx, id)));
}

/// Answers the folder panel the window just asked for.
fn choose(cx: &mut VisualTestContext, folder: &Path) {
    assert!(cx.did_prompt_for_paths(), "a folder panel is open");
    let folder = folder.to_path_buf();
    cx.simulate_path_prompt_response(move |_| Some(vec![folder]));
    draw(cx);
}

fn lines(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| wb.folders_popover_lines(cx))
}

fn menu_labels(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .unwrap_or_default()
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, prefix: &str) {
    let index = menu_labels(view, cx)
        .iter()
        .position(|label| label.starts_with(prefix))
        .unwrap_or_else(|| panic!("no menu entry `{prefix}` in {:?}", menu_labels(view, cx)));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
    draw(cx);
}

/// The ＋ menu of the sidebar, opened through the window's own dispatch.
fn plus_menu(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update_in(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(actions::ShowPlusMenu(point(px(60.0), px(60.0)))),
            cx,
        );
    });
    draw(cx);
    menu_labels(view, cx)
}

fn create_chat(
    daemon: &TestDaemon,
    agent: &str,
    cwd: &Path,
    workspace: Option<hutan_core::WorkspaceId>,
) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: workspace,
        cwd: cwd.to_path_buf(),
        agent: agent.to_owned(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    created.session
}

fn terminal(
    daemon: &TestDaemon,
    script: &str,
    cwd: &Path,
    workspace: Option<hutan_core::WorkspaceId>,
) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: workspace,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), script.to_owned()],
            cwd: Some(cwd.to_path_buf()),
            env: [("PS1".to_owned(), "$ ".to_owned())].into(),
            ..TerminalSpec::default()
        }),
    }));
    created.session
}

fn codex_daemon(env: &ChatEnv) -> TestDaemon {
    TestDaemon::start_with_codex(env.fixture("plain-turn"))
}

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn acp_fixture(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A daemon whose Claude Code replays one transcript per start (`new`, then `load` for the
/// restart), as the daemon's own session-folder tests do.
fn claude_daemon(dir: &Path) -> TestDaemon {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in ["dirs-claude-new", "dirs-claude-load"].iter().enumerate() {
        let text: Vec<String> = acp_fixture(name).iter().map(Value::to_string).collect();
        std::fs::write(
            scripted.join(format!("{:02}.jsonl", index + 1)),
            text.join("\n") + "\n",
        )
        .unwrap();
    }
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.claude]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n",
        bin = acp_bin(),
        received = dir.join("received.jsonl"),
    ))
}

fn project(daemon: &TestDaemon, folder: &Path, name: &str) -> WorkspaceInfo {
    daemon.create_workspace(name, &[folder])
}

// ---- the chip and the popover -----------------------------------------------------------------

#[gpui::test]
fn a_chat_header_has_a_folders_chip_and_its_popover_adds_and_removes_folders(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = codex_daemon(&env);
    let folders = Folders::new();
    let (a, b) = (folders.make("shared-lib"), folders.make("docs"));
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    open_chat(&view, cx, chat.id);

    // The chip names the working folder; no `+N` while there are no extra folders.
    wait_region(&view, cx, Region::Folders("chip"));
    let model = view.read_with(cx, |wb, cx| wb.sessions().read(cx).session_folders(chat.id));
    let model = model.expect("a Codex chat holds folders");
    assert_eq!(model.chip_count(), None);
    assert_eq!(
        model.chip_name(),
        env.cwd().file_name().unwrap().to_string_lossy()
    );

    // A click opens the popover under it: the working directory, locked, and the harness line.
    click(&view, cx, Region::Folders("chip"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.folders_popover_session()),
        Some(chat.id)
    );
    wait_region(&view, cx, Region::Folders("add"));
    let said = lines(&view, cx);
    assert!(
        said[0].ends_with("· working directory"),
        "the first folder is the session's own: {said:?}"
    );
    assert!(said.last().unwrap().contains("Codex"), "{said:?}");
    assert!(
        bounds_of(&view, cx, Region::Folders("restart")).is_none(),
        "nothing needs a restart"
    );

    // Add Folder… goes through the system's folder panel; Codex takes it with the next turn.
    click(&view, cx, Region::Folders("add"));
    choose(cx, &a);
    wait_extras(&view, cx, chat.id, "the folder", |e| {
        e == [(a.clone(), ChatDirApplied::NextTurn)]
    });
    wait_until(cx, "the folder in the popover", |cx| {
        lines(&view, cx).contains(&"shared-lib · applies from the next turn".to_owned())
    });
    let model = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).session_folders(chat.id))
        .unwrap();
    assert_eq!(model.chip_count().as_deref(), Some("+1"));

    // A second one, then the first one out again.
    click(&view, cx, Region::Folders("add"));
    choose(cx, &b);
    wait_extras(&view, cx, chat.id, "both folders", |e| e.len() == 2);
    draw(cx);
    click(&view, cx, Region::FolderRow("remove", 0));
    wait_extras(&view, cx, chat.id, "the first folder to go", |e| {
        e == [(b.clone(), ChatDirApplied::NextTurn)]
    });

    // The daemon's refusal is said in the popover, in its words: a session's own folder.
    click(&view, cx, Region::Folders("add"));
    choose(cx, &env.cwd());
    wait_until(cx, "the refusal", |cx| {
        view.read_with(cx, |wb, _| wb.folders_popover_problem().is_some())
    });
    let problem = view
        .read_with(cx, |wb, _| wb.folders_popover_problem().map(str::to_owned))
        .unwrap();
    assert!(problem.contains("own folder"), "{problem}");
    assert_eq!(extras(&view, cx, chat.id).len(), 1, "nothing was added");

    // Escape closes it.
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.folders_popover_session()),
        None
    );
}

#[gpui::test]
fn the_session_menu_opens_the_folders_popover(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = codex_daemon(&env);
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    open_chat(&view, cx, chat.id);

    view.update_in(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(actions::ShowSessionMenu(actions::SessionMenuAt {
                session: chat.id.to_string(),
                position: point(px(80.0), px(80.0)),
            })),
            cx,
        );
    });
    draw(cx);
    let labels = menu_labels(&view, cx);
    assert!(labels.contains(&"Folders…".to_owned()), "{labels:?}");
    run_entry(&view, cx, "Folders…");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.folders_popover_session()),
        Some(chat.id)
    );
}

#[gpui::test]
fn the_agents_page_row_menu_opens_the_folders_of_a_chat(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = codex_daemon(&env);
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(actions::ShowAgentMenu(actions::SessionMenuAt {
                session: chat.id.to_string(),
                position: point(px(80.0), px(80.0)),
            })),
            cx,
        );
    });
    draw(cx);
    let labels = menu_labels(&view, cx);
    let at = labels.iter().position(|l| l == "Folders…");
    let copy = labels.iter().position(|l| l == "Copy session ID");
    assert!(at.is_some() && at < copy, "{labels:?}");
    run_entry(&view, cx, "Folders…");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.folders_popover_session()),
        Some(chat.id)
    );
}

#[gpui::test]
fn a_claude_chat_says_a_new_folder_needs_a_restart_and_restarts_with_it(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let daemon = claude_daemon(work.path());
    let folders = Folders::new();
    let (cwd, docs) = (folders.make("app"), folders.make("docs"));
    let chat = create_chat(&daemon, "claude", &cwd, None);
    let (view, cx) = open(cx, &daemon);
    open_chat(&view, cx, chat.id);
    wait_region(&view, cx, Region::Folders("chip"));

    click(&view, cx, Region::Folders("chip"));
    wait_region(&view, cx, Region::Folders("add"));
    click(&view, cx, Region::Folders("add"));
    choose(cx, &docs);
    // ACP defines no change mid-session: the daemon keeps the folder and says what it needs.
    wait_extras(&view, cx, chat.id, "the folder", |e| {
        e == [(docs.clone(), ChatDirApplied::Restart)]
    });
    // The window offers the restart at once, in a dialog that says what it costs.
    wait_until(cx, "the restart prompt", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    let (title, message) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (modal.title.to_string(), modal.message.to_string())
    });
    assert_eq!(title, "Restart with this folder?");
    assert!(
        message.contains("docs") && message.contains("stops it and starts it again"),
        "{message}"
    );
    // Not now: the folder stays and keeps saying so.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert_eq!(extras(&view, cx, chat.id).len(), 1);
    assert!(
        lines(&view, cx).contains(&"docs · needs a restart".to_owned()),
        "{:?}",
        lines(&view, cx)
    );
    assert!(lines(&view, cx).contains(&"Restart with this folder".to_owned()));

    // The popover's button asks again; confirmed, the session stops and starts with the folder.
    wait_region(&view, cx, Region::Folders("restart"));
    click(&view, cx, Region::Folders("restart"));
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_extras(&view, cx, chat.id, "the folder to be applied", |e| {
        e == [(docs.clone(), ChatDirApplied::Now)]
    });
    assert!(session(&view, cx, chat.id).state.is_live());
    wait_until(cx, "the button to go", |cx| {
        bounds_of(&view, cx, Region::Folders("restart")).is_none()
    });
    assert_eq!(
        session(&view, cx, chat.id).chat.unwrap().dirs[0].path,
        cwd,
        "the working folder is still the first"
    );
}

// ---- a terminal agent -------------------------------------------------------------------------

#[gpui::test]
fn a_running_terminal_agent_is_typed_add_dir_for_the_user_to_confirm(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config("[terminal]\nenv_inherit = \"daemon\"\n");
    let folders = Folders::new();
    let (project, docs) = (folders.make("project"), folders.make("docs"));
    let typed = format!("/add-dir {}", docs.display());
    let log = folders.root.path().join("typed.log");
    // OMP's own status mark names the agent; the terminal is then raw and the program keeps
    // exactly the bytes typed at it.
    let script = format!(
        "printf '\\033]7501;state=idle:app=omp\\033\\\\'; stty raw -echo; dd bs=1 count={} of={log:?} 2>/dev/null; exec sleep 60",
        typed.len()
    );
    let agent = terminal(&daemon, &script, &project, None);
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the agent to be recognised", |cx| {
        session(&view, cx, agent.id)
            .terminal_agent
            .is_some_and(|a| a.app == "omp")
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(agent.id, window, cx));
    wait_region(&view, cx, Region::Folders("terminal-chip"));

    click(&view, cx, Region::Folders("terminal-chip"));
    wait_region(&view, cx, Region::Folders("add"));
    let said = lines(&view, cx);
    assert!(said.last().unwrap().contains("press Return"), "{said:?}");
    click(&view, cx, Region::Folders("add"));
    choose(cx, &docs);

    // The daemon typed the command, nothing more: no Enter, so the agent waits for the user.
    wait_until(cx, "the typed text to arrive", |_| {
        std::fs::read_to_string(&log).unwrap_or_default().len() >= typed.len()
    });
    assert_eq!(std::fs::read_to_string(&log).unwrap(), typed);
    wait_extras(&view, cx, agent.id, "the folder", |e| {
        e == [(docs.clone(), ChatDirApplied::Paste)]
    });
    // The popover gets out of the way and the status line says what is left to do.
    wait_until(cx, "the popover to close", |cx| {
        view.read_with(cx, |wb, _| wb.folders_popover_session().is_none())
    });
    let notice = view
        .read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
        .expect("a notice");
    assert!(
        notice.contains(&typed) && notice.contains("press Return"),
        "{notice}"
    );
    // The chip counts it, and says in words that it is not applied yet.
    let model = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session_folders(agent.id)
        })
        .unwrap();
    assert_eq!(model.chip_count().as_deref(), Some("+1"));
    assert!(model.waits(&model.extras[0]));
}

#[gpui::test]
fn a_plain_shell_has_no_chip_or_menu_entry_until_the_plus_menu_gives_it_a_folder(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start_with_config("[terminal]\nenv_inherit = \"daemon\"\n");
    let folders = Folders::new();
    let (project, docs) = (folders.make("project"), folders.make("docs"));
    let shell = terminal(&daemon, "exec sleep 60", &project, None);
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(shell.id, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(shell.id).is_some())
    });
    draw(cx);
    let session_menu = |view: &Entity<Workbench>, cx: &mut VisualTestContext| {
        view.update_in(cx, |_, window, cx| {
            window.dispatch_action(
                Box::new(actions::ShowSessionMenu(actions::SessionMenuAt {
                    session: shell.id.to_string(),
                    position: point(px(80.0), px(80.0)),
                })),
                cx,
            );
        });
        draw(cx);
        menu_labels(view, cx)
    };
    assert!(bounds_of(&view, cx, Region::Folders("terminal-chip")).is_none());
    assert!(
        !session_menu(&view, cx)
            .iter()
            .any(|l| l.starts_with("Folders…")),
        "a shell runs no agent that could use a folder"
    );
    cx.simulate_keystrokes("escape");

    // The ＋ menu's entry still gives the session a folder (the file service has it at once, and
    // Files shows it); then the chip and the menu entry appear, and the popover says what the
    // folder is good for here.
    let labels = plus_menu(&view, cx);
    assert!(labels.contains(&"Add Folder to the Focused Session…".to_owned()));
    run_entry(&view, cx, "Add Folder to the Focused Session…");
    choose(cx, &docs);
    wait_extras(&view, cx, shell.id, "the folder", |e| {
        e == [(docs.clone(), ChatDirApplied::Now)]
    });
    wait_region(&view, cx, Region::Folders("terminal-chip"));
    assert!(
        session_menu(&view, cx).contains(&"Folders… (+1)".to_owned()),
        "the menu has it now"
    );
    run_entry(&view, cx, "Folders…");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.folders_popover_session()),
        Some(shell.id)
    );
    assert!(
        lines(&view, cx).last().unwrap().contains("No coding agent"),
        "{:?}",
        lines(&view, cx)
    );
}

// ---- the ＋ menu ------------------------------------------------------------------------------

#[gpui::test]
fn the_plus_menu_adds_a_folder_to_the_workspace_and_to_the_focused_session(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = codex_daemon(&env);
    let folders = Folders::new();
    let home_chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);

    // At Home with nothing open: Home has one folder, and no session is in view.
    let labels = plus_menu(&view, cx);
    assert!(
        labels.contains(&"Add Folder to This Workspace… (Home has one folder)".to_owned()),
        "{labels:?}"
    );
    assert!(
        labels.contains(
            &"Add Folder to the Focused Session… (no chat or terminal in view)".to_owned()
        ),
        "{labels:?}"
    );
    let disabled: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_disabled())
        .iter()
        .map(ToString::to_string)
        .collect();
    for entry in [
        "Add Folder to This Workspace… (Home has one folder)",
        "Add Folder to the Focused Session… (no chat or terminal in view)",
    ] {
        assert!(disabled.contains(&entry.to_owned()), "{disabled:?}");
    }
    cx.simulate_keystrokes("escape");

    // With the chat in view, the entry is live and the folder goes to that session.
    open_chat(&view, cx, home_chat.id);
    let labels = plus_menu(&view, cx);
    assert!(labels.contains(&"Add Folder to the Focused Session…".to_owned()));
    let extra = folders.make("extra");
    run_entry(&view, cx, "Add Folder to the Focused Session…");
    choose(cx, &extra);
    wait_extras(&view, cx, home_chat.id, "the folder", |e| {
        e == [(extra.clone(), ChatDirApplied::NextTurn)]
    });
    // Folders are no projects: nothing joined the sidebar.
    let kinds: Vec<WorkspaceKind> = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .workspaces()
            .iter()
            .map(|w| w.kind)
            .collect()
    });
    assert_eq!(kinds, [WorkspaceKind::Home]);
    // The palette has it too.
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(titles.contains(&"Session: Add Folder to the Focused Session…".to_owned()));
}

#[gpui::test]
fn add_folder_to_this_workspace_makes_a_workspace_around_a_lone_project_and_keeps_the_layout(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let folders = Folders::new();
    let (app, docs, more) = (
        folders.make("acme-web"),
        folders.make("acme-docs"),
        folders.make("shared-ui"),
    );
    let web = project(&daemon, &app, "acme-web");
    let shell = terminal(&daemon, "exec sleep 60", &app, Some(web.id));
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.switch_workspace(web.id, window, cx));
    view.update_in(cx, |wb, window, cx| wb.open_session(shell.id, window, cx));
    draw(cx);
    let tab = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .center
                .panes()
                .into_iter()
                .flat_map(|pane| pane.items.iter())
                .filter(|item| matches!(item, ItemRef::Terminal { .. }))
                .count()
        })
    };
    assert_eq!(tab(cx), 1);

    // A lone project: the entry is there (not greyed) and makes a workspace around the project
    // and the folder.
    let labels = plus_menu(&view, cx);
    assert!(
        labels.contains(&"Add Folder to This Workspace…".to_owned()),
        "{labels:?}"
    );
    run_entry(&view, cx, "Add Folder to This Workspace…");
    choose(cx, &docs);
    wait_until(cx, "the workspace around the project", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store
                .workspace(web.id)
                .and_then(|p| p.parent_id)
                .and_then(|parent| store.workspace(parent))
                .is_some_and(|ws| ws.kind == WorkspaceKind::Workspace && ws.roots.contains(&docs))
        })
    });
    let workspace = view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).workspace(web.id).unwrap().parent_id
    });
    let workspace = workspace.expect("the project is in a workspace now");
    // The layout went with the project: the window shows the workspace, the tab is still there.
    wait_until(cx, "the window to follow into the workspace", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(workspace))
    });
    assert_eq!(
        tab(cx),
        1,
        "the project's tab moved into the workspace's layout"
    );
    let name = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .workspace(workspace)
            .unwrap()
            .name
            .clone()
    });
    assert_eq!(name, "acme-web + acme-docs");

    // Inside the workspace the entry adds a root (`workspace.add_root`).
    plus_menu(&view, cx);
    run_entry(&view, cx, "Add Folder to This Workspace…");
    choose(cx, &more);
    wait_until(cx, "the third folder", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspace(workspace)
                .is_some_and(|ws| ws.roots.contains(&more) && ws.roots.contains(&docs))
        })
    });
    assert_eq!(tab(cx), 1);
}

// ---- Files ------------------------------------------------------------------------------------

#[gpui::test]
fn files_shows_a_sessions_extra_folders_as_roots_while_that_session_is_shown(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = codex_daemon(&env);
    let folders = Folders::new();
    let (app, docs) = (folders.make("app"), folders.make("docs"));
    std::fs::write(app.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(docs.join("guide.md"), "# guide\n").unwrap();
    let web = project(&daemon, &app, "app");
    let chat = create_chat(&daemon, "codex", &app, Some(web.id));
    let shell = terminal(&daemon, "exec sleep 60", &app, Some(web.id));
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.switch_workspace(web.id, window, cx));
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn");
    let roots = |cx: &VisualTestContext| panel.read_with(cx, |p, _| p.roots().to_vec());
    wait_until(cx, "Files to follow the project", |cx| {
        roots(cx) == [app.clone()]
    });

    // The chat takes a folder; with its tab shown, Files lists it as a root next to the project.
    open_chat(&view, cx, chat.id);
    let added =
        daemon.call::<SessionResult>(Request::ChatAddDir(hutan_protocol::chat::ChatAddDir {
            session_id: chat.id,
            path: docs.clone(),
        }));
    assert_eq!(added.session.id, chat.id);
    wait_until(cx, "the session's folder as a root", |cx| {
        roots(cx) == [app.clone(), docs.clone()]
    });
    wait_until(cx, "the folder's files", |cx| {
        panel.read_with(cx, |p, _| {
            p.rows().iter().any(|row| row.name.as_ref() == "docs")
        })
    });

    // Another tab of the same project has no such folder: the root leaves with the session.
    view.update_in(cx, |wb, window, cx| wb.open_session(shell.id, window, cx));
    wait_until(cx, "the root to leave", |cx| roots(cx) == [app.clone()]);
    // Back to the chat, it is a root again; the keyboard being in the Files panel changes nothing.
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the root to come back", |cx| {
        roots(cx) == [app.clone(), docs.clone()]
    });
    view.update_in(cx, |_, window, cx| {
        panel.update(cx, |p, cx| p.focus_tree(window, cx));
    });
    draw(cx);
    assert_eq!(roots(cx), [app.clone(), docs.clone()]);

    // Taking the folder out takes the root away.
    let _: SessionResult = daemon.call(Request::ChatRemoveDir(
        hutan_protocol::chat::ChatRemoveDir {
            session_id: chat.id,
            path: docs.clone(),
        },
    ));
    wait_until(cx, "the root to go with the folder", |cx| {
        roots(cx) == [app.clone()]
    });
}

// ---- terminals that start with a folder -------------------------------------------------------

#[gpui::test]
fn a_terminal_agent_started_with_a_folder_holds_it_now_and_the_chip_counts_it(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start_with_config("[terminal]\nenv_inherit = \"daemon\"\n");
    let folders = Folders::new();
    let (project, docs) = (folders.make("project"), folders.make("docs"));
    // A fake `omp` that logs its arguments, so the folder was given at launch.
    let omp = folders.root.path().join("omp");
    let log = folders.root.path().join("argv.log");
    std::fs::write(
        &omp,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {log:?}\nprintf '\\033]7501;state=idle:app=omp\\033\\\\'\nexec sleep 60\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&omp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec![omp.display().to_string()],
            cwd: Some(project.clone()),
            extra_dirs: vec![docs.clone()],
            ..TerminalSpec::default()
        }),
    }));
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the folder to be held now", |cx| {
        extras(&view, cx, created.session.id) == [(docs.clone(), ChatDirApplied::Now)]
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(created.session.id, window, cx)
    });
    wait_region(&view, cx, Region::Folders("terminal-chip"));
    wait_until(cx, "the program to log its arguments", |_| log.exists());
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("--add-dir={}\n", docs.display())
    );
}
