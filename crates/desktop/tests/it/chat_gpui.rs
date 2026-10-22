#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The agent chat view (M4-B) against an in-process daemon whose `codex app-server` is
//! `hutan-codex-fixture replay` over the transcripts recorded for M4-A: a new chat, sending,
//! streamed text, command and file-change cards, approvals (answered here and from another
//! client), reconnecting mid-turn, interrupting, and "Send to agent".

use crate::common;

use std::time::Duration;

use common::chat::{ChatEnv, method, says};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_chat_view::{ApprovalState, Body, TurnOutcome};
use hutan_core::agent::ToolStatus;
use hutan_core::{ApprovalId, SessionId, SessionState};
use hutan_desktop::workbench::chat::{ChatModel, ChatView, LoadStatus};
use hutan_desktop::workbench::{LaunchAgent, Models, Region, Workbench};
use hutan_protocol::chat::{ApprovalInfo, ApprovalRespond};
use hutan_protocol::messages::{Empty, Request, SessionRef};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_connected(cx, daemon, daemon.connection())
}

fn open_connected<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// Shows the chat of `session` in the focused pane and waits until its journal is loaded.
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

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

/// Types `text` into the focused composer and sends it with ⌘↩.
fn say(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.simulate_keystrokes("cmd-enter");
}

fn transcript<T>(
    model: &Entity<ChatModel>,
    cx: &VisualTestContext,
    f: impl FnOnce(&hutan_chat_view::Transcript) -> T,
) -> T {
    model.read_with(cx, |m, _| f(m.transcript()))
}

fn count(model: &Entity<ChatModel>, cx: &VisualTestContext, which: fn(&Body) -> bool) -> usize {
    transcript(model, cx, |t| {
        t.entries().iter().filter(|e| which(&e.body)).count()
    })
}

fn is_user(body: &Body) -> bool {
    matches!(body, Body::User(_))
}
fn is_assistant(body: &Body) -> bool {
    matches!(body, Body::Assistant(_))
}
fn is_tool(body: &Body) -> bool {
    matches!(body, Body::Tool(_))
}
fn is_footer(body: &Body) -> bool {
    matches!(body, Body::Footer(_))
}

fn assistant_texts(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<String> {
    transcript(model, cx, |t| {
        t.entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::Assistant(m) => Some(m.text.clone()),
                _ => None,
            })
            .collect()
    })
}

fn pending(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<(ApprovalId, String)> {
    transcript(model, cx, |t| {
        t.pending_approvals()
            .iter()
            .map(|a| (a.id, a.title.clone()))
            .collect()
    })
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

fn session_state(daemon: &TestDaemon, id: SessionId) -> SessionState {
    let list: hutan_protocol::messages::SessionList =
        daemon.call(Request::SessionList(Default::default()));
    list.sessions
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .state
}

fn pending_on_daemon(daemon: &TestDaemon) -> Vec<ApprovalInfo> {
    let list: hutan_protocol::chat::ApprovalList = daemon.call(Request::ApprovalList(
        hutan_protocol::chat::ApprovalListRequest::default(),
    ));
    list.approvals
}

// ---- send, stream, interrupt ----------------------------------------------------------------

#[gpui::test]
fn a_message_is_sent_streams_and_ends_with_a_footer(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);

    // The chat has the keyboard focus: typing goes to the composer.
    assert!(cx.update(|window, cx| chat.read(cx).focus_handle_is_focused(window)));
    cx.simulate_input("Reply with the single word pong and nothing else.");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "Reply with the single word pong and nothing else."
    );
    assert!(chat.read_with(cx, |c, cx| c.can_send(cx)));
    cx.simulate_keystrokes("cmd-enter");
    assert!(
        model.read_with(cx, |m, _| m.input().is_empty()),
        "the composer empties on send"
    );

    pump(cx, "the answer and the footer", |cx| {
        count(&model, cx, is_footer) == 1
    });
    assert_eq!(assistant_texts(&model, cx), ["pong"]);
    assert_eq!(count(&model, cx, is_user), 1);
    transcript(&model, cx, |t| {
        let Body::User(user) = &t.entries()[0].body else {
            panic!("the user's message comes first: {:?}", t.entries()[0].body);
        };
        assert_eq!(
            user.text,
            "Reply with the single word pong and nothing else."
        );
        let footer = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Footer(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert_eq!(footer.outcome, TurnOutcome::Completed);
        assert!(
            footer.usage.is_some_and(|u| u.total_tokens.is_some()),
            "the footer says what the turn used"
        );
    });
    // The drawn list shows the entries.
    draw(cx);
    for index in 0..3 {
        assert!(
            region(&view, cx, Region::ChatEntry(index)).is_some(),
            "entry {index} is drawn"
        );
    }
    assert_eq!(
        env.sent_texts(),
        ["Reply with the single word pong and nothing else."]
    );
    // The session is idle again and Send is offered.
    pump(cx, "the session to be idle", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .is_some_and(|s| s.state == SessionState::Idle)
        })
    });
    draw(cx);
    assert!(region(&view, cx, Region::Chat("send")).is_some());
    assert!(region(&view, cx, Region::Chat("interrupt")).is_none());
}

#[gpui::test]
fn the_interrupt_button_stops_a_turn_that_streams(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("interrupt"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(
        cx,
        "Write the numbers from 1 to 300, one per line, no other text.",
    );

    // The text so far is on screen while the turn runs.
    pump(cx, "streamed text", |cx| {
        assistant_texts(&model, cx)
            .first()
            .is_some_and(|t| t.starts_with("1\n2"))
    });
    pump(cx, "the interrupt button", |cx| {
        draw(cx);
        region(&view, cx, Region::Chat("interrupt")).is_some()
    });
    assert!(
        region(&view, cx, Region::Chat("send")).is_none(),
        "no Send while a turn runs"
    );
    assert!(transcript(&model, cx, |t| t.turn_active()));
    assert!(!chat.read_with(cx, |c, cx| c.can_send(cx)));
    // Typing is still possible while the turn runs (⌘↩ would queue it: `queue_gpui`).
    cx.simulate_input("next");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "next"
    );
    assert!(env.sent_texts().len() == 1);

    click(&view, cx, Region::Chat("interrupt"));
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    transcript(&model, cx, |t| {
        assert!(matches!(
            &t.entries().last().unwrap().body,
            Body::Footer(f) if f.outcome == TurnOutcome::Cancelled
        ));
        assert!(!t.turn_active());
    });
    assert!(
        env.received().iter().any(|(m, _)| m == "turn/interrupt"),
        "{:?}",
        env.received()
    );
    pump(cx, "Send to come back", |cx| {
        draw(cx);
        region(&view, cx, Region::Chat("send")).is_some()
            && region(&view, cx, Region::Chat("interrupt")).is_none()
    });
}

#[gpui::test]
fn the_interrupt_key_stops_the_turn_too(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("interrupt"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "count");
    pump(cx, "the turn to run", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .and_then(|s| s.chat.as_ref().map(|c| c.turn_active))
                == Some(true)
        })
    });
    cx.simulate_keystrokes("cmd-.");
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
}

// ---- cards ----------------------------------------------------------------------------------

/// `approval-touch` without the approval: the command runs, streams output and exits 0.
fn command_with_output(env: &ChatEnv) -> hutan_agents::CodexConfig {
    env.edited("approval-touch", |entries| {
        let ids = entries
            .iter()
            .find(|e| {
                method(e) == Some("item/started")
                    && e["msg"]["params"]["item"]["type"] == "commandExecution"
            })
            .map(|e| {
                (
                    e["msg"]["params"]["threadId"].clone(),
                    e["msg"]["params"]["turnId"].clone(),
                    e["msg"]["params"]["item"]["id"].clone(),
                )
            })
            .unwrap();
        entries.retain(|e| {
            let is_request = method(e) == Some("item/commandExecution/requestApproval");
            let is_resolved = method(e) == Some("serverRequest/resolved");
            // The client's answer to the request: a response, no method.
            let is_answer = e["dir"] == "out" && e["msg"].get("method").is_none();
            !(is_request || is_resolved || is_answer)
        });
        let at = entries
            .iter()
            .position(|e| {
                method(e) == Some("item/completed")
                    && e["msg"]["params"]["item"]["type"] == "commandExecution"
            })
            .unwrap();
        entries[at]["msg"]["params"]["item"]["aggregatedOutput"] = json!("hello\nworld\n");
        entries[at]["msg"]["params"]["item"]["exitCode"] = json!(0);
        entries[at]["msg"]["params"]["item"]["durationMs"] = json!(12);
        let delta = |text: &str| {
            says(json!({
                "method": "item/commandExecution/outputDelta",
                "params": {"threadId": ids.0, "turnId": ids.1, "itemId": ids.2, "delta": text}
            }))
        };
        entries.insert(at, delta("world\n"));
        entries.insert(at, delta("hello\n"));
    })
}

