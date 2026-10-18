#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The settings sheet as a management surface (ST-2): the layer each value comes from and
//! Reset that removes a key, validated choices and ranges, the key/value editor, history
//! indexing, the agents' hooks, the Data & logs page, the keymap's bindings, and the search
//! over pages the app draws itself. Everything runs against an in-process daemon and a
//! temporary data folder; the Finder and the system's "open" are the handlers a test installs.

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use common::history::HistoryEnv;
use common::{TestDaemon, draw, open_options, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::integrations::{FakeInstaller, IntegrationStatus};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::panels::HistoryPanel;
use hutan_desktop::workbench::{DataTarget, Region, Workbench, WorkbenchOptions};
use hutan_gateway::{HttpConfig, MemoryStore};
use hutan_protocol::history::HistoryStatusInfo;
use hutan_protocol::messages::{Empty, Request};
use hutan_protocol::provider::{ProviderAdd, ProviderResult, ProviderSetCredential};
use hutan_settings::{SettingsLayer, SettingsPaths, SettingsStore};
use serde_json::json;

fn store(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, store(daemon), daemon.connection(), None);
    wait_synced(&view, cx);
    (view, cx)
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

fn user_file(daemon: &TestDaemon) -> String {
    std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap_or_default()
}

fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn type_into_field(view: &Entity<Workbench>, cx: &mut VisualTestContext, field: &str, text: &str) {
    view.update(cx, |wb, cx| wb.sheet_begin_edit(field, cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text(text, cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
}

fn value(view: &Entity<Workbench>, cx: &VisualTestContext, field: &str) -> serde_json::Value {
    view.read_with(cx, |wb, _| wb.sheet_value(field)).unwrap()
}

fn modal_title(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.title.to_string()))
}

fn modal_message(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.modal().map(|m| m.message.to_string()))
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
}

// ---- layers and provenance ---------------------------------------------------------------------

#[gpui::test]
fn a_project_override_is_shown_edited_and_reset_in_its_own_file(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "# mine\n[terminal]\nfont_size = 15\n",
    );
    let project = tempfile::tempdir().unwrap();
    let file = project.path().join(".hutan/settings.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "# the project's\n[terminal]\nfont_size = 20\n[terminal.cursor]\nblink = false\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .set_project_root(Some(project.path().to_owned()));
    });
    open_page(&view, cx, "terminal");

    // The field says where its value comes from, and the file is the hover.
    let source = view
        .read_with(cx, |wb, cx| wb.sheet_source("font_size", cx))
        .unwrap();
    assert_eq!(source.layer, Some(SettingsLayer::Project));
    assert!(source.label.starts_with("Project"), "{}", source.label);
    assert_eq!(source.path.as_deref(), Some(file.as_path()));
    assert_eq!(value(&view, cx, "font_size"), json!(20.0));
    let other = view
        .read_with(cx, |wb, cx| wb.sheet_source("copy_on_select", cx))
        .unwrap();
    assert_eq!(other.layer, None, "nothing sets it");
    assert_eq!(other.label, "Default");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sheet_source("cursor.blink", cx))
            .unwrap()
            .layer,
        Some(SettingsLayer::Project)
    );

    // The control edits the layer the value comes from.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_edit_layer("font_size")),
        Some(SettingsLayer::Project)
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_edit_layer("copy_on_select")),
        Some(SettingsLayer::User),
        "a value nothing overrides is the user's"
    );
    type_into_field(&view, cx, "font_size", "22");
    assert_eq!(value(&view, cx, "font_size"), json!(22.0));
    let project_text = std::fs::read_to_string(&file).unwrap();
    assert!(
        project_text.contains("font_size = 22") && project_text.contains("# the project's"),
        "{project_text}"
    );
    assert!(
        user_file(&daemon).contains("font_size = 15"),
        "the user's file is untouched: {}",
        user_file(&daemon)
    );

    // The header offers the layers there are; the switch moves every control to one.
    let choices = view.read_with(cx, |wb, cx| wb.sheet_layer_choices(cx));
    assert!(
        choices
            .iter()
            .any(|(l, label)| *l == SettingsLayer::User && label == "You")
    );
    assert!(
        choices
            .iter()
            .any(|(l, label)| *l == SettingsLayer::Project && label.starts_with("Project"))
    );
    assert!(drawn(&view, cx, Region::SettingsItem("layer", 0)));
    assert!(drawn(&view, cx, Region::Settings("reset-page")));
    view.update(cx, |wb, cx| {
        wb.sheet_set_layer(Some(SettingsLayer::User), cx)
    });
    view.update(cx, |wb, cx| wb.sheet_toggle("copy_on_select", cx));
    assert!(user_file(&daemon).contains("copy_on_select = true"));
    assert!(
        !std::fs::read_to_string(&file)
            .unwrap()
            .contains("copy_on_select")
    );
    view.update(cx, |wb, cx| wb.sheet_set_layer(None, cx));

    // Reset removes the key from that file; the user's value shows again.
    assert!(view.read_with(cx, |wb, _| wb.sheet_has_key("font_size")));
    view.update(cx, |wb, cx| wb.sheet_reset("font_size", cx));
    let project_text = std::fs::read_to_string(&file).unwrap();
    assert!(!project_text.contains("font_size"), "{project_text}");
    assert!(project_text.contains("blink = false") && project_text.contains("# the project's"));
    assert_eq!(value(&view, cx, "font_size"), json!(15.0));
    let source = view
        .read_with(cx, |wb, cx| wb.sheet_source("font_size", cx))
        .unwrap();
    assert_eq!(source.layer, Some(SettingsLayer::User));
    assert_eq!(source.label, "You");
    assert!(
        user_file(&daemon).contains("font_size = 15"),
        "the user's value stays"
    );
}

