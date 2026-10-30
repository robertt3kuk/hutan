#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Mawas Home (MW-1): the page in the center with its mode chip, New chat, Stop and the Today
//! rail, and Settings → Mawas, against a real in-process daemon with the in-memory credential
//! store and a loopback provider (Mawas on the provider backend is the chat that asks to act:
//! the daemon gates every chat session). Nothing calls a model.

use crate::common;

use std::time::Duration;

use common::assistant::{
    AssistantEnv, Options, PATH, open, open_assistant, panel, pump_until, shown,
};
use common::{draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, ItemRef, PanelId};
use hutan_desktop::workbench::panels::{AssistantPanel, MawasMode, TodayKind};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_gateway::testing::Reply;
use hutan_protocol::daemon_settings::DaemonSettings;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Request, SessionInfo, SessionRename,
    SessionResult,
};
use hutan_protocol::ui::{UiActionList, UiTarget};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn started() -> AssistantEnv {
    AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    })
}

fn home() -> ItemRef {
    ItemRef::panel("assistant")
}

fn rename_request(session: SessionId, title: &str) -> Request {
    Request::SessionRename(SessionRename {
        session_id: session,
        title: title.to_owned(),
    })
}

/// A terminal session in which an agent reported itself: its requests are an agent's, and (under
/// `[control] agents = "ask"`) wait for the user like Mawas's.
fn reported_agent(env: &AssistantEnv) -> SessionInfo {
    let session = env.daemon.sh("sleep 600");
    let applied: AgentApplied = env.daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session.id,
        source: "test-hooks".into(),
        agent: "claude".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        pid: None,
        node: None,
    }));
    assert!(applied.applied);
    session
}

fn title_of(env: &AssistantEnv, session: SessionId) -> String {
    env.daemon.session(session).title
}

/// Waits until the rail lists `count` rows under Requests.
fn wait_needs(panel: &Entity<AssistantPanel>, cx: &mut VisualTestContext, count: usize) {
    wait_until(cx, &format!("{count} request(s) under Requests"), |cx| {
        panel.read_with(cx, |p, cx| p.today(cx).requests.len() == count)
    });
}

#[gpui::test]
fn the_nav_row_opens_one_home_in_the_center_with_mawas_in_its_header(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let labels = view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx)
            .into_iter()
            .map(|e| e.label.to_string())
            .collect::<Vec<_>>()
    });
    assert!(labels.iter().any(|l| l == "Mawas"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "Assistant"), "{labels:?}");

    click(&view, cx, Region::Nav("assistant"));
    let panel = panel(&view, cx);
    wait_until(cx, "Mawas to open", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some())
    });
    let count = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .all_items()
                .into_iter()
                .filter(|i| *i == home())
                .count()
        })
    };
    assert_eq!(count(cx), 1);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(home())
    );
    assert!(
        view.read_with(cx, |wb, _| wb
            .state()
            .docks
            .find(&PanelId::new("assistant")))
            .is_none(),
        "a tab of the center, in no dock"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "Mawas",
        "the tab is titled Mawas"
    );
    // The nav row again, ⌘K and the action: still one.
    click(&view, cx, Region::Nav("assistant"));
    cx.dispatch_action(actions::OpenAssistant);
    assert_eq!(count(cx), 1);

    // The header: Chats, the Focus chip, mode and ⋯, and on a page of 640 pt or more the backend
    // and New chat. Stop is there only while there is something to stop; Today is in ⋯.
    let folded = panel.read_with(cx, |p, _| p.folded());
    let mut regions = vec!["chats", "focus", "mode", "more"];
    if !folded {
        regions.extend(["backend", "new"]);
    }
    for region in regions {
        draw(cx);
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(region)))
                .is_some(),
            "{region} is in the header"
        );
    }
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("stop")))
            .is_none(),
        "nothing to stop, no Stop"
    );
    // The first chat is Mawas's, and the session is titled so.
    let (session, _, _) = shown(&panel, cx);
    assert_eq!(env.daemon.session(session).title, "Mawas");
    assert!(panel.read_with(cx, |p, _| p.rail_open()), "Today is open");
}

#[gpui::test]
fn the_mode_chip_switches_the_mode_and_says_it_applies_from_the_next_message(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the mode to be read", |cx| {
        panel.read_with(cx, |p, _| p.mode().is_some())
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.mode()),
        Some(MawasMode::Ask),
        "Ask before acting is the default"
    );
    assert!(panel.read_with(cx, |p, _| p.mode_note().is_none()));

    click(&view, cx, Region::Assistant("mode"));
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the mode menu opened");
    let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_ref()).collect();
    assert_eq!(labels, ["Ask before acting", "Advisor"]);
    assert!(entries[0].1, "the current mode is checked");
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    wait_until(cx, "the mode to switch", |cx| {
        panel.read_with(cx, |p, _| p.mode() == Some(MawasMode::Advisor))
    });
    let note = panel
        .read_with(cx, |p, _| p.mode_note().map(str::to_owned))
        .expect("the next-turn note");
    assert!(
        note.contains("Advisor") && note.contains("next message"),
        "{note}"
    );
    // The daemon has it: this is `assistant.mode`, a live setting of daemon.toml.
    let settings: DaemonSettings = env.daemon.call(Request::DaemonSettingsGet(Empty {}));
    let mode = settings
        .entries
        .iter()
        .find(|e| e.key == "assistant.mode")
        .unwrap();
    assert_eq!(mode.value, json!("advisor"));
    assert!(!mode.pending_restart);
    // The chip says so.
    draw(cx);
    click(&view, cx, Region::Assistant("mode"));
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .unwrap();
    assert!(!entries[0].1 && entries[1].1, "{entries:?}");
    // Back to Ask before acting from the same menu.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    wait_until(cx, "the mode to switch back", |cx| {
        panel.read_with(cx, |p, _| p.mode() == Some(MawasMode::Ask))
    });
}

#[gpui::test]
fn today_lists_mawas_approvals_first_and_answering_one_there_runs_the_request(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);
    assert!(
        panel.read_with(cx, |p, cx| p.today(cx).requests.is_empty()),
        "no request yet"
    );

    // Another agent asks first, then Mawas.
    let by_other = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "agent"));
    wait_needs(&panel, cx, 1);
    let by_mawas = env
        .daemon
        .call_from_in_background::<SessionResult>(mawas, rename_request(target.id, "mawas"));
    wait_needs(&panel, cx, 2);
    let today = panel.read_with(cx, |p, cx| p.today(cx));
    match (&today.requests[0].kind, &today.requests[1].kind) {
        (
            TodayKind::Approval {
                mawas: first,
                options,
                ..
            },
            TodayKind::Approval { mawas: second, .. },
        ) => {
            assert!(*first && !*second, "Mawas's request is listed first");
            let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
            assert_eq!(
                ids,
                ["allow_once", "allow_chat", "allow_workspace", "deny"],
                "the options the daemon offered"
            );
        }
        other => panic!("two approvals expected: {other:?}"),
    }
    assert!(
        today.requests[0]
            .title
            .starts_with("Mawas asks Hutan to rename"),
        "{}",
        today.requests[0].title
    );
    // It is drawn: the row and its options, and Mawas's own above the conversation, where the
    // transcript cannot hold what Hutan's gate asks.
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("asks")))
            .is_some(),
        "Mawas's request is shown in the chat too"
    );
    for region in [
        Region::AssistantItem("today-row", 0),
        Region::AssistantItem("today-option", 0),
        Region::AssistantItem("today-option", 3),
        Region::AssistantItem("today-row", 1),
    ] {
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(region))
                .is_some(),
            "{region:?} is drawn"
        );
    }

    // Allow once, from Today.
    click(&view, cx, Region::AssistantItem("today-option", 0));
    wait_until(cx, "Mawas's request to run", |cx| {
        let _ = cx;
        by_mawas.try_take().is_some()
    });
    assert_eq!(title_of(&env, target.id), "mawas");
    wait_needs(&panel, cx, 1);
    // The other agent's request stays, with the same options; deny it from Today as well.
    let today = panel.read_with(cx, |p, cx| p.today(cx));
    assert!(matches!(
        today.requests[0].kind,
        TodayKind::Approval { mawas: false, .. }
    ));
    draw(cx);
    // Deny is the third option of the other agent's request (allow once, allow for the
    // session, deny).
    click(&view, cx, Region::AssistantItem("today-option", 2));
    wait_until(cx, "the other request to be denied", |cx| {
        let _ = cx;
        by_other.try_take().is_some()
    });
    assert_eq!(title_of(&env, target.id), "mawas", "denied: not renamed");
    wait_needs(&panel, cx, 0);
    let today = panel.read_with(cx, |p, cx| p.today(cx));
    assert!(
        today.working.iter().any(|w| w.session == other.id),
        "the agent that reported itself is working: {today:?}"
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-working", 0))
        })
        .is_some()
    );
}

#[gpui::test]
fn stop_denies_what_mawas_asked_and_interrupts_its_turn(cx: &mut TestAppContext) {
    let env = started();
    // The provider streams a little and never ends: the turn stays open until it is stopped.
    env.mock.route(
        "POST",
        PATH,
        Reply::sse([format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"working on it"}}]})
        )])
        .then_hold(),
    );
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, model, chat) = shown(&panel, cx);
    let target = env.daemon.sh("sleep 600");

    // Nothing to stop yet: the button is there but does nothing.
    draw(cx);
    assert!(!panel.read_with(cx, |p, cx| p.can_stop(cx)));

    model.update(cx, |m, cx| {
        m.edit(
            |input| {
                input.clear();
                input.insert("please rename it");
            },
            cx,
        );
    });
    assert!(chat.update(cx, |v, cx| v.send(cx)), "the message was sent");
    wait_until(cx, "the turn to run", |cx| {
        panel.read_with(cx, |p, cx| p.can_stop(cx))
    });
    // Mawas asks to change something while its turn runs.
    let asked = env
        .daemon
        .call_from_in_background::<SessionResult>(mawas, rename_request(target.id, "never"));
    wait_needs(&panel, cx, 1);

    click(&view, cx, Region::Assistant("stop"));
    wait_until(cx, "the request to be denied", |cx| {
        let _ = cx;
        asked.try_take().is_some()
    });
    wait_needs(&panel, cx, 0);
    wait_until(cx, "the turn to end", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(mawas)
                .and_then(|s| s.chat.as_ref())
                .is_some_and(|chat| !chat.turn_active)
        })
    });
    assert_eq!(
        title_of(&env, target.id),
        target_title(&target),
        "denied: nothing was renamed"
    );
    assert!(panel.read_with(cx, |p, _| p.error().is_none()));
    assert!(!panel.read_with(cx, |p, cx| p.can_stop(cx)));
}

