#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The assistant panel and its explicit context (M16-B), against a real in-process daemon with
//! the in-memory credential store and a loopback OpenAI-compatible server standing in for the
//! provider. Terminals are real PTYs with OSC 133 blocks, files are real files, the history
//! index reads synthetic transcripts in temporary directories.

use crate::common;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::assistant::{
    AssistantEnv, Options, PATH, eventually, last_prompt, open, open_assistant, panel, pump_until,
    reply, shown, texts, wait_answer,
};
use common::history::{HistoryEnv, OMP_ID};
use common::{TestDaemon, draw, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_chat_view::Body;
use hutan_core::TerminalId;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::chat::{ChatModel, ChatView, ContextChip, MentionKind};
use hutan_desktop::workbench::model::PanelId;
use hutan_desktop::workbench::panels::{AssistantChoice, Failure, HistoryPanel, Opened, ScopeKey};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::assistant::{AssistantList, AssistantListRequest, Attachment, AttachmentKind};
use hutan_protocol::blocks::BlockState;
use hutan_protocol::history::HistoryStatusInfo;
use hutan_protocol::messages::{Empty, Request, SessionInfo};

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
}

/// Clicks the remove button (the right edge) of the composer's context chip `index`.
fn remove_chip(view: &Entity<Workbench>, cx: &mut VisualTestContext, index: usize) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::ChatContextChip(index)))
        .unwrap_or_else(|| panic!("chip {index} is drawn"));
    let at = gpui::point(bounds.right() - gpui::px(8.0), bounds.center().y);
    cx.simulate_click(at, Modifiers::none());
}

fn session_infos(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<SessionInfo> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().to_vec())
}

fn type_message(model: &Entity<ChatModel>, cx: &mut VisualTestContext, text: &str) {
    model.update(cx, |m, cx| {
        m.edit(
            |input| {
                input.clear();
                input.insert(text);
            },
            cx,
        );
    });
}

/// Sends `text` from the composer of `chat` (with whatever chips it holds).
fn send(
    model: &Entity<ChatModel>,
    chat: &Entity<ChatView>,
    cx: &mut VisualTestContext,
    text: &str,
) {
    type_message(model, cx, text);
    assert!(chat.update(cx, |v, cx| v.send(cx)), "the message was sent");
}

