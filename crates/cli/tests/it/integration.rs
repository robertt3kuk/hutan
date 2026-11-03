//! `hutan integration install | uninstall | status` on temporary copies of real-shaped agent
//! configuration. Every command gets `--dir` or a temporary `HOME`/`CODEX_HOME`/
//! `CLAUDE_CONFIG_DIR`/`PI_CODING_AGENT_DIR`: nothing here can reach the user's own agents.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use common::{HUTAN, Out, command, finish};
use serde_json::{Value, json};
use tempfile::TempDir;

async fn hutan(args: &[&str], env: &[(&str, &Path)]) -> Out {
    let mut command = command(Path::new(HUTAN), None, args);
    for (key, value) in env {
        command.env(key, value);
    }
    finish(command).await
}

async fn run(args: &[&str], dir: &Path) -> Out {
    let mut full = args.to_vec();
    full.extend(["--dir", dir.to_str().unwrap()]);
    hutan(&full, &[("HOME", dir)]).await
}

/// JSON as the agents write it: two-space indentation and a trailing newline.
fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

fn backups(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("{name}.hutan-"))
        })
        .collect();
    found.sort();
    found
}

/// A directory name with a space and an apostrophe, to exercise the quoting of the command.
fn awkward_dir(root: &TempDir, name: &str) -> PathBuf {
    let dir = root.path().join(format!("it's {name}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Codex's `hooks.json` as Herdr and Orca leave it (their entries abbreviated, shape kept).
fn codex_hooks() -> Value {
    let orca = ": orca-agent-hook-form=1; if [ -n \"${ORCA_PANE_KEY-}\" ]; then /bin/sh \"$X/codex-hook.sh\" || :; else { command -p cat 2>/dev/null || cat; } >/dev/null 2>&1 || :; fi";
    json!({
        "hooks": {
            "SessionStart": [
                {"hooks": [{"command": "bash '/h/.codex/herdr-agent-state.sh' session", "timeout": 10, "type": "command"}]},
                {"hooks": [{"type": "command", "command": orca, "timeout": 10}]}
            ],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": orca, "timeout": 10}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": orca, "timeout": 10}]}],
            "PermissionRequest": [{"hooks": [{"type": "command", "command": orca, "timeout": 10}]}],
            "Stop": [{"hooks": [{"type": "command", "command": orca, "timeout": 10}]}]
        }
    })
}

/// Claude Code's `settings.json` with unrelated settings and hooks of other tools.
fn claude_settings() -> Value {
    json!({
        "model": "opus",
        "hooks": {
            "SessionStart": [
                {"matcher": "*", "hooks": [{"type": "command", "command": "bash '/h/.claude/hooks/herdr-agent-state.sh' session", "timeout": 10}]}
            ],
            "Stop": [{"hooks": [{"type": "command", "command": "echo done", "timeout": 5}]}],
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "audit.sh"}]}]
        },
        "statusLine": {"type": "command", "command": "statusline.sh"},
        "theme": "dark"
    })
}

fn command_of(hook: &Value) -> &str {
    hook["command"].as_str().unwrap()
}

/// Every Hutan hook command in a hooks document.
fn hutan_commands(doc: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (event, groups) in doc["hooks"].as_object().unwrap() {
        for group in groups.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                if command_of(hook).contains("hutan-integration=") {
                    found.push((event.clone(), command_of(hook).to_owned()));
                }
            }
        }
    }
    found
}

