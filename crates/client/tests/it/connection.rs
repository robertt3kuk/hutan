//! `Connection` over a real in-process daemon, with a proxy that severs the link on demand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::*;
use hutan_client::{Connection, ConnectionState, GoneReason, Notice, TerminalUpdate};
use hutan_core::SessionState;
use hutan_daemon::bus::RingLimits;
use hutan_protocol::messages::{
    DaemonStatusInfo, Empty, Request, SessionRef, SessionResult, TerminalColors,
};
use tokio::time::{sleep, timeout};

/// Blocking wait for a plain thread or `futures::executor::block_on` (no Tokio timers there).
fn wait_connected_blocking(connection: &Connection) {
    let state = connection.state();
    let deadline = Instant::now() + WAIT;
    while state.borrow().connection != ConnectionState::Connected {
        assert!(Instant::now() < deadline, "never connected");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn daemon_runtime() -> (tokio::runtime::Runtime, Env) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let env = runtime.block_on(Env::start());
    (runtime, env)
}

#[test]
fn start_and_call_from_a_plain_thread() {
    let (runtime, env) = daemon_runtime();
    let opts = env.options();
    let worker = std::thread::spawn(move || {
        // No Tokio context exists on this thread.
        assert!(tokio::runtime::Handle::try_current().is_err());
        let connection = Connection::start(opts);
        wait_connected_blocking(&connection);
        let status: DaemonStatusInfo =
            futures::executor::block_on(connection.call(Request::DaemonStatus(Empty {}))).unwrap();
        let state = connection.state().borrow().clone();
        assert_eq!(state.epoch, Some(status.epoch));
        assert_eq!(state.workspaces.len(), 1, "the default workspace");
        assert_eq!(state.workspaces[0].name, "Home");
        assert!(state.sessions.is_empty());
        connection.shutdown();
    });
    worker.join().unwrap();
    drop(runtime);
}

#[test]
fn start_and_call_under_futures_block_on() {
    let (runtime, env) = daemon_runtime();
    let opts = env.options();
    futures::executor::block_on(async move {
        let connection = Connection::start(opts);
        wait_connected_blocking(&connection);
        let status: DaemonStatusInfo = connection
            .call(Request::DaemonStatus(Empty {}))
            .await
            .unwrap();
        assert_eq!(connection.state().borrow().epoch, Some(status.epoch));
        // A call that fails remotely still surfaces as a remote error, not a hang.
        let missing = connection
            .call::<SessionResult>(Request::SessionStop(SessionRef {
                session_id: hutan_core::SessionId::new(),
            }))
            .await;
        assert!(matches!(missing, Err(hutan_client::ClientError::Remote(_))));
        connection.shutdown();
    });
    drop(runtime);
}

#[tokio::test(flavor = "multi_thread")]
async fn call_while_not_connected_fails_instead_of_queueing() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;
    env.proxy.block();
    env.proxy.sever();
    wait_reconnecting(&mut state).await;
    let result = connection
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await;
    assert!(
        matches!(result, Err(hutan_client::ClientError::Disconnected)),
        "{result:?}"
    );
    env.proxy.unblock();
    wait_connected(&mut state).await;
    connection
        .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
        .await
        .unwrap();
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn sever_reconnects_and_replays_missed_events_without_duplicates() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    let mut notices = connection.notices();
    let first = wait_connected(&mut state).await;
    let epoch = first.epoch.unwrap();

    let direct = env.direct().await;
    let before = create_workspace(&direct, "before").await;
    wait_state(&mut state, "first workspace", |s| s.workspaces.len() == 2).await;

    env.proxy.block();
    env.proxy.sever();
    let reconnecting = wait_reconnecting(&mut state).await;
    assert_eq!(
        reconnecting.workspaces.len(),
        2,
        "the last known view stays while reconnecting"
    );

    // Published while the connection is down.
    let during_a = create_workspace(&direct, "during-a").await;
    let during_b = create_workspace(&direct, "during-b").await;
    let session = create_session(&direct, spec(&["sh", "-c", "exit 3"])).await;

    env.proxy.unblock();
    let state_after = wait_state(&mut state, "replayed state", |s| {
        s.connection == ConnectionState::Connected
            && s.workspaces.len() == 4
            && s.sessions.iter().any(|x| x.id == session.id)
    })
    .await;
    assert_eq!(state_after.epoch, Some(epoch), "same daemon, same epoch");
    let names: Vec<_> = state_after
        .workspaces
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(names, ["Home", "before", "during-a", "during-b"]);
    let ids = workspace_ids(&state_after.workspaces);
    assert_eq!(
        ids.iter().collect::<HashSet<_>>().len(),
        ids.len(),
        "no duplicates"
    );
    assert_eq!(state_after.workspaces[1].id, before.id);
    assert_eq!(state_after.workspaces[2].id, during_a.id);
    assert_eq!(state_after.workspaces[3].id, during_b.id);
    assert_eq!(state_after.sessions.len(), 1);

    // The replay also carried the exit, which is a transient notice.
    let code = wait_notice(&mut notices, "replayed exit", |n| match n {
        Notice::SessionExited {
            session: id, code, ..
        } if *id == session.id => Some(*code),
        _ => None,
    })
    .await;
    assert_eq!(code, Some(3));

    // The state converges on a fresh snapshot.
    let (workspaces, sessions) = expected_view(snapshot(&direct).await);
    let settled = wait_state(&mut state, "equal to a fresh snapshot", |s| {
        s.workspaces == workspaces && s.sessions == sessions
    })
    .await;
    assert_eq!(settled.connection, ConnectionState::Connected);
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn evicted_ring_falls_back_to_a_snapshot() {
    let env = Env::start_with_ring(Some(RingLimits {
        max_events: 2,
        max_bytes: usize::MAX,
    }))
    .await;
    let connection = connect(&env);
    let mut state = connection.state();
    let mut notices = connection.notices();
    let epoch = wait_connected(&mut state).await.epoch.unwrap();

    env.proxy.block();
    env.proxy.sever();
    wait_reconnecting(&mut state).await;

    let direct = env.direct().await;
    for name in ["a", "b", "c", "d"] {
        create_workspace(&direct, name).await;
    }
    let session = create_session(&direct, spec(&["sh", "-c", "exit 0"])).await;

    // The exit must be published while the connection is down, or it would arrive live.
    timeout(WAIT, async {
        loop {
            let view = snapshot(&direct).await;
            let exited = view
                .sessions
                .iter()
                .any(|s| s.id == session.id && matches!(s.state, SessionState::Exited { .. }));
            if exited {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session never exited");
    env.proxy.unblock();
    let (workspaces, sessions) = {
        wait_state(&mut state, "snapshot state", |s| {
            s.connection == ConnectionState::Connected
                && s.workspaces.len() == 5
                && s.sessions
                    .iter()
                    .any(|x| x.id == session.id && matches!(x.state, SessionState::Exited { .. }))
        })
        .await;
        expected_view(snapshot(&direct).await)
    };
    let settled = wait_state(&mut state, "equal to a fresh snapshot", |s| {
        s.workspaces == workspaces && s.sessions == sessions
    })
    .await;
    assert_eq!(settled.epoch, Some(epoch));

    // Events the ring no longer had are not replayed, so there is no exit notice: the exit is
    // only visible through the snapshot.
    sleep(Duration::from_millis(200)).await;
    while let Ok(notice) = notices.try_recv() {
        assert!(
            !matches!(notice, Notice::SessionExited { .. }),
            "snapshot path must not replay events: {notice:?}"
        );
    }
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_restart_resets_state_and_ends_terminal_handles() {
    let mut env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    let mut notices = connection.notices();
    let old_epoch = wait_connected(&mut state).await.epoch.unwrap();

    let direct = env.direct().await;
    create_workspace(&direct, "old-world").await;
    let session = create_session(&direct, spec(&["cat"])).await;
    wait_state(&mut state, "session known", |s| {
        s.sessions.len() == 1 && s.workspaces.len() == 2
    })
    .await;
    let mut view = View::new(connection.attach(terminal_of(&session), Some(size(80, 24))));
    view.wait_screen("first snapshot", |s| s.snapshots == 1)
        .await;
    drop(direct);

    env.stop_daemon().await;
    wait_reconnecting(&mut state).await;
    env.start_daemon().await;

    let restarted = wait_state(&mut state, "new daemon", |s| {
        s.connection == ConnectionState::Connected && s.epoch.is_some_and(|e| e != old_epoch)
    })
    .await;
    let new_epoch = restarted.epoch.unwrap();
    // The daemon restores its stored state: the same session runs again, in a new terminal (ids count from 1 per daemon).
    assert_eq!(restarted.sessions.len(), 1, "{:?}", restarted.sessions);
    assert_eq!(restarted.sessions[0].id, session.id);
    assert!(
        restarted.sessions[0].state.is_live(),
        "{:?}",
        restarted.sessions
    );
    assert_eq!(
        restarted.workspaces.len(),
        2,
        "the default workspace and old-world"
    );

    let (old, new) = wait_notice(&mut notices, "daemon restarted", |n| match n {
        Notice::DaemonRestarted { old, new } => Some((*old, *new)),
        _ => None,
    })
    .await;
    assert_eq!((old, new), (old_epoch, new_epoch));

    let reason = view
        .wait_update("gone", |u| match u {
            TerminalUpdate::Gone { reason } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, GoneReason::EpochChanged);
    assert!(
        view.handle.updates().recv().await.is_none(),
        "nothing follows Gone"
    );

    let direct = env.direct().await;
    let (workspaces, sessions) = expected_view(snapshot(&direct).await);
    assert_eq!(restarted.workspaces, workspaces);
    assert_eq!(restarted.sessions, sessions);
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_handle_survives_a_sever_and_input_still_reaches_the_pty() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;

    let direct = env.direct().await;
    let session = create_session(&direct, spec(&["cat"])).await;
    let mut view = View::new(connection.attach(terminal_of(&session), Some(size(60, 10))));
    view.wait_screen("first snapshot", |s| s.snapshots == 1)
        .await;

    view.handle.input(b"one\n".to_vec());
    view.wait_screen("first input echoed", |s| s.contains("one"))
        .await;

    env.proxy.block();
    env.proxy.sever();
    view.wait_update("reconnecting", |u| {
        matches!(u, TerminalUpdate::Reconnecting).then_some(())
    })
    .await;
    wait_reconnecting(&mut state).await;
    // Dropped, not queued: nothing is connected.
    view.handle.input(b"lost\n".to_vec());

    env.proxy.unblock();
    wait_connected(&mut state).await;
    let snapshots = view.screen.snapshots;
    view.wait_screen("snapshot after reconnect", |s| s.snapshots == snapshots + 1)
        .await;
    assert_eq!(view.reconnects, 1);
    assert!(view.screen.contains("one"), "{}", view.screen.text());
    assert!(!view.screen.contains("lost"), "{}", view.screen.text());

    view.handle.input(b"two\n".to_vec());
    view.wait_screen("input after reconnect reaches the pty", |s| {
        s.contains("two")
    })
    .await;
    assert!(!view.screen.contains("lost"), "{}", view.screen.text());
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn colors_set_while_disconnected_are_sent_again_after_the_reattach() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;

    // The child waits for a byte, queries OSC 11 and shows the raw reply with `cat -v`.
    let reply_len = "\x1b]11;rgb:aaaa/bbbb/cccc\x07".len();
    let script = format!(
        r"stty raw -echo; printf READY; dd bs=1 count=1 2>/dev/null; printf '\033]11;?\007'; dd bs=1 count={reply_len} 2>/dev/null | cat -v; printf '\nDONE\n'"
    );
    let direct = env.direct().await;
    let session = create_session(&direct, spec(&["sh", "-c", &script])).await;
    let mut view = View::new(connection.attach(terminal_of(&session), Some(size(80, 10))));
    view.wait_screen("ready", |s| s.contains("READY")).await;

    let colors = |background| TerminalColors {
        foreground: (1, 2, 3),
        background,
        cursor: (4, 5, 6),
    };
    view.handle.set_colors(colors((0x11, 0x22, 0x33)));

    env.proxy.block();
    env.proxy.sever();
    view.wait_update("reconnecting", |u| {
        matches!(u, TerminalUpdate::Reconnecting).then_some(())
    })
    .await;
    wait_reconnecting(&mut state).await;
    // Remembered while nothing is connected; the daemon still holds the older colors.
    view.handle.set_colors(colors((0xaa, 0xbb, 0xcc)));

    env.proxy.unblock();
    wait_connected(&mut state).await;
    let snapshots = view.screen.snapshots;
    view.wait_screen("snapshot after reconnect", |s| s.snapshots == snapshots + 1)
        .await;
    // Sent on the same connection after the colors, so the child's query sees them.
    view.handle.input(b"g".to_vec());
    view.wait_screen("the reply", |s| s.contains("DONE")).await;
    assert!(
        view.screen.contains("^[]11;rgb:aaaa/bbbb/cccc^G"),
        "{}",
        view.screen.text()
    );
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_restart_ends_the_old_handle_with_the_new_terminal() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;

    let session = create_session(&env.direct().await, spec(&["sh", "-c", "exit 0"])).await;
    wait_state(&mut state, "session exited", |s| {
        s.sessions
            .iter()
            .any(|x| x.id == session.id && matches!(x.state, SessionState::Exited { .. }))
    })
    .await;
    // Ended sessions keep their final screen attachable.
    let mut view = View::new(connection.attach(terminal_of(&session), None));
    view.wait_screen("final screen", |s| s.snapshots == 1).await;

    let restarted: SessionResult = connection
        .call(Request::SessionRestart(SessionRef {
            session_id: session.id,
        }))
        .await
        .unwrap();
    let new_terminal = terminal_of(&restarted.session);
    assert_ne!(new_terminal, terminal_of(&session));

    let reason = view
        .wait_update("gone", |u| match u {
            TerminalUpdate::Gone { reason } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, GoneReason::SessionRestarted { new_terminal });
    assert!(view.handle.updates().recv().await.is_none());

    // The state follows the restart too.
    let current = state.borrow().clone();
    let now = current
        .sessions
        .iter()
        .find(|s| s.id == session.id)
        .unwrap();
    assert_eq!(now.terminal_id, Some(new_terminal));
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn attaching_an_unknown_terminal_reports_not_found() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;
    let mut view = View::new(connection.attach(hutan_core::TerminalId(987_654), None));
    let reason = view
        .wait_update("gone", |u| match u {
            TerminalUpdate::Gone { reason } => Some(*reason),
            _ => None,
        })
        .await;
    assert_eq!(reason, GoneReason::NotFound);
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handle_created_while_disconnected_attaches_once_connected() {
    let env = Env::start().await;
    env.proxy.block();
    let connection = connect(&env);
    let mut state = connection.state();
    wait_reconnecting(&mut state).await;

    let direct = env.direct().await;
    let session = create_session(&direct, spec(&["cat"])).await;
    let mut view = View::new(connection.attach(terminal_of(&session), None));

    env.proxy.unblock();
    wait_connected(&mut state).await;
    view.wait_screen("snapshot", |s| s.snapshots == 1).await;
    view.handle.input(b"hi\n".to_vec());
    view.wait_screen("echo", |s| s.contains("hi")).await;
    connection.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_handle_detaches_and_shutdown_stops_everything() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    wait_connected(&mut state).await;
    let session = create_session(&env.direct().await, spec(&["cat"])).await;
    let view = View::new(connection.attach(terminal_of(&session), None));
    let mut view = view;
    view.wait_screen("snapshot", |s| s.snapshots == 1).await;
    let View { handle, .. } = view;
    drop(handle);

    let clone = connection.clone();
    connection.shutdown();
    assert_eq!(clone.state().borrow().connection, ConnectionState::Stopped);
    assert!(matches!(
        clone
            .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
            .await,
        Err(hutan_client::ClientError::Disconnected)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn resource_samples_arrive_as_notices_and_a_reconnect_subscribes_again() {
    let env = Env::start().await;
    let connection = connect(&env);
    let mut state = connection.state();
    let mut notices = connection.notices();
    wait_connected(&mut state).await;
    assert!(connection.supports_resources());

    let first = connection.resources_subscribe(Some(1000)).await.unwrap();
    assert_eq!(first.interval_ms, 1000);
    // The periodic samples come as notices, although the event's seq is one the connection
    // has applied already.
    let sample = wait_notice(&mut notices, "a resource sample", |notice| match notice {
        Notice::ResourcesUpdated { sample } => Some(sample.at_ms),
        _ => None,
    })
    .await;
    assert!(sample >= first.sample.at_ms);

    // The link drops: the subscription died with it; the new connection asks again by itself
    // and announces the answer's sample.
    env.proxy.block();
    env.proxy.sever();
    wait_reconnecting(&mut state).await;
    // Wishes survive a disconnect: the call fails, the wish stays.
    assert!(matches!(
        connection.resources_subscribe(Some(1000)).await,
        Err(hutan_client::ClientError::Disconnected)
    ));
    env.proxy.unblock();
    wait_connected(&mut state).await;
    wait_notice(
        &mut notices,
        "a sample after the reconnect",
        |notice| match notice {
            Notice::ResourcesUpdated { sample } => Some(sample.at_ms),
            _ => None,
        },
    )
    .await;

    // Unsubscribed: nothing is announced, and a reconnect does not subscribe again.
    connection.resources_unsubscribe().await;
    sleep(Duration::from_millis(300)).await;
    while notices.try_recv().is_ok() {}
    env.proxy.block();
    env.proxy.sever();
    wait_reconnecting(&mut state).await;
    env.proxy.unblock();
    wait_connected(&mut state).await;
    sleep(Duration::from_millis(2500)).await;
    while let Ok(notice) = notices.try_recv() {
        assert!(
            !matches!(notice, Notice::ResourcesUpdated { .. }),
            "no samples after unsubscribing"
        );
    }
    connection.shutdown();
}