#[gpui::test]
fn a_command_card_shows_the_command_its_directory_and_output(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(command_with_output(&env));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(
        cx,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    );
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });

    assert_eq!(count(&model, cx, is_tool), 1);
    transcript(&model, cx, |t| {
        let tool = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Tool(tool) => Some(tool),
                _ => None,
            })
            .unwrap();
        assert_eq!(tool.command(), "/bin/zsh -lc 'touch hi.txt'");
        assert_eq!(tool.cwd(), Some("/tmp/hutan-fixture"));
        assert_eq!(tool.status, Some(ToolStatus::Completed));
        assert_eq!(tool.exit_code, Some(0));
        assert_eq!(tool.output, "hello\nworld\n");
        assert!(tool.approvals.is_empty());
    });
    assert_eq!(
        assistant_texts(&model, cx),
        ["I\u{2019}ll run `touch hi.txt` in the shell.", "done"]
    );
    draw(cx);
    // The card is on screen: the command, its status and the toggle.
    let tool_ix = transcript(&model, cx, |t| {
        t.entries().iter().position(|e| is_tool(&e.body)).unwrap()
    });
    assert!(region(&view, cx, Region::ChatEntry(tool_ix)).is_some());
    assert!(region(&view, cx, Region::ChatPart(tool_ix, 0)).is_some());
    // The inline Markdown of the assistant's text: the card list drew every entry.
    assert!(region(&view, cx, Region::ChatEntry(tool_ix + 1)).is_some());
}

#[gpui::test]
fn a_command_card_expands_and_collapses_its_output(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let many = env.edited("approval-touch", |entries| {
        entries.retain(|e| {
            method(e) != Some("item/commandExecution/requestApproval")
                && method(e) != Some("serverRequest/resolved")
                && !(e["dir"] == "out" && e["msg"].get("method").is_none())
        });
        let at = entries
            .iter()
            .position(|e| {
                method(e) == Some("item/completed")
                    && e["msg"]["params"]["item"]["type"] == "commandExecution"
            })
            .unwrap();
        let output: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        entries[at]["msg"]["params"]["item"]["aggregatedOutput"] = json!(output);
        entries[at]["msg"]["params"]["item"]["exitCode"] = json!(1);
    });
    let daemon = TestDaemon::start_with_codex(many);
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    let ix = transcript(&model, cx, |t| {
        t.entries().iter().position(|e| is_tool(&e.body)).unwrap()
    });
    pump(cx, "the card", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(ix)).is_some()
    });
    let collapsed = region(&view, cx, Region::ChatEntry(ix))
        .unwrap()
        .size
        .height;
    click(&view, cx, Region::ChatPart(ix, 0));
    pump(cx, "the output to be shown whole", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(ix)).is_some_and(|b| b.size.height > collapsed)
    });
    click(&view, cx, Region::ChatPart(ix, 0));
    pump(cx, "the output to collapse again", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(ix)).is_some_and(|b| b.size.height <= collapsed)
    });
}

/// `plain-turn` with the assistant's answer replaced by `text`.
fn answering(env: &ChatEnv, text: &'static str) -> hutan_agents::CodexConfig {
    env.edited("plain-turn", |entries| {
        for entry in entries.iter_mut() {
            if method(entry) == Some("item/completed")
                && entry["msg"]["params"]["item"]["type"] == "agentMessage"
            {
                entry["msg"]["params"]["item"]["text"] = json!(text);
            }
        }
    })
}

#[gpui::test]
fn markdown_messages_get_a_code_block_with_a_copy_button(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let answer = "Here is **bold** and `code`:\n\n```sh\nls -la\n```\n\n- one\n- two\n\nSee [the docs](https://example.com).";
    let daemon = TestDaemon::start_with_codex(answering(&env, answer));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "show me");
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    assert_eq!(assistant_texts(&model, cx), [answer]);
    pump(cx, "the copy button", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatCopy(1, 0)).is_some()
    });
    assert!(
        region(&view, cx, Region::ChatCopy(1, 1)).is_none(),
        "one code block"
    );
    click(&view, cx, Region::ChatCopy(1, 0));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("ls -la")
    );
}

#[gpui::test]
fn reasoning_plans_and_warnings_are_cards_of_their_own(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let config = env.edited("plain-turn", |entries| {
        let at = entries
            .iter()
            .position(|e| method(e) == Some("turn/started"))
            .unwrap()
            + 1;
        let started = &entries[at - 1]["msg"]["params"];
        let (thread, turn) = (started["threadId"].clone(), started["turn"]["id"].clone());
        let item = |kind: &str, summary: Vec<&str>| {
            json!({"type": "reasoning", "id": "rs_1", "summary": summary, "content": [], "kind": kind})
        };
        let inserted = vec![
            says(json!({"method": "item/started", "params": {"item": item("start", vec![]), "threadId": thread, "turnId": turn}})),
            says(json!({"method": "item/reasoning/summaryTextDelta", "params": {"threadId": thread, "turnId": turn, "itemId": "rs_1", "delta": "Check"}})),
            says(json!({"method": "item/completed", "params": {"item": item("end", vec!["Check the plan"]), "threadId": thread, "turnId": turn}})),
            says(json!({"method": "turn/plan/updated", "params": {"threadId": thread, "turnId": turn, "explanation": "two steps", "plan": [{"step": "read", "status": "completed"}, {"step": "write", "status": "inProgress"}]}})),
            says(json!({"method": "warning", "params": {"message": "careful now"}})),
        ];
        for (offset, entry) in inserted.into_iter().enumerate() {
            entries.insert(at + offset, entry);
        }
    });
    let daemon = TestDaemon::start_with_codex(config);
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    transcript(&model, cx, |t| {
        let reasoning = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Reasoning(r) => Some(r),
                _ => None,
            })
            .expect("a reasoning entry");
        assert!(reasoning.done);
        assert_eq!(reasoning.summary, "Check the plan");
        let plan = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Plan(p) => Some(p),
                _ => None,
            })
            .expect("a plan entry");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.explanation.as_deref(), Some("two steps"));
        assert!(t.entries().iter().any(|e| matches!(
            &e.body,
            Body::Notice { message, .. } if message == "careful now"
        )));
    });
    let reasoning_ix = transcript(&model, cx, |t| {
        t.entries()
            .iter()
            .position(|e| matches!(e.body, Body::Reasoning(_)))
            .unwrap()
    });
    // Reasoning starts folded; its header opens it.
    pump(cx, "the reasoning card", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(reasoning_ix)).is_some()
    });
    let folded = region(&view, cx, Region::ChatEntry(reasoning_ix))
        .unwrap()
        .size
        .height;
    click(&view, cx, Region::ChatPart(reasoning_ix, 0));
    pump(cx, "the summary to show", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(reasoning_ix)).is_some_and(|b| b.size.height > folded)
    });
}

#[gpui::test]
fn a_failed_turn_ends_with_its_error_and_the_chat_stays_usable(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("turn-failed"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "this will fail");
    pump(cx, "the turn to fail", |cx| {
        count(&model, cx, is_footer) == 1
    });
    transcript(&model, cx, |t| {
        let footer = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Footer(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert!(
            matches!(&footer.outcome, TurnOutcome::Failed { message, .. } if !message.is_empty())
        );
        assert!(!t.turn_active());
    });
    pump(cx, "the session to be idle again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .is_some_and(|s| s.state == SessionState::Idle)
        })
    });
    cx.simulate_input("again");
    assert!(chat.read_with(cx, |c, cx| c.can_send(cx)));
}

// ---- approvals ------------------------------------------------------------------------------

