#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! What a daemon restart did to the sessions, in the desktop (protocol 0.26, capability
//! `session.recovery`): the summary that comes up once per restart, the chips of the sidebar
//! rows, terminal and chat headers, and the actions that route to the daemon. Daemons are
//! in-process on a temporary `HUTAN_HOME` and are stopped and started again under a connected
//! workbench.

use crate::common;

use std::time::Duration;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::ConnectionState;
use hutan_core::{SessionId, SessionState};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{BadgeSite, Region, Workbench};
use hutan_protocol::messages::SessionInfo;
use hutan_protocol::recovery::{LastTurn, RecoveryAck, RecoveryAcked, RecoveryKind};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    sessions: usize,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        bounds(view, cx, region).is_some()
    });
    let at = bounds(view, cx, region).unwrap().center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

fn summary_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.recovery_summary_open())
}

fn recovery_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Option<RecoveryKind> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(id)
            .and_then(|s| s.recovery.as_ref().map(|r| r.kind.clone()))
    })
}

fn connected_again(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    old: hutan_core::Epoch,
) -> bool {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store.connection_state() == ConnectionState::Connected
            && store.state().epoch.is_some_and(|epoch| epoch != old)
    })
}

fn epoch(view: &Entity<Workbench>, cx: &VisualTestContext) -> hutan_core::Epoch {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch)
        .expect("a synchronised epoch")
}

/// Stops the daemon and starts a new one on its home; returns once the workbench follows it.
fn restart(view: &Entity<Workbench>, cx: &mut VisualTestContext, daemon: &mut TestDaemon) {
    restart_with(view, cx, daemon, |_| {});
}

fn restart_with(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    daemon: &mut TestDaemon,
    tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig),
) {
    let old = epoch(view, cx);
    daemon.stop();
    daemon.restart_with(tweak);
    wait_until(cx, "the workbench to follow the new daemon", |cx| {
        connected_again(view, cx, old)
    });
}

fn ack(daemon: &TestDaemon, session: Option<SessionId>) -> Vec<SessionId> {
    daemon
        .call::<RecoveryAcked>(hutan_protocol::messages::Request::SessionRecoveryAck(
            RecoveryAck {
                session_id: session,
            },
        ))
        .sessions
}

fn session_of(daemon: &TestDaemon, id: SessionId) -> SessionInfo {
    daemon.session(id)
}

#[gpui::test]
fn one_summary_per_restart_chips_until_acknowledged_and_dismissals_clear_them(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let a = daemon.create_session(&["cat"], None);
    let b = daemon.create_session(&["cat"], None);
    // Ended before the restart: listed for history, nothing to say.
    let ended = daemon.sh("exit 0");
    let (view, cx) = open(cx, &daemon, 3);
    assert!(!summary_open(&view, cx), "a first start restored nothing");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary()),
        None
    );

    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    let summary = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary())
        .expect("a summary");
    // Two live sessions came back as new shells; the ended one is not counted.
    assert_eq!(summary.counts, "2 sessions: 2 shells respawned");
    assert!(
        summary.headline.starts_with("hutand restarted ") && !summary.headline.contains(" at "),
        "{}",
        summary.headline
    );
    assert!(
        summary.headline.contains("(it was stopped)"),
        "{}",
        summary.headline
    );
    assert!(
        !summary.headline.contains("still running"),
        "nothing ran through the restart"
    );
    assert_eq!(summary.rows.len(), 2);
    assert!(summary.rows.iter().all(|row| row.action.is_none()));
    for session in [a.id, b.id] {
        assert!(matches!(
            recovery_of(&view, cx, session),
            Some(RecoveryKind::ShellRespawned { .. })
        ));
    }
    assert_eq!(
        recovery_of(&view, cx, ended.id),
        Some(RecoveryKind::EndedBefore)
    );

    // The sidebar rows of the restored sessions wear a chip; the ended one does not.
    draw(cx);
    wait_until(cx, "the row chips", |cx| {
        [a.id, b.id]
            .iter()
            .all(|id| bounds(&view, cx, Region::RecoveryChip(BadgeSite::Row, *id)).is_some())
    });
    assert!(bounds(&view, cx, Region::RecoveryChip(BadgeSite::Row, ended.id)).is_none());

    // Each line has Open, Dismiss; the first line's Dismiss clears that session only.
    let first = summary.rows[0].session;
    let second = summary.rows[1].session;
    click(&view, cx, Region::RecoveryRow(0, "dismiss"));
    wait_until(cx, "the first session to be acknowledged", |cx| {
        recovery_of(&view, cx, first).is_none()
    });
    assert!(recovery_of(&view, cx, second).is_some());
    wait_until(cx, "its chip to go", |cx| {
        bounds(&view, cx, Region::RecoveryChip(BadgeSite::Row, first)).is_none()
    });
    assert!(bounds(&view, cx, Region::RecoveryChip(BadgeSite::Row, second)).is_some());
    assert!(summary_open(&view, cx), "one line is left");

    // Close keeps the chip; what the daemon says afterwards does not bring the card back.
    click(&view, cx, Region::Recovery("close"));
    assert!(!summary_open(&view, cx));
    assert!(recovery_of(&view, cx, second).is_some());
    assert!(session_of(&daemon, second).recovery.is_some());
    draw(cx);
    assert!(bounds(&view, cx, Region::Recovery("sheet")).is_none());
    assert_eq!(ack(&daemon, Some(second)), [second]);
    wait_until(cx, "the acknowledgement elsewhere", |cx| {
        recovery_of(&view, cx, second).is_none()
    });
    assert!(!summary_open(&view, cx));

    // A new restart is a new summary.
    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary of the second restart", |cx| {
        summary_open(&view, cx)
    });
    let again = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary())
        .unwrap();
    assert_eq!(again.counts, "2 sessions: 2 shells respawned");
    assert_ne!(again.restarted_at_ms, summary.restarted_at_ms);

    // Dismiss all acknowledges every session and takes the card down.
    click(&view, cx, Region::Recovery("dismiss-all"));
    wait_until(cx, "everything acknowledged", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().iter().all(|s| {
                s.recovery
                    .as_ref()
                    .is_none_or(|r| r.kind == RecoveryKind::EndedBefore)
            })
        })
    });
    assert!(!summary_open(&view, cx));
    assert!(session_of(&daemon, a.id).recovery.is_none());
    assert!(session_of(&daemon, b.id).recovery.is_none());
}