fn context_labels(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<String> {
    model.read_with(cx, |m, _| {
        m.context().iter().map(ContextChip::label).collect()
    })
}

fn context_attachments(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<Attachment> {
    model.read_with(cx, |m, _| {
        m.context().iter().map(|c| c.attachment.clone()).collect()
    })
}

fn error_of(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Option<String> {
    model.read_with(cx, |m, _| m.error().map(ToString::to_string))
}

/// `printf` marks of one finished command, as a shell integration writes them.
fn finished(command: &str, output: &str, code: i32) -> String {
    format!(
        "printf '\\033]133;A\\007$ \\033]133;B\\007{command}\\r\\n\\033]133;C\\007{output}\\r\\n\\033]133;D;{code}\\007'; "
    )
}

/// A terminal session in `workspace` whose screen holds `cmd1`, `cmd2` and `cmd3` as finished
/// blocks, the third with a line nobody attaches.
fn terminal_with_blocks(
    daemon: &TestDaemon,
    workspace: hutan_core::WorkspaceId,
) -> (SessionInfo, TerminalId) {
    let script = format!(
        "{}{}{}printf '\\033]133;A\\007$ \\033]133;B\\007'; sleep 600",
        finished("cmd1", "out1", 1),
        finished("cmd2", "out2", 0),
        finished("cmd3", "out3 UNATTACHED-SHELL", 0),
    );
    let info = daemon.create_session(&["sh", "-c", &script], Some(workspace));
    let terminal = info.terminal_id.expect("a terminal");
    eventually("three finished blocks", || {
        daemon
            .blocks(terminal)
            .iter()
            .filter(|b| b.state == BlockState::Finished)
            .count()
            == 3
    });
    (info, terminal)
}

fn block_id(daemon: &TestDaemon, terminal: TerminalId, command: &str) -> u64 {
    daemon
        .blocks(terminal)
        .into_iter()
        .find(|b| b.command.as_deref().is_some_and(|c| c.contains(command)))
        .unwrap_or_else(|| panic!("the daemon has the block of {command}"))
        .id
}

fn user_items(model: &Entity<ChatModel>, cx: &VisualTestContext) -> Vec<hutan_core::ItemId> {
    model.read_with(cx, |m, _| {
        m.transcript()
            .entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::User(msg) => msg.item,
                _ => None,
            })
            .collect()
    })
}

// ---- scenario 1: the global assistant, streaming, scopes ----------------------------------------

/// Picks row `index` (0 Global, 1 the workspace) of the header's scope dropdown.
fn choose_scope(view: &Entity<Workbench>, cx: &mut VisualTestContext, index: usize) {
    click(view, cx, Region::Assistant("scope"));
    let labels = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    assert_eq!(labels.len(), 2, "Global and the workspace");
    assert_eq!(labels[0].0.as_ref(), "Global");
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

#[gpui::test]
fn the_global_assistant_streams_without_a_terminal_and_each_scope_keeps_its_conversation(
    cx: &mut TestAppContext,
) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("global reply"));
    env.mock.route("POST", PATH, reply("project reply"));
    env.mock.route("POST", PATH, reply("global again"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    let sessions_before = session_infos(&view, cx).len();

    // The panel is registered, in ⌘K, and its action opens it.
    assert!(view.read_with(cx, |wb, _| {
        wb.panel_registry()
            .get(&PanelId::new("assistant"))
            .is_some()
    }));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("Mawas: Open");
    assert!(view.read_with(cx, |wb, _| {
        wb.palette_titles()
            .iter()
            .any(|t| t.as_ref() == "Mawas: Open")
    }));
    cx.simulate_keystrokes("escape");
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenAssistant);
    let panel = open_assistant(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(hutan_desktop::workbench::model::ItemRef::panel("assistant")),
        "Mawas Home is a tab of the center"
    );

    // Home is active: the global assistant, a direct chat of the configured provider — and no
    // terminal, no vendor agent.
    assert_eq!(panel.read_with(cx, |p, cx| p.key(cx)), ScopeKey::Global);
    let (global, global_model, global_chat) = shown(&panel, cx);
    let infos = session_infos(&view, cx);
    assert_eq!(
        infos.len(),
        sessions_before + 1,
        "only the assistant session"
    );
    assert!(infos.iter().all(|s| s.terminal_id.is_none()), "no PTY");
    let info = infos.iter().find(|s| s.id == global).unwrap();
    let chat = info.chat.as_ref().expect("a chat");
    assert_eq!(chat.agent, "assistant");
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(chat.model, "m-1");

    // Typing reaches the composer; Send streams the answer into the transcript.
    cx.simulate_input("what is hutan?");
    assert_eq!(
        global_model.read_with(cx, |m, _| m.input().text().to_owned()),
        "what is hutan?"
    );
    cx.simulate_keystrokes("cmd-enter");
    wait_answer(&global_model, cx, "global reply");
    assert_eq!(last_prompt(&env.mock), "what is hutan?");
    assert!(
        session_infos(&view, cx)
            .iter()
            .all(|s| s.terminal_id.is_none())
    );

    // A project workspace: its own assistant, its own transcript.
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the project's assistant", |cx| {
        panel.read_with(cx, |p, cx| {
            p.key(cx) == ScopeKey::Workspace(workspace.id) && p.session(cx).is_some()
        })
    });
    let (project, project_model, project_chat) = shown(&panel, cx);
    assert_ne!(project, global);
    let project_info = session_infos(&view, cx)
        .into_iter()
        .find(|s| s.id == project)
        .unwrap();
    assert_eq!(project_info.workspace_id, workspace.id);
    assert_eq!(project_info.chat.as_ref().unwrap().agent, "assistant");
    assert!(
        texts(&project_model, cx).is_empty(),
        "nothing of the global conversation came along"
    );
    send(
        &project_model,
        &project_chat,
        cx,
        "what is in this project?",
    );
    wait_answer(&project_model, cx, "project reply");
    assert_eq!(last_prompt(&env.mock), "what is in this project?");

    // A chip and a draft of the project do not move to the global assistant.
    project_model.update(cx, |m, cx| {
        m.attach_context(ContextChip::file("src/lib.rs", None), cx);
    });
    type_message(&project_model, cx, "unsent project draft");
    choose_scope(&view, cx, 0);
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(global));
    assert_eq!(
        texts(&global_model, cx),
        [
            (true, "what is hutan?".to_owned()),
            (false, "global reply".to_owned())
        ],
        "the global conversation is as it was"
    );
    assert!(context_labels(&global_model, cx).is_empty());
    assert_eq!(
        global_model.read_with(cx, |m, _| m.input().text().to_owned()),
        "",
        "the draft stayed with its scope"
    );
    send(&global_model, &global_chat, cx, "one more thing");
    wait_answer(&global_model, cx, "global again");
    // The project's conversation, chip and draft are where they were.
    assert_eq!(
        texts(&project_model, cx),
        [
            (true, "what is in this project?".to_owned()),
            (false, "project reply".to_owned())
        ]
    );
    assert_eq!(context_labels(&project_model, cx).len(), 1);
    assert_eq!(
        project_model.read_with(cx, |m, _| m.input().text().to_owned()),
        "unsent project draft"
    );
    choose_scope(&view, cx, 1);
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(project));

    // The daemon has exactly two assistants, one per scope, and never started a terminal.
    let list: AssistantList = env
        .daemon
        .call(Request::AssistantList(AssistantListRequest::default()));
    assert_eq!(list.assistants.len(), 2);
    assert!(
        session_infos(&view, cx)
            .iter()
            .all(|s| s.terminal_id.is_none())
    );
    assert_eq!(env.mock.requests_to(PATH).len(), 3);
}