fn target_title(target: &SessionInfo) -> String {
    target.title.clone()
}

#[gpui::test]
fn new_chat_starts_another_conversation_and_shows_it(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (first, _, _) = shown(&panel, cx);
    click(&view, cx, Region::Assistant("new"));
    wait_until(cx, "the new conversation", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some_and(|s| s != first))
    });
    let (second, _, _) = shown(&panel, cx);
    assert_ne!(first, second);
    assert_eq!(env.daemon.session(second).title, "Mawas 2");
    // The palette's command does the same.
    cx.dispatch_action(actions::NewMawasChat);
    wait_until(cx, "a third conversation", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some_and(|s| s != second))
    });
}

#[gpui::test]
fn a_hidden_home_does_no_work_when_the_store_changes(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.is_visible()));

    // Another tab takes the pane: Home is hidden.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder("Elsewhere")).unwrap();
        })
    });
    draw(cx);
    wait_until(cx, "Home to be hidden", |cx| {
        draw(cx);
        !panel.read_with(cx, |p, _| p.is_visible())
    });
    let before = panel.read_with(cx, |p, _| p.render_count());

    // The daemon's state changes: sessions appear and an agent asks.
    let sessions_before = view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len());
    env.daemon.sh("sleep 600");
    env.daemon.sh("sleep 600");
    wait_until(cx, "the store to take the sessions", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().len() >= sessions_before + 2
        })
    });
    for _ in 0..5 {
        draw(cx);
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    assert_eq!(
        panel.read_with(cx, |p, _| p.render_count()),
        before,
        "a hidden page is not drawn again"
    );

    // Shown again, it draws and has caught up.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("assistant"), actions_dock())
        })
    });
    wait_until(cx, "Home to be drawn again", |cx| {
        draw(cx);
        panel.read_with(cx, |p, _| p.render_count() > before && p.is_visible())
    });
}

fn actions_dock() -> hutan_desktop::workbench::model::DockPosition {
    hutan_desktop::workbench::model::DockPosition::Left
}

#[gpui::test]
fn settings_mawas_lists_the_grants_and_revokes_one_or_all(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let target = env.daemon.sh("sleep 600");

    // Two grants: "always allow rename in this workspace", "allow stop for this chat".
    let asked = env
        .daemon
        .call_from_in_background::<SessionResult>(mawas, rename_request(target.id, "one"));
    wait_needs(&panel, cx, 1);
    draw(cx);
    click(&view, cx, Region::AssistantItem("today-option", 2));
    wait_until(cx, "the first request", |cx| {
        let _ = cx;
        asked.try_take().is_some()
    });
    let asked = env.daemon.call_from_in_background::<SessionResult>(
        mawas,
        Request::SessionRestart(hutan_protocol::messages::SessionRef {
            session_id: target.id,
        }),
    );
    wait_needs(&panel, cx, 1);
    draw(cx);
    click(&view, cx, Region::AssistantItem("today-option", 1));
    wait_until(cx, "the second request", |cx| {
        let _ = cx;
        asked.try_take().is_some()
    });

    // Settings → Mawas lists them.
    cx.dispatch_action(actions::OpenSettingsFor("assistant".into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("assistant".to_owned())
    );
    wait_until(cx, "the grants to be listed", |cx| {
        view.read_with(cx, |wb, _| wb.mawas_grants().grants.len() == 2)
    });
    let grants = view.read_with(cx, |wb, _| wb.mawas_grants().grants.clone());
    let tools: Vec<&str> = grants.iter().map(|g| g.tool.as_str()).collect();
    assert_eq!(tools, ["session.restart", "session.rename"], "newest first");
    let workspace: Option<WorkspaceId> = grants[1].workspace_id;
    assert!(workspace.is_some(), "the rename grant is the workspace's");
    assert_eq!(grants[0].chat_session_id, Some(mawas));

    // Revoke one: the daemon forgets it, the list follows.
    view.update(cx, |wb, cx| wb.revoke_mawas_grant(grants[1].id, cx));
    wait_until(cx, "the grant to be revoked", |cx| {
        view.read_with(cx, |wb, _| wb.mawas_grants().grants.len() == 1)
    });
    let listed: hutan_protocol::grants::ControlGrants = env.daemon.call(
        Request::ControlGrantsList(hutan_protocol::grants::ControlGrantsList::default()),
    );
    assert_eq!(listed.grants.len(), 1);
    assert_eq!(listed.grants[0].tool, "session.restart");
    // And all.
    view.update(cx, |wb, cx| wb.revoke_all_mawas_grants(cx));
    wait_until(cx, "every grant to be revoked", |cx| {
        view.read_with(cx, |wb, _| {
            wb.mawas_grants().grants.is_empty() && wb.mawas_grants().loaded
        })
    });
    let listed: hutan_protocol::grants::ControlGrants = env.daemon.call(
        Request::ControlGrantsList(hutan_protocol::grants::ControlGrantsList::default()),
    );
    assert!(listed.grants.is_empty());
    draw(cx);
}

#[gpui::test]
fn turning_mawas_off_hides_its_nav_row_and_closes_home(cx: &mut TestAppContext) {
    let env = started();
    let settings =
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
    let (view, cx) = open_with(cx, settings.clone(), env.daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    click(&view, cx, Region::Nav("assistant"));
    assert!(view.read_with(cx, |wb, _| wb.panel_open("assistant")));
    settings
        .write_user("assistant.enabled", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "Mawas to go", |cx| {
        view.read_with(cx, |wb, cx| {
            !wb.assistant_enabled()
                && !wb.panel_open("assistant")
                && wb.nav_entries(cx).iter().all(|e| e.key != "assistant")
        })
    });
}

/// Below 640 pt the header folds into ⋯, Today leaves the page, and the composer keeps its
/// width (it once wrapped letter by letter in a 470 pt pane).
#[gpui::test]
fn below_640_pt_the_header_folds_today_leaves_and_the_composer_keeps_its_width(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    // What is proved is the page in a narrow center, with the panel pushed in beside it (EX-2:
    // a peek would leave the center alone).
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("layout.rail_peek", toml::Value::Boolean(false))
        .unwrap();
    click(&view, cx, Region::Nav("assistant"));
    let panel = panel(&view, cx);
    wait_until(cx, "Mawas to open", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some())
    });
    let painted = |cx: &VisualTestContext, what: &'static str| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(what)))
            .is_some()
    };

    // Room: the backend, New chat and the Today column beside the conversation.
    wait_until(cx, "the unfolded header", |cx| {
        !panel.read_with(cx, |p, _| p.folded())
    });
    wait_until(cx, "the header controls", |cx| {
        painted(cx, "backend") && painted(cx, "new") && painted(cx, "today")
    });
    assert!(!painted(cx, "backend-line"), "the chip says it, no line");

    // No room (a 1000 × 700 pt window with the right panel open at 480 pt): scope, mode and ⋯;
    // no Today column; a composer of a usable width.
    cx.simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(700.0)));
    cx.dispatch_action(actions::OpenPanel("agents".into()));
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Right, 480.0));
    });
    wait_until(cx, "the folded header", |cx| {
        panel.read_with(cx, |p, _| p.folded())
    });
    wait_until(cx, "the folded page to be drawn", |cx| {
        !painted(cx, "backend") && !painted(cx, "new") && !painted(cx, "today")
    });
    for what in ["chats", "focus", "mode", "more"] {
        assert!(painted(cx, what), "{what} stays in the folded header");
    }
    // What Mawas runs on still shows, as a line under the header.
    wait_until(cx, "the backend line", |cx| painted(cx, "backend-line"));
    let label = panel.read_with(cx, |p, cx| p.backend_label(cx));
    assert!(label.contains(" · "), "backend · model: {label:?}");
    let page = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("header")))
        .expect("the header");
    wait_until(cx, "the composer", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("composer")))
            .is_some()
    });
    let composer = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("composer")))
        .unwrap();
    assert!(
        composer.size.width >= gpui::px(200.0) && composer.right() <= page.right(),
        "the composer wraps per character in {composer:?}"
    );

    // ⋯ holds what folded: New chat, Stop, Today and the backend.
    click(&view, cx, Region::Assistant("more"));
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the ⋯ menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(
        &labels[..3],
        ["New chat", "Stop Mawas", "Today"],
        "{labels:?}"
    );
    assert!(labels[3].starts_with("Backend: "), "{labels:?}");
    // Today is a page of its own from there, and the conversation comes back.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    wait_until(cx, "Today in place of the conversation", |cx| {
        painted(cx, "today") && panel.read_with(cx, |p, _| p.today_only())
    });
    let today = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Assistant("today")))
        .unwrap();
    assert!(today.size.width >= gpui::px(300.0), "{today:?}");
    click(&view, cx, Region::Assistant("more"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    wait_until(cx, "the conversation again", |cx| {
        !panel.read_with(cx, |p, _| p.today_only()) && !painted(cx, "today")
    });
}

// ---- MAWAS-LAYOUT: the conversation's width and the Today rail's switch -------------------------

/// A window with room for the widest column and the rail beside it, Home open and drawn.
fn open_wide<'a>(
    cx: &'a mut TestAppContext,
    env: &AssistantEnv,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    Entity<AssistantPanel>,
) {
    let (view, cx) = open(cx, &env.daemon);
    cx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the column and its handles", |cx| {
        painted(&view, cx, "column")
            && painted(&view, cx, "chat-edge-left")
            && painted(&view, cx, "chat-edge-right")
    });
    (view, cx, panel)
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, name: &'static str) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(name)))
        .is_some()
}

fn bounds_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    name: &'static str,
) -> gpui::Bounds<gpui::Pixels> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(name)))
        .unwrap_or_else(|| panic!("{name} is drawn"))
}

fn column_width(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> f32 {
    f32::from(bounds_of(view, cx, "column").size.width)
}

fn assert_close(got: f32, want: f32, what: &str) {
    assert!((got - want).abs() <= 0.75, "{what}: {got} is not {want}");
}

/// `assistant.chat_width_rem` as `settings.toml` holds it.
fn written_width(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<f64> {
    view.read_with(cx, |wb, _| {
        wb.settings_store().layer_value(
            hutan_settings::SettingsLayer::User,
            "assistant.chat_width_rem",
        )
    })
    .map(|value| {
        value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64))
            .expect("a number")
    })
}

fn press(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, Modifiers::none());
}

/// Moves with the button held, in small steps, to `to`.
fn drag_to(
    cx: &mut VisualTestContext,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
) {
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let at = gpui::point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(at, gpui::MouseButton::Left, Modifiers::none());
    }
}

