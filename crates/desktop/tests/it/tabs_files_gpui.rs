#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! TABS-FILES: closing the tab of a session closes the session (`tabs.close_session`), one tab
//! per file, and files opened beside the agent. Real daemon, real sockets; every wait is on
//! state.

use crate::common;

use common::chat::ChatEnv;
use common::{TestDaemon, click_at, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, SessionState};
use hutan_desktop::workbench::model::{ItemRef, PaneId};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request};
use hutan_settings::{SettingsPaths, SettingsStore};

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n[history]\nenabled = false\n{text}"),
    )
    .unwrap();
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
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

fn tab_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Option<(PaneId, usize)> {
    view.read_with(cx, |wb, _| {
        wb.state().find_session_tab(&session.to_string())
    })
}

/// Shows `session` in the focused pane and waits for its tab.
fn show(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> (PaneId, usize) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the tab of the session", |cx| {
        tab_of(view, cx, session).is_some()
    });
    tab_of(view, cx, session).unwrap()
}

fn click_close(view: &Entity<Workbench>, cx: &mut VisualTestContext, tab: (PaneId, usize)) {
    let target = Region::TabClose(tab.0, tab.1);
    wait_until(cx, "the × of the tab to be drawn", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(target).is_some())
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap()
        .center();
    click_at(cx, at);
}

fn known(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(session).is_some()
    })
}

fn wait_closed(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    wait_until(cx, "the session to be closed", |cx| {
        !known(view, cx, session)
    });
}

fn report_working(daemon: &TestDaemon, session: SessionId) {
    let _: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session,
        source: "fake-hooks".into(),
        agent: "Claude Code".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
}

fn wait_working(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    wait_until(cx, "the agent working", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(session).is_some_and(|s| {
                s.agent
                    .as_ref()
                    .is_some_and(|a| a.state == AgentState::Working)
            })
        })
    });
}

fn running(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> bool {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(session)
            .is_some_and(|s| s.state == SessionState::Running)
    })
}

// ---- 1: closing a session's tab closes the session ---------------------------------------

#[gpui::test]
fn the_x_of_an_idle_terminal_tab_closes_its_session_and_undo_brings_it_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    let tab = show(&view, cx, session.id);

    click_close(&view, cx, tab);
    wait_closed(&view, cx, session.id);
    assert!(tab_of(&view, cx, session.id).is_none(), "the tab went too");
    assert!(
        view.read_with(cx, |wb, _| !wb.modal_open()),
        "an idle session closes without a question"
    );
    wait_until(cx, "the Undo toast", |cx| {
        view.read_with(cx, |wb, _| {
            wb.close_toast().is_some_and(|t| t.session == session.id)
        })
    });

    draw(cx);
    let undo = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::CloseToast("undo")))
        .unwrap()
        .center();
    click_at(cx, undo);
    wait_until(cx, "the session and its tab to come back", |cx| {
        known(&view, cx, session.id) && tab_of(&view, cx, session.id).is_some()
    });
    wait_until(cx, "running again", |cx| running(&view, cx, session.id));
}

#[gpui::test]
fn cmd_w_in_a_terminal_closes_the_session_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, session.id);
    cx.simulate_keystrokes("cmd-w");
    wait_closed(&view, cx, session.id);
    assert!(tab_of(&view, cx, session.id).is_none());
}

#[gpui::test]
fn a_tab_whose_session_runs_something_asks_keep_running_or_close(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let session = daemon.create_session(&["cat"], None);
    report_working(&daemon, session.id);
    let (view, cx) = open(cx, &daemon);
    wait_working(&view, cx, session.id);
    let tab = show(&view, cx, session.id);

    // The question names what runs and offers both ways out.
    click_close(&view, cx, tab);
    wait_until(cx, "the question", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    let (title, message, keep, close, destructive) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (
            modal.title.to_string(),
            modal.message.to_string(),
            modal.confirm_label.to_string(),
            modal.choice_label().map(str::to_owned),
            modal.destructive,
        )
    });
    assert!(title.starts_with("Close "), "{title}");
    assert!(message.contains("Claude Code is working"), "{message}");
    assert!(message.ends_with("Closing stops it."), "{message}");
    assert_eq!(keep, "Keep running in background");
    assert_eq!(close.as_deref(), Some("Close"));
    assert!(!destructive, "Return keeps it running, never the stop");

    // Escape leaves everything as it was.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert!(tab_of(&view, cx, session.id).is_some(), "the tab stays");
    assert!(running(&view, cx, session.id));

    // Keep running in background: the tab goes, the session stays in the sidebar.
    click_close(&view, cx, tab);
    wait_until(cx, "the question again", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the tab to go", |cx| {
        tab_of(&view, cx, session.id).is_none()
    });
    assert!(running(&view, cx, session.id), "the session keeps running");

    // Close: stops it and closes the session.
    let tab = show(&view, cx, session.id);
    click_close(&view, cx, tab);
    wait_until(cx, "the question a third time", |cx| {
        view.read_with(cx, |wb, _| wb.modal_open())
    });
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    wait_closed(&view, cx, session.id);
    assert!(tab_of(&view, cx, session.id).is_none());
}