#[gpui::test]
fn home_is_a_center_tab_that_keeps_its_conversation(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let active =
        |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.state().active_center_item());
    assert_eq!(
        active(cx),
        Some(hutan_desktop::workbench::model::ItemRef::panel("assistant"))
    );
    let before = panel.read_with(cx, |p, cx| p.session(cx));
    assert!(before.is_some());

    // Another tab replaces it, and the nav brings it back with the same conversation.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(
                pane,
                hutan_desktop::workbench::model::ItemRef::placeholder("Elsewhere"),
            )
            .unwrap();
        });
    });
    assert_ne!(
        active(cx),
        Some(hutan_desktop::workbench::model::ItemRef::panel("assistant"))
    );
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    draw(cx);
    assert_eq!(
        active(cx),
        Some(hutan_desktop::workbench::model::ItemRef::panel("assistant"))
    );
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), before);
}

// ---- scenario 2: context chips, add / remove, only what was selected --------------------------

#[gpui::test]
fn every_kind_of_chip_adds_and_removes_and_only_the_selected_sources_reach_the_prompt(
    cx: &mut TestAppContext,
) {
    let history = HistoryEnv::new();
    history.write_fixtures();
    let env = AssistantEnv::start(Options {
        configured: true,
        assistant: "context_budget_chars = 500",
        history: Some(&history),
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("answer one"));
    env.mock.route("POST", PATH, reply("answer two"));
    env.mock.route("POST", PATH, reply("answer three"));
    let root = env.project("project");
    fs::write(
        root.join("src/lib.rs"),
        "line one\nline two\nline three\nline four\n",
    )
    .unwrap();
    fs::write(root.join("src/secret.txt"), "UNATTACHED-FILE-MARKER\n").unwrap();
    fs::write(root.join("src/big.txt"), "abcdefghij\n".repeat(200)).unwrap();
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (terminal_session, terminal) = terminal_with_blocks(&env.daemon, workspace.id);
    let cmd2 = block_id(&env.daemon, terminal, "cmd2");
    eventually("the transcripts to be indexed", || {
        let status: HistoryStatusInfo = env.daemon.call(Request::HistoryStatus(Empty {}));
        status.enabled && status.sources.iter().map(|s| s.rows).sum::<u64>() >= 10
    });

    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);
    assert!(chat.read_with(cx, |v, cx| v.supports_context(cx)));
    assert_eq!(
        chat.read_with(cx, |v, cx| v.mention(cx)),
        None,
        "no mention in an empty draft"
    );

    // @shell: the mention in the composer opens the terminal form; Recent output attaches the
    // lines the field asks for.
    cx.simulate_input("@shell");
    assert_eq!(
        chat.read_with(cx, |v, cx| v.mention(cx).map(|m| m.kind)),
        Some(MentionKind::Shell)
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(
        chat.read_with(cx, |v, _| v.attach_form()),
        Some(MentionKind::Shell)
    );
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "",
        "the mention left the draft"
    );
    cx.simulate_input("3");
    assert_eq!(
        chat.read_with(cx, |v, _| v.attach_arg()),
        Some("3".to_owned())
    );
    click(&view, cx, Region::Attach("recent"));
    assert!(!chat.read_with(cx, |v, _| v.attach_open()));
    assert_eq!(
        context_attachments(&model, cx),
        [Attachment::ShellRecent {
            terminal_id: terminal,
            lines: 3
        }]
    );
    assert!(
        context_labels(&model, cx)[0]
            .ends_with(&format!("{} · last 3 lines", terminal_session.title)),
        "{:?}",
        context_labels(&model, cx)
    );

    // The Attach menu → shell → a block of the terminal.
    click(&view, cx, Region::Attach("button"));
    click(&view, cx, Region::Attach("shell"));
    assert_eq!(
        chat.read_with(cx, |v, _| v.attach_form()),
        Some(MentionKind::Shell)
    );
    click(&view, cx, Region::Attach("blocks"));
    wait_until(cx, "the blocks to be listed", |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Attach("block")))
            .is_some()
    });
    assert!(chat.update(cx, |v, cx| v.attach_block(terminal, cmd2, cx)));
    let labels = context_labels(&model, cx);
    assert_eq!(labels.len(), 2);
    assert!(
        labels[1].contains("$ cmd2") && labels[1].contains("exit 0"),
        "{labels:?}"
    );

    // @omp with a filter: indexed transcripts of OMP; the row attaches the session's last turns.
    cx.simulate_input("@omp axolotl");
    cx.simulate_keystrokes("tab");
    assert_eq!(
        chat.read_with(cx, |v, _| v.attach_form()),
        Some(MentionKind::Omp)
    );
    assert_eq!(
        chat.read_with(cx, |v, _| v.attach_arg()),
        Some("axolotl".to_owned())
    );
    pump_until(cx, "the indexed OMP session", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| wb.region_bounds(Region::Attach("transcript")))
            .is_some()
    });
    click(&view, cx, Region::Attach("transcript"));
    assert_eq!(
        context_attachments(&model, cx)[2],
        Attachment::AgentTranscript {
            session_id: OMP_ID.to_owned(),
            last_turns: 5
        }
    );

    // The menu → file, typed into the form's field.
    click(&view, cx, Region::Attach("button"));
    click(&view, cx, Region::Attach("file"));
    cx.simulate_input("src/lib.rs:2-3");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        context_labels(&model, cx)[3],
        "file · src/lib.rs · lines 2-3"
    );

    // @search with its words: a chip at once.
    cx.simulate_input("@search hutantest");
    cx.simulate_keystrokes("tab");
    assert!(!chat.read_with(cx, |v, _| v.attach_open()));
    assert_eq!(
        context_labels(&model, cx)[4],
        "search · “hutantest” · 5 hits"
    );
    assert_eq!(model.read_with(cx, |m, _| m.input().text().to_owned()), "");
    assert_eq!(context_labels(&model, cx).len(), 5);

    // Every chip says its kind, its source and the bounds asked for, and is removable.
    for chip in model.read_with(cx, |m, _| m.context().to_vec()) {
        assert!(!chip.source.is_empty() && !chip.bounds.is_empty());
    }
    remove_chip(&view, cx, 0);
    assert_eq!(context_labels(&model, cx).len(), 4);
    assert!(
        matches!(
            context_attachments(&model, cx)[0],
            Attachment::ShellBlock { .. }
        ),
        "the recent-output chip is gone"
    );
    // Keep the block and the file only.
    model.update(cx, |m, cx| {
        m.remove_context(3, cx);
        m.remove_context(1, cx);
    });
    assert_eq!(
        context_attachments(&model, cx),
        [
            Attachment::ShellBlock {
                terminal_id: terminal,
                block_id: cmd2
            },
            Attachment::File {
                path: "src/lib.rs".to_owned(),
                range: Some(hutan_protocol::assistant::LineRange { start: 2, end: 3 }),
                picked: false
            }
        ]
    );

    // Nothing was read yet: the provider has not been called.
    assert!(env.mock.requests_to(PATH).is_empty());
    send(&model, &chat, cx, "why did cmd2 print that?");
    wait_answer(&model, cx, "answer one");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.starts_with("why did cmd2 print that?"), "{prompt}");
    assert!(prompt.contains("[shell: cmd2"), "{prompt}");
    assert!(prompt.contains("out2"), "{prompt}");
    assert!(prompt.contains("[file: src/lib.rs · lines 2-3"), "{prompt}");
    assert!(
        prompt.contains("line two") && prompt.contains("line three"),
        "{prompt}"
    );
    for absent in [
        "out1",
        "out3",
        "UNATTACHED",
        "line one",
        "line four",
        "[agent:",
        "[search:",
    ] {
        assert!(
            !prompt.contains(absent),
            "`{absent}` was not attached: {prompt}"
        );
    }
    assert!(model.read_with(cx, |m, _| m.context().is_empty()));
    // The daemon's own description of each attachment is shown with the message.
    let first = user_items(&model, cx)[0];
    let meta = model.read_with(cx, |m, _| m.attachments_of(first).to_vec());
    assert_eq!(
        meta.iter().map(|m| m.kind).collect::<Vec<_>>(),
        [AttachmentKind::ShellBlock, AttachmentKind::File]
    );
    assert!(meta.iter().all(|m| m.chars > 0 && !m.truncated), "{meta:?}");

    // The other three kinds, alone: terminal text, a transcript and a search.
    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::recent("t", terminal, 3), cx);
        m.attach_context(ContextChip::transcript("OMP", OMP_ID, 2), cx);
        m.attach_context(ContextChip::search("hutantest", 3), cx);
    });
    send(&model, &chat, cx, "and the others?");
    wait_answer(&model, cx, "answer two");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.contains("[shell: terminal"), "{prompt}");
    assert!(prompt.contains("[agent:"), "{prompt}");
    assert!(prompt.contains("[search: \"hutantest\""), "{prompt}");
    assert!(
        !prompt.contains("[file:") && !prompt.contains("[shell: cmd2"),
        "{prompt}"
    );

    // Truncation by the daemon's budget is reported and shown.
    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::file("src/big.txt", None), cx);
    });
    send(&model, &chat, cx, "summarise the big file");
    wait_answer(&model, cx, "answer three");
    let last = *user_items(&model, cx).last().unwrap();
    let meta = model.read_with(cx, |m, _| m.attachments_of(last).to_vec());
    assert_eq!(meta.len(), 1);
    assert!(meta[0].truncated, "{meta:?}");
    assert!(meta[0].chars < 2200, "{meta:?}");
    draw(cx);
}

