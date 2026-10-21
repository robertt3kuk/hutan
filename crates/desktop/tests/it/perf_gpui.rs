#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The deterministic half of the performance budgets (`docs/perf.md`): what a frame costs is
//! counted in rows built and markdown parsed, not in milliseconds. The timing half is `just
//! bench`. Each test fails when a surface goes back to doing work for everything it holds
//! instead of what is in view: the sidebar's rows, the transcript's cards, the layout of the
//! sidebar's entries, the markdown of a message, and the timers of hidden panels.

use crate::common;

use std::time::Duration;

use common::chat::{ChatEnv, method, says};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_desktop::workbench::panels::SessionsPanel;
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;

/// Sessions in the sidebar test: far more than fit in the panel.
const SESSIONS: usize = 150;
/// Messages in the transcript test.
const MESSAGES: usize = 300;
/// Most rows or cards a frame may build: what fits the window with room to spare.
const IN_VIEW: usize = 40;

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    // The first-run setup would cover the window.
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
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

fn sessions_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<SessionsPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock")
}

// ---- the sidebar ------------------------------------------------------------------------------

fn crowded_sidebar(
    cx: &mut TestAppContext,
) -> (TestDaemon, Entity<Workbench>, &mut VisualTestContext) {
    let daemon = TestDaemon::start();
    for _ in 0..SESSIONS {
        daemon.create_session(&["sh", "-c", "exit 0"], None);
    }
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "every session to be known", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().len() >= SESSIONS
        })
    });
    // The sessions end at once; their exits are real daemon events and would otherwise land
    // between two samples of a render count. So does the answer to the window's first
    // `task.list` (the sidebar lists tasks too), which a loaded daemon gives late.
    wait_until(
        cx,
        "every session to have exited and the tasks to be listed",
        |cx| {
            view.read_with(cx, |wb, cx| {
                let store = wb.sessions().read(cx);
                store.tasks().loaded()
                    && store
                        .sessions()
                        .iter()
                        .all(|s| matches!(s.state, hutan_core::SessionState::Exited { .. }))
            })
        },
    );
    (daemon, view, cx)
}

#[gpui::test]
fn the_sidebar_builds_only_the_rows_in_view(cx: &mut TestAppContext) {
    let (_daemon, view, cx) = crowded_sidebar(cx);
    let panel = sessions_panel(&view, cx);
    for _ in 0..3 {
        draw(cx);
    }
    let listed = panel.read_with(cx, |p, cx| p.visible_titles(cx).len());
    assert!(
        listed >= SESSIONS,
        "the panel lists every session ({listed}); only the drawing is limited"
    );
    let before = panel.read_with(cx, |p, _| p.rows_rendered());
    draw(cx);
    let one_frame = panel.read_with(cx, |p, _| p.rows_rendered()) - before;
    assert!(
        one_frame <= IN_VIEW,
        "a frame built {one_frame} sidebar rows for {listed} listed: only the rows in view may be built"
    );
}

#[gpui::test]
fn a_frame_does_not_lay_the_sidebar_out_again(cx: &mut TestAppContext) {
    let (_daemon, view, cx) = crowded_sidebar(cx);
    let panel = sessions_panel(&view, cx);
    // Let the daemon's events (session states, placements) settle.
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(cx);
    }
    let builds = panel.read_with(cx, |p, _| p.layout_builds());
    for _ in 0..10 {
        draw(cx);
    }
    assert_eq!(
        panel.read_with(cx, |p, _| p.layout_builds()),
        builds,
        "nothing changed, so the entries (a clone of every session) must not be built again"
    );

    // A key in the filter changes what is listed: one layout, whatever the number of frames.
    panel.update(cx, |p, cx| p.set_query("exit", cx));
    for _ in 0..5 {
        draw(cx);
    }
    assert_eq!(
        panel.read_with(cx, |p, _| p.layout_builds()),
        builds + 1,
        "one layout for one change of the filter"
    );
}

