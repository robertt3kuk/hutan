#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The chat queue in the desktop (UX-QUEUE-STEER, protocol 0.36): while a turn runs the send key
//! queues the draft as a chip above the composer, a chip opens in the composer to be edited and
//! saved with the send key, ✕ removes it, and Stop with messages queued asks what becomes of
//! them. The daemon runs in this process; Codex is `hutan-codex-fixture replay`.

use crate::common;

use std::time::Duration;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::chat::{ChatModel, LoadStatus};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

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

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    pump(cx, &format!("{what:?} to be drawn"), |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(what)).is_some()
    });
    let bounds = view.read_with(cx, |wb, _| wb.region_bounds(what)).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn queued(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<String> {
    model.read_with(cx, |m, _| {
        m.queued().iter().map(|q| q.text.clone()).collect()
    })
}

#[gpui::test]
fn the_send_key_queues_during_a_turn_chips_edit_and_stop_clears_them(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("interrupt"));
    let info = daemon.create_chat(&env.cwd());
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
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(info.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(info.id))
        .unwrap();
    cx.simulate_input("count");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the turn to run", |cx| {
        model.read_with(cx, |m, _| m.transcript().turn_active())
    });

    // The send key queues while the turn runs: two chips, nothing more sent.
    cx.simulate_input("next");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the first chip", |cx| queued(&model, cx) == ["next"]);
    assert!(model.read_with(cx, |m, _| m.input().is_empty()));
    cx.simulate_input("second");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the second chip", |cx| {
        queued(&model, cx) == ["next", "second"]
    });
    assert_eq!(env.sent_texts().len(), 1, "queued, not sent");
    pump(cx, "the chips to be drawn", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChatQueue("chip", 1)).is_some()
        })
    });

    // Edit opens the chip in the composer; the send key saves it.
    cx.simulate_input("draft");
    click(&view, cx, Region::ChatQueue("edit", 0));
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "next"
    );
    assert!(model.read_with(cx, |m, _| m.editing().is_some()));
    cx.simulate_input(", edited");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the edit to be saved", |cx| {
        queued(&model, cx) == ["next, edited", "second"]
    });
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "draft",
        "the draft comes back"
    );
    // ✕ removes a chip.
    click(&view, cx, Region::ChatQueue("remove", 1));
    pump(cx, "the chip to go", |cx| {
        queued(&model, cx) == ["next, edited"]
    });

    // Stop with a message queued asks; Stop and clear empties the queue.
    click(&view, cx, Region::Chat("interrupt"));
    let entries = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    assert!(
        entries[0].0.starts_with("Stop and clear the queue"),
        "{entries:?}"
    );
    assert_eq!(entries[1].0.as_ref(), "Stop, keep the queue");
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    assert!(model.read_with(cx, |m, _| m.stopping()), "Stopping…");
    pump(cx, "the turn to end and the queue to clear", |cx| {
        model.read_with(cx, |m, _| {
            !m.transcript().turn_active() && m.queued().is_empty() && !m.stopping()
        })
    });
    assert_eq!(
        env.sent_texts().len(),
        1,
        "nothing queued started after Stop"
    );
}

// ---- Send now on an agent that takes no message during a turn (FX-POLISH-1) -------------------

/// A daemon whose OMP replays `cancel` (a turn that runs until it is cancelled), then one more
/// turn that ends by itself. `dir` holds the transcript.
fn omp_cancellable(dir: &std::path::Path) -> TestDaemon {
    use std::fmt::Write as _;
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11/cancel.jsonl");
    let mut text = std::fs::read_to_string(fixtures).unwrap();
    let done = [
        serde_json::json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "session/prompt", "id": 1}}),
        serde_json::json!({"dir": "in", "msg": {"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "sess_fixture_1",
            "update": {"sessionUpdate": "agent_message_chunk", "messageId": "msg-done",
                "content": {"type": "text", "text": "Done."}}}}}),
        serde_json::json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"stopReason": "end_turn"}}}),
    ];
    for entry in done {
        writeln!(text, "{entry}").unwrap();
    }
    let transcript = dir.join("cancellable.jsonl");
    std::fs::write(&transcript, text).unwrap();
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", {transcript:?}, \"acp\"]\n"
    ))
}

fn omp_chat(daemon: &TestDaemon, cwd: &std::path::Path) -> hutan_core::SessionId {
    let created: hutan_protocol::messages::SessionResult = daemon.call(
        hutan_protocol::messages::Request::ChatCreate(hutan_protocol::chat::ChatCreate {
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
        }),
    );
    created.session.id
}

fn open_omp_chat<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    session: hutan_core::SessionId,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    Entity<hutan_desktop::workbench::chat::ChatView>,
    Entity<ChatModel>,
) {
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
    (view, cx, chat, model)
}