// ---- scenario 2b: permission refusals ---------------------------------------------------------

#[gpui::test]
fn a_refused_attachment_is_named_marked_and_nothing_is_sent(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        tables: "[assistant.context]\nshell = false\n",
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("fine"));
    let root = env.project("project");
    fs::write(root.join("src/lib.rs"), "fn lib() {}\n").unwrap();
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (_, terminal) = terminal_with_blocks(&env.daemon, workspace.id);
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);

    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::recent("t", terminal, 5), cx);
        m.attach_context(ContextChip::file("src/lib.rs", None), cx);
    });
    send(&model, &chat, cx, "look at my terminal");
    wait_until(cx, "the refusal", |cx| error_of(&model, cx).is_some());
    let error = error_of(&model, cx).unwrap();
    assert!(error.starts_with("Not allowed:"), "{error}");
    assert!(error.contains("assistant.context.shell"), "{error}");
    assert!(error.contains("daemon.toml"), "{error}");
    assert!(
        env.mock.requests_to(PATH).is_empty(),
        "nothing reached the provider"
    );
    assert!(texts(&model, cx).is_empty(), "nothing was journaled");
    // The draft and both chips are back; the shell chip says it was refused, the file one not.
    assert_eq!(
        model.read_with(cx, |m, _| m.input().text().to_owned()),
        "look at my terminal"
    );
    let refused = model.read_with(cx, |m, _| {
        m.context()
            .iter()
            .map(|c| c.refused.is_some())
            .collect::<Vec<_>>()
    });
    assert_eq!(refused, [true, false]);
    draw(cx);

    // Without the refused chip the message goes through.
    model.update(cx, |m, cx| m.remove_context(0, cx));
    assert!(chat.update(cx, |v, cx| v.send(cx)));
    wait_answer(&model, cx, "fine");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.contains("[file: src/lib.rs"), "{prompt}");
    assert!(prompt.contains("fn lib()"), "{prompt}");
    assert!(!prompt.contains("[shell:"), "{prompt}");
}