#[tokio::test]
async fn codex_install_keeps_other_hooks_and_uninstall_restores_the_original() {
    let root = TempDir::new().unwrap();
    let dir = awkward_dir(&root, "codex home");
    let original = pretty(&codex_hooks());
    let hooks_file = dir.join("hooks.json");
    fs::write(&hooks_file, &original).unwrap();
    fs::write(dir.join("herdr-agent-state.sh"), "#!/bin/sh\n# herdr's\n").unwrap();

    let out = run(&["integration", "install", "codex"], &dir).await.ok();
    assert!(out.stdout.contains("codex: installed"), "{out:?}");

    // Only additions: every original entry is still there, in place.
    let installed: Value = serde_json::from_str(&fs::read_to_string(&hooks_file).unwrap()).unwrap();
    let ours = hutan_commands(&installed);
    let mut events: Vec<_> = ours.iter().map(|(event, _)| event.as_str()).collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "PermissionRequest",
            "PostToolUse",
            "SessionStart",
            "Stop",
            "UserPromptSubmit"
        ],
        "one hook per event"
    );
    for (event, groups) in codex_hooks()["hooks"].as_object().unwrap() {
        let kept = &installed["hooks"][event.as_str()].as_array().unwrap()
            [..groups.as_array().unwrap().len()];
        assert_eq!(
            kept,
            groups.as_array().unwrap().as_slice(),
            "{event}: original groups first, untouched"
        );
    }
    assert_eq!(
        installed["hooks"]["PreToolUse"],
        codex_hooks()["hooks"]["PreToolUse"]
    );

    // The command runs the installed script, quoted for sh.
    let script = dir.join("hutan-agent-state.sh");
    assert!(script.is_file());
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o111,
        0o111
    );
    let (_, command) = &ours[0];
    assert_eq!(
        command,
        &format!(
            ": hutan-integration=codex; sh '{}' codex",
            script.to_str().unwrap().replace('\'', "'\\''")
        )
    );
    assert_eq!(
        fs::read_to_string(dir.join("herdr-agent-state.sh")).unwrap(),
        "#!/bin/sh\n# herdr's\n",
        "the other tool's script is not touched"
    );

    // A backup of the original was written before the edit.
    let made = backups(&dir, "hooks.json");
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(fs::read_to_string(&made[0]).unwrap(), original);

    // Installing again changes nothing: same bytes, no second backup.
    let before = fs::read_to_string(&hooks_file).unwrap();
    let again = run(&["integration", "install", "codex"], &dir).await.ok();
    assert!(again.stdout.contains("already installed"), "{again:?}");
    assert_eq!(fs::read_to_string(&hooks_file).unwrap(), before);
    assert_eq!(backups(&dir, "hooks.json").len(), 1);

    let status = run(&["integration", "status", "codex", "--json"], &dir)
        .await
        .ok();
    assert_eq!(status.json()["status"], "installed");

    // Uninstalling leaves exactly the original bytes and removes only Hutan's script.
    let out = run(&["integration", "uninstall", "codex"], &dir).await.ok();
    assert!(out.stdout.contains("codex: removed"), "{out:?}");
    assert_eq!(fs::read_to_string(&hooks_file).unwrap(), original);
    assert!(!script.exists());
    assert!(dir.join("herdr-agent-state.sh").exists());
    let status = run(&["integration", "status", "codex", "--json"], &dir)
        .await
        .ok();
    assert_eq!(status.json()["status"], "not_installed");
    let again = run(&["integration", "uninstall", "codex"], &dir).await.ok();
    assert!(again.stdout.contains("nothing to remove"), "{again:?}");
}

