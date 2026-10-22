#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! TERM-SPLITS: one tab holds a tree of terminals, as in Ghostty and iTerm. The keys act on the
//! splits while the keyboard is in a terminal, every split is its own daemon session, the layout
//! comes back after a restart, a tab can be dragged into another as a split, and ⌘D in a file
//! stays what it was.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::agents::{Node, Reporter};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{
    Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, point, px,
};
use hutan_core::SessionId;
use hutan_desktop::workbench::model::{
    Axis, DockPosition, Edge, FocusTarget, ItemRef, PaneId, PaneTree, TermSplit,
};
use hutan_desktop::workbench::persistence::STATE_FILE;
use hutan_desktop::workbench::{BadgeSite, FileOpen, Region, SAVE_DEBOUNCE, Workbench, actions};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    state_path: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        state_path,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// A terminal that starts in `cwd`, tells the terminal it works in `reported` (OSC 7) when
/// given, and then sleeps.
fn terminal(daemon: &TestDaemon, cwd: &Path, reported: Option<&Path>) -> SessionId {
    let script = match reported {
        Some(dir) => format!(
            "printf '\\033]7;file://localhost%s\\007' '{}'; sleep 120",
            dir.display()
        ),
        None => "sleep 120".to_owned(),
    };
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), script],
            cwd: Some(cwd.to_path_buf()),
            env: [("PS1".to_owned(), "$ ".to_owned())].into(),
            ..TerminalSpec::default()
        }),
    }));
    created.session.id
}

fn show(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(id).is_some())
    });
    settle(cx);
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.executor().advance_clock(Duration::from_millis(60));
        draw(cx);
    }
}

fn splits(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<TermSplit> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .into_iter()
            .flat_map(|p| p.items.iter())
            .find_map(|i| i.as_term_split().cloned())
    })
}

fn sessions_of(split: &TermSplit) -> Vec<String> {
    split.sessions().into_iter().map(str::to_owned).collect()
}

fn tabs(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<ItemRef> {
    view.read_with(cx, |wb, _| {
        wb.state()
            .panes()
            .into_iter()
            .flat_map(|p| p.items.iter().cloned())
            .collect()
    })
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> gpui::Bounds<Pixels> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} was not drawn"))
}

fn drawn(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for i in 1..=8 {
        let t = i as f32 / 8.0;
        let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(p, MouseButton::Left, Modifiers::none());
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
}

fn focused(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<SessionId> {
    view.read_with(cx, |wb, _| wb.focused_session())
}

/// Waits for the tab to hold `count` splits.
fn wait_splits(view: &Entity<Workbench>, cx: &mut VisualTestContext, count: usize) -> TermSplit {
    wait_until(cx, "the split", |cx| {
        splits(view, cx).is_some_and(|s| s.len() == count)
    });
    settle(cx);
    splits(view, cx).unwrap()
}

#[gpui::test]
fn cmd_d_and_cmd_shift_d_split_inside_the_tab_in_the_cwd_the_terminal_reported(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let started = tempfile::tempdir().unwrap();
    let moved = tempfile::tempdir().unwrap();
    let moved_dir = moved.path().canonicalize().unwrap();
    let a = terminal(&daemon, started.path(), Some(&moved_dir));
    let (view, cx) = open(cx, &daemon, None);
    // The shell's OSC 7 arrives: the session's folder follows it.
    wait_until(cx, "the terminal's OSC 7", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(a)
                .and_then(|s| s.cwd.clone())
                .and_then(|c| c.canonicalize().ok())
                == Some(moved_dir.clone())
        })
    });
    show(&view, cx, a);
    let panes_before = view.read_with(cx, |wb, _| wb.state().panes().len());

    // The keys, not the actions: the keyboard is in the terminal.
    cx.simulate_keystrokes("cmd-d");
    let split = wait_splits(&view, cx, 2);
    let b: SessionId = split.sessions()[1].parse().unwrap();
    assert_eq!(split.sessions()[0], a.to_string());
    assert_eq!(split.focused_session(), Some(b.to_string().as_str()));
    assert_eq!(
        focused(&view, cx),
        Some(b),
        "the keyboard is in the new split"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().panes().len()),
        panes_before,
        "a split of a tab is not a new pane"
    );
    assert_eq!(tabs(&view, cx).len(), 1, "no tab strip entry per split");
    assert_eq!(
        daemon.session(b).cwd.and_then(|c| c.canonicalize().ok()),
        Some(moved_dir.clone()),
        "the new terminal starts where the focused one is (OSC 7)"
    );
    let (left, right) = (
        bounds(&view, cx, Region::TermSplit(a)),
        bounds(&view, cx, Region::TermSplit(b)),
    );
    assert!(left.right() <= right.left() + px(2.0), "{left:?} {right:?}");
    assert!((left.top() - right.top()).abs() < px(1.0));

    // Each split is a session of the sidebar, in the container the tab's terminal lives in.
    wait_until(cx, "both sessions in the sidebar", |cx| {
        draw(cx);
        drawn(&view, cx, Region::SessionRow(a)) && drawn(&view, cx, Region::SessionRow(b))
    });

    // ⌘⇧D splits the focused one downwards.
    cx.simulate_keystrokes("cmd-shift-d");
    let split = wait_splits(&view, cx, 3);
    let c: SessionId = split.sessions()[2].parse().unwrap();
    assert!(matches!(
        &split.tree,
        PaneTree::Split { axis: Axis::Horizontal, children, .. }
            if matches!(children[1], PaneTree::Split { axis: Axis::Vertical, .. })
    ));
    let (upper, lower) = (
        bounds(&view, cx, Region::TermSplit(b)),
        bounds(&view, cx, Region::TermSplit(c)),
    );
    assert!(
        upper.bottom() <= lower.top() + px(2.0),
        "{upper:?} {lower:?}"
    );
    assert_eq!(
        daemon.session(c).cwd.and_then(|c| c.canonicalize().ok()),
        Some(moved_dir)
    );
    // Three splits, one tab, one `⊞ 3`.
    assert!(drawn(
        &view,
        cx,
        Region::TabSplits(split_pane(&view, cx), 0)
    ));
}

