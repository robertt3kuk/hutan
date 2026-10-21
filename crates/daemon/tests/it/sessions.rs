//! Terminal sessions end to end: the daemon runs in-process on a temporary `HUTAN_HOME` and
//! real shells run in real PTYs; everything is driven over the daemon's Unix socket with
//! `hutan-client`. Tests 1–9 mirror the acceptance list of `docs/briefs/M1-D1.md`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use common::*;
use hutan_client::{Client, ConnectOptions};
use hutan_core::{SessionId, SessionState, TerminalId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonShutdown, DaemonStatusInfo, Empty, Event, Request, SessionList,
    SessionListFilter, SessionRef, SessionRename, SessionResult, SessionSnapshot, TerminalAck,
    TerminalColors, TerminalHistory, TerminalHistoryRows, TerminalRef, TerminalResize,
    TerminalSetColors, WorkspaceCreate, WorkspaceInfo, WorkspaceList, WorkspaceRef,
};
use hutan_protocol::render::TerminalRender;
use tokio::time::{sleep, timeout};

const QUIET: Duration = Duration::from_millis(500);

async fn status(client: &Client) -> DaemonStatusInfo {
    timeout(
        Duration::from_secs(3),
        client.call(Request::DaemonStatus(Empty {})),
    )
    .await
    .expect("daemon.status answered in time")
    .unwrap()
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_gone(pid: u32) {
    timeout(WAIT, async {
        while process_exists(pid) {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("process {pid} is still running"));
}

/// Creates `echo PID=$$; exec cat` and returns the session with the shell's pid.
async fn create_with_pid(client: &Client) -> (hutan_protocol::messages::SessionInfo, u32) {
    let session = create(client, sh("echo PID=$$; exec cat")).await;
    let mut view = View::attach(client, terminal_of(&session), None).await;
    view.wait_for("pid line", |s| {
        s.after("PID=").is_some_and(|p| !p.is_empty())
    })
    .await;
    let pid = view.screen.after("PID=").unwrap().parse().unwrap();
    (session, pid)
}

// 1 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn create_attach_snapshot_and_typed_input() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;

    let session = create(&client, sh("echo hello; exec cat")).await;
    assert_eq!(session.state, SessionState::Running);
    assert_eq!(session.title, "sh");
    assert_eq!(session.terminal_id, Some(TerminalId(1)), "ids start at 1");
    let tid = terminal_of(&session);

    let mut view = View::attach(&client, tid, None).await;
    view.wait_for("hello on screen", |s| s.contains("hello"))
        .await;

    // A later attach starts from a snapshot that already holds the output.
    let second = View::attach(&client, tid, None).await;
    assert!(second.screen.contains("hello"), "{}", second.screen.text());
    drop(view);
    let mut view = second;

    client.send_input(tid, b"ping\r").await.unwrap();
    view.wait_for("typed text rendered", |s| s.contains("ping"))
        .await;
    t.stop().await;
}

// 2 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ack_pacing_keeps_floods_bounded_and_the_daemon_responsive() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let observer_client = t.connect().await;

    // A flood plus a marker printed when it is over. 2 MB is 1 M lines of `yes`: 70 times the
    // retained history (10 000 lines + the engine's 4 096 headroom) and 30 times the reader's
    // 64 KiB buffer, so it cannot be held anywhere between the child and the client. (The
    // brief's 20 MB took 15–16 s in a debug build alone and over a minute on a busy machine.)
    let session = create(
        &client,
        sh("yes | head -c 2000000; echo FLOOD-DONE; exec cat"),
    )
    .await;
    let tid = terminal_of(&session);

    let mut view = View::attach(&client, tid, None).await;
    view.auto_ack = false;
    assert_eq!(view.screen.snapshots, 1);

    // Credit is spent by the snapshot: nothing follows until it is acked, however much the
    // child prints, and the daemon keeps answering.
    for _ in 0..4 {
        view.expect_silence(Duration::from_millis(150)).await;
        status(&client).await;
    }

    // Acking the snapshot buys exactly one render, then silence again.
    view.ack().await;
    let render = view.next_render().await;
    assert!(matches!(render, TerminalRender::Frame(_)), "{render:?}");
    assert_eq!(view.screen.frame, view.attached.frame + 1);
    view.expect_silence(QUIET).await;
    status(&client).await;

    // A well-behaved second client sees the flood finish: the marker line ends up directly
    // above the cursor row, and nothing else is printed after it.
    let finished = |s: &Screen| {
        s.lines
            .get(usize::from(s.rows).saturating_sub(2))
            .is_some_and(|line| line.trim() == "FLOOD-DONE")
    };
    let mut observer = View::attach(&observer_client, tid, None).await;
    // No wall-clock bound on the flood: every render must arrive within `WAIT` of the last.
    observer.wait_for_progress("flood finished", finished).await;
    status(&observer_client).await;

    // The next render after acking reflects the latest screen, not the intermediate one.
    view.ack().await;
    view.next_render().await;
    assert!(finished(&view.screen), "{}", view.screen.text());
    assert_eq!(view.screen.lines, observer.screen.lines);
    t.stop().await;
}

// 3 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_see_the_same_output_and_detach_is_independent() {
    let t = TestDaemon::start().await;
    let a = t.connect().await;
    let b = t.connect().await;
    let session = create(&a, sh("exec cat")).await;
    let tid = terminal_of(&session);

    let mut view_a = View::attach(&a, tid, None).await;
    let mut view_b = View::attach(&b, tid, None).await;

    a.send_input(tid, b"one\r").await.unwrap();
    view_a.wait_for("a sees one", |s| s.contains("one")).await;
    view_b.wait_for("b sees one", |s| s.contains("one")).await;

    a.call::<Empty>(Request::TerminalDetach(TerminalRef { terminal_id: tid }))
        .await
        .unwrap();
    // Everything rendered before the detach response is already delivered; nothing follows.
    view_a.drain();

    b.send_input(tid, b"two\r").await.unwrap();
    view_b.wait_for("b sees two", |s| s.contains("two")).await;
    view_a.expect_silence(QUIET).await;
    assert!(!view_a.screen.contains("two"));

    // The detached client can attach again and gets the current screen.
    let again = View::attach(&a, tid, None).await;
    assert!(again.screen.contains("two"));
    t.stop().await;
}

