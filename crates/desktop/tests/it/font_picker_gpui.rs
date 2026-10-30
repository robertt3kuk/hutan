#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The font picker of Settings (FONT-PICKER), driven with the real keys and the real pointer on
//! a real workbench whose installed fonts are a fake: the list is read off the main thread and
//! follows the system, only the monospaced families are listed until *Show all*, the current
//! value comes first, typing searches, a pick writes the setting once (the system default
//! removes the key), a name that is not installed still works and is warned about, and
//! *Install font…* hands the file to the system.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{click_at, draw, offline_connection, open_options, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::macos::{FakeFonts, FontFamily, MacServices};
use hutan_desktop::workbench::{
    FontField, FontRowKind, Region, Workbench, WorkbenchOptions, actions,
};
use hutan_settings::SettingsStore;

const FIELD: FontField = FontField::TerminalFamily;

pub(crate) fn fam(name: &str, mono: bool) -> FontFamily {
    FontFamily::new(name, mono)
}

/// What a Mac has: a few of each, out of order, with a hidden system family and a duplicate.
pub(crate) fn installed() -> Vec<FontFamily> {
    vec![
        fam("Menlo", true),
        fam("Helvetica", false),
        fam(".SF NS Mono", true),
        fam("Fira Code", true),
        fam("Avenir", false),
        fam("Courier New", true),
        fam("Menlo", true),
        fam("Zapfino", false),
    ]
}

pub(crate) struct Fixture {
    pub(crate) home: PathBuf,
    pub(crate) settings: SettingsStore,
    pub(crate) fonts: FakeFonts,
}

impl Fixture {
    pub(crate) fn settings_file(&self) -> String {
        std::fs::read_to_string(self.home.join("settings.toml")).unwrap_or_default()
    }

    fn generation(&self) -> u64 {
        self.settings.changes().borrow().generation
    }
}

pub(crate) fn open<'a>(
    cx: &'a mut TestAppContext,
    settings_toml: &str,
    families: Vec<FontFamily>,
) -> (Entity<Workbench>, Fixture, &'a mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    let home = settings
        .paths()
        .unwrap()
        .user
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::write(home.join("settings.toml"), settings_toml).unwrap();
    settings.reload();
    let fonts = FakeFonts::new(families);
    let mac = MacServices {
        fonts: Arc::new(fonts.clone()),
        ..MacServices::inert()
    };
    let options = WorkbenchOptions::builtin(settings.clone(), connection, None).with_mac(mac);
    let (view, cx) = open_options(cx, options);
    (
        view,
        Fixture {
            home,
            settings,
            fonts,
        },
        cx,
    )
}

fn open_terminal_page(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    open_page(view, cx, "terminal");
}

pub(crate) fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
    wait_until(cx, "the installed fonts to be read", |cx| {
        view.read_with(cx, |wb, _| wb.fonts_loaded())
    });
}

pub(crate) fn click_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    click_at(cx, bounds.center());
    draw(cx);
}

/// Opens the picker the way the head's click does.
fn open_picker(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    click_region(view, cx, Region::SettingsItem("font-picker", FIELD.index()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_font_picker()),
        Some(FIELD),
        "the picker opened"
    );
}

fn values(view: &Entity<Workbench>, cx: &VisualTestContext, filter: &str) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.font_rows(FIELD, filter)
            .into_iter()
            .map(|r| r.value)
            .collect()
    })
}

fn cursor(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, _| wb.font_cursor().map(str::to_owned))
}

fn names(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| {
        wb.font_families().iter().map(|f| f.name.clone()).collect()
    })
}

// ---- the list ------------------------------------------------------------------------------------

#[gpui::test]
fn the_list_is_read_once_sorted_and_follows_the_systems_changes(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    assert!(
        !view.read_with(cx, |wb, _| wb.fonts_loaded()),
        "nothing is read before Settings opens"
    );
    assert_eq!(fixture.fonts.reads(), 0);
    open_terminal_page(&view, cx);
    assert_eq!(
        names(&view, cx),
        [
            "Avenir",
            "Courier New",
            "Fira Code",
            "Helvetica",
            "Menlo",
            "Zapfino"
        ],
        "sorted, without the hidden family and the duplicate"
    );
    assert_eq!(fixture.fonts.reads(), 1);
    let generation = view.read_with(cx, |wb, _| wb.fonts_generation());

    // Settings closed and opened again: the list is kept, not read again.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    assert_eq!(fixture.fonts.reads(), 1);

    // The system says a font was added: the list is read again.
    let mut more = installed();
    more.push(fam("Berkeley Mono", true));
    fixture.fonts.set_families(more);
    wait_until(cx, "the list to follow the system", |cx| {
        view.read_with(cx, |wb, _| wb.fonts_generation()) > generation
    });
    assert!(names(&view, cx).contains(&"Berkeley Mono".to_owned()));
    assert_eq!(names(&view, cx).iter().filter(|n| *n == "Menlo").count(), 1);
}

// ---- what the picker lists -----------------------------------------------------------------------