fn split_pane(view: &Entity<Workbench>, cx: &VisualTestContext) -> PaneId {
    view.read_with(cx, |wb, _| wb.state().active_pane_id())
}

#[gpui::test]
fn keys_focus_resize_zoom_and_close_the_splits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    cx.simulate_keystrokes("cmd-d");
    let split = wait_splits(&view, cx, 2);
    let b: SessionId = split.sessions()[1].parse().unwrap();
    wait_until(cx, "the second terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(b).is_some())
    });
    settle(cx);

    // ⌘⌥← / ⌘⌥→ move the keyboard between the splits.
    cx.simulate_keystrokes("cmd-alt-left");
    settle(cx);
    assert_eq!(focused(&view, cx), Some(a));
    let in_a = view.read_with(cx, |wb, _| wb.terminal_view_of(a).unwrap());
    cx.update(|window, cx| {
        use gpui::Focusable as _;
        assert!(
            in_a.focus_handle(cx).is_focused(window),
            "the GPUI focus follows"
        );
    });
    cx.simulate_keystrokes("cmd-alt-right");
    settle(cx);
    assert_eq!(focused(&view, cx), Some(b));
    // At the edge of the tab's splits the key moves on, as it does everywhere: here to the right
    // dock; the keyboard comes back to the split.
    cx.simulate_keystrokes("cmd-alt-right");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().focus),
        FocusTarget::Dock(DockPosition::Right)
    );
    view.update_in(cx, |wb, window, cx| wb.focus_split(b, window, cx));
    settle(cx);
    assert_eq!(focused(&view, cx), Some(b));

    // ⌃⌘→ / ⌃⌘← move the divider.
    let before = bounds(&view, cx, Region::TermSplit(a)).size.width;
    cx.simulate_keystrokes("ctrl-cmd-right");
    settle(cx);
    assert!(
        bounds(&view, cx, Region::TermSplit(a)).size.width > before,
        "the focused split's divider moved with the key"
    );
    cx.simulate_keystrokes("ctrl-cmd-left ctrl-cmd-left");
    settle(cx);
    assert!(bounds(&view, cx, Region::TermSplit(a)).size.width < before);

    // ⌘⇧↩ zooms the focused split; again puts it back.
    cx.simulate_keystrokes("cmd-shift-enter");
    settle(cx);
    assert!(splits(&view, cx).unwrap().zoomed);
    assert!(drawn(&view, cx, Region::TermSplit(b)));
    assert!(
        !drawn(&view, cx, Region::TermSplit(a)),
        "only the zoomed split is drawn"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.state().zoomed.is_none()),
        "the pane itself did not zoom"
    );
    cx.simulate_keystrokes("cmd-shift-enter");
    settle(cx);
    assert!(!splits(&view, cx).unwrap().zoomed);
    assert!(drawn(&view, cx, Region::TermSplit(a)));

    // ⌘W closes the focused split: its session ends, the tab keeps the other terminal.
    cx.simulate_keystrokes("cmd-w");
    wait_until(cx, "the split to close", |cx| splits(&view, cx).is_none());
    assert_eq!(
        tabs(&view, cx),
        vec![ItemRef::Terminal {
            session: a.to_string()
        }]
    );
    assert_eq!(focused(&view, cx), Some(a));
    wait_until(cx, "the closed session", |_| {
        daemon
            .call::<hutan_protocol::messages::SessionList>(Request::SessionList(Default::default()))
            .sessions
            .iter()
            .all(|s| s.id != b)
    });
    // The last terminal is the tab: ⌘W closes it like any tab (it detaches).
    cx.simulate_keystrokes("cmd-w");
    settle(cx);
    assert!(tabs(&view, cx).is_empty());
}

