#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The terminal block overlay (M10-C) in the desktop, against an in-process daemon and real zsh,
//! bash 3.2 and fish sessions with the daemon's automatic shell integration (shells that are not
//! installed are skipped with a message), plus scripted OSC 133 sessions where exact blocks
//! matter more than a shell.

mod common;

use std::path::PathBuf;

use common::{Shell, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::actions::{BlockRef, JumpToBlock};
use hutan_desktop::workbench::panels::InspectorPanel;
use hutan_protocol::blocks::{Block, BlockPart, BlockState};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::TerminalView;
use hutan_terminal_view::blocks::{OverlayReport, Tone, chip_text};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        state,
    )
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn term(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Option<Entity<TerminalView>> {
    view.read_with(cx, |wb, _| wb.terminal_view_of(id))
}

/// The blocks the desktop's model holds, oldest first.
fn blocks(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> Vec<Block> {
    term(view, cx, id)
        .map(|v| v.read_with(cx, |v, _| v.model().blocks().iter().cloned().collect()))
        .unwrap_or_default()
}

fn wait_blocks(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
    what: &str,
    pred: impl Fn(&[Block]) -> bool,
) {
    wait_until(cx, what, |cx| pred(&blocks(view, cx, id)));
}

fn finished(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter(|b| b.state == BlockState::Finished)
        .count()
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn read<T>(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
    f: impl FnOnce(&TerminalView) -> T,
) -> T {
    let v = term(view, cx, id).expect("the session is open");
    v.read_with(cx, |v, _| f(v))
}

fn report(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> OverlayReport {
    read(view, cx, id, |v| v.painted_overlay().clone())
}

fn text(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> Vec<String> {
    read(view, cx, id, TerminalView::visible_text)
}

fn type_line(cx: &mut VisualTestContext, line: &str) {
    cx.simulate_input(line);
    cx.simulate_keystrokes("enter");
}

fn write_settings(daemon: &TestDaemon, text: &str) {
    std::fs::write(daemon.home().join("settings.toml"), text).unwrap();
}

fn clipboard(cx: &VisualTestContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|c| c.text())
}

fn terminal_of(info: &hutan_protocol::messages::SessionInfo) -> hutan_core::TerminalId {
    info.terminal_id.expect("a terminal")
}

/// Everything the brief asks of one shell, end to end.
fn shell_flow(cx: &mut TestAppContext, shell: Shell) {
    let Some(path) = shell.path() else {
        eprintln!("skipping: {shell:?} is not installed");
        return;
    };
    let daemon = TestDaemon::start();
    let session = daemon.shell_session(shell, path);
    let id = session.info.id;
    let terminal = terminal_of(&session.info);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, id);
    wait_blocks(&view, cx, id, "the first prompt", |b| {
        b.len() == 1 && b[0].state == BlockState::Prompt
    });
    draw(cx);

    // `true`, `false` and a command with output: status and duration of each.
    for (index, line) in ["true", "false", "printf 'a\\nb\\nc\\n'"]
        .into_iter()
        .enumerate()
    {
        type_line(cx, line);
        wait_blocks(&view, cx, id, line, |b| {
            finished(b) == index + 1 && b.last().is_some_and(|b| b.state == BlockState::Prompt)
        });
    }
    draw(cx);
    let now = blocks(&view, cx, id);
    assert_eq!(
        now.iter().map(|b| b.exit_code).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(0), None],
        "{shell:?}: {now:#?}"
    );
    assert_eq!(
        now,
        daemon.blocks(terminal),
        "{shell:?}: the desktop's list is the daemon's"
    );
    let overlay = report(&view, cx, id);
    assert_eq!(
        overlay
            .bars
            .iter()
            .map(|(_, _, tone)| *tone)
            .collect::<Vec<_>>(),
        [Tone::Success, Tone::Error, Tone::Success],
        "{shell:?}: {overlay:?}"
    );
    let chips: Vec<&str> = overlay
        .chips
        .iter()
        .map(|(_, text)| text.as_str())
        .collect();
    assert_eq!(chips.len(), 3, "{shell:?}: {overlay:?}");
    assert!(
        chips[0].starts_with("✓ ") && chips[1].starts_with("✗ 1 "),
        "{chips:?}"
    );
    for (chip, block) in chips.iter().zip(&now) {
        assert_eq!(Some((*chip).to_owned()), chip_text(block), "{shell:?}");
        let duration = block.finished_at_ms.unwrap() - block.started_at_ms;
        assert!(duration < 30_000, "{shell:?}: {duration} ms");
    }

    // ⌘↑ / ⌘↓ move between prompts: the open prompt is the 4th block.
    let highlighted = |cx: &VisualTestContext| read(&view, cx, id, TerminalView::highlighted_block);
    cx.simulate_keystrokes("cmd-up");
    assert_eq!(highlighted(cx), Some(3), "{shell:?}");
    draw(cx);
    assert!(!report(&view, cx, id).highlighted.is_empty());
    cx.simulate_keystrokes("cmd-up");
    assert_eq!(highlighted(cx), Some(2), "{shell:?}");
    cx.simulate_keystrokes("cmd-down");
    assert_eq!(highlighted(cx), Some(3), "{shell:?}");
    cx.simulate_keystrokes("cmd-down");
    assert_eq!(highlighted(cx), Some(4), "{shell:?}: the open prompt");
    cx.simulate_keystrokes("cmd-down");
    assert_eq!(highlighted(cx), None, "{shell:?}: back at the live screen");

    // Collapse folds the output into one row and expanding restores it, and the daemon never
    // notices: its screen, its blocks and the block's text stay as they were.
    let before = text(&view, cx, id);
    let screen = read(&view, cx, id, |v| {
        v.model()
            .viewport()
            .iter()
            .map(hutan_terminal_view::model::TerminalModel::row_text)
            .collect::<Vec<_>>()
    });
    assert!(before.iter().any(|row| row == "c"), "{shell:?}: {before:?}");
    let view_of = term(&view, cx, id).unwrap();
    assert!(view_of.update(cx, |v, cx| v.toggle_block_collapsed(3, cx)));
    draw(cx);
    let folded = text(&view, cx, id);
    assert!(
        folded.iter().any(|row| row == "… 3 lines, exit 0"),
        "{shell:?}: {folded:?}"
    );
    assert!(
        !folded
            .iter()
            .any(|row| row == "a" || row == "b" || row == "c"),
        "{shell:?}: the output is folded away: {folded:?}"
    );
    assert_eq!(report(&view, cx, id).summaries.len(), 1);
    assert_eq!(
        read(&view, cx, id, |v| v
            .model()
            .viewport()
            .iter()
            .map(hutan_terminal_view::model::TerminalModel::row_text)
            .collect::<Vec<_>>()),
        screen,
        "{shell:?}: the model's screen is the daemon's"
    );
    assert_eq!(daemon.blocks(terminal), now, "{shell:?}");
    assert_eq!(daemon.block_output(terminal, 3), "a\nb\nc", "{shell:?}");
    assert!(view_of.update(cx, |v, cx| v.toggle_block_collapsed(3, cx)));
    draw(cx);
    assert_eq!(text(&view, cx, id), before, "{shell:?}");

    // Copy output equals what terminal.block_text answers.
    assert!(view_of.update(cx, |v, cx| v.copy_block_part(3, BlockPart::Output, cx)));
    wait_until(cx, "the clipboard", |cx| {
        clipboard(cx).as_deref() == Some("a\nb\nc")
    });
    assert_eq!(clipboard(cx).unwrap(), daemon.block_output(terminal, 3));
    // ⌘⇧C copies the current block's output (the last command with output).
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("x".into()));
    cx.simulate_keystrokes("cmd-shift-c");
    wait_until(cx, "⌘⇧C", |cx| {
        clipboard(cx).as_deref() == Some("a\nb\nc")
    });
    // ⌘⇧A selects it on the grid.
    cx.simulate_keystrokes("cmd-shift-a");
    assert_eq!(
        read(&view, cx, id, TerminalView::selection_text).as_deref(),
        Some("a\nb\nc"),
        "{shell:?}"
    );

    // Rerun types the command and Enter: a new block with the same command.
    assert!(view_of.update(cx, |v, cx| v.rerun_block(3, cx)));
    wait_blocks(&view, cx, id, "the rerun", |b| finished(b) == 4);
    let now = blocks(&view, cx, id);
    assert_eq!(
        now.iter().rev().find_map(|b| b.command.clone()).as_deref(),
        Some("printf 'a\\nb\\nc\\n'"),
        "{shell:?}"
    );
    wait_blocks(&view, cx, id, "the prompt", |b| {
        b.last().is_some_and(|b| b.state == BlockState::Prompt)
    });

    // A program on the alternate screen hides the overlay and gets its keys untouched.
    type_line(cx, "printf '\\033[?1049h'; sleep 2; printf '\\033[?1049l'");
    wait_until(cx, "the alternate screen", |cx| {
        read(&view, cx, id, |v| v.model().modes().alt_screen)
    });
    draw(cx);
    assert!(
        !read(&view, cx, id, TerminalView::blocks_visible),
        "{shell:?}"
    );
    assert_eq!(report(&view, cx, id), OverlayReport::default(), "{shell:?}");
    cx.simulate_keystrokes("cmd-up");
    assert_eq!(
        highlighted(cx),
        None,
        "{shell:?}: ⌘↑ does nothing on the alternate screen"
    );
    wait_until(cx, "the primary screen", |cx| {
        !read(&view, cx, id, |v| v.model().modes().alt_screen)
    });
    wait_blocks(&view, cx, id, "the command to finish", |b| {
        finished(b) == 5 && b.last().is_some_and(|b| b.state == BlockState::Prompt)
    });
    draw(cx);
    assert!(
        read(&view, cx, id, TerminalView::blocks_visible),
        "{shell:?}"
    );
    assert!(!report(&view, cx, id).bars.is_empty(), "{shell:?}");

    // `terminal.blocks = false` hot-reloads: the overlay goes, the text stays.
    let with_overlay = text(&view, cx, id);
    write_settings(&daemon, "[terminal]\nblocks = false\n");
    wait_until(cx, "blocks = false", |cx| {
        !read(&view, cx, id, TerminalView::blocks_visible)
    });
    draw(cx);
    assert_eq!(report(&view, cx, id), OverlayReport::default(), "{shell:?}");
    assert_eq!(text(&view, cx, id), with_overlay, "{shell:?}");
    cx.simulate_keystrokes("cmd-up");
    assert_eq!(highlighted(cx), None, "{shell:?}");
    write_settings(&daemon, "[terminal]\nblocks = true\n");
    wait_until(cx, "blocks = true", |cx| {
        read(&view, cx, id, TerminalView::blocks_visible)
    });
    draw(cx);
    assert!(!report(&view, cx, id).bars.is_empty(), "{shell:?}");
    drop(session);
}

#[gpui::test]
fn zsh_blocks_in_the_desktop(cx: &mut TestAppContext) {
    shell_flow(cx, Shell::Zsh);
}

#[gpui::test]
fn bash_3_2_blocks_in_the_desktop(cx: &mut TestAppContext) {
    shell_flow(cx, Shell::Bash);
}

#[gpui::test]
fn fish_blocks_in_the_desktop(cx: &mut TestAppContext) {
    shell_flow(cx, Shell::Fish);
}

/// A session whose output is `count` finished commands `cmd1..`, each printing one line, written
/// as OSC 133 marks by `sh`: exact blocks without a shell.
fn scripted(daemon: &TestDaemon, count: u32) -> hutan_protocol::messages::SessionInfo {
    daemon.sh(&format!(
        "i=1; while [ $i -le {count} ]; do \
         printf '\\033]133;A\\007$ \\033]133;B\\007cmd%d\\r\\n\\033]133;C\\007out%d\\r\\n\\033]133;D;%d\\007' $i $i $((i%2)); \
         i=$((i+1)); done; printf '\\033]133;A\\007$ \\033]133;B\\007'; sleep 600"
    ))
}

#[gpui::test]
fn the_inspector_lists_the_last_twenty_blocks_and_jumps_to_one(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = scripted(&daemon, 150);
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_blocks(&view, cx, info.id, "150 commands", |b| finished(b) == 150);
    // Requests is a tab of the right sidebar; History is the one showing.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "inspector".into(),
    ));
    draw(cx);

    let inspector = view
        .read_with(cx, |wb, _| wb.panel::<InspectorPanel>("inspector"))
        .expect("the inspector is drawn in the right sidebar");
    let current = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(info.id).cloned()
        })
        .unwrap();
    let rows = inspector.read_with(cx, |p, cx| p.block_rows(&current, cx));
    assert_eq!(rows.len(), 20);
    assert_eq!(rows[0].command, "cmd150", "newest first");
    assert_eq!(rows[19].command, "cmd131");
    assert_eq!(rows[0].exit_code, Some(0));
    assert_eq!(rows[1].exit_code, Some(1));
    assert!(rows.iter().all(|r| r.state == BlockState::Finished));
    assert!(rows.iter().all(|r| r.duration.is_some()));

    // A click on a row asks the workbench to scroll its terminal to the block and highlight it:
    // a block that is on the screen stays where it is, an old one is scrolled into view.
    let jump = |cx: &mut VisualTestContext, block: u64| {
        cx.dispatch_action(JumpToBlock(BlockRef {
            session: info.id.to_string(),
            block,
        }));
        cx.run_until_parked();
    };
    assert_eq!(read(&view, cx, info.id, TerminalView::scroll_offset), 0);
    jump(cx, rows[0].id);
    assert_eq!(
        read(&view, cx, info.id, TerminalView::highlighted_block),
        Some(rows[0].id)
    );
    assert_eq!(read(&view, cx, info.id, TerminalView::scroll_offset), 0);
    jump(cx, 1);
    assert_eq!(
        read(&view, cx, info.id, TerminalView::highlighted_block),
        Some(1)
    );
    assert!(
        read(&view, cx, info.id, TerminalView::scroll_offset) > 0,
        "an old block is scrolled into view"
    );
    // The inspector keeps listing the terminal while the dock has focus.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.focus_dock(hutan_desktop::workbench::model::DockPosition::Right)
        });
    });
    draw(cx);
    let still = inspector.read_with(cx, |p, cx| p.block_rows(&current, cx));
    assert_eq!(still.len(), 20);
}

