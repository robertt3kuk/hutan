//! `hutan new/ls/send/stop/restart/rename/history` against a real daemon.

use crate::common;

use std::time::Duration;

use common::{TestDaemon, WAIT};
use tokio::time::{sleep, timeout};

#[tokio::test(flavor = "multi_thread")]
async fn new_then_ls_json_shows_a_running_session() {
    let t = TestDaemon::start().await;
    let out = t
        .hutan(&[
            "new", "--title", "worker", "--env", "A=1", "--env", "B=x=y", "--", "cat",
        ])
        .await
        .ok();
    let words: Vec<_> = out.stdout.split_whitespace().collect();
    assert_eq!(words.len(), 4, "{out:?}");
    assert_eq!((words[0], words[2]), ("session", "terminal"), "{out:?}");
    assert_eq!(
        words[3].parse::<u64>().unwrap(),
        1,
        "first terminal of the epoch"
    );

    let session = t.wait_state(words[1], "running").await;
    assert_eq!(session["title"], "worker");
    assert_eq!(session["terminal_id"], 1);
    assert_eq!(session["command"], serde_json::json!(["cat"]));
    assert_eq!(
        session["cwd"],
        std::env::current_dir().unwrap().to_str().unwrap(),
        "--cwd defaults to the current directory"
    );

    let table = t.hutan(&["ls"]).await.ok().stdout;
    let mut lines = table.lines();
    let header: Vec<_> = lines.next().unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        [
            "ID", "TERMINAL", "STATE", "RECOVERY", "PROJECT", "PLACE", "TITLE", "FOLDER", "AGE"
        ]
    );
    let row = lines.next().unwrap();
    assert!(
        row.starts_with(words[1])
            && row.contains("t:1")
            && row.contains("running")
            && row.contains("worker"),
        "{table}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_honours_cwd_and_environment() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(
            &["--cwd", "/tmp", "--env", "GREETING=hi there"],
            &["sh", "-c", "echo \"$GREETING in $(pwd -P)\"; exec cat"],
        )
        .await;
    assert_eq!(t.session(&id).await["cwd"], "/tmp");
    wait_history(&t, &id, "hi there in ").await;
    let history = t.hutan(&["history", &id]).await.ok().stdout;
    assert!(history.contains("/tmp"), "{history}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_cannot_start_fails_the_command() {
    let t = TestDaemon::start().await;
    let out = t
        .hutan(&["new", "--", "/nonexistent/hutan-test-binary"])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("failed to start"), "{out:?}");
    let sessions = t.sessions().await;
    assert_eq!(sessions[0]["state"]["state"], "failed");
    t.stop().await;
}

