#![allow(clippy::expect_used, clippy::unwrap_used)] // tests
//! SCROLLBARS item 4, in the workbench: the lists that scroll (Search results, the palette, the
//! Settings page) have a scrollbar whose thumb drags to the last row and whose track pages, the
//! overlay style shows it while the list scrolls, and `appearance.scrollbars` and the color
//! pack's `ui.scrollbar.*` reach every bar live. The editor, the terminal and the chat have
//! their drags tested next to their views (`code_view`, `terminal_view`, `chat_gpui`).

use crate::common;

use std::path::Path;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};
use hutan_desktop::workbench::panels::SearchPanel;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_settings::{ScrollbarsSetting, SettingsPaths, SettingsStore};
use hutan_ui::Policy;

fn settings(daemon: &TestDaemon) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(daemon), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

/// Drags the thumb at `thumb` far past the end of its track (or, with `up`, past the start).
fn drag(cx: &mut VisualTestContext, thumb: gpui::Bounds<gpui::Pixels>, up: bool) {
    let x = thumb.center().x;
    cx.simulate_mouse_down(
        point(x, thumb.center().y),
        MouseButton::Left,
        Modifiers::none(),
    );
    let to = if up { px(-4_000.0) } else { px(8_000.0) };
    cx.simulate_mouse_move(point(x, to), Some(MouseButton::Left), Modifiers::none());
    draw(cx);
    cx.simulate_mouse_up(point(x, to), MouseButton::Left, Modifiers::none());
    draw(cx);
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[gpui::test]
fn the_palette_list_has_a_thumb_that_drags_to_the_last_command(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_palette(window, cx));
    draw(cx);
    draw(cx);
    let total = view.read_with(cx, |wb, _| wb.palette_titles().len());
    let thumb = view
        .read_with(cx, |wb, _| wb.palette_scrollbar().thumb())
        .expect("the list is longer than the window: it scrolls, and says so");
    assert_eq!(view.read_with(cx, |wb, _| wb.palette_scrolled()), px(0.0));

    drag(cx, thumb, false);
    assert!(
        view.read_with(cx, |wb, _| wb.palette_scrolled()) > px(100.0),
        "the list followed the thumb"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::PaletteRow(total - 1)))
            .is_some(),
        "the last command is on screen after the drag to the end"
    );
}

#[gpui::test]
fn the_search_results_have_a_thumb_that_drags_to_the_last_match(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for n in 0..300 {
        write(&root, &format!("src/f{n:03}.txt"), "needle here\n");
    }
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(root.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(root.as_path())
        })
    });
    cx.dispatch_action(actions::ShowSearch);
    draw(cx);
    draw(cx);
    let panel: Entity<SearchPanel> = view
        .read_with(cx, |wb, _| wb.panel::<SearchPanel>("search"))
        .expect("the Search panel is drawn");
    cx.simulate_input("needle");
    wait_until(cx, "300 files with a match", |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(200));
        draw(cx);
        panel.read_with(cx, |p, _| p.files().len() == 300 && !p.searching())
    });
    draw(cx);
    let thumb = panel
        .read_with(cx, |p, _| p.scrollbar().thumb())
        .expect("600 rows do not fit the panel");
    drag(cx, thumb, false);
    let (offset, max) = panel.read_with(cx, |p, _| p.scroll_range());
    assert!(max > 0.0);
    assert_eq!(offset, max, "the last row is the last in view");

    // And the thumb back to the top.
    let end = panel
        .read_with(cx, |p, _| p.scrollbar().thumb())
        .expect("scrolled");
    drag(cx, end, true);
    assert_eq!(panel.read_with(cx, |p, _| p.scroll_range().0), 0.0);
}

#[gpui::test]
fn the_settings_page_scrolls_with_its_thumb_and_pages_on_a_click_on_the_track(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("terminal"), window, cx);
    });
    draw(cx);
    draw(cx);
    let scroll = view.read_with(cx, |wb, _| wb.settings_sheet().map(|s| s.scroll().clone()));
    let scroll = scroll.expect("the sheet is open");
    let max = f32::from(scroll.max_offset().y);
    assert!(max > 0.0, "the Terminal page is longer than the sheet");
    let bounds = scroll.bounds();
    let x = bounds.right() - px(2.0 + 3.5);

    // A click on the track below the thumb: one page down.
    cx.simulate_click(point(x, bounds.bottom() - px(20.0)), Modifiers::none());
    draw(cx);
    let paged = -f32::from(scroll.offset().y);
    assert!(
        (paged - f32::from(bounds.size.height).min(max)).abs() < 1.0,
        "a page is the viewport: {paged} of {max} for {:?}",
        bounds.size.height
    );

    // A drag of the thumb past the end of the track reaches the end of the page. The thumb is
    // where the geometry puts it for this scroll position.
    let viewport = f32::from(bounds.size.height);
    let thumb = hutan_ui::scrollbar::geometry::thumb(
        hutan_ui::Metrics {
            viewport,
            max,
            offset: paged,
        },
        viewport - 4.0,
    )
    .expect("scrollable");
    let y = bounds.origin.y + px(2.0 + thumb.start + thumb.len / 2.0);
    cx.simulate_mouse_down(point(x, y), MouseButton::Left, Modifiers::none());
    assert!(
        cx.update(|_, _| true),
        "the window is still there after the press"
    );
    cx.simulate_mouse_move(
        point(x, px(8_000.0)),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw(cx);
    cx.simulate_mouse_up(point(x, px(8_000.0)), MouseButton::Left, Modifiers::none());
    assert_eq!(
        -f32::from(scroll.offset().y),
        max,
        "the last row of the page is the last in view"
    );
}

#[gpui::test]
fn appearance_scrollbars_and_the_pack_colors_reach_every_bar_live(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let store = settings(&daemon);
    let (_view, cx) = open_with(cx, store.clone(), daemon.connection(), None);
    // Until the user says otherwise the setting follows macOS, which the tests never read: the
    // overlay style, the macOS default.
    let policy = cx.update(|_, cx| Policy::of(cx));
    assert_eq!(policy.setting, ScrollbarsSetting::System);
    assert_ne!(
        policy.style,
        hutan_ui::Style::default(),
        "the pack's colors"
    );

    store
        .write_user(
            "appearance.scrollbars",
            toml::Value::String("always".into()),
        )
        .unwrap();
    wait_until(cx, "the setting to reach the bars", |cx| {
        cx.update(|_, cx| Policy::of(cx).setting == ScrollbarsSetting::Always)
    });
    store
        .write_user(
            "appearance.scrollbars",
            toml::Value::String("scrolling".into()),
        )
        .unwrap();
    wait_until(cx, "the setting to reach the bars", |cx| {
        cx.update(|_, cx| Policy::of(cx).setting == ScrollbarsSetting::Scrolling)
    });
    let overlay = cx.update(|_, cx| Policy::of(cx));
    assert_eq!(
        overlay.visibility(),
        hutan_ui::Visibility::Scrolling,
        "an explicit setting beats the system's"
    );
}
