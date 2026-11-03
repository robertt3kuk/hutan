#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! FX-AGENT-FILES: an OMP `edit` over three files — two of them `Cargo.toml`s of different
//! folders, and one whose old and new text OMP did not send (it keeps the snapshots of the first
//! files of a call only) — is listed whole by *Files touched* (Context panel) and *Changed by
//! agents* (Files panel): three distinct rows, the two `Cargo.toml` rows telling their folders.
//! A real daemon whose `omp` replays an ACP transcript held until the test has changed the files
//! in a git checkout, as the agent would; the turn's checkpoint gives the file without text its
//! base.

use crate::common;

use std::path::{Path, PathBuf};

use common::{TestDaemon, draw, open_with, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::context_model as model;
use hutan_desktop::workbench::panels::{ContextPanel, FilesPanel};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::messages::{Request, SessionInfo, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;

const ROOT_OLD: &str = "[workspace]\nmembers = [\"crates/*\"]\n";
const ROOT_NEW: &str = "[workspace]\nmembers = [\"crates/*\"]\npretty_assertions = \"1\"\n";
const CORE_OLD: &str = "[package]\nname = \"core\"\n\n[dev-dependencies]\n";
const CORE_NEW: &str =
    "[package]\nname = \"core\"\n\n[dev-dependencies]\npretty_assertions.workspace = true\n";

fn safety(line_six: &str) -> String {
    (1..=12)
        .map(|n| {
            if n == 6 {
                format!("{line_six}\n")
            } else {
                format!("line {n}\n")
            }
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A checkout with the three files as they are before the turn.
fn repository(base: &Path) -> PathBuf {
    git(base, &["init", "-q", "-b", "main", "repo"]);
    let root = base.join("repo");
    std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), ROOT_OLD).unwrap();
    std::fs::write(root.join("crates/core/Cargo.toml"), CORE_OLD).unwrap();
    std::fs::write(root.join("crates/core/src/safety.rs"), safety("line 6")).unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

/// What the agent does: the files after the turn.
fn change_files(root: &Path) {
    std::fs::write(root.join("Cargo.toml"), ROOT_NEW).unwrap();
    std::fs::write(root.join("crates/core/Cargo.toml"), CORE_NEW).unwrap();
    std::fs::write(root.join("crates/core/src/safety.rs"), safety("line six")).unwrap();
}

/// `tool-diff.jsonl`'s handshake, then a prompt held until `gate` exists, then one `edit` call
/// of OMP's shape: a `diff` block for the two manifests, the three paths in `locations` and in the
/// result's `perFileResults`, the third without text (`snapshotsPruned`).
fn transcript(dir: &Path, root: &Path, gate: &Path) -> PathBuf {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11");
    let base = std::fs::read_to_string(fixtures.join("tool-diff.jsonl")).unwrap();
    let mut out: Vec<String> = base.lines().take(5).map(str::to_owned).collect();
    let at = |rel: &str| root.join(rel).display().to_string();
    let update = |update: serde_json::Value| {
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "sess_fixture_1", "update": update}}})
        .to_string()
    };
    out.push(json!({"dir": "hold", "file": gate}).to_string());
    out.push(update(json!({
        "sessionUpdate": "tool_call", "toolCallId": "call_edit_1",
        "title": "Adding a dev-dependency", "kind": "edit", "status": "pending",
        "rawInput": {"input": "[Cargo.toml#5850]\nPUT >2:\n+pretty_assertions = \"1\"\n"},
    })));
    out.push(update(json!({
        "sessionUpdate": "tool_call_update", "toolCallId": "call_edit_1", "status": "completed",
        "content": [
            {"type": "diff", "path": at("Cargo.toml"), "oldText": ROOT_OLD, "newText": ROOT_NEW},
            {"type": "diff", "path": at("crates/core/Cargo.toml"), "oldText": CORE_OLD, "newText": CORE_NEW},
            {"type": "content", "content": {"type": "text", "text": "edited 3 files"}},
        ],
        "locations": [
            {"path": at("Cargo.toml")},
            {"path": at("crates/core/Cargo.toml")},
            {"path": at("crates/core/src/safety.rs")},
        ],
        "rawOutput": {"details": {"perFileResults": [
            {"path": at("Cargo.toml"), "op": "update", "oldText": ROOT_OLD, "newText": ROOT_NEW},
            {"path": at("crates/core/Cargo.toml"), "op": "update", "oldText": CORE_OLD, "newText": CORE_NEW},
            {"path": at("crates/core/src/safety.rs"), "op": "update", "snapshotsPruned": true},
        ]}},
    })));
    out.push(update(json!({
        "sessionUpdate": "agent_message_chunk", "messageId": "msg-2",
        "content": {"type": "text", "text": "Done."},
    })));
    out.push(
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"stopReason": "end_turn"}}})
            .to_string(),
    );
    let path = dir.join("edit-three-files.jsonl");
    std::fs::write(&path, out.join("\n") + "\n").unwrap();
    path
}

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn daemon(transcript: &Path) -> TestDaemon {
    TestDaemon::start_tweaked(
        &format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", {t:?}, \"acp\"]\n",
            bin = acp_bin(),
            t = transcript,
        ),
        |c| {
            c.checkpoints.git_env = [
                ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ]
            .into();
        },
    )
}