#[gpui::test]
fn send_now_says_interrupt_and_send_only_while_a_turn_runs_that_it_interrupts(
    cx: &mut TestAppContext,
) {
    let work = tempfile::tempdir().unwrap();
    let daemon = omp_cancellable(work.path());
    let id = omp_chat(&daemon, work.path());
    let (view, cx, chat, model) = open_omp_chat(cx, &daemon, id);
    cx.simulate_input("long task");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the turn to run", |cx| {
        model.read_with(cx, |m, _| m.transcript().turn_active())
    });
    cx.simulate_input("later");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the chip", |cx| queued(&model, cx) == ["later"]);
    pump(cx, "the queue to say OMP does not steer", |cx| {
        model.read_with(cx, |m, _| m.queue().is_some_and(|q| !q.steer))
    });
    // A turn runs that Send now has to interrupt: the chip and the composer say so.
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.send_now_label(cx)),
        "Interrupt & send"
    );

    // Stop, keep the queue: no turn runs and the queue holds; Send now just sends.
    click(&view, cx, Region::Chat("interrupt"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    pump(cx, "the queue to hold", |cx| {
        model.read_with(cx, |m, _| {
            !m.transcript().turn_active() && m.queue().is_some_and(|q| q.held.is_some())
        })
    });
    assert_eq!(
        chat.read_with(cx, |chat, cx| chat.send_now_label(cx)),
        "Send now"
    );
    click(&view, cx, Region::ChatQueue("now", 0));
    pump(cx, "the held message to go", |cx| {
        model.read_with(cx, |m, _| m.queued().is_empty())
    });
}

#[gpui::test]
fn ctrl_enter_that_interrupts_the_turn_shows_stopping_like_stop_does(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let daemon = omp_cancellable(work.path());
    let id = omp_chat(&daemon, work.path());
    let (_view, cx, _chat, model) = open_omp_chat(cx, &daemon, id);
    cx.simulate_input("long task");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the turn to run", |cx| {
        model.read_with(cx, |m, _| m.transcript().turn_active())
    });
    pump(cx, "the queue to say OMP does not steer", |cx| {
        model.read_with(cx, |m, _| m.queue().is_some_and(|q| !q.steer))
    });
    cx.simulate_input("stop and do this");
    cx.simulate_keystrokes("ctrl-enter");
    assert!(
        model.read_with(cx, |m, _| m.stopping()),
        "Stopping… as with Stop"
    );
    pump(cx, "the turn to end and the message to go", |cx| {
        model.read_with(cx, |m, _| !m.stopping() && !m.transcript().turn_active())
    });
}

// ---- a chip that leaves folds away (UX-MOTION-2) -----------------------------------------------

fn strip_height(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> f32 {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Chat("queue-strip")))
        .map(|bounds| f32::from(bounds.size.height))
        .expect("the strip is drawn")
}

/// Two chips queued behind a running turn, then ✕ on the second: the heights of the strip
/// before, just after and well after the chip left.
fn remove_the_second_chip(cx: &mut TestAppContext, reduce_motion: &str) -> (f32, f32, f32, f32) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("interrupt"));
    let info = daemon.create_chat(&env.cwd());
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    settings
        .write_user(
            "appearance.reduce_motion",
            toml::Value::String(reduce_motion.into()),
        )
        .unwrap();
    hutan_desktop::workbench::ui::motion::note_input(false);
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(info.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(info.id))
        .unwrap();
    cx.simulate_input("count");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the turn to run", |cx| {
        model.read_with(cx, |m, _| m.transcript().turn_active())
    });
    cx.simulate_input("next");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the first chip", |cx| queued(&model, cx) == ["next"]);
    cx.simulate_input("second");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the second chip", |cx| {
        queued(&model, cx) == ["next", "second"]
    });
    pump(cx, "the chips to be drawn", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::ChatQueue("chip", 1)).is_some()
        })
    });
    // Let the chips' own entrance end.
    cx.executor().advance_clock(Duration::from_millis(300));
    strip_height(&view, cx);
    let full = strip_height(&view, cx);

    // The pointer removes the second chip (the press is a pointer input).
    click(&view, cx, Region::ChatQueue("remove", 1));
    pump(cx, "the chip to go", |cx| queued(&model, cx) == ["next"]);
    draw(cx);
    let just_after = strip_height(&view, cx);
    cx.executor().advance_clock(Duration::from_millis(15));
    let later = strip_height(&view, cx);
    cx.executor().advance_clock(Duration::from_millis(400));
    strip_height(&view, cx);
    let settled = strip_height(&view, cx);
    cx.executor().advance_clock(Duration::from_millis(400));
    assert_eq!(
        strip_height(&view, cx),
        settled,
        "the strip does not jump after the exit"
    );
    (full, just_after, later, settled)
}

#[gpui::test]
fn a_chip_that_is_removed_folds_away_and_the_strip_does_not_jump_after(cx: &mut TestAppContext) {
    let (full, just_after, later, settled) = remove_the_second_chip(cx, "off");
    assert!(
        settled < full - 10.0,
        "one chip row is gone: {full} -> {settled}"
    );
    assert!(
        just_after <= full,
        "it did not grow: {just_after} vs {full}"
    );
    assert!(
        later < full,
        "the strip was shrinking on the way: {full} / {just_after} / {later} / {settled}"
    );
    assert!(
        later > settled,
        "and was not yet at its end 15 ms in: {later} vs {settled}"
    );
}

#[gpui::test]
fn under_reduce_motion_a_removed_chip_does_not_fold(cx: &mut TestAppContext) {
    let (full, just_after, later, settled) = remove_the_second_chip(cx, "on");
    assert_eq!(just_after, full, "no height animation");
    assert_eq!(later, full, "it only fades, in place, for its fade");
    assert!(
        settled < full - 10.0,
        "then it is gone: {full} -> {settled}"
    );
}
