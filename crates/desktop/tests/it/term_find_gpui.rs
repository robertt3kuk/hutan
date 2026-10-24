#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Find in a terminal (TERM-FIND), in the desktop against an in-process daemon: ⌘F opens the bar,
//! typing highlights the matches in the visible rows, next and previous wrap, a match in the old
//! scrollback is scrolled to and loaded, new output keeps the count current, Escape gives the
//! keyboard back to the terminal, and with the bar closed nothing is searching or listening.

use crate::common;

use std::time::Duration;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::terminal_find::{REFRESH, TerminalFind};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::TerminalView;

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    )
}

/// A workbench showing one session that ran `script`, with the terminal view ready.
fn show<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    script: &str,
    wait_for: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext, SessionId) {
    let session = daemon.sh(script);
    let id = session.id;
    let (view, cx) = open(cx, daemon);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == 1
        })
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the output", |cx| {
        screen(&view, cx, id).iter().any(|l| l.contains(wait_for))
    });
    (view, cx, id)
}

fn term(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> Entity<TerminalView> {
    view.read_with(cx, |wb, _| wb.terminal_view_of(id))
        .expect("the session is open")
}

fn screen(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.terminal_view_of(id))
        .map(|v| v.read_with(cx, |v, _| v.visible_text()))
        .unwrap_or_default()
}

fn find_of(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<Entity<TerminalFind>> {
    view.read_with(cx, |wb, _| wb.focused_terminal_find())
}

fn bar(view: &Entity<Workbench>, cx: &VisualTestContext) -> Entity<TerminalFind> {
    find_of(view, cx).expect("the find bar is open")
}

fn summary(find: &Entity<TerminalFind>, cx: &VisualTestContext) -> String {
    find.read_with(cx, |f, _| f.summary())
}

fn painted(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Vec<(u16, u16, u16, bool)> {
    term(view, cx, id).read_with(cx, |v, _| v.painted_find().to_vec())
}

fn terminal_has_focus(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) -> bool {
    let terminal = term(view, cx, id);
    cx.update(|window, cx| terminal.focus_handle(cx).is_focused(window))
}

fn find_has_focus(find: &Entity<TerminalFind>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| find.read(cx).is_focused(window))
}

/// Lets the throttle that searches again after output run out.
fn after_output(cx: &mut VisualTestContext) {
    cx.executor()
        .advance_clock(REFRESH + Duration::from_millis(50));
    draw(cx);
}

const ALPHA: &str = "printf 'alpha needle one\\nbeta\\nneedle two NEEDLE\\nEND-MARK\\n'; exec cat";

#[gpui::test]
fn cmd_f_opens_the_bar_typing_highlights_and_escape_gives_the_keyboard_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx, id) = show(cx, &daemon, ALPHA, "END-MARK");
    assert!(terminal_has_focus(&view, cx, id));
    assert!(find_of(&view, cx).is_none(), "no bar until asked");

    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    assert!(find_has_focus(&find, cx));
    assert!(!terminal_has_focus(&view, cx, id));

    // Typing goes to the field, not the shell; every match is tinted, the newest is current.
    let before = screen(&view, cx, id);
    cx.simulate_input("needle");
    wait_until(cx, "three matches", |cx| {
        find.read_with(cx, |f, _| f.matches().len() == 3)
    });
    assert_eq!(find.read_with(cx, |f, _| f.query().to_owned()), "needle");
    assert_eq!(summary(&find, cx), "3 of 3");
    assert_eq!(screen(&view, cx, id), before, "nothing reached the shell");
    wait_until(cx, "the highlights", |cx| painted(&view, cx, id).len() == 3);
    let cells = painted(&view, cx, id);
    // Rows 0 and 2 of the screen: "alpha needle one" at column 6; "needle two NEEDLE" twice.
    assert_eq!(
        cells,
        [(0, 6, 12, false), (2, 0, 6, false), (2, 11, 17, true)]
    );

    // Escape closes the bar, drops the highlights, and the terminal has the keyboard again.
    cx.simulate_keystrokes("escape");
    assert!(find_of(&view, cx).is_none());
    assert!(terminal_has_focus(&view, cx, id));
    draw(cx);
    assert!(painted(&view, cx, id).is_empty());
    // And it types.
    cx.simulate_input("typed");
    wait_until(cx, "the echo", |cx| {
        screen(&view, cx, id).iter().any(|l| l.contains("typed"))
    });
}