#[gpui::test]
fn cmd_shift_w_closes_the_whole_tab_of_splits(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    cx.simulate_keystrokes("cmd-d");
    wait_splits(&view, cx, 2);
    cx.simulate_keystrokes("cmd-shift-w");
    settle(cx);
    assert!(
        tabs(&view, cx).is_empty(),
        "the tab and its splits are gone"
    );
    // Detach: the sessions keep running in the daemon.
    assert!(daemon.session(a).state.is_live());
}

#[gpui::test]
fn the_pane_split_commands_still_make_panes_and_cmd_d_in_a_file_is_not_a_split(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    // Palette / ⌘K arrows: a pane split with a terminal of its own.
    cx.dispatch_action(actions::SplitRight);
    wait_until(cx, "the new pane", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 2)
    });
    assert!(
        splits(&view, cx).is_none(),
        "a pane split is not a terminal split"
    );

    // In a file tab ⌘D keeps its meaning: no terminal appears.
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "one one\n").unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file.clone()), window, cx);
    });
    wait_until(cx, "the file tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .active_item()
                .is_some_and(|i| i.as_file().is_some())
        })
    });
    settle(cx);
    let sessions_before = daemon
        .call::<hutan_protocol::messages::SessionList>(Request::SessionList(Default::default()))
        .sessions
        .len();
    cx.simulate_keystrokes("cmd-d");
    settle(cx);
    let sessions_after = daemon
        .call::<hutan_protocol::messages::SessionList>(Request::SessionList(Default::default()))
        .sessions
        .len();
    assert_eq!(
        sessions_before, sessions_after,
        "⌘D in a file started no terminal"
    );
    assert!(splits(&view, cx).is_none());
}

#[gpui::test]
fn the_layout_with_splits_comes_back_after_a_restart_and_reattaches_the_sessions(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let state_dir = tempfile::tempdir().unwrap();
    let path = state_dir.path().join(STATE_FILE);
    let saved = {
        let (view, cx) = open(cx, &daemon, Some(path.clone()));
        show(&view, cx, a);
        cx.simulate_keystrokes("cmd-d");
        wait_splits(&view, cx, 2);
        cx.simulate_keystrokes("cmd-shift-d");
        let split = wait_splits(&view, cx, 3);
        cx.simulate_keystrokes("cmd-alt-left");
        cx.executor().advance_clock(SAVE_DEBOUNCE);
        cx.run_until_parked();
        view.update(cx, |wb, _| wb.flush_save());
        assert!(path.exists());
        let _ = split;
        splits(&view, cx).unwrap()
    };
    // A second window is the restarted app.
    let (view, cx) = open(cx, &daemon, Some(path));
    wait_until(cx, "the restored splits", |cx| {
        splits(&view, cx).is_some_and(|s| s.len() == 3)
    });
    settle(cx);
    let restored = splits(&view, cx).unwrap();
    assert_eq!(restored, saved, "tree, ratios, focus and zoom come back");
    for session in restored.sessions() {
        let id: SessionId = session.parse().unwrap();
        assert!(view.read_with(cx, |wb, _| wb.holds(id)), "{id} reattached");
        assert!(drawn(&view, cx, Region::TermSplit(id)), "{id} drawn");
    }
}

