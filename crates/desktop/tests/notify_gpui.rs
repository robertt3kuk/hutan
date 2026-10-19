#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The desktop's part of notification intents (protocol 0.13) against an in-process daemon: it
//! reports the session it shows (`client.focus`, cleared with the window's activation and
//! repeated after a reconnect), and it shows the session a notification's "Open" asks for
//! (`ui.open`). The daemon side is observable through `chat_turn_finished`, which a focused
//! session does not raise.

mod common;

use common::chat::ChatEnv;
use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_client::ConnectionState;
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_protocol::chat::{ChatHistory, ChatHistoryRequest, ChatSend, ChatSent};
use hutan_protocol::messages::{Empty, Request};
use hutan_protocol::notify::{
    ACTION_OPEN, ActRoute, NotificationIntent, NotifyAct, NotifyActed, NotifyList,
};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: hutan_client::Connection,
    sessions: usize,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(daemon.home()))),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
    (view, cx)
}

fn reported(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<Option<SessionId>> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).focus_reported())
}

fn intents(daemon: &TestDaemon) -> Vec<NotificationIntent> {
    daemon
        .call::<NotifyList>(Request::NotifyList(Empty {}))
        .intents
}

fn send(daemon: &TestDaemon, session: SessionId) {
    daemon.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: session,
        text: "Say hi".to_owned(),
        attachments: Vec::new(),
    }));
}

fn turn_ended(daemon: &TestDaemon, session: SessionId) -> bool {
    daemon
        .call::<ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 100,
        }))
        .events
        .iter()
        .any(|event| event.kind.ends_turn())
}

/// Runs one turn in the session the desktop shows, then one in `other`; the intents the daemon
/// holds afterwards are only about `other`.
fn only_the_unwatched_turn_raises(
    daemon: &TestDaemon,
    cx: &mut VisualTestContext,
    watched: SessionId,
    other: SessionId,
) -> NotificationIntent {
    send(daemon, watched);
    wait_until(cx, "the watched turn to end", |_| {
        turn_ended(daemon, watched)
    });
    send(daemon, other);
    wait_until(cx, "the unwatched turn's intent", |_| {
        intents(daemon).iter().any(|i| i.session_id == other)
    });
    let pending = intents(daemon);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].session_id, other);
    pending.into_iter().next().unwrap()
}

#[gpui::test]
fn the_desktop_reports_what_it_shows_and_opens_what_a_notification_asks_for(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let watched = daemon.create_chat(&env.cwd());
    let other = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon, daemon.connection(), 2);

    // Nothing is shown yet; the focus of the window is "no session".
    wait_until(cx, "the first focus report", |cx| {
        reported(&view, cx).is_some()
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(watched.id, window, cx));
    wait_until(cx, "the shown session to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // An inactive window shows nothing to the user; activating it again says so.
    cx.deactivate_window();
    wait_until(cx, "the deactivation to be reported", |cx| {
        reported(&view, cx) == Some(None)
    });
    cx.update(|window, _| window.activate_window());
    wait_until(cx, "the activation to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // The daemon raises no intent for the shown session, one for the other.
    let intent = only_the_unwatched_turn_raises(&daemon, cx, watched.id, other.id);

    // "Open" asks the desktop to show the other session.
    let acted: NotifyActed = daemon.call(Request::NotifyAct(NotifyAct {
        id: intent.id,
        action_id: ACTION_OPEN.to_owned(),
    }));
    assert_eq!(acted.route, ActRoute::UiOpen);
    assert_eq!(acted.desktop_clients, 1);
    wait_until(cx, "the session to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(other.id)
    });
    wait_until(cx, "the new focus to be reported", |cx| {
        reported(&view, cx) == Some(Some(other.id))
    });

    // A session that has ended is not shown; the window says so and keeps what it had.
    let failing = daemon.sh("exit 2");
    wait_until(cx, "the failure intent", |_| {
        intents(&daemon).iter().any(|i| i.session_id == failing.id)
    });
    wait_until(cx, "the session to be seen as ended", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(failing.id)
                .is_some_and(|s| !s.state.is_live())
        })
    });
    let failure = intents(&daemon)
        .into_iter()
        .find(|i| i.session_id == failing.id)
        .unwrap();
    let acted: NotifyActed = daemon.call(Request::NotifyAct(NotifyAct {
        id: failure.id,
        action_id: ACTION_OPEN.to_owned(),
    }));
    assert_eq!(acted.desktop_clients, 1);
    wait_until(cx, "the notice", |cx| {
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            == Some("That session is no longer running".to_owned())
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(other.id)
    );
}

