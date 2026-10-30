#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Mawas is global first (ADR 0025, MAWAS-GLOBAL): Home opens the Global chat whatever workspace
//! is active, a workspace is a *Focus* picked in the header, the *Chats* menu reaches the chats
//! of workspaces from before, and `assistant.home_scope = "follow_workspace"` keeps how Home
//! worked. A real in-process daemon, the in-memory credential store and a loopback provider;
//! nothing calls a model.

use crate::common;

use common::assistant::{
    AssistantEnv, Options, PATH, last_prompt, open, open_assistant, open_following, open_settings,
    reply, shown, texts, wait_answer,
};
use common::{click_at, draw, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::sessions::MawasFocus;
use hutan_desktop::workbench::chat::{ChatModel, ChatView};
use hutan_desktop::workbench::panels::ScopeKey;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::assistant::{
    AssistantList, AssistantListRequest, AssistantOpen, AssistantScope,
};
use hutan_protocol::messages::{Request, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};

fn started() -> AssistantEnv {
    AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    })
}

/// Sends `text` from the composer of `chat`.
fn send(
    model: &Entity<ChatModel>,
    chat: &Entity<ChatView>,
    cx: &mut VisualTestContext,
    text: &str,
) {
    model.update(cx, |m, cx| {
        m.edit(
            |input| {
                input.clear();
                input.insert(text);
            },
            cx,
        );
    });
    assert!(chat.update(cx, |v, cx| v.send(cx)), "the message was sent");
}

/// Presses the middle of `region` as the frame on screen has it.
fn press(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"));
    click_at(cx, bounds.center());
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .is_some()
}

/// The labels (and checkmarks) of the open menu.
fn menu(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<(String, bool)> {
    view.read_with(cx, |wb, _| {
        wb.context_menu_entries()
            .expect("a menu is open")
            .into_iter()
            .map(|(label, checked)| (label.to_string(), checked))
            .collect()
    })
}

fn choose(view: &Entity<Workbench>, cx: &mut VisualTestContext, index: usize) {
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

fn workspace_chat(env: &AssistantEnv, workspace: WorkspaceId) -> SessionId {
    env.daemon
        .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
            scope: AssistantScope::Workspace,
            workspace_id: Some(workspace),
            new: false,
        }))
        .session
        .id
}

fn daemon_focus(env: &AssistantEnv, session: SessionId) -> Option<WorkspaceId> {
    env.daemon.session(session).chat.and_then(|chat| chat.focus)
}

#[gpui::test]
fn home_opens_the_global_chat_whatever_workspace_is_active_and_makes_no_workspace_chat(
    cx: &mut TestAppContext,
) {
    let env = started();
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    wait_until(cx, "the project to be the active workspace", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).active_workspace_id() == Some(workspace.id)
        })
    });
    let panel = open_assistant(&view, cx);
    assert_eq!(panel.read_with(cx, |p, cx| p.key(cx)), ScopeKey::Global);
    assert!(panel.read_with(cx, |p, _| p.global_first()));
    let (global, _, _) = shown(&panel, cx);

    // Only the Global assistant exists: no assistant was made for the active workspace.
    let list: AssistantList = env
        .daemon
        .call(Request::AssistantList(AssistantListRequest::default()));
    assert_eq!(list.assistants.len(), 1, "{:?}", list.assistants);
    assert_eq!(list.assistants[0].scope, AssistantScope::Global);
    assert_eq!(list.assistants[0].session.id, global);

    // The header says Chats and Focus, not the old scope dropdown.
    assert!(drawn(&view, cx, Region::Assistant("chats")));
    assert!(drawn(&view, cx, Region::Assistant("focus")));
    assert!(!drawn(&view, cx, Region::Assistant("scope")));

    // Changing the active workspace moves nothing: Home stays on the Global chat.
    let other = env.project("other");
    let second = env.daemon.create_workspace("other", &[&other]);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(second.id, window, cx);
    });
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(global));
}