#[gpui::test]
fn the_global_assistant_may_not_read_files_unless_the_daemon_says_so(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);
    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::file("notes.txt", None), cx);
    });
    send(&model, &chat, cx, "read my notes");
    wait_until(cx, "the refusal", |cx| error_of(&model, cx).is_some());
    let error = error_of(&model, cx).unwrap();
    assert!(error.contains("assistant.global_context.files"), "{error}");
    assert!(env.mock.requests_to(PATH).is_empty());
}

// ---- scenario 3: files cannot escape the workspace roots --------------------------------------

#[gpui::test]
fn a_file_chip_cannot_escape_the_workspace_roots_and_the_desktop_never_opens_the_file(
    cx: &mut TestAppContext,
) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("inside only"));
    let root = env.project("project");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/inside-only.txt"), "fn inside() {}\n").unwrap();
    // Outside the root: a sibling file, a sibling directory that has the root's name as a
    // prefix, and symlinks inside the root that lead out.
    fs::write(env.base.join("outside.txt"), "OUTSIDE-MARKER\n").unwrap();
    fs::create_dir_all(env.base.join("project-evil")).unwrap();
    fs::write(env.base.join("project-evil/loot.txt"), "OUTSIDE-MARKER\n").unwrap();
    symlink(env.base.join("outside.txt"), root.join("link-out.txt")).unwrap();
    symlink(&env.base, root.join("link-dir")).unwrap();
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);

    let outside = env.base.join("outside.txt").display().to_string();
    let escapes = [
        "../outside.txt".to_owned(),
        outside,
        "link-out.txt".to_owned(),
        "link-dir/outside.txt".to_owned(),
        "../project-evil/loot.txt".to_owned(),
        "/etc/hosts".to_owned(),
    ];
    for path in escapes {
        // Through the mention, like a user would.
        type_message(&model, cx, &format!("@file {path}"));
        cx.simulate_keystrokes("tab");
        assert_eq!(
            context_labels(&model, cx),
            [format!("file · {path} · whole file")]
        );
        send(&model, &chat, cx, "read it");
        wait_until(cx, &format!("the refusal of {path}"), |cx| {
            error_of(&model, cx).is_some()
        });
        let error = error_of(&model, cx).unwrap();
        assert!(error.starts_with("Not allowed:"), "{path}: {error}");
        assert!(
            model.read_with(cx, |m, _| m.context()[0].refused.is_some()),
            "{path} is marked refused"
        );
        model.update(cx, |m, cx| {
            m.remove_context(0, cx);
            m.clear_error(cx);
        });
        type_message(&model, cx, "");
    }
    assert!(
        env.mock.requests_to(PATH).is_empty(),
        "no escape reached the provider"
    );

    // A path inside the root works. It is relative to the workspace's first folder, which is
    // not this process's directory: the daemon found it, the desktop never opened it.
    assert!(!Path::new("notes/inside-only.txt").exists());
    type_message(&model, cx, "@file notes/inside-only.txt:1-1");
    cx.simulate_keystrokes("tab");
    send(&model, &chat, cx, "what does it say?");
    wait_answer(&model, cx, "inside only");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.contains("fn inside()"), "{prompt}");
    assert!(!prompt.contains("OUTSIDE-MARKER"), "{prompt}");
}