#[gpui::test]
fn reset_removes_the_key_and_reset_page_asks_first(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "# mine\n[terminal]\nfont_size = 17\ncopy_on_select = true\n\n[appearance]\ndensity = \"comfortable\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "terminal");

    // Reset removes one key; it does not write the default.
    view.update(cx, |wb, cx| wb.sheet_reset("copy_on_select", cx));
    let text = user_file(&daemon);
    assert!(!text.contains("copy_on_select"), "{text}");
    assert!(
        text.contains("# mine") && text.contains("font_size = 17") && text.contains("density"),
        "{text}"
    );
    assert!(!view.read_with(cx, |wb, _| wb.sheet_has_key("copy_on_select")));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sheet_source("copy_on_select", cx))
            .unwrap()
            .label,
        "Default"
    );

    // Reset page lists what goes and waits for the yes.
    view.update_in(cx, |wb, window, cx| wb.sheet_reset_page(window, cx));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Reset this page?"));
    let message = modal_message(&view, cx).unwrap();
    assert!(message.contains("Font size (You)"), "{message}");
    assert!(!message.contains("Copy on select"), "not set: {message}");
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(
        user_file(&daemon).contains("font_size = 17"),
        "Cancel changes nothing"
    );

    view.update_in(cx, |wb, window, cx| wb.sheet_reset_page(window, cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    let text = user_file(&daemon);
    assert!(!text.contains("font_size"), "{text}");
    assert!(
        text.contains("# mine") && text.contains("density"),
        "{text}"
    );
    assert_eq!(value(&view, cx, "font_size"), json!(13.0));
    assert!(view.read_with(cx, |wb, _| wb.sheet_page_keys().is_empty()));

    // Nothing left to reset: a note, not a question.
    view.update_in(cx, |wb, window, cx| wb.sheet_reset_page(window, cx));
    assert!(modal_title(&view, cx).is_none());
    assert!(notice(&view, cx).is_some_and(|n| n.starts_with("Nothing on this page")));
}

// ---- validated choices and ranges ----------------------------------------------------------------

#[gpui::test]
fn known_sets_are_choices_and_numbers_stay_in_their_range(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::settings_schema::FieldKind;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);

    open_page(&view, cx, "terminal");
    let theme = view.read_with(cx, |wb, _| wb.sheet_kind("theme")).unwrap();
    assert_eq!(
        theme,
        FieldKind::Enum(vec![
            "auto".into(),
            "hutan-dark".into(),
            "hutan-light".into()
        ]),
        "the installed themes, not a text box"
    );
    view.update(cx, |wb, cx| wb.sheet_choose("theme", "solarized", cx));
    assert!(
        !user_file(&daemon).contains("solarized"),
        "an unknown theme is refused"
    );
    view.update(cx, |wb, cx| wb.sheet_choose("theme", "hutan-light", cx));
    assert!(user_file(&daemon).contains("theme = \"hutan-light\""));

    // The number says its range and a value outside it is kept inside.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_range("font_size")),
        Some((4.0, 72.0))
    );
    type_into_field(&view, cx, "font_size", "500");
    assert_eq!(value(&view, cx, "font_size"), json!(72.0));
    assert!(
        user_file(&daemon).contains("font_size = 72"),
        "{}",
        user_file(&daemon)
    );
    assert!(notice(&view, cx).is_some_and(|n| n.contains("Font size") && n.contains("72")));
    type_into_field(&view, cx, "font_size", "1");
    assert_eq!(value(&view, cx, "font_size"), json!(4.0));
    type_into_field(&view, cx, "line_height", "0.1");
    assert_eq!(value(&view, cx, "line_height"), json!(0.8));
    type_into_field(&view, cx, "line_height", "1.5");
    assert_eq!(
        value(&view, cx, "line_height"),
        json!(1.5),
        "inside the range as typed"
    );
    wait_until(cx, "the terminal style", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 4.0
    });

    // An integer too.
    open_page(&view, cx, "resources");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_range("interval_ms")),
        Some((1000.0, 60000.0))
    );
    type_into_field(&view, cx, "interval_ms", "5");
    assert_eq!(value(&view, cx, "interval_ms"), json!(1000));
    type_into_field(&view, cx, "interval_ms", "9999999");
    assert_eq!(value(&view, cx, "interval_ms"), json!(60000));
    // Text that is no number still stays in the box with its error.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("interval_ms", cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("often", cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    let error = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .edit()
            .and_then(|e| e.error.clone())
    });
    assert_eq!(error.as_deref(), Some("expected a whole number"));
    cx.simulate_keystrokes("escape");

    // The layout preset is picked from the presets there are, saved ones included.
    write(
        daemon.home(),
        "settings.toml",
        "[layout.presets.mine]\nleft = { panels = [\"sessions\"], size = 200.0, visible = true }\n",
    );
    open_page(&view, cx, "layout");
    wait_until(cx, "the saved preset to load", |cx| {
        view.read_with(cx, |wb, _| {
            matches!(wb.sheet_kind("preset"), Some(FieldKind::Enum(v)) if v.iter().any(|p| p == "mine"))
        })
    });
    let FieldKind::Enum(presets) = view.read_with(cx, |wb, _| wb.sheet_kind("preset")).unwrap()
    else {
        panic!("the preset is a choice");
    };
    for builtin in ["default", "focus", "overview", "mine"] {
        assert!(
            presets.iter().any(|p| p == builtin),
            "{builtin} in {presets:?}"
        );
    }
    view.update(cx, |wb, cx| wb.sheet_choose("preset", "nope", cx));
    assert!(!user_file(&daemon).contains("nope"));
    view.update(cx, |wb, cx| wb.sheet_choose("preset", "focus", cx));
    assert!(
        user_file(&daemon).contains("preset = \"focus\""),
        "{}",
        user_file(&daemon)
    );

    // Row parts and inspector tabs come from a menu of the ones that exist.
    open_page(&view, cx, "panels.sessions");
    assert!(matches!(
        view.read_with(cx, |wb, _| wb.sheet_kind("row")),
        Some(FieldKind::ChoiceList(parts)) if parts.len() == 6
    ));
    view.update(cx, |wb, cx| wb.sheet_add_list_item("row", "bogus", cx));
    view.update(cx, |wb, cx| wb.sheet_add_list_item("row", "title", cx));
    assert_eq!(
        value(&view, cx, "row"),
        json!(["state", "kind", "title", "folder", "placement", "age"]),
        "an unknown part and a part that is there are refused"
    );
    view.update(cx, |wb, cx| wb.sheet_remove_list_item("row", 5, cx));
    view.update(cx, |wb, cx| wb.sheet_add_list_item("row", "age", cx));
    assert_eq!(
        value(&view, cx, "row"),
        json!(["state", "kind", "title", "folder", "placement", "age"])
    );
    view.update(cx, |wb, cx| wb.sheet_begin_edit("row", cx));
    view.update(cx, |wb, cx| wb.sheet_remove_list_item("row", 0, cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("bogus", cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    let error = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .edit()
            .and_then(|e| e.error.clone())
    });
    assert!(
        error.is_some_and(|e| e.starts_with("one of:")),
        "typed text is checked too"
    );
    cx.simulate_keystrokes("escape");
    open_page(&view, cx, "panels.inspector");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_kind("tabs")),
        Some(FieldKind::ChoiceList(vec![
            "approvals".into(),
            "changes".into(),
            "info".into()
        ]))
    );
    view.update(cx, |wb, cx| wb.sheet_remove_list_item("tabs", 1, cx));
    view.update(cx, |wb, cx| wb.sheet_add_list_item("tabs", "bogus", cx));
    assert_eq!(value(&view, cx, "tabs"), json!(["approvals", "info"]));
    view.update(cx, |wb, cx| wb.sheet_add_list_item("tabs", "changes", cx));
    assert_eq!(
        value(&view, cx, "tabs"),
        json!(["approvals", "info", "changes"])
    );
}