#[gpui::test]
fn dividers_drag_with_a_minimum_and_a_double_click_equalises(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    cx.simulate_resize(gpui::size(px(1400.0), px(800.0)));
    show(&view, cx, a);
    cx.simulate_keystrokes("cmd-d");
    let split = wait_splits(&view, cx, 2);
    let b: SessionId = split.sessions()[1].parse().unwrap();
    let a_before = bounds(&view, cx, Region::TermSplit(a));
    let divider = point(a_before.right(), a_before.center().y);
    drag(cx, divider, divider + point(px(120.0), px(0.0)));
    settle(cx);
    let a_after = bounds(&view, cx, Region::TermSplit(a));
    assert!(
        a_after.size.width > a_before.size.width + px(100.0),
        "{a_before:?} {a_after:?}"
    );

    // Far past the other side: the split keeps six columns.
    let divider = point(a_after.right(), a_after.center().y);
    drag(cx, divider, point(px(1390.0), divider.y));
    settle(cx);
    let b_squeezed = bounds(&view, cx, Region::TermSplit(b));
    assert!(
        b_squeezed.size.width >= px(6.0 * 13.0 * 0.6),
        "six columns of 13 pt text stay: {b_squeezed:?}"
    );
    assert!(b_squeezed.size.width < px(200.0));

    // A double-click on the divider shares the space equally.
    let divider = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::TermDivider(0)))
        .unwrap();
    let at = divider.center();
    for click_count in 1..=2 {
        cx.simulate_event(gpui::MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(gpui::MouseUpEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
        });
    }
    settle(cx);
    let (wa, wb_) = (
        bounds(&view, cx, Region::TermSplit(a)).size.width,
        bounds(&view, cx, Region::TermSplit(b)).size.width,
    );
    assert!((wa - wb_).abs() < px(4.0), "{wa:?} vs {wb_:?}");
}

#[gpui::test]
fn the_context_menu_splits_and_moves_a_split_to_a_new_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    // Right-click on the terminal; the entries are listed, then one is chosen.
    let choose =
        |view: &Entity<Workbench>, cx: &mut VisualTestContext, at: Point<Pixels>, label: &str| {
            cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
            cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
            let entries: Vec<String> = view
                .read_with(cx, |wb, _| wb.context_menu_entries())
                .expect("the terminal's menu is open")
                .into_iter()
                .map(|(label, _)| label.to_string())
                .collect();
            let index = entries
                .iter()
                .position(|e| e == label)
                .unwrap_or_else(|| panic!("no `{label}` in {entries:?}"));
            view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
            entries
        };
    let at = bounds(&view, cx, Region::Pane(split_pane(&view, cx))).center();
    let plain = choose(&view, cx, at, "Split Right");
    assert_eq!(plain, ["Split Right", "Split Down"]);
    let split = wait_splits(&view, cx, 2);
    let b: SessionId = split.sessions()[1].parse().unwrap();
    wait_until(cx, "the second terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(b).is_some())
    });
    settle(cx);
    let at = bounds(&view, cx, Region::TermSplit(b)).center();
    let in_splits = choose(&view, cx, at, "Move Split to New Tab");
    assert_eq!(
        in_splits,
        [
            "Split Right",
            "Split Down",
            "Move Split to New Tab",
            "Close Split"
        ]
    );
    settle(cx);
    assert_eq!(
        tabs(&view, cx),
        vec![
            ItemRef::Terminal {
                session: a.to_string()
            },
            ItemRef::Terminal {
                session: b.to_string()
            }
        ]
    );
    assert_eq!(focused(&view, cx), Some(b));
}

