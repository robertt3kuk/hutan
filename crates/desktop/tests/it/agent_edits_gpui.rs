#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! An agent's edits in file tabs (slice E-10) over a real daemon: a write by an agent's turn
//! with no `file_diff` (a shell command) reaches an open clean buffer as one undo step through
//! `fs.changed` (origin `agent`), its hunks show, the pending ledger records it and is saved for
//! the next start of the desktop.

use crate::common;

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::TestAppContext;
use hutan_desktop::workbench::FileOpen;
use hutan_desktop::workbench::agent_edits::{LEDGER_FILE, Ledger};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};

#[gpui::test]
fn a_shell_write_of_an_agent_turn_reaches_the_open_file_and_the_ledger(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    // The turn waits on an approval: the chat is working while the "shell" writes.
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open_with(cx, settings, daemon.connection(), Some(state.clone()));
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });

    let base: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let file = env.cwd().join("notes.txt");
    std::fs::write(&file, &base).unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&file), window, cx);
    });
    let key = file.display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.buffers()
                .get(&key)
                .is_some_and(|b| b.read(cx).is_ready())
        })
    });
    let buffer = view.read_with(cx, |wb, _| wb.buffers().get(&key).unwrap().clone());

    let _: ChatSent = daemon.call(Request::ChatSend(ChatSend {
        session_id: info.id,
        text: "Run `touch hi.txt` in a shell, then reply with the word done.".to_owned(),
        attachments: Vec::new(),
    }));
    wait_until(cx, "the turn to wait on its approval", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len() == 1)
    });

    // The agent's shell rewrites line 5 (no file_diff names the file), once: the tab's watch,
    // which the desktop registers after reading the file, may come after it, and the daemon
    // still reports a change since that read (FX-FSWATCH).
    let after = base.replace("line 5\n", "line five\n");
    std::fs::write(&file, &after).unwrap();
    wait_until(cx, "the agent's write in the ledger and the buffer", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.agent_ledger().ledger().entry(&key).is_some()
                && buffer.read(cx).text().to_string() == after
                && buffer.read(cx).agent_hunks_current()
                && buffer.read(cx).agent_hunks().len() == 1
        })
    });
    buffer.read_with(cx, |b, _| {
        assert!(!b.is_dirty(), "a clean buffer took the write");
        let text = b.text().to_string();
        let hunks = b.agent_hunks();
        assert_eq!(&text[hunks[0].bytes.clone()], "line five\n");
        assert_eq!(&*hunks[0].base_text, "line 5\n");
        assert_eq!(b.agent_epoch(), 1);
    });
    let entry = view.read_with(cx, |wb, _| wb.agent_ledger().ledger().entry(&key).cloned());
    let entry = entry.unwrap();
    assert_eq!(entry.session_id, info.id);
    assert_eq!(entry.base, base);
    assert_eq!(entry.agent, after);

    // Saved next to the layout: the next start of the desktop has it.
    view.update(cx, |wb, _| wb.flush_save());
    let restored = Ledger::load(&state.with_file_name(LEDGER_FILE));
    assert_eq!(restored.entry(&key), Some(&entry));

    // One undo step takes the agent's write back.
    let code = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let code = code.read_with(cx, |v, _| v.code().clone());
    let origin = code.entity_id();
    buffer.update(cx, |b, cx| {
        assert!(b.undo(origin, cx).is_some());
    });
    draw(cx);
    assert_eq!(buffer.read_with(cx, |b, _| b.text().to_string()), base);
}