#[gpui::test]
fn always_stops_what_runs_without_asking(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "[tabs]\nclose_session = \"always\"\n");
    let session = daemon.create_session(&["cat"], None);
    report_working(&daemon, session.id);
    let (view, cx) = open(cx, &daemon);
    wait_working(&view, cx, session.id);
    let tab = show(&view, cx, session.id);

    click_close(&view, cx, tab);
    wait_closed(&view, cx, session.id);
    assert!(view.read_with(cx, |wb, _| !wb.modal_open()));
}

#[gpui::test]
fn never_closes_the_tab_only(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "[tabs]\nclose_session = \"never\"\n");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    let tab = show(&view, cx, session.id);

    click_close(&view, cx, tab);
    wait_until(cx, "the tab to go", |cx| {
        tab_of(&view, cx, session.id).is_none()
    });
    draw(cx);
    assert!(known(&view, cx, session.id), "the session stays listed");
    assert!(running(&view, cx, session.id));
    assert!(view.read_with(cx, |wb, _| wb.close_toast().is_none()));
}

#[gpui::test]
fn the_older_terminal_tab_close_still_counts_until_tabs_close_session_is_set(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "[terminal]\ntab_close = \"detach\"\n");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    let tab = show(&view, cx, session.id);
    click_close(&view, cx, tab);
    wait_until(cx, "the tab to go", |cx| {
        tab_of(&view, cx, session.id).is_none()
    });
    draw(cx);
    assert!(known(&view, cx, session.id), "detach is never");

    // The new key wins where both are set.
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user(
                "tabs.close_session",
                toml::Value::String("when_idle".to_owned()),
            )
            .unwrap();
    });
    let tab = show(&view, cx, session.id);
    click_close(&view, cx, tab);
    wait_closed(&view, cx, session.id);
}

#[gpui::test]
fn the_tab_menu_can_still_close_the_tab_alone(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let session = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    let tab = show(&view, cx, session.id);
    wait_until(cx, "the tab to be drawn", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Tab(tab.0, tab.1)).is_some()
        })
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Tab(tab.0, tab.1)))
        .unwrap()
        .center();
    cx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Right, gpui::Modifiers::none());
    draw(cx);
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the tab's menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert!(
        labels.iter().any(|l| l == "Close Tab, Keep Running"),
        "{labels:?}"
    );
    assert_eq!(labels.last().map(String::as_str), Some("Close"));
}

#[gpui::test]
fn closing_a_chat_tab_closes_the_chat_session(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    write_settings(&daemon, "");
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let tab = show(&view, cx, chat.id);
    assert!(matches!(
        view.read_with(cx, |wb, _| wb
            .state()
            .pane(tab.0)
            .and_then(|p| p.items.get(tab.1).cloned())),
        Some(ItemRef::Chat { .. })
    ));
    click_close(&view, cx, tab);
    wait_closed(&view, cx, chat.id);
    assert!(tab_of(&view, cx, chat.id).is_none());
}

#[gpui::test]
fn splits_follow_the_same_rule(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let first = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, first.id);
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the split", |cx| {
        view.read_with(cx, |wb, _| wb.state().active_term_split().is_some())
    });
    let both: Vec<SessionId> = view.read_with(cx, |wb, _| {
        wb.state()
            .active_term_split()
            .unwrap()
            .2
            .sessions()
            .iter()
            .map(|s| s.parse().unwrap())
            .collect()
    });
    assert_eq!(both.len(), 2);
    // The × of a tab of splits closes the session of every split.
    let (pane, index, _) = view.read_with(cx, |wb, _| {
        let (pane, index, split) = wb.state().active_term_split().unwrap();
        (pane, index, split.len())
    });
    click_close(&view, cx, (pane, index));
    wait_until(cx, "both sessions to be closed", |cx| {
        both.iter().all(|s| !known(&view, cx, *s))
    });
}

// ---- 2: one tab per file ---------------------------------------------------------------------