// ---- scenario 4: search a persisted conversation, open the hit, continue it -------------------

#[gpui::test]
fn a_history_hit_of_an_assistant_conversation_opens_it_in_the_panel_and_continues_it(
    cx: &mut TestAppContext,
) {
    let history = HistoryEnv::new();
    let env = AssistantEnv::start(Options {
        configured: true,
        history: Some(&history),
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("quartz answer"));
    env.mock.route("POST", PATH, reply("continued"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (view, cx) = open(cx, &env.daemon);

    let panel = open_assistant(&view, cx);
    let (global, global_model, global_chat) = shown(&panel, cx);
    send(
        &global_model,
        &global_chat,
        cx,
        "remember the quokkaquartz protocol",
    );
    wait_answer(&global_model, cx, "quartz answer");
    // The chat journal feeds the index as source `hutan`, keyed by the session id.
    eventually("the conversation to be indexed", || {
        let hits: hutan_protocol::history::HistoryHits = env.daemon.call(Request::HistorySearch(
            hutan_protocol::history::HistorySearch {
                query: "quokkaquartz".to_owned(),
                limit: 5,
                sources: Vec::new(),
                roles: Vec::new(),
                project: None,
                since_ms: None,
                until_ms: None,
            },
        ));
        hits.hits.iter().any(|h| h.session_id == Some(global))
    });

    // Another scope is on screen; the History panel finds the conversation and opens it.
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    let panel = open_assistant(&view, cx);
    let project_session = panel.read_with(cx, |p, cx| p.session(cx)).unwrap();
    assert_ne!(project_session, global);
    cx.dispatch_action(actions::OpenPanel("history".into()));
    draw(cx);
    let history_panel: Entity<HistoryPanel> = view
        .read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn");
    cx.dispatch_action(actions::SearchHistory);
    // The keyboard is in the search field at once, also while the panel still reads the index
    // state (`HistoryPanel::focus_search`): type right after one frame.
    draw(cx);
    cx.simulate_input("quokkaquartz");
    assert_eq!(
        history_panel.read_with(cx, |p, _| p.query().to_owned()),
        "quokkaquartz",
        "the typed text lands in the search field"
    );
    pump_until(cx, "the hit", |cx| {
        history_panel.read_with(cx, |p, _| !p.searching() && !p.hits().is_empty())
    });
    let hit = history_panel.read_with(cx, |p, _| p.hits()[0].clone());
    assert_eq!(hit.session_id, Some(global));
    assert!(hit.resume_argv.is_none());
    let before = session_infos(&view, cx).len();
    cx.simulate_keystrokes("down enter");
    pump_until(cx, "the assistant to show the conversation", |cx| {
        panel.read_with(cx, |p, cx| p.session(cx)) == Some(global)
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.choice()),
        AssistantChoice::Global
    );
    assert_eq!(
        session_infos(&view, cx).len(),
        before,
        "nothing was started: no terminal, no new chat"
    );
    assert!(
        session_infos(&view, cx)
            .iter()
            .all(|s| s.terminal_id.is_none())
    );
    assert!(
        view.read_with(cx, |wb, _| wb.chat_view_of(global))
            .is_none(),
        "it did not open as a tab"
    );
    let (shown_session, model, chat) = shown(&panel, cx);
    assert_eq!(shown_session, global);
    wait_until(cx, "the persisted conversation", |cx| {
        texts(&model, cx).len() == 2
    });
    assert_eq!(
        texts(&model, cx)[0],
        (true, "remember the quokkaquartz protocol".to_owned())
    );

    // Continue it: the provider sees the earlier turns, and it is still the one assistant.
    send(&model, &chat, cx, "and then?");
    wait_answer(&model, cx, "continued");
    let request = env.mock.requests_to(PATH).last().unwrap().json();
    let roles: Vec<(String, String)> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap().to_owned(),
                m["content"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(roles.len(), 4, "{roles:?}");
    assert_eq!(roles[1].1, "remember the quokkaquartz protocol");
    assert_eq!(roles[2].1, "quartz answer");
    assert_eq!(roles[3].1, "and then?");
    let list: AssistantList = env
        .daemon
        .call(Request::AssistantList(AssistantListRequest {
            scope: Some(hutan_protocol::assistant::AssistantScope::Global),
            workspace_id: None,
        }));
    assert_eq!(list.assistants.len(), 1);
    // The same conversation is also what the History panel opens a second time.
    let again = actions::OpenHistoryHit(hutan_desktop::workbench::panels::HutanTarget {
        session: global.to_string(),
        block: None,
    });
    cx.dispatch_action(again);
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), Some(global));
    assert_eq!(
        panel.read_with(cx, |p, _| p.choice()),
        AssistantChoice::Global
    );
}

// ---- scenario 5: the prerequisite ---------------------------------------------------------------

#[gpui::test]
fn an_assistant_with_nothing_ready_names_the_prerequisite_and_never_calls_a_model(
    cx: &mut TestAppContext,
) {
    // No harness is ready (the test daemon's OMP, Claude Code and Codex do not exist) and the
    // only provider is removed again: nothing may answer, and nothing is called.
    let env = AssistantEnv::start(Options::default());
    let _: Empty = env.daemon.call(Request::ProviderRemove(
        hutan_protocol::provider::ProviderRef {
            provider_id: "mock".into(),
        },
    ));
    env.mock.route("POST", PATH, reply("must not be asked"));
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| wb.open_assistant(window, cx));
    let panel = panel(&view, cx);
    wait_until(cx, "the daemon's answer", |cx| {
        panel.read_with(cx, |p, _| {
            matches!(p.state(ScopeKey::Global), Opened::Failed(_))
        })
    });
    let state = panel.read_with(cx, |p, _| p.state(ScopeKey::Global));
    let Opened::Failed(Failure::Unconfigured(message)) = state else {
        panic!("not the prerequisite: {state:?}");
    };
    assert!(
        message.contains("[assistant]") && message.contains("provider"),
        "{message}"
    );
    assert_eq!(panel.read_with(cx, |p, cx| p.session(cx)), None);
    for region in ["models", "retry"] {
        draw(cx);
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(region)))
                .is_some(),
            "{region} is offered"
        );
    }
    // No chat session was created and the provider heard nothing.
    assert!(session_infos(&view, cx).iter().all(|s| s.chat.is_none()));
    assert!(env.mock.requests().is_empty());

    // The link opens Models settings.
    click(&view, cx, Region::Assistant("models"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("models".to_owned())
    );
    cx.simulate_keystrokes("escape");
    // Retrying asks the daemon again (it still lacks the setting) and still starts nothing.
    click(&view, cx, Region::Assistant("retry"));
    wait_until(cx, "the second answer", |cx| {
        panel.read_with(cx, |p, _| {
            matches!(p.state(ScopeKey::Global), Opened::Failed(_))
        })
    });
    assert!(env.mock.requests().is_empty());
    assert!(session_infos(&view, cx).iter().all(|s| s.chat.is_none()));
}

