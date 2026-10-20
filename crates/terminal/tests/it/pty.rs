//! Real PTYs: spawn, input, resize, alternate screen, replies, metadata, teardown.
#![allow(clippy::expect_used)] // tests assert by panicking

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hutan_protocol::messages::TerminalSize;
use hutan_protocol::render::TerminalSnapshot;
use hutan_terminal::{
    DefaultColors, PromptMark, RenderTracker, Terminal, TerminalConfig, TerminalMeta,
};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(15);

fn size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn config(argv: &[&str]) -> TerminalConfig {
    TerminalConfig {
        argv: argv.iter().map(|a| (*a).to_owned()).collect(),
        size: size(80, 24),
        env: BTreeMap::from([("PS1".to_owned(), "$ ".to_owned())]),
        ..TerminalConfig::default()
    }
}

fn text(snapshot: &TerminalSnapshot) -> String {
    snapshot
        .lines
        .iter()
        .map(|row| {
            row.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Waits until `check` accepts a fresh snapshot, re-evaluating on every damage notification.
async fn wait_for(
    term: &Terminal,
    what: &str,
    check: impl Fn(&TerminalSnapshot) -> bool,
) -> TerminalSnapshot {
    let mut damage = term.damage();
    let mut tracker = RenderTracker::new();
    let result = timeout(LIMIT, async {
        loop {
            let snapshot = term.snapshot(&mut tracker);
            if check(&snapshot) {
                return snapshot;
            }
            if damage.changed().await.is_err() {
                return snapshot;
            }
        }
    })
    .await;
    match result {
        Ok(snapshot) if check(&snapshot) => snapshot,
        Ok(snapshot) => panic!(
            "{what}: terminal closed first; screen:\n{}",
            text(&snapshot)
        ),
        Err(_) => panic!(
            "{what}: timed out; screen:\n{}",
            text(&term.snapshot(&mut RenderTracker::new()))
        ),
    }
}

#[tokio::test]
async fn echo_round_trip_through_write_input() {
    let term = Terminal::spawn(config(&["/bin/sh"])).expect("spawn sh");
    assert!(term.pid().is_some());
    wait_for(&term, "prompt", |s| text(s).contains('$')).await;
    term.write_input(b"echo hutan-$((20+22))\n").expect("write");
    // The typed line contains `hutan-$((`; only the shell's output contains `hutan-42`.
    let snapshot = wait_for(&term, "echo output", |s| text(s).contains("hutan-42")).await;
    assert_eq!(snapshot.cols, 80);
    term.write_input(b"exit 7\n").expect("write");
    let status = timeout(LIMIT, term.wait_exit())
        .await
        .expect("exit in time");
    assert_eq!(status.code, Some(7));
    assert!(term.pid().is_none());
    assert!(
        term.write_input(b"x").is_err(),
        "input after exit is rejected"
    );
}

#[tokio::test]
async fn resize_is_observed_by_the_child() {
    let term = Terminal::spawn(config(&["/bin/sh"])).expect("spawn sh");
    wait_for(&term, "prompt", |s| text(s).contains('$')).await;
    term.write_input(b"stty size\n").expect("write");
    wait_for(&term, "initial size", |s| {
        text(s).lines().any(|l| l.trim() == "24 80")
    })
    .await;

    term.resize(size(100, 30)).expect("resize");
    assert_eq!((term.size().cols, term.size().rows), (100, 30));
    term.write_input(b"stty size\n").expect("write");
    let snapshot = wait_for(&term, "resized", |s| {
        text(s).lines().any(|l| l.trim() == "30 100")
    })
    .await;
    assert_eq!(
        (snapshot.cols, snapshot.rows, snapshot.lines.len()),
        (100, 30, 30)
    );

    let mut tracker = RenderTracker::new();
    term.snapshot(&mut tracker);
    term.resize(size(90, 20)).expect("resize");
    assert!(
        term.frame(&mut tracker).is_none(),
        "a resized tracker needs a snapshot, not a frame"
    );
    assert!(matches!(
        term.render(&mut tracker),
        Some(hutan_protocol::render::TerminalRender::Snapshot(s)) if (s.cols, s.rows) == (90, 20)
    ));
    assert!(term.resize(size(0, 5)).is_err());
    term.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(LIMIT, term.wait_exit()).await.expect("exit");
}

#[tokio::test]
async fn vim_enters_the_alternate_screen_and_returns() {
    let term = Terminal::spawn(config(&["vim", "-u", "NONE", "-n"])).expect("spawn vim");
    let snapshot = wait_for(&term, "vim alt screen", |s| {
        s.modes.alt_screen && text(s).contains('~')
    })
    .await;
    assert!(snapshot.modes.alt_screen);

    term.write_input(b"ihello hutan\x1b").expect("write");
    wait_for(&term, "inserted text", |s| text(s).contains("hello hutan")).await;

    term.write_input(b":q!\r").expect("write");
    let status = timeout(LIMIT, term.wait_exit()).await.expect("vim exits");
    assert_eq!(status.code, Some(0));
    let mut tracker = RenderTracker::new();
    let snapshot = term.snapshot(&mut tracker);
    assert!(!snapshot.modes.alt_screen, "alternate screen was left");
    assert!(
        !text(&snapshot).contains("hello hutan"),
        "primary screen restored"
    );
}

#[tokio::test]
async fn engine_replies_reach_the_child_byte_for_byte() {
    // The child switches to raw mode without echo, emits primary DA and DSR-5 queries, reads
    // exactly the 9 reply bytes (`ESC[?6c` + `ESC[0n`) from its stdin and shows them with
    // `cat -v` (ESC rendered as `^[`).
    let script = r#"stty raw -echo; printf '\033[c\033[5n'; dd bs=1 count=9 2>/dev/null | cat -v; printf '\nDONE\n'"#;
    let term = Terminal::spawn(config(&["/bin/sh", "-c", script])).expect("spawn");
    let snapshot = wait_for(&term, "replies", |s| text(s).contains("DONE")).await;
    assert!(
        text(&snapshot).contains("^[[?6c^[[0n"),
        "screen:\n{}",
        text(&snapshot)
    );
}

#[tokio::test]
async fn color_and_window_size_queries_reach_the_child() {
    // OSC 10 (ST-terminated, configured default colour), OSC 4 (BEL-terminated, xterm table)
    // and CSI 14/16/18 t; the child reads the exact reply bytes and shows them with `cat -v`.
    let reply = "\x1b]10;rgb:0a0a/0b0b/0c0c\x1b\\\x1b]4;9;rgb:ffff/0000/0000\x07\x1b[4;480;800t\x1b[6;20;10t\x1b[8;24;80t";
    let script = format!(
        r#"stty raw -echo; printf '\033]10;?\033\\\033]4;9;?\007\033[14t\033[16t\033[18t'; dd bs=1 count={} 2>/dev/null | cat -v; printf '\nDONE\n'"#,
        reply.len()
    );
    let mut cfg = config(&["/bin/sh", "-c", &script]);
    cfg.size = TerminalSize {
        cols: 80,
        rows: 24,
        pixel_width: 800,
        pixel_height: 480,
    };
    cfg.default_colors = DefaultColors {
        foreground: (0x0a, 0x0b, 0x0c),
        ..DefaultColors::default()
    };
    let term = Terminal::spawn(cfg).expect("spawn");
    let snapshot = wait_for(&term, "replies", |s| text(s).contains("DONE")).await;
    let want = "^[]10;rgb:0a0a/0b0b/0c0c^[\\^[]4;9;rgb:ffff/0000/0000^G^[[4;480;800t^[[6;20;10t^[[8;24;80t";
    // The 83-column reply wraps once at 80 columns.
    let screen = text(&snapshot);
    assert!(screen.replace('\n', "").contains(want), "screen:\n{screen}");
}

#[tokio::test]
async fn set_default_colors_changes_the_osc_11_reply_a_running_child_gets() {
    // The child announces READY, then queries OSC 11 with both terminators and shows the raw
    // replies with `cat -v`; the colours are changed while it waits.
    let reply = "\x1b]11;rgb:1212/3434/5656\x07\x1b]11;rgb:1212/3434/5656\x1b\\";
    let script = format!(
        r#"stty raw -echo; printf READY; sleep 1; printf '\033]11;?\007\033]11;?\033\\'; dd bs=1 count={} 2>/dev/null | cat -v; printf '\nDONE\n'"#,
        reply.len()
    );
    let term = Terminal::spawn(config(&["/bin/sh", "-c", &script])).expect("spawn");
    wait_for(&term, "ready", |s| text(s).contains("READY")).await;
    term.set_default_colors(DefaultColors {
        background: (0x12, 0x34, 0x56),
        ..DefaultColors::default()
    });
    let snapshot = wait_for(&term, "replies", |s| text(s).contains("DONE")).await;
    let want = "^[]11;rgb:1212/3434/5656^G^[]11;rgb:1212/3434/5656^[\\";
    let screen = text(&snapshot);
    assert!(screen.replace('\n', "").contains(want), "screen:\n{screen}");
}

#[tokio::test]
async fn exit_reports_the_code_or_the_raw_signal_number() {
    for (script, code, signal) in [
        ("exit 3", Some(3), None),
        ("kill -TERM $$; sleep 5", None, Some(15)),
        ("kill -KILL $$; sleep 5", None, Some(9)),
        ("kill -HUP $$; sleep 5", None, Some(1)),
    ] {
        let term = Terminal::spawn(config(&["/bin/sh", "-c", script])).expect("spawn");
        let status = timeout(LIMIT, term.wait_exit()).await.expect("exit");
        assert_eq!((status.code, status.signal), (code, signal), "{script}");
    }
}

#[tokio::test]
async fn unclosed_synchronized_batch_is_rendered_after_the_timeout() {
    let script = r#"printf '\033[?2026hSYNCED'; sleep 5"#;
    let started = Instant::now();
    let term = Terminal::spawn(config(&["/bin/sh", "-c", script])).expect("spawn");
    let mut damage = term.damage();
    timeout(LIMIT, damage.changed())
        .await
        .expect("damage in time")
        .expect("sender alive");
    let elapsed = started.elapsed();
    // The first damage can only be the forced end of the batch, 150 ms after it opened.
    assert!(
        elapsed >= Duration::from_millis(120),
        "rendered too early: {elapsed:?}"
    );
    let snapshot = term.snapshot(&mut RenderTracker::new());
    assert!(text(&snapshot).contains("SYNCED"));
    term.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(LIMIT, term.wait_exit()).await.expect("exit");
}

#[tokio::test]
async fn metadata_events_are_broadcast() {
    let term = Terminal::spawn(config(&["/bin/sh"])).expect("spawn sh");
    wait_for(&term, "prompt", |s| text(s).contains('$')).await;
    let mut meta = term.metadata();
    term.write_input(
        b"printf '\\033]0;hutan-title\\007\\033]133;A\\007\\033]7;file://h/tmp/x\\007\\a'\n",
    )
    .expect("write");
    let mut seen = Vec::new();
    timeout(LIMIT, async {
        while seen.last() != Some(&TerminalMeta::Bell) {
            match meta.recv().await {
                Ok(event) => seen.push(event),
                Err(error) => panic!("metadata channel: {error}"),
            }
        }
    })
    .await
    .expect("metadata in time");
    // The prompt mark also starts a block (`Block` event, covered by the block tests).
    assert!(
        seen.iter()
            .any(|event| matches!(event, TerminalMeta::Block(block) if block.id == 1))
    );
    seen.retain(|event| !matches!(event, TerminalMeta::Block(_)));
    assert_eq!(
        seen,
        [
            TerminalMeta::Title("hutan-title".to_owned()),
            TerminalMeta::PromptMark(PromptMark::A),
            TerminalMeta::Cwd("/tmp/x".into()),
            TerminalMeta::Bell,
        ]
    );
    let snapshot = term.snapshot(&mut RenderTracker::new());
    assert_eq!(snapshot.title.as_deref(), Some("hutan-title"));
    term.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(LIMIT, term.wait_exit()).await.expect("exit");
}

#[tokio::test]
async fn terminate_hangs_up_then_kills_after_the_grace_period() {
    // A child that ignores SIGHUP survives the hang-up and is killed once the grace elapsed.
    let term = Terminal::spawn(config(&[
        "/bin/sh",
        "-c",
        "trap '' HUP; echo ready; while :; do sleep 1; done",
    ]))
    .expect("spawn");
    wait_for(&term, "ready", |s| text(s).contains("ready")).await;
    let started = Instant::now();
    term.terminate(Duration::from_millis(400))
        .expect("terminate");
    let status = timeout(LIMIT, term.wait_exit()).await.expect("exit");
    assert_eq!(status.signal, Some(9), "killed by SIGKILL, got {status:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "grace period was honoured"
    );
    // Reaped exactly once: waiting again yields the same status.
    assert_eq!(
        timeout(LIMIT, term.wait_exit()).await.expect("exit"),
        status
    );
}

#[tokio::test]
async fn environment_and_cwd_are_applied() {
    let mut cfg = config(&[
        "/bin/sh",
        "-c",
        "echo T=$TERM C=$COLORTERM P=$TERM_PROGRAM X=$HUTAN_X; pwd",
    ]);
    cfg.env.insert("HUTAN_X".to_owned(), "yes".to_owned());
    cfg.env.insert("TERM".to_owned(), "dumb".to_owned());
    cfg.cwd = Some("/".into());
    let term = Terminal::spawn(cfg).expect("spawn");
    let status = timeout(LIMIT, term.wait_exit()).await.expect("exit");
    assert!(status.success());
    let screen = text(&term.snapshot(&mut RenderTracker::new()));
    assert!(
        screen.contains("T=xterm-256color C=truecolor P=hutan X=yes"),
        "screen:\n{screen}"
    );
    assert!(screen.lines().any(|l| l.trim() == "/"), "screen:\n{screen}");
}

#[tokio::test]
async fn spawning_a_missing_program_fails_cleanly() {
    let error =
        Terminal::spawn(config(&["/definitely/not/a/program"])).expect_err("spawn must fail");
    assert!(
        error.to_string().contains("/definitely/not/a/program"),
        "{error}"
    );
}

#[tokio::test]
async fn default_argv_starts_the_login_shell() {
    let mut cfg = config(&[]);
    cfg.env.insert("SHELL".to_owned(), "/bin/sh".to_owned());
    let term = Terminal::spawn(cfg).expect("spawn default shell");
    assert!(term.pid().is_some());
    term.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(LIMIT, term.wait_exit()).await.expect("exit");
}

/// The account's login shell (what `chsh` / `dscl … UserShell` report) wins over the daemon's
/// own `$SHELL`: a daemon started from a zsh-based tool must still give a fish user fish.
#[test]
fn default_shell_is_the_account_login_shell_not_the_daemons_shell() {
    let output = std::process::Command::new("/usr/bin/dscl")
        .args([".", "-read"])
        .arg(format!(
            "/Users/{}",
            std::env::var("USER").expect("USER is set")
        ))
        .arg("UserShell")
        .output()
        .expect("run dscl");
    let account_shell = String::from_utf8_lossy(&output.stdout)
        .trim()
        .strip_prefix("UserShell:")
        .expect("dscl prints UserShell")
        .trim()
        .to_owned();

    assert_eq!(
        hutan_terminal::default_shell(&BTreeMap::new()),
        account_shell
    );
    let explicit = BTreeMap::from([("SHELL".to_owned(), "/bin/sh".to_owned())]);
    assert_eq!(hutan_terminal::default_shell(&explicit), "/bin/sh");
}

#[tokio::test]
async fn the_login_shell_child_sees_itself_as_shell() {
    let mut cfg = config(&[]);
    cfg.env.insert("SHELL".to_owned(), "/bin/sh".to_owned());
    cfg.env.insert("ENV".to_owned(), String::new());
    let term = Terminal::spawn(cfg).expect("spawn default shell");
    term.write_input(b"echo \"shell=$SHELL\"\r").expect("write");
    let deadline = Instant::now() + LIMIT;
    let mut tracker = RenderTracker::default();
    loop {
        let shown = text(&term.snapshot(&mut tracker));
        if shown.contains("shell=/bin/sh") {
            break;
        }
        assert!(Instant::now() < deadline, "no SHELL echo in:\n{shown}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    term.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(LIMIT, term.wait_exit()).await.expect("exit");
}

#[test]
fn handles_are_send_sync_and_cheap_to_clone() {
    fn assert_send_sync<T: Send + Sync + Clone>() {}
    assert_send_sync::<Terminal>();
    fn assert_send<T: Send>() {}
    assert_send::<RenderTracker>();
}
