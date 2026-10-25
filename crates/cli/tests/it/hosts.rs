//! `hutan host …` and `hutan --host NAME <command>` (slice R-2, ADR 0022): profiles in
//! `settings.toml`, commands routed to a daemon over a fake `ssh` that execs `hutan proxy`, what
//! `--host` refuses, and `host forward` with its fake `ssh`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::time::{sleep, timeout};

use crate::common::*;
use crate::ssh_host::FakeSsh;

/// A scratch `HUTAN_HOME` holding `settings.toml`, with a host called `fake` that runs `ssh`.
async fn home_with_host(ssh: &FakeSsh) -> TempDir {
    let home = tempfile::tempdir().unwrap();
    enable_remote_hosts(home.path());
    hutan_in(
        home.path(),
        &ssh.script,
        &["host", "add", "fake", "me@fake.example"],
    )
    .await
    .ok();
    home
}

#[tokio::test]
async fn hosts_are_added_listed_and_removed_in_settings_toml() {
    let home = tempfile::tempdir().unwrap();
    enable_remote_hosts(home.path());
    let ssh = Path::new("/nonexistent/ssh");
    let h = |args: &'static [&'static str]| hutan_in(home.path(), ssh, args);

    let empty = h(&["host", "list", "--json"]).await.ok().json();
    assert_eq!(empty["hosts"].as_array().unwrap().len(), 0);
    assert!(
        h(&["host", "list"])
            .await
            .ok()
            .stdout
            .contains("hutan host add")
    );

    h(&[
        "host",
        "add",
        "studio",
        "me@studio.local",
        "--hutan",
        "/opt/homebrew/bin/hutan",
    ])
    .await
    .ok();
    h(&["host", "add", "box", "box-alias"]).await.ok();
    let listed = h(&["host", "list", "--json"]).await.ok().json();
    assert_eq!(
        listed["hosts"],
        serde_json::json!([
            {"name": "studio", "ssh": "me@studio.local", "hutan": "/opt/homebrew/bin/hutan"},
            {"name": "box", "ssh": "box-alias"},
        ])
    );
    let text = h(&["host", "ls"]).await.ok().stdout;
    assert!(
        text.contains("studio") && text.contains("(PATH on the host)"),
        "{text}"
    );

    let file = std::fs::read_to_string(home.path().join("settings.toml")).unwrap();
    assert!(file.contains("[[host]]"), "{file}");
    assert!(file.contains("ssh = \"me@studio.local\""), "{file}");

    // Mistakes are refused with a usage error, and change nothing.
    for bad in [
        &["host", "add", "studio", "other@x"][..],
        &["host", "add", "bad name", "x"][..],
        &["host", "add", "evil", "-oProxyCommand=x"][..],
    ] {
        let out = hutan_in(home.path(), ssh, bad).await;
        assert_eq!(out.code, 2, "{bad:?}: {out:?}");
    }
    assert_eq!(
        h(&["host", "list", "--json"]).await.ok().json()["hosts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    h(&["host", "remove", "studio"]).await.ok();
    assert_eq!(h(&["host", "remove", "studio"]).await.code, 1);
    h(&["host", "rm", "box"]).await.ok();
    let file = std::fs::read_to_string(home.path().join("settings.toml")).unwrap();
    assert!(!file.contains("[[host]]"), "{file:?}");
    assert!(file.contains("remote_hosts = true"), "{file:?}");
}

#[tokio::test]
async fn host_commands_reach_the_daemon_through_ssh() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let home = home_with_host(&ssh).await;
    let id = t.new_session(&[], &["sh", "-c", "exec cat"]).await;

    // The home here has no daemon and no socket: the answer can only come through ssh.
    let ls = hutan_in(
        home.path(),
        &ssh.script,
        &["--host", "fake", "ls", "--json"],
    )
    .await
    .ok()
    .json();
    let sessions = ls["sessions"].as_array().unwrap();
    assert!(sessions.iter().any(|s| s["id"] == id), "{ls}");

    let status = hutan_in(
        home.path(),
        &ssh.script,
        &["--host", "fake", "daemon", "status", "--json"],
    )
    .await
    .ok()
    .json();
    assert_eq!(
        status["host"],
        serde_json::json!({"name": "fake", "ssh": "me@fake.example"})
    );
    assert!(status.get("socket").is_none(), "{status}");
    assert_eq!(status["epoch"], t.daemon.epoch().to_string());
    let human = hutan_in(
        home.path(),
        &ssh.script,
        &["--host", "fake", "daemon", "status"],
    )
    .await
    .ok()
    .stdout;
    assert!(
        human.contains("host   fake (ssh me@fake.example)"),
        "{human}"
    );

    // Every command was one ssh run of `hutan proxy` on the host.
    let runs = ssh.runs();
    assert_eq!(runs.len(), 3, "{runs:?}");
    for run in &runs {
        assert!(run.contains(&"BatchMode=yes".to_owned()), "{run:?}");
        assert_eq!(
            run.last().map(String::as_str),
            Some("hutan proxy"),
            "{run:?}"
        );
    }

    // A mutation lands on the host's daemon.
    hutan_in(
        home.path(),
        &ssh.script,
        &["--host", "fake", "rename", &id[..12], "over-ssh"],
    )
    .await
    .ok();
    assert_eq!(t.session(&id).await["title"], "over-ssh");
    t.stop().await;
}