#[gpui::test]
fn only_monospaced_families_are_listed_until_show_all_and_the_current_value_is_first(
    cx: &mut TestAppContext,
) {
    let (view, _fixture, cx) = open(cx, "[terminal]\nfont_family = \"Helvetica\"\n", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    // Helvetica is not monospaced but it is the current value: first. Then System default.
    assert_eq!(
        values(&view, cx, ""),
        ["Helvetica", "", "Courier New", "Fira Code", "Menlo"]
    );
    let kinds: Vec<FontRowKind> = view.read_with(cx, |wb, _| {
        wb.font_rows(FIELD, "")
            .into_iter()
            .map(|r| r.kind)
            .collect()
    });
    assert_eq!(kinds[0], FontRowKind::Family { monospaced: false });
    assert_eq!(kinds[1], FontRowKind::SystemDefault);

    // Show all: a click on the button lists every family; a second click hides them again.
    click_region(&view, cx, Region::SettingsItem("font-all", FIELD.index()));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().font_show_all()));
    assert_eq!(
        values(&view, cx, ""),
        [
            "Helvetica",
            "",
            "Avenir",
            "Courier New",
            "Fira Code",
            "Menlo",
            "Zapfino"
        ]
    );
    click_region(&view, cx, Region::SettingsItem("font-all", FIELD.index()));
    assert!(!view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().font_show_all()));
}

#[gpui::test]
fn while_unset_system_default_is_the_current_value_and_comes_first(cx: &mut TestAppContext) {
    let (view, _fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    assert_eq!(
        values(&view, cx, ""),
        ["", "Courier New", "Fira Code", "Menlo"]
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.font_explicit(FIELD)), None);
}

// ---- searching and picking -----------------------------------------------------------------------

#[gpui::test]
fn typing_searches_the_families_and_return_writes_the_setting_once(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    let before = fixture.generation();

    cx.simulate_keystrokes("f i r");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_filter().to_owned()),
        "fir"
    );
    assert_eq!(values(&view, cx, "fir"), ["Fira Code"]);
    assert_eq!(cursor(&view, cx).as_deref(), Some("Fira Code"));
    cx.simulate_keystrokes("x backspace");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_filter().to_owned()),
        "fir"
    );
    assert_eq!(
        fixture.generation(),
        before,
        "nothing is written by searching"
    );

    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(
        fixture
            .settings_file()
            .contains("font_family = \"Fira Code\""),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(fixture.settings_file().matches("font_family").count(), 1);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_font_picker()),
        None,
        "the picker closed"
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.font_filter().to_owned()), "");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_explicit(FIELD))
            .as_deref(),
        Some("Fira Code")
    );
    let written = fixture.generation();

    // Choosing what is already set writes nothing.
    open_picker(&view, cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(fixture.generation(), written);
}

#[gpui::test]
fn the_arrow_keys_move_through_the_list_and_a_click_on_a_family_picks_it(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    cx.simulate_keystrokes("down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("Courier New"));
    cx.simulate_keystrokes("down up up");
    assert_eq!(
        cursor(&view, cx).as_deref(),
        Some(""),
        "the list starts at System default"
    );
    cx.simulate_keystrokes("down down down");
    assert_eq!(cursor(&view, cx).as_deref(), Some("Menlo"));
    // Nothing was written by moving.
    assert_eq!(fixture.settings_file(), "");

    // The lines are System default, Courier New, Fira Code, Menlo: the third is index 2.
    click_region(&view, cx, Region::SettingsItem("font-option", 2));
    assert!(
        fixture
            .settings_file()
            .contains("font_family = \"Fira Code\""),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.open_font_picker()), None);
}

#[gpui::test]
fn system_default_removes_the_key(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "[terminal]\nfont_family = \"Menlo\"\n", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    assert_eq!(values(&view, cx, "")[..2], ["Menlo", ""]);
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(
        !fixture.settings_file().contains("font_family"),
        "{}",
        fixture.settings_file()
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.font_explicit(FIELD)), None);
}

#[gpui::test]
fn escape_and_a_click_outside_close_the_picker_without_writing(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.open_font_picker()), None);
    assert_eq!(fixture.settings_file(), "");
    assert!(
        view.read_with(cx, |wb, _| wb.settings_sheet().is_some()),
        "Escape closed the picker, not Settings"
    );

    open_picker(&view, cx);
    click_at(cx, gpui::point(gpui::px(2.0), gpui::px(2.0)));
    draw(cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.open_font_picker()), None);
    assert_eq!(fixture.settings_file(), "");
}