#[gpui::test]
fn the_block_commands_are_in_the_palette(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, None);
    wait_synced(&view, cx, 0);
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    for wanted in [
        "Block: Previous Prompt",
        "Block: Next Prompt",
        "Block: Select Output",
        "Block: Copy Output",
        "Block: Copy Command",
        "Block: Collapse / Expand",
        "Block: Rerun",
        "Block: Send to Agent",
    ] {
        assert!(titles.iter().any(|t| t == wanted), "{wanted}: {titles:?}");
    }
    // With no terminal focused there is no block to send: the command says so.
    cx.dispatch_action(hutan_terminal_view::SendBlockToAgent);
    cx.run_until_parked();
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert!(notice.is_some_and(|n| n.contains("finished command")));
}

#[gpui::test]
fn folds_are_saved_with_the_layout_and_restored_with_the_terminal(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = scripted(&daemon, 3);
    let terminal = terminal_of(&info);
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_blocks(&view, cx, info.id, "3 commands", |b| finished(b) == 3);
    draw(cx);

    let view_of = term(&view, cx, info.id).unwrap();
    assert!(view_of.update(cx, |v, cx| v.toggle_block_collapsed(2, cx)));
    view.update(cx, |wb, _| wb.flush_save());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(
        saved["collapsed_blocks"][terminal.to_string()],
        serde_json::json!([2]),
        "{saved}"
    );
    assert!(saved["docks"].is_object(), "the layout is still there");

    // Unfolding removes the entry.
    assert!(view_of.update(cx, |v, cx| v.toggle_block_collapsed(2, cx)));
    view.update(cx, |wb, _| wb.flush_save());
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert!(saved.get("collapsed_blocks").is_none(), "{saved}");
}