#[gpui::test]
fn an_approval_card_answers_through_the_daemon_and_resolves(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(
        cx,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    );

    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let (approval, title) = pending(&model, cx).remove(0);
    assert_eq!(title, "/bin/zsh -lc 'touch hi.txt'");
    // The request sits on its tool's card, not in an entry of its own.
    transcript(&model, cx, |t| {
        let tool = t
            .entries()
            .iter()
            .find_map(|e| match &e.body {
                Body::Tool(tool) => Some(tool),
                _ => None,
            })
            .unwrap();
        assert_eq!(tool.approvals, [approval]);
        let options: Vec<_> = t
            .approval(approval)
            .unwrap()
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect();
        assert_eq!(options.first(), Some(&"accept"));
        assert_eq!(options.last(), Some(&"cancel"));
    });
    // Global surfacing: the session is blocked, the status bar counts it, the store lists it.
    pump(cx, "the session to be blocked", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.approvals().len() == 1
                && store
                    .session(info.id)
                    .is_some_and(|s| store.presentation(s).label == "approval")
        })
    });
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        assert_eq!(store.requests(), [info.id]);
        let presentation = store.presentation(store.session(info.id).unwrap());
        assert!(presentation.is_request());
    });
    draw(cx);
    assert!(
        region(&view, cx, Region::StatusItem("attention"))
            .is_some_and(|b| b.size.width > gpui::px(10.0)),
        "the status bar counts the request"
    );
    assert!(region(&view, cx, Region::ChatOption(0)).is_some());
    assert!(region(&view, cx, Region::ChatOption(2)).is_some());
    assert!(
        region(&view, cx, Region::ChatOption(3)).is_none(),
        "three options offered"
    );

    // Nothing reached the agent yet.
    assert!(!env.received().iter().any(|(m, _)| m == "response"));
    click(&view, cx, Region::ChatOption(0));
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    transcript(&model, cx, |t| {
        let approval = t.approval(approval).unwrap();
        assert_eq!(
            approval.state,
            ApprovalState::Selected {
                option_id: "accept".into()
            }
        );
    });
    // The daemon passed exactly the chosen decision on.
    let answers: Vec<Value> = env
        .received()
        .into_iter()
        .filter(|(m, _)| m == "response")
        .map(|(_, msg)| msg["result"].clone())
        .collect();
    assert_eq!(answers, [json!({"decision": "accept"})]);
    view.read_with(cx, |wb, cx| {
        assert_eq!(
            wb.sessions().read(cx).answered_here(approval),
            Some("accept"),
            "this window remembers it answered"
        );
    });
    // The card says what was decided and where the answer came from.
    assert_eq!(
        chat.read_with(cx, |c, cx| c.resolution_text(approval, cx))
            .as_deref(),
        Some("Allowed once · from this window")
    );
    assert_eq!(
        transcript(&model, cx, |t| t
            .approval(approval)
            .unwrap()
            .decided_by
            .clone())
        .map(|by| by.client_kind),
        Some("desktop".to_owned())
    );
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    assert_eq!(
        assistant_texts(&model, cx).last().map(String::as_str),
        Some("done")
    );
    // The session's state follows the turn's last event (`session.updated` is a separate
    // message): wait for it rather than assume it arrived with the footer.
    pump(cx, "the session to stop needing the user", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).requests().is_empty())
    });
    view.read_with(cx, |wb, cx| {
        assert!(wb.sessions().read(cx).approvals().is_empty());
    });
}

#[gpui::test]
fn the_number_keys_answer_the_oldest_request(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    // A draft in the composer keeps the digit (it is text then).
    cx.simulate_input("a");
    cx.simulate_keystrokes("1");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "a1"
    );
    assert_eq!(pending(&model, cx).len(), 1);
    cx.simulate_keystrokes("backspace backspace");
    cx.simulate_keystrokes("1");
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    let answers: Vec<Value> = env
        .received()
        .into_iter()
        .filter(|(m, _)| m == "response")
        .map(|(_, msg)| msg["result"].clone())
        .collect();
    assert_eq!(answers, [json!({"decision": "accept"})]);
}

#[gpui::test]
fn an_approval_answered_from_another_client_resolves_the_card_live(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let (approval, _) = pending(&model, cx).remove(0);

    // `hutan approve <id> cancel`, as the CLI sends it.
    let _: Empty = daemon.call_as(
        hutan_protocol::messages::ClientKind::Cli,
        Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval,
            option_id: "cancel".into(),
        }),
    );
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    transcript(&model, cx, |t| {
        assert_eq!(
            t.approval(approval).unwrap().state,
            ApprovalState::Selected {
                option_id: "cancel".into()
            }
        );
    });
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        assert_eq!(store.answered_here(approval), None, "answered elsewhere");
        assert!(store.approvals().is_empty());
    });
    // The CLI says it answered, and the card says so.
    assert_eq!(
        chat.read_with(cx, |c, cx| c.resolution_text(approval, cx))
            .as_deref(),
        Some("Cancelled · from CLI")
    );
    draw(cx);
    assert!(
        region(&view, cx, Region::ChatOption(0)).is_none(),
        "the buttons are gone"
    );
}

#[gpui::test]
fn the_inspector_lists_the_requests_and_answers_them(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    pump(cx, "the inspector's button", |cx| {
        draw(cx);
        region(&view, cx, Region::InspectorOption(0)).is_some()
    });
    // The options are a grid of two columns (CL-POLISH-4): equal cells, a row at a time.
    let option = |cx: &VisualTestContext, n: usize| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::InspectorOption(n)))
    };
    let (a, b) = (
        option(cx, 0).unwrap(),
        option(cx, 1).expect("a second option"),
    );
    assert!((a.top() - b.top()).abs() <= gpui::px(1.0), "two to a row");
    assert!(
        (a.size.width - b.size.width).abs() <= gpui::px(3.0),
        "equal cells"
    );
    assert!(b.left() > a.left());
    if let Some(c) = option(cx, 2) {
        assert!(
            (c.left() - a.left()).abs() <= gpui::px(2.0),
            "the third starts the second row"
        );
        assert!(c.top() >= a.bottom());
        if let Some(d) = option(cx, 3) {
            assert!((d.top() - c.top()).abs() <= gpui::px(1.0));
            assert!((d.left() - b.left()).abs() <= gpui::px(2.0));
        }
    }
    click(&view, cx, Region::InspectorOption(0));
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    assert_eq!(
        env.received()
            .into_iter()
            .filter(|(m, _)| m == "response")
            .count(),
        1
    );
}

#[gpui::test]
fn review_approvals_lists_every_pending_request_across_sessions(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    pump(cx, "the store to know the request", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len() == 1)
    });

    // Another tab is in front: the review shows the chat again.
    view.update_in(cx, |wb, window, cx| wb.new_terminal(window, cx));
    pump(cx, "the terminal tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state().active_item().is_some_and(|i| {
                matches!(i, hutan_desktop::workbench::model::ItemRef::Terminal { .. })
            })
        })
    });
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("Review approvals");
    cx.simulate_keystrokes("enter");
    pump(cx, "the picker", |cx| {
        view.read_with(cx, |wb, _| wb.picker().is_some())
    });
    let titles = view.read_with(cx, |wb, _| wb.picker_titles());
    assert_eq!(titles.len(), 1, "{titles:?}");
    assert!(titles[0].contains("touch hi.txt"), "{titles:?}");
    cx.simulate_keystrokes("enter");
    pump(cx, "the chat to be shown", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .active_item()
                .is_some_and(|i| matches!(i, hutan_desktop::workbench::model::ItemRef::Chat { .. }))
        })
    });
    // Agent: Review Approvals opens the same list from anywhere.
    view.update_in(cx, |wb, window, cx| wb.new_terminal(window, cx));
    cx.run_until_parked();
    draw(cx);
    cx.dispatch_action(hutan_desktop::workbench::actions::ReviewApprovals);
    pump(cx, "the picker", |cx| {
        view.read_with(cx, |wb, _| wb.picker().is_some())
    });
    cx.simulate_keystrokes("escape");

    // No request waits: the picker says so.
    let _: Empty = daemon.call(Request::ApprovalRespond(ApprovalRespond {
        approval_id: pending(&model, cx).remove(0).0,
        option_id: "cancel".into(),
    }));
    pump(cx, "the store to be empty", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().is_empty())
    });
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("Review approvals");
    cx.simulate_keystrokes("enter");
    pump(cx, "the empty picker", |cx| {
        view.read_with(cx, |wb, _| wb.picker().is_some())
    });
    assert!(view.read_with(cx, |wb, _| wb.picker_titles().is_empty()));
}

// ---- file changes ---------------------------------------------------------------------------