#[gpui::test]
fn an_unconfigured_assistant_opens_on_the_ready_provider_without_calling_it(
    cx: &mut TestAppContext,
) {
    // `[assistant]` names nothing, but the provider `mock` has a key: it is the automatic
    // backend, with its default model, and opening it asks the provider nothing.
    let env = AssistantEnv::start(Options::default());
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (session, _, _) = shown(&panel, cx);
    let chat = session_infos(&view, cx)
        .into_iter()
        .find(|s| s.id == session)
        .and_then(|s| s.chat)
        .expect("a chat session");
    assert!(chat.is_assistant());
    assert_eq!(chat.provider_id.as_deref(), Some("mock"));
    assert_eq!(chat.model, "m-1");
    assert!(env.mock.requests().is_empty());
}

// ---- send to assistant from a terminal block --------------------------------------------------

#[gpui::test]
fn a_terminal_block_goes_to_the_assistant_as_a_structured_attachment(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("it failed"));
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (info, terminal) = terminal_with_blocks(&env.daemon, workspace.id);
    let cmd1 = block_id(&env.daemon, terminal, "cmd1");
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    // The user opened the Assistant: "Send to agent" goes to the assistant it shows.
    let panel = open_assistant(&view, cx);
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the terminal view", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(info.id))
            .is_some_and(|v| v.read_with(cx, |v, _| v.model().blocks().get(cmd1).is_some()))
    });
    let terminal_view = view
        .read_with(cx, |wb, _| wb.terminal_view_of(info.id))
        .unwrap();
    assert!(terminal_view.update(cx, |v, cx| v.send_block_to_agent(cmd1, cx)));
    wait_until(cx, "the block chip", |cx| {
        panel.read_with(cx, |p, cx| {
            p.session(cx)
                .and_then(|s| p.chat_model(s))
                .is_some_and(|m| !m.read(cx).context().is_empty())
        })
    });
    let (session, model, chat) = shown(&panel, cx);
    assert_eq!(
        panel.read_with(cx, |p, cx| p.key(cx)),
        ScopeKey::Workspace(workspace.id)
    );
    assert_eq!(
        context_attachments(&model, cx),
        [Attachment::ShellBlock {
            terminal_id: terminal,
            block_id: cmd1
        }],
        "a block reference, not text read here"
    );
    assert!(
        model.read_with(cx, |m, _| m.chips().is_empty()),
        "no client-side quote"
    );
    assert!(
        session_infos(&view, cx)
            .iter()
            .any(|s| s.id == session && s.chat.as_ref().is_some_and(|c| c.agent == "assistant"))
    );
    send(&model, &chat, cx, "why exit 1?");
    wait_answer(&model, cx, "it failed");
    let prompt = last_prompt(&env.mock);
    assert!(prompt.contains("[shell: cmd1 · exit 1"), "{prompt}");
    assert!(prompt.contains("out1"), "{prompt}");
    assert!(!prompt.contains("out2"), "{prompt}");
}