#[gpui::test]
fn next_and_previous_wrap_by_keys_and_return(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, id) = show(cx, &daemon, ALPHA, "END-MARK");
    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    cx.simulate_input("needle");
    wait_until(cx, "three matches", |cx| {
        find.read_with(cx, |f, _| f.matches().len() == 3)
    });
    let current = |cx: &VisualTestContext| find.read_with(cx, |f, _| f.current());
    assert_eq!(current(cx), Some(2));

    // Next wraps from the newest to the oldest; previous wraps back.
    cx.simulate_keystrokes("enter");
    assert_eq!(current(cx), Some(0));
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(current(cx), Some(2));
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(current(cx), Some(1));
    // ⌘G and ⌘⇧G work from the field, and from the terminal.
    cx.simulate_keystrokes("cmd-g");
    assert_eq!(current(cx), Some(2));
    cx.simulate_keystrokes("cmd-shift-g cmd-shift-g");
    assert_eq!(current(cx), Some(0));
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-g");
    assert!(
        find_of(&view, cx).is_none(),
        "⌘G without a bar opens nothing"
    );

    // The current match is the stronger one in the highlights.
    cx.simulate_keystrokes("cmd-f");
    draw(cx);
    wait_until(cx, "highlights again", |cx| {
        !painted(&view, cx, id).is_empty()
    });
    assert_eq!(
        painted(&view, cx, id)
            .iter()
            .filter(|(_, _, _, current)| *current)
            .count(),
        1
    );
}

#[gpui::test]
fn a_match_in_the_old_scrollback_is_scrolled_to_and_loaded(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, id) = show(
        cx,
        &daemon,
        "printf 'old-mark here\\n'; seq 1 2000; printf 'END-MARK\\n'; exec cat",
        "END-MARK",
    );
    assert_eq!(
        term(&view, cx, id).read_with(cx, |v, _| v.scroll_offset()),
        0
    );
    assert!(!screen(&view, cx, id).iter().any(|l| l.contains("old-mark")));

    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    cx.simulate_input("old-mark");
    wait_until(cx, "the match", |cx| {
        find.read_with(cx, |f, _| f.matches().len() == 1)
    });
    // The view went back about two thousand rows, asked the daemon for them, and shows the line
    // with the match tinted.
    wait_until(cx, "the old rows to load", |cx| {
        screen(&view, cx, id)
            .iter()
            .any(|l| l.contains("old-mark here"))
    });
    assert!(term(&view, cx, id).read_with(cx, |v, _| v.scroll_offset()) > 1500);
    wait_until(cx, "the highlight", |cx| painted(&view, cx, id).len() == 1);
    let (row, from, to, current) = painted(&view, cx, id)[0];
    assert!(current);
    assert_eq!((from, to), (0, 8));
    assert!(
        screen(&view, cx, id)[usize::from(row)].starts_with("old-mark here"),
        "the tinted row is the match's row"
    );
}

#[gpui::test]
fn new_output_keeps_the_matches_and_the_count_current(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let session = daemon.sh("printf 'needle one\\nEND-MARK\\n'; exec cat");
    let terminal = session.terminal_id.expect("a terminal");
    let id = session.id;
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the output", |cx| {
        screen(&view, cx, id).iter().any(|l| l.contains("END-MARK"))
    });
    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    cx.simulate_input("needle");
    wait_until(cx, "one match", |cx| summary(&find, cx) == "1 of 1");

    // The terminal echoes a typed line and `cat` prints it again: two more matches. The bar is
    // searched again once the output has settled, and keeps the current match while at it.
    let first = find.read_with(cx, |f, _| f.matches()[0].clone());
    daemon.send_input(terminal, b"needle two\n");
    wait_until(cx, "the count to follow the output", |cx| {
        after_output(cx);
        find.read_with(cx, |f, _| f.matches().len() == 3)
    });
    assert_eq!(find.read_with(cx, |f, _| f.matches()[0].clone()), first);
    assert_eq!(
        summary(&find, cx).split(" of ").nth(1),
        Some("3"),
        "{}",
        summary(&find, cx)
    );
    let highlighted = |cx: &VisualTestContext| painted(&view, cx, id).len();
    wait_until(cx, "the new matches to be drawn", |cx| highlighted(cx) == 3);
}