// ---- the layout presets ---------------------------------------------------------------------------

#[gpui::test]
fn presets_are_saved_renamed_and_deleted_from_the_layout_page(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "layout");
    assert!(drawn(&view, cx, Region::Settings("preset-save")));

    // "Save current layout as…" also makes it the preset in use, as its doc says.
    cx.dispatch_action(actions::ToggleRightDock);
    view.update_in(cx, |wb, window, cx| wb.save_layout_as_preset(window, cx));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Save layout as preset")
    );
    view.update_in(cx, |wb, window, cx| {
        wb.set_modal_input("Mine!", cx);
        wb.confirm_modal(window, cx);
    });
    let text = user_file(&daemon);
    assert!(text.contains("[layout.presets.Mine-]"), "{text}");
    assert!(
        text.contains("preset = \"Mine-\""),
        "layout.preset follows: {text}"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.saved_layout_presets()),
        ["Mine-"]
    );

    // Rename keeps the table and follows the preset in use.
    draw(cx);
    assert!(drawn(&view, cx, Region::SettingsItem("preset-rename", 3)));
    view.update_in(cx, |wb, window, cx| {
        wb.ask_rename_layout_preset("Mine-", window, cx)
    });
    view.update_in(cx, |wb, window, cx| {
        wb.set_modal_input("calm", cx);
        wb.confirm_modal(window, cx);
    });
    let text = user_file(&daemon);
    assert!(
        text.contains("[layout.presets.calm]") && !text.contains("Mine-\""),
        "{text}"
    );
    assert!(text.contains("preset = \"calm\""), "{text}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.saved_layout_presets()),
        ["calm"]
    );

    // The preset in use cannot be deleted; another one can, after the question.
    assert!(!view.update(cx, |wb, cx| wb.delete_layout_preset("calm", cx)));
    assert!(notice(&view, cx).is_some_and(|n| n.contains("in use")));
    cx.dispatch_action(actions::ToggleBottomDock);
    view.update_in(cx, |wb, window, cx| wb.save_layout_as_preset(window, cx));
    view.update_in(cx, |wb, window, cx| {
        wb.set_modal_input("other", cx);
        wb.confirm_modal(window, cx);
    });
    view.update_in(cx, |wb, window, cx| {
        wb.ask_delete_layout_preset("calm", window, cx)
    });
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Delete the layout preset calm?")
    );
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    let text = user_file(&daemon);
    assert!(
        !text.contains("calm") && text.contains("[layout.presets.other]"),
        "{text}"
    );
}

