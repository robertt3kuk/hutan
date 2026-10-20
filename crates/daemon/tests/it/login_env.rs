//! Sessions with an explicit command in login mode start from the environment the user's login
//! shell builds (`<shell> -l -c '/usr/bin/env -0'`, captured once), so a tool that only the
//! shell configuration puts on `PATH` is found. A stand-in login shell script plays the user's
//! shell, and the daemon's logs are scanned for the secret values it exports.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use common::*;
use hutan_protocol::messages::{Event, TerminalSpec};

/// What the daemon logged, at every level.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// Installs the log capture once for this test binary (before any daemon starts, so the shared
/// test helper's own subscriber does not win).
fn logs() -> Logs {
    static LOGS: OnceLock<Logs> = OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        let writer = logs.clone();
        let _ = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .try_init();
        logs
    })
    .clone()
}

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

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    settle(&path);
    path
}

/// Creates a session running `command`, waits for it to exit and returns its screen.
async fn run_to_exit(t: &TestDaemon, command: &[&str]) -> String {
    let (client, mut events) = t.connect_subscribed().await;
    let session = create(
        &client,
        TerminalSpec {
            command: command.iter().map(|c| (*c).to_owned()).collect(),
            size: Some(size(4000, 100)),
            ..TerminalSpec::default()
        },
    )
    .await;
    wait_event(&mut events, "session.exited", |e| match e {
        Event::SessionExited { session_id, .. } if *session_id == session.id => Some(()),
        _ => None,
    })
    .await;
    let view = View::attach(&client, terminal_of(&session), None).await;
    view.screen.text()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_only_the_login_shell_puts_on_path_is_found_by_an_explicit_command() {
    let logs = logs();
    let dir = tempfile::tempdir().unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    script(
        &tools,
        "stand-in-tool",
        "echo \"TOOL-RAN secret=$B1_SECRET_VALUE\"",
    );
    // The user's shell configuration: `PATH` and a secret-looking variable, plus the noise a
    // shell reports about itself.
    let shell = script(
        dir.path(),
        "login-shell",
        &format!(
            "export PATH=\"{}:$PATH\" B1_SECRET_VALUE=sentinel-secret-1 PWD=/dropped SHLVL=7 TERM=dumb\nexec /usr/bin/env -0",
            tools.display()
        ),
    );
    let t = TestDaemon::start_with("", |config| config.login_shell = Some(shell.clone())).await;

    // A session started before the first capture is done (a spawn waits at most two seconds for
    // it, and starting scripts can be slow on a loaded machine) runs from the base environment
    // and cannot find the tool; the capture is cached, so a later one does.
    let mut screen = String::new();
    for _ in 0..10 {
        screen = run_to_exit(&t, &["stand-in-tool"]).await;
        if screen.contains("TOOL-RAN") {
            break;
        }
    }
    assert!(
        screen.contains("TOOL-RAN secret=sentinel-secret-1"),
        "the tool was not found or lacks the login environment:\n{screen}"
    );

    // What the shell reported about itself is not part of the environment, and the session's
    // terminal variables stand.
    let screen = run_to_exit(&t, &["/usr/bin/env"]).await;
    assert!(!screen.contains("PWD=/dropped"), "{screen}");
    assert!(!screen.contains("SHLVL=7"), "{screen}");
    assert!(screen.contains("TERM=xterm-256color"), "{screen}");
    assert!(screen.contains("HUTAN_SESSION_ID="), "{screen}");

    t.stop().await;
    let logged = logs.text();
    assert!(
        !logged.contains("sentinel-secret-1"),
        "a captured value reached the logs:\n{logged}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_login_shell_means_the_base_environment_and_a_warning_without_its_output() {
    let logs = logs();
    let dir = tempfile::tempdir().unwrap();
    let shell = script(
        dir.path(),
        "broken-shell",
        "echo B1_LEAK=sentinel-secret-2\necho sentinel-secret-2 >&2\nexit 3",
    );
    let t = TestDaemon::start_with("", |config| config.login_shell = Some(shell.clone())).await;

    // The session still starts, from the base environment (whether or not the capture, which a
    // spawn waits two seconds for, has failed yet).
    let screen = run_to_exit(&t, &["/usr/bin/env"]).await;
    assert!(screen.contains("HUTAN_SESSION_ID="), "{screen}");
    assert!(
        screen.contains("PATH=/usr/local/bin"),
        "path_helper PATH:\n{screen}"
    );
    assert!(!screen.contains("sentinel-secret-2"), "{screen}");

    t.stop().await;
    let started = std::time::Instant::now();
    while !logs.text().contains("login environment capture failed") {
        assert!(started.elapsed() < WAIT, "no warning:\n{}", logs.text());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let logged = logs.text();
    assert!(
        logged.contains(&*shell.to_string_lossy()) && logged.contains("exited"),
        "the warning does not name the shell and the reason:\n{logged}"
    );
    assert!(
        !logged.contains("sentinel-secret-2"),
        "the shell's output reached the logs:\n{logged}"
    );
}