#[gpui::test]
fn case_and_regex_follow_the_toggles_and_a_bad_pattern_says_so(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, _) = show(cx, &daemon, ALPHA, "END-MARK");
    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    cx.simulate_input("needle");
    wait_until(cx, "three", |cx| summary(&find, cx) == "3 of 3");
    find.update(cx, |f, cx| f.set_case_sensitive(true, cx));
    wait_until(cx, "two with case", |cx| summary(&find, cx) == "2 of 2");
    find.update(cx, |f, cx| {
        f.set_case_sensitive(false, cx);
        f.set_regex(true, cx);
        f.set_query(r"^needle \w+", cx);
    });
    wait_until(cx, "one by regex", |cx| summary(&find, cx) == "1 of 1");
    find.update(cx, |f, cx| f.set_query("(open", cx));
    wait_until(cx, "the pattern error", |cx| {
        summary(&find, cx).starts_with("invalid pattern")
    });
    find.update(cx, |f, cx| f.set_query("nothing-like-it", cx));
    wait_until(cx, "no matches", |cx| summary(&find, cx) == "No matches");
    find.update(cx, |f, cx| f.set_query("", cx));
    assert_eq!(summary(&find, cx), "");
}

const BLOCKS: &str = "printf '\\033]133;A\\007$ \\033]133;B\\007one\\r\\n\\033]133;C\\007shared\\r\\nin-one\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007two\\r\\n\\033]133;C\\007shared\\r\\nin-two\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat";

#[gpui::test]
fn the_block_option_limits_the_search_to_the_selected_blocks_output(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, id) = show(cx, &daemon, BLOCKS, "in-two");
    wait_until(cx, "the blocks", |cx| {
        term(&view, cx, id).read_with(cx, |v, _| v.model().blocks().iter().count() == 3)
    });
    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    cx.simulate_input("shared");
    wait_until(cx, "two", |cx| summary(&find, cx) == "2 of 2");
    // No block is selected: the option does nothing.
    find.update(cx, |f, cx| f.toggle_block(cx));
    assert_eq!(find.read_with(cx, |f, _| f.block()), None);

    // ⌘↑ selects the previous block (the second command's, `two`); the option limits to it.
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-up");
    let selected = term(&view, cx, id)
        .read_with(cx, |v, _| v.highlighted_block())
        .expect("a block is selected");
    cx.simulate_keystrokes("cmd-f");
    let find = bar(&view, cx);
    find.update(cx, |f, cx| f.toggle_block(cx));
    assert_eq!(find.read_with(cx, |f, _| f.block()), Some(selected));
    wait_until(cx, "one in the block", |cx| summary(&find, cx) == "1 of 1");
    find.update(cx, |f, cx| f.toggle_block(cx));
    wait_until(cx, "two again", |cx| summary(&find, cx) == "2 of 2");
}

#[gpui::test]
fn under_vim_mode_the_terminal_is_unaffected_and_cmd_f_never_reaches_the_shell(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[editor]\nvim_mode = true\n",
    )
    .unwrap();
    let (view, cx, id) = show(cx, &daemon, ALPHA, "END-MARK");
    // Keys go to the shell as ever: `jk` and Escape are not Vim commands in a terminal.
    cx.simulate_input("jk");
    wait_until(cx, "the echo", |cx| {
        screen(&view, cx, id).iter().any(|l| l.starts_with("jk"))
    });
    let before = screen(&view, cx, id);
    cx.simulate_keystrokes("cmd-f");
    assert_eq!(
        screen(&view, cx, id),
        before,
        "⌘F is not typed into the shell"
    );
    let find = bar(&view, cx);
    assert!(find_has_focus(&find, cx));
    cx.simulate_keystrokes("escape");
    // Vim's Insert → Normal first: the field leaves Insert mode, the bar stays; the second
    // Escape closes it.
    if find_of(&view, cx).is_some() {
        cx.simulate_keystrokes("escape");
    }
    assert!(find_of(&view, cx).is_none());
    assert!(terminal_has_focus(&view, cx, id));
    cx.simulate_input("ls");
    wait_until(cx, "the echo of ls", |cx| {
        screen(&view, cx, id).iter().any(|l| l.contains("ls"))
    });
}

#[gpui::test]
fn with_the_bar_closed_nothing_listens_or_draws(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx, id) = show(cx, &daemon, ALPHA, "END-MARK");
    let terminal = term(&view, cx, id);
    assert!(terminal.read_with(cx, |v, _| v.find_highlights().is_none()));
    assert!(painted(&view, cx, id).is_empty());
    assert_eq!(view.read_with(cx, |wb, _| wb.terminal_find_count()), 0);
    // Opening and closing leaves nothing behind.
    cx.simulate_keystrokes("cmd-f");
    assert_eq!(view.read_with(cx, |wb, _| wb.terminal_find_count()), 1);
    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |wb, _| wb.terminal_find_count()), 0);
    assert!(terminal.read_with(cx, |v, _| v.find_highlights().is_none()));
}