// ---- terminal.env ------------------------------------------------------------------------------------

#[gpui::test]
fn the_variable_editor_adds_edits_and_removes_rows_and_warns_about_secrets(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "# mine\n[terminal.env]\nKEEP = \"1\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "terminal");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_map("env"))
            .unwrap()
            .get("KEEP")
            .map(String::as_str),
        Some("1")
    );
    assert!(drawn(&view, cx, Region::Settings("env-add")));
    assert!(words_warn().contains("secrets"));

    // "NAME=value" adds a row, the value of a row is edited in place, × removes it.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("env", cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("no equals sign", cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    assert!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .edit()
            .and_then(|e| e.error.clone()))
            .is_some()
    );
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("1BAD=x", cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    assert!(user_file(&daemon).contains("KEEP") && !user_file(&daemon).contains("1BAD"));
    view.update(cx, |wb, cx| {
        wb.sheet_set_edit_text("API_URL=http://localhost:1?a=b", cx)
    });
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    let text = user_file(&daemon);
    assert!(
        text.contains("# mine") && text.contains("KEEP = \"1\""),
        "{text}"
    );
    assert!(
        text.contains("API_URL = \"http://localhost:1?a=b\""),
        "{text}"
    );
    view.update(cx, |wb, cx| wb.sheet_begin_edit_entry("env", "KEEP", cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("2", cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    assert!(
        user_file(&daemon).contains("KEEP = \"2\""),
        "{}",
        user_file(&daemon)
    );
    assert!(drawn(&view, cx, Region::SettingsItem("env-remove", 1)));
    view.update(cx, |wb, cx| wb.sheet_map_remove("env", "API_URL", cx));
    let text = user_file(&daemon);
    assert!(!text.contains("API_URL") && text.contains("KEEP"), "{text}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_map("env").unwrap().len()),
        1
    );
    // Reset removes the whole map; a stale name or value is refused.
    assert!(
        view.update(cx, |wb, cx| wb.sheet_map_set("env", "A B", "x", cx))
            .is_err()
    );
    view.update(cx, |wb, cx| wb.sheet_reset("env", cx));
    assert!(!user_file(&daemon).contains("env") && !user_file(&daemon).contains("KEEP"));
}

fn words_warn() -> &'static str {
    hutan_desktop::workbench::settings_labels::words::ENV_WARNING
}

// ---- history indexing -------------------------------------------------------------------------------

#[gpui::test]
fn history_indexing_is_switched_on_and_off_from_the_history_page(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("history"), window, cx)
    });
    wait_until(cx, "the index status", |cx| {
        view.read_with(cx, |wb, _| wb.history_index().status.is_some())
    });
    assert!(view.read_with(cx, |wb, _| {
        wb.history_index().status.clone().unwrap().enabled
    }));
    assert!(drawn(&view, cx, Region::Settings("history-index")));

    // Off: the daemon says so, and the History panel follows.
    cx.dispatch_action(actions::OpenPanel("history".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn");
    wait_until(cx, "the panel's status", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| s.enabled))
    });
    click(&view, cx, Region::Settings("history-index"));
    wait_until(cx, "indexing to be off", |cx| {
        view.read_with(cx, |wb, _| {
            wb.history_index()
                .status
                .as_ref()
                .is_some_and(|s| !s.enabled)
                && !wb.history_index().busy
        })
    });
    let status: HistoryStatusInfo = daemon.call(Request::HistoryStatus(Empty {}));
    assert!(!status.enabled, "history.status says disabled");
    wait_until(cx, "the panel to follow", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| !s.enabled))
    });
    assert!(
        std::fs::read_to_string(&daemon.paths.config_file)
            .unwrap()
            .contains("enabled = false"),
        "the daemon records it in daemon.toml"
    );

    // On again.
    click(&view, cx, Region::Settings("history-index"));
    wait_until(cx, "indexing to be on", |cx| {
        view.read_with(cx, |wb, _| {
            wb.history_index()
                .status
                .as_ref()
                .is_some_and(|s| s.enabled)
                && !wb.history_index().busy
        })
    });
    let status: HistoryStatusInfo = daemon.call(Request::HistoryStatus(Empty {}));
    assert!(status.enabled, "history.status says enabled");
    wait_until(cx, "the panel to follow", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| s.enabled))
    });

    // The History panel can switch it off too.
    panel.update(cx, |p, cx| p.disable_indexing(cx));
    wait_until(cx, "the panel to disable", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| !s.enabled))
    });
    let status: HistoryStatusInfo = daemon.call(Request::HistoryStatus(Empty {}));
    assert!(!status.enabled);
}