#[gpui::test]
fn the_focus_is_reported_again_on_the_connection_that_follows_a_loss(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let mut daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let watched = daemon.create_chat(&env.cwd());
    let other = daemon.create_chat(&env.cwd());
    let connection = daemon.proxied_connection();
    let (view, cx) = open(cx, &daemon, connection, 2);
    view.update_in(cx, |wb, window, cx| wb.open_session(watched.id, window, cx));
    wait_until(cx, "the shown session to be reported", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // The link drops; the daemon forgets the old connection's focus, and so does the store's
    // record of what was acknowledged.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the loss to be seen", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(
                wb.sessions().read(cx).connection_state(),
                ConnectionState::Reconnecting { .. }
            )
        }) && reported(&view, cx).is_none()
    });
    daemon.proxy().block(false);
    wait_until(cx, "the focus to be reported on the new connection", |cx| {
        reported(&view, cx) == Some(Some(watched.id))
    });

    // Had it not been, the finished turn of the shown session would raise an intent.
    only_the_unwatched_turn_raises(&daemon, cx, watched.id, other.id);
}

/// The assistant panel's composer holding the keyboard, as the workbench reports it.
fn looked_at_assistant(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> bool {
    reported(view, cx) == Some(Some(session))
}

#[gpui::test]
fn the_assistant_page_counts_as_looked_at_only_while_it_is_on_screen(cx: &mut TestAppContext) {
    use common::assistant::{AssistantEnv, Options, PATH, open, open_assistant, reply, shown};
    use hutan_desktop::workbench::panels::AssistantChoice;
    use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
    use hutan_protocol::messages::SessionResult;

    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("shown reply"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);

    // Opening the panel puts the keyboard in its composer: that session is looked at.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenAssistant);
    let panel = open_assistant(&view, cx);
    let (global, model, chat) = shown(&panel, cx);
    wait_until(cx, "the docked assistant to be reported", |cx| {
        looked_at_assistant(&view, cx, global)
    });

    // Its turn finishes: nothing is raised. The project's assistant, which nobody looks at,
    // finishes one too: that raises.
    model.update(cx, |m, cx| {
        m.edit(
            |input| {
                input.clear();
                input.insert("hello");
            },
            cx,
        );
    });
    assert!(chat.update(cx, |v, cx| v.send(cx)));
    common::assistant::wait_answer(&model, cx, "shown reply");
    let other = env
        .daemon
        .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
            scope: AssistantScope::Workspace,
            workspace_id: Some(workspace.id),
            new: false,
        }))
        .session;
    env.mock.route("POST", PATH, reply("other reply"));
    send(&env.daemon, other.id);
    wait_until(cx, "the other assistant's intent", |_| {
        intents(&env.daemon)
            .iter()
            .any(|i| i.session_id == other.id)
    });
    let pending = intents(&env.daemon);
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].session_id, other.id);

    // Another tab takes the pane: Mawas Home is not on screen, not looked at.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(
                pane,
                hutan_desktop::workbench::model::ItemRef::placeholder("Elsewhere"),
            )
            .unwrap();
        });
    });
    wait_until(cx, "the hidden page to stop counting", |cx| {
        reported(&view, cx) != Some(Some(global))
    });
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    panel.update(cx, |p, cx| p.focus_composer(cx));
    wait_until(cx, "the shown page to count again", |cx| {
        looked_at_assistant(&view, cx, global)
    });

    // An inactive window shows nothing to the user, whatever holds its keyboard.
    cx.deactivate_window();
    wait_until(cx, "the deactivation to be reported", |cx| {
        reported(&view, cx) == Some(None)
    });
    cx.update(|window, _| window.activate_window());
    wait_until(cx, "the activation to be reported", |cx| {
        looked_at_assistant(&view, cx, global)
    });

    // Another scope: the panel shows the project's assistant and that one is looked at.
    panel.update(cx, |p, cx| {
        p.choose(AssistantChoice::Pinned(workspace.id), cx)
    });
    panel.update(cx, |p, cx| p.focus_composer(cx));
    wait_until(cx, "the other scope to be reported", |cx| {
        looked_at_assistant(&view, cx, other.id)
    });

    // "Open" on an intent of the global assistant shows it in the panel, in its own scope, with
    // the keyboard in its composer: not in a tab.
    let tabs_before = view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .iter()
            .map(|p| p.items.len())
            .sum::<usize>()
    });
    env.mock.route("POST", PATH, reply("global again"));
    send(&env.daemon, global);
    wait_until(cx, "the global assistant's intent", |_| {
        intents(&env.daemon).iter().any(|i| i.session_id == global)
    });
    let intent = intents(&env.daemon)
        .into_iter()
        .find(|i| i.session_id == global)
        .unwrap();
    let acted: NotifyActed = env.daemon.call(Request::NotifyAct(NotifyAct {
        id: intent.id,
        action_id: ACTION_OPEN.to_owned(),
    }));
    assert_eq!(acted.desktop_clients, 1);
    wait_until(cx, "the global assistant to be shown and reported", |cx| {
        looked_at_assistant(&view, cx, global)
    });
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(global));
    assert_eq!(
        view.read_with(cx, |wb, _| {
            wb.state()
                .panes()
                .iter()
                .map(|p| p.items.len())
                .sum::<usize>()
        }),
        tabs_before,
        "no tab was opened for an assistant conversation"
    );
}