fn release(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, Modifiers::none());
}

/// A double click: a press and release with `click_count` 1, then another with 2.
fn double_click_at(cx: &mut VisualTestContext, at: gpui::Point<gpui::Pixels>) {
    use gpui::{InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent};
    let modifiers = Modifiers::none();
    cx.update(|window, app| {
        window.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
            app,
        );
        for count in [1, 2] {
            window.dispatch_event(
                MouseDownEvent {
                    position: at,
                    modifiers,
                    button: MouseButton::Left,
                    click_count: count,
                    first_mouse: false,
                }
                .to_platform_input(),
                app,
            );
            window.dispatch_event(
                MouseUpEvent {
                    position: at,
                    modifiers,
                    button: MouseButton::Left,
                    click_count: count,
                }
                .to_platform_input(),
                app,
            );
        }
    });
    cx.run_until_parked();
    draw(cx);
}

#[gpui::test]
fn dragging_an_edge_widens_the_column_symmetrically_and_writes_once_on_release(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    // The default: 46 rem, nothing in the file.
    assert_close(column_width(&view, cx), 736.0, "the default column");
    assert_eq!(written_width(&view, cx), None);
    assert_close(panel.read_with(cx, |p, _| p.chat_width_rem()), 46.0, "rem");

    // 40 pt on the right edge: 80 pt wider (the column stays centered), live, and not written.
    let edge = bounds_of(&view, cx, "chat-edge-right").center();
    press(cx, edge);
    drag_to(cx, edge, edge + gpui::point(gpui::px(40.0), gpui::px(0.0)));
    assert!(panel.read_with(cx, |p, _| p.resizing_chat()));
    assert_close(column_width(&view, cx), 816.0, "the column while dragged");
    assert_eq!(
        written_width(&view, cx),
        None,
        "nothing is written mid-drag"
    );
    release(cx, edge + gpui::point(gpui::px(40.0), gpui::px(0.0)));
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.resizing_chat()));
    assert_eq!(
        written_width(&view, cx),
        Some(51.0),
        "written on release, in rem"
    );
    assert_close(column_width(&view, cx), 816.0, "the column after");
    // What the column holds follows it: the composer is no longer held to the 720 pt reading
    // width of a chat's own tab.
    let composer = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("composer")))
        .expect("the composer is drawn");
    assert!(
        composer.size.width > gpui::px(760.0) && composer.size.width <= gpui::px(816.0),
        "the composer fills the widened column: {composer:?}"
    );

    // The left edge works the same way: 40 pt inward is 80 pt narrower.
    let edge = bounds_of(&view, cx, "chat-edge-left").center();
    press(cx, edge);
    drag_to(cx, edge, edge + gpui::point(gpui::px(40.0), gpui::px(0.0)));
    assert_close(column_width(&view, cx), 736.0, "the column while dragged");
    assert_eq!(
        written_width(&view, cx),
        Some(51.0),
        "still the old value mid-drag"
    );
    release(cx, edge + gpui::point(gpui::px(40.0), gpui::px(0.0)));
    draw(cx);
    assert_eq!(written_width(&view, cx), Some(46.0));

    // A press that did not move writes nothing: the file keeps what it had.
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.chat_width_rem", toml::Value::Float(52.0))
        .unwrap();
    draw(cx);
    let edge = bounds_of(&view, cx, "chat-edge-right").center();
    press(cx, edge);
    release(cx, edge);
    assert_eq!(written_width(&view, cx), Some(52.0));
    // A width is rounded to half a rem when written: 52 rem + 2 · 3 pt = 52.375 rem → 52.5.
    press(cx, edge);
    drag_to(cx, edge, edge + gpui::point(gpui::px(3.0), gpui::px(0.0)));
    release(cx, edge + gpui::point(gpui::px(3.0), gpui::px(0.0)));
    assert_eq!(written_width(&view, cx), Some(52.5));
}

#[gpui::test]
fn escape_during_a_drag_restores_the_width_and_writes_nothing(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    let edge = bounds_of(&view, cx, "chat-edge-left").center();
    press(cx, edge);
    drag_to(cx, edge, edge - gpui::point(gpui::px(40.0), gpui::px(0.0)));
    assert_close(column_width(&view, cx), 816.0, "widened while dragged");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.resizing_chat()));
    assert_close(column_width(&view, cx), 736.0, "back to what it was");
    // The pointer is still down and moves on, then lets go: nothing follows, nothing is written.
    drag_to(cx, edge, edge - gpui::point(gpui::px(80.0), gpui::px(0.0)));
    release(cx, edge - gpui::point(gpui::px(80.0), gpui::px(0.0)));
    draw(cx);
    assert_close(
        column_width(&view, cx),
        736.0,
        "unchanged after the release",
    );
    assert_eq!(written_width(&view, cx), None);
}

#[gpui::test]
fn a_double_click_on_an_edge_removes_the_key(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, _panel) = open_wide(cx, &env);
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.chat_width_rem", toml::Value::Float(60.0))
        .unwrap();
    wait_until(cx, "the wider column", |cx| {
        (column_width(&view, cx) - 960.0).abs() < 1.0
    });
    let edge = bounds_of(&view, cx, "chat-edge-right").center();
    double_click_at(cx, edge);
    assert_eq!(written_width(&view, cx), None, "the key is gone");
    assert_close(column_width(&view, cx), 736.0, "the default width again");
    let text = std::fs::read_to_string(env.daemon.home().join("settings.toml")).unwrap_or_default();
    assert!(!text.contains("chat_width_rem"), "{text}");
}

#[gpui::test]
fn the_width_is_held_within_30_rem_90_rem_and_the_page(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, _panel) = open_wide(cx, &env);
    let store = view.read_with(cx, |wb, _| wb.settings_store().clone());

    // As the file says it: out of range reads as the nearest end.
    store
        .write_user("assistant.chat_width_rem", toml::Value::Float(200.0))
        .unwrap();
    wait_until(cx, "90 rem", |cx| {
        (column_width(&view, cx) - 1440.0).abs() < 1.0
    });
    store
        .write_user("assistant.chat_width_rem", toml::Value::Integer(5))
        .unwrap();
    wait_until(cx, "30 rem", |cx| {
        (column_width(&view, cx) - 480.0).abs() < 1.0
    });
    store
        .remove(
            hutan_settings::SettingsLayer::User,
            "assistant",
            "chat_width_rem",
        )
        .unwrap();

    // Dragged: never below 30 rem, never past the page minus its 12 pt margins.
    cx.simulate_resize(gpui::size(gpui::px(2000.0), gpui::px(1000.0)));
    draw(cx);
    wait_until(cx, "the narrower page", |cx| {
        bounds_of(&view, cx, "conversation").size.width < gpui::px(1450.0)
    });
    let page = bounds_of(&view, cx, "conversation");
    let edge = bounds_of(&view, cx, "chat-edge-right").center();
    let far = edge + gpui::point(gpui::px(4000.0), gpui::px(0.0));
    press(cx, edge);
    drag_to(cx, edge, far);
    let column = bounds_of(&view, cx, "column");
    assert!(
        column.left() >= page.left() + gpui::px(11.5)
            && column.right() <= page.right() - gpui::px(11.5),
        "the column {column:?} keeps its margins in {page:?}"
    );
    assert_close(
        f32::from(column.size.width),
        f32::from(page.size.width) - 24.0,
        "as wide as the page allows",
    );
    release(cx, far);
    let rem = written_width(&view, cx).expect("written");
    assert!((30.0..=90.0).contains(&rem), "{rem}");
    assert!(
        rem * 16.0 <= f64::from(f32::from(page.size.width)) - 24.0 + 8.0,
        "{rem} rem fits the page"
    );

    // And the other way, to the narrowest.
    let edge = bounds_of(&view, cx, "chat-edge-right").center();
    let far = edge - gpui::point(gpui::px(4000.0), gpui::px(0.0));
    press(cx, edge);
    drag_to(cx, edge, far);
    assert_close(column_width(&view, cx), 480.0, "30 rem");
    release(cx, far);
    assert_eq!(written_width(&view, cx), Some(30.0));
}

#[gpui::test]
fn below_640_pt_the_column_has_no_handles(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("layout.rail_peek", toml::Value::Boolean(false))
        .unwrap();
    click(&view, cx, Region::Nav("assistant"));
    let panel = panel(&view, cx);
    wait_until(cx, "Mawas to open", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx).is_some())
    });
    wait_until(cx, "the handles of a roomy page", |cx| {
        !panel.read_with(cx, |p, _| p.folded())
            && painted(&view, cx, "chat-edge-left")
            && painted(&view, cx, "chat-edge-right")
    });
    cx.simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(700.0)));
    cx.dispatch_action(actions::OpenPanel("agents".into()));
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Right, 480.0));
    });
    wait_until(cx, "the folded page", |cx| {
        panel.read_with(cx, |p, _| p.folded())
            && !painted(&view, cx, "chat-edge-left")
            && !painted(&view, cx, "chat-edge-right")
    });
    assert!(
        !painted(&view, cx, "today-toggle"),
        "no Today button either"
    );
}

#[gpui::test]
fn another_window_follows_the_written_width(cx: &mut TestAppContext) {
    let env = started();
    let store =
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
    let open_window = |cx: &mut TestAppContext| {
        let (view, vcx) = open_with(cx, store.clone(), env.daemon.connection(), None);
        wait_until(vcx, "the first synchronisation", |cx| {
            view.read_with(cx, |wb, cx| {
                wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
            })
        });
        vcx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
        let panel = open_assistant(&view, vcx);
        wait_until(vcx, "the column", |cx| painted(&view, cx, "column"));
        let handle = vcx.update(|window, _| window.window_handle());
        (view, panel, handle)
    };
    let (a, _panel_a, handle_a) = open_window(cx);
    let (b, _panel_b, handle_b) = open_window(cx);
    let mut window_a = gpui::VisualTestContext::from_window(handle_a, cx);
    assert_close(column_width(&a, &mut window_a), 736.0, "A");
    drop(window_a);

    // The first window's drag release writes it; the second window's column follows.
    store
        .write_user("assistant.chat_width_rem", toml::Value::Float(58.0))
        .unwrap();
    let mut window_a = gpui::VisualTestContext::from_window(handle_a, cx);
    wait_until(&mut window_a, "window A to follow", |cx| {
        (column_width(&a, cx) - 928.0).abs() < 1.0
    });
    drop(window_a);
    let mut window_b = gpui::VisualTestContext::from_window(handle_b, cx);
    wait_until(&mut window_b, "window B to follow", |cx| {
        (column_width(&b, cx) - 928.0).abs() < 1.0
    });
}

