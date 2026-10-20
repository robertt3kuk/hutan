//! `hutan search --resume N`: a new session in the hit's working directory running the agent's
//! resume command. `claude` is a stand-in script, never the real agent: the daemon runs in the
//! default login mode, so the stand-in is found the way a real agent is, through the `PATH` that
//! the (here fake) login shell exports.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use common::{TestDaemon, WAIT};
use hutan_daemon::{DaemonConfig, Paths};
use tokio::time::sleep;

/// Runs a freshly written script once and waits for it. macOS vets the first execution of a new
/// executable, which can take tens of seconds on a loaded machine; doing that here keeps it out
/// of the timeouts of what the test is about.
fn settle(path: &std::path::Path) {
    let _ = std::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_opens_a_session_in_the_hits_cwd_with_the_resume_command() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (home, claude, project, bin) = (
        base.join("home"),
        base.join("claude"),
        base.join("project"),
        base.join("bin"),
    );
    for dir in [&home, &claude, &project, &bin] {
        fs::create_dir_all(dir).unwrap();
    }
    // A stand-in `claude` that reports how it was started.
    let script = bin.join("claude");
    fs::write(
        &script,
        "#!/bin/sh\necho \"FAKE-CLAUDE $* IN $(pwd)\"\nexec cat\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    settle(&script);
    // The login shell: puts `bin` on `PATH` like a user's shell configuration does and prints
    // its environment, as `<shell> -l -c '/usr/bin/env -0'` is asked to.
    let shell = base.join("login-shell");
    fs::write(
        &shell,
        format!(
            "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nexec /usr/bin/env -0\n",
            bin.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).unwrap();
    settle(&shell);

    fs::write(
        home.join("daemon.toml"),
        format!(
            "[history]\nenabled = true\ncodex = false\nomp = false\nhutan = false\n[history.roots]\nclaude = \"{}\"\n",
            claude.display()
        ),
    )
    .unwrap();
    let paths = Paths::in_home(&home);
    let daemon = hutan_daemon::start(DaemonConfig {
        login_shell: Some(shell),
        ..DaemonConfig::new(paths.clone())
    })
    .await
    .unwrap();
    let t = TestDaemon {
        _dir: tempfile::tempdir().unwrap(),
        paths,
        daemon,
    };

    let line = format!(
        r#"{{"type":"user","cwd":"{}","message":{{"role":"user","content":[{{"type":"text","text":"remember the pangolin plan"}}]}}}}"#,
        project.display()
    );
    let file = claude.join("-proj/sess-xyz.jsonl");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, format!("{line}\n")).unwrap();

    let start = Instant::now();
    loop {
        let out = t.hutan(&["search", "--json", "pangolin"]).await;
        if out.code == 0 && !out.json()["hits"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(start.elapsed() < WAIT, "not indexed: {out:?}");
        sleep(Duration::from_millis(100)).await;
    }

    // Not a terminal: the session is created but not attached.
    let out = t
        .hutan(&["search", "--json", "pangolin", "--resume", "1"])
        .await
        .ok();
    let id = out.json()["session"]["id"].as_str().unwrap().to_owned();
    let session = t.session(&id).await;
    assert_eq!(
        session["command"],
        serde_json::json!(["claude", "--resume", "sess-xyz"])
    );
    assert_eq!(session["cwd"], project.to_str().unwrap());
    assert_eq!(session["title"], "resume: remember the pangolin plan");

    let expected = format!("FAKE-CLAUDE --resume sess-xyz IN {}", project.display());
    let start = Instant::now();
    loop {
        let out = t.hutan(&["history", &id]).await;
        // The screen is 80 columns wide: the line wraps.
        if out.stdout.replace('\n', "").contains(&expected) {
            break;
        }
        assert!(
            start.elapsed() < WAIT,
            "the resume command did not run: {out:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
    t.stop().await;
}