#[tokio::test]
async fn host_names_and_commands_that_cannot_work_are_refused_before_any_ssh() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let home = home_with_host(&ssh).await;
    let run = |args: &'static [&'static str]| hutan_in(home.path(), &ssh.script, args);

    let unknown = run(&["--host", "nope", "ls"]).await;
    assert_eq!(unknown.code, 2, "{unknown:?}");
    assert!(
        unknown.stderr.contains("no host named `nope`") && unknown.stderr.contains("known: fake"),
        "{unknown:?}"
    );

    for args in [
        &["--host", "fake", "daemon", "stop"][..],
        &["--host", "fake", "daemon", "start"][..],
        &["--host", "fake", "mcp"][..],
        &["--host", "fake", "proxy"][..],
        &["--host", "fake", "host", "list"][..],
        &["--host", "fake", "theme", "ls"][..],
    ] {
        let out = hutan_in(home.path(), &ssh.script, args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(out.stderr.contains("--host fake"), "{args:?}: {out:?}");
    }

    // The current directory here is no directory there.
    let cwd = run(&["--host", "fake", "new", "--", "sh"]).await;
    assert_eq!(cwd.code, 2, "{cwd:?}");
    assert!(cwd.stderr.contains("absolute path on the host"), "{cwd:?}");
    let relative = run(&["--host", "fake", "new", "--cwd", "rel/dir", "--", "sh"]).await;
    assert_eq!(relative.code, 2, "{relative:?}");

    // `--host` and `--socket` are two ways to say where; both is a mistake.
    let both = run(&["--host", "fake", "--socket", "/tmp/x.sock", "ls"]).await;
    assert_eq!(both.code, 2, "{both:?}");

    assert!(ssh.runs().is_empty(), "ssh ran: {:?}", ssh.runs());
    t.stop().await;
}

#[tokio::test]
async fn a_new_session_with_an_absolute_path_runs_on_the_host() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let home = home_with_host(&ssh).await;
    let dir = tempfile::tempdir().unwrap();
    let out = hutan_in(
        home.path(),
        &ssh.script,
        &[
            "--host",
            "fake",
            "new",
            "--json",
            "--cwd",
            dir.path().to_str().unwrap(),
            "--",
            "sh",
            "-c",
            "exec cat",
        ],
    )
    .await
    .ok()
    .json();
    let id = out["session"]["id"].as_str().unwrap();
    let session = t.session(id).await;
    assert_eq!(
        std::fs::canonicalize(session["cwd"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(dir.path()).unwrap()
    );
    t.stop().await;
}

#[tokio::test]
async fn host_test_reports_what_answers_and_what_does_not() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let home = home_with_host(&ssh).await;
    let out = hutan_in(
        home.path(),
        &ssh.script,
        &["host", "test", "fake", "--json"],
    )
    .await
    .ok()
    .json();
    assert_eq!(out["connected"], true);
    assert_eq!(out["host"], "fake");
    assert_eq!(out["daemon"]["epoch"], t.daemon.epoch().to_string());
    assert!(out["connect_ms"].as_f64().unwrap() > 0.0, "{out}");
    assert!(out["round_trip_ms"].as_f64().unwrap() > 0.0, "{out}");
    assert_eq!(out["calls"], 5);
    let human = hutan_in(home.path(), &ssh.script, &["host", "test", "fake"])
        .await
        .ok()
        .stdout;
    assert!(
        human.contains("host fake (ssh me@fake.example): ok"),
        "{human}"
    );
    assert!(human.contains("round trip"), "{human}");
    t.stop().await;

    // A host that cannot be reached: unreachable (3), with what ssh said.
    let broken = FakeSsh::with_body(
        "echo 'me@fake.example: Permission denied (publickey).' >&2\nexit 255\n",
    );
    let home = home_with_host(&broken).await;
    let failed = hutan_in(home.path(), &broken.script, &["host", "test", "fake"]).await;
    assert_eq!(failed.code, 3, "{failed:?}");
    assert!(
        failed.stderr.contains("Permission denied (publickey)"),
        "{failed:?}"
    );
}

