#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The chat lane of the UX plan (UX-CHAT-1) against an in-process daemon whose OMP is
//! `hutan-acp-fixture replay` over the recorded ACP transcripts (some of them edited here): safe
//! approval cards, the context meter, durable drafts and the jump-to-latest pill.

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_chat_view::{ApprovalState, Body};
use hutan_core::{ApprovalId, SessionId};
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_desktop::workbench::{Region, Workbench};
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
        contain: None,
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
        account: None,
        safety: None,
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
    // ... and "Always allow" is not the primary button: *allow once* is the one primary of the
    // decision (CL-CHAT); rejecting is a secondary like the standing options.
    assert_eq!(
        chat.read_with(cx, |c, cx| c.primary_options(approval, cx).unwrap()),
        ["allow_once"]
    );
    // What the standing option does is its button's tooltip, word for word what the card
    // printed under its buttons before: its key and label, then whose rule it is and for what.
    let scopes = chat.read_with(cx, |c, cx| c.scope_text(approval, cx).unwrap());
    assert_eq!(scopes.len(), 1, "{scopes:?}");
    assert_eq!(
        scopes[0],
        "1 Always allow edits: beyond this one request, OMP stops asking about what its rule for “edit: 3 files” covers. Hutan cannot show or revoke that rule."
    );
    // Under the buttons, one sentence stands for it.
    assert_eq!(
        chat.read_with(cx, |c, cx| c.always_note_text(approval, cx))
            .as_deref(),
        Some("Always sets a standing rule for “edit: 3 files” that Hutan cannot show or revoke.")
    );

    // The card says where the patch would act and lists every file of it, none clamped.
    let lines: Vec<&str> = detail.lines().collect();
    assert_eq!(lines[0], format!("in {}", cwd.display()));
    assert_eq!(
        &lines[1..],
        ["/work/src/lib.rs", "/work/src/main.rs", "/work/README.md"]
    );
    draw(cx);
    // The options are one row of buttons in the reading column.
    let tops: Vec<gpui::Pixels> = (0..3)
        .map(|n| {
            view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatOption(n)))
                .expect("an option button")
                .origin
                .y
        })
        .collect();
    // (Probes may sit a pixel apart; a second row would be a whole button lower.)
    assert!(
        tops.iter().all(|y| (*y - tops[0]).abs() < gpui::px(4.0)),
        "one row: {tops:?}"
    );

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

// ---- UX-CONTEXT-METER -----------------------------------------------------------------------

/// `plain-turn` after `edit` changed its `usage_update` entry (the agent's report of how full its
/// context is: 1 234 of 200 000 tokens); `None` removes it.
fn plain_turn_with_usage(dir: &Path, edit: impl Fn(&mut Value) -> bool) -> PathBuf {
    edited(dir, "plain-turn", |entries| {
        entries.retain_mut(|entry| {
            let update = &mut entry["msg"]["params"]["update"];
            update["sessionUpdate"] != "usage_update" || edit(update)
        });
    })
}

/// Sends one message on a fresh chat of a daemon replaying `transcript`; returns once the turn
/// ended.
fn chat_after_one_turn<'a>(
    cx: &'a mut TestAppContext,
    scripts: &Path,
    transcript: &Path,
) -> (
    TestDaemon,
    Entity<Workbench>,
    Entity<ChatView>,
    &'a mut VisualTestContext,
) {
    let daemon = daemon(scripts, transcript);
    let id = create_chat(&daemon, &scripts.canonicalize().unwrap());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, id);
    say(cx, "Reply with the single word pong and nothing else.");
    pump(cx, "the footer", |cx| {
        model.read_with(cx, |m, _| {
            m.transcript()
                .entries()
                .iter()
                .any(|e| matches!(e.body, Body::Footer(_)))
        })
    });
    (daemon, view, chat, cx)
}

#[gpui::test]
fn the_header_shows_how_full_the_context_is(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let transcript = plain_turn_with_usage(scripts.path(), |_| true);
    let (_daemon, view, chat, cx) = chat_after_one_turn(cx, scripts.path(), &transcript);
    assert_eq!(
        chat.read_with(cx, |c, cx| c.context_meter(cx)).as_deref(),
        Some("context <1 % · 1.2k / 200k")
    );
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("context")))
            .is_some_and(|b| b.size.width > gpui::px(10.0)),
        "the chip is drawn in the header"
    );
}

#[gpui::test]
fn without_a_window_size_the_chip_shows_only_what_was_used(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let transcript = plain_turn_with_usage(scripts.path(), |update| {
        update.as_object_mut().unwrap().remove("size");
        true
    });
    let (_daemon, _view, chat, cx) = chat_after_one_turn(cx, scripts.path(), &transcript);
    assert_eq!(
        chat.read_with(cx, |c, cx| c.context_meter(cx)).as_deref(),
        Some("context 1.2k")
    );
}

