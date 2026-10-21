//! Runs the real `hutan` binary against a real `hutand`: in-process on a temporary
//! `HUTAN_HOME` for most commands, a spawned `hutand` process for `daemon start`/`stop`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{HUTAN, TestDaemon, WAIT, command, finish, hutan, hutand_dir};
use hutan_daemon::testing::DaemonReaper;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::time::timeout;

fn missing_socket() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("nobody.sock");
    (dir, socket)
}

// daemon status ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_json_reports_the_running_daemon() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["daemon", "status", "--json"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let status = out.json();
    assert_eq!(status["running"], true);
    assert_eq!(status["pid"], std::process::id());
    assert_eq!(status["epoch"], t.daemon.epoch().to_string());
    assert_eq!(status["socket"], t.paths.socket.to_str().unwrap());
    assert_eq!(status["sessions"], 0);
    assert_eq!(status["protocol"]["major"], 0);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_human_output() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["daemon", "status"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert!(
        out.stdout.contains(&format!("pid {}", std::process::id())),
        "{out:?}"
    );
    assert!(out.stdout.contains("0 session(s)"), "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_is_found_through_hutan_home_and_hutan_socket() {
    let t = TestDaemon::start().await;

    let mut by_home = command(Path::new(HUTAN), None, &["daemon", "status", "--json"]);
    by_home.env("HUTAN_HOME", &t.paths.base);
    let out = finish(by_home).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["pid"], std::process::id());

    let (_dir, nowhere) = missing_socket();
    let mut by_socket = command(Path::new(HUTAN), None, &["daemon", "status", "--json"]);
    by_socket
        .env("HUTAN_HOME", nowhere.parent().unwrap())
        .env("HUTAN_SOCKET", &t.paths.socket);
    let out = finish(by_socket).await;
    assert_eq!(out.code, 0, "{out:?}");

    // --socket beats the environment.
    let mut flag = command(Path::new(HUTAN), None, &["daemon", "status"]);
    flag.env("HUTAN_SOCKET", &nowhere)
        .arg("--socket")
        .arg(&t.paths.socket);
    assert_eq!(finish(flag).await.code, 0);
    t.stop().await;
}