use hutan_desktop::workbench::FileOpen;
use hutan_desktop::workbench::files::FilePlace;
use std::path::{Path, PathBuf};

fn scratch_files(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let paths = names
        .iter()
        .map(|name| {
            let path = root.join(name);
            std::fs::write(&path, name).unwrap();
            path
        })
        .collect();
    (dir, paths)
}

/// Opens `path` the way a click in the Files tree does and waits until its tab is shown.
fn open_file(view: &Entity<Workbench>, cx: &mut VisualTestContext, path: &Path, place: FilePlace) {
    let target = path.display().to_string();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file_placed(FileOpen::picked(path), place, window, cx);
    });
    wait_until(cx, &format!("{target} to have a tab"), |cx| {
        file_tab(view, cx, &target).is_some()
    });
}

fn file_tab(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    path: &str,
) -> Option<(PaneId, usize)> {
    view.read_with(cx, |wb, _| wb.state().find_item(&ItemRef::file(path)))
}

/// The names of the file tabs of every pane, in tree order.
fn file_names(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| pane.items.iter())
            .filter_map(|item| item.as_file())
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    })
}

#[gpui::test]
fn every_file_opened_gets_its_own_tab_by_default(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let (view, cx) = open(cx, &daemon);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt", "c.txt"]);
    // What a single click in the Files tree does, three times.
    for file in &files {
        open_file(&view, cx, file, FilePlace::Preview);
    }
    assert_eq!(file_names(&view, cx), ["a.txt", "b.txt", "c.txt"]);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.preview_file().map(str::to_owned)),
        None,
        "no tab is a preview"
    );
}

#[gpui::test]
fn preview_tabs_stay_an_option(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "[editor]\npreview_tabs = true\n");
    let (view, cx) = open(cx, &daemon);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt"]);
    open_file(&view, cx, &files[0], FilePlace::Preview);
    open_file(&view, cx, &files[1], FilePlace::Preview);
    assert_eq!(
        file_names(&view, cx),
        ["b.txt"],
        "the second look replaced the first"
    );
}

// ---- 3: files beside the agent ----------------------------------------------------------------

fn pane_count(view: &Entity<Workbench>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |wb, _| wb.state().center.panes().len())
}

/// The panes of the center with the names of their tabs (a session's tab by its kind).
fn layout(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<Vec<String>> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .center
            .panes()
            .into_iter()
            .map(|pane| {
                pane.items
                    .iter()
                    .map(|item| match item {
                        ItemRef::Chat { .. } => "chat".to_owned(),
                        ItemRef::Terminal { .. } => "terminal".to_owned(),
                        ItemRef::File { path } => Path::new(path)
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        other => format!("{other:?}"),
                    })
                    .collect()
            })
            .collect()
    })
}

fn focused_pane_holds(view: &Entity<Workbench>, cx: &VisualTestContext, name: &str) -> bool {
    view.read_with(cx, |wb, _| {
        wb.state().active_pane().is_some_and(|p| {
            p.active_item()
                .and_then(|i| i.as_file())
                .is_some_and(|f| f.ends_with(name))
        })
    })
}

#[gpui::test]
fn a_file_opens_in_a_split_right_of_a_chat_and_the_split_is_reused(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    write_settings(&daemon, "");
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, chat.id);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt"]);

    open_file(&view, cx, &files[0], FilePlace::Focused);
    assert_eq!(layout(&view, cx), [vec!["chat"], vec!["a.txt"]]);
    assert!(focused_pane_holds(&view, cx, "a.txt"));

    // The chat in front again: the next file goes to the same split, as a tab.
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to have the focus", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session() == Some(chat.id))
    });
    open_file(&view, cx, &files[1], FilePlace::Focused);
    assert_eq!(layout(&view, cx), [vec!["chat"], vec!["a.txt", "b.txt"]]);
    assert_eq!(pane_count(&view, cx), 2);

    // The file's own pane in front: the next file stays with it (nothing to go beside).
    let (_dir2, more) = scratch_files(&["c.txt"]);
    open_file(&view, cx, &more[0], FilePlace::Focused);
    assert_eq!(
        layout(&view, cx),
        [vec!["chat"], vec!["a.txt", "b.txt", "c.txt"]]
    );
}