#[gpui::test]
fn saved_folds_come_back_when_the_desktop_restarts(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = scripted(&daemon, 3);
    let terminal = terminal_of(&info);
    // The daemon has the blocks before any desktop is there.
    let deadline = std::time::Instant::now() + common::WAIT;
    while daemon
        .blocks(terminal)
        .iter()
        .filter(|b| b.state == BlockState::Finished)
        .count()
        < 3
    {
        assert!(std::time::Instant::now() < deadline, "the script is slow");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let state = daemon.home().join("desktop-state.json");
    // What an earlier desktop saved for this terminal; stale entries of other terminals are
    // pruned once the daemon has been heard.
    std::fs::write(
        &state,
        serde_json::json!({
            "version": 1,
            "collapsed_blocks": { terminal.to_string(): [1, 99], "01GONEGONEGONEGONEGONEGONE": [4] },
        })
        .to_string(),
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon, Some(state.clone()));
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_blocks(&view, cx, info.id, "3 commands", |b| finished(b) == 3);
    draw(cx);
    // Block 1 is folded again; block 99 never existed.
    let folded = read(&view, cx, info.id, |v| {
        v.model().collapsed().iter().copied().collect::<Vec<_>>()
    });
    assert_eq!(folded, [1]);
    assert!(
        text(&view, cx, info.id)
            .iter()
            .any(|row| row == "… 1 line, exit 1"),
        "{:?}",
        text(&view, cx, info.id)
    );
    // The terminal the daemon no longer has is forgotten in the file.
    view.update(cx, |wb, _| wb.flush_save());
    wait_until(cx, "the stale entry to be pruned", |cx| {
        view.update(cx, |wb, _| wb.flush_save());
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        saved["collapsed_blocks"]
            .get("01GONEGONEGONEGONEGONEGONE")
            .is_none()
    });
}

#[gpui::test]
fn blocks_are_fetched_again_after_the_connection_comes_back(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let Some(path) = Shell::Zsh.path().or_else(|| Shell::Bash.path()) else {
        eprintln!("skipping: no zsh or bash");
        return;
    };
    let shell = if path.ends_with("zsh") {
        Shell::Zsh
    } else {
        Shell::Bash
    };
    let session = daemon.shell_session(shell, path);
    let id = session.info.id;
    let terminal = terminal_of(&session.info);
    cx.executor().allow_parking();
    let connection = daemon.proxied_connection();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, connection, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, id);
    wait_blocks(&view, cx, id, "the first prompt", |b| b.len() == 1);

    // Cut the link, run commands behind the desktop's back, and let it reconnect.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the connection to drop", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() != hutan_client::ConnectionState::Connected
        })
    });
    daemon.send_input(terminal, b"true\n");
    daemon.send_input(terminal, b"false\n");
    let deadline = std::time::Instant::now() + common::WAIT;
    while daemon
        .blocks(terminal)
        .iter()
        .filter(|b| b.state == BlockState::Finished)
        .count()
        < 2
    {
        assert!(std::time::Instant::now() < deadline, "the shell is slow");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        finished(&blocks(&view, cx, id)),
        0,
        "the desktop has not heard"
    );
    daemon.proxy().block(false);
    wait_blocks(&view, cx, id, "the list after the reconnect", |b| {
        finished(b) == 2
    });
    assert_eq!(blocks(&view, cx, id), daemon.blocks(terminal));
    drop(session);
}
