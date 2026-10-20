//! The environment of a real child process in both `env_inherit` modes: `/usr/bin/env` run in a
//! PTY while the test process carries a sentinel variable.
#![allow(unsafe_code)] // `set_var`, see below
#![allow(clippy::expect_used)] // tests assert by panicking

use std::collections::BTreeMap;
use std::time::Duration;

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::{EnvInherit, RenderTracker, Terminal, TerminalConfig};
use tokio::time::timeout;

const SENTINEL: &str = "HUTAN_B1_SENTINEL";

/// A stand-in login shell: exports two variables and prints its environment, as `<shell> -l -c
/// '/usr/bin/env -0'` is asked to. It keeps the test off the account's real shell and its
/// configuration.
fn fake_login_shell(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("login-shell");
    std::fs::write(
        &path,
        "#!/bin/sh\nexport FROM_LOGIN_SHELL=yes EDITOR=from-login-shell PWD=/dropped\nexec /usr/bin/env -0\n",
    )
    .expect("write the fake shell");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // macOS vets the first execution of a new executable, which can take tens of seconds on a
    // loaded machine; do that here, outside the timeouts the test is about.
    let _ = std::process::Command::new(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    path.to_string_lossy().into_owned()
}

/// Runs `/usr/bin/env` with `client_env` and returns what it printed as a map.
async fn child_env(
    env_inherit: EnvInherit,
    client_env: &[(&str, &str)],
) -> BTreeMap<String, String> {
    let term = Terminal::spawn(TerminalConfig {
        argv: vec!["/usr/bin/env".to_owned()],
        env_inherit,
        env: client_env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        // Wide enough that no line wraps.
        size: TerminalSize {
            cols: 4000,
            rows: 200,
            pixel_width: 0,
            pixel_height: 0,
        },
        ..TerminalConfig::default()
    })
    .expect("spawn env");
    timeout(Duration::from_secs(15), term.wait_exit())
        .await
        .expect("env exits");
    let snapshot = term.snapshot(&mut RenderTracker::new());
    snapshot
        .lines
        .iter()
        .map(|row| {
            row.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.to_owned(), value.trim_end().to_owned()))
        })
        .collect()
}

// One test: the sentinel is process-wide, and `set_var` is unsafe in edition 2024 (no other
// thread of this test binary reads the environment concurrently: nextest runs one test per
// process, and the PTY threads start only after it is set).
#[tokio::test]
async fn login_mode_hides_the_daemons_environment_and_daemon_mode_keeps_it() {
    // SAFETY: see above; set before anything else runs.
    unsafe { std::env::set_var(SENTINEL, "from-the-daemon") };

    let dir = tempfile::tempdir().expect("temp dir");
    let shell = fake_login_shell(dir.path());
    // A spawn waits at most two seconds for the first capture of the login shell; on a loaded
    // machine an early one can miss it, a later one (cached for the process) does not.
    let mut login = child_env(EnvInherit::Login, &[("EDITOR", "ed"), ("SHELL", &shell)]).await;
    for _ in 0..10 {
        if login.contains_key("FROM_LOGIN_SHELL") {
            break;
        }
        login = child_env(EnvInherit::Login, &[("EDITOR", "ed"), ("SHELL", &shell)]).await;
    }
    // The explicit command starts from the login shell's environment; the client's wins.
    assert_eq!(login["FROM_LOGIN_SHELL"], "yes");
    assert_eq!(login["EDITOR"], "ed");
    assert_ne!(login.get("PWD").map(String::as_str), Some("/dropped"));
    assert!(!login.contains_key(SENTINEL), "leaked: {login:?}");
    for key in [
        "HOME", "USER", "LOGNAME", "SHELL", "PATH", "LANG", "TERM", "EDITOR",
    ] {
        assert!(login.contains_key(key), "{key} missing: {login:?}");
    }
    assert_eq!(login["HOME"], std::env::var("HOME").expect("HOME is set"));
    assert_eq!(login["EDITOR"], "ed");
    assert_eq!(login["TERM_PROGRAM"], "hutan");
    assert!(
        login["PATH"].contains("/usr/bin"),
        "path_helper PATH: {}",
        login["PATH"]
    );
    assert!(
        login["TMPDIR"].starts_with("/var/folders/"),
        "TMPDIR: {}",
        login["TMPDIR"]
    );
    // Only what the base, the client and the terminal contribute: nothing else of the test
    // process (cargo's variables, `PWD`, …) arrives.
    assert!(
        !login.contains_key("CARGO_MANIFEST_DIR") && !login.contains_key("PWD"),
        "{login:?}"
    );

    let daemon = child_env(EnvInherit::Daemon, &[("SHELL", &shell)]).await;
    assert!(
        !daemon.contains_key("FROM_LOGIN_SHELL"),
        "no capture in daemon mode"
    );
    assert_eq!(daemon[SENTINEL], "from-the-daemon");
    assert_eq!(daemon["TERM"], "xterm-256color");
}