/// `hutan new -- tool` runs the tool from the environment the user's login shell builds, so a
/// tool that only the shell configuration puts on `PATH` is found. A stand-in script plays the
/// login shell (`<shell> -l -c '/usr/bin/env -0'`).
#[tokio::test(flavor = "multi_thread")]
async fn new_finds_a_tool_the_login_shell_puts_on_path() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let write = |path: &std::path::Path, text: String| {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // macOS vets the first execution of a new executable, which can take tens of seconds on
        // a loaded machine; do that here, outside the timeouts the test is about.
        let _ = std::process::Command::new(path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    write(
        &tools.join("stand-in-tool"),
        "#!/bin/sh\necho \"TOOL-RAN $B1_FROM_LOGIN\"\nexec cat\n".to_owned(),
    );
    let shell = dir.path().join("login-shell");
    write(
        &shell,
        format!(
            "#!/bin/sh\nexport PATH=\"{}:$PATH\" B1_FROM_LOGIN=yes\nexec /usr/bin/env -0\n",
            tools.display()
        ),
    );
    let t = TestDaemon::start_with(|config| config.login_shell = Some(shell.clone())).await;

    // A spawn waits at most two seconds for the first capture of the login shell; on a loaded
    // machine an early one can miss it ("failed to start") and a later one finds the tool.
    let mut id = None;
    for _ in 0..10 {
        let out = t.hutan(&["new", "--json", "--", "stand-in-tool"]).await;
        if out.code == 0 {
            id = Some(out.json()["session"]["id"].as_str().unwrap().to_owned());
            break;
        }
        assert!(out.stderr.contains("failed to start"), "{out:?}");
    }
    let id = id.expect("the tool was never found");
    wait_history(&t, &id, "TOOL-RAN yes").await;
    t.stop().await;
}

/// Polls `hutan history` until it contains `needle`.
async fn wait_history(t: &TestDaemon, target: &str, needle: &str) {
    let result = timeout(WAIT, async {
        loop {
            let out = t.hutan(&["history", target]).await.ok();
            if out.stdout.contains(needle) {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(result.is_ok(), "history never contained {needle:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn send_enter_reaches_the_session_and_shows_in_history() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["cat"]).await;
    t.hutan(&["send", &id, "hello hutan", "--enter"]).await.ok();
    wait_history(&t, &id, "hello hutan").await;

    // Without --enter nothing is submitted: cat has not echoed it yet, but the line discipline
    // did (the typed text is visible), and the next --enter completes the line.
    t.hutan(&["send", &id, "second"]).await.ok();
    wait_history(&t, &id, "second").await;
    let json = t
        .hutan(&["--json", "history", &id, "--lines", "2"])
        .await
        .ok()
        .json();
    assert!(json["lines"].as_array().unwrap().len() <= 2, "{json}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_is_oldest_first_across_scrollback_and_screen() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "seq 1 100; exec cat"])
        .await;
    wait_history(&t, &id, "100").await;
    let all = t
        .hutan(&["history", &id, "--lines", "5000"])
        .await
        .ok()
        .stdout;
    let numbers: Vec<u32> = all.lines().filter_map(|l| l.parse().ok()).collect();
    assert_eq!(numbers, (1..=100).collect::<Vec<_>>(), "{all}");

    let last = t.hutan(&["history", &id, "--lines", "3"]).await.ok().stdout;
    assert_eq!(last.lines().collect::<Vec<_>>(), ["98", "99", "100"]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_moves_the_session_to_stopped_and_restart_revives_it() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["cat"]).await;
    let first_terminal = t.session(&id).await["terminal_id"].as_u64().unwrap();

    let out = t.hutan(&["stop", &id]).await.ok();
    assert!(out.stdout.contains(&id), "{out:?}");
    assert_eq!(t.session(&id).await["state"]["state"], "stopped");
    let table = t.hutan(&["ls"]).await.ok().stdout;
    assert!(table.contains("stopped"), "{table}");

    // Input to an ended session is an error, not a silent drop.
    let out = t.hutan(&["send", &id, "x"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("stopped"), "{out:?}");

    let out = t.hutan(&["restart", &id]).await.ok();
    let second_terminal = t.session(&id).await["terminal_id"].as_u64().unwrap();
    assert!(second_terminal > first_terminal);
    assert_eq!(
        out.stdout.trim(),
        format!("session {id} terminal {second_terminal}")
    );
    t.wait_state(&id, "running").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_codes_show_in_ls() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exit 7"]).await;
    let session = t.wait_state(&id, "exited").await;
    assert_eq!(session["state"]["code"], 7);
    let table = t.hutan(&["ls"]).await.ok().stdout;
    assert!(table.contains("exited (7)"), "{table}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_changes_the_title() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&["--title", "before"], &["cat"]).await;
    let out = t.hutan(&["rename", "before", "after"]).await.ok();
    assert!(out.stdout.contains("after"), "{out:?}");
    assert_eq!(t.session(&id).await["title"], "after");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn targets_resolve_by_id_prefix_terminal_and_title() {
    let t = TestDaemon::start().await;
    let a = t.new_session(&["--title", "alpha"], &["cat"]).await;
    let b = t.new_session(&["--title", "beta"], &["cat"]).await;
    let c = t.new_session(&["--title", "beta"], &["cat"]).await;

    // `history --json` names the session it resolved and changes nothing.
    async fn probe(t: &TestDaemon, target: &str) -> common::Out {
        t.hutan(&["--json", "history", target]).await
    }

    // Full id, long unique prefix, lowercase prefix, terminal id, unique title.
    let lower = a[..20].to_ascii_lowercase();
    for (target, want) in [
        (a.as_str(), &a),
        (&a[..20], &a),
        (lower.as_str(), &a),
        ("t:2", &b),
        ("alpha", &a),
    ] {
        let out = probe(&t, target).await.ok();
        assert_eq!(out.json()["session_id"], want.as_str(), "{target}");
    }

    // The shared timestamp prefix is ambiguous: exit 2 listing every candidate.
    let out = probe(&t, &a[..4]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    for id in [&a, &b, &c] {
        assert!(out.stderr.contains(id.as_str()), "{out:?}");
    }
    assert!(
        out.stderr.contains("t:1") && out.stderr.contains("t:3"),
        "{out:?}"
    );

    // Two sessions titled `beta`: ambiguous too.
    let out = probe(&t, "beta").await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains(&b) && out.stderr.contains(&c),
        "{out:?}"
    );
    assert!(!out.stderr.contains(&a), "{out:?}");

    // No match: a plain failure, with a hint for short prefixes.
    let out = probe(&t, "nothing").await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no session matches"), "{out:?}");
    assert_eq!(probe(&t, "t:99").await.code, 1);
    let out = probe(&t, "01J").await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("at least 4"), "{out:?}");
    assert_eq!(probe(&t, "t:x").await.code, 2);
    t.stop().await;
}
#[tokio::test(flavor = "multi_thread")]
async fn ls_filters_by_workspace() {
    let t = TestDaemon::start().await;
    let ws = t.hutan(&["ws", "new", "other", "--json"]).await.ok().json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let in_default = t.new_session(&[], &["cat"]).await;
    let in_other = t.new_session(&["--ws", &ws], &["cat"]).await;

    let out = t.hutan(&["ls", "--ws", &ws, "--json"]).await.ok().json();
    let ids: Vec<_> = out["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].clone())
        .collect();
    assert_eq!(ids, [serde_json::json!(in_other)]);
    assert_ne!(in_default, in_other);

    let empty = t.hutan(&["ws", "new", "empty", "--json"]).await.ok().json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = t.hutan(&["ls", "--ws", &empty]).await.ok();
    assert_eq!(out.stdout.trim(), "no sessions");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_and_missing_daemon() {
    let t = TestDaemon::start().await;
    for args in [
        &["new", "--env", "no-equals"][..],
        &["history", "x", "--lines", "0"],
        &["send", "x"],
        &["rename", "x"],
        &["attach"],
    ] {
        let out = t.hutan(args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
    }
    // `--ws` takes an id, a prefix or a name; one that matches nothing is a failure, not usage.
    let out = t.hutan(&["new", "--ws", "not-a-workspace"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("no workspace or project matches"),
        "{out:?}"
    );
    // attach needs a terminal; stdin is /dev/null here.
    let out = t.hutan(&["attach", "anything"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("needs a terminal"), "{out:?}");
    let out = t.hutan(&["new", "--attach", "--", "cat"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        t.sessions().await.is_empty(),
        "no session is created without a terminal"
    );

    let dir = tempfile::tempdir().unwrap();
    let nowhere = dir.path().join("no.sock");
    for args in [&["ls"][..], &["stop", "x"], &["new", "--", "cat"]] {
        let out = common::hutan(&nowhere, args).await;
        assert_eq!(out.code, 3, "{args:?}: {out:?}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_refuses_a_busy_session_without_force_and_reopen_brings_it_back() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&["--title", "worker"], &["cat"]).await;
    t.wait_state(&id, "running").await;

    // An agent working in the session makes it busy.
    let mut report = common::command(
        std::path::Path::new(common::HUTAN),
        Some(&t.paths.socket),
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    );
    report.env("HUTAN_SESSION_ID", &id);
    common::finish(report).await.ok();
    timeout(WAIT, async {
        while t.session(&id).await.get("agent").is_none() {
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the report never showed up");

    let refused = t.hutan(&["close", &id]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("busy") && refused.stderr.contains("fake is working"),
        "says what runs: {refused:?}"
    );
    assert!(
        refused.stderr.contains("--force"),
        "says how to go on: {refused:?}"
    );
    t.wait_state(&id, "running").await;

    let closed = t.hutan(&["close", "--force", &id, "--json"]).await.ok();
    assert_eq!(closed.json()["closed"], true);
    assert!(t.sessions().await.is_empty(), "gone from `hutan ls`");

    // Reopened by the full id: same session, running again; idle, so it closes without --force.
    let reopened = t.hutan(&["reopen", &id]).await.ok();
    assert!(reopened.stdout.contains(&id), "{reopened:?}");
    let session = t.wait_state(&id, "running").await;
    assert_eq!(session["title"], "worker");
    t.hutan(&["close", &id]).await.ok();
    assert!(t.sessions().await.is_empty());

    // Reopened once more (as JSON), it is open: a second reopen has nothing to bring back.
    let out = t.hutan(&["reopen", &id, "--json"]).await.ok();
    assert_eq!(out.json()["session"]["id"], id.as_str());
    let out = t.hutan(&["reopen", &id]).await;
    assert_eq!(out.code, 1, "already open: {out:?}");
    assert!(out.stderr.contains("not closed"), "{out:?}");
    let out = t.hutan(&["close", "nonexistent"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    t.stop().await;
}

// Contain (SANDBOX-2) ---------------------------------------------------------------------------

/// A folder outside everything a contained session may write (not a temp folder).
fn outside_dir(name: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cli-contain-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn new_contain_runs_the_session_in_the_boundary_and_says_what_it_holds_back() {
    let t = TestDaemon::start().await;
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().canonicalize().unwrap();
    let outside = outside_dir("new");
    let script = "echo in > in.txt; echo \"in=$?\"; ( echo x > \"$OUT/x\" ) 2>/dev/null; echo \"out=$?\"; exec cat";

    let out = t
        .hutan(&[
            "new",
            "--contain",
            "--network",
            "off",
            "--cwd",
            cwd.to_str().unwrap(),
            "--env",
            &format!("OUT={}", outside.display()),
            "--",
            "sh",
            "-c",
            script,
        ])
        .await
        .ok();
    assert!(
        out.stdout.contains(
            "contained: writes limited to this worktree · can read your files · network off"
        ),
        "{out:?}"
    );
    let id = out.stdout.split_whitespace().nth(1).unwrap().to_owned();
    wait_history(&t, &id, "out=1").await;
    wait_history(&t, &id, "in=0").await;
    assert!(!outside.join("x").exists());

    // `--json` carries what was applied, the network choice included.
    let json = t
        .hutan(&[
            "new",
            "--contain",
            "--json",
            "--cwd",
            cwd.to_str().unwrap(),
            "--",
            "cat",
        ])
        .await
        .ok()
        .json();
    let containment = &json["session"]["containment"];
    assert_eq!(containment["kind"], "seatbelt");
    assert_eq!(containment["applied"], true);
    assert_eq!(containment["network"], true, "on unless told otherwise");
    assert_eq!(containment["write_roots"], serde_json::json!([cwd]));
    // A session without the flag says nothing of a boundary.
    let plain = t
        .new_session(&["--cwd", cwd.to_str().unwrap()], &["cat"])
        .await;
    assert!(t.session(&plain).await.get("containment").is_none());
    let _ = std::fs::remove_dir_all(&outside);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_contain_flags_go_together_and_a_boundary_that_cannot_be_applied_refuses() {
    let t =
        TestDaemon::start_toml("[sandbox]\nbin = \"/nonexistent/sandbox-exec\"\n", |_| {}).await;
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().to_str().unwrap().to_owned();
    // clap: --network and --write only make sense with --contain; the value is on or off.
    for args in [
        ["--network", "off"].as_slice(),
        ["--write", "/tmp"].as_slice(),
        ["--contain", "--network", "maybe"].as_slice(),
    ] {
        let mut all = vec!["new", "--cwd", &cwd];
        all.extend_from_slice(args);
        all.extend(["--", "cat"]);
        assert_eq!(t.hutan(&all).await.code, 2, "{args:?}");
    }
    // Never started uncontained.
    let refused = t
        .hutan(&["new", "--contain", "--cwd", &cwd, "--", "touch", "started"])
        .await;
    assert_ne!(refused.code, 0);
    assert!(
        refused.stderr.contains("Contain is not available")
            && refused.stderr.contains("not started uncontained"),
        "{}",
        refused.stderr
    );
    assert!(t.sessions().await.is_empty());
    assert!(!work.path().join("started").exists());
    t.stop().await;
}