#[gpui::test]
fn the_focus_chip_picks_a_workspace_and_all_workspaces_and_reaches_the_next_turn(
    cx: &mut TestAppContext,
) {
    let env = started();
    env.mock.route("POST", PATH, reply("first"));
    env.mock.route("POST", PATH, reply("second"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (global, model, chat) = shown(&panel, cx);
    assert!(panel.read_with(cx, |p, cx| p.focus_chip_shown(cx)));
    assert_eq!(panel.read_with(cx, |p, cx| p.focus(cx)), None);
    assert_eq!(
        panel.read_with(cx, |p, cx| p.focus_label(cx)),
        "All workspaces"
    );
    send(&model, &chat, cx, "hello");
    wait_answer(&model, cx, "first");

    // The picker: All workspaces (checked) and the workspaces, never Home.
    press(&view, cx, Region::Assistant("focus"));
    let rows = menu(&view, cx);
    assert_eq!(rows[0], ("All workspaces".to_owned(), true), "{rows:?}");
    assert!(rows.iter().any(|(label, _)| label == "project"), "{rows:?}");
    assert!(
        !rows
            .iter()
            .any(|(label, _)| label.to_lowercase().contains("home")),
        "Home is not a Focus: {rows:?}"
    );
    let at = rows
        .iter()
        .position(|(label, _)| label == "project")
        .unwrap();
    choose(&view, cx, at);
    wait_until(cx, "the Focus to reach the chip", |cx| {
        panel.read_with(cx, |p, cx| p.focus(cx) == Some(workspace.id))
    });
    assert_eq!(daemon_focus(&env, global), Some(workspace.id));
    assert_eq!(
        panel.read_with(cx, |p, cx| p.focus_label(cx)),
        "project",
        "the chip names the workspace"
    );
    press(&view, cx, Region::Assistant("focus"));
    let rows = menu(&view, cx);
    assert!(
        rows.iter()
            .any(|(label, checked)| label == "project" && *checked),
        "{rows:?}"
    );
    assert!(!rows[0].1, "{rows:?}");
    cx.simulate_keystrokes("escape");

    // The next turn carries the Focus to Mawas; nothing restarted and no chat was created.
    send(&model, &chat, cx, "what is going on?");
    wait_answer(&model, cx, "second");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.starts_with("what is going on?"), "{prompt}");
    assert!(prompt.contains("--- Hutan focus update"), "{prompt}");
    assert!(prompt.contains("Focus is now \"project\""), "{prompt}");
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(global));

    // And back to All workspaces.
    press(&view, cx, Region::Assistant("focus"));
    choose(&view, cx, 0);
    wait_until(cx, "the Focus to be cleared", |cx| {
        panel.read_with(cx, |p, cx| p.focus(cx).is_none())
    });
    assert_eq!(daemon_focus(&env, global), None);
    assert_eq!(
        panel.read_with(cx, |p, cx| p.focus_label(cx)),
        "All workspaces"
    );
}

#[gpui::test]
fn the_chats_menu_lists_global_chats_and_the_old_workspace_chats_and_opens_either(
    cx: &mut TestAppContext,
) {
    let env = started();
    env.mock.route("POST", PATH, reply("from the workspace"));
    env.mock.route("POST", PATH, reply("from global"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    // A chat of the workspace that an earlier version made.
    let old = workspace_chat(&env, workspace.id);
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (global, global_model, global_chat) = shown(&panel, cx);
    assert_ne!(global, old);

    press(&view, cx, Region::Assistant("chats"));
    let rows = menu(&view, cx);
    let labels: Vec<&str> = rows.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Global chats",
            "Mawas",
            "Workspace chats",
            "Mawas · project"
        ],
        "{rows:?}"
    );
    assert!(
        rows[1].1 && !rows[3].1,
        "the shown chat is checked: {rows:?}"
    );

    // Opening the old chat shows it as it was: its own scope, the focus fixed by its workspace.
    choose(&view, cx, 3);
    wait_until(cx, "the workspace chat to show", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx) == Some(old))
    });
    assert_eq!(
        panel.read_with(cx, |p, cx| p.key(cx)),
        ScopeKey::Workspace(workspace.id)
    );
    assert!(!panel.read_with(cx, |p, cx| p.focus_chip_shown(cx)));
    assert!(
        drawn(&view, cx, Region::Assistant("focus")),
        "its fixed workspace is named in the Focus's place"
    );
    let (_, old_model, old_chat) = shown(&panel, cx);
    send(&old_model, &old_chat, cx, "workspace question");
    wait_answer(&old_model, cx, "from the workspace");
    // The workspace chat was not given a Focus, and the active workspace did not matter.
    assert_eq!(daemon_focus(&env, old), None);

    // Back to the Global chat, which kept its conversation, via the menu.
    press(&view, cx, Region::Assistant("chats"));
    let rows = menu(&view, cx);
    assert!(rows[3].1 && !rows[1].1, "{rows:?}");
    choose(&view, cx, 1);
    wait_until(cx, "the Global chat to show again", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx) == Some(global))
    });
    assert!(panel.read_with(cx, |p, cx| p.focus_chip_shown(cx)));
    assert!(texts(&global_model, cx).is_empty());
    send(&global_model, &global_chat, cx, "global question");
    wait_answer(&global_model, cx, "from global");
    assert_eq!(
        texts(&old_model, cx),
        [
            (true, "workspace question".to_owned()),
            (false, "from the workspace".to_owned())
        ],
        "the old conversation is as it was"
    );

    // New chat makes another Global chat, listed first-come, with the shown one checked.
    panel.update(cx, |p, cx| p.new_chat(cx));
    wait_until(cx, "a second Global chat", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some_and(|s| s != global))
    });
    press(&view, cx, Region::Assistant("chats"));
    let rows = menu(&view, cx);
    assert_eq!(rows[1].0, "Mawas");
    assert_eq!(rows[2].0, "Mawas 2", "{rows:?}");
    assert!(rows[2].1, "{rows:?}");
}