/// How many sessions of `today` have a request, counted here from the rows.
fn sessions_asking(panel: &Entity<AssistantPanel>, cx: &VisualTestContext) -> usize {
    panel.read_with(cx, |p, cx| {
        let mut sessions: Vec<SessionId> = p.today(cx).requests.iter().map(|r| r.session).collect();
        sessions.sort();
        sessions.dedup();
        sessions.len()
    })
}

#[gpui::test]
fn the_today_button_hides_the_rail_for_good_and_its_badge_counts_sessions_with_a_request(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    let (mawas, _, _) = shown(&panel, cx);
    wait_until(cx, "Today beside the conversation", |cx| {
        painted(&view, cx, "today") && painted(&view, cx, "today-toggle")
    });
    assert!(panel.read_with(cx, |p, _| p.rail_open()));
    assert_eq!(panel.read_with(cx, |p, cx| p.today_badge(cx)), 0);
    let user = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .layer_value(hutan_settings::SettingsLayer::User, "assistant.rail_open")
        })
    };
    assert_eq!(user(&view, cx), None, "the default is not written");

    // Hide it: the column goes, the setting says so, and the column widens into its room.
    let before = column_width(&view, cx);
    click(&view, cx, Region::Assistant("today-toggle"));
    wait_until(cx, "Today hidden", |cx| !painted(&view, cx, "today"));
    assert!(!panel.read_with(cx, |p, _| p.rail_open()));
    assert_eq!(user(&view, cx), Some(toml::Value::Boolean(false)));
    assert!(column_width(&view, cx) >= before);
    assert!(
        !painted(&view, cx, "today-badge"),
        "nothing waits: no badge"
    );

    // Requests arrive: two sessions ask, one of them twice. The rail stays hidden; the badge
    // is the number of sessions.
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);
    let _first = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "one"));
    let _second = env
        .daemon
        .call_from_in_background::<SessionResult>(mawas, rename_request(target.id, "two"));
    wait_needs(&panel, cx, 2);
    let _third = env
        .daemon
        .call_from_in_background::<SessionResult>(mawas, rename_request(target.id, "three"));
    wait_needs(&panel, cx, 3);
    wait_until(cx, "the badge", |cx| painted(&view, cx, "today-badge"));
    assert_eq!(sessions_asking(&panel, cx), 2);
    assert_eq!(panel.read_with(cx, |p, cx| p.today_badge(cx)), 2);
    assert!(
        !painted(&view, cx, "today"),
        "a request never reopens Today"
    );
    assert!(!panel.read_with(cx, |p, _| p.rail_open()));

    // Show it again: the badge goes with the button's other state.
    click(&view, cx, Region::Assistant("today-toggle"));
    wait_until(cx, "Today shown", |cx| painted(&view, cx, "today"));
    assert_eq!(user(&view, cx), Some(toml::Value::Boolean(true)));
    assert_eq!(panel.read_with(cx, |p, cx| p.today_badge(cx)), 0);
    assert!(!painted(&view, cx, "today-badge"));
    click(&view, cx, Region::Assistant("today-toggle"));
    wait_until(cx, "Today hidden again", |cx| !painted(&view, cx, "today"));
    assert_eq!(panel.read_with(cx, |p, cx| p.today_badge(cx)), 2);
}

#[gpui::test]
fn a_hidden_today_stays_hidden_in_a_new_window_and_the_more_menu_and_palette_agree(
    cx: &mut TestAppContext,
) {
    let env = started();
    {
        let (view, cx, _panel) = open_wide(cx, &env);
        // ⋯ → Hide Today is the same switch as the button.
        click(&view, cx, Region::Assistant("more"));
        let labels: Vec<String> = view
            .read_with(cx, |wb, _| wb.context_menu_entries())
            .expect("the ⋯ menu")
            .into_iter()
            .map(|(label, _)| label.to_string())
            .collect();
        let index = labels
            .iter()
            .position(|l| l == "Hide Today")
            .unwrap_or_else(|| panic!("{labels:?}"));
        view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
        wait_until(cx, "Today hidden by the menu", |cx| {
            !painted(&view, cx, "today")
        });
    }
    // The palette command brings it back, and hides it again.
    let (view, cx, panel) = {
        let store =
            common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())));
        assert!(
            !store
                .get::<hutan_desktop::settings::AssistantSettings>()
                .rail_open,
            "the file says hidden"
        );
        let (view, vcx) = open_with(cx, store, env.daemon.connection(), None);
        wait_until(vcx, "the first synchronisation", |cx| {
            view.read_with(cx, |wb, cx| {
                wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
            })
        });
        vcx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
        let panel = open_assistant(&view, vcx);
        (view, vcx, panel)
    };
    wait_until(cx, "the new window's Home", |cx| {
        painted(&view, cx, "column")
    });
    assert!(
        !panel.read_with(cx, |p, _| p.rail_open()),
        "a new window starts hidden"
    );
    assert!(!painted(&view, cx, "today"));
    cx.dispatch_action(actions::ToggleMawasToday);
    wait_until(cx, "Today shown by the palette command", |cx| {
        painted(&view, cx, "today")
    });
    assert!(panel.read_with(cx, |p, _| p.rail_open()));
    cx.dispatch_action(actions::ToggleMawasToday);
    wait_until(cx, "Today hidden by the palette command", |cx| {
        !painted(&view, cx, "today")
    });
}

#[gpui::test]
fn the_palette_steps_resets_and_lists_the_layout_commands(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, _panel) = open_wide(cx, &env);
    // They are in the palette and in `ui.actions`.
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    for title in [
        "Mawas: Wider Chat",
        "Mawas: Narrower Chat",
        "Mawas: Reset Chat Width",
        "Mawas: Toggle Today",
    ] {
        assert!(titles.iter().any(|t| t == title), "{title} in {titles:?}");
    }
    pump_until(cx, "the window to publish its actions", |_| {
        env.daemon
            .try_call::<UiActionList>(Request::UiActions(UiTarget::default()))
            .is_ok_and(|list| {
                [
                    "WiderMawasChat",
                    "NarrowerMawasChat",
                    "ResetMawasChatWidth",
                    "ToggleMawasToday",
                ]
                .iter()
                .all(|name| {
                    list.actions
                        .iter()
                        .any(|a| a.id == format!("workbench::{name}"))
                })
            })
    });

    // ±4 rem; clamped; reset removes the key.
    cx.dispatch_action(actions::WiderMawasChat);
    assert_eq!(written_width(&view, cx), Some(50.0));
    wait_until(cx, "50 rem", |cx| {
        (column_width(&view, cx) - 800.0).abs() < 1.0
    });
    cx.dispatch_action(actions::NarrowerMawasChat);
    cx.dispatch_action(actions::NarrowerMawasChat);
    assert_eq!(written_width(&view, cx), Some(42.0));
    for _ in 0..5 {
        cx.dispatch_action(actions::NarrowerMawasChat);
    }
    assert_eq!(written_width(&view, cx), Some(30.0), "never below 30 rem");
    for _ in 0..20 {
        cx.dispatch_action(actions::WiderMawasChat);
    }
    assert_eq!(written_width(&view, cx), Some(90.0), "never above 90 rem");
    cx.dispatch_action(actions::ResetMawasChatWidth);
    assert_eq!(written_width(&view, cx), None);
    wait_until(cx, "the default width", |cx| {
        (column_width(&view, cx) - 736.0).abs() < 1.0
    });
}

// ---- acting on another agent (MAWAS-GATE, protocol 0.62) -----------------------------------------

/// What Mawas says to another agent in these tests.
const NUDGE: &str = "Please run the formatter before you commit.";

/// Waits until `region` is drawn and returns where.
fn drawn(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    pump_until(cx, &format!("{region:?} to be drawn"), |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .is_some()
    });
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
}

