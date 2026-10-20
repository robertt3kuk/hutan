#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The chat lane of the UX plan (UX-CHAT-1) against an in-process daemon whose OMP is
//! `hutan-acp-fixture replay` over the recorded ACP transcripts (some of them edited here): safe
//! approval cards, the context meter, durable drafts and the jump-to-latest pill.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_chat_view::{ApprovalState, Body};
use hutan_core::{ApprovalId, SessionId};
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{Request, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

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

fn recorded(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

/// The recorded transcript `name` after `edit` changed its entries (`{"dir", "msg"}` objects),
/// written to `dir`.
fn edited(dir: &Path, name: &str, edit: impl FnOnce(&mut Vec<Value>)) -> PathBuf {
    let text = std::fs::read_to_string(recorded(name)).unwrap();
    let mut entries: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    edit(&mut entries);
    let path = dir.join(format!("{name}-edited.jsonl"));
    let out: Vec<String> = entries.iter().map(Value::to_string).collect();
    std::fs::write(&path, out.join("\n") + "\n").unwrap();
    path
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A daemon whose OMP replays `transcript`; no real harness, login or model is touched.
fn daemon(scripts: &Path, transcript: &Path) -> TestDaemon {
    let omp = script(
        scripts,
        "omp",
        r#"case "$1" in
  models) echo '{"models":[]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n",
        acp = acp_bin(),
        t = transcript,
    ))
}

fn create_chat(daemon: &TestDaemon, cwd: &Path) -> SessionId {
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        workspace_id: None,
        cwd: cwd.to_path_buf(),
        agent: "omp".into(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: None,
        mode: None,
        provider_id: None,
        system_prompt: None,
    }));
    created.session.id
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn open_chat(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> (Entity<ChatView>, Entity<ChatModel>) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(session).is_some()
        })
    });
    let chat = view
        .read_with(cx, |wb, _| wb.chat_view_of(session))
        .unwrap();
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(session))
        .unwrap();
    (chat, model)
}

fn pump(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        condition(cx)
    });
}

/// Types `text` into the focused composer and sends it with ⌘↩.
fn say(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.simulate_keystrokes("cmd-enter");
}

fn pending(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<ApprovalId> {
    model.read_with(cx, |m, _| {
        m.transcript()
            .pending_approvals()
            .iter()
            .map(|a| a.id)
            .collect()
    })
}

// ---- UX-APPROVAL-CARD -----------------------------------------------------------------------

/// `permission-allow` where the agent offers `[allow_always, allow_once, reject_once]` in that
/// order for a patch that touches three files, one of them named only by a diff.
fn three_file_patch(dir: &Path) -> PathBuf {
    edited(dir, "permission-allow", |entries| {
        for entry in entries {
            if entry["msg"]["method"] == "session/request_permission" {
                entry["msg"]["params"]["toolCall"]["title"] = json!("edit: 3 files");
                entry["msg"]["params"]["toolCall"]["locations"] = json!([
                    {"path": "/work/src/lib.rs"},
                    {"path": "/work/src/main.rs"},
                ]);
                entry["msg"]["params"]["toolCall"]["content"] = json!([
                    {"type": "diff", "path": "/work/src/lib.rs", "oldText": "a", "newText": "b"},
                    {"type": "diff", "path": "/work/README.md", "oldText": "a", "newText": "b"},
                ]);
                entry["msg"]["params"]["options"] = json!([
                    {"optionId": "allow_always", "name": "Always allow edits", "kind": "allow_always"},
                    {"optionId": "allow_once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject_once", "name": "Reject", "kind": "reject_once"},
                ]);
            }
        }
    })
}

#[gpui::test]
fn an_approval_card_does_not_make_the_harnesss_first_option_the_easy_click(
    cx: &mut TestAppContext,
) {
    let scripts = tempfile::tempdir().unwrap();
    let cwd = scripts.path().canonicalize().unwrap();
    let daemon = daemon(scripts.path(), &three_file_patch(scripts.path()));
    let id = create_chat(&daemon, &cwd);
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, id);
    say(cx, "edit the three files");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let approval = pending(&model, cx).remove(0);

    // The agent's order is kept (the digits 1–4 follow it) ...
    let (options, detail) = model.read_with(cx, |m, _| {
        let a = m.transcript().approval(approval).unwrap();
        (
            a.options.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
            a.detail.clone().unwrap(),
        )
    });
    assert_eq!(options, ["allow_always", "allow_once", "reject_once"]);
    // ... but "Always allow" is not the primary button: the choices about this one request are.
    assert_eq!(
        chat.read_with(cx, |c, cx| c.primary_options(approval, cx).unwrap()),
        ["allow_once", "reject_once"]
    );
    // The standing option says what it does beyond this request and whose rule it is.
    let scopes = chat.read_with(cx, |c, cx| c.scope_text(approval, cx).unwrap());
    assert_eq!(scopes.len(), 1, "{scopes:?}");
    assert!(
        scopes[0].starts_with("1 Always allow edits: beyond this one request, OMP stops asking about what its rule for “edit: 3 files” covers."),
        "{scopes:?}"
    );
    assert!(scopes[0].contains("Hutan cannot show or revoke that rule"));

    // The card says where the patch would act and lists every file of it, none clamped.
    let lines: Vec<&str> = detail.lines().collect();
    assert_eq!(lines[0], format!("in {}", cwd.display()));
    assert_eq!(
        &lines[1..],
        ["/work/src/lib.rs", "/work/src/main.rs", "/work/README.md"]
    );
    draw(cx);

    // Return approves nothing: it is a line break in the composer (the send key is ⌘↩).
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "\n"
    );
    cx.simulate_keystrokes("backspace");
    assert_eq!(pending(&model, cx), [approval], "Return did not answer");
    assert_eq!(
        model.read_with(cx, |m, _| m
            .transcript()
            .approval(approval)
            .unwrap()
            .state
            .clone()),
        ApprovalState::Pending
    );

    // Choosing is explicit: option 2 (Allow once) is the key that approves.
    cx.simulate_keystrokes("2");
    pump(cx, "the answer to be applied", |cx| {
        pending(&model, cx).is_empty()
    });
    let chosen = model.read_with(cx, |m, _| {
        matches!(
            &m.transcript().approval(approval).unwrap().state,
            ApprovalState::Selected { option_id } if option_id == "allow_once"
        )
    });
    assert!(chosen);
    // The resolved card keeps what was decided.
    assert!(model.read_with(cx, |m, _| {
        m.transcript()
            .entries()
            .iter()
            .any(|e| matches!(e.body, Body::Tool(_) | Body::Approval(_)))
    }));
}