#[gpui::test]
fn a_focus_is_shown_in_the_chats_menu_next_to_its_chat(cx: &mut TestAppContext) {
    let env = started();
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (global, _, _) = shown(&panel, cx);
    panel.update(cx, |p, cx| p.set_focus(Some(workspace.id), cx));
    wait_until(cx, "the Focus", |cx| {
        panel.read_with(cx, |p, cx| p.focus(cx) == Some(workspace.id))
    });
    let rows = panel.read_with(cx, |p, cx| p.chat_entries(cx));
    assert_eq!(rows[1].session, Some(global));
    assert_eq!(rows[1].label, "Mawas · Focus project");
}

#[gpui::test]
fn follow_workspace_keeps_the_old_behaviour(cx: &mut TestAppContext) {
    let env = started();
    env.mock.route("POST", PATH, reply("project reply"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open_following(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    assert_eq!(
        panel.read_with(cx, |p, cx| p.key(cx)),
        ScopeKey::Global,
        "at Home it is the Global chat"
    );
    assert!(!panel.read_with(cx, |p, _| p.global_first()));
    // The old header: the scope dropdown, no Chats menu and no Focus chip.
    assert!(drawn(&view, cx, Region::Assistant("scope")));
    assert!(!drawn(&view, cx, Region::Assistant("chats")));
    assert!(!drawn(&view, cx, Region::Assistant("focus")));
    assert!(!panel.read_with(cx, |p, cx| p.focus_chip_shown(cx)));

    // An active project is the Home of its own Mawas, as before.
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the project's assistant", |cx| {
        panel.read_with(cx, |p, cx| {
            p.key(cx) == ScopeKey::Workspace(workspace.id) && p.session(cx).is_some()
        })
    });
    let (project, model, chat) = shown(&panel, cx);
    send(&model, &chat, cx, "what is in this project?");
    wait_answer(&model, cx, "project reply");
    press(&view, cx, Region::Assistant("scope"));
    let rows = menu(&view, cx);
    let labels: Vec<&str> = rows.iter().map(|(label, _)| label.as_str()).collect();
    assert_eq!(labels, ["Global", "Workspace · project"], "{rows:?}");
    assert_eq!(daemon_focus(&env, project), None);
}

#[gpui::test]
fn changing_home_scope_in_settings_sends_home_back_to_what_it_opens(cx: &mut TestAppContext) {
    let env = started();
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let settings =
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
    settings
        .write_user(
            "assistant.home_scope",
            toml::Value::String("follow_workspace".to_owned()),
        )
        .unwrap();
    let (view, cx) = open_settings(cx, &env.daemon, settings.clone());
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the project's assistant", |cx| {
        panel.read_with(cx, |p, cx| p.key(cx) == ScopeKey::Workspace(workspace.id))
    });
    // Home now opens the Global chat, whatever the active workspace is.
    settings
        .write_user(
            "assistant.home_scope",
            toml::Value::String("global".to_owned()),
        )
        .unwrap();
    draw(cx);
    wait_until(cx, "Home to open Global", |cx| {
        draw(cx);
        panel.read_with(cx, |p, cx| {
            p.global_first() && p.key(cx) == ScopeKey::Global
        })
    });
    assert!(drawn(&view, cx, Region::Assistant("chats")));
}

#[gpui::test]
fn a_focus_is_not_a_working_directory_mawas_still_works_in_the_home_folder(
    cx: &mut TestAppContext,
) {
    let env = started();
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let folder = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .mawas_folder(MawasFocus::Global)
                .map(|f| (f.home, f.root))
        })
    };
    let before = folder(cx).expect("the Home folder is known");
    assert!(before.0, "Global works in the Home folder");
    panel.update(cx, |p, cx| p.set_focus(Some(workspace.id), cx));
    wait_until(cx, "the Focus", |cx| {
        panel.read_with(cx, |p, cx| p.focus(cx) == Some(workspace.id))
    });
    assert_eq!(folder(cx), Some(before), "the Focus moved no folder");
    assert_eq!(panel.read_with(cx, |p, cx| p.key(cx)), ScopeKey::Global);
}