#[tokio::test]
async fn claude_install_keeps_settings_and_hooks_and_uninstall_restores_the_original() {
    let root = TempDir::new().unwrap();
    let dir = awkward_dir(&root, "claude home");
    let original = pretty(&claude_settings());
    let settings = dir.join("settings.json");
    fs::write(&settings, &original).unwrap();

    run(&["integration", "install", "claude"], &dir).await.ok();
    let installed: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    for key in ["model", "statusLine", "theme"] {
        assert_eq!(installed[key], claude_settings()[key], "{key}");
    }
    let events: Vec<String> = hutan_commands(&installed)
        .into_iter()
        .map(|(event, _)| event)
        .collect();
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PostToolUseFailure",
        "PostModelSwitch",
        "Stop",
        "StopFailure",
        "SessionEnd",
        "SubagentStart",
        "SubagentStop",
    ] {
        assert!(
            events.iter().any(|e| e == event),
            "{event} not hooked: {events:?}"
        );
    }
    // The user's own PreToolUse group stays first and untouched; ours follows, in the background.
    assert_eq!(
        installed["hooks"]["PreToolUse"][0],
        claude_settings()["hooks"]["PreToolUse"][0]
    );
    assert_eq!(
        installed["hooks"]["PreToolUse"][1]["hooks"][0]["async"],
        true
    );
    assert!(
        installed["hooks"]["Stop"][1]["hooks"][0]
            .get("async")
            .is_none()
    );
    // The existing SessionStart group (with its matcher) and Stop hook come first, unchanged.
    assert_eq!(
        installed["hooks"]["SessionStart"][0],
        claude_settings()["hooks"]["SessionStart"][0]
    );
    assert_eq!(
        installed["hooks"]["Stop"][0],
        claude_settings()["hooks"]["Stop"][0]
    );
    let script = dir.join("hooks").join("hutan-agent-state.sh");
    assert!(script.is_file());
    assert_eq!(backups(&dir, "settings.json").len(), 1);

    run(&["integration", "uninstall", "claude"], &dir)
        .await
        .ok();
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);
    assert!(!script.exists());
}

#[tokio::test]
async fn files_keep_their_own_indentation_and_missing_files_are_created() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("claude");
    fs::create_dir_all(&dir).unwrap();

    // Four spaces, no trailing newline.
    let original = serde_json::to_string_pretty(&claude_settings())
        .unwrap()
        .replace("  ", "    ");
    let settings = dir.join("settings.json");
    fs::write(&settings, &original).unwrap();
    run(&["integration", "install", "claude"], &dir).await.ok();
    let text = fs::read_to_string(&settings).unwrap();
    assert!(
        text.contains("\n    \"hooks\""),
        "four-space indentation kept: {text}"
    );
    assert!(!text.ends_with('\n'));
    run(&["integration", "uninstall", "claude"], &dir)
        .await
        .ok();
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);

    // No file yet: created, and gone again after the uninstall (nothing else was in it).
    let fresh = root.path().join("fresh");
    run(&["integration", "install", "codex"], &fresh).await.ok();
    let created: Value =
        serde_json::from_str(&fs::read_to_string(fresh.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(hutan_commands(&created).len(), 5);
    assert!(
        backups(&fresh, "hooks.json").is_empty(),
        "nothing to back up"
    );
    run(&["integration", "uninstall", "codex"], &fresh)
        .await
        .ok();
    assert!(!fresh.join("hooks.json").exists());
    assert!(!fresh.join("hutan-agent-state.sh").exists());
}

#[tokio::test]
async fn omp_extension_is_added_next_to_the_others_and_removed_alone() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("omp agent");
    let extensions = dir.join("extensions");
    fs::create_dir_all(&extensions).unwrap();
    let others = [
        (
            "herdr-omp-agent-state.ts",
            "// installed by herdr\nexport default function () {}\n",
        ),
        (
            "orca-agent-status.ts",
            "// @orca-managed-pi-extension\nexport default function () {}\n",
        ),
    ];
    for (name, text) in others {
        fs::write(extensions.join(name), text).unwrap();
    }

    let out = run(&["integration", "install", "omp"], &dir).await.ok();
    assert!(out.stdout.contains("omp: installed"), "{out:?}");
    let ours = extensions.join("hutan-agent-state.ts");
    let text = fs::read_to_string(&ours).unwrap();
    assert!(text.starts_with("// hutan-managed"), "{text}");
    assert!(text.contains("omp-extension") && text.contains("--resume="));
    for (name, original) in others {
        assert_eq!(fs::read_to_string(extensions.join(name)).unwrap(), original);
    }
    assert!(
        run(&["integration", "install", "omp"], &dir)
            .await
            .ok()
            .stdout
            .contains("already installed")
    );
    assert_eq!(
        run(&["integration", "status", "omp", "--json"], &dir)
            .await
            .ok()
            .json()["status"],
        "installed"
    );

    // An outdated copy is reported and repaired by the next install.
    fs::write(&ours, "// hutan-managed: old\n").unwrap();
    let status = run(&["integration", "status", "omp", "--json"], &dir)
        .await
        .ok()
        .json();
    assert_eq!(status["status"], "partial");
    run(&["integration", "install", "omp"], &dir).await.ok();
    assert_eq!(fs::read_to_string(&ours).unwrap(), text);

    run(&["integration", "uninstall", "omp"], &dir).await.ok();
    assert!(!ours.exists());
    for (name, original) in others {
        assert_eq!(fs::read_to_string(extensions.join(name)).unwrap(), original);
    }
}