// ---- agent integrations -----------------------------------------------------------------------------

#[gpui::test]
fn each_agent_has_hook_status_and_install_repair_uninstall_with_a_confirmation(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let installer = FakeInstaller::new();
    installer.set_status("omp", IntegrationStatus::Installed);
    installer.set_status("claude", IntegrationStatus::Partial);
    installer.set_status("codex", IntegrationStatus::NotInstalled);
    let options = WorkbenchOptions::builtin(store(&daemon), daemon.connection(), None)
        .with_installer(Arc::new(installer.clone()));
    let (view, cx) = open_options(cx, options);
    wait_synced(&view, cx);
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("integrations"), window, cx)
    });
    wait_until(cx, "the three statuses", |cx| {
        view.read_with(cx, |wb, _| {
            ["omp", "claude", "codex"]
                .iter()
                .all(|h| wb.integration_status(h).is_some())
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.integration_status("claude")),
        Some(IntegrationStatus::Partial)
    );
    // Installed: Uninstall. Outdated: Repair and Uninstall. Not installed: Install.
    assert!(drawn(&view, cx, Region::SettingsItem("hooks-uninstall", 0)));
    assert!(!drawn(&view, cx, Region::SettingsItem("hooks-install", 0)));
    assert!(drawn(&view, cx, Region::SettingsItem("hooks-repair", 1)));
    assert!(drawn(&view, cx, Region::SettingsItem("hooks-uninstall", 1)));
    assert!(drawn(&view, cx, Region::SettingsItem("hooks-install", 2)));
    assert!(!drawn(&view, cx, Region::SettingsItem("hooks-repair", 2)));

    // Repair asks first, naming the files, and Cancel changes nothing.
    click(&view, cx, Region::SettingsItem("hooks-repair", 1));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Repair the hooks of Claude Code?")
    );
    let message = modal_message(&view, cx).unwrap();
    assert!(
        message.contains("~/.claude/settings.json") && message.contains("backed up"),
        "{message}"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(installer.installs().is_empty());
    click(&view, cx, Region::SettingsItem("hooks-repair", 1));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the repair", |cx| {
        view.read_with(cx, |wb, _| {
            wb.integration_status("claude") == Some(IntegrationStatus::Installed)
                && !wb.integration_busy()
        })
    });
    assert_eq!(installer.installs(), ["claude"]);

    // Install names the Codex files; Uninstall names what it removes.
    click(&view, cx, Region::SettingsItem("hooks-install", 2));
    assert!(
        modal_message(&view, cx)
            .unwrap()
            .contains("~/.codex/hooks.json")
    );
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the install", |cx| {
        view.read_with(cx, |wb, _| {
            wb.integration_status("codex") == Some(IntegrationStatus::Installed)
                && !wb.integration_busy()
        })
    });
    click(&view, cx, Region::SettingsItem("hooks-uninstall", 0));
    assert!(
        modal_message(&view, cx)
            .unwrap()
            .contains("~/.omp/agent/extensions")
    );
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the uninstall", |cx| {
        view.read_with(cx, |wb, _| {
            wb.integration_status("omp") == Some(IntegrationStatus::NotInstalled)
                && !wb.integration_busy()
        })
    });
    let calls = installer.calls();
    assert!(
        calls.contains(&("uninstall", "omp".to_owned())),
        "{calls:?}"
    );

    // Turning the setting off uninstalls nothing, and the page says so.
    let before = installer.calls().len();
    view.update(cx, |wb, cx| wb.sheet_toggle("auto_install", cx));
    view.update(cx, |wb, cx| wb.sheet_toggle("auto_install", cx));
    cx.run_until_parked();
    assert!(
        installer.calls()[before..]
            .iter()
            .all(|(action, _)| *action != "uninstall"),
        "{:?}",
        installer.calls()
    );
}