#[gpui::test]
fn a_chat_whose_agent_reports_no_usage_has_no_chip(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let transcript = plain_turn_with_usage(scripts.path(), |_| false);
    let (_daemon, view, chat, cx) = chat_after_one_turn(cx, scripts.path(), &transcript);
    assert_eq!(chat.read_with(cx, |c, cx| c.context_meter(cx)), None);
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("context")))
            .is_none(),
        "hidden, not zero"
    );
}

// ---- UX-DRAFTS ------------------------------------------------------------------------------

fn settings_for(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open_saving<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: &Path,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        settings_for(daemon),
        daemon.connection(),
        Some(state_path.to_path_buf()),
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn composer_text(model: &Entity<ChatModel>, cx: &VisualTestContext) -> String {
    model.read_with(cx, |m, _| m.input().text().to_owned())
}

#[gpui::test]
fn a_composer_draft_survives_closing_the_tab_and_reopening_the_window(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), &recorded("plain-turn"));
    let id = create_chat(&daemon, &scripts.path().canonicalize().unwrap());
    let state_path = daemon.home().join("desktop-state.json");
    {
        let (view, cx) = open_saving(cx, &daemon, &state_path);
        let (_chat, model) = open_chat(&view, cx, id);
        cx.simulate_input("half of a thought");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("and the other half");
        assert_eq!(
            composer_text(&model, cx),
            "half of a thought\nand the other half"
        );
        // Closing the tab drops the chat's model; the draft is kept anyway.
        cx.simulate_keystrokes("cmd-w");
        wait_until(cx, "the chat to leave the workbench", |cx| {
            view.read_with(cx, |wb, _| wb.chat_model_of(id).is_none())
        });
        view.update(cx, |wb, _| wb.flush_save());
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
        assert_eq!(
            saved["chat_drafts"][id.to_string()],
            "half of a thought\nand the other half",
            "plaintext in the state file, keyed by session"
        );
    }
    // A new window (a restarted desktop) reads the file: the draft is back in the composer.
    let (view, cx) = open_saving(cx, &daemon, &state_path);
    let (_chat, model) = open_chat(&view, cx, id);
    assert_eq!(
        composer_text(&model, cx),
        "half of a thought\nand the other half"
    );

    // Sending clears it: the file no longer holds it.
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the composer to empty", |cx| {
        composer_text(&model, cx).is_empty()
    });
    view.update(cx, |wb, _| wb.flush_save());
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert!(saved.get("chat_drafts").is_none(), "{saved}");
}

#[gpui::test]
fn the_draft_of_a_chat_that_no_longer_exists_is_dropped(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let daemon = daemon(scripts.path(), &recorded("plain-turn"));
    let id = create_chat(&daemon, &scripts.path().canonicalize().unwrap());
    let state_path = daemon.home().join("desktop-state.json");
    let ghost = SessionId::new();
    std::fs::write(
        &state_path,
        json!({"chat_drafts": {ghost.to_string(): "for nobody"}}).to_string(),
    )
    .unwrap();
    let (view, cx) = open_saving(cx, &daemon, &state_path);
    open_chat(&view, cx, id);
    view.update(cx, |wb, _| wb.flush_save());
    let saved: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert!(saved.get("chat_drafts").is_none(), "{saved}");
}

#[gpui::test]
fn a_question_that_ends_keeps_what_was_typed_and_never_a_secret(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("question-interrupt"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "ask me");
    pump(cx, "the question to be pending", |cx| {
        !model.read_with(cx, |m, _| m.transcript().pending_questions().is_empty())
    });
    let id = model.read_with(cx, |m, _| m.transcript().pending_questions()[0].id);
    pump(cx, "the card", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatQuestion(0)))
            .is_some()
    });
    let click = |cx: &mut VisualTestContext, region: Region| {
        let bounds = view
            .read_with(cx, |wb, _| wb.region_bounds(region))
            .unwrap_or_else(|| panic!("{region:?} is not drawn"));
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    };
    // Choose an option, write the note, Tab into the secret field and type the token.
    click(cx, Region::ChatQuestion(100));
    click(cx, Region::ChatQuestion(8 + 1));
    cx.simulate_input("the reviewer should know this");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("hunter2");
    assert_eq!(
        chat.read_with(cx, |c, cx| c.ended_answers(id, cx)),
        None,
        "it still waits"
    );

    // The turn is interrupted: the question ends unanswered.
    cx.simulate_keystrokes("escape");
    click(cx, Region::Chat("interrupt"));
    pump(cx, "the question to end", |cx| {
        model.read_with(cx, |m, _| {
            m.transcript().question(id).unwrap().state == hutan_chat_view::QuestionState::Expired
        })
    });
    draw(cx);
    let kept = chat
        .read_with(cx, |c, cx| c.ended_answers(id, cx))
        .expect("the typed answers stay");
    assert!(kept.contains(": Only the CLI"), "{kept}");
    assert!(kept.contains(": the reviewer should know this"), "{kept}");
    assert!(!kept.contains("hunter2"), "a secret is never kept: {kept}");
    assert_eq!(kept.lines().count(), 2, "{kept}");
    pump(cx, "the Copy button", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatQuestion(3)))
            .is_some()
    });
    click(cx, Region::ChatQuestion(3));
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(kept)
    );
}