#[tokio::test]
async fn foreign_files_and_unreadable_configuration_are_never_overwritten() {
    let root = TempDir::new().unwrap();

    // A file of the same name that Hutan did not write.
    let omp = root.path().join("omp");
    fs::create_dir_all(omp.join("extensions")).unwrap();
    let foreign = omp.join("extensions").join("hutan-agent-state.ts");
    fs::write(&foreign, "// somebody else's\n").unwrap();
    let out = run(&["integration", "install", "omp"], &omp).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("was not written by Hutan"), "{out:?}");
    assert_eq!(
        fs::read_to_string(&foreign).unwrap(),
        "// somebody else's\n"
    );
    run(&["integration", "uninstall", "omp"], &omp).await.ok();
    assert!(
        foreign.exists(),
        "uninstall leaves files that are not Hutan's"
    );

    // Configuration that is not JSON, or not the documented shape.
    for (content, complaint) in [
        ("{ \"hooks\": ", "not valid JSON"),
        ("[1, 2]", "not a JSON object"),
        ("{\"hooks\": [1]}", "not the shape"),
    ] {
        let dir = root.path().join(format!("codex-{}", content.len()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("hooks.json"), content).unwrap();
        let out = run(&["integration", "install", "codex"], &dir).await;
        assert_eq!(out.code, 1, "{content}: {out:?}");
        assert!(out.stderr.contains(complaint), "{content}: {out:?}");
        assert_eq!(fs::read_to_string(dir.join("hooks.json")).unwrap(), content);
        assert!(backups(&dir, "hooks.json").is_empty());
        assert!(
            !dir.join("hutan-agent-state.sh").exists(),
            "no script without a hook to run it"
        );
    }
}

#[tokio::test]
async fn the_default_directory_comes_from_the_agents_own_environment() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let codex_home = root.path().join("codex-home");
    let claude_dir = root.path().join("claude-dir");
    let omp_dir = root.path().join("omp-dir");
    for dir in [&home, &codex_home, &claude_dir, &omp_dir] {
        fs::create_dir_all(dir).unwrap();
    }

    // Variables first.
    let env = [
        ("HOME", home.as_path()),
        ("CODEX_HOME", codex_home.as_path()),
        ("CLAUDE_CONFIG_DIR", claude_dir.as_path()),
        ("PI_CODING_AGENT_DIR", omp_dir.as_path()),
    ];
    for target in ["codex", "claude", "omp"] {
        hutan(&["integration", "install", target], &env).await.ok();
    }
    assert!(codex_home.join("hooks.json").is_file());
    assert!(claude_dir.join("settings.json").is_file());
    assert!(omp_dir.join("extensions/hutan-agent-state.ts").is_file());
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        0,
        "HOME was not used when variables are set"
    );

    // Defaults under HOME otherwise.
    let mut command = common::command(Path::new(HUTAN), None, &["integration", "install", "codex"]);
    command.env("HOME", &home).env_remove("CODEX_HOME");
    finish(command).await.ok();
    assert!(home.join(".codex/hooks.json").is_file());
    for (target, expected) in [
        ("claude", ".claude/settings.json"),
        ("omp", ".omp/agent/extensions/hutan-agent-state.ts"),
    ] {
        let mut command =
            common::command(Path::new(HUTAN), None, &["integration", "install", target]);
        command
            .env("HOME", &home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("PI_CODING_AGENT_DIR");
        finish(command).await.ok();
        assert!(home.join(expected).is_file(), "{expected}");
    }
}