#[gpui::test]
fn a_file_change_card_lists_the_files_and_expands_a_diff(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("file-change"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(
        cx,
        "Create a file named hello.txt containing the single line hi.",
    );
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });

    let (index, files) = transcript(&model, cx, |t| {
        let (index, tool) = t
            .entries()
            .iter()
            .enumerate()
            .find_map(|(i, e)| match &e.body {
                Body::Tool(tool) => Some((i, tool)),
                _ => None,
            })
            .unwrap();
        assert_eq!(tool.name, "apply_patch");
        (index, tool.changes.clone())
    });
    assert_eq!(files.len(), 1);
    assert!(files[0].path.ends_with("hello.txt"));
    let diff = hutan_chat_view::diff::from_change(&files[0]);
    assert_eq!((diff.added, diff.removed), (1, 0));
    assert_eq!(diff.lines[0].text, "hi");

    // A request that waits shows its diff; an answered one folds it away.
    pump(cx, "the card with the diff", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(index)).is_some()
    });
    let with_diff = region(&view, cx, Region::ChatEntry(index))
        .unwrap()
        .size
        .height;
    click(&view, cx, Region::ChatOption(0));
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    pump(cx, "the diff to fold away", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(index)).is_some_and(|b| b.size.height < with_diff)
    });
    let folded = region(&view, cx, Region::ChatEntry(index))
        .unwrap()
        .size
        .height;
    // Clicking the file shows the diff again.
    click(&view, cx, Region::ChatPart(index, 100));
    pump(cx, "the diff to open", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatEntry(index)).is_some_and(|b| b.size.height > folded)
    });
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    // The turn's own diff duplicates the card and is not drawn as another card.
    assert!(transcript(&model, cx, |t| t
        .entries()
        .iter()
        .any(|e| matches!(e.body, Body::TurnDiff(_)))));
}

// ---- reconnecting ---------------------------------------------------------------------------

#[gpui::test]
fn a_reconnect_mid_turn_continues_without_duplicates(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let mut daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let connection = daemon.proxied_connection();
    let (view, cx) = open_connected(cx, &daemon, connection);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(
        cx,
        "Run `touch hi.txt` in a shell, then reply with the word done.",
    );
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let before = transcript(&model, cx, |t| t.len());

    // The link drops and stays down while the turn goes on without this window: another
    // client answers, the agent finishes.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    pump(cx, "the connection to notice", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() != hutan_client::ConnectionState::Connected
        })
    });
    let approval = pending_on_daemon(&daemon).remove(0);
    let _: Empty = daemon.call(Request::ApprovalRespond(ApprovalRespond {
        approval_id: approval.id,
        option_id: "accept".into(),
    }));
    pump(cx, "the daemon's turn to finish", |_| {
        session_state(&daemon, info.id) == SessionState::Idle
    });
    assert_eq!(
        transcript(&model, cx, |t| t.len()),
        before,
        "nothing arrived while the link was down"
    );

    daemon.proxy().block(false);
    pump(
        cx,
        "the connection to come back and the turn to show",
        |cx| count(&model, cx, is_footer) == 1,
    );
    // Everything once: one question, one command, two assistant messages (before and after the
    // command), one footer; the request is answered.
    assert_eq!(count(&model, cx, is_user), 1);
    assert_eq!(count(&model, cx, is_tool), 1);
    assert_eq!(count(&model, cx, is_assistant), 2);
    assert_eq!(count(&model, cx, is_footer), 1);
    assert_eq!(
        assistant_texts(&model, cx).last().map(String::as_str),
        Some("done")
    );
    transcript(&model, cx, |t| {
        assert!(t.pending_approvals().is_empty());
        assert_eq!(
            t.approval(approval.id).unwrap().state,
            ApprovalState::Selected {
                option_id: "accept".into()
            }
        );
    });
    // And the conversation goes on over the new connection.
    pump(cx, "the connection", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() == hutan_client::ConnectionState::Connected
        })
    });
    assert!(!model.read_with(cx, |m, _| m.sending()));
}

#[gpui::test]
fn a_short_reconnect_during_an_approval_keeps_the_card_and_answers_afterwards(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let mut daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let connection = daemon.proxied_connection();
    let (view, cx) = open_connected(cx, &daemon, connection);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let before = transcript(&model, cx, |t| t.len());

    // The link stays down (new connections are closed at once) until the test has seen the
    // drop: the first retry comes ~100 ms after it, and a loaded machine can leave the test
    // thread busy for longer than that, so the drop would go unnoticed.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    pump(cx, "the connection to drop", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() != hutan_client::ConnectionState::Connected
        })
    });
    daemon.proxy().block(false);
    pump(cx, "the connection to return", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() == hutan_client::ConnectionState::Connected
        })
    });
    // Let the reload settle: a second Resync is harmless, the list is unchanged.
    pump(cx, "the reload", |cx| {
        cx.executor().advance_clock(Duration::from_millis(200));
        transcript(&model, cx, |t| t.len()) == before
    });
    assert_eq!(pending(&model, cx).len(), 1, "the request still waits");
    assert_eq!(count(&model, cx, is_user), 1);
    assert_eq!(count(&model, cx, is_tool), 1);
    click(&view, cx, Region::ChatOption(0));
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    assert_eq!(count(&model, cx, is_assistant), 2);
}

// ---- start -----------------------------------------------------------------------------------

#[gpui::test]
fn start_runs_a_codex_chat_as_a_task_and_opens_it_focused(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let (view, cx) = open(cx, &daemon);
    cx.simulate_keystrokes("cmd-shift-t");
    // A first sheet opens on a plain terminal: the agent chat is picked by hand.
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    // Nothing else is installed here: the sheet finds OMP and Claude Code missing and
    // preselects Codex by itself.
    pump(cx, "Codex to be startable and its models", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher().is_some_and(|l| {
                l.agent() == &LaunchAgent::Harness("codex".into())
                    && matches!(wb.launcher_models(cx), Models::Ready(_))
            }) && wb
                .launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "codex" && h.enabled())
        })
    });
    let (default, home) = view.read_with(cx, |wb, cx| {
        (
            wb.launcher_chosen_model(cx).map(|m| m.id),
            wb.sessions()
                .read(cx)
                .home_workspace()
                .and_then(|w| w.roots.first().cloned()),
        )
    });
    assert!(
        default.is_some(),
        "the catalog's default is the chat's model"
    );
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("start")).is_some());
    assert!(region(&view, cx, Region::LauncherItem("model", 0)).is_none());

    // Escape closes it without a session.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_none()));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0
    );

    // ⌘↩ starts the chat as a task; its tab opens and has the focus.
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    pump(cx, "Codex to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "codex" && h.enabled())
                && wb
                    .launcher()
                    .is_some_and(|l| l.agent() == &LaunchAgent::Harness("codex".into()))
        })
    });
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_input("Say hello");
    cx.simulate_keystrokes("cmd-enter");
    // The first prompt to this agent and model asks before it is sent.
    cx.simulate_keystrokes("enter");
    pump(cx, "the chat tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher().is_none()
                && wb.state().active_item().is_some_and(|i| {
                    matches!(i, hutan_desktop::workbench::model::ItemRef::Chat { .. })
                })
        })
    });
    let session = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).sessions().first().cloned()
        })
        .unwrap();
    let chat = session.chat.clone().expect("a chat session");
    assert_eq!(chat.agent, "codex");
    assert_eq!(Some(chat.model.clone()), default);
    assert_eq!(session.cwd, home);
    // It is a task like any other: the Tasks panel and the history know it.
    let tasks = view.read_with(cx, |wb, cx| wb.sessions().read(cx).tasks().ordered().len());
    assert_eq!(tasks, 1);
    pump(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    let focused = view
        .read_with(cx, |wb, _| wb.chat_view_of(session.id))
        .unwrap();
    assert!(cx.update(|window, cx| focused.read(cx).focus_handle_is_focused(window)));
    // Once the first turn ended the task is done; the turn changed no files, so nothing
    // waits for a reviewer (FX-REVIEW rule 2).
    pump(cx, "the first turn to end", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .tasks()
                .ordered()
                .first()
                .is_some_and(|t| {
                    t.state == hutan_core::task::TaskState::Done
                        && t.delivery == hutan_core::task::DeliveryState::None
                })
        })
    });
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        let shown = store
            .session(session.id)
            .map(|s| store.presentation(s).label);
        assert_ne!(shown.as_deref(), Some("review"));
    });
}

#[gpui::test]
fn start_says_why_it_cannot_start_without_a_daemon(cx: &mut TestAppContext) {
    let (connection, settings, state) = common::offline_connection();
    let (view, cx) = open_with(cx, settings, connection, Some(state));
    cx.simulate_keystrokes("cmd-shift-t");
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_none()));
    assert!(view.read_with(cx, |wb, _| {
        wb.notice().is_some_and(|n| n.contains("Not connected"))
    }));
}

// ---- the composer ---------------------------------------------------------------------------