#[gpui::test]
fn the_card_for_a_message_shows_the_agent_and_every_word_and_the_message_arrives_from_mawas(
    cx: &mut TestAppContext,
) {
    use hutan_chat_view::Body;
    use hutan_desktop::workbench::chat::LoadStatus;
    use hutan_protocol::chat::{ChatCreate, ChatInterrupt, ChatQueued, ChatSendNow, ControlTarget};

    let env = started();
    // The target's first turn is held; every later one answers at once.
    env.mock.route(
        "POST",
        PATH,
        Reply::sse([format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"on it"}}]})
        )])
        .then_hold(),
    );
    env.mock
        .route("POST", PATH, common::assistant::reply("noted"));
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let api = env
        .daemon
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: env.base.clone(),
            agent: "direct".into(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some("api".into()),
            mode: None,
            provider_id: Some("mock".into()),
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .session;
    env.daemon
        .call::<hutan_protocol::chat::ChatSent>(Request::ChatSend(
            hutan_protocol::chat::ChatSend {
                session_id: api.id,
                text: "first".into(),
                attachments: Vec::new(),
            },
        ));
    pump_until(cx, "its turn to run", |_| {
        env.daemon
            .session(api.id)
            .chat
            .as_ref()
            .is_some_and(|chat| chat.turn_active)
    });

    // Mawas asks to queue a message for it: the rail's card names the agent and shows all of it,
    // and offers *Allow once*, *Allow for this chat and this agent* and *Deny* — nothing wider.
    let asking = env.daemon.call_from_in_background::<ChatQueued>(
        mawas,
        Request::ChatQueueAdd(hutan_protocol::chat::ChatQueueAdd {
            session_id: api.id,
            text: NUDGE.into(),
        }),
    );
    wait_needs(&panel, cx, 1);
    let today = panel.read_with(cx, |p, cx| p.today(cx));
    match &today.requests[0].kind {
        TodayKind::Approval {
            mawas: true,
            target,
            options,
            ..
        } => {
            assert_eq!(
                target.as_ref(),
                Some(&ControlTarget {
                    session_id: api.id,
                    title: "api".into(),
                    text: Some(NUDGE.into()),
                })
            );
            let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
            assert_eq!(
                labels,
                ["Allow once", "Allow for this chat and this agent", "Deny"]
            );
        }
        other => panic!("Mawas's approval expected: {other:?}"),
    }
    drawn(&view, cx, Region::AssistantItem("today-target", 0));
    drawn(&view, cx, Region::AssistantItem("today-text", 0));
    drawn(&view, cx, Region::AssistantItem("today-option", 2));
    // Allow once, from the card.
    let allow = drawn(&view, cx, Region::AssistantItem("today-option", 0));
    common::click_at(cx, allow.center());
    wait_until(cx, "the message to be queued", |cx| {
        let _ = cx;
        asking.try_take().is_some()
    });
    // Open the target to see the queue (Home stays behind it: the rail is not drawn then).
    view.update_in(cx, |wb, window, cx| wb.open_session(api.id, window, cx));
    wait_until(cx, "the target chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(api.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(api.id))
        .unwrap();

    // In the target chat the queued message says whose it is, and nothing was sent into the turn.
    pump_until(cx, "the queue to show it", |cx| {
        model.read_with(cx, |m, _| {
            m.queued().len() == 1 && m.queued()[0].origin.is_some()
        })
    });
    drawn(&view, cx, Region::ChatQueue("origin", 0));
    // Stop keeping the queue, then Send now: it goes, as Mawas's, and the turn that answers it
    // is the one the transcript labels.
    let _: hutan_protocol::messages::Empty =
        env.daemon.call(Request::ChatInterrupt(ChatInterrupt {
            session_id: api.id,
            keep_queue: true,
        }));
    pump_until(cx, "the turn to stop", |cx| {
        model.read_with(cx, |m, _| !m.transcript().turn_active())
    });
    let item = model.read_with(cx, |m, _| m.queued()[0].id);
    let _: hutan_protocol::chat::ChatSentNow = env.daemon.call(Request::ChatSendNow(ChatSendNow {
        session_id: api.id,
        text: None,
        item_id: Some(item),
    }));
    let (index, user) = {
        let mut found = None;
        pump_until(cx, "the message to be in the transcript", |cx| {
            found = model.read_with(cx, |m, _| {
                m.transcript()
                    .entries()
                    .iter()
                    .enumerate()
                    .find_map(|(index, entry)| match &entry.body {
                        Body::User(message) if message.text.starts_with(NUDGE) => {
                            Some((index, message.item))
                        }
                        _ => None,
                    })
            });
            found.is_some()
        });
        found.unwrap()
    };
    let origin = model
        .read_with(cx, |m, _| user.and_then(|item| m.origin_of(item).cloned()))
        .expect("the delivered message carries its origin");
    assert_eq!(origin.session_id, mawas);
    assert_eq!(origin.approval, "allow_once");
    // The label sits on the message (`ChatPart(index, 200)` is the *from Mawas* label).
    drawn(&view, cx, Region::ChatPart(index, 200));
    // The user's own message has none.
    let own = model.read_with(cx, |m, _| {
        m.transcript()
            .entries()
            .iter()
            .find_map(|entry| match &entry.body {
                Body::User(message) if message.text == "first" => message.item,
                _ => None,
            })
    });
    assert!(
        own.is_some_and(|item| model.read_with(cx, |m, _| m.origin_of(item).is_none())),
        "the user's own message is not labelled"
    );
}

// ---- AGENTS-BOARD-B: the rail's tabs (Now, Memory) and its width --------------------------------

fn rail_width(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> f32 {
    f32::from(bounds_of(view, cx, "today").size.width)
}

/// `assistant.rail_width_rem` as `settings.toml` holds it.
fn written_rail_width(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<f64> {
    view.read_with(cx, |wb, _| {
        wb.settings_store().layer_value(
            hutan_settings::SettingsLayer::User,
            "assistant.rail_width_rem",
        )
    })
    .map(|value| {
        value
            .as_float()
            .or_else(|| value.as_integer().map(|i| i as f64))
            .expect("a number")
    })
}

fn written_tab(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .layer_value(hutan_settings::SettingsLayer::User, "assistant.rail_tab")
    })
    .and_then(|value| value.as_str().map(str::to_owned))
}

fn click_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, name: &'static str) {
    let at = bounds_of(view, cx, name).center();
    cx.simulate_click(at, Modifiers::none());
}

#[gpui::test]
fn the_rail_has_a_now_and_a_memory_tab_and_remembers_the_one_left_open(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, _panel) = open_wide(cx, &env);
    // It opens on Now: the board's own section, not the memory's.
    wait_until(cx, "the tabs", |cx| {
        painted(&view, cx, "today-tab-now") && painted(&view, cx, "today-tab-memory")
    });
    assert!(
        !painted(&view, cx, "today-memory"),
        "the link to the Memory page is on the Memory tab"
    );
    assert_eq!(
        written_tab(&view, cx),
        None,
        "nothing is written by looking"
    );

    click_region(&view, cx, "today-tab-memory");
    wait_until(cx, "the Memory tab", |cx| {
        painted(&view, cx, "today-memory")
    });
    assert_eq!(written_tab(&view, cx).as_deref(), Some("memory"));

    // Another page (or window) starts where this one left it.
    click_region(&view, cx, "today-tab-now");
    wait_until(cx, "the Now tab again", |cx| {
        !painted(&view, cx, "today-memory")
    });
    assert_eq!(written_tab(&view, cx).as_deref(), Some("now"));
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.rail_tab", toml::Value::String("memory".into()))
        .unwrap();
    wait_until(cx, "the file's tab", |cx| {
        painted(&view, cx, "today-memory")
    });
}

#[gpui::test]
fn idle_agents_sit_under_one_folded_line_with_what_their_state_is_worth(cx: &mut TestAppContext) {
    let env = started();
    let session = env.daemon.sh("sleep 600");
    let applied: AgentApplied = env.daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session.id,
        source: "test-hooks".into(),
        agent: "claude".into(),
        state: Some(AgentState::Idle),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        pid: None,
        node: None,
    }));
    assert!(applied.applied);
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "the idle agent", |cx| {
        panel.read_with(cx, |p, cx| {
            p.today(cx).idle.iter().any(|row| row.session == session.id)
        })
    });
    // One line, folded: Idle agents are not news.
    wait_until(cx, "the Idle line", |cx| painted(&view, cx, "today-idle"));
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-idle-row", 0))
        })
        .is_none(),
        "the rows are folded"
    );
    click_region(&view, cx, "today-idle");
    wait_until(cx, "the idle row", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-idle-row", 0))
        })
        .is_some()
    });
    // A terminal agent's own report: its state, not a journal.
    let row = panel.read_with(cx, |p, cx| p.today(cx).idle[0].record.managed);
    assert!(!row);
    click_region(&view, cx, "today-idle");
    wait_until(cx, "the rows folded again", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-idle-row", 0))
        })
        .is_none()
    });
}

#[gpui::test]
fn the_rail_is_340_pt_wide_and_dragging_its_border_writes_once_on_release(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "the rail's border", |cx| {
        painted(&view, cx, "rail-edge")
    });
    assert_close(rail_width(&view, cx), 340.0, "the default rail");
    assert_eq!(written_rail_width(&view, cx), None);

    // Start from a whole number of rem so that the grain of what is written is not in the way.
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.rail_width_rem", toml::Value::Float(22.0))
        .unwrap();
    wait_until(cx, "352 pt", |cx| {
        (rail_width(&view, cx) - 352.0).abs() < 1.0
    });

    // 32 pt to the left: the column is 32 pt wider, live, and nothing is written.
    let edge = bounds_of(&view, cx, "rail-edge").center();
    press(cx, edge);
    drag_to(cx, edge, edge - gpui::point(gpui::px(32.0), gpui::px(0.0)));
    assert!(panel.read_with(cx, |p, _| p.resizing_rail()));
    assert!(!panel.read_with(cx, |p, _| p.resizing_chat()));
    assert_close(rail_width(&view, cx), 384.0, "the rail while dragged");
    assert_eq!(
        written_rail_width(&view, cx),
        Some(22.0),
        "the old value mid-drag"
    );
    release(cx, edge - gpui::point(gpui::px(32.0), gpui::px(0.0)));
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.resizing_rail()));
    assert_eq!(
        written_rail_width(&view, cx),
        Some(24.0),
        "written on release, in rem"
    );
    assert_close(rail_width(&view, cx), 384.0, "the rail after");
    assert_eq!(
        written_width(&view, cx),
        None,
        "the conversation's width is untouched"
    );
}

#[gpui::test]
fn escape_cancels_a_rail_drag_and_a_double_click_on_its_border_removes_the_key(
    cx: &mut TestAppContext,
) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "the rail's border", |cx| {
        painted(&view, cx, "rail-edge")
    });
    let edge = bounds_of(&view, cx, "rail-edge").center();
    press(cx, edge);
    drag_to(cx, edge, edge - gpui::point(gpui::px(40.0), gpui::px(0.0)));
    assert_close(rail_width(&view, cx), 380.0, "widened while dragged");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!panel.read_with(cx, |p, _| p.resizing_rail()));
    assert_close(rail_width(&view, cx), 340.0, "back to what it was");
    release(cx, edge - gpui::point(gpui::px(40.0), gpui::px(0.0)));
    draw(cx);
    assert_eq!(written_rail_width(&view, cx), None);

    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.rail_width_rem", toml::Value::Float(28.0))
        .unwrap();
    wait_until(cx, "448 pt", |cx| {
        (rail_width(&view, cx) - 448.0).abs() < 1.0
    });
    let edge = bounds_of(&view, cx, "rail-edge").center();
    double_click_at(cx, edge);
    assert_eq!(written_rail_width(&view, cx), None, "the key is gone");
    assert_close(rail_width(&view, cx), 340.0, "the default width again");
}

#[gpui::test]
fn the_rail_width_is_held_within_17_5_and_32_5_rem(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, _panel) = open_wide(cx, &env);
    let store = view.read_with(cx, |wb, _| wb.settings_store().clone());
    store
        .write_user("assistant.rail_width_rem", toml::Value::Float(200.0))
        .unwrap();
    wait_until(cx, "520 pt", |cx| {
        (rail_width(&view, cx) - 520.0).abs() < 1.0
    });
    store
        .write_user("assistant.rail_width_rem", toml::Value::Integer(2))
        .unwrap();
    wait_until(cx, "280 pt", |cx| {
        (rail_width(&view, cx) - 280.0).abs() < 1.0
    });

    // A drag does not go past the end either.
    store
        .write_user("assistant.rail_width_rem", toml::Value::Float(32.0))
        .unwrap();
    wait_until(cx, "512 pt", |cx| {
        (rail_width(&view, cx) - 512.0).abs() < 1.0
    });
    let edge = bounds_of(&view, cx, "rail-edge").center();
    press(cx, edge);
    drag_to(cx, edge, edge - gpui::point(gpui::px(200.0), gpui::px(0.0)));
    assert_close(rail_width(&view, cx), 520.0, "held at the widest");
    release(cx, edge - gpui::point(gpui::px(200.0), gpui::px(0.0)));
    assert_eq!(written_rail_width(&view, cx), Some(32.5));
}

/// Waits until Mawas's `ask.suggest` came back with the suggestion.
fn allow_her_suggestion_done(
    pending: &common::control::Pending<hutan_protocol::suggest::AskSuggested>,
    cx: &mut VisualTestContext,
) {
    wait_until(cx, "her suggestion to be accepted", |_| {
        pending.try_take().is_some_and(|result| result.is_ok())
    });
}