#[tokio::test]
async fn host_forward_runs_ssh_with_a_replaceable_socket_and_cleans_up() {
    // The fake `ssh` answers `hutan daemon start --json` with the host's socket and, when
    // asked to forward, stays up.
    let ssh = FakeSsh::with_body(
        "case \"$*\" in\n  *'daemon start --json'*) echo '{\"started\":false,\"socket\":\"/home/me/hutand.sock\"}'; exit 0;;\nesac\nexec sleep 30\n",
    );
    let home = home_with_host(&ssh).await;
    let listen = home.path().join("fwd.sock");
    let mut command = command(
        Path::new(HUTAN),
        None,
        &[
            "host",
            "forward",
            "fake",
            "--listen",
            listen.to_str().unwrap(),
            "--json",
        ],
    );
    command
        .env("HUTAN_HOME", home.path())
        .env("HUTAN_SSH", &ssh.script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = spawn_child(&mut command);

    // The second ssh run is the forward.
    timeout(WAIT, async {
        while ssh.runs().len() < 2 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the forward was not started");
    let forward = &ssh.runs()[1];
    let l = forward.iter().position(|a| a == "-L").unwrap();
    assert_eq!(
        forward[l + 1],
        format!("{}:/home/me/hutand.sock", listen.display())
    );
    for option in [
        "-N",
        "StreamLocalBindUnlink=yes",
        "ExitOnForwardFailure=yes",
        "BatchMode=yes",
    ] {
        assert!(
            forward.contains(&option.to_owned()),
            "{option}: {forward:?}"
        );
    }
    assert_eq!(forward.last().map(String::as_str), Some("me@fake.example"));

    // Interrupted, it stops its ssh and exits cleanly.
    let pid = child.id().unwrap();
    assert!(
        std::process::Command::new("kill")
            .arg("-INT")
            .arg(pid.to_string())
            .status()
            .unwrap()
            .success()
    );
    let output = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let printed: serde_json::Value =
        serde_json::from_slice(output.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(printed["remote_socket"], "/home/me/hutand.sock");
    assert_eq!(printed["host"], "fake");
}

#[tokio::test]
async fn remote_hosts_are_refused_until_the_experimental_setting_is_on() {
    let t = TestDaemon::start().await;
    let ssh = FakeSsh::proxying(&t.paths.socket);
    let home = tempfile::tempdir().unwrap();
    let run = |args: &'static [&'static str]| hutan_in(home.path(), &ssh.script, args);

    // Off (the default): `--host` and `host` refuse before anything runs, naming the setting
    // and saying what it risks.
    for args in [
        &["host", "list"][..],
        &["host", "add", "fake", "me@fake.example"][..],
        &["host", "test", "fake"][..],
        &["--host", "fake", "ls"][..],
    ] {
        let out = hutan_in(home.path(), &ssh.script, args).await;
        assert_eq!(out.code, 2, "{args:?}: {out:?}");
        assert!(
            out.stderr.contains("experimental.remote_hosts"),
            "{args:?}: {out:?}"
        );
        assert!(
            out.stderr.contains("Use at your own risk"),
            "{args:?}: {out:?}"
        );
        assert!(
            out.stderr.contains("Settings ▸ Experimental"),
            "{args:?}: {out:?}"
        );
    }
    assert!(ssh.runs().is_empty());
    assert!(
        !home.path().join("settings.toml").exists(),
        "nothing was written"
    );

    // A workspace cannot switch it on: only the user's file counts.
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".hutan")).unwrap();
    std::fs::write(
        repo.path().join(".hutan/settings.toml"),
        "[experimental]\nremote_hosts = true\n",
    )
    .unwrap();
    let mut command = command(Path::new(HUTAN), None, &["host", "list"]);
    command
        .env("HUTAN_HOME", home.path())
        .current_dir(repo.path());
    assert_eq!(finish(command).await.code, 2);

    // `hutan proxy` is what the host's ssh runs: it never reads the setting (`proxy.rs` runs it
    // with no settings at all), and it is not in `--help`.
    let help = run(&["--help"]).await.ok().stdout;
    assert!(!help.contains("proxy"), "{help}");
    assert!(
        help.contains("host"),
        "`hutan host` stays listed, marked experimental: {help}"
    );
    assert!(help.contains("EXPERIMENTAL"), "{help}");

    // On: the same commands work, and `add` and `test` print the disclaimer.
    enable_remote_hosts(home.path());
    let added = run(&["host", "add", "fake", "me@fake.example"]).await.ok();
    assert!(added.stdout.contains("Use at your own risk"), "{added:?}");
    let tested = run(&["host", "test", "fake"]).await.ok();
    assert!(tested.stdout.contains("Use at your own risk"), "{tested:?}");
    assert!(
        run(&["host", "test", "fake", "--json"]).await.ok().json()["disclaimer"]
            .as_str()
            .is_some_and(|d| d.contains("at your own risk"))
    );
    // Its listing does not repeat it.
    assert!(
        !run(&["host", "list"])
            .await
            .ok()
            .stdout
            .contains("own risk")
    );
    run(&["--host", "fake", "ls"]).await.ok();
    t.stop().await;
}