// ---- Data & logs --------------------------------------------------------------------------------------

#[gpui::test]
fn the_data_page_reveals_and_opens_through_the_seams_and_guards_the_daemon(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    write(daemon.home(), "settings.toml", "# mine\n");
    let session = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon);
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        let r = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| r.borrow_mut().push(path.to_owned()));
        let o = Rc::clone(&opened);
        wb.set_open_handler(move |path, _| o.borrow_mut().push(path.to_owned()));
        wb.set_daemon_log_path(daemon_log(&daemon));
    });
    open_page(&view, cx, "data");
    wait_until(cx, "the daemon status", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_status().is_some())
    });

    // The places, and a Reveal and an Open for each.
    let home = daemon.home().to_owned();
    let place = |target: DataTarget| view.read_with(cx, |wb, _| wb.data_path(target)).unwrap();
    assert_eq!(place(DataTarget::Folder), home);
    assert_eq!(place(DataTarget::Settings), home.join("settings.toml"));
    assert_eq!(place(DataTarget::Keymap), home.join("keymap.toml"));
    assert_eq!(place(DataTarget::Daemon), home.join("daemon.toml"));
    assert_eq!(place(DataTarget::Logs), daemon.paths.log_dir);
    for index in 0..DataTarget::ALL.len() {
        assert!(
            drawn(&view, cx, Region::SettingsItem("reveal", index)),
            "reveal {index}"
        );
        assert!(
            drawn(&view, cx, Region::SettingsItem("open", index)),
            "open {index}"
        );
    }
    assert!(drawn(&view, cx, Region::Settings("restart-daemon")));
    assert!(drawn(&view, cx, Region::Settings("stop-daemon")));

    // Reveal calls the seam; what is not there yet is said, not revealed.
    click(&view, cx, Region::SettingsItem("reveal", 0));
    click(&view, cx, Region::SettingsItem("reveal", 1));
    assert_eq!(
        *revealed.borrow(),
        [home.clone(), home.join("settings.toml")]
    );
    click(&view, cx, Region::SettingsItem("reveal", 2));
    assert_eq!(revealed.borrow().len(), 2);
    assert!(notice(&view, cx).is_some_and(|n| n.contains("keymap.toml does not exist yet")));
    // Open creates Hutan's own files (a keymap starts as a comment) and hands them over.
    assert!(!home.join("keymap.toml").exists());
    click(&view, cx, Region::SettingsItem("open", 2));
    assert!(home.join("keymap.toml").exists());
    assert_eq!(*opened.borrow(), [home.join("keymap.toml")]);
    // daemon.toml is the daemon's: opened when it is there, never created.
    click(&view, cx, Region::SettingsItem("open", 3));
    assert_eq!(
        opened.borrow().len(),
        1 + usize::from(daemon.paths.config_file.exists())
    );
    let config = daemon.paths.config_file.clone();
    let _ = std::fs::remove_file(&config);
    let before = opened.borrow().len();
    click(&view, cx, Region::SettingsItem("open", 3));
    assert_eq!(opened.borrow().len(), before);
    assert!(!config.exists());
    assert!(notice(&view, cx).is_some_and(|n| n.contains("daemon.toml does not exist yet")));

    // Sizes: the database is a file; the index and the daemon come from its answers.
    assert!(
        view.read_with(cx, |wb, _| wb.database_bytes())
            .is_some_and(|n| n > 0)
    );
    wait_until(cx, "the index status", |cx| {
        view.read_with(cx, |wb, _| wb.history_index().status.is_some())
    });

    // The Daemon menu's flows, with their questions: sessions are running.
    wait_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| wb.live_sessions(cx) == 1)
    });
    // The buttons are at the foot of a scrolling page: the test calls what they call.
    view.update_in(cx, |wb, window, cx| wb.stop_daemon(window, cx));
    assert_eq!(modal_title(&view, cx).as_deref(), Some("Stop the daemon?"));
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    view.update_in(cx, |wb, window, cx| wb.restart_daemon(window, cx));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Restart the daemon?")
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    assert!(
        daemon.session(session.id).state.is_live(),
        "nothing was stopped"
    );
}