#[gpui::test]
fn return_inserts_a_newline_and_the_keys_edit_the_draft(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    let draft = |cx: &VisualTestContext| model.read_with(cx, |m, _| m.input().text().to_owned());
    cx.simulate_input("ab");
    cx.simulate_keystrokes("left");
    cx.simulate_input("X");
    assert_eq!(draft(cx), "aXb");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("é");
    assert_eq!(draft(cx), "aX\néb");
    cx.simulate_keystrokes("backspace backspace");
    assert_eq!(draft(cx), "aXb");
    assert!(env.sent_texts().is_empty(), "Return did not send");
    cx.simulate_keystrokes("cmd-backspace");
    assert_eq!(draft(cx), "b");
}

#[gpui::test]
fn the_enter_send_key_makes_return_send_and_shift_return_a_newline(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[chat]\nsend_key = \"enter\"\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    cx.simulate_input("one");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("two");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "one\ntwo"
    );
    cx.simulate_keystrokes("enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["one\ntwo"]);
    assert!(model.read_with(cx, |m, _| m.input().is_empty()));
}

/// Switches `chat.send_key` the way a settings change does, while everything stays open.
fn set_send_key(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str) {
    view.update(cx, |wb, _| {
        wb.settings_store()
            .write_user("chat.send_key", toml::Value::String(key.to_owned()))
            .unwrap();
    });
    draw(cx);
}

#[gpui::test]
fn with_the_cmd_enter_send_key_return_and_shift_return_add_a_line_and_cmd_return_sends(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.send_key()),
        hutan_desktop::settings::SendKey::CmdEnter,
        "the default"
    );
    cx.simulate_input("one");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("two");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("three");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "one\ntwo\nthree"
    );
    assert!(env.sent_texts().is_empty(), "no Return sent");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["one\ntwo\nthree"]);
}

#[gpui::test]
fn with_the_enter_send_key_alt_return_adds_a_line_and_cmd_return_still_sends(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[chat]\nsend_key = \"enter\"\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    cx.simulate_input("one");
    cx.simulate_keystrokes("alt-enter");
    cx.simulate_input("two");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "one\ntwo"
    );
    assert!(env.sent_texts().is_empty(), "⌥Return added a line");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(
        env.sent_texts(),
        ["one\ntwo"],
        "⌘Return is never a dead key"
    );
}

#[gpui::test]
fn a_return_that_commits_marked_text_never_sends(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[chat]\nsend_key = \"enter\"\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    cx.simulate_input("ab");
    // An input method is composing: the text is marked.
    model.update(cx, |m, cx| {
        m.edit(|input| input.mark(None, "かな", None), cx)
    });
    assert!(model.read_with(cx, |m, _| m.input().marked().is_some()));
    cx.simulate_keystrokes("enter");
    assert!(
        env.sent_texts().is_empty(),
        "Return only committed the text"
    );
    assert!(model.read_with(cx, |m, _| m.input().marked().is_none()));
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "abかな"
    );
    // The next Return sends.
    cx.simulate_keystrokes("enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["abかな"]);
}

#[gpui::test]
fn the_hint_switches_the_send_key_in_an_open_composer_and_the_palette_toggles_it(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::settings::SendKey;
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    cx.simulate_input("one");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("two");
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "one\ntwo",
        "⌘Return mode: Return added a line"
    );

    // The hint is a control: it opens a menu of the two choices, the current one checked.
    click(&view, cx, Region::Chat("send-key"));
    let entries = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the menu is open");
    assert_eq!(
        entries
            .iter()
            .map(|(label, checked)| (label.to_string(), *checked))
            .collect::<Vec<_>>(),
        [
            ("⌘Return (Return adds a line)".to_owned(), true),
            ("Return (⇧Return adds a line)".to_owned(), false)
        ]
    );
    click(&view, cx, Region::MenuEntry(1));
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::Enter);
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_none()));
    let saved = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();
    assert!(saved.contains("send_key = \"enter\""), "{saved}");

    // The composer that was open follows at once, keyboard and all: Return sends now.
    assert!(cx.update(|window, cx| chat.read(cx).focus_handle_is_focused(window)));
    cx.simulate_keystrokes("enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    assert_eq!(env.sent_texts(), ["one\ntwo"]);

    // ⌘K "Chat: Toggle Send Key" flips it back and says what sends now.
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("toggle send key");
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    assert_eq!(titles[0].as_ref(), "Chat: Toggle Send Key");
    cx.simulate_keystrokes("enter");
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::CmdEnter);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("⌘Return now sends".to_owned())
    );
    cx.dispatch_action(hutan_desktop::workbench::actions::ToggleSendKey);
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::Enter);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("Return now sends".to_owned())
    );
}

#[gpui::test]
fn the_first_composer_asks_once_which_key_sends_after_the_setup_is_done(cx: &mut TestAppContext) {
    use hutan_desktop::settings::{ChatSettings, SendKey};
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, _model) = open_chat(&view, cx, info.id);
    draw(cx);
    assert!(
        region(&view, cx, Region::Chat("send-key-enter")).is_none(),
        "not before the setup is done"
    );
    view.update(cx, |wb, _| {
        wb.settings_store()
            .write_user("app.setup_done", toml::Value::Boolean(true))
            .unwrap();
    });
    draw(cx);
    click(&view, cx, Region::Chat("send-key-enter"));
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::Enter);
    assert!(!view.read_with(cx, |wb, _| {
        wb.settings_store().get::<ChatSettings>().ask_send_key
    }));
    draw(cx);
    assert!(
        region(&view, cx, Region::Chat("send-key-enter")).is_none(),
        "the question is gone for good"
    );

    // Dismissing it keeps the key and stops the question too.
    view.update(cx, |wb, _| {
        wb.settings_store()
            .write_user("chat.ask_send_key", toml::Value::Boolean(true))
            .unwrap();
    });
    draw(cx);
    click(&view, cx, Region::Chat("send-key-dismiss"));
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::Enter);
    assert!(!view.read_with(cx, |wb, _| {
        wb.settings_store().get::<ChatSettings>().ask_send_key
    }));
}

#[gpui::test]
fn an_ended_session_disables_the_composer(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, _model) = open_chat(&view, cx, info.id);
    assert_eq!(chat.read_with(cx, |c, cx| c.blocked_reason(cx)), None);
    cx.simulate_input("hello");
    assert!(chat.read_with(cx, |c, cx| c.can_send(cx)));

    let _: hutan_protocol::messages::SessionResult =
        daemon.call(Request::SessionStop(SessionRef {
            session_id: info.id,
        }));
    pump(cx, "the session to stop", |cx| {
        chat.read_with(cx, |c, cx| c.blocked_reason(cx).is_some())
    });
    assert!(!chat.read_with(cx, |c, cx| c.can_send(cx)));
    pump(cx, "the restart button", |cx| {
        draw(cx);
        region(&view, cx, Region::Chat("restart")).is_some()
    });
    assert!(region(&view, cx, Region::Chat("send")).is_none());
    cx.simulate_keystrokes("cmd-enter");
    assert!(env.sent_texts().is_empty());
}

// ---- send to agent --------------------------------------------------------------------------

fn scripted_terminal(daemon: &TestDaemon) -> hutan_protocol::messages::SessionInfo {
    daemon.sh(
        "i=1; while [ $i -le 3 ]; do \
         printf '\\033]133;A\\007$ \\033]133;B\\007cmd%d\\r\\n\\033]133;C\\007out%d\\r\\n\\033]133;D;%d\\007' $i $i $((i%2)); \
         i=$((i+1)); done; printf '\\033]133;A\\007$ \\033]133;B\\007'; sleep 600",
    )
}