// 4 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn process_exit_is_reported_and_the_final_screen_stays_attachable() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;

    let session = create(&client, sh("echo bye; exit 3")).await;
    let tid = terminal_of(&session);

    let exited = wait_event(&mut events, "session.exited", |e| match e {
        Event::SessionExited {
            session_id,
            code,
            signal,
        } if *session_id == session.id => Some((*code, *signal)),
        _ => None,
    })
    .await;
    assert_eq!(exited, (Some(3), None));

    let ended = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    assert_eq!(
        ended[0].state,
        SessionState::Exited {
            code: Some(3),
            signal: None
        }
    );
    // The terminal outlives its process: attach (even with a resize request) shows the screen.
    let view = View::attach(&client, tid, Some(size(100, 30))).await;
    assert!(view.screen.contains("bye"), "{}", view.screen.text());
    // Input for an ended terminal is dropped without hurting the connection.
    client.send_input(tid, b"ignored\r").await.unwrap();
    status(&client).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_publishes_updated_then_exited_in_order() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(&client, sh("exit 7")).await;

    let mut seen = Vec::new();
    wait_event(&mut events, "exit events", |event| {
        match event {
            Event::SessionUpdated { session: s } if s.id == session.id => {
                seen.push(format!("updated:{:?}", s.state));
            }
            Event::SessionExited { code, .. } => {
                seen.push(format!("exited:{code:?}"));
                return Some(());
            }
            _ => {}
        }
        None
    })
    .await;
    assert_eq!(
        seen.last().map(String::as_str),
        Some("exited:Some(7)"),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|s| s.starts_with("updated:Exited") && s.contains("Some(7)")),
        "{seen:?}"
    );
    assert!(seen[0].starts_with("updated:Starting"), "{seen:?}");
    t.stop().await;
}

// 5 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stop_then_restart_gives_a_new_terminal_and_a_new_process() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;

    let session = create(&client, sh("echo PID=$$; exec cat")).await;
    let first = terminal_of(&session);
    let mut view = View::attach(&client, first, None).await;
    view.wait_for("first pid", |s| {
        s.after("PID=").is_some_and(|p| !p.is_empty())
    })
    .await;
    let first_pid = view.screen.after("PID=").unwrap();

    // Restarting something that runs is refused.
    assert_eq!(
        remote_code(
            client
                .call::<SessionResult>(Request::SessionRestart(SessionRef {
                    session_id: session.id
                }))
                .await
        ),
        ErrorCode::InvalidState
    );

    let stopped = stop(&client, session.id).await.unwrap();
    assert_eq!(stopped.state, SessionState::Stopped);
    wait_event(&mut events, "stopped update", |e| match e {
        Event::SessionUpdated { session: s }
            if s.id == session.id && s.state == SessionState::Stopped =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    // The stopped session keeps its screen for late readers, but cannot be stopped again.
    let late = View::attach(&client, first, None).await;
    assert!(late.screen.contains("PID="));
    assert_eq!(
        remote_code(stop(&client, session.id).await),
        ErrorCode::InvalidState
    );

    let restarted = client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: session.id,
        }))
        .await
        .unwrap()
        .session;
    assert_eq!(restarted.id, session.id);
    assert_eq!(restarted.state, SessionState::Running);
    let second = terminal_of(&restarted);
    assert!(second > first, "{second} must be newer than {first}");

    let mut view = View::attach(&client, second, None).await;
    view.wait_for("second pid", |s| {
        s.after("PID=").is_some_and(|p| !p.is_empty())
    })
    .await;
    assert_ne!(view.screen.after("PID=").unwrap(), first_pid);

    // Restart dropped the old terminal: it is gone for everyone, attachments included.
    assert_eq!(
        remote_code(
            client
                .call::<hutan_protocol::messages::TerminalAttached>(Request::TerminalAttach(
                    hutan_protocol::messages::TerminalAttach {
                        terminal_id: first,
                        size: None
                    }
                ))
                .await
        ),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_kills_a_child_that_ignores_hangup_after_the_grace_period() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(
        &client,
        sh("trap '' HUP; echo PID=$$; while :; do sleep 1; done"),
    )
    .await;
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("pid", |s| s.after("PID=").is_some_and(|p| !p.is_empty()))
        .await;
    let pid: u32 = view.screen.after("PID=").unwrap().parse().unwrap();

    let started = std::time::Instant::now();
    let stopped = stop(&client, session.id).await.unwrap();
    let took = started.elapsed();
    assert_eq!(stopped.state, SessionState::Stopped);
    assert!(
        took >= Duration::from_millis(2500),
        "SIGHUP is ignored, so the kill must wait for the 3 s grace (took {took:?})"
    );
    wait_gone(pid).await;
    t.stop().await;
}