/// Answers, as the user, the permission request Mawas's own `ask.suggest` raised (the gate asks
/// for it like for any change), and waits for it to be gone.
fn allow_her_suggestion(env: &AssistantEnv, cx: &mut VisualTestContext, mawas: SessionId) {
    use hutan_protocol::chat::{ApprovalList, ApprovalListRequest, ApprovalRespond};

    let mut card = None;
    pump_until(cx, "Mawas's gate card", |_| {
        let list: ApprovalList = env
            .daemon
            .call(Request::ApprovalList(ApprovalListRequest::default()));
        card = list.approvals.into_iter().find(|a| a.session_id == mawas);
        card.is_some()
    });
    let card = card.unwrap();
    assert!(
        card.title.contains("suggest an answer"),
        "the gate says what she asks: {}",
        card.title
    );
    let _: Empty = env.daemon.call(Request::ApprovalRespond(ApprovalRespond {
        approval_id: card.id,
        option_id: "allow_once".into(),
    }));
}

#[gpui::test]
fn a_suggestion_of_mawas_on_an_approval_is_words_until_the_user_uses_it(cx: &mut TestAppContext) {
    use hutan_protocol::suggest::{AskSuggest, AskSuggested, AskTarget, SuggestedAnswer};

    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);

    // Another agent asks to rename a session; the request waits for the user.
    let by_other = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "agent"));
    wait_needs(&panel, cx, 1);
    let today = panel.read_with(cx, |p, cx| p.today(cx));
    let TodayKind::Approval {
        approval,
        suggestion: None,
        ..
    } = &today.requests[0].kind
    else {
        panic!("an approval without a suggestion expected: {today:?}");
    };
    let approval = *approval;
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Suggestion("today-box", 0))
        })
        .is_none(),
        "no suggestion, no box"
    );

    let suggest = |reason: &str| {
        env.daemon.call_from_in_background::<AskSuggested>(
            mawas,
            Request::AskSuggest(AskSuggest {
                target: AskTarget::Approval { id: approval },
                answer: SuggestedAnswer::Option {
                    option_id: "allow_once".into(),
                },
                reason: reason.into(),
            }),
        )
    };
    let shown_on_the_card = |panel: &Entity<AssistantPanel>, cx: &mut VisualTestContext| {
        pump_until(cx, "the suggestion on the card", |cx| {
            panel.read_with(cx, |p, cx| {
                matches!(
                    p.today(cx).requests.first().map(|r| &r.kind),
                    Some(TodayKind::Approval { suggestion: Some(s), .. })
                        if s.option_id == "allow_once" && s.label == "Allow once"
                )
            })
        });
    };

    // Mawas suggests (her own gate asks first: it is a change like the others).
    let first = suggest("The same rename you allowed last week.");
    wait_needs(&panel, cx, 2);
    click(&view, cx, Region::AssistantItem("today-option", 0));
    allow_her_suggestion_done(&first, cx);
    shown_on_the_card(&panel, cx);
    for part in ["today-box", "today-use", "today-dismiss"] {
        drawn(&view, cx, Region::Suggestion(part, 0));
    }
    // It answers nothing: the request waits, and the options are the user's.
    assert!(
        by_other.try_take().is_none(),
        "nothing answered the request"
    );
    assert_eq!(title_of(&env, target.id), target.title);
    drawn(&view, cx, Region::AssistantItem("today-option", 0));

    // Dismiss hides it; the request still waits.
    click(&view, cx, Region::Suggestion("today-dismiss", 0));
    pump_until(cx, "the suggestion to go", |cx| {
        panel.read_with(cx, |p, cx| {
            matches!(
                p.today(cx).requests.first().map(|r| &r.kind),
                Some(TodayKind::Approval {
                    suggestion: None,
                    ..
                })
            )
        })
    });
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Suggestion("today-box", 0))
        })
        .is_none()
    );
    assert!(by_other.try_take().is_none());

    // A newer suggestion shows again; *Use* is the user's own click on the option: the request
    // runs as if they had pressed Allow once.
    let second = suggest("It is still the same rename.");
    wait_needs(&panel, cx, 2);
    click(&view, cx, Region::AssistantItem("today-option", 0));
    allow_her_suggestion_done(&second, cx);
    shown_on_the_card(&panel, cx);
    click(&view, cx, Region::Suggestion("today-use", 0));
    let mut result = None;
    wait_until(cx, "the request to run", |cx| {
        let _ = cx;
        result = by_other.try_take();
        result.is_some()
    });
    result.unwrap().expect("the rename ran");
    assert_eq!(title_of(&env, target.id), "agent");
    wait_needs(&panel, cx, 0);
}

#[gpui::test]
fn a_suggested_answer_to_a_question_fills_its_card_and_the_user_sends_it(cx: &mut TestAppContext) {
    use common::chat::ChatEnv;
    use hutan_chat_view::QuestionState;
    use hutan_desktop::workbench::chat::LoadStatus;
    use hutan_protocol::chat::{ChatSend, ChatSent};
    use hutan_protocol::suggest::{AskSuggest, AskSuggested, AskTarget, SuggestedAnswer};

    let chat_env = ChatEnv::new();
    let env = AssistantEnv::start(Options {
        configured: true,
        codex: Some(chat_env.fixture("question-elicitation")),
        ..Options::default()
    });
    let chat = env.daemon.create_chat(&chat_env.cwd());
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let _: ChatSent = env.daemon.call(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "deploy".into(),
        attachments: Vec::new(),
    }));
    // The agent's chat, with its question card, is what the center shows.
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(chat.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(chat.id))
        .unwrap();
    let chat_view = view
        .read_with(cx, |wb, _| wb.chat_view_of(chat.id))
        .expect("the chat's view");
    let mut question = None;
    pump_until(cx, "the question", |cx| {
        question = model.read_with(cx, |m, _| {
            m.transcript().pending_questions().first().map(|q| q.id)
        });
        question.is_some()
    });
    let question = question.unwrap();
    drawn(&view, cx, Region::ChatQuestion(0));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Suggestion("question-box", 0))
        })
        .is_none(),
        "no suggestion, no box"
    );

    let suggested = env.daemon.call_from_in_background::<AskSuggested>(
        mawas,
        Request::AskSuggest(AskSuggest {
            target: AskTarget::Question { id: question },
            answer: SuggestedAnswer::Values {
                values: [("environment".to_owned(), json!("production"))].into(),
            },
            reason: "The release notes say production is next.".into(),
        }),
    );
    allow_her_suggestion(&env, cx, mawas);
    wait_until(cx, "the suggestion to be accepted", |cx| {
        let _ = cx;
        suggested.try_take().is_some_and(|r| r.is_ok())
    });
    for part in ["question-box", "question-use", "question-dismiss"] {
        drawn(&view, cx, Region::Suggestion(part, 0));
    }
    let held = |cx: &mut VisualTestContext| {
        chat_view.read_with(cx, |c, cx| c.question_values(question, cx))
    };
    // The form holds its own default until the user clicks *Use*.
    assert_eq!(held(cx).unwrap().unwrap()["environment"], "staging");

    // *Use* fills the field with the suggested option. Nothing is sent.
    click(&view, cx, Region::Suggestion("question-use", 0));
    pump_until(cx, "the field to be filled", |cx| {
        held(cx).is_some_and(|v| v.is_ok_and(|v| v["environment"] == "production"))
    });
    assert_eq!(
        model.read_with(cx, |m, _| m
            .transcript()
            .question(question)
            .unwrap()
            .state
            .clone()),
        QuestionState::Pending
    );
    assert!(
        chat_env.received().iter().all(|(m, _)| m != "response"),
        "nothing was sent to the agent"
    );

    // The user sends it, from their own Submit.
    click(&view, cx, Region::ChatQuestion(0));
    pump_until(cx, "the answer", |cx| {
        model.read_with(cx, |m, _| {
            matches!(
                m.transcript().question(question).unwrap().state,
                QuestionState::Answered { .. }
            )
        })
    });
    let response = chat_env
        .received()
        .into_iter()
        .find(|(m, _)| m == "response")
        .expect("the agent got the answer");
    // A field the suggestion left out keeps its default (`notify`).
    assert_eq!(
        response.1["result"],
        json!({"action": "accept", "content": {"environment": "production", "notify": true}})
    );
}

#[gpui::test]
fn a_suggestion_on_an_approval_card_of_a_chat_is_used_with_one_click_of_the_user(
    cx: &mut TestAppContext,
) {
    use common::chat::ChatEnv;
    use hutan_chat_view::ApprovalState;
    use hutan_desktop::workbench::chat::LoadStatus;
    use hutan_protocol::chat::{ChatSend, ChatSent};
    use hutan_protocol::suggest::{AskSuggest, AskSuggested, AskTarget, SuggestedAnswer};

    let chat_env = ChatEnv::new();
    let env = AssistantEnv::start(Options {
        configured: true,
        codex: Some(chat_env.fixture("approval-touch")),
        ..Options::default()
    });
    let chat = env.daemon.create_chat(&chat_env.cwd());
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, _, _) = shown(&panel, cx);
    let _: ChatSent = env.daemon.call(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "Run `touch hi.txt` in a shell, then reply with the word done.".into(),
        attachments: Vec::new(),
    }));
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(chat.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(chat.id))
        .unwrap();
    let mut approval = None;
    pump_until(cx, "the approval", |cx| {
        approval = model.read_with(cx, |m, _| {
            m.transcript().pending_approvals().first().map(|a| a.id)
        });
        approval.is_some()
    });
    let approval = approval.unwrap();

    let suggested = env.daemon.call_from_in_background::<AskSuggested>(
        mawas,
        Request::AskSuggest(AskSuggest {
            target: AskTarget::Approval { id: approval },
            answer: SuggestedAnswer::Option {
                option_id: "accept".into(),
            },
            reason: "A touch in the project folder is harmless.".into(),
        }),
    );
    allow_her_suggestion(&env, cx, mawas);
    allow_her_suggestion_done(&suggested, cx);
    for part in ["chat-box", "chat-use", "chat-dismiss"] {
        drawn(&view, cx, Region::Suggestion(part, 0));
    }
    assert!(
        !chat_env.received().iter().any(|(m, _)| m == "response"),
        "the suggestion answered nothing"
    );

    // *Use* is the user's click on that option.
    click(&view, cx, Region::Suggestion("chat-use", 0));
    pump_until(cx, "the request to resolve", |cx| {
        model.read_with(cx, |m, _| {
            m.transcript()
                .approval(approval)
                .is_some_and(|a| a.state != ApprovalState::Pending)
        })
    });
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.transcript().approval(approval).unwrap().state,
            ApprovalState::Selected {
                option_id: "accept".into()
            }
        );
    });
    let answers: Vec<_> = chat_env
        .received()
        .into_iter()
        .filter(|(m, _)| m == "response")
        .map(|(_, msg)| msg["result"].clone())
        .collect();
    assert_eq!(answers, [json!({"decision": "accept"})]);
}

