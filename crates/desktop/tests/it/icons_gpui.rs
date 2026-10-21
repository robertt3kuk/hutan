#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! TH-2 renderer fixture: a user icon pack replaces exactly the themed controls while their
//! geometry, click and keyboard behavior stay the same, and selecting it changes no color.

use crate::common;

use common::{draw, offline_connection, open_with, wait_until};
use gpui::{Modifiers, TestAppContext};
use hutan_desktop::icons::{self, Icon};
use hutan_desktop::workbench::Region;
use hutan_desktop::workbench::model::{DockPosition, ItemRef};
use hutan_desktop::workbench::style::UiStyle;

const CLOSE: &str = "<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\"><path d=\"M6 6 18 18M18 6 6 18\"/></svg>";

fn write_pack(dir: &std::path::Path) {
    let pack = dir.join("icon-themes/fixture");
    std::fs::create_dir_all(pack.join("icons")).unwrap();
    std::fs::write(
        pack.join("icon-theme.toml"),
        "schema_version = 1\nid = \"fixture\"\nname = \"Fixture\"\nauthors = [\"Hutan tests\"]\nlicense = \"MIT\"\nextends = \"builtin:phosphor\"\n\n[icons]\n\"dock.left\" = \"icons/close.svg\"\n\"action.close\" = \"icons/close.svg\"\n\"nav.search\" = \"icons/close.svg\"\n",
    )
    .unwrap();
    std::fs::write(pack.join("icons/close.svg"), CLOSE).unwrap();
}

#[gpui::test]
fn a_themed_dock_control_paints_the_user_svg_and_keeps_its_behavior(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let data = settings
        .paths()
        .as_ref()
        .and_then(|p| p.user.parent().map(std::path::Path::to_path_buf))
        .unwrap();
    write_pack(&data);
    settings
        .write_user(
            "appearance.icon_theme",
            toml::Value::String("fixture".into()),
        )
        .unwrap();
    let (view, cx) = open_with(cx, settings.clone(), connection, None);
    draw(cx);

    let dock = icons::path(Icon::DockLeft);
    assert!(dock.starts_with("theme-icons/fixture/"), "{dock}");
    assert_eq!(
        dock.replace("dock.left", "action.close"),
        icons::path(Icon::ActionClose).as_ref()
    );
    assert!(
        icons::path(Icon::ActionSearch).starts_with("theme-icons/phosphor/"),
        "`action.search` is a different id and keeps the default"
    );
    assert!(
        icons::path(Icon::NavSearch).starts_with("theme-icons/fixture/"),
        "the themed search control"
    );
    assert!(
        icons::path(Icon::NavTasks).starts_with("theme-icons/phosphor/"),
        "unchanged id"
    );
    wait_until(cx, "the themed dock icon to be rasterized", |_| {
        icons::rasterized(&dock)
    });

    let toggle = |view: &gpui::Entity<hutan_desktop::workbench::Workbench>,
                  cx: &gpui::VisualTestContext| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::DockToggle(DockPosition::Left))
        })
        .unwrap()
    };
    let before = toggle(&view, cx);
    let style = cx.update(|_, cx| format!("{:?}", UiStyle::get(cx).theme));
    cx.simulate_click(before.center(), Modifiers::none());
    assert!(
        !view.read_with(cx, |wb, _| wb.state().docks.left.visible),
        "click hides the sidebar"
    );
    draw(cx);
    cx.simulate_click(toggle(&view, cx).center(), Modifiers::none());
    assert!(
        view.read_with(cx, |wb, _| wb.state().docks.left.visible),
        "click shows it again"
    );
    cx.simulate_keystrokes("cmd-b");
    assert!(
        !view.read_with(cx, |wb, _| wb.state().docks.left.is_expanded()),
        "keyboard collapses"
    );
    cx.simulate_keystrokes("cmd-b");
    assert!(
        view.read_with(cx, |wb, _| wb.state().docks.left.is_expanded()),
        "keyboard expands"
    );

    // The jump palette (the Search row is gone: the filter field and ⌘P) opens from its action
    // and Escape closes it.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenJump);
    assert!(
        view.read_with(cx, |wb, _| wb.picker().is_some()),
        "the action opens search"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.picker().is_none()));

    // The themed close control of a tab still closes exactly its tab.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder("New tab")).unwrap();
        });
    });
    draw(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let tabs = |view: &gpui::Entity<hutan_desktop::workbench::Workbench>,
                cx: &gpui::VisualTestContext| {
        view.read_with(cx, |wb, _| wb.state().pane(pane).unwrap().items.len())
    };
    let before_close = tabs(&view, cx);
    let tab = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::Tab(pane, before_close - 1))
        })
        .unwrap();
    cx.simulate_click(
        gpui::point(tab.right() - gpui::px(10.0), tab.center().y),
        Modifiers::none(),
    );
    assert_eq!(tabs(&view, cx), before_close - 1, "× closes its tab");
    draw(cx);
    assert_eq!(
        before.size,
        toggle(&view, cx).size,
        "artwork never changes the hit target"
    );

    // A live save is picked up by the shared theme watcher and painted under a new cache key.
    std::fs::write(
        data.join("icon-themes/fixture/icons/close.svg"),
        CLOSE.replace("M6 6 18 18M18 6 6 18", "M4 4 20 20M20 4 4 20"),
    )
    .unwrap();
    wait_until(cx, "the saved SVG to be resolved", |_| {
        icons::path(Icon::DockLeft) != dock
    });
    let saved = icons::path(Icon::DockLeft);
    wait_until(cx, "the saved SVG to be painted", |_| {
        icons::rasterized(&saved)
    });

    // A malformed save warns and keeps the last-good artwork.
    std::fs::write(
        data.join("icon-themes/fixture/icons/close.svg"),
        "<svg><script/></svg>",
    )
    .unwrap();
    wait_until(cx, "the malformed save to be diagnosed", |_| {
        settings
            .errors()
            .iter()
            .any(|e| e.key_path == "icons.dock.left")
    });
    assert_eq!(icons::path(Icon::DockLeft), saved);
    settings
        .write_user(
            "appearance.icon_theme",
            toml::Value::String("phosphor".into()),
        )
        .unwrap();
    wait_until(cx, "the built-in icon to return", |_| {
        icons::path(Icon::DockLeft).starts_with("theme-icons/phosphor/")
    });
    assert_eq!(
        style,
        cx.update(|_, cx| format!("{:?}", UiStyle::get(cx).theme)),
        "icons changed no color"
    );
}

#[gpui::test]
fn agents_changes_requests_and_branches_have_distinct_default_artwork(cx: &mut TestAppContext) {
    let (connection, settings, _) = offline_connection();
    let (view, cx) = open_with(cx, settings, connection, None);
    let right = view.read_with(cx, |wb, _| wb.state().docks.right.panels.clone());
    assert!(!right.is_empty());
    for panel in right {
        assert_ne!(
            Icon::for_panel(panel.as_str()),
            Icon::StateInfo,
            "{panel} has a semantic icon"
        );
    }
    let key = |id| {
        icons::path(id)
            .rsplit_once('/')
            .map(|(hash, _)| hash.to_owned())
            .unwrap()
    };
    assert_ne!(key(Icon::PanelAgents), key(Icon::PanelChanges));
    assert_ne!(key(Icon::PanelAgents), key(Icon::VcsBranch));
    assert_ne!(key(Icon::PanelInspector), key(Icon::StateInfo));
}