#[gpui::test]
fn the_send_menu_offers_the_assistant_when_no_chat_running_is_one(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let root = env.project("project");
    let workspace = env.daemon.create_workspace("project", &[&root]);
    let (info, terminal) = terminal_with_blocks(&env.daemon, workspace.id);
    let cmd2 = block_id(&env.daemon, terminal, "cmd2");
    // Two ordinary direct chats are running: there is no obvious target, so the user picks.
    for title in ["first chat", "second chat"] {
        let _: hutan_protocol::messages::SessionResult =
            env.daemon
                .call(Request::ChatCreate(hutan_protocol::chat::ChatCreate {
                    workspace_id: Some(workspace.id),
                    cwd: root.clone(),
                    agent: "direct".to_owned(),
                    model: Some("m-1".to_owned()),
                    approval_policy: None,
                    sandbox: None,
                    title: Some(title.to_owned()),
                    mode: None,
                    provider_id: Some("mock".to_owned()),
                    system_prompt: None,
                    account: None,
                }));
    }
    let (view, cx) = open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the terminal view", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(info.id))
            .is_some_and(|v| v.read_with(cx, |v, _| v.model().blocks().get(cmd2).is_some()))
    });
    let terminal_view = view
        .read_with(cx, |wb, _| wb.terminal_view_of(info.id))
        .unwrap();
    assert!(terminal_view.update(cx, |v, cx| v.send_block_to_agent(cmd2, cx)));
    let entries: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu asks where to send it")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(
        entries,
        ["Send to first chat", "Send to second chat", "Send to Mawas"]
    );
    // The assistant of the terminal's workspace is opened on demand and gets a block reference.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    let panel = panel(&view, cx);
    wait_until(cx, "the block chip", |cx| {
        panel.read_with(cx, |p, cx| {
            p.session(cx)
                .and_then(|s| p.chat_model(s))
                .is_some_and(|m| !m.read(cx).context().is_empty())
        })
    });
    let (_, model, _) = shown(&panel, cx);
    assert_eq!(
        panel.read_with(cx, |p, cx| p.key(cx)),
        ScopeKey::Workspace(workspace.id)
    );
    assert_eq!(
        context_attachments(&model, cx),
        [Attachment::ShellBlock {
            terminal_id: terminal,
            block_id: cmd2
        }]
    );
}

#[gpui::test]
fn an_assistants_provider_errors_offer_the_recovery_of_a_direct_chat(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route(
        "POST",
        PATH,
        hutan_gateway::testing::Reply::json(401, r#"{"error":{"message":"key rejected"}}"#),
    );
    let (view, cx) = open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);
    send(&model, &chat, cx, "hello");
    wait_until(cx, "the Set key action", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::Chat("fix-key")).is_some()
        })
    });
    // The header's backend chip opens the menu with the model switch, as for any gateway chat.
    click(&view, cx, Region::Assistant("backend"));
    let entries = view.read_with(cx, |wb, _| wb.context_menu_entries().unwrap());
    assert_eq!(entries[0].0.as_ref(), "Switch model…");
    assert_eq!(entries[1].0.as_ref(), "Backends and models…");
    assert!(
        view.read_with(cx, |wb, _| wb.context_menu_disabled().is_empty()),
        "between turns both rows work"
    );
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.context_menu_entries().is_none()));
    click(&view, cx, Region::Chat("fix-key"));
    assert!(view.read_with(cx, |wb, _| wb.modal().is_some_and(|m| m.secure_input)));
}