fn terminal_blocks(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Vec<hutan_protocol::blocks::Block> {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(session)
            .map(|v| v.read(cx).model().blocks().iter().cloned().collect())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn send_to_agent_attaches_a_block_to_the_composer_of_the_last_chat(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let chat_info = daemon.create_chat(&env.cwd());
    let terminal = scripted_terminal(&daemon);
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, chat_info.id);
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(terminal.id, window, cx)
    });
    pump(cx, "three finished commands", |cx| {
        terminal_blocks(&view, cx, terminal.id)
            .iter()
            .filter(|b| b.state == hutan_protocol::blocks::BlockState::Finished)
            .count()
            == 3
    });
    draw(cx);

    // Block 2 is `cmd2` (exit 0 for 2 % 2, 1 for the others).
    let block = terminal_blocks(&view, cx, terminal.id)
        .into_iter()
        .find(|b| b.command.as_deref() == Some("cmd1"))
        .unwrap();
    let terminal_view = view
        .read_with(cx, |wb, _| wb.terminal_view_of(terminal.id))
        .unwrap();
    assert!(
        terminal_view.update(cx, |v, cx| v.send_block_to_agent(block.id, cx)),
        "the block can be sent"
    );
    pump(cx, "the chip", |cx| {
        model.read_with(cx, |m, _| !m.chips().is_empty())
    });
    let chips = model.read_with(cx, |m, _| m.chips().to_vec());
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].label, "$ cmd1 · exit 1");
    assert!(
        chips[0].quoted.contains("$ cmd1\nout1"),
        "{}",
        chips[0].quoted
    );
    assert!(!chips[0].truncated);
    // The chat came to the front, where the context went.
    assert!(
        view.read_with(cx, |wb, _| wb.state().active_item().is_some_and(
            |i| matches!(i, hutan_desktop::workbench::model::ItemRef::Chat { .. })
        ))
    );

    // Sending the message sends the quote in front of what was typed.
    draw(cx);
    assert!(region(&view, cx, Region::Chat("composer")).is_some());
    cx.simulate_input("why did it fail?");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the message to arrive", |_| env.sent_texts().len() == 1);
    let sent = env.sent_texts().remove(0);
    assert!(
        sent.starts_with("Context from my terminal (exit 1):\n```\n$ cmd1\nout1\n```"),
        "{sent}"
    );
    assert!(sent.ends_with("\n\nwhy did it fail?"), "{sent}");
    assert!(model.read_with(cx, |m, _| m.chips().is_empty()));
}

#[gpui::test]
fn send_to_agent_cuts_long_output_at_the_setting(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let chat_info = daemon.create_chat(&env.cwd());
    let terminal = daemon.sh(
        "printf '\\033]133;A\\007$ \\033]133;B\\007big\\r\\n\\033]133;C\\007'; \
         i=0; while [ $i -lt 40 ]; do printf 'line %02d\\r\\n' $i; i=$((i+1)); done; \
         printf '\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; sleep 600",
    );
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[chat]\nattach_max_bytes = 64\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, chat_info.id);
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(terminal.id, window, cx)
    });
    pump(cx, "the finished command", |cx| {
        terminal_blocks(&view, cx, terminal.id)
            .iter()
            .any(|b| b.state == hutan_protocol::blocks::BlockState::Finished)
    });
    let block = terminal_blocks(&view, cx, terminal.id)
        .into_iter()
        .find(|b| b.state == hutan_protocol::blocks::BlockState::Finished)
        .unwrap();
    let terminal_view = view
        .read_with(cx, |wb, _| wb.terminal_view_of(terminal.id))
        .unwrap();
    assert!(terminal_view.update(cx, |v, cx| v.send_block_to_agent(block.id, cx)));
    pump(cx, "the chip", |cx| {
        model.read_with(cx, |m, _| !m.chips().is_empty())
    });
    let chip = model.read_with(cx, |m, _| m.chips()[0].clone());
    assert!(chip.truncated);
    assert!(chip.label.contains("cut"), "{}", chip.label);
    assert!(chip.quoted.ends_with("(the output was cut)"));
    assert!(chip.quoted.len() < 400, "{} bytes", chip.quoted.len());
    assert!(chip.quoted.contains("line 00"));
    assert!(!chip.quoted.contains("line 39"));
}

#[gpui::test]
fn send_to_agent_without_a_chat_says_so(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let terminal = scripted_terminal(&daemon);
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_session(terminal.id, window, cx)
    });
    pump(cx, "a finished command", |cx| {
        terminal_blocks(&view, cx, terminal.id)
            .iter()
            .any(|b| b.state == hutan_protocol::blocks::BlockState::Finished)
    });
    let block = terminal_blocks(&view, cx, terminal.id)
        .into_iter()
        .find(|b| b.state == hutan_protocol::blocks::BlockState::Finished)
        .unwrap();
    let terminal_view = view
        .read_with(cx, |wb, _| wb.terminal_view_of(terminal.id))
        .unwrap();
    terminal_view.update(cx, |v, cx| v.send_block_to_agent(block.id, cx));
    cx.run_until_parked();
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert!(
        notice.is_some_and(|n| n.contains("No agent chat")),
        "a notice says why"
    );
}

// ---- the chat as a tab ----------------------------------------------------------------------

#[gpui::test]
fn a_chat_tab_follows_the_tab_rules_and_its_model_goes_with_the_last_tab(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, _model) = open_chat(&view, cx, info.id);
    let item = hutan_desktop::workbench::model::ItemRef::Chat {
        session: info.id.to_string(),
    };
    assert!(view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some()));
    // Opening it again focuses the tab instead of adding one.
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .state()
            .panes()
            .iter()
            .flat_map(|p| p.items.iter())
            .filter(|i| **i == item)
            .count()),
        1
    );
    // The tab is named after the session; the session's pane header icon is the agent's.
    let title = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&item, cx));
    assert_eq!(title.0, hutan_desktop::icons::Icon::SessionHarness);
    // A split shows the same conversation in a second view over one model.
    view.update_in(cx, |wb, window, cx| {
        wb.open_session_in_split(
            info.id,
            hutan_desktop::workbench::model::Edge::Right,
            window,
            cx,
        );
    });
    cx.run_until_parked();
    draw(cx);
    // Closing every tab drops the model; opening again reads the journal again.
    view.update_in(cx, |wb, _, cx| {
        wb.edit_layout(cx, |s| {
            while let Some((pane, index)) = s.find_item(&item) {
                if s.close_item(pane, index).is_err() {
                    break;
                }
            }
        });
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.chat_model_of(info.id).is_none()));
    let (_chat, model) = open_chat(&view, cx, info.id);
    assert!(model.read_with(cx, |_, _| true));
}

#[gpui::test]
fn history_shows_after_the_tab_is_closed_and_opened_again(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "ping");
    pump(cx, "the turn", |cx| count(&model, cx, is_footer) == 1);
    let shown = transcript(&model, cx, |t| t.len());

    let item = hutan_desktop::workbench::model::ItemRef::Chat {
        session: info.id.to_string(),
    };
    view.update_in(cx, |wb, _, cx| {
        wb.edit_layout(cx, |s| {
            while let Some((pane, index)) = s.find_item(&item) {
                if s.close_item(pane, index).is_err() {
                    break;
                }
            }
        });
    });
    cx.run_until_parked();
    let (_chat, again) = open_chat(&view, cx, info.id);
    assert_eq!(
        transcript(&again, cx, |t| t.len()),
        shown,
        "the journal gives the same list"
    );
    assert_eq!(assistant_texts(&again, cx), ["pong"]);
}

#[gpui::test]
fn an_answer_from_another_window_says_so_and_so_does_a_reopened_journal(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "go");
    pump(cx, "the approval card", |cx| {
        !pending(&model, cx).is_empty()
    });
    let (approval, _) = pending(&model, cx).remove(0);

    // Another desktop window answers.
    let _: Empty = daemon.call_as(
        hutan_protocol::messages::ClientKind::Desktop,
        Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval,
            option_id: "accept".into(),
        }),
    );
    pump(cx, "the card to resolve", |cx| {
        pending(&model, cx).is_empty()
    });
    assert_eq!(
        chat.read_with(cx, |c, cx| c.resolution_text(approval, cx))
            .as_deref(),
        Some("Allowed once · from another window")
    );
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });

    // A window that opens the chat later reads the answer's origin from the journal.
    let (second, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&second, cx, info.id);
    assert_eq!(
        transcript(&model, cx, |t| t
            .approval(approval)
            .unwrap()
            .decided_by
            .clone())
        .map(|by| by.client_kind),
        Some("desktop".to_owned())
    );
    assert_eq!(
        chat.read_with(cx, |c, cx| c.resolution_text(approval, cx))
            .as_deref(),
        Some("Allowed once · from another window"),
        "this window did not answer it"
    );
}

/// `plain-turn` with its turn repeated `turns` times, ids made unique.
pub(crate) fn many_turns(env: &ChatEnv, turns: usize) -> hutan_agents::CodexConfig {
    env.edited("plain-turn", |entries| {
        let turn: Vec<Value> = entries.split_off(9);
        for k in 1..=turns {
            for entry in &turn {
                let text = entry
                    .to_string()
                    .replace("000000000002", &format!("{k:010x}02"))
                    .replace("000000000003", &format!("{k:010x}03"))
                    .replace("msg_fixture_1", &format!("msg_fixture_{k}"))
                    .replace(
                        "Reply with the single word pong and nothing else.",
                        &format!("message {k}"),
                    )
                    .replace("\"pong\"", &format!("\"answer {k}\""));
                entries.push(serde_json::from_str(&text).unwrap());
            }
        }
    })
}