// workspaces -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ws_new_then_ls_json() {
    let folders = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(folders.path()).unwrap();
    std::fs::create_dir_all(base.join("a")).unwrap();
    std::fs::create_dir_all(base.join("rel/b")).unwrap();
    let t = TestDaemon::start().await;
    let a = base.join("a");
    let mut new = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["ws", "new", "alpha", a.to_str().unwrap(), "rel/b", "--json"],
    );
    new.current_dir(&base);
    let created = finish(new).await;
    assert_eq!(created.code, 0, "{created:?}");
    let created = created.json();
    assert_eq!(created["name"], "alpha");
    assert_eq!(created["kind"], "workspace");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["roots"],
        json!([a.to_str().unwrap(), base.join("rel/b").to_str().unwrap()]),
        "relative folders are resolved against the caller's directory"
    );

    t.hutan(&["ws", "new", "beta"]).await;
    let list = t.hutan(&["ws", "ls", "--all", "--json"]).await;
    assert_eq!(list.code, 0, "{list:?}");
    let workspaces = list.json()["workspaces"].clone();
    let names: Vec<_> = workspaces
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect();
    // Workspaces with their projects, then Home.
    assert_eq!(names, ["alpha", "a", "b", "beta", "Home"]);
    assert_eq!(workspaces[0]["id"], id);
    assert_eq!(workspaces[1]["parent_id"], id);

    let human = t.hutan(&["ws", "ls", "--all"]).await;
    assert!(human.stdout.contains(&id), "{human:?}");
    assert!(human.stdout.contains("alpha"), "{human:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_on_a_fresh_daemon_shows_home() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["ws", "ls"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout.lines().count(), 2, "header and one row: {out:?}");
    assert!(out.stdout.lines().nth(1).unwrap().contains("Home"));
    let out = t.hutan(&["ws", "ls", "--json"]).await.json();
    assert_eq!(out["workspaces"].as_array().unwrap().len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_rename_and_rm() {
    let t = TestDaemon::start().await;
    let created = t.hutan(&["ws", "new", "old", "--json"]).await.json();
    let id = created["id"].as_str().unwrap();

    let renamed = t.hutan(&["ws", "rename", id, "fresh", "--json"]).await;
    assert_eq!(renamed.code, 0, "{renamed:?}");
    assert_eq!(renamed.json()["name"], "fresh");
    let human = t.hutan(&["ws", "rename", id, "again"]).await;
    assert!(human.stdout.contains("again"), "{human:?}");

    let deleted = t.hutan(&["ws", "rm", id, "--json"]).await;
    assert_eq!(deleted.code, 0, "{deleted:?}");
    assert_eq!(deleted.json(), json!({"workspace_id": id, "deleted": true}));
    let list = t.hutan(&["ws", "ls", "--json"]).await.json();
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(list["workspaces"][0]["name"], "Home");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_errors_exit_1_with_the_code_on_stderr() {
    let t = TestDaemon::start().await;
    let unknown = "01J00000000000000000000000";
    let out = t.hutan(&["ws", "new", "x", "/definitely/not/here"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(out.stderr.starts_with("hutan: not_found:"), "{out:?}");
    // Targets are resolved before anything is sent: an unknown one is a plain failure.
    let out = t.hutan(&["ws", "rename", unknown, "x"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no workspace matches"), "{out:?}");
    t.stop().await;
}

// api -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn api_daemon_status_prints_the_result() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["api", "daemon.status"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let result = out.json();
    assert_eq!(result["pid"], std::process::id());
    assert_eq!(result["epoch"], t.daemon.epoch().to_string());
    assert!(
        out.stdout.lines().count() > 1,
        "human output is pretty-printed"
    );

    let compact = t.hutan(&["api", "daemon.status", "--json"]).await;
    assert_eq!(compact.stdout.lines().count(), 1, "{compact:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_passes_params_through() {
    let t = TestDaemon::start().await;
    let folder = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(folder.path()).unwrap();
    // `roots` is what a client before protocol 0.19 calls `paths`.
    let params = json!({"name": "via-api", "roots": [folder]}).to_string();
    let out = t
        .hutan(&["api", "workspace.create", &params, "--json"])
        .await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["name"], "via-api");
    let list = t.hutan(&["api", "workspace.list", "{}", "--json"]).await;
    assert_eq!(list.json()["workspaces"][1]["name"], "via-api");
    assert_eq!(list.json()["workspaces"][1]["roots"], json!([folder]));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_error_bodies_go_to_stdout_with_exit_1() {
    let t = TestDaemon::start().await;
    let unknown_method = t.hutan(&["api", "no.such.method", "--json"]).await;
    assert_eq!(unknown_method.code, 1, "{unknown_method:?}");
    assert_eq!(unknown_method.json()["code"], "bad_request");
    assert!(unknown_method.stderr.is_empty(), "{unknown_method:?}");

    let bad_params = t
        .hutan(&["api", "workspace.create", r#"{"nope": 1}"#, "--json"])
        .await;
    assert_eq!(bad_params.code, 1, "{bad_params:?}");
    assert_eq!(bad_params.json()["code"], "bad_request");

    let remote = t
        .hutan(&[
            "api",
            "workspace.close",
            r#"{"workspace_id": "01J00000000000000000000000"}"#,
            "--json",
        ])
        .await;
    assert_eq!(remote.code, 1, "{remote:?}");
    assert_eq!(remote.json()["code"], "not_found");
    assert!(remote.json()["message"].is_string());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_params_that_are_not_json_are_a_usage_error() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["api", "daemon.status", "{not json"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("not valid JSON"), "{out:?}");
    t.stop().await;
}

// events --------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn events_without_follow_prints_current_state_and_exits() {
    let t = TestDaemon::start().await;
    t.hutan(&["ws", "new", "one"]).await;
    let out = t.hutan(&["events"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    let lines: Vec<Value> = out
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{out:?}");
    assert!(lines.iter().all(|l| l["event"] == "workspace.updated"));
    assert_eq!(lines[0]["data"]["workspace"]["name"], "Home");
    assert_eq!(lines[1]["data"]["workspace"]["name"], "one");
    assert_eq!(lines[1]["epoch"], t.daemon.epoch().to_string());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn events_follow_streams_new_events_as_json_lines() {
    let t = TestDaemon::start().await;
    t.hutan(&["ws", "new", "before"]).await;

    let mut child = command(Path::new(HUTAN), None, &["events", "--follow"])
        .arg("--socket")
        .arg(&t.paths.socket)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next = async || -> Value {
        let line = timeout(WAIT, lines.next_line()).await.unwrap().unwrap();
        serde_json::from_str(&line.expect("stdout closed")).unwrap()
    };

    assert_eq!(next().await["data"]["workspace"]["name"], "Home");
    let first = next().await;
    assert_eq!(first["data"]["workspace"]["name"], "before");
    t.hutan(&["ws", "new", "after"]).await;
    let second = next().await;
    assert_eq!(second["event"], "workspace.updated");
    assert_eq!(second["data"]["workspace"]["name"], "after");
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());

    child.kill().await.unwrap();
    t.stop().await;
}

// daemon stop ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_stop_shuts_the_daemon_down() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["daemon", "stop", "--json"]).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json(), json!({"stopped": true}));
    assert!(!t.paths.socket.exists());
    timeout(WAIT, t.daemon.wait()).await.unwrap().unwrap();
}

// exit codes ----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_daemon_exits_3() {
    let (_dir, socket) = missing_socket();
    for args in [
        &["daemon", "status"][..],
        &["daemon", "stop"],
        &["ws", "ls"],
        &["ws", "new", "x"],
        &["api", "daemon.status"],
        &["events"],
    ] {
        let out = hutan(&socket, args).await;
        assert_eq!(out.code, 3, "{args:?}: {out:?}");
        assert!(out.stderr.contains("hutan daemon start"), "{out:?}");
    }
    let json = hutan(&socket, &["daemon", "status", "--json"]).await;
    assert_eq!(json.code, 3);
    assert_eq!(json.json()["running"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_file_counts_as_unreachable() {
    let t = TestDaemon::start().await;
    let stale = t.paths.base.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(stale.exists());
    let out = hutan(&stale, &["daemon", "status"]).await;
    assert_eq!(out.code, 3, "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_errors_exit_2() {
    let (_dir, socket) = missing_socket();
    for args in [
        &[][..],
        &["bogus"],
        &["ws"],
        &["ws", "new"],
        &["ws", "rm"],
        &["ws", "add", "only-one"],
        &["ws", "close", "gone-since-0.19"],
        &["project"],
        &["project", "open"],
        &["daemon", "status", "--bogus"],
        &["api"],
    ] {
        let out = hutan(&socket, args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{out:?}");
    }
    let help = hutan(&socket, &["--help"]).await;
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("daemon"));
}

// daemon start (real hutand process) -------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_start_status_stop_with_hutand_beside_hutan() {
    hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonReaper::new(home.path());
    let run = |args: &'static [&'static str]| {
        let mut cmd = command(Path::new(HUTAN), None, args);
        cmd.env("HUTAN_HOME", home.path());
        finish(cmd)
    };

    let started = run(&["daemon", "start", "--json"]).await;
    assert_eq!(started.code, 0, "{started:?}");
    let started = started.json();
    assert_eq!(started["started"], true);
    let pid = started["pid"].as_u64().unwrap();
    assert_ne!(pid, u64::from(std::process::id()));

    let again = run(&["daemon", "start", "--json"]).await.json();
    assert_eq!(again["started"], false, "start is idempotent");
    assert_eq!(again["pid"], pid);
    assert_eq!(again["epoch"], started["epoch"]);

    let status = run(&["daemon", "status", "--json"]).await;
    assert_eq!(status.json()["pid"], pid);
    assert!(home.path().join("hutand.sock").exists());

    let ws = run(&["ws", "new", "real", "--json"]).await;
    assert_eq!(ws.code, 0, "{ws:?}");

    let stopped = run(&["daemon", "stop"]).await;
    assert_eq!(stopped.code, 0, "{stopped:?}");
    assert!(!home.path().join("hutand.sock").exists());
    assert_eq!(run(&["daemon", "status"]).await.code, 3);

    // A fresh start after a stop works (the lock was released).
    let restarted = run(&["daemon", "start", "--json"]).await.json();
    assert_eq!(restarted["started"], true);
    assert_ne!(restarted["epoch"], started["epoch"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_start_finds_hutand_on_path() {
    let bin_dir = hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonReaper::new(home.path());
    // A copy of `hutan` somewhere without a sibling hutand.
    let lonely = tempfile::tempdir().unwrap();
    let copy = lonely.path().join("hutan");
    std::fs::copy(HUTAN, &copy).unwrap();

    let mut missing = command(&copy, None, &["daemon", "start"]);
    missing
        .env("HUTAN_HOME", home.path())
        .env("PATH", lonely.path());
    let out = finish(missing).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("hutand not found") && out.stderr.contains("PATH"),
        "{out:?}"
    );

    let mut found = command(&copy, None, &["daemon", "start", "--json"]);
    found.env("HUTAN_HOME", home.path()).env("PATH", &bin_dir);
    let out = finish(found).await;
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.json()["started"], true);
}

// daemon restart (real hutand process) ------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn daemon_restart_without_sessions_gives_a_new_daemon() {
    hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonReaper::new(home.path());
    let run = |args: &'static [&'static str]| {
        let mut cmd = command(Path::new(HUTAN), None, args);
        cmd.env("HUTAN_HOME", home.path());
        finish(cmd)
    };

    // Nothing runs yet: restart just starts the daemon.
    let first = run(&["daemon", "restart", "--json"]).await;
    assert_eq!(first.code, 0, "{first:?}");
    let first = first.json();
    assert_eq!(first["was_running"], false);
    assert_eq!(first["started"], true);

    // An ended session is not live: no flag needed.
    let ended = run(&["new", "--json", "--", "true"]).await.ok().json();
    let id = ended["session"]["id"].as_str().unwrap().to_owned();
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let list = run(&["ls", "--json"]).await.ok().json();
        let session = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id.as_str())
            .cloned()
            .unwrap();
        if session["state"]["state"] != "starting" && session["state"]["state"] != "running" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{session}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let second = run(&["daemon", "restart", "--json"]).await;
    assert_eq!(second.code, 0, "{second:?}");
    let second = second.json();
    assert_eq!(second["was_running"], true);
    assert_eq!(second["started"], true);
    assert_ne!(second["epoch"], first["epoch"], "a new daemon epoch");
    assert_ne!(second["pid"], first["pid"], "a new daemon process");
    // The session that ended before the restart is listed again, marked as restored.
    assert_eq!(second["sessions"], 1, "the stored session comes back");
    let listed = run(&["ls", "--json"]).await.ok().json();
    assert_eq!(listed["sessions"][0]["id"], id.as_str());
    assert_eq!(listed["sessions"][0]["restored"], true, "{listed}");
    let table = run(&["ls"]).await.ok();
    assert!(table.stdout.contains(", restored"), "{table:?}");

    let status = run(&["daemon", "status", "--json"]).await.ok().json();
    assert_eq!(status["epoch"], second["epoch"]);

    let human = run(&["daemon", "restart"]).await.ok();
    assert!(human.stdout.starts_with("restarted: hutand"), "{human:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_restart_refuses_live_sessions_unless_asked_to_stop_them() {
    hutand_dir();
    let home = tempfile::tempdir().unwrap();
    let _guard = DaemonReaper::new(home.path());
    let run = |args: &'static [&'static str]| {
        let mut cmd = command(Path::new(HUTAN), None, args);
        cmd.env("HUTAN_HOME", home.path());
        finish(cmd)
    };

    let started = run(&["daemon", "start", "--json"]).await.ok().json();
    run(&["new", "--json", "--", "sleep", "600"]).await.ok();
    let before = run(&["ls", "--json"]).await.ok().json();

    let refused = run(&["daemon", "restart"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("1 live session(s)")
            && refused.stderr.contains("daemon restart --stop-sessions"),
        "the error names the sessions and the flag: {refused:?}"
    );
    let still = run(&["daemon", "status", "--json"]).await.ok().json();
    assert_eq!(still["epoch"], started["epoch"], "nothing was restarted");
    assert_eq!(still["pid"], started["pid"]);

    let restarted = run(&["daemon", "restart", "--stop-sessions", "--json"]).await;
    assert_eq!(restarted.code, 0, "{restarted:?}");
    let restarted = restarted.json();
    assert_eq!(restarted["was_running"], true);
    assert_ne!(restarted["epoch"], started["epoch"]);
    assert_ne!(restarted["pid"], started["pid"]);
    // Stopping sessions for a restart keeps them in the restore set: the new daemon runs the
    // same session again.
    let list = run(&["ls", "--json"]).await.ok().json();
    let sessions = list["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{list}");
    assert_eq!(sessions[0]["id"], before["sessions"][0]["id"], "{list}");
    assert_eq!(sessions[0]["state"]["state"], "running", "{list}");
    assert!(sessions[0].get("restored").is_none(), "{list}");

    // The restart is said on the session: `ls` has a RECOVERY column, `hutan recovery` the
    // summary (it was a clean stop), and `recovery ack` dismisses it.
    assert_eq!(
        sessions[0]["recovery"]["kind"]["type"], "shell_respawned",
        "{list}"
    );
    assert_eq!(
        sessions[0]["recovery"]["daemon_stop"]["state"], "clean",
        "{list}"
    );
    let table = run(&["ls"]).await.ok();
    assert!(
        table.stdout.contains("RECOVERY") && table.stdout.contains("respawned"),
        "{table:?}"
    );
    let summary = run(&["recovery"]).await.ok();
    assert!(
        summary.stdout.contains("hutand restarted at 20")
            && (summary.stdout.contains(", just now") || summary.stdout.contains(" ago"))
            && summary.stdout.contains("(it was stopped)")
            && summary.stdout.contains("1 session: 1 shell respawned"),
        "{summary:?}"
    );
    assert!(!summary.stdout.contains("still running"), "{summary:?}");
    let refused = run(&["recovery", "ack"]).await;
    assert_ne!(
        refused.code, 0,
        "a target or --all is required: {refused:?}"
    );
    let acked = run(&["recovery", "ack", "--all", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(acked["sessions"].as_array().unwrap().len(), 1, "{acked}");
    let after = run(&["ls", "--json"]).await.ok().json();
    assert!(after["sessions"][0].get("recovery").is_none(), "{after}");
    let nothing = run(&["recovery"]).await.ok();
    assert!(
        nothing.stdout.contains("no session was changed"),
        "{nothing:?}"
    );
}