#[gpui::test]
fn an_ended_question_nobody_typed_into_leaves_no_form_behind(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("question-interrupt"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "ask me");
    pump(cx, "the question to be pending", |cx| {
        !model.read_with(cx, |m, _| m.transcript().pending_questions().is_empty())
    });
    let id = model.read_with(cx, |m, _| m.transcript().pending_questions()[0].id);
    pump(cx, "the card", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("interrupt")))
            .is_some()
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("interrupt")))
        .unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    pump(cx, "the question to end", |cx| {
        model.read_with(cx, |m, _| {
            m.transcript().question(id).unwrap().state == hutan_chat_view::QuestionState::Expired
        })
    });
    draw(cx);
    assert_eq!(chat.read_with(cx, |c, cx| c.ended_answers(id, cx)), None);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatQuestion(3)))
            .is_none(),
        "no Copy without answers"
    );
}

// ---- UX-JUMP-LATEST (chat half) -------------------------------------------------------------

fn send_on_daemon(daemon: &TestDaemon, id: SessionId, text: &str) {
    let _: hutan_protocol::chat::ChatSent =
        daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: id,
            text: text.to_owned(),
            attachments: vec![],
        }));
}

/// Scrolls the chat's list by `dy` pixels (positive reads further up) over an entry that is
/// drawn.
fn scroll(view: &Entity<Workbench>, cx: &mut VisualTestContext, dy: f32) {
    let bounds = (0..64)
        .find_map(|at| view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatEntry(at))))
        .expect("an entry is drawn");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: bounds.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(dy))),
        modifiers: gpui::Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
    draw(cx);
}

#[gpui::test]
fn output_below_a_reader_who_scrolled_up_is_announced_and_one_click_returns(
    cx: &mut TestAppContext,
) {
    const BEFORE: usize = 12;
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(crate::chat_gpui::many_turns(&env, BEFORE + 2));
    let info = daemon.create_chat(&env.cwd());
    for turn in 1..=BEFORE {
        send_on_daemon(&daemon, info.id, &format!("message {turn}"));
        std::thread::sleep(Duration::from_millis(5));
        crate::chat_gpui::wait_idle(&daemon, info.id);
    }
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    draw(cx);
    assert!(chat.read_with(cx, |c, _| c.following_tail()));
    assert_eq!(chat.read_with(cx, |c, cx| c.new_below(cx)), None);

    // The reader scrolls up; the next turn's output arrives below.
    scroll(&view, cx, 100_000.0);
    assert!(!chat.read_with(cx, |c, _| c.following_tail()));
    assert_eq!(
        chat.read_with(cx, |c, cx| c.new_below(cx)),
        None,
        "nothing new yet"
    );
    let anchor = chat.read_with(cx, |c, cx| c.scroll_anchor(cx));
    send_on_daemon(&daemon, info.id, &format!("message {}", BEFORE + 1));
    pump(cx, "the new turn", |cx| {
        model.read_with(cx, |m, _| {
            m.transcript()
                .entries()
                .iter()
                .filter(|e| matches!(e.body, Body::Footer(_)))
                .count()
                > 0
                && m.transcript().entries().len() > 3 * BEFORE
        })
    });
    draw(cx);
    // The place was kept, and the pill says what arrived.
    assert_eq!(chat.read_with(cx, |c, cx| c.scroll_anchor(cx)), anchor);
    let pill = chat.read_with(cx, |c, cx| c.new_below(cx)).expect("a pill");
    assert!(
        pill.ends_with(" new ↓") && !pill.starts_with("0 "),
        "{pill}"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("jump-latest")))
            .is_some(),
        "the pill is drawn"
    );

    // Reading back down to the end makes the pill go and the list follows again.
    scroll(&view, cx, -1_000_000.0);
    scroll(&view, cx, -1_000_000.0);
    assert_eq!(chat.read_with(cx, |c, cx| c.new_below(cx)), None);
    assert!(chat.read_with(cx, |c, _| c.following_tail()));
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("jump-latest")))
            .is_none()
    );

    // Up again, another turn: the click on the pill is the way back.
    scroll(&view, cx, 100_000.0);
    pump(cx, "the pill to be quiet", |cx| {
        chat.read_with(cx, |c, cx| c.new_below(cx)).is_none()
    });
    send_on_daemon(&daemon, info.id, &format!("message {}", BEFORE + 2));
    pump(cx, "the pill", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("jump-latest")))
            .is_some()
    });
    assert!(
        !chat.read_with(cx, |c, _| c.following_tail()),
        "streaming does not drag the reader"
    );
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Chat("jump-latest")))
        .unwrap();
    cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    draw(cx);
    draw(cx);
    assert!(chat.read_with(cx, |c, _| c.following_tail()));
    assert_eq!(chat.read_with(cx, |c, cx| c.new_below(cx)), None);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("jump-latest")))
            .is_none(),
        "the pill is gone at the bottom"
    );
}