pub(crate) fn wait_idle(daemon: &TestDaemon, id: SessionId) {
    let start = std::time::Instant::now();
    while session_state(daemon, id) != SessionState::Idle {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the turn never ended"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn load_earlier_pages_back_to_the_start_keeping_the_readers_place(cx: &mut TestAppContext) {
    const TURNS: usize = 60;
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(many_turns(&env, TURNS));
    let info = daemon.create_chat(&env.cwd());
    for turn in 1..=TURNS {
        let _: hutan_protocol::chat::ChatSent =
            daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
                session_id: info.id,
                text: format!("message {turn}"),
                attachments: vec![],
            }));
        // Each turn waits for the one before: the session returns to idle when it ends.
        std::thread::sleep(Duration::from_millis(5));
        wait_idle(&daemon, info.id);
    }
    let journal: hutan_protocol::chat::ChatHistory = daemon.call(Request::ChatHistory(
        hutan_protocol::chat::ChatHistoryRequest {
            session_id: info.id,
            before_seq: None,
            limit: hutan_protocol::chat::HISTORY_MAX_LIMIT,
        },
    ));
    assert!(
        journal.events.len() > 2 * hutan_protocol::chat::HISTORY_DEFAULT_LIMIT as usize,
        "{} events: more than two pages",
        journal.events.len()
    );

    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    // The first page is the newest events only.
    assert!(model.read_with(cx, |m, _| m.has_earlier()));
    let first_page = transcript(&model, cx, |t| t.len());
    let users = count(&model, cx, is_user);
    assert!(
        users < TURNS,
        "{users} of {TURNS} messages on the first page"
    );
    assert!(first_page > 0);
    draw(cx);
    assert!(
        region(&view, cx, Region::Chat("load-earlier")).is_some(),
        "a Load earlier row is offered"
    );

    // Scrolling away from the end and to the top reads the page before it; the list keeps
    // showing the entry the reader was at.
    let bounds = region(&view, cx, Region::Chat("earlier")).unwrap();
    let over_list = gpui::point(bounds.center().x, bounds.bottom() + gpui::px(200.0));
    let scroll_up = |cx: &mut VisualTestContext| {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: over_list,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(100_000.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        draw(cx);
    };
    // The list measures what is near the reader as it goes: scroll until the top is reached.
    let oldest = transcript(&model, cx, |t| t.entries()[0].id);
    let mut scrolls = 0;
    while transcript(&model, cx, |t| t.len()) == first_page {
        assert!(scrolls < 40, "never reached the top");
        scroll_up(cx);
        scrolls += 1;
        // The page is on its way (or, when it landed during the draw, here).
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !chat.read_with(cx, |c, _| c.following_tail()),
        "scrolled away from the end"
    );
    pump(cx, "the earlier page", |cx| {
        !model.read_with(cx, |m, _| m.loading_earlier())
    });
    draw(cx);
    let after = chat
        .read_with(cx, |c, cx| c.scroll_anchor(cx))
        .expect("an anchor");
    assert_eq!(
        after.0, oldest,
        "the reader is still at the entry that was the oldest"
    );
    assert!(
        transcript(&model, cx, |t| t.entries()[0].id) != oldest,
        "the older entries are in front"
    );

    // Keep scrolling to the top until the journal has nothing more.
    for _ in 0..20 {
        if !model.read_with(cx, |m, _| m.has_earlier()) {
            break;
        }
        scroll_up(cx);
        pump(cx, "the page to land", |cx| {
            !model.read_with(cx, |m, _| m.loading_earlier())
        });
    }
    assert!(
        !model.read_with(cx, |m, _| m.has_earlier()),
        "paged to the start"
    );
    draw(cx);
    assert!(
        region(&view, cx, Region::Chat("load-earlier")).is_none(),
        "nothing more to load"
    );
    // Every message exactly once, oldest first.
    let sent: Vec<String> = transcript(&model, cx, |t| {
        t.entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::User(m) => Some(m.text.clone()),
                _ => None,
            })
            .collect()
    });
    let expected: Vec<String> = (1..=TURNS).map(|k| format!("message {k}")).collect();
    assert_eq!(sent, expected);
    assert_eq!(count(&model, cx, is_footer), TURNS);
    assert_eq!(
        assistant_texts(&model, cx),
        (1..=TURNS)
            .map(|k| format!("answer {k}"))
            .collect::<Vec<_>>()
    );
    let ids: std::collections::HashSet<u64> =
        transcript(&model, cx, |t| t.entries().iter().map(|e| e.id).collect());
    assert_eq!(
        ids.len(),
        transcript(&model, cx, |t| t.len()),
        "no entry twice"
    );
}

#[gpui::test]
fn a_window_opened_mid_stream_shows_the_text_so_far_and_ends_with_the_final_text(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let gate_one = env.work.path().join("gate-one");
    let gate_two = env.work.path().join("gate-two");
    let config = env.edited("plain-turn", |entries| {
        let at = entries
            .iter()
            .position(|e| e["msg"]["method"] == "item/agentMessage/delta")
            .unwrap();
        let template = entries[at].clone();
        let delta = |text: &str| {
            let mut entry = template.clone();
            entry["msg"]["params"]["delta"] = json!(text);
            entry
        };
        let mut completed = entries[at + 1].clone();
        completed["msg"]["params"]["item"]["text"] = json!("Hello, brave world");
        let tail: Vec<Value> = entries.split_off(at + 2);
        entries.truncate(at);
        entries.extend([
            delta("Hello, "),
            json!({"dir": "hold", "file": gate_one}),
            delta("brave "),
            delta("world"),
            json!({"dir": "hold", "file": gate_two}),
            completed,
        ]);
        entries.extend(tail);
    });
    let daemon = TestDaemon::start_with_codex(config);
    let info = daemon.create_chat(&env.cwd());
    let _: hutan_protocol::chat::ChatSent =
        daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: info.id,
            text: "greet me".to_owned(),
            attachments: vec![],
        }));
    // The stream is stopped at the first gate; give the daemon a moment to have read it.
    std::thread::sleep(Duration::from_millis(300));

    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    pump(cx, "the text so far", |cx| {
        assistant_texts(&model, cx) == ["Hello, "]
    });
    assert!(transcript(&model, cx, |t| t.turn_active()));
    transcript(&model, cx, |t| {
        let Body::Assistant(message) = &t.entries().last().unwrap().body else {
            panic!("the message is the last entry");
        };
        assert!(!message.done, "it is still streaming");
    });

    // The rest streams in live: the window continues from the text it has.
    std::fs::write(&gate_one, "").unwrap();
    pump(cx, "the live deltas", |cx| {
        assistant_texts(&model, cx) == ["Hello, brave world"]
    });
    std::fs::write(&gate_two, "").unwrap();
    pump(cx, "the end of the turn", |cx| {
        count(&model, cx, is_footer) == 1
    });
    assert_eq!(assistant_texts(&model, cx), ["Hello, brave world"]);
    assert_eq!(
        count(&model, cx, is_assistant),
        1,
        "one message, not one per snapshot"
    );
}

// ---- questions (protocol 0.24) --------------------------------------------------------------

fn pending_questions(
    model: &Entity<ChatModel>,
    cx: &VisualTestContext,
) -> Vec<hutan_core::QuestionId> {
    transcript(model, cx, |t| {
        t.pending_questions().iter().map(|q| q.id).collect()
    })
}

fn question_state(
    model: &Entity<ChatModel>,
    cx: &VisualTestContext,
    id: hutan_core::QuestionId,
) -> hutan_chat_view::QuestionState {
    transcript(model, cx, |t| t.question(id).unwrap().state.clone())
}

