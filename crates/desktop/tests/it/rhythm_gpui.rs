#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! UI-RHYTHM: the quit sheet and every dialog with three buttons stay inside their card at any
//! window width, the primary is the last button of a row (the first of a stack) and the sessions
//! a quit would stop are listed.

use crate::common;

use common::{TestDaemon, draw, open_with, settle, wait_until};
use gpui::{Bounds, Entity, Pixels, TestAppContext, VisualTestContext, px, size};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    sessions: usize,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let store = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, store, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
    (view, cx)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> Option<Bounds<Pixels>> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn part(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    name: &'static str,
) -> Bounds<Pixels> {
    bounds(view, cx, Region::Modal(name))
        .unwrap_or_else(|| panic!("the dialog's {name} was not drawn"))
}

fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    // Probes are inside borders: allow a pixel.
    f32::from(inner.left()) >= f32::from(outer.left()) - 1.0
        && f32::from(inner.right()) <= f32::from(outer.right()) + 1.0
        && f32::from(inner.top()) >= f32::from(outer.top()) - 1.0
        && f32::from(inner.bottom()) <= f32::from(outer.bottom()) + 1.0
}

#[gpui::test]
fn the_quit_sheet_stays_inside_its_card_at_400_600_and_900_pt(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon, 1);
    cx.dispatch_action(actions::Quit);
    cx.run_until_parked();
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));

    for width in [400.0_f32, 600.0, 900.0] {
        cx.simulate_resize(size(px(width), px(800.0)));
        settle(cx);
        let card = part(&view, cx, "card");
        let window = Bounds {
            origin: gpui::point(px(0.0), px(0.0)),
            size: size(px(width), px(800.0)),
        };
        assert!(
            inside(card, window),
            "{width}: the card {card:?} leaves the window"
        );
        let footer = part(&view, cx, "footer");
        let (cancel, choice, ok) = (
            part(&view, cx, "cancel"),
            part(&view, cx, "choice"),
            part(&view, cx, "ok"),
        );
        for (name, button) in [("cancel", cancel), ("choice", choice), ("ok", ok)] {
            assert!(
                inside(button, card),
                "{width}: {name} {button:?} leaves the card {card:?}"
            );
            assert!(
                inside(button, footer),
                "{width}: {name} is outside the footer"
            );
        }
        // The card is 480 pt wide, or the window less its margins.
        let expected = (width - 32.0).min(480.0);
        assert!(
            (f32::from(card.size.width) - expected).abs() <= 1.0,
            "{width}: the card is {} wide, expected {expected}",
            f32::from(card.size.width)
        );
        if width >= 512.0 {
            // One right-aligned row: Cancel, the danger option, the primary last.
            let same_row = |a: Bounds<Pixels>, b: Bounds<Pixels>| {
                (f32::from(a.top()) - f32::from(b.top())).abs() <= 1.0
            };
            assert!(
                same_row(cancel, choice) && same_row(choice, ok),
                "{width}: one row"
            );
            assert!(cancel.right() <= choice.left() && choice.right() <= ok.left());
            assert!(
                f32::from(card.right()) - f32::from(ok.right()) <= 25.0,
                "{width}: the primary sits at the right edge, inside the 24 pt padding"
            );
        } else {
            // Stacked: the primary on top, Cancel at the bottom, each as wide as the card's
            // content.
            assert!(ok.bottom() <= choice.top() && choice.bottom() <= cancel.top());
            assert!(
                (f32::from(ok.size.width) - f32::from(cancel.size.width)).abs() <= 2.5,
                "{width}: stacked buttons are as wide as each other: ok {ok:?} choice {choice:?} cancel {cancel:?} footer {footer:?}"
            );
        }
    }

    // Keep running is the primary and Return keeps it; Stop is the danger option.
    let (confirm, choice, destructive) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (
            modal.confirm_label.to_string(),
            modal.choice_label().map(str::to_owned),
            modal.destructive,
        )
    });
    assert_eq!(confirm, "Keep running");
    assert_eq!(choice.as_deref(), Some("Stop sessions and quit"));
    assert!(!destructive);
}

#[gpui::test]
fn the_quit_sheet_lists_the_sessions_and_counts_the_ones_it_does_not_show(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    for index in 0..8 {
        daemon.sh_titled(&format!("job {index}"), "sleep 600");
    }
    let (view, cx) = open(cx, &daemon, 8);
    cx.dispatch_action(actions::Quit);
    cx.run_until_parked();
    settle(cx);

    let (rows, message) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (modal.rows.len(), modal.message.to_string())
    });
    assert_eq!(rows, 8, "every live session is a row of the dialog");
    assert!(message.starts_with("8 sessions are running."), "{message}");
    // At most six lines are drawn: five sessions and "and 3 more".
    let list = part(&view, cx, "rows");
    let card = part(&view, cx, "card");
    assert!(inside(list, card));
    let line = f32::from(list.size.height) / 6.0;
    assert!(
        (20.0..=40.0).contains(&line),
        "six lines of {line} pt make the list {:?}",
        list.size.height
    );
}

#[gpui::test]
fn a_session_line_of_the_quit_sheet_is_a_dot_an_icon_and_a_name_never_a_hollow_circle(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::sessions::Tone;
    use hutan_settings::themes::icons::IconId;

    let daemon = TestDaemon::start();
    daemon.sh_titled("build watcher", "sleep 600");
    daemon.sh_titled("server", "sleep 600");
    let (view, cx) = open(cx, &daemon, 2);
    cx.dispatch_action(actions::Quit);
    cx.run_until_parked();
    settle(cx);

    let rows = view.read_with(cx, |wb, _| wb.modal().unwrap().rows.clone());
    assert_eq!(rows.len(), 2);
    for (index, row) in rows.iter().enumerate() {
        // The icon says what the session is (a shell: the terminal icon), never a state glyph:
        // the hollow circles of the idle and to-do states read as radio buttons.
        assert_eq!(row.icon, IconId::SessionTerminal, "{row:?}");
        assert!(
            ![IconId::StateIdle, IconId::StateTodo, IconId::StateUnread].contains(&row.icon),
            "{row:?}"
        );
        // A shell that does nothing is idle: the muted tone, as a filled dot.
        assert_eq!(row.tone, Tone::Muted, "{row:?}");
        assert_eq!(row.status.as_ref(), "idle");

        let dot = bounds(&view, cx, Region::ModalItem("dot", index)).expect("a state dot");
        let icon = bounds(&view, cx, Region::ModalItem("icon", index)).expect("a session icon");
        assert_eq!(
            (f32::from(dot.size.width), f32::from(dot.size.height)),
            (8.0, 8.0),
            "a small dot"
        );
        assert_eq!(
            (f32::from(icon.size.width), f32::from(icon.size.height)),
            (14.0, 14.0)
        );
        assert!(
            dot.right() <= icon.left(),
            "the dot comes first: {dot:?} {icon:?}"
        );
        let centre = |b: Bounds<Pixels>| f32::from(b.top()) + f32::from(b.size.height) / 2.0;
        assert!(
            (centre(dot) - centre(icon)).abs() <= 1.0,
            "they share a line: {dot:?} {icon:?}"
        );
    }
}