fn daemon_log(daemon: &TestDaemon) -> PathBuf {
    daemon.paths.log_file()
}

// ---- Keyboard -----------------------------------------------------------------------------------------

#[gpui::test]
fn the_keyboard_page_lists_keymap_bindings_and_search_filters_them(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "keymap.toml",
        "[[binding]]\nkeys = \"cmd-shift-j\"\naction = \"workbench::SplitDown\"\ncontext = \"Pane\"\n\n[[binding]]\nkeys = \"ctrl-alt-q\"\naction = \"workbench::ToggleRightDock\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        let o = Rc::clone(&opened);
        wb.set_open_handler(move |path, _| o.borrow_mut().push(path.to_owned()));
    });
    wait_until(cx, "the bindings", |cx| {
        view.read_with(cx, |wb, _| wb.custom_bindings().len() == 2)
    });
    let bindings = view.read_with(cx, |wb, _| wb.custom_bindings());
    assert_eq!(bindings[0].action, "workbench::SplitDown");
    assert_eq!(bindings[0].context.as_deref(), Some("Pane"));
    assert_eq!(bindings[1].context, None);

    // Searching for a binding finds the page; the list narrows to what matches.
    view.update_in(cx, |wb, window, cx| wb.open_settings(None, window, cx));
    view.update(cx, |wb, cx| wb.set_settings_query("ctrl-alt-q", cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("keymap".to_owned())
    );
    draw(cx);

    // Open and Reload are on the page; Reload reads the file again.
    assert!(drawn(&view, cx, Region::Settings("open-keymap")));
    click(&view, cx, Region::Settings("open-keymap"));
    assert_eq!(*opened.borrow(), [daemon.home().join("keymap.toml")]);
    write(
        daemon.home(),
        "keymap.toml",
        "[[binding]]\nkeys = \"cmd-shift-j\"\naction = \"workbench::SplitDown\"\n",
    );
    click(&view, cx, Region::Settings("reload-keymap"));
    assert_eq!(view.read_with(cx, |wb, _| wb.custom_bindings().len()), 1);
}