// 6 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn resize_reaches_attached_clients_as_a_snapshot_and_the_child() {
    let t = TestDaemon::start().await;
    let a = t.connect().await;
    let b = t.connect().await;
    let session = create(&a, sh("exec sh")).await;
    let tid = terminal_of(&session);
    let mut view = View::attach(&a, tid, None).await;
    assert_eq!((view.screen.cols, view.screen.rows), (80, 24));
    view.wait_for("prompt", |s| s.contains('$'.to_string().as_str()))
        .await;
    assert_eq!(view.screen.snapshots, 1);

    a.call::<Empty>(Request::TerminalResize(TerminalResize {
        terminal_id: tid,
        size: size(100, 30),
    }))
    .await
    .unwrap();
    view.wait_for("snapshot with the new size", |s| {
        (s.cols, s.rows) == (100, 30)
    })
    .await;
    assert_eq!(
        view.screen.snapshots, 2,
        "a resize is delivered as a snapshot"
    );
    assert_eq!(view.screen.lines.len(), 30);

    a.send_input(tid, b"stty size\r").await.unwrap();
    view.wait_for("child sees the new size", |s| {
        s.lines.iter().any(|l| l.trim() == "30 100")
    })
    .await;

    // Attaching with a size resizes first (last writer wins) and the snapshot has that size;
    // the other attachment follows.
    let other = View::attach(&b, tid, Some(size(90, 20))).await;
    assert_eq!((other.screen.cols, other.screen.rows), (90, 20));
    view.wait_for("first view follows", |s| (s.cols, s.rows) == (90, 20))
        .await;

    // Invalid sizes are rejected.
    assert_eq!(
        remote_code(
            a.call::<Empty>(Request::TerminalResize(TerminalResize {
                terminal_id: tid,
                size: size(0, 10),
            }))
            .await
        ),
        ErrorCode::BadRequest
    );
    t.stop().await;
}

