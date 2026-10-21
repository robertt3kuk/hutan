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