#[tokio::test]
async fn the_installed_script_reports_inside_hutan_and_is_silent_outside() {
    let root = TempDir::new().unwrap();
    let dir = awkward_dir(&root, "codex home");
    run(&["integration", "install", "codex"], &dir).await.ok();
    let hooks: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("hooks.json")).unwrap()).unwrap();
    let (_, hook_command) = hutan_commands(&hooks).remove(0);

    // A stand-in for `hutan` that records how it was called and what it read.
    let bin = root.path().join("fake-hutan");
    let record = root.path().join("record");
    fs::write(
        &bin,
        format!(
            "#!/bin/sh\n{{ echo \"$@\"; cat; }} > '{}'\n",
            record.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();

    let run_hook = |env: &[(&str, &str)]| {
        let mut command = std::process::Command::new("sh");
        command
            .arg("-c")
            .arg(&hook_command)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        {
            use std::io::Write as _;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"{\"hook_event_name\":\"Stop\"}")
                .unwrap();
        }
        child.wait_with_output().unwrap()
    };

    // Outside Hutan: nothing runs, nothing is said, the exit code is 0.
    let out = run_hook(&[("HUTAN_BIN", bin.to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert!(!record.exists());

    // Inside: `hutan agent hook codex` with the hook's stdin.
    let out = run_hook(&[
        ("HUTAN_ENV", "1"),
        ("HUTAN_SESSION_ID", "01K7F3B5N2Q9W8E4R6T1Y0V3KM"),
        ("HUTAN_BIN", bin.to_str().unwrap()),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(&record).unwrap(),
        "agent hook codex\n{\"hook_event_name\":\"Stop\"}"
    );

    // A missing CLI never breaks the hook either.
    fs::remove_file(&record).unwrap();
    let out = run_hook(&[
        ("HUTAN_ENV", "1"),
        ("HUTAN_SESSION_ID", "01K7F3B5N2Q9W8E4R6T1Y0V3KM"),
        ("HUTAN_BIN", "/nonexistent/hutan"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

/// The status-line tee (INSIGHT-B): opt-in, chained to the user's own command, restored by the
/// uninstall. The tee script is run for real with `sh`.
#[tokio::test]
async fn the_status_line_tee_is_opt_in_chains_to_the_users_command_and_is_undone_by_the_uninstall()
{
    let root = TempDir::new().unwrap();
    let dir = awkward_dir(&root, "claude home");
    let mut settings_doc = claude_settings();
    settings_doc["statusLine"] = json!({
        "type": "command",
        "command": "tr a-z A-Z; exit 3",
        "padding": 2
    });
    let original = pretty(&settings_doc);
    let settings = dir.join("settings.json");
    fs::write(&settings, &original).unwrap();

    // Without the flag the status line is never touched.
    run(&["integration", "install", "claude"], &dir).await.ok();
    let plain: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(plain["statusLine"], settings_doc["statusLine"]);
    let status = run(&["integration", "status", "claude", "--json"], &dir)
        .await
        .ok()
        .json();
    assert_eq!(status["statusline"], "not_installed");
    let not_reported = status["not_reported"].to_string();
    assert!(not_reported.contains("--statusline"), "{status}");

    // With it, the tee takes the place, keeping the other keys of the entry.
    let out = run(&["integration", "install", "claude", "--statusline"], &dir)
        .await
        .ok();
    assert!(out.stdout.contains("claude"), "{out:?}");
    let teed: Value = serde_json::from_str(&fs::read_to_string(&settings).unwrap()).unwrap();
    let tee = teed["statusLine"]["command"].as_str().unwrap().to_owned();
    assert!(tee.contains("hutan-integration=claude-statusline"), "{tee}");
    assert_eq!(teed["statusLine"]["padding"], 2);
    assert_eq!(teed["statusLine"]["type"], "command");
    let hooks = dir.join("hooks");
    let script = hooks.join("hutan-statusline.sh");
    assert_eq!(
        fs::read_to_string(hooks.join("hutan-statusline.command")).unwrap(),
        "tr a-z A-Z; exit 3"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            &fs::read_to_string(hooks.join("hutan-statusline.original.json")).unwrap()
        )
        .unwrap()["statusLine"],
        settings_doc["statusLine"]
    );
    let status = run(&["integration", "status", "claude", "--json"], &dir)
        .await
        .ok()
        .json();
    assert_eq!(
        (status["status"].as_str(), status["statusline"].as_str()),
        (Some("installed"), Some("installed"))
    );
    assert!(
        status["reports"].to_string().contains("status line"),
        "{status}"
    );
    // Installing again keeps what the first install kept (not the tee as the "original").
    run(&["integration", "install", "claude", "--statusline"], &dir)
        .await
        .ok();
    assert_eq!(
        fs::read_to_string(hooks.join("hutan-statusline.command")).unwrap(),
        "tr a-z A-Z; exit 3"
    );

    // The script passes the user's output and exit status through, outside a Hutan session.
    let input = r#"{"model":{"id":"x"},"context_window":{"context_window_size":1000,"used_percentage":5,"current_usage":null}}"#;
    let ran = run_tee(&script, input, &[]).await;
    assert_eq!(
        ran.0,
        input.to_uppercase(),
        "the user's command sees the same input"
    );
    assert_eq!(ran.1, 3, "and its exit status is the status line's");

    // Inside a Hutan session it also tells `hutan agent statusline` (here a recording stand-in).
    let spy = root.path().join("spy.sh");
    let seen = root.path().join("seen");
    fs::write(
        &spy,
        format!(
            "#!/bin/sh\necho \"$@\" > '{}'\ncat >> '{}'\n",
            seen.display(),
            seen.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&spy, fs::Permissions::from_mode(0o755)).unwrap();
    let ran = run_tee(
        &script,
        input,
        &[
            ("HUTAN_ENV", "1"),
            ("HUTAN_SESSION_ID", "01K7F3B5N2Q9W8E4R6T1Y0V3KM"),
            ("HUTAN_BIN", spy.to_str().unwrap()),
        ],
    )
    .await;
    assert_eq!(ran.0, input.to_uppercase());
    let told = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(text) = fs::read_to_string(&seen)
                && text.contains("context_window")
            {
                return text;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the tee told Hutan");
    assert!(told.starts_with("agent statusline\n"), "{told}");
    assert!(told.contains(input), "{told}");

    // Uninstalling puts the user's status line back, byte for byte, and removes the tee's files.
    run(&["integration", "uninstall", "claude"], &dir)
        .await
        .ok();
    assert_eq!(fs::read_to_string(&settings).unwrap(), original);
    assert!(!script.exists());
    assert!(!hooks.join("hutan-statusline.command").exists());
    assert!(!hooks.join("hutan-statusline.original.json").exists());
}

async fn run_tee(script: &Path, input: &str, env: &[(&str, &str)]) -> (String, i32) {
    use tokio::io::AsyncWriteExt as _;
    let mut command = tokio::process::Command::new("sh");
    command
        .arg(script)
        .env_remove("HUTAN_ENV")
        .env_remove("HUTAN_SESSION_ID")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).await.unwrap();
    drop(stdin);
    let out = child.wait_with_output().await.unwrap();
    (
        String::from_utf8(out.stdout).unwrap(),
        out.status.code().unwrap(),
    )
}

#[tokio::test]
async fn without_a_status_line_of_their_own_the_tee_prints_nothing_and_the_uninstall_removes_it() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("claude");
    fs::create_dir_all(&dir).unwrap();
    let original = pretty(&json!({"model": "opus"}));
    fs::write(dir.join("settings.json"), &original).unwrap();
    run(&["integration", "install", "claude", "--statusline"], &dir)
        .await
        .ok();
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(doc["statusLine"]["type"], "command");
    let ran = run_tee(&dir.join("hooks").join("hutan-statusline.sh"), "{}", &[]).await;
    assert_eq!(ran, (String::new(), 0));
    run(&["integration", "uninstall", "claude"], &dir)
        .await
        .ok();
    assert_eq!(
        fs::read_to_string(dir.join("settings.json")).unwrap(),
        original
    );
}

#[tokio::test]
async fn a_status_line_the_user_set_after_the_install_is_theirs_and_one_that_cannot_be_chained_is_refused()
 {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("claude");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("settings.json"),
        pretty(&json!({"statusLine": {"type": "command", "command": "echo mine"}})),
    )
    .unwrap();
    run(&["integration", "install", "claude", "--statusline"], &dir)
        .await
        .ok();
    // The user replaces it by another one.
    fs::write(
        dir.join("settings.json"),
        pretty(&json!({"statusLine": {"type": "command", "command": "echo newer"}})),
    )
    .unwrap();
    run(&["integration", "uninstall", "claude"], &dir)
        .await
        .ok();
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(doc["statusLine"]["command"], "echo newer");
    assert!(!dir.join("hooks").join("hutan-statusline.sh").exists());

    // A status line that is not a command has nothing to chain to: nothing is written.
    let odd = root.path().join("odd");
    fs::create_dir_all(&odd).unwrap();
    let text = pretty(&json!({"statusLine": {"type": "text", "text": "hi"}}));
    fs::write(odd.join("settings.json"), &text).unwrap();
    let out = run(&["integration", "install", "claude", "--statusline"], &odd).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("statusLine"), "{out:?}");
    assert_eq!(fs::read_to_string(odd.join("settings.json")).unwrap(), text);
    assert!(!odd.join("hooks").exists());

    // Only Claude Code has one.
    let out = run(
        &["integration", "install", "codex", "--statusline"],
        &root.path().join("codex"),
    )
    .await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("--statusline"), "{out:?}");
}

#[tokio::test]
async fn status_says_what_each_installed_integration_reports_and_what_it_cannot() {
    let root = TempDir::new().unwrap();
    for (target, reports, missing) in [
        ("omp", "tokens, cost", "OMP's /context"),
        (
            "claude",
            "tools with their times (hooks)",
            "tokens per turn",
        ),
        (
            "codex",
            "files changed by apply_patch",
            "tool times, tokens",
        ),
    ] {
        let dir = root.path().join(target);
        let before = run(&["integration", "status", target, "--json"], &dir)
            .await
            .ok()
            .json();
        assert_eq!(
            before["reports"],
            json!([]),
            "{target}: nothing before the install"
        );
        // Without its hooks the transcript is found by folder: likely, not certain.
        if target != "omp" {
            assert!(
                before["transcript"].to_string().contains("likely"),
                "{target}: {before}"
            );
        }
        run(&["integration", "install", target], &dir).await.ok();
        let status = run(&["integration", "status", target, "--json"], &dir)
            .await
            .ok()
            .json();
        assert!(
            status["reports"].to_string().contains(reports),
            "{target}: {status}"
        );
        assert!(
            status["not_reported"].to_string().contains(missing),
            "{target}: {status}"
        );
        let text = run(&["integration", "status", target], &dir)
            .await
            .ok()
            .stdout;
        assert!(
            text.contains("reports: ") && text.contains("not reported: "),
            "{target}: {text}"
        );
        // The transcript route (INSIGHT-C): what it adds, how it is found, the consent it needs.
        // OMP reports through its extension and has none.
        if target == "omp" {
            assert_eq!(status["transcript"], json!([]));
            assert!(!text.contains("transcript: "), "{text}");
        } else {
            let route = status["transcript"].to_string();
            assert!(
                route.contains("tools with their times"),
                "{target}: {route}"
            );
            assert!(
                route.contains("certain") && !route.contains("likely"),
                "{target}: with its hooks the match is certain: {route}"
            );
            assert!(route.contains("[history]"), "{target}: {route}");
            assert!(text.contains("transcript: "), "{target}: {text}");
        }
        run(&["integration", "uninstall", target], &dir).await.ok();
        let after = run(&["integration", "status", target, "--json"], &dir)
            .await
            .ok()
            .json();
        assert_eq!(
            after["reports"],
            json!([]),
            "{target}: nothing after the uninstall"
        );
    }
}