#[gpui::test]
fn a_file_opens_beside_a_terminal_agent_but_not_beside_a_plain_terminal(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let agent = daemon.create_session(&["cat"], None);
    report_working(&daemon, agent.id);
    let plain = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    wait_working(&view, cx, agent.id);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt"]);

    show(&view, cx, plain.id);
    open_file(&view, cx, &files[0], FilePlace::Focused);
    assert_eq!(
        layout(&view, cx),
        [vec!["terminal", "a.txt"]],
        "a plain terminal: the file is a tab of the pane"
    );

    show(&view, cx, agent.id);
    open_file(&view, cx, &files[1], FilePlace::Focused);
    assert_eq!(
        layout(&view, cx),
        [vec!["terminal", "a.txt", "terminal"], vec!["b.txt"]],
        "a terminal agent: the file goes to a split on its right"
    );
}

#[gpui::test]
fn beside_the_agent_is_a_setting(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    write_settings(&daemon, "[editor]\nopen_beside_agent = false\n");
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    show(&view, cx, chat.id);
    let (_dir, files) = scratch_files(&["a.txt"]);
    open_file(&view, cx, &files[0], FilePlace::Focused);
    assert_eq!(layout(&view, cx), [vec!["chat", "a.txt"]]);
}

#[gpui::test]
fn the_side_places_always_open_to_the_side_and_reuse_the_split(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let (view, cx) = open(cx, &daemon);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt", "c.txt"]);
    open_file(&view, cx, &files[0], FilePlace::Focused);
    assert_eq!(layout(&view, cx), [vec!["a.txt"]]);

    // Beside a file: a split, once; the pane it was opened from stays the one to open from.
    open_file(&view, cx, &files[1], FilePlace::Split);
    assert_eq!(layout(&view, cx), [vec!["a.txt"], vec!["b.txt"]]);
    focus_first_pane(&view, cx);
    open_file(&view, cx, &files[2], FilePlace::Split);
    assert_eq!(
        layout(&view, cx),
        [vec!["a.txt"], vec!["b.txt", "c.txt"]],
        "the split on the right takes it"
    );
}

/// A click on the first tab of the first pane gives that pane the focus.
fn focus_first_pane(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    let pane = view.read_with(cx, |wb, _| wb.state().center.panes()[0].id);
    let target = Region::Tab(pane, 0);
    wait_until(cx, "the first tab to be drawn", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(target).is_some())
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap()
        .center();
    click_at(cx, at);
    wait_until(cx, "the first pane to have the focus", |cx| {
        view.read_with(cx, |wb, _| wb.state().active_pane_id() == pane)
    });
}

#[gpui::test]
fn alt_click_in_the_files_tree_opens_to_the_side_and_the_command_reuses_the_split(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::theme::DensityExt as _;
    use hutan_desktop::workbench::actions;
    use hutan_desktop::workbench::panels::FilesPanel;
    use hutan_desktop::workbench::style::UiStyle;
    let daemon = TestDaemon::start();
    write_settings(&daemon, "");
    let (view, cx) = open(cx, &daemon);
    let (_dir, files) = scratch_files(&["a.txt", "b.txt"]);
    let root = files[0].parent().unwrap().to_owned();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(root.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(root.as_path())
        })
    });
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn");
    wait_until(cx, "two rows", |cx| {
        panel.read_with(cx, |p, _| p.rows().len() == 2)
    });
    let row_at = |cx: &mut VisualTestContext, index: usize| {
        let tree = view
            .read_with(cx, |wb, _| wb.region_bounds(Region::Files("tree")))
            .expect("the tree was drawn");
        let height = cx.update(|_, app| UiStyle::get(app).density.ui().sidebar_row - 4.0);
        gpui::point(
            tree.origin.x + gpui::px(80.0),
            tree.origin.y + gpui::px(height * (index as f32 + 0.5)),
        )
    };

    // A plain click shows the file in the pane it is in; the ⌥-click puts the next beside it.
    let at = row_at(cx, 0);
    cx.simulate_click(at, gpui::Modifiers::none());
    draw(cx);
    wait_until(cx, "a.txt to be shown", |cx| {
        file_names(&view, cx) == ["a.txt"]
    });
    assert_eq!(pane_count(&view, cx), 1);
    let at = row_at(cx, 1);
    cx.simulate_mouse_move(at, None, gpui::Modifiers::alt());
    cx.simulate_click(at, gpui::Modifiers::alt());
    draw(cx);
    wait_until(cx, "b.txt to be shown to the side", |cx| {
        layout(&view, cx) == [vec!["a.txt"], vec!["b.txt"]]
    });

    // *Open to the Side* from the first pane: the split is reused, no third pane.
    focus_first_pane(&view, cx);
    cx.dispatch_action(actions::FilesOpenToSide);
    draw(cx);
    assert_eq!(pane_count(&view, cx), 2);
}