fn create_chat(daemon: &TestDaemon, cwd: &Path) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: None,
        cwd: cwd.to_path_buf(),
        agent: "omp".to_owned(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: Some("Dev dependency".to_owned()),
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    created.session
}

fn open_files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

#[gpui::test]
fn an_edit_naming_three_files_lists_three_rows_in_both_panels(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let base = work.path().canonicalize().unwrap();
    let root = repository(&base);
    let gate = base.join("gate");
    let script = transcript(&base, &root, &gate);
    let daemon = daemon(&script);
    let chat = create_chat(&daemon, &root);

    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
    )
    .unwrap();
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        Some(state),
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(root.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(root.as_path())
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    wait_until(cx, "the chat to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.shown_session()) == Some(chat.id)
    });
    settle(cx);

    // The turn runs while the files change; the replay then says what the agent did.
    let _: ChatSent = daemon.call(Request::ChatSend(ChatSend {
        session_id: chat.id,
        text: "Add pretty_assertions as a dev-dependency.".to_owned(),
        attachments: Vec::new(),
    }));
    change_files(&root);
    std::fs::write(&gate, "").unwrap();

    // Changed by agents: three files, whatever the agent sent of each.
    let files = open_files_panel(&view, cx);
    wait_until(cx, "the three files in Changed by agents", |cx| {
        files.read_with(cx, |p, cx| p.changed_by_agents(cx).len() == 3)
    });
    let listed = files.read_with(cx, |p, cx| p.changed_by_agents(cx));
    let folders = files.read_with(cx, |p, cx| p.changed_by_agents_folders(cx));
    let mut rows: Vec<(String, Option<String>)> = listed
        .iter()
        .zip(&folders)
        .map(|(file, folder)| {
            (
                file.path.strip_prefix(&root).unwrap().display().to_string(),
                folder.clone(),
            )
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            // The two manifests say where they are; the root's file says the root's name.
            ("Cargo.toml".to_owned(), Some("repo".to_owned())),
            (
                "crates/core/Cargo.toml".to_owned(),
                Some("crates/core".to_owned())
            ),
            // A name nothing else has needs no folder.
            ("crates/core/src/safety.rs".to_owned(), None),
        ]
    );
    assert!(listed.iter().all(|file| file.who == "OMP"), "{listed:?}");
    assert!(listed.iter().all(|file| file.hunks == 1), "{listed:?}");
    draw(cx);
    for index in 0..3 {
        assert!(
            view.read_with(cx, |wb, _| {
                wb.region_bounds(Region::FilesSection("changed-by-agents", index))
            })
            .is_some(),
            "row {index} is drawn"
        );
    }

    // Files touched: the same three by path.
    cx.dispatch_action(actions::OpenPanel("context".into()));
    wait_until(cx, "the Context panel", |cx| {
        view.read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
            .is_some_and(|panel| panel.read_with(cx, |p, _| p.is_visible()))
    });
    let context = view
        .read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
        .unwrap();
    wait_until(cx, "three files touched", |cx| {
        context.read_with(cx, |p, _| {
            p.insight()
                .and_then(|i| i.files.as_ref())
                .is_some_and(|files| files.count == 3)
        })
    });
    let lines = context.read_with(cx, |p, _| model::file_lines(p.insight().unwrap()));
    let paths: Vec<&str> = lines.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(
        paths.iter().filter(|p| p.ends_with("Cargo.toml")).count(),
        2,
        "{paths:?}"
    );
    assert_eq!(paths.len(), 3, "{paths:?}");
    assert!(
        paths
            .iter()
            .any(|p| p.ends_with("crates/core/src/safety.rs"))
    );
    let mut distinct = paths.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "three distinct rows");
    // The header's line counts say which files they are of: where the agent sent no text for a
    // file, its lines are not in them (the turn footer counts every changed file from the
    // checkpoint), and the panel says so instead of passing the sum off as the whole.
    let summary = context
        .read_with(cx, |p, _| model::files_summary(p.insight().unwrap()))
        .unwrap();
    let uncounted = lines.iter().filter(|l| l.stat.is_none()).count();
    assert_eq!(
        uncounted,
        context.read_with(cx, |p, _| model::files_without_counts(p.insight().unwrap()))
    );
    if uncounted > 0 && uncounted < lines.len() {
        assert!(
            summary.ends_with(&format!(
                " in {} of {}",
                lines.len() - uncounted,
                lines.len()
            )),
            "{summary}"
        );
    } else {
        assert!(!summary.contains(" of "), "{summary}");
    }
    settle(cx);
    for index in 0..3 {
        assert!(
            view.read_with(cx, |wb, _| wb
                .region_bounds(Region::ContextFile(index, "open")))
                .is_some(),
            "file row {index} is drawn"
        );
    }
    let _ = &daemon;
}
