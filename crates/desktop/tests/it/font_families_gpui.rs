#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The code font and the interface font of Settings (FONT-PICKER-2): `editor.font_family` and
//! `appearance.ui_font_family` are picked like the terminal's, each over the color pack's font
//! (a setting wins over the pack), live, removable, and ignored with a warning while the family
//! is not installed.

use crate::common::{draw, wait_until};
use crate::font_picker_gpui::{click_region, fam, installed, open, open_page};

use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::ui::{code_family, prose_family, ui_family};
use hutan_desktop::workbench::{FontField, Region, Workbench};

fn open_picker(view: &Entity<Workbench>, cx: &mut VisualTestContext, field: FontField) {
    click_region(view, cx, Region::SettingsItem("font-picker", field.index()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_font_picker()),
        Some(field),
        "the picker opened"
    );
}

fn values(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    field: FontField,
    filter: &str,
) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.font_rows(field, filter)
            .into_iter()
            .map(|r| r.value)
            .collect()
    })
}

fn code_font(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| view.read(cx).code_font_family(cx))
}

// ---- the code font -------------------------------------------------------------------------------

#[gpui::test]
fn the_code_font_is_picked_from_the_monospaced_families_and_drawn_at_once(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_page(&view, cx, "editor");
    assert_eq!(code_font(&view, cx), "Menlo", "the pack's monospace font");
    open_picker(&view, cx, FontField::EditorFamily);
    assert_eq!(
        values(&view, cx, FontField::EditorFamily, ""),
        ["", "Courier New", "Fira Code", "Menlo"],
        "monospaced only, System default first while unset"
    );

    cx.simulate_keystrokes("f i r enter");
    draw(cx);
    assert!(
        fixture
            .settings_file()
            .contains("[editor]\nfont_family = \"Fira Code\""),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(code_font(&view, cx), "Fira Code");
    assert_eq!(code_family().as_deref(), Some("Fira Code"));
    assert_eq!(
        ui_family().as_ref(),
        ".SystemUIFont",
        "the interface is not the code font"
    );

    // System default takes the key out and the pack's font comes back.
    open_picker(&view, cx, FontField::EditorFamily);
    assert_eq!(
        values(&view, cx, FontField::EditorFamily, "")[..2],
        ["Fira Code", ""]
    );
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(
        !fixture.settings_file().contains("font_family"),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(code_font(&view, cx), "Menlo");
    assert_eq!(code_family(), None);
}

#[gpui::test]
fn a_code_font_in_the_file_is_followed_live_and_waits_for_the_family_to_be_installed(
    cx: &mut TestAppContext,
) {
    let (view, fixture, cx) = open(
        cx,
        "[editor]\nfont_family = \"Berkeley Mono\"\n",
        installed(),
    );
    open_page(&view, cx, "editor");
    // Not installed: the pack's font is drawn, and the row says so.
    assert_eq!(code_font(&view, cx), "Menlo");
    let warning = view.read_with(cx, |wb, _| wb.font_warning(FontField::EditorFamily));
    assert!(
        warning
            .as_deref()
            .is_some_and(|w| w.contains("Berkeley Mono") && w.contains("monospace font is drawn")),
        "{warning:?}"
    );

    // The font arrives: the code is drawn in it without touching a setting.
    let mut more = installed();
    more.push(fam("Berkeley Mono", true));
    fixture.fonts.set_families(more);
    wait_until(cx, "the code font to apply", |cx| {
        code_font(&view, cx) == "Berkeley Mono"
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_warning(FontField::EditorFamily)),
        None
    );

    // Edited in the file, it is live too.
    std::fs::write(
        fixture.home.join("settings.toml"),
        "[editor]\nfont_family = \"Courier New\"\n",
    )
    .unwrap();
    fixture.settings.reload();
    wait_until(cx, "the file's change to apply", |cx| {
        code_font(&view, cx) == "Courier New"
    });
}

// ---- the interface font --------------------------------------------------------------------------

#[gpui::test]
fn the_interface_font_is_picked_from_every_family_and_chat_prose_follows_it(
    cx: &mut TestAppContext,
) {
    let (view, fixture, cx) = open(cx, "[appearance]\nappearance = \"dark\"\n", installed());
    open_page(&view, cx, "appearance");
    assert_eq!(ui_family().as_ref(), ".SystemUIFont");
    open_picker(&view, cx, FontField::UiFamily);
    assert_eq!(
        values(&view, cx, FontField::UiFamily, ""),
        [
            "",
            "Avenir",
            "Courier New",
            "Fira Code",
            "Helvetica",
            "Menlo",
            "Zapfino"
        ],
        "every family: the interface is not monospaced"
    );
    cx.simulate_keystrokes("a v e enter");
    draw(cx);
    assert!(
        fixture
            .settings_file()
            .contains("ui_font_family = \"Avenir\""),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(ui_family().as_ref(), "Avenir");
    assert_eq!(
        prose_family().as_ref(),
        "Avenir",
        "the pack names no prose family of its own: prose follows the interface"
    );
    assert_eq!(
        code_font(&view, cx),
        "Menlo",
        "code is not the interface font"
    );

    // Removed: the pack's fonts again.
    open_picker(&view, cx, FontField::UiFamily);
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(!fixture.settings_file().contains("ui_font_family"));
    assert_eq!(ui_family().as_ref(), ".SystemUIFont");
    assert_eq!(prose_family().as_ref(), ".SystemUIFont");
}

#[gpui::test]
fn an_interface_font_that_is_not_installed_is_ignored_and_warned_about(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(
        cx,
        "[appearance]\nui_font_family = \"Nope Sans\"\n",
        installed(),
    );
    open_page(&view, cx, "appearance");
    assert_eq!(ui_family().as_ref(), ".SystemUIFont");
    let warning = view.read_with(cx, |wb, _| wb.font_warning(FontField::UiFamily));
    assert!(
        warning.as_deref().is_some_and(
            |w| w.contains("Nope Sans") && w.contains("the color theme's font is drawn")
        ),
        "{warning:?}"
    );
    // It is installed later: drawn at once.
    let mut more = installed();
    more.push(fam("Nope Sans", false));
    fixture.fonts.set_families(more);
    wait_until(cx, "the interface font to apply", |_| {
        ui_family().as_ref() == "Nope Sans"
    });
}

// ---- the rows are where the settings are ---------------------------------------------------------

#[gpui::test]
fn the_rows_are_in_their_sections_and_a_search_for_font_finds_all_three(cx: &mut TestAppContext) {
    let (view, _fixture, cx) = open(cx, "", installed());
    open_page(&view, cx, "editor");
    let editor_rows: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect()
    });
    let at = |name: &str| editor_rows.iter().position(|r| r == name).unwrap();
    assert_eq!(
        at("font_size"),
        at("font_family") + 1,
        "the family is the row before the size"
    );
    open_page(&view, cx, "appearance");
    let appearance_rows: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect()
    });
    assert!(appearance_rows.contains(&"ui_font_family".to_owned()));
}