#[gpui::test]
fn the_wheel_scrolls_a_long_list_and_the_arrow_keys_keep_the_cursor_in_view(
    cx: &mut TestAppContext,
) {
    let many: Vec<FontFamily> = (0..40)
        .map(|i| fam(&format!("Mono {i:02}"), true))
        .collect();
    let (view, _fixture, cx) = open(cx, "", many);
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.font_top()), 0);
    view.update(cx, |wb, cx| wb.font_wheel(26.0 * 3.0, cx));
    assert_eq!(view.read_with(cx, |wb, _| wb.font_top()), 3);
    view.update(cx, |wb, cx| wb.font_wheel(-26.0 * 10.0, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_top()),
        0,
        "stops at the top"
    );
    view.update(cx, |wb, cx| wb.font_wheel(26.0 * 1000.0, cx));
    let total = values(&view, cx, "").len();
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_top()),
        total - 8,
        "stops with the last line at the bottom"
    );
    // Down from the top of the list follows the cursor: after eight moves it has scrolled.
    view.update(cx, |wb, cx| wb.font_wheel(-26.0 * 1000.0, cx));
    for _ in 0..9 {
        cx.simulate_keystrokes("down");
    }
    assert_eq!(cursor(&view, cx).as_deref(), Some("Mono 08"));
    assert_eq!(view.read_with(cx, |wb, _| wb.font_top()), 2);
}

// ---- a name that is not installed ----------------------------------------------------------------

#[gpui::test]
fn a_typed_name_that_is_not_installed_still_works_and_is_warned_about(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    open_picker(&view, cx);
    cx.simulate_keystrokes("b e r k e l e y space m o n o");
    let rows = view.read_with(cx, |wb, _| wb.font_rows(FIELD, "berkeley mono"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, FontRowKind::Typed { installed: false });
    assert_eq!(rows[0].label, "Use “berkeley mono”");
    assert_eq!(cursor(&view, cx).as_deref(), Some("berkeley mono"));
    assert_eq!(view.read_with(cx, |wb, _| wb.font_warning(FIELD)), None);

    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(
        fixture
            .settings_file()
            .contains("font_family = \"berkeley mono\""),
        "{}",
        fixture.settings_file()
    );
    let warning = view.read_with(cx, |wb, _| wb.font_warning(FIELD));
    assert!(
        warning
            .as_deref()
            .is_some_and(|w| w.contains("berkeley mono") && w.contains("not installed")),
        "{warning:?}"
    );
    // The row lists it first, marked missing.
    open_picker(&view, cx);
    let kinds = view.read_with(cx, |wb, _| wb.font_rows(FIELD, "")[0].kind);
    assert_eq!(kinds, FontRowKind::Missing);
    cx.simulate_keystrokes("escape");

    // Installing it takes the warning away.
    let mut more = installed();
    more.push(fam("Berkeley Mono", true));
    fixture.fonts.set_families(more);
    wait_until(cx, "the warning to go", |cx| {
        view.read_with(cx, |wb, _| wb.font_warning(FIELD)).is_none()
    });
}

#[gpui::test]
fn a_family_the_system_hides_is_never_warned_about(cx: &mut TestAppContext) {
    let (view, _fixture, cx) = open(
        cx,
        "[terminal]\nfont_family = \".SystemUIFont\"\n",
        installed(),
    );
    open_terminal_page(&view, cx);
    assert_eq!(view.read_with(cx, |wb, _| wb.font_warning(FIELD)), None);
}

#[gpui::test]
fn a_system_that_lists_nothing_is_never_warned_about(cx: &mut TestAppContext) {
    let (view, _fixture, cx) = open(cx, "[terminal]\nfont_family = \"Whatever\"\n", Vec::new());
    open_terminal_page(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.font_warning(FIELD)),
        None,
        "nothing is known about the installed fonts"
    );
}

// ---- the text box is gone ------------------------------------------------------------------------

#[gpui::test]
fn beginning_to_edit_the_font_opens_the_picker_instead_of_a_text_box(cx: &mut TestAppContext) {
    let (view, _fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    view.update(cx, |wb, cx| wb.sheet_begin_edit("font_family", cx));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.open_font_picker()),
        Some(FIELD)
    );
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().edit().is_none()));
}

// ---- Install font… -------------------------------------------------------------------------------

#[gpui::test]
fn install_font_hands_the_file_to_the_system_and_the_list_follows(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    let path = Path::new("/tmp/hutan-fake/Scratch Mono.ttf").to_path_buf();
    view.update(cx, |wb, cx| wb.font_install(path.clone(), cx));
    wait_until(cx, "the font to be installed", |cx| {
        view.read_with(cx, |wb, _| {
            wb.font_families().iter().any(|f| f.name == "Scratch Mono")
        })
    });
    assert_eq!(fixture.fonts.installed(), [path]);
    let notice = view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string));
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("Scratch Mono.ttf")),
        "{notice:?}"
    );
}

#[gpui::test]
fn a_font_the_system_refuses_says_why_and_changes_nothing(cx: &mut TestAppContext) {
    let (view, fixture, cx) = open(cx, "", installed());
    open_terminal_page(&view, cx);
    fixture
        .fonts
        .refuse_installs(Some("Could not install x.ttf: not a font"));
    view.update(cx, |wb, cx| {
        wb.font_install(PathBuf::from("/tmp/x.ttf"), cx)
    });
    wait_until(cx, "the refusal to be said", |cx| {
        view.read_with(cx, |wb, _| wb.notice().is_some())
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .as_deref(),
        Some("Could not install x.ttf: not a font")
    );
    assert!(fixture.fonts.installed().is_empty());
    assert_eq!(names(&view, cx).len(), 6);
}