#[gpui::test]
fn a_focus_report_cut_off_by_a_connection_loss_is_made_again_on_the_next_connection(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let mut daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let watched: Vec<_> = (0..3).map(|_| daemon.create_chat(&env.cwd())).collect();
    let control = daemon.create_chat(&env.cwd());
    let connection = daemon.proxied_connection();
    let (view, cx) = open(cx, &daemon, connection, 4);

    for session in &watched {
        // The focus changes and the link is cut at once: the report is in flight or fails, and
        // the connection that follows must be told, not stamped with the old request's answer.
        view.update_in(cx, |wb, window, cx| wb.open_session(session.id, window, cx));
        daemon.proxy().block(true);
        daemon.proxy().sever();
        wait_until(cx, "the loss to be seen", |cx| {
            view.read_with(cx, |wb, cx| {
                matches!(
                    wb.sessions().read(cx).connection_state(),
                    ConnectionState::Reconnecting { .. }
                )
            })
        });
        daemon.proxy().block(false);
        wait_until(cx, "the focus to be reported on the new connection", |cx| {
            reported(&view, cx) == Some(Some(session.id))
        });
        // The daemon really has it: the turn that ends now raises nothing.
        send(&daemon, session.id);
        wait_until(cx, "the watched turn to end", |_| {
            turn_ended(&daemon, session.id)
        });
    }
    assert!(intents(&daemon).is_empty(), "{:?}", intents(&daemon));

    // The control: a session nobody looks at still raises.
    send(&daemon, control.id);
    wait_until(cx, "the control's intent", |_| {
        intents(&daemon).iter().any(|i| i.session_id == control.id)
    });
    assert_eq!(intents(&daemon).len(), 1);
}