// ---- Models and Accounts ----------------------------------------------------------------------------

#[gpui::test]
fn settings_search_finds_the_models_and_accounts_pages_by_their_controls(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_settings(None, window, cx));
    for (query, page) in [
        ("api key", "models"),
        ("base url", "models"),
        ("endpoint", "models"),
        ("provider", "models"),
        ("model", "models"),
        ("login", "accounts"),
        ("logout", "accounts"),
        ("account", "accounts"),
        ("restart daemon", "data"),
        ("revoke", "launcher"),
        ("reveal in finder", "data"),
    ] {
        view.update(cx, |wb, cx| wb.set_settings_query(query, cx));
        let hits = view.read_with(cx, |wb, _| wb.sheet_pages());
        assert!(hits.contains(&page), "{query}: {hits:?}");
    }
    // "api key" is the Models page and nothing else; the sheet goes there.
    view.update(cx, |wb, cx| wb.set_settings_query("api key", cx));
    assert_eq!(view.read_with(cx, |wb, _| wb.sheet_pages()), ["models"]);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("models".to_owned())
    );
    view.update(cx, |wb, cx| wb.set_settings_query("", cx));
}

#[gpui::test]
fn clear_key_asks_before_it_removes_the_key(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_gateway(hutan_daemon::GatewayConfig {
        credentials: Some(Arc::new(MemoryStore::new())),
        http: HttpConfig {
            no_proxy: true,
            ..Default::default()
        },
        discovery: Some(Vec::new()),
    });
    let added: ProviderResult = daemon.call(Request::ProviderAdd(ProviderAdd {
        kind: "openai".into(),
        name: "Mine".into(),
        base_url: Some("http://127.0.0.1:9/v1".into()),
        default_model: None,
    }));
    let id = added.provider.id;
    let _: ProviderResult = daemon.call(Request::ProviderSetCredential(ProviderSetCredential {
        provider_id: id.clone(),
        secret: "sk-synthetic-settings-test-123456".into(),
    }));
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_models_settings(window, cx));
    wait_until(cx, "the provider list", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers()
                .iter()
                .any(|p| p.id == id && p.has_credential)
        })
    });

    view.update_in(cx, |wb, window, cx| wb.clear_provider_key(&id, window, cx));
    assert_eq!(
        modal_title(&view, cx).as_deref(),
        Some("Clear the API key?")
    );
    assert!(modal_message(&view, cx).unwrap().contains("Keychain"));
    // Cancel: the key is still there, here and in the daemon.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    cx.run_until_parked();
    let list: hutan_protocol::provider::ProviderList = daemon.call(Request::ProviderList(Empty {}));
    assert!(
        list.providers
            .iter()
            .any(|p| p.id == id && p.has_credential)
    );

    view.update_in(cx, |wb, window, cx| wb.clear_provider_key(&id, window, cx));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the key to go", |cx| {
        view.read_with(cx, |wb, _| {
            wb.providers()
                .iter()
                .any(|p| p.id == id && !p.has_credential)
        })
    });
    let list: hutan_protocol::provider::ProviderList = daemon.call(Request::ProviderList(Empty {}));
    assert!(
        list.providers
            .iter()
            .any(|p| p.id == id && !p.has_credential)
    );
}