#[gpui::test]
fn the_summary_opens_the_session_of_a_line(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let a = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon, 1);
    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    click(&view, cx, Region::RecoveryRow(0, "open"));
    wait_until(cx, "the session's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_pane().is_some_and(|pane| {
                pane.items.iter().any(|item| {
                    matches!(item, ItemRef::Terminal { session } if *session == a.id.to_string())
                })
            })
        })
    });
}

#[gpui::test]
fn a_terminal_says_it_was_respawned_until_dismissed_or_typed_into(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let dismissed = daemon.create_session(&["cat"], None);
    let typed = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon, 2);
    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    click(&view, cx, Region::Recovery("close"));

    // The terminal's tab carries the chip and Dismiss.
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(dismissed.id, window, cx)
    });
    wait_until(cx, "the terminal's recovery bar", |cx| {
        bounds(&view, cx, Region::Recovery("terminal")).is_some()
            && bounds(
                &view,
                cx,
                Region::RecoveryChip(BadgeSite::Tab, dismissed.id),
            )
            .is_some()
    });
    click(&view, cx, Region::Recovery("terminal-dismiss"));
    wait_until(cx, "the bar to go", |cx| {
        recovery_of(&view, cx, dismissed.id).is_none()
            && bounds(&view, cx, Region::Recovery("terminal")).is_none()
    });

    // Typing is the next user action: the daemon clears it by itself.
    assert!(recovery_of(&view, cx, typed.id).is_some());
    let terminal = session_of(&daemon, typed.id).terminal_id.unwrap();
    daemon.send_input(terminal, b"hello\r");
    wait_until(cx, "the recovery to clear after typing", |cx| {
        recovery_of(&view, cx, typed.id).is_none()
    });
}

#[gpui::test]
fn a_session_that_was_not_restarted_offers_restart_in_the_summary(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start_with_config("[daemon]\nrestore_sessions = \"stopped\"\n");
    let a = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon, 1);
    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    let summary = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary())
        .unwrap();
    assert!(
        summary.counts.starts_with("1 session: 1 not restarted ("),
        "{}",
        summary.counts
    );
    assert_eq!(summary.rows[0].action.map(|a| a.label()), Some("Restart"));
    assert_eq!(session_of(&daemon, a.id).state, SessionState::Stopped);

    click(&view, cx, Region::RecoveryRow(0, "action"));
    wait_until(cx, "the session to run again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(a.id)
                .is_some_and(|s| s.state == SessionState::Running && s.recovery.is_none())
        })
    });
    wait_until(cx, "the card to go", |cx| !summary_open(&view, cx));
}

/// A chat whose first turn waits for an approval when the daemon stops, and a daemon that
/// replays the transcript of the resumed run next.
fn interrupted_chat() -> (ChatEnv, TestDaemon, SessionId) {
    let env = ChatEnv::new();
    let dir = env.work.path().join("script");
    std::fs::create_dir_all(&dir).unwrap();
    for (index, name) in ["approval-touch", "resume-turn", "resume-turn"]
        .iter()
        .enumerate()
    {
        std::fs::copy(
            common::chat::fixtures().join(format!("{name}.jsonl")),
            dir.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    let daemon = TestDaemon::start_with_codex(env.replay(&dir));
    let chat = daemon.create_chat(&env.cwd());
    let id = chat.id;
    (env, daemon, id)
}

/// The server received "go", then the user's message of the interrupted turn as the journal has
/// it (the recorded transcript says what that message was) — what Retry last message sends.
fn sent_again(env: &ChatEnv) -> bool {
    let texts = env.sent_texts();
    texts.len() == 2
        && texts[0] == "go"
        && texts[1] == "Run `touch hi.txt` in a shell, then reply with the word done."
}

fn send_and_wait_for_approval(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    daemon: &TestDaemon,
    id: SessionId,
) {
    let _: hutan_protocol::chat::ChatSent = daemon.call(
        hutan_protocol::messages::Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: id,
            text: "go".to_owned(),
            attachments: vec![],
        }),
    );
    wait_until(cx, "the approval", |cx| {
        view.read_with(cx, |wb, cx| !wb.sessions().read(cx).approvals().is_empty())
    });
}