#[gpui::test]
fn a_question_card_is_filled_with_clicks_and_keys_and_submitted_with_cmd_enter(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("question-user-input"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    say(cx, "ask me");
    pump(cx, "the question to be pending", |cx| {
        !pending_questions(&model, cx).is_empty()
    });
    let id = pending_questions(&model, cx)[0];
    // The session says it waits for the user, and says why.
    wait_until(cx, "the session to wait", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .and_then(|s| s.chat.as_ref().map(|c| c.pending_questions))
                == Some(1)
        })
    });
    pump(cx, "the card", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatQuestion(0)).is_some()
    });

    // The card's rhythm and buttons (CL-POLISH-4): the options are full-width rows in one
    // column (radio on the left, description inside), the groups are 16 px apart, and Decline is
    // the only way out besides Submit: no Cancel.
    let at = |cx: &VisualTestContext, n: usize| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::ChatQuestion(n)))
    };
    let (first, second) = (at(cx, 100).unwrap(), at(cx, 101).unwrap());
    assert_eq!(first.left(), second.left(), "one column of options");
    assert_eq!(first.size.width, second.size.width, "full-width rows");
    assert!(
        second.top() >= first.bottom() && second.top() - first.bottom() <= gpui::px(4.0),
        "the options of one field are stacked closely: {first:?} {second:?}"
    );
    let note = at(cx, 8 + 1).expect("the note field");
    assert!(
        note.top() - second.bottom() >= gpui::px(16.0),
        "16 px between the groups: {second:?} then {note:?}"
    );
    assert!(at(cx, 1).is_some(), "Decline");
    assert!(at(cx, 2).is_none(), "no Cancel next to Decline");

    // An incomplete form is refused here, with the reason on the card; nothing is sent.
    click(&view, cx, Region::ChatQuestion(0));
    pump(cx, "the refusal", |cx| {
        draw(cx);
        chat.read_with(cx, |c, _| c.question_error(id).is_some())
    });
    assert!(env.received().iter().all(|(m, _)| m != "response"));

    // Choose an option, type into the note field (digits are text there, never an approval's
    // answer), Tab into the secret field and type it; ⌘↩ submits.
    click(&view, cx, Region::ChatQuestion(100));
    click(&view, cx, Region::ChatQuestion(8 + 1));
    cx.simulate_input("none1");
    cx.simulate_keystrokes("tab");
    cx.simulate_input("hunter2");
    cx.simulate_keystrokes("cmd-enter");
    pump(cx, "the answer", |cx| {
        question_state(&model, cx, id) != hutan_chat_view::QuestionState::Pending
            || chat.read_with(cx, |c, _| c.question_error(id).is_some())
    });
    let error = chat.read_with(cx, |c, _| c.question_error(id));
    let state = question_state(&model, cx, id);
    assert!(error.is_none(), "the card says: {error:?}");
    let hutan_chat_view::QuestionState::Answered { values, withheld } = state else {
        panic!("{state:?}")
    };
    assert_eq!(values["scope"], "Only the CLI");
    assert_eq!(values["note"], "none1");
    assert_eq!(withheld, ["token"], "the secret is not in the transcript");
    // Answering clears the question's own attention entry (the store notices a change of the
    // daemon's question list even when no session or tree changed).
    pump(cx, "the question to leave the attention list", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .attention_entries()
                .iter()
                .all(|e| e.kind != hutan_desktop::sessions::AttentionKind::Question)
        })
    });
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    let response = env
        .received()
        .into_iter()
        .find(|(m, _)| m == "response")
        .expect("the answer reached the agent");
    assert_eq!(
        response.1["result"]["answers"]["token"],
        json!({"answers": ["hunter2"]})
    );
    assert!(!format!("{:?}", model.read_with(cx, |m, _| m.transcript().len())).is_empty());
    // The daemon coalesces tree changes for `hutan_daemon::tree::EVENT_INTERVAL` (250 ms): the
    // node leaves `Blocked` a moment after the turn ended, and with it the attention entry.
    // Answering the question itself removes its `Question` entry at once.
    pump(cx, "the attention entry to clear", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attention_entries())
            .is_empty()
    });
}

#[gpui::test]
fn a_question_can_be_declined_from_its_card_and_counts_as_attention_while_it_waits(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::sessions::AttentionKind;

    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("question-elicitation"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let (_chat, model) = open_chat(&view, cx, info.id);
    say(cx, "deploy");
    pump(cx, "the question to be pending", |cx| {
        !pending_questions(&model, cx).is_empty()
    });
    let id = pending_questions(&model, cx)[0];
    // It counts for the badge and the menu bar like an approval does.
    wait_until(cx, "the attention entry", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .attention_entries()
                .iter()
                .any(|e| e.session == info.id && e.kind == AttentionKind::Question)
        })
    });
    // 1–4 answer approvals only: with a question pending they do nothing.
    cx.simulate_input("1");
    assert_eq!(
        question_state(&model, cx, id),
        hutan_chat_view::QuestionState::Pending
    );
    pump(cx, "the card", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatQuestion(1)).is_some()
    });
    click(&view, cx, Region::ChatQuestion(1));
    pump(cx, "the decline", |cx| {
        question_state(&model, cx, id) == hutan_chat_view::QuestionState::Declined
    });
    pump(cx, "the turn to end", |cx| {
        count(&model, cx, is_footer) == 1
    });
    let response = env
        .received()
        .into_iter()
        .find(|(m, _)| m == "response")
        .unwrap();
    assert_eq!(
        response.1["result"],
        json!({"action": "decline", "content": null})
    );
    wait_until(cx, "the attention to clear", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).attention_entries().is_empty()
        })
    });
}

// ---- the send key in the other message inputs -------------------------------------------------

#[gpui::test]
fn the_enter_send_key_submits_a_question_card_from_its_last_field(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("question-user-input"));
    let info = daemon.create_chat(&env.cwd());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[chat]\nsend_key = \"enter\"\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    let (chat, model) = open_chat(&view, cx, info.id);
    // ⌘↩ still sends under the Enter key.
    say(cx, "ask me");
    pump(cx, "the question to be pending", |cx| {
        !pending_questions(&model, cx).is_empty()
    });
    let id = pending_questions(&model, cx)[0];
    pump(cx, "the card", |cx| {
        draw(cx);
        region(&view, cx, Region::ChatQuestion(0)).is_some()
    });

    // The note field is not the last: Return moves on to the token field and submits nothing.
    click(&view, cx, Region::ChatQuestion(100));
    click(&view, cx, Region::ChatQuestion(8 + 1));
    cx.simulate_input("none1");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("hunter2");
    assert_eq!(
        question_state(&model, cx, id),
        hutan_chat_view::QuestionState::Pending
    );
    assert!(env.received().iter().all(|(m, _)| m != "response"));
    // Return in the last field submits.
    cx.simulate_keystrokes("enter");
    pump(cx, "the answer", |cx| {
        question_state(&model, cx, id) != hutan_chat_view::QuestionState::Pending
            || chat.read_with(cx, |c, _| c.question_error(id).is_some())
    });
    let error = chat.read_with(cx, |c, _| c.question_error(id));
    assert!(error.is_none(), "the card says: {error:?}");
    let hutan_chat_view::QuestionState::Answered { values, .. } = question_state(&model, cx, id)
    else {
        panic!("not answered")
    };
    assert_eq!(values["note"], "none1");
}

#[gpui::test]
fn the_start_sheet_prompt_follows_the_send_key_and_its_hint_switches_it(cx: &mut TestAppContext) {
    use hutan_desktop::settings::SendKey;
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let (view, cx) = open(cx, &daemon);
    cx.simulate_keystrokes("cmd-shift-t");
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(hutan_desktop::sessions::launch::LaunchKind::Chat, cx);
    });
    pump(cx, "Codex to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "codex" && h.enabled())
                && wb
                    .launcher()
                    .is_some_and(|l| l.agent() == &LaunchAgent::Harness("codex".into()))
                && matches!(wb.launcher_models(cx), Models::Ready(_))
        })
    });
    let prompt = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.launcher().unwrap().prompt().to_owned())
    };
    let asking = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.launcher().unwrap().confirm().is_some())
    };
    click(&view, cx, Region::Launcher("prompt"));
    cx.simulate_input("Say hello");

    // ⌘Return mode: Return and ⇧Return add lines; nothing starts.
    cx.simulate_keystrokes("enter");
    cx.simulate_input("x");
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(prompt(cx), "Say hello\nx\n");
    assert!(!asking(cx));

    // Under the Enter key, in the sheet that is already open: ⇧Return and ⌥Return add lines,
    // Return starts (the first start of this agent and model asks before it goes).
    set_send_key(&view, cx, "enter");
    cx.simulate_keystrokes("alt-enter");
    assert_eq!(prompt(cx), "Say hello\nx\n\n");
    assert!(!asking(cx));
    cx.simulate_keystrokes("enter");
    assert!(asking(cx), "Return started it");
    assert_eq!(prompt(cx), "Say hello\nx\n\n");
    cx.simulate_keystrokes("escape");
    assert!(!asking(cx));

    // The hint under the prompt is the same quick switch.
    click(&view, cx, Region::Launcher("send-key"));
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_some()));
    click(&view, cx, Region::MenuEntry(0));
    assert_eq!(view.read_with(cx, |wb, _| wb.send_key()), SendKey::CmdEnter);
    assert!(
        view.read_with(cx, |wb, _| wb.launcher().is_some()),
        "the sheet stays open"
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        prompt(cx),
        "Say hello\nx\n\n\n",
        "the sheet has the keys back"
    );
    assert!(!asking(cx));
}