#[gpui::test]
fn a_terminal_tab_dropped_on_another_terminals_edge_becomes_a_split(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let b = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    cx.simulate_resize(gpui::size(px(1400.0), px(800.0)));
    show(&view, cx, a);
    show(&view, cx, b);
    assert_eq!(tabs(&view, cx).len(), 2);
    // Drag b's tab (the active one) onto the right edge of the body, where a is not shown: the
    // drop zones of a terminal pane are its edges.
    let pane = split_pane(&view, cx);
    let tab_b = bounds(&view, cx, Region::Tab(pane, 1));
    let body = bounds(&view, cx, Region::Pane(pane));
    let target = point(body.right() - px(30.0), body.center().y);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let _ = s.set_active_item(pane, 0);
        });
    });
    settle(cx);
    drag(cx, tab_b.center(), target);
    settle(cx);
    let split = splits(&view, cx).expect("the dropped tab is a split");
    assert_eq!(sessions_of(&split), [a.to_string(), b.to_string()]);
    assert_eq!(tabs(&view, cx).len(), 1);
}

fn blocked(daemon: &TestDaemon, session: SessionId) {
    Reporter::new(session, "omp-extension", "omp").report(daemon, Node::main("m").blocked());
}

/// A request in any split shows on the tab: a dot while the split is on screen, the toast and
/// the accent dot while its tab is not, and looking at the tab clears them.
#[gpui::test]
fn the_attention_of_any_split_shows_on_the_tab(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let a = terminal(&daemon, dir.path(), None);
    let c = terminal(&daemon, dir.path(), None);
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    cx.simulate_keystrokes("cmd-d");
    let split = wait_splits(&view, cx, 2);
    let b: SessionId = split.sessions()[1].parse().unwrap();
    wait_until(cx, "the second terminal", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(b).is_some())
    });
    // The keyboard goes back to a: b is on screen, not focused.
    cx.simulate_keystrokes("cmd-alt-left");
    settle(cx);
    assert_eq!(focused(&view, cx), Some(a));
    let pane = split_pane(&view, cx);

    blocked(&daemon, b);
    wait_until(cx, "the tab's attention dot", |cx| {
        draw(cx);
        drawn(&view, cx, Region::TabAttention(pane, 0))
    });
    assert!(
        view.read_with(cx, |wb, _| wb.attention_toasts().is_empty()),
        "b is on screen: no toast"
    );

    // Another tab in front: the split's tab is out of sight.
    show(&view, cx, c);
    assert_eq!(tabs(&view, cx).len(), 2);
    let split_tab = view.read_with(cx, |wb, _| wb.state().find_session_tab(&a.to_string()));
    let (pane, index) = split_tab.expect("the tab of splits");
    blocked(&daemon, a);
    wait_until(cx, "the toast for the split that is out of sight", |cx| {
        view.read_with(cx, |wb, _| {
            wb.attention_toasts().iter().any(|t| t.session == a)
        })
    });
    wait_until(cx, "the accent dot on the tab", |cx| {
        draw(cx);
        drawn(&view, cx, Region::TabCall(pane, index))
    });
    // Bringing the tab forward is seeing it: the dot goes.
    view.update_in(cx, |wb, window, cx| wb.open_session(b, window, cx));
    wait_until(cx, "the dot to clear", |cx| {
        draw(cx);
        !drawn(&view, cx, Region::TabCall(pane, index))
    });
}

/// The recovery card after a daemon restart is per split: each respawned terminal of a tab
/// says so, with its own chip.
#[gpui::test]
fn after_a_daemon_restart_every_split_has_its_own_recovery_chip(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let a = daemon.create_session(&["cat"], None).id;
    let b = daemon.create_session(&["cat"], None).id;
    let (view, cx) = open(cx, &daemon, None);
    show(&view, cx, a);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            let index = s.pane(pane).unwrap().active;
            s.split_terminal(pane, index, None, Edge::Right, &b.to_string())
                .unwrap();
        });
    });
    wait_splits(&view, cx, 2);
    let old = view
        .read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch)
        .expect("a synchronised epoch");
    daemon.stop();
    daemon.restart();
    wait_until(cx, "the workbench to follow the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.connection_state() == hutan_client::ConnectionState::Connected
                && store.state().epoch.is_some_and(|epoch| epoch != old)
        })
    });
    wait_until(cx, "a recovery chip on each split", |cx| {
        draw(cx);
        drawn(&view, cx, Region::RecoveryChip(BadgeSite::Tab, a))
            && drawn(&view, cx, Region::RecoveryChip(BadgeSite::Tab, b))
    });
    // Both are still the tab's splits, reattached.
    let split = splits(&view, cx).expect("the splits survive the restart");
    assert_eq!(sessions_of(&split), [a.to_string(), b.to_string()]);
}