// ---- AGENTS-ACTIVITY: the rail's Recent tab -------------------------------------------------------

fn seen_of(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<u64> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).agents_seen())
}

#[gpui::test]
fn the_recent_tab_lists_what_happened_by_agent_and_mark_as_seen_empties_it(
    cx: &mut TestAppContext,
) {
    let env = started();
    // Something that happened: a session began, and an agent reported in it.
    let _session = reported_agent(&env);
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "the tabs", |cx| painted(&view, cx, "today-tab-recent"));
    assert_eq!(seen_of(&view, cx), None, "nobody caught up yet");

    click_region(&view, cx, "today-tab-recent");
    assert_eq!(written_tab(&view, cx).as_deref(), Some("recent"));
    // The daemon's catch-up, grouped by agent: the session that started, with its project.
    wait_until(cx, "the first agent of the catch-up", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-recent-group", 0))
        })
        .is_some()
    });
    let activity = panel
        .read_with(cx, |p, _| p.recent_activity().cloned())
        .expect("the daemon answered");
    assert!(activity.summary.sessions_started >= 1);
    assert!(!activity.not_recorded.is_empty());
    assert!(painted(&view, cx, "today-recent-not-recorded"));
    assert!(painted(&view, cx, "today-recent-since"));
    // The tab counts the events.
    assert!(panel.read_with(cx, |p, _| {
        p.recent_activity().is_some_and(|a| !a.events.is_empty())
    }));

    // Mark as seen: the point moves to the end of what was read, and what was read is behind it.
    click_region(&view, cx, "today-recent-seen");
    wait_until(cx, "the caught-up point", |cx| seen_of(&view, cx).is_some());
    let seen = seen_of(&view, cx).unwrap();
    assert!(seen >= activity.to_ms);
    wait_until(cx, "an empty catch-up", |cx| {
        panel.read_with(cx, |p, _| {
            p.recent_activity()
                .is_some_and(|a| a.events.is_empty() && a.from_ms >= seen)
        })
    });
    assert!(
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::AssistantItem("today-recent-group", 0))
        })
        .is_none(),
        "nothing is listed after the point"
    );
    // A later start shows up in it, once the tab reads again.
    let later = env.daemon.sh("sleep 600");
    // The tab reads again at most every two seconds when the agents change: the test's clock
    // does not run by itself.
    wait_until(cx, "the later session", |cx| {
        cx.executor().advance_clock(Duration::from_millis(500));
        panel.read_with(cx, |p, _| {
            p.recent_activity()
                .is_some_and(|a| a.events.iter().any(|e| e.session_id == Some(later.id)))
        })
    });
}

#[gpui::test]
fn summarise_with_mawas_fills_the_composer_and_sends_nothing(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "Mawas's session", |cx| {
        panel.read_with(cx, |p, cx| {
            p.session(cx).is_some_and(|s| p.chat_model(s).is_some())
        })
    });
    click_region(&view, cx, "today-tab-recent");
    wait_until(cx, "the catch-up", |cx| {
        panel.read_with(cx, |p, _| p.recent_activity().is_some())
    });
    click_region(&view, cx, "today-recent-summarise");
    let session = panel.read_with(cx, |p, cx| p.session(cx)).unwrap();
    let model = panel.read_with(cx, |p, _| p.chat_model(session)).unwrap();
    wait_until(cx, "the prepared ask", |cx| {
        model.read_with(cx, |m, _| m.draft_message().contains("agents_activity"))
    });
    let draft = model.read_with(cx, |m, _| m.draft_message());
    assert!(
        draft.starts_with("Summarise what my agents did since"),
        "{draft}"
    );
    assert!(draft.contains("agents_board"), "{draft}");
    // The user sends it, or does not: nothing was sent, nothing is journaled.
    let history: hutan_protocol::chat::ChatHistory = env.daemon.call(Request::ChatHistory(
        hutan_protocol::chat::ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 50,
        },
    ));
    assert!(history.events.is_empty(), "{history:?}");
    assert!(
        !env.daemon
            .session(session)
            .chat
            .is_some_and(|c| c.turn_active)
    );
}

// ---- AGENTS-FLOW: the Flow section of the Recent tab ----------------------------------------------

/// A terminal agent that reported `blocked` through a hook: the daemon's state log opens a wait.
fn blocked_agent(env: &AssistantEnv) -> SessionInfo {
    let session = reported_agent(env);
    let applied: AgentApplied = env.daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session.id,
        source: "test-hooks".into(),
        agent: "claude".into(),
        state: Some(AgentState::Blocked),
        message: None,
        seq: 2,
        agent_session_id: None,
        resume_argv: None,
        pid: None,
        node: None,
    }));
    assert!(applied.applied);
    session
}

#[gpui::test]
fn the_flow_section_shows_the_waits_with_what_they_rest_on_and_analyse_fills_the_composer(
    cx: &mut TestAppContext,
) {
    let env = started();
    let _session = blocked_agent(&env);
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "Mawas's session", |cx| {
        panel.read_with(cx, |p, cx| {
            p.session(cx).is_some_and(|s| p.chat_model(s).is_some())
        })
    });
    click_region(&view, cx, "today-tab-recent");
    wait_until(cx, "the flow of the last week", |cx| {
        panel.read_with(cx, |p, _| {
            p.recent_flow()
                .and_then(|f| f.total.waits.as_ref())
                .is_some_and(|w| w.count == 1)
        })
    });
    assert!(painted(&view, cx, "today-flow"));
    let flow = panel
        .read_with(cx, |p, _| p.recent_flow().cloned())
        .expect("flow");
    // The wait is a terminal agent's: from the state log, still going, and the section says so.
    let waits = flow.total.waits.as_ref().unwrap();
    assert_eq!(waits.by_source.len(), 1);
    assert_eq!(
        waits.by_source[0].source,
        hutan_protocol::stats::WaitSource::StateLog
    );
    assert_eq!(waits.by_source[0].open, 1);
    assert!(flow.coverage.state_log);
    let lines = hutan_desktop::workbench::panels::flow_lines(&flow);
    assert!(lines[0].starts_with("Waiting for you: 1"), "{lines:?}");
    assert!(lines[0].contains("1 still going"), "{lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("Tokens: none reported")),
        "nothing reported is not zero: {lines:?}"
    );

    // *Analyse with Mawas* prepares the ask and sends nothing.
    let session = panel.read_with(cx, |p, cx| p.session(cx)).unwrap();
    let model = panel.read_with(cx, |p, _| p.chat_model(session)).unwrap();
    click_region(&view, cx, "today-flow-analyse");
    wait_until(cx, "the prepared ask", |cx| {
        model.read_with(cx, |m, _| m.draft_message().contains("agents_stats"))
    });
    let draft = model.read_with(cx, |m, _| m.draft_message());
    assert!(
        draft.starts_with("Analyse how my agents worked over the last 7 days"),
        "{draft}"
    );
    assert!(draft.contains("what is not recorded"), "{draft}");
    let history: hutan_protocol::chat::ChatHistory = env.daemon.call(Request::ChatHistory(
        hutan_protocol::chat::ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 50,
        },
    ));
    assert!(history.events.is_empty(), "{history:?}");
    assert!(
        !env.daemon
            .session(session)
            .chat
            .is_some_and(|c| c.turn_active)
    );
}

/// The chip of Mawas's composer that holds a `turn_review`, once there is one.
fn review_chip(
    model: &Entity<hutan_desktop::workbench::chat::ChatModel>,
    cx: &VisualTestContext,
) -> Option<hutan_desktop::workbench::chat::ContextChip> {
    model.read_with(cx, |m, _| {
        m.context()
            .iter()
            .find(|chip| chip.kind() == hutan_protocol::assistant::AttachmentKind::TurnReview)
            .cloned()
    })
}

#[gpui::test]
fn review_this_turn_attaches_the_packet_to_mawas_without_sending_and_the_chip_opens_it(
    cx: &mut TestAppContext,
) {
    use common::chat::ChatEnv;
    use hutan_desktop::workbench::chat::{LoadStatus, Preview, ended_turns};
    use hutan_protocol::assistant::Attachment;
    use hutan_protocol::chat::{ChatSend, ChatSent};

    let chat_env = ChatEnv::new();
    let env = AssistantEnv::start(Options {
        configured: true,
        codex: Some(chat_env.fixture("plain-turn")),
        ..Options::default()
    });
    let chat = env.daemon.create_chat(&chat_env.cwd());
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (mawas, mawas_model, _) = shown(&panel, cx);
    let _: ChatSent = env.daemon.call(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "ping".into(),
        attachments: Vec::new(),
    }));
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(chat.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(chat.id))
        .unwrap();
    let mut turn = None;
    pump_until(cx, "the turn to end", |cx| {
        turn = model.read_with(cx, |m, _| {
            ended_turns(m.transcript(), false).first().map(|t| t.turn)
        });
        turn.is_some()
    });
    let turn = turn.unwrap();
    assert!(
        review_chip(&mawas_model, cx).is_none(),
        "nothing attached yet"
    );

    // *Review* sits on the turn's footer.
    let mut button = None;
    pump_until(cx, "the Review button of the footer", |cx| {
        draw(cx);
        button = (0..64)
            .find_map(|n| view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatReview(n))));
        button.is_some()
    });
    common::click_at(cx, button.unwrap().center());

    // It attaches the turn's review to Mawas's composer — and sends nothing.
    pump_until(cx, "the chip on Mawas's composer", |cx| {
        review_chip(&mawas_model, cx).is_some()
    });
    let chip = review_chip(&mawas_model, cx).unwrap();
    assert_eq!(
        chip.attachment,
        Attachment::TurnReview {
            session_id: chat.id,
            turn_id: Some(turn),
        }
    );
    assert!(chip.label().starts_with("review · "), "{}", chip.label());
    assert_eq!(
        chip.preview,
        Preview::None,
        "nothing is read until it is opened"
    );
    assert!(
        mawas_model.read_with(cx, |m, _| m.transcript().entries().is_empty()),
        "Mawas was sent nothing"
    );
    assert_eq!(env.mock.requests_to(PATH).len(), 0, "no model was called");
    let _ = mawas;

    // Mawas Home is shown with the chip; opening it reads the packet and shows its text.
    let chip_at = drawn(&view, cx, Region::ChatContextChip(0));
    common::click_at(cx, chip_at.center());
    pump_until(cx, "the packet's text", |cx| {
        matches!(
            review_chip(&mawas_model, cx).map(|c| c.preview),
            Some(Preview::Ready(_))
        )
    });
    let Some(Preview::Ready(text)) = review_chip(&mawas_model, cx).map(|c| c.preview) else {
        unreachable!()
    };
    for part in [
        "Review of ",
        "Turn 1 completed",
        "Prompt:",
        "Signals are heuristics",
        "Not recorded:",
    ] {
        assert!(text.contains(part), "missing {part:?} in\n{text}");
    }
    drawn(&view, cx, Region::Chat("review"));
    assert_eq!(
        env.mock.requests_to(PATH).len(),
        0,
        "opening it sent nothing either"
    );

    // Closing it hides the text again (the composer grows upward while it is open, so the chip
    // has moved: ask where it is now).
    let chip_at = drawn(&view, cx, Region::ChatContextChip(0));
    common::click_at(cx, chip_at.center());
    pump_until(cx, "the packet's text to close", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("review")))
            .is_none()
    });
}