#[gpui::test]
fn an_interrupted_turn_is_retried_from_the_summary(cx: &mut TestAppContext) {
    let (env, mut daemon, id) = interrupted_chat();
    let (view, cx) = open(cx, &daemon, 1);
    send_and_wait_for_approval(&view, cx, &daemon, id);

    let codex = env.replay(&env.work.path().join("script"));
    restart_with(&view, cx, &mut daemon, move |config| {
        config.codex = Some(codex)
    });
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    let summary = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary())
        .unwrap();
    assert_eq!(summary.counts, "1 session: 1 chat turn interrupted");
    assert_eq!(
        summary.rows[0].action.map(|a| a.label()),
        Some("Retry last message")
    );
    assert!(matches!(
        recovery_of(&view, cx, id),
        Some(RecoveryKind::ChatResumed {
            last_turn: LastTurn::Interrupted,
            ..
        })
    ));
    // The agent is up again (resumed) before the retry can be sent to it.
    wait_until(cx, "the chat to be idle", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(id)
                .is_some_and(|s| s.state == SessionState::Idle)
        })
    });

    click(&view, cx, Region::RecoveryRow(0, "action"));
    wait_until(cx, "the message to be sent again", |_| sent_again(&env));
    wait_until(cx, "the recovery to clear", |cx| {
        recovery_of(&view, cx, id).is_none()
    });
    wait_until(cx, "the card to go", |cx| !summary_open(&view, cx));
}

#[gpui::test]
fn the_chat_header_says_the_turn_was_interrupted_and_retries_it(cx: &mut TestAppContext) {
    let (env, mut daemon, id) = interrupted_chat();
    let (view, cx) = open(cx, &daemon, 1);
    send_and_wait_for_approval(&view, cx, &daemon, id);
    let codex = env.replay(&env.work.path().join("script"));
    restart_with(&view, cx, &mut daemon, move |config| {
        config.codex = Some(codex)
    });
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));
    click(&view, cx, Region::Recovery("close"));
    wait_until(cx, "the chat to be idle", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(id)
                .is_some_and(|s| s.state == SessionState::Idle)
        })
    });

    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the chat's recovery row", |cx| {
        bounds(&view, cx, Region::Chat("recovery")).is_some()
            && bounds(&view, cx, Region::RecoveryChip(BadgeSite::Tab, id)).is_some()
    });
    click(&view, cx, Region::Chat("retry-last"));
    wait_until(cx, "the message to be sent again", |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        sent_again(&env)
    });
    wait_until(cx, "the row to go", |cx| {
        recovery_of(&view, cx, id).is_none()
            && bounds(&view, cx, Region::Chat("recovery")).is_none()
    });
}

#[gpui::test]
fn a_large_sidebar_shows_restart_chips_as_icons_and_the_summary_as_one_row(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let ids: Vec<SessionId> = (0..31)
        .map(|_| daemon.create_session(&["cat"], None).id)
        .collect();
    let (view, cx) = open(cx, &daemon, ids.len());
    restart(&view, cx, &mut daemon);
    wait_until(cx, "the summary", |cx| summary_open(&view, cx));

    // One row: how many came back, with Review and Dismiss all; no list yet.
    wait_until(cx, "the one-row summary", |cx| {
        bounds(&view, cx, Region::Recovery("review")).is_some()
    });
    assert!(bounds(&view, cx, Region::RecoveryRow(0, "open")).is_none());
    let headline = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).recovery_summary())
        .unwrap()
        .counts;
    assert!(headline.starts_with("31 sessions"), "{headline}");

    // The chips of the rows that are on screen are icons with a tooltip, not words.
    let chip = ids
        .iter()
        .find_map(|id| bounds(&view, cx, Region::RecoveryChip(BadgeSite::Row, *id)))
        .expect("a restarted row is on screen");
    assert!(
        chip.size.width < gpui::px(24.0),
        "a compact chip is a glyph: {chip:?}"
    );

    // Review brings the list back.
    click(&view, cx, Region::Recovery("review"));
    wait_until(cx, "the list", |cx| {
        bounds(&view, cx, Region::RecoveryRow(0, "open")).is_some()
    });
    ack(&daemon, None);
}