// 7 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn history_returns_scrollback_newest_first() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("seq 1 300; exec cat")).await;
    let tid = terminal_of(&session);
    let mut view = View::attach(&client, tid, None).await;
    view.wait_for("seq finished", |s| {
        s.lines.iter().any(|l| l.trim() == "300")
    })
    .await;

    let top: u32 = view.screen.lines[0].trim().parse().unwrap();
    assert!(top > 200, "the viewport shows the tail of the output");
    let history = |from_line, count| {
        let client = client.clone();
        async move {
            client
                .call::<TerminalHistoryRows>(Request::TerminalHistory(TerminalHistory {
                    terminal_id: tid,
                    from_line,
                    count,
                }))
                .await
                .unwrap()
        }
    };

    let near = history(0, 5).await;
    assert_eq!(near.from_line, 0);
    let texts: Vec<String> = near.rows.iter().map(row_text).collect();
    let expected: Vec<String> = (1..=5).map(|i| (top - i).to_string()).collect();
    assert_eq!(texts, expected, "line 0 is directly above the viewport");

    let deeper = history(2, 3).await;
    let texts: Vec<String> = deeper.rows.iter().map(row_text).collect();
    let expected: Vec<String> = (3..=5).map(|i| (top - i).to_string()).collect();
    assert_eq!(texts, expected);

    let all = history(0, 10_000).await;
    assert_eq!(
        all.rows.len() as u32,
        top - 1,
        "everything above the viewport"
    );
    assert_eq!(row_text(all.rows.last().unwrap()), "1");
    assert!(history(10_000, 5).await.rows.is_empty());

    // The snapshot a fresh attach receives announces the same amount.
    let fresh = View::attach(&client, tid, None).await;
    assert_eq!(fresh.screen.history_len, top - 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_is_capped_per_call_and_paging_covers_the_whole_scrollback() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // The scrollback keeps at most 10 000 lines; 12 000 printed lines fill it completely.
    let session = create(&client, sh("seq 1 12000; echo HISTORY-DONE; exec cat")).await;
    let tid = terminal_of(&session);
    let mut view = View::attach(&client, tid, None).await;
    view.wait_for("output finished", |s| s.contains("HISTORY-DONE"))
        .await;
    let history_len = view.screen.history_len;
    assert_eq!(history_len, 10_000, "the scrollback is full");

    let history = |from_line, count| {
        let client = client.clone();
        async move {
            client
                .call::<TerminalHistoryRows>(Request::TerminalHistory(TerminalHistory {
                    terminal_id: tid,
                    from_line,
                    count,
                }))
                .await
                .unwrap()
        }
    };

    // Far more than exists, and far more than a call may return: capped, connection intact.
    let first = history(0, 100_000).await;
    assert_eq!(first.rows.len(), 5_000, "count is clamped to 5 000 rows");
    status(&client).await;

    // Paging with `from_line` reaches every line, newest first, without gaps or repeats.
    let mut texts: Vec<String> = first.rows.iter().map(row_text).collect();
    while texts.len() < history_len as usize {
        let page = history(u32::try_from(texts.len()).unwrap(), 100_000).await;
        assert!(!page.rows.is_empty(), "paging stalled at {}", texts.len());
        assert!(page.rows.len() <= 5_000);
        assert_eq!(page.from_line as usize, texts.len());
        texts.extend(page.rows.iter().map(row_text));
    }
    assert_eq!(texts.len(), history_len as usize);
    let newest: u32 = texts[0].trim().parse().unwrap();
    for (offset, text) in texts.iter().enumerate() {
        assert_eq!(text.trim(), (newest - offset as u32).to_string());
    }
    assert!(history(history_len, 5).await.rows.is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_stops_at_the_encoded_size_budget_and_paging_continues() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // 2500 lines of 120 single-character runs (~3.5 KB of JSON each): well beyond the 2 MiB
    // budget within the 5 000-row cap.
    let mut wide = sh(
        "L=; for i in $(seq 60); do L=\"$L$(printf '\\033[31ma\\033[32mb')\"; done; \
         yes \"$L\" | head -n 2500; echo HISTORY-DONE; exec cat",
    );
    wide.size = Some(size(200, 24));
    let session = create(&client, wide).await;
    let tid = terminal_of(&session);
    let mut view = View::attach(&client, tid, None).await;
    view.wait_for("output finished", |s| s.contains("HISTORY-DONE"))
        .await;
    let history_len = view.screen.history_len as usize;
    assert!(history_len > 2_000, "{history_len}");

    let request = |from_line, count| {
        Request::TerminalHistory(TerminalHistory {
            terminal_id: tid,
            from_line,
            count,
        })
    };
    let first = client
        .call::<TerminalHistoryRows>(request(0, 5_000))
        .await
        .unwrap();
    assert!(!first.rows.is_empty());
    assert!(
        first.rows.len() < history_len,
        "the size budget shortens the answer: {} of {history_len}",
        first.rows.len()
    );
    let encoded = serde_json::to_vec(&first.rows).unwrap().len();
    assert!(encoded <= 2 * 1024 * 1024, "{encoded} bytes of rows");

    let mut covered = first.rows.len();
    while covered < history_len {
        let page = client
            .call::<TerminalHistoryRows>(request(u32::try_from(covered).unwrap(), 5_000))
            .await
            .unwrap();
        assert!(!page.rows.is_empty(), "paging stalled at {covered}");
        covered += page.rows.len();
    }
    assert_eq!(covered, history_len);
    status(&client).await;
    t.stop().await;
}

// 8 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn terminal_title_becomes_an_event_and_the_session_title() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(&client, sh("printf '\\033]2;hey\\007'; exec cat")).await;
    let tid = terminal_of(&session);

    let title = wait_event(&mut events, "terminal.title", |e| match e {
        Event::TerminalTitle { terminal_id, title } if *terminal_id == tid => Some(title.clone()),
        _ => None,
    })
    .await;
    assert_eq!(title.as_deref(), Some("hey"));
    wait_event(&mut events, "session title follows", |e| match e {
        Event::SessionUpdated { session: s } if s.id == session.id && s.title == "hey" => Some(()),
        _ => None,
    })
    .await;
    let listed = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap();
    assert_eq!(listed.sessions[0].title, "hey");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renamed_session_keeps_its_title_while_other_metadata_flows() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(&client, sh("exec cat")).await;
    let tid = terminal_of(&session);

    let renamed = client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: session.id,
            title: "  mine ".into(),
        }))
        .await
        .unwrap()
        .session;
    assert_eq!(renamed.title, "mine");
    assert_eq!(
        remote_code(
            client
                .call::<SessionResult>(Request::SessionRename(SessionRename {
                    session_id: session.id,
                    title: "   ".into(),
                }))
                .await
        ),
        ErrorCode::BadRequest
    );

    wait_event(&mut events, "rename update", |e| match e {
        Event::SessionUpdated { session: s } if s.title == "mine" => Some(()),
        _ => None,
    })
    .await;

    // `cat` echoes raw bytes back, so the escape sequences reach the terminal's output.
    client
        .send_input(
            tid,
            b"\x1b]2;again\x07\r\x07\r\x1b]9;note body\x07\r\x1b]777;notify;Head;Body\x07\r\x1b]7;file://h/tmp/hutan%20x\x07\r",
        )
        .await
        .unwrap();

    let mut seen_title = false;
    let mut seen_bell = false;
    let mut notes = Vec::new();
    let mut cwd = None;
    timeout(WAIT, async {
        while !(seen_title && seen_bell && notes.len() == 2 && cwd.is_some()) {
            let (_, event) = events.recv().await.unwrap();
            match event {
                Event::TerminalTitle { title, .. } => {
                    assert_eq!(title.as_deref(), Some("again"));
                    seen_title = true;
                }
                Event::TerminalBell { terminal_id } => {
                    assert_eq!(terminal_id, tid);
                    seen_bell = true;
                }
                Event::TerminalNotification { title, body, .. } => notes.push((title, body)),
                Event::SessionUpdated { session: s } => {
                    assert_eq!(
                        s.title, "mine",
                        "terminal titles never rename a named session"
                    );
                    if s.cwd.is_some() {
                        cwd = s.cwd;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .expect("metadata events in time");
    assert_eq!(
        notes,
        [
            (None, "note body".to_owned()),
            (Some("Head".to_owned()), "Body".to_owned())
        ]
    );
    assert_eq!(cwd, Some(PathBuf::from("/tmp/hutan x")));
    t.stop().await;
}

// 9 ---------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn workspace_close_is_refused_for_home_and_for_live_sessions() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let workspaces = client
        .call::<WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces;
    let default = workspaces.iter().find(|w| w.name == "Home").unwrap().id;
    let close = |id| {
        let client = client.clone();
        async move {
            client
                .call::<Empty>(Request::WorkspaceClose(WorkspaceRef { workspace_id: id }))
                .await
        }
    };
    assert_eq!(remote_code(close(default).await), ErrorCode::InvalidState);

    let other: WorkspaceInfo = client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "other".into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();
    let session = create_in(&client, Some(other.id), sh("exec cat")).await;
    assert_eq!(session.workspace_id, other.id);
    assert_eq!(remote_code(close(other.id).await), ErrorCode::InvalidState);

    // An ended session no longer blocks closing; it leaves with its workspace and terminal.
    let tid = terminal_of(&session);
    stop(&client, session.id).await.unwrap();
    close(other.id).await.unwrap();
    let sessions = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap()
        .sessions;
    assert!(sessions.is_empty(), "{sessions:?}");
    assert_eq!(
        remote_code(
            client
                .call::<Empty>(Request::TerminalDetach(TerminalRef { terminal_id: tid }))
                .await
        ),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_without_stop_sessions_is_refused_while_a_session_lives() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("exec cat")).await;

    let refused = client
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: false,
            forget: false,
        }))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert!(t.paths.socket.exists());
    assert_eq!(status(&client).await.sessions, 1);

    // Once nothing is live the same request succeeds.
    stop(&client, session.id).await.unwrap();
    client
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: false,
            forget: false,
        }))
        .await
        .unwrap();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_with_stop_sessions_terminates_children_and_removes_the_socket() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let (_, first) = create_with_pid(&client).await;
    let (_, second) = create_with_pid(&client).await;
    assert!(process_exists(first) && process_exists(second));

    client
        .call::<Empty>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions: true,
            forget: false,
        }))
        .await
        .unwrap();
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();

    assert!(!t.paths.socket.exists(), "socket must be removed");
    wait_gone(first).await;
    wait_gone(second).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_stops_sessions_and_removes_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let socket = home.join("hutand.sock");
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_hutand"))
        .env("HUTAN_HOME", &home)
        .env_remove("HUTAN_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let connect = || async {
        Client::connect(ConnectOptions {
            socket: Some(socket.clone()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: "test".into(),
                origin: None,
            },
        })
        .await
    };
    let client = timeout(WAIT, async {
        loop {
            match connect().await {
                Ok(client) => return client,
                Err(_) => sleep(Duration::from_millis(25)).await,
            }
        }
    })
    .await
    .expect("hutand started");
    let (_, pid) = create_with_pid(&client).await;
    assert!(process_exists(pid));

    let signalled = Command::new("kill")
        .args(["-TERM", &daemon.id().to_string()])
        .status()
        .unwrap();
    assert!(signalled.success());
    let exit = timeout(WAIT, async {
        loop {
            if let Some(status) = daemon.try_wait().unwrap() {
                return status;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("hutand exited after SIGTERM");
    assert!(exit.success(), "{exit:?}");
    assert!(!socket.exists());
    wait_gone(pid).await;
}

// beyond the brief's list --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn spawn_failure_yields_a_failed_session_that_is_still_listed() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(&client, spec(&["/definitely/not/a/binary"])).await;
    assert_eq!(session.terminal_id, None);
    let SessionState::Failed { reason } = &session.state else {
        panic!("expected a failed session, got {:?}", session.state);
    };
    assert!(reason.contains("/definitely/not/a/binary"), "{reason}");
    wait_event(&mut events, "failed update", |e| match e {
        Event::SessionUpdated { session: s } if matches!(s.state, SessionState::Failed { .. }) => {
            Some(())
        }
        _ => None,
    })
    .await;

    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert_eq!(snapshot.sessions, vec![session.clone()]);
    // A failed session is ended: it cannot be stopped, but it can be restarted.
    assert_eq!(
        remote_code(stop(&client, session.id).await),
        ErrorCode::InvalidState
    );
    let again = client
        .call::<SessionResult>(Request::SessionRestart(SessionRef {
            session_id: session.id,
        }))
        .await
        .unwrap()
        .session;
    assert!(matches!(again.state, SessionState::Failed { .. }));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_managed_environment_wins_over_the_spec() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let workspace = client
        .call::<WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces
        .remove(0);
    let mut wide = sh(
        "echo SID=$HUTAN_SESSION_ID WS=$HUTAN_WORKSPACE_ID SOCK=$HUTAN_SOCKET FOO=$FOO; exec cat",
    );
    wide.size = Some(size(400, 10));
    wide.env.insert("FOO".into(), "bar".into());
    wide.env.insert("HUTAN_SESSION_ID".into(), "spoofed".into());
    wide.env.insert("HUTAN_SOCKET".into(), "/spoofed".into());
    let session = create(&client, wide).await;
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("env line", |s| s.contains("FOO=")).await;
    let line = view.screen.after("SID=").unwrap();
    assert_eq!(
        line,
        format!(
            "{} WS={} SOCK={} FOO=bar",
            session.id,
            workspace.id,
            t.paths.socket.display()
        )
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn zsh_sessions_get_the_installed_shell_integration() {
    if !std::path::Path::new("/bin/zsh").exists() {
        eprintln!("skipping: zsh is not installed");
        return;
    }
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let home = tempfile::tempdir().unwrap();
    // `zsh -c` reads the injected ZDOTDIR shim's .zshenv like an interactive zsh does, which
    // restores the user's ZDOTDIR (unset here) and leaves the integration variables behind.
    let mut spec = spec(&[
        "/bin/zsh",
        "-c",
        "echo \"VER=$HUTAN_SHELL_INTEGRATION_VERSION DIR=$HUTAN_SHELL_INTEGRATION_DIR ZD=[$ZDOTDIR]\"; exec cat",
    ]);
    spec.size = Some(size(400, 10));
    spec.env
        .insert("HOME".into(), home.path().to_string_lossy().into_owned());
    let session = create(&client, spec).await;
    let mut view = View::attach(&client, terminal_of(&session), None).await;
    view.wait_for("integration line", |s| s.contains("ZD=["))
        .await;
    let root = t.paths.base.join("shell-integration").join("1");
    assert_eq!(
        view.screen.after("VER=").unwrap(),
        format!("1 DIR={} ZD=[]", root.display())
    );
    assert!(root.join("zsh/.zshenv").is_file());
    assert!(root.join("bash/hutan.bash").is_file());
    assert!(
        root.join("fish/vendor_conf.d/hutan-shell-integration.fish")
            .is_file()
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn titles_cwd_and_listing_follow_the_spec_and_filters() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;
    let other: WorkspaceInfo = client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "other".into(),
            paths: vec![],
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap();

    let plain = create(&client, spec(&["/bin/cat"])).await;
    assert_eq!(plain.title, "cat", "argv[0] basename");
    assert_eq!(plain.command, ["/bin/cat"]);
    let mut named = sh("exec cat");
    named.title = Some("build".into());
    named.cwd = Some(PathBuf::from("/tmp"));
    let named = create_in(&client, Some(other.id), named).await;
    assert_eq!(named.title, "build");
    assert_eq!(named.cwd, Some(PathBuf::from("/tmp")));

    // A title the client chose is never overwritten by the program.
    client
        .send_input(terminal_of(&named), b"\x1b]2;prog\x07\r")
        .await
        .unwrap();
    wait_event(&mut events, "terminal.title", |e| {
        matches!(e, Event::TerminalTitle { .. }).then_some(())
    })
    .await;

    let all = client
        .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
        .await
        .unwrap();
    assert_eq!(all.sessions.len(), 2);
    let filtered = client
        .call::<SessionList>(Request::SessionList(SessionListFilter {
            workspace_id: Some(other.id),
        }))
        .await
        .unwrap();
    assert_eq!(filtered.sessions.len(), 1);
    assert_eq!(filtered.sessions[0].title, "build");

    // List and snapshot carry the seq of the latest event.
    let mut last = 0;
    while let Ok((seq, _)) = events.try_recv() {
        last = seq.0;
    }
    assert_eq!(all.seq.0, last.max(all.seq.0));
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    assert!(snapshot.seq.0 >= last);
    assert_eq!(snapshot.sessions.len(), 2);

    assert_eq!(
        remote_code(
            client
                .call::<SessionResult>(Request::SessionCreate(
                    hutan_protocol::messages::SessionCreate {
                        workspace_id: Some(hutan_core::WorkspaceId::new()),
                        spec: hutan_protocol::messages::SessionSpec::Terminal(spec(&["cat"])),
                    }
                ))
                .await
        ),
        ErrorCode::NotFound
    );
    let mut zero = spec(&["cat"]);
    zero.size = Some(size(0, 24));
    assert_eq!(
        remote_code(
            client
                .call::<SessionResult>(Request::SessionCreate(
                    hutan_protocol::messages::SessionCreate {
                        workspace_id: None,
                        spec: hutan_protocol::messages::SessionSpec::Terminal(zero),
                    }
                ))
                .await
        ),
        ErrorCode::BadRequest
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_ids_are_not_found() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let ghost = TerminalId(4242);
    let attach = client
        .call::<Empty>(Request::TerminalAttach(
            hutan_protocol::messages::TerminalAttach {
                terminal_id: ghost,
                size: None,
            },
        ))
        .await;
    assert_eq!(remote_code(attach), ErrorCode::NotFound);
    for request in [
        Request::TerminalDetach(TerminalRef { terminal_id: ghost }),
        Request::TerminalResize(TerminalResize {
            terminal_id: ghost,
            size: size(80, 24),
        }),
        Request::TerminalAck(TerminalAck {
            terminal_id: ghost,
            frame: 1,
        }),
        Request::TerminalHistory(TerminalHistory {
            terminal_id: ghost,
            from_line: 0,
            count: 1,
        }),
    ] {
        assert_eq!(
            remote_code(client.call::<Empty>(request.clone()).await),
            ErrorCode::NotFound,
            "{request:?}"
        );
    }
    let ghost_session = SessionId::new();
    for request in [
        Request::SessionStop(SessionRef {
            session_id: ghost_session,
        }),
        Request::SessionRestart(SessionRef {
            session_id: ghost_session,
        }),
        Request::SessionRename(SessionRename {
            session_id: ghost_session,
            title: "x".into(),
        }),
    ] {
        assert_eq!(
            remote_code(client.call::<Empty>(request.clone()).await),
            ErrorCode::NotFound,
            "{request:?}"
        );
    }
    // Input for a terminal that does not exist is dropped; the connection survives.
    client.send_input(ghost, b"x").await.unwrap();
    status(&client).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_never_reads_stalls_only_the_connection_feeding_it() {
    let t = TestDaemon::start().await;
    let feeder = t.connect().await;
    let other = t.connect().await;
    let session = create(&feeder, spec(&["sleep", "60"])).await;
    let tid = terminal_of(&session);

    // Far more (72 MiB) than every queue between the socket and the PTY can hold. Whole lines,
    // because a tty drops input that never forms a line once its buffer is full instead of
    // blocking the writer.
    let feeder_task = feeder.clone();
    let pump = tokio::spawn(async move {
        feeder_task
            .send_input(tid, &b"filler line\n".repeat(6 * 1024 * 1024))
            .await
    });
    sleep(Duration::from_millis(1000)).await;

    // The daemon stopped reading the feeder's socket, so even a status call on that connection
    // waits behind the input...
    let stalled = timeout(
        Duration::from_millis(1000),
        feeder.call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {})),
    )
    .await;
    assert!(
        stalled.is_err(),
        "input must be back-pressured, not buffered without bound"
    );
    // ...while every other connection is unaffected and can look at the terminal.
    status(&other).await;
    let view = View::attach(&other, tid, None).await;
    assert_eq!(view.screen.cols, 80);

    // Ending the session unblocks the feeder: the rest of its input is dropped.
    stop(&other, session.id).await.unwrap();
    timeout(WAIT, pump)
        .await
        .expect("feeder unblocked")
        .unwrap()
        .unwrap();
    status(&feeder).await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_connection_ends_its_attachments_and_not_the_session() {
    let t = TestDaemon::start().await;
    let owner = t.connect().await;
    let session = create(&owner, sh("exec cat")).await;
    let tid = terminal_of(&session);
    {
        let visitor = t.connect().await;
        let _view = View::attach(&visitor, tid, None).await;
        assert_eq!(status(&owner).await.clients, 2);
    }
    timeout(WAIT, async {
        while status(&owner).await.clients != 1 {
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();

    // The terminal keeps working for the remaining client.
    let mut view = View::attach(&owner, tid, None).await;
    owner.send_input(tid, b"still\r").await.unwrap();
    view.wait_for("echo", |s| s.contains("still")).await;
    t.stop().await;
}

// Exit signals ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_session_killed_by_a_signal_reports_the_raw_signal_number() {
    let t = TestDaemon::start().await;
    let (client, mut events) = t.connect_subscribed().await;

    for (script, signal) in [
        ("kill -TERM $$; sleep 5", 15),
        ("kill -HUP $$; sleep 5", 1),
        ("kill -KILL $$; sleep 5", 9),
    ] {
        let session = create(&client, sh(script)).await;
        let exited = wait_event(&mut events, "session.exited", |e| match e {
            Event::SessionExited {
                session_id,
                code,
                signal,
            } if *session_id == session.id => Some((*code, *signal)),
            _ => None,
        })
        .await;
        assert_eq!(exited, (None, Some(signal)), "{script}");
        let listed = client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap();
        let state = &listed
            .sessions
            .iter()
            .find(|s| s.id == session.id)
            .unwrap()
            .state;
        assert_eq!(
            *state,
            SessionState::Exited {
                code: None,
                signal: Some(signal)
            }
        );
    }
    t.stop().await;
}

// Restart cwd -----------------------------------------------------------------------------------

/// First run: `cd` to `$TARGET`, report it with OSC 7, exit. Later runs (the marker exists):
/// print where they were started.
const REPORT_THEN_SHOW: &str = r#"
if [ -e "$MARK" ]; then
    echo "NOW-IN=$(pwd -P)"
    exec cat
fi
touch "$MARK"
cd "$TARGET" || exit 1
printf '\033]7;file://localhost%s\007' "$(pwd -P)"
echo FIRST-RUN
"#;

/// Exits the first run, then waits until the registry shows `cwd` and the exit.
async fn wait_exited_with_cwd(client: &Client, id: SessionId, cwd: &std::path::Path) {
    timeout(WAIT, async {
        loop {
            let listed = client
                .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
                .await
                .unwrap();
            let session = listed.sessions.iter().find(|s| s.id == id).unwrap();
            if matches!(session.state, SessionState::Exited { .. })
                && session.cwd.as_deref() == Some(cwd)
            {
                return;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session never exited with cwd {}", cwd.display()));
}

async fn restart(client: &Client, id: SessionId) -> hutan_protocol::messages::SessionInfo {
    client
        .call::<SessionResult>(Request::SessionRestart(SessionRef { session_id: id }))
        .await
        .unwrap()
        .session
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_starts_in_the_cwd_the_program_last_reported() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let target = work.path().join("reported").canonicalize_or_create();
    let mark = work.path().join("ran");

    let mut spec = sh(REPORT_THEN_SHOW);
    spec.cwd = Some(PathBuf::from("/"));
    spec.env
        .insert("TARGET".into(), target.display().to_string());
    spec.env.insert("MARK".into(), mark.display().to_string());
    let session = create(&client, spec).await;
    assert_eq!(session.cwd, Some(PathBuf::from("/")));
    wait_exited_with_cwd(&client, session.id, &target).await;

    let restarted = restart(&client, session.id).await;
    assert_eq!(
        restarted.cwd,
        Some(target.clone()),
        "SessionInfo reflects it"
    );
    let mut view = View::attach(&client, terminal_of(&restarted), None).await;
    let expected = format!("NOW-IN={}", target.display());
    // The temp path is longer than the 80 columns: look through the wrap.
    view.wait_for("restarted in the reported directory", |s| {
        s.lines.concat().contains(&expected)
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_falls_back_to_the_spec_cwd_when_the_reported_directory_is_gone() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let work = tempfile::tempdir().unwrap();
    let target = work.path().join("doomed").canonicalize_or_create();
    let mark = work.path().join("ran");

    let mut spec = sh(REPORT_THEN_SHOW);
    spec.cwd = Some(PathBuf::from("/"));
    spec.env
        .insert("TARGET".into(), target.display().to_string());
    spec.env.insert("MARK".into(), mark.display().to_string());
    let session = create(&client, spec).await;
    wait_exited_with_cwd(&client, session.id, &target).await;

    std::fs::remove_dir(&target).unwrap();
    let restarted = restart(&client, session.id).await;
    assert_eq!(restarted.cwd, Some(PathBuf::from("/")));
    let mut view = View::attach(&client, terminal_of(&restarted), None).await;
    view.wait_for("restarted in the spec directory", |s| {
        s.contains("NOW-IN=/")
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn set_colors_changes_the_osc_11_reply_and_unknown_terminals_are_not_found() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    assert!(
        client
            .welcome()
            .capabilities
            .iter()
            .any(|c| c == "terminal.colors")
    );

    let reply_len = "\x1b]11;rgb:1234/5678/9abc\x07".len();
    // The child waits for a byte (sent after the colors), queries OSC 11 and shows the raw
    // reply with `cat -v`.
    let script = format!(
        r"stty raw -echo; printf READY; dd bs=1 count=1 2>/dev/null; printf '\033]11;?\007'; dd bs=1 count={reply_len} 2>/dev/null | cat -v; printf '\nDONE\n'"
    );
    let session = create(&client, sh(&script)).await;
    let tid = terminal_of(&session);
    let mut view = View::attach(&client, tid, None).await;
    view.wait_for("ready", |s| s.contains("READY")).await;

    client
        .call::<Empty>(Request::TerminalSetColors(TerminalSetColors {
            terminal_id: tid,
            colors: TerminalColors {
                foreground: (0x10, 0x20, 0x30),
                background: (0x12, 0x56, 0x9a),
                cursor: (0xff, 0xff, 0xff),
            },
        }))
        .await
        .unwrap();
    client.send_input(tid, b"g").await.unwrap();
    view.wait_for("the reply", |s| s.contains("DONE")).await;
    assert!(
        view.screen.contains("^[]11;rgb:1212/5656/9a9a^G"),
        "screen:\n{}",
        view.screen.text()
    );

    let ghost = client
        .call::<Empty>(Request::TerminalSetColors(TerminalSetColors {
            terminal_id: TerminalId(4242),
            colors: TerminalColors {
                foreground: (0, 0, 0),
                background: (0, 0, 0),
                cursor: (0, 0, 0),
            },
        }))
        .await;
    assert_eq!(remote_code(ghost), ErrorCode::NotFound);
    t.stop().await;
}

/// Creates the directory and returns its canonical path (macOS temp dirs sit behind symlinks,
/// and `pwd -P` reports the physical path).
trait CanonicalizeOrCreate {
    fn canonicalize_or_create(self) -> PathBuf;
}

impl CanonicalizeOrCreate for PathBuf {
    fn canonicalize_or_create(self) -> PathBuf {
        std::fs::create_dir_all(&self).unwrap();
        self.canonicalize().unwrap()
    }
}