#[gpui::test]
fn a_terminal_agent_is_reviewed_from_its_menu_entry_and_the_chip_says_what_is_not_recorded(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::chat::Preview;

    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (_, mawas_model, _) = shown(&panel, cx);
    let agent = reported_agent(&env);

    // The entry of the session menus dispatches this action (a terminal agent has no turns:
    // none is named).
    wait_until(cx, "the agent to be known to the window", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(agent.id)
                .is_some_and(|s| s.agent.is_some())
        })
    });
    cx.dispatch_action(actions::ReviewTurn(actions::ReviewAt {
        session: agent.id,
        turn: None,
        number: None,
    }));
    pump_until(cx, "the chip", |cx| review_chip(&mawas_model, cx).is_some());
    let chip = review_chip(&mawas_model, cx).unwrap();
    assert!(
        chip.label().ends_with("last finished turn"),
        "{}",
        chip.label()
    );

    let chip_at = drawn(&view, cx, Region::ChatContextChip(0));
    common::click_at(cx, chip_at.center());
    pump_until(cx, "the packet's text", |cx| {
        matches!(
            review_chip(&mawas_model, cx).map(|c| c.preview),
            Some(Preview::Ready(_))
        )
    });
    let Some(Preview::Ready(text)) = review_chip(&mawas_model, cx).map(|c| c.preview) else {
        unreachable!()
    };
    for part in [
        "a terminal agent's hook report",
        "Reported state: working",
        "Not recorded:",
        "a terminal agent has no journal",
    ] {
        assert!(text.contains(part), "missing {part:?} in\n{text}");
    }
}

// ---- MAWAS-DIGEST-A: the Catch up chip -----------------------------------------------------------

fn chip_drawn(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("catch-up")))
        .is_some()
}

fn chip_text(panel: &Entity<AssistantPanel>, cx: &VisualTestContext) -> Option<gpui::SharedString> {
    panel.read_with(cx, |p, cx| {
        p.session(cx)
            .and_then(|s| p.chat_view(s))
            .and_then(|view| view.read(cx).catch_up_text())
    })
}

#[gpui::test]
fn the_catch_up_chip_says_what_waits_and_one_click_opens_the_recent_tab(cx: &mut TestAppContext) {
    let env = started();
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);
    let (view, cx, panel) = open_wide(cx, &env);
    // A request waits: something the user was away for.
    let _asked = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "x"));
    wait_needs(&panel, cx, 1);
    wait_until(cx, "the chip", |cx| chip_drawn(&view, cx));
    let text = chip_text(&panel, cx).expect("the chip has its line");
    assert!(
        text.contains("1 request waits for you"),
        "the one summary line of the daemon's records: {text}"
    );
    // Zero tokens: no turn was started to say it.
    let session = panel.read_with(cx, |p, cx| p.session(cx)).unwrap();
    assert!(
        !env.daemon
            .session(session)
            .chat
            .is_some_and(|c| c.turn_active)
    );

    // The rail is hidden: one click opens it on the Recent tab, and remembers both.
    panel.update(cx, |p, cx| p.set_rail_open(false, cx));
    wait_until(cx, "the rail hidden", |cx| {
        !panel.read_with(cx, |p, _| p.rail_open())
    });
    click(&view, cx, Region::Chat("catch-up"));
    wait_until(cx, "the rail open on Recent", |cx| {
        panel.read_with(cx, |p, _| p.rail_open())
            && written_tab(&view, cx).as_deref() == Some("recent")
    });
    wait_until(cx, "the Recent tab", |cx| {
        painted(&view, cx, "today-recent-since")
    });
}

#[gpui::test]
fn marking_as_seen_takes_the_chip_away_and_an_empty_home_has_none(cx: &mut TestAppContext) {
    // Nothing happened: the empty conversation has no chip, and the read that found out was made.
    let env = started();
    let (view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "the read of the catch-up", |cx| {
        panel.read_with(cx, |p, _| p.recent_activity().is_some())
    });
    assert!(chip_text(&panel, cx).is_none());
    assert!(!chip_drawn(&view, cx), "nothing happened: no chip");

    // Something did: the chip; Mark as seen moves the point past it and the chip goes.
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);
    let _asked = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "x"));
    wait_until(cx, "the chip", |cx| {
        cx.executor().advance_clock(Duration::from_millis(500));
        chip_drawn(&view, cx)
    });
    click(&view, cx, Region::Chat("catch-up"));
    wait_until(cx, "the Recent tab", |cx| {
        painted(&view, cx, "today-recent-seen")
    });
    click_region(&view, cx, "today-recent-seen");
    wait_until(cx, "the chip gone", |cx| {
        cx.executor().advance_clock(Duration::from_millis(500));
        chip_text(&panel, cx).is_none()
    });
    assert!(!chip_drawn(&view, cx));
}

// ---- the digest (MAWAS-DIGEST-A) ---------------------------------------------------------------

/// A daemon whose digest counts a 150 ms "hour" and looks every 20 ms; `[notify] digest_hours`
/// as `hours` says.
fn digest_env(hours: u64) -> AssistantEnv {
    let tables = Box::leak(format!("[notify]\ndigest_hours = {hours}\n").into_boxed_str());
    AssistantEnv::start(Options {
        configured: true,
        tables,
        tweak: Some(|config| {
            config.digest = hutan_daemon::digest::DigestTuning {
                tick: Duration::from_millis(20),
                hour_ms: 150,
            };
        }),
        ..Options::default()
    })
}

#[gpui::test]
fn the_digest_setting_is_a_row_of_the_mawas_page_and_off_by_default(cx: &mut TestAppContext) {
    let env = started();
    let (view, cx) = open(cx, &env.daemon);
    cx.dispatch_action(actions::OpenSettingsFor("assistant".into()));
    draw(cx);
    wait_until(cx, "the daemon's settings", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.daemon_settings().entry("notify.digest_hours").is_some()
        })
    });
    // A row of the Mawas page, after Memory, with the rule in words; off by default.
    let rows = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
    assert!(
        rows.iter().any(|row| row == "d:notify.digest_hours"),
        "{rows:?}"
    );
    assert!(
        rows.iter().position(|row| row == "d:notify.digest_hours")
            > rows.iter().position(|row| row == "d:assistant.memory")
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .daemon_settings()
            .value("notify.digest_hours")
            .cloned()),
        Some(json!(0))
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_number_hint("notify.digest_hours")),
        Some("Hours away; 0 is off (at most 720)".to_owned())
    );
    // It is not also a row of the Notifications section of General.
    cx.dispatch_action(actions::OpenSettingsFor("notifications".into()));
    draw(cx);
    let general = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
    assert!(!general.iter().any(|row| row == "d:notify.digest_hours"));

    // Writing it reaches the daemon live.
    view.update(cx, |wb, cx| {
        wb.daemon_setting_set("notify.digest_hours", Some(json!(4)), cx);
    });
    wait_until(cx, "the daemon to take it", |_| {
        let settings: DaemonSettings = env.daemon.call(Request::DaemonSettingsGet(Empty {}));
        settings
            .entries
            .iter()
            .find(|e| e.key == "notify.digest_hours")
            .unwrap()
            .value
            == json!(4)
    });
}

#[gpui::test]
fn the_window_tells_the_daemon_when_it_is_not_the_active_one_and_a_digests_open_shows_recent(
    cx: &mut TestAppContext,
) {
    let env = digest_env(1);
    let target = env.daemon.sh("sleep 600");
    let other = reported_agent(&env);
    let (view, cx, panel) = open_wide(cx, &env);
    let reported = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).window_active_reported())
    };
    wait_until(cx, "the window reported as active", |cx| {
        reported(cx) == Some(true)
    });

    // The user leaves: the daemon is told, and starts counting the period.
    cx.deactivate_window();
    wait_until(cx, "the deactivation to be reported", |cx| {
        reported(cx) == Some(false)
    });
    // Something needs them while they are away.
    let _asked = env
        .daemon
        .call_from_in_background::<SessionResult>(other.id, rename_request(target.id, "x"));
    wait_needs(&panel, cx, 1);

    // One digest, with the line the Catch up chip shows.
    let mut digest = None;
    wait_until(cx, "the digest", |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        let list: hutan_protocol::notify::NotifyList =
            env.daemon.call(Request::NotifyList(Empty {}));
        digest = list
            .intents
            .into_iter()
            .find(|i| i.kind == hutan_protocol::notify::IntentKind::Digest);
        digest.is_some()
    });
    let digest = digest.unwrap();
    assert!(
        digest.body.contains("1 request waits for you"),
        "{}",
        digest.body
    );

    // Its *Open* (a click on the notification) shows Mawas Home on the Recent tab, whatever
    // tab the rail was on and even if it was hidden.
    panel.update(cx, |p, cx| p.set_rail_open(false, cx));
    wait_until(cx, "the rail hidden", |cx| {
        !panel.read_with(cx, |p, _| p.rail_open())
    });
    let _: hutan_protocol::notify::NotifyActed =
        env.daemon
            .call(Request::NotifyAct(hutan_protocol::notify::NotifyAct {
                id: digest.id.clone(),
                action_id: hutan_protocol::notify::ACTION_OPEN.into(),
            }));
    wait_until(cx, "the rail open on Recent", |cx| {
        panel.read_with(cx, |p, _| p.rail_open())
            && written_tab(&view, cx).as_deref() == Some("recent")
    });
    wait_until(cx, "the Recent tab", |cx| {
        painted(&view, cx, "today-recent-since")
    });
}