/// A notification of the sessions store about something a panel does not show (an answer given
/// here, the focus, a remembered choice) must not render the sessions panel or History; one
/// about what they list must.
#[gpui::test]
fn a_store_notification_that_changes_nothing_they_show_renders_no_panel(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::panels::HistoryPanel;
    let (daemon, view, cx) = crowded_sidebar(cx);
    let sessions = sessions_panel(&view, cx);
    let history = view.read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"));
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(cx);
    }
    let store = view.read_with(cx, |wb, _| wb.sessions().clone());
    let renders = |cx: &mut VisualTestContext| {
        (
            sessions.read_with(cx, |p, _| p.renders()),
            history
                .as_ref()
                .map(|h| h.read_with(cx, |p, _| p.renders())),
        )
    };
    let before = renders(cx);

    // Not about what they list: a remembered choice, and an answer nobody waits for.
    store.update(cx, |store, cx| {
        store.note_assistant_pinned(None, true, cx);
        store.set_assistant_file_choice(true, cx);
    });
    for _ in 0..5 {
        draw(cx);
    }
    assert_eq!(renders(cx), before, "nothing they show changed");

    // About what they list: a new session.
    daemon.sh("sleep 30");
    wait_until(cx, "the new session in the sidebar", |cx| {
        sessions.read_with(cx, |p, _| p.renders()) > before.0
    });
    let after = renders(cx);
    assert!(after.0 > before.0);
    if let (Some(before), Some(after)) = (before.1, after.1) {
        // History follows the set of sessions (which it uses to tell what still exists).
        assert!(after >= before);
    }
}

/// A terminal in a cached pane is painted again when output arrives while it is visible (the
/// paint is also what acks the frame, so a pane that stayed on its replayed frame would stall
/// the daemon's stream after one frame).
#[gpui::test]
fn output_arriving_in_a_visible_cached_terminal_repaints_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("printf 'first\\n'; read go; i=0; while [ $i -lt 400 ]; do echo line-$i; i=$((i+1)); done; sleep 30");
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    let screen = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id).map(|v| {
                (
                    v.read(cx).paints(),
                    common::screen_text(&v.read(cx).model()),
                )
            })
        })
    };
    wait_until(cx, "the first line", |cx| {
        screen(cx).is_some_and(|(_, text)| text.contains("first"))
    });
    // Idle frames: nothing notifies the terminal, nothing may repaint it needlessly.
    for _ in 0..5 {
        draw(cx);
    }
    let (before, _) = screen(cx).expect("the terminal view exists");
    assert!(before > 0, "painted at least once");
    // More output, a frame at a time: each arrives while the pane is visible.
    daemon.send_input(info.terminal_id.expect("a terminal session"), b"go\r");
    wait_until(cx, "the output on the screen", |cx| {
        screen(cx).is_some_and(|(_, text)| text.contains("line-399"))
    });
    let (after, _) = screen(cx).expect("the terminal view exists");
    assert!(
        after > before,
        "the cached terminal repainted ({before} → {after})"
    );
}

// ---- the transcript ---------------------------------------------------------------------------

/// `plain-turn` with `MESSAGES` assistant messages (markdown with a code block) in place of its
/// one.
fn long_conversation(env: &ChatEnv) -> hutan_agents::CodexConfig {
    env.edited("plain-turn", |entries| {
        let first = entries
            .iter()
            .position(|e| {
                method(e) == Some("item/started")
                    && e["msg"]["params"]["item"]["type"] == "agentMessage"
            })
            .expect("the fixture streams an agent message");
        let last = entries
            .iter()
            .position(|e| {
                method(e) == Some("item/completed")
                    && e["msg"]["params"]["item"]["type"] == "agentMessage"
            })
            .expect("and completes it");
        let (thread, turn) = {
            let params = &entries[first]["msg"]["params"];
            (params["threadId"].clone(), params["turnId"].clone())
        };
        let mut messages = Vec::new();
        for n in 0..MESSAGES {
            let id = format!("msg_{n}");
            let text = format!(
                "## Part {n}\n\nSome **bold** text and `code`, then a list:\n\n- one\n- two\n\n```rust\nfn part_{n}() {{}}\n```\n"
            );
            let item = |text: &str| {
                json!({"type": "agentMessage", "id": id, "text": text, "phase": "commentary",
                       "memoryCitation": null, "delivery": null, "questions": null})
            };
            messages.push(says(json!({"method": "item/started", "params":
                {"item": item(""), "threadId": thread, "turnId": turn, "startedAtMs": 1}})));
            messages.push(says(json!({"method": "item/agentMessage/delta", "params":
                {"threadId": thread, "turnId": turn, "itemId": id, "delta": text}})));
            messages.push(says(json!({"method": "item/completed", "params":
                {"item": item(&text), "threadId": thread, "turnId": turn, "completedAtMs": 1}})));
        }
        entries.splice(first..=last, messages);
    })
}

/// What a test that owns a long chat keeps alive: the scratch directory of the fake agent and
/// the daemon.
struct LongChat {
    _env: ChatEnv,
    _daemon: TestDaemon,
    chat: Entity<ChatView>,
    model: Entity<ChatModel>,
}

fn open_long_chat(cx: &mut TestAppContext) -> (LongChat, &mut VisualTestContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(long_conversation(&env));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    // Run the turn: the fake agent plays all the messages.
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(info.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(info.id).is_some()
        })
    });
    let chat = view
        .read_with(cx, |wb, _| wb.chat_view_of(info.id))
        .unwrap();
    let model = view
        .read_with(cx, |wb, _| wb.chat_model_of(info.id))
        .unwrap();
    view.update_in(cx, |wb, window, cx| {
        if let Some(chat) = wb.chat_view_of(info.id) {
            window.focus(&chat.focus_handle(cx), cx);
        }
    });
    cx.simulate_input("go");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the conversation to arrive", |cx| {
        cx.executor().advance_clock(Duration::from_millis(50));
        model.read_with(cx, |m, _| m.transcript().len() >= MESSAGES)
    });
    (
        LongChat {
            _env: env,
            _daemon: daemon,
            chat,
            model,
        },
        cx,
    )
}

#[gpui::test]
fn a_long_transcript_builds_only_the_cards_in_view(cx: &mut TestAppContext) {
    let (long, cx) = open_long_chat(cx);
    let (chat, model) = (&long.chat, &long.model);
    for _ in 0..3 {
        draw(cx);
    }
    let entries = model.read_with(cx, |m, _| m.transcript().len());
    assert!(entries >= MESSAGES, "{entries} entries");
    let before = chat.read_with(cx, |c, _| c.entries_rendered());
    draw(cx);
    let one_frame = chat.read_with(cx, |c, _| c.entries_rendered()) - before;
    assert!(
        one_frame <= IN_VIEW,
        "a frame built {one_frame} cards of {entries}: only the cards in view may be built"
    );
}

#[gpui::test]
fn markdown_is_parsed_once_per_message_revision(cx: &mut TestAppContext) {
    let (long, cx) = open_long_chat(cx);
    let chat = &long.chat;
    for _ in 0..3 {
        draw(cx);
    }
    // Streaming parses a message again for each revision that is on screen; what must not
    // happen is a parse for a frame that changed no message.
    let parsed = chat.read_with(cx, |c, _| c.markdown_parses());
    assert!(parsed > 0, "the visible messages were parsed");
    for _ in 0..10 {
        draw(cx);
    }
    assert_eq!(
        chat.read_with(cx, |c, _| c.markdown_parses()),
        parsed,
        "frames that change no message parse nothing"
    );
}

/// The id of the chat's session, for messages of a failed assertion.
#[allow(dead_code)]
fn session_of(chat: &Entity<ChatView>, cx: &VisualTestContext) -> SessionId {
    chat.read_with(cx, |c, _| c.session_id())
}
