//! The font pickers of the Settings sheet (FONT-PICKER): a font is chosen from the families
//! installed on this Mac, never typed.
//!
//! A picker is a dropdown of the sheet with a richer body, like the theme pickers: a search box,
//! the families drawn each in its own face (the monospaced ones; *Show all* lists every family),
//! the current value first, *System default*, a preview line in the family under the cursor and
//! *Install font…*. A name typed into the search box that is no installed family is offered as
//! *Use “name”*: it works, and the row warns that it is not installed.
//!
//! The list is read from the system off the main thread, kept, and read again when the system
//! says fonts were added or removed (`crate::macos::fonts`). Nothing is written until a family
//! is picked; Escape and a click outside drop the picker.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, div, prelude::*, px,
};

use super::render::probe;
use super::settings_labels::{field_help, field_title};
use super::settings_rows::{SheetRow, menu_option, menu_panel, note};
use super::settings_schema::Field;
use super::ui::{Button, FocusRingExt as _, StyledExt as _};
use super::view::{Region, Workbench};
use crate::macos::FontFamily;
use crate::macos::fonts::tidy;
use crate::settings::TerminalSettings;
use crate::theme::DensityExt as _;

/// A setting that is picked from the installed font families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontField {
    /// `terminal.font_family`
    TerminalFamily,
    /// `editor.font_family`: code in file tabs, diffs and chat code.
    EditorFamily,
    /// `appearance.ui_font_family`: the interface (and prose with it).
    UiFamily,
}

impl FontField {
    pub const ALL: [FontField; 3] = [Self::TerminalFamily, Self::EditorFamily, Self::UiFamily];

    /// Position in [`Self::ALL`]: the number that tells the controls of different rows apart.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }

    /// The picker of the setting `key` (dotted inside its namespace), if it has one.
    pub fn of(namespace: &str, key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.namespace() == namespace && f.key() == key)
    }

    pub const fn namespace(self) -> &'static str {
        match self {
            Self::TerminalFamily => "terminal",
            Self::EditorFamily => "editor",
            Self::UiFamily => "appearance",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::TerminalFamily | Self::EditorFamily => "font_family",
            Self::UiFamily => "ui_font_family",
        }
    }

    /// The setting's path in the settings file.
    fn key_path(self) -> String {
        format!("{}.{}", self.namespace(), self.key())
    }

    /// The picker lists only monospaced families until *Show all* is on.
    pub const fn monospaced_by_default(self) -> bool {
        match self {
            Self::TerminalFamily | Self::EditorFamily => true,
            Self::UiFamily => false,
        }
    }

    /// What is drawn while the family is not installed.
    const fn fallback(self) -> &'static str {
        match self {
            Self::TerminalFamily => "the interface font is drawn",
            Self::EditorFamily => "the color theme's monospace font is drawn",
            Self::UiFamily => "the color theme's font is drawn",
        }
    }
}

/// The value of the *System default* line: no family, so the key is removed. No family name is
/// empty.
pub const SYSTEM_DEFAULT: &str = "";

/// What a line of a font picker is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontRowKind {
    /// The setting is not set: the color pack's font applies.
    SystemDefault,
    /// An installed family.
    Family { monospaced: bool },
    /// The value the setting holds, which is no installed family.
    Missing,
    /// The text typed into the search box, offered as the value.
    Typed { installed: bool },
}

/// One line of a font picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontRow {
    /// The value written when it is picked: a family name, or [`SYSTEM_DEFAULT`].
    pub value: String,
    pub label: String,
    pub kind: FontRowKind,
}

/// *System default*, with the family it stands for unless that is a hidden system one
/// (`.SystemUIFont`).
fn system_default_label(family: &str) -> String {
    if family.starts_with('.') {
        "System default".to_owned()
    } else {
        format!("System default ({family})")
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn family_kind(family: &FontFamily) -> FontRowKind {
    FontRowKind::Family {
        monospaced: family.monospaced,
    }
}

/// The lines of a picker over `families` for the search `filter`: the current value first (the
/// family the setting holds, or *System default* while it is not set), then *System default*,
/// then every family that fits — only the monospaced ones when `mono_only` — in name order, and
/// last the typed text as `Use “text”` when no family fits it, so a name the system does not
/// list can still be used.
pub fn font_rows(
    families: &[FontFamily],
    current: Option<&str>,
    filter: &str,
    mono_only: bool,
) -> Vec<FontRow> {
    let typed = filter.trim();
    let needle = typed.to_lowercase();
    let fits = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);
    let installed = |name: &str| families.iter().find(|f| same_name(&f.name, name));
    let mut rows = Vec::new();

    let system = FontRow {
        value: SYSTEM_DEFAULT.to_owned(),
        label: "System default".to_owned(),
        kind: FontRowKind::SystemDefault,
    };
    let system_fits = fits("system default");
    match current {
        None => {
            if system_fits {
                rows.push(system);
            }
        }
        Some(name) => {
            if fits(name) {
                rows.push(FontRow {
                    value: name.to_owned(),
                    label: name.to_owned(),
                    kind: installed(name).map_or(FontRowKind::Missing, family_kind),
                });
            }
            if system_fits {
                rows.push(system);
            }
        }
    }
    rows.extend(
        families
            .iter()
            .filter(|f| (!mono_only || f.monospaced) && fits(&f.name))
            .filter(|f| !current.is_some_and(|c| same_name(c, &f.name)))
            .map(|f| FontRow {
                value: f.name.clone(),
                label: f.name.clone(),
                kind: family_kind(f),
            }),
    );
    let listed = rows
        .iter()
        .any(|r| matches!(r.kind, FontRowKind::Family { .. } | FontRowKind::Missing));
    if !typed.is_empty() && !listed {
        let is_installed = installed(typed).is_some();
        rows.push(FontRow {
            value: typed.to_owned(),
            label: format!("Use “{typed}”"),
            kind: FontRowKind::Typed {
                installed: is_installed,
            },
        });
    }
    rows
}

/// The installed families, read off the main thread and kept.
#[derive(Default)]
pub(crate) struct FontCatalog {
    families: Option<Arc<Vec<FontFamily>>>,
    /// How many times the list was replaced (a test waits on it).
    generation: u64,
    loading: bool,
    /// A change arrived while the list was being read: read it again.
    stale: bool,
    watching: bool,
    /// Keeps the system's change observer alive.
    observer: Option<Box<dyn Any>>,
    watch_task: Option<Task<()>>,
    load_task: Option<Task<()>>,
    install_task: Option<Task<()>>,
}

impl std::fmt::Debug for FontCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCatalog")
            .field("families", &self.families.as_ref().map(|f| f.len()))
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Lines of a picker's list shown at a time.
const SHOWN: usize = 8;
/// The height of one line of the list, in pixels (what the wheel scrolls by).
const LINE_HEIGHT: f32 = 26.0;
/// What the preview's caption calls a family: the system's own hidden families (the interface
/// font of the system is `.SystemUIFont`) are the system font, not an internal name.
fn family_caption(family: &str) -> String {
    if family.starts_with('.') {
        "System font".to_owned()
    } else {
        family.to_owned()
    }
}

/// The text of the preview line.
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog 0123456789 {}[]() <= != -> =>";

impl Workbench {
    // ---- the catalog ----------------------------------------------------------------------

    /// Starts watching the installed fonts and reads the list, once; later changes read it again.
    pub(crate) fn ensure_fonts(&mut self, cx: &mut Context<Self>) {
        if !self.fonts.watching {
            self.fonts.watching = true;
            let (changed, mut changes) = tokio::sync::mpsc::channel(1);
            self.fonts.observer = self.mac.fonts.observe(changed);
            self.fonts.watch_task = Some(cx.spawn(async move |this, cx| {
                while changes.recv().await.is_some() {
                    if this.update(cx, |this, cx| this.load_fonts(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        if self.fonts.families.is_none() {
            self.load_fonts(cx);
        }
    }

    /// Reads the installed families on the background executor.
    fn load_fonts(&mut self, cx: &mut Context<Self>) {
        if self.fonts.loading {
            self.fonts.stale = true;
            return;
        }
        self.fonts.loading = true;
        let source = Arc::clone(&self.mac.fonts);
        self.fonts.load_task = Some(cx.spawn(async move |this, cx| {
            let list = cx
                .background_executor()
                .spawn(async move { tidy(source.families()) })
                .await;
            this.update(cx, |this, cx| {
                this.fonts.loading = false;
                this.fonts.families = Some(Arc::new(list));
                this.fonts.generation += 1;
                // A family the settings name may have just become installed (or gone).
                this.sync_fonts(cx);
                if std::mem::take(&mut this.fonts.stale) {
                    this.load_fonts(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The installed families, sorted; empty until the first read is done.
    pub fn font_families(&self) -> &[FontFamily] {
        self.fonts.families.as_deref().map_or(&[], Vec::as_slice)
    }

    /// Whether the list was read at least once.
    pub fn fonts_loaded(&self) -> bool {
        self.fonts.families.is_some()
    }

    /// How many times the list was read (and replaced).
    pub fn fonts_generation(&self) -> u64 {
        self.fonts.generation
    }

    /// Whether the list says `name` is an installed family (`false` while it is not known).
    pub(crate) fn font_listed(&self, name: &str) -> bool {
        self.font_families()
            .iter()
            .any(|f| same_name(&f.name, name))
    }

    /// Whether `name` is an installed family. `true` while nothing is known (no list yet, or a
    /// system that cannot list) and for the system's hidden families (`.SystemUIFont`).
    pub fn font_installed(&self, name: &str) -> bool {
        let families = self.font_families();
        families.is_empty()
            || name.starts_with('.')
            || families.iter().any(|f| same_name(&f.name, name))
    }

    // ---- reading the picker ---------------------------------------------------------------

    /// The font picker open on the selected page, if one is.
    pub fn open_font_picker(&self) -> Option<FontField> {
        let sheet = self.sheet.as_ref()?;
        let tag = sheet.dropdown.as_deref()?;
        let field = sheet.field(tag)?;
        FontField::of(field.namespace, &field.dotted())
    }

    /// The family the picker's cursor is on (keys or pointer); `None` while it has not moved.
    pub fn font_cursor(&self) -> Option<&str> {
        let sheet = self.sheet.as_ref()?;
        let (tag, value) = sheet.cursor.as_ref()?;
        (sheet.dropdown.as_ref() == Some(tag)).then_some(value.as_str())
    }

    /// The text typed into the picker's search box.
    pub fn font_filter(&self) -> &str {
        self.sheet.as_ref().map_or("", |s| s.pack_filter.as_str())
    }

    /// The first line of the picker's list that is shown.
    pub fn font_top(&self) -> usize {
        self.sheet.as_ref().map_or(0, |s| s.font_top)
    }

    /// What the files set for `field`, `None` while none does (the system default applies).
    pub fn font_explicit(&self, field: FontField) -> Option<String> {
        self.settings.source_of(&field.key_path())?;
        self.sheet_value(field.key())
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|name| !name.is_empty())
    }

    /// The family drawn while the setting is not set: the color pack's monospace font, or for
    /// the interface its UI font.
    pub fn font_default_family(&self, field: FontField, cx: &gpui::App) -> String {
        match field {
            FontField::TerminalFamily | FontField::EditorFamily => {
                super::terminals::mono_family(cx, &self.resolved_theme)
            }
            FontField::UiFamily => {
                super::ui::pick_family(cx, &self.resolved_theme.fonts().ui).to_string()
            }
        }
    }

    /// The size of the preview text: the setting's, in points.
    fn font_preview_size(&self, field: FontField) -> f32 {
        match field {
            FontField::TerminalFamily => self.settings.get::<TerminalSettings>().font_size as f32,
            FontField::EditorFamily => {
                self.settings
                    .get::<crate::settings::EditorSettings>()
                    .font_size as f32
            }
            FontField::UiFamily => self.density.ui().font_size,
        }
    }

    /// The lines of `field`'s picker for the search `filter`.
    pub fn font_rows(&self, field: FontField, filter: &str) -> Vec<FontRow> {
        let all = self.sheet.as_ref().is_some_and(|s| s.font_all);
        font_rows(
            self.font_families(),
            self.font_explicit(field).as_deref(),
            filter,
            field.monospaced_by_default() && !all,
        )
    }

    /// The options of the open picker for the arrow keys, `(value, label)`.
    pub(super) fn font_options(&self, field: FontField) -> Vec<(String, String)> {
        let filter = self.font_filter().to_owned();
        self.font_rows(field, &filter)
            .into_iter()
            .map(|row| (row.value, row.label))
            .collect()
    }

    /// The line of the list the cursor is on, or the current value's when it has not moved.
    fn font_cursor_index(&self, rows: &[FontRow], field: FontField) -> usize {
        let cursor = self.font_cursor().map(str::to_owned);
        let current = self.font_explicit(field).unwrap_or_default();
        cursor
            .and_then(|c| rows.iter().position(|r| r.value == c))
            .or_else(|| rows.iter().position(|r| r.value == current))
            .unwrap_or(0)
    }

    /// The warning under a row whose family is no installed one.
    pub fn font_warning(&self, field: FontField) -> Option<String> {
        let name = self.font_explicit(field)?;
        (!self.font_installed(&name)).then(|| {
            format!(
                "“{name}” is not installed on this Mac: {} until it is",
                field.fallback()
            )
        })
    }

    // ---- the picker -----------------------------------------------------------------------

    /// Opens or closes the picker of `field`. Either way the keyboard is on the field's row
    /// afterwards.
    pub fn toggle_font_picker(&mut self, field: FontField, cx: &mut Context<Self>) {
        let open = self.open_font_picker() == Some(field);
        if let Some(sheet) = &mut self.sheet {
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            sheet.pack_menu = None;
            sheet.font_top = 0;
            sheet.font_wheel = 0.0;
            sheet.dropdown = (!open).then(|| field.key().to_owned());
            sheet.edit = None;
            sheet.zone = super::settings_keys::SheetFocus::Controls;
            sheet.focus_row = Some(format!("f:{}", field.key()));
        }
        if !open {
            self.ensure_fonts(cx);
            if let Some(at) = self.sheet.as_ref().and_then(|s| {
                s.page_fields()
                    .iter()
                    .position(|f| FontField::of(f.namespace, &f.dotted()) == Some(field))
            }) {
                self.scroll_row_into_view(at);
            }
        }
        cx.notify();
    }

    /// Puts the picker's cursor on `value` (the pointer arriving on it).
    pub fn font_point_at(&mut self, field: FontField, value: &str, cx: &mut Context<Self>) {
        if self.open_font_picker() != Some(field) || self.font_cursor() == Some(value) {
            return;
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.cursor = Some((field.key().to_owned(), value.to_owned()));
        }
        cx.notify();
    }

    /// Shows or hides the families that are not monospaced.
    pub fn set_font_show_all(&mut self, all: bool, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet
            && sheet.font_all != all
        {
            sheet.font_all = all;
            sheet.font_top = 0;
            sheet.cursor = None;
            cx.notify();
        }
    }

    /// A key typed into the open font picker's search box. `true` when the key was used.
    pub(super) fn font_picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(field) = self.open_font_picker() else {
            return false;
        };
        let Some(edit) = super::settings_themes::search_edit(event) else {
            return false;
        };
        let changed = match (&mut self.sheet, edit) {
            (Some(sheet), Some(text)) => {
                sheet.pack_filter.push_str(&text);
                true
            }
            (Some(sheet), None) => sheet.pack_filter.pop().is_some(),
            _ => false,
        };
        if changed {
            // The cursor goes to the first line that fits, and the list to its top.
            let first = self.font_options(field).into_iter().next().map(|(v, _)| v);
            if let Some(sheet) = &mut self.sheet {
                sheet.cursor = first.map(|value| (field.key().to_owned(), value));
                sheet.font_top = 0;
            }
            cx.notify();
        }
        true
    }

    /// Keeps the line the arrow keys moved to inside the shown part of the list.
    pub(super) fn font_follow_cursor(&mut self) {
        let Some(field) = self.open_font_picker() else {
            return;
        };
        let rows = self.font_rows(field, self.font_filter());
        let at = self.font_cursor_index(&rows, field);
        if let Some(sheet) = &mut self.sheet {
            if at < sheet.font_top {
                sheet.font_top = at;
            } else if at >= sheet.font_top + SHOWN {
                sheet.font_top = at + 1 - SHOWN;
            }
        }
    }

    /// The wheel over the list: `pixels` of scroll (positive scrolls toward the end), a line at
    /// a time.
    pub fn font_wheel(&mut self, pixels: f32, cx: &mut Context<Self>) {
        let Some(field) = self.open_font_picker() else {
            return;
        };
        let total = self.font_rows(field, self.font_filter()).len();
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        sheet.font_wheel += pixels;
        let lines = (sheet.font_wheel / LINE_HEIGHT).trunc();
        if lines != 0.0 {
            sheet.font_wheel -= lines * LINE_HEIGHT;
            let last = total.saturating_sub(SHOWN);
            sheet.font_top = sheet
                .font_top
                .saturating_add_signed(lines as isize)
                .min(last);
            cx.notify();
        }
    }

    /// Picks `value` (a family, or [`SYSTEM_DEFAULT`]): written once, and the system default is
    /// a removed key.
    pub fn font_apply(&mut self, field: FontField, value: &str, cx: &mut Context<Self>) {
        let current = self.font_explicit(field);
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.font_top = 0;
        }
        if value == SYSTEM_DEFAULT {
            if current.is_some() {
                self.sheet_reset(field.key(), cx);
            }
        } else if current.as_deref() != Some(value) {
            self.sheet_write(field.key(), toml::Value::String(value.to_owned()), cx);
        }
        cx.notify();
    }

    // ---- Install font… --------------------------------------------------------------------

    /// *Install font…*: the system's open panel for a font file, then [`Self::font_install`].
    pub fn font_install_choose(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update(cx, |this, cx| this.font_install(path, cx)).ok();
            }
        })
        .detach();
    }

    /// Adds the font file `path` to the user's account (the system registers it; it stays
    /// installed). The list follows the system's word that fonts changed.
    pub fn font_install(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let source = Arc::clone(&self.mac.fonts);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.fonts.install_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { source.install(&path) })
                .await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.set_notice(format!("Installed {name}: pick it from the list"), cx);
                    // The system's notification may come late: read the list now.
                    this.load_fonts(cx);
                }
                Err(reason) => this.set_notice(reason, cx),
            })
            .ok();
        }));
    }

    // ---- rendering ------------------------------------------------------------------------

    /// The row of a font setting: its title, help, the `You` chip and ↺, and the head, a button
    /// with the current family, from which the picker hangs.
    pub(super) fn render_font_row(
        &self,
        font: FontField,
        field: &Field,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let ns = field.namespace;
        let open = self.dropdown_drawn(font.key(), cx);
        let head_label = match self.font_explicit(font) {
            Some(name) => name,
            None => system_default_label(&self.font_default_family(font, cx)),
        };
        let under: Vec<AnyElement> = self
            .font_warning(font)
            .map(|line| {
                note(line, &theme, &tokens)
                    .text_color(theme.warning)
                    .into_any_element()
            })
            .into_iter()
            .collect();
        let head = Button::new(
            SharedString::from(format!("dropdown-{key}")),
            SharedString::from(head_label),
            &theme,
            &tokens,
        )
        .compact()
        .min_width(super::ui::CONTROL_WIDTH)
        .selected(open)
        .child(crate::icons::glyph(crate::icons::Icon::ActionMenuDown))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_font_picker(font, cx)))
        .child(probe(
            &self.regions,
            Region::SettingsItem("font-picker", font.index()),
        ));
        let panel = open.then(|| self.render_font_picker(font, cx));
        let control = self.menu_anchor(font.key(), head, panel, Self::close_dropdown, cx);
        let row_tag = format!("f:{key}");
        let row = SheetRow::new(
            SharedString::from(format!("field-{key}")),
            field_title(ns, &key),
            field_help(ns, &key, &field.doc),
            control,
        )
        .under(under)
        .chip(self.field_chip(field, cx))
        .reset(self.field_reset(field, &self.sheet_kind_of(field), cx))
        .marked(probe(
            &self.regions,
            Region::SettingsItem("row", self.field_index(field)),
        ))
        .control_marked(probe(
            &self.regions,
            Region::SettingsItem("control", self.field_index(field)),
        ))
        .build(&theme, &tokens);
        self.sheet_row_focus(&row_tag, row)
    }

    /// The picker: a popover with the search box, *Show all* and *Install font…* on top, the
    /// list of families (each in its own face), the preview line and the keys underneath.
    fn render_font_picker(
        &self,
        font: FontField,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let filter = self.font_filter().to_owned();
        let rows = self.font_rows(font, &filter);
        let all = self.sheet.as_ref().is_some_and(|s| s.font_all);
        let at = self.font_cursor_index(&rows, font);
        let last = rows.len().saturating_sub(SHOWN);
        let start = self.font_top().min(last);
        let end = (start + SHOWN).min(rows.len());
        let cursor = self.font_cursor().map(str::to_owned);
        let current = self.font_explicit(font).unwrap_or_default();
        let default_family = self.font_default_family(font, cx);
        let loading = !self.fonts_loaded();

        let search = div()
            .id("font-filter")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.control_height))
            .px(px(tokens.spacing * 2.0))
            .outlined(&theme, &tokens)
            // The picker's field has the keyboard while the picker is open.
            .focus_ring(true, &theme, &tokens)
            .child(crate::icons::glyph(crate::icons::Icon::ActionSearch).text_color(theme.muted))
            .child(if filter.is_empty() {
                div()
                    .text_color(theme.muted)
                    .child("Search fonts or type a name")
            } else {
                div().child(SharedString::from(filter.clone()))
            });
        let show_all = Button::new("font-show-all", "Show all", &theme, &tokens)
            .compact()
            .selected(all)
            .on_click(cx.listener(move |this, _, _, cx| {
                let all = this.sheet.as_ref().is_some_and(|s| s.font_all);
                this.set_font_show_all(!all, cx);
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("font-all", font.index()),
            ));
        let install = Button::new("font-install", "Install font…", &theme, &tokens)
            .compact()
            .on_click(cx.listener(|this, _, _, cx| this.font_install_choose(cx)))
            .child(probe(
                &self.regions,
                Region::SettingsItem("font-install", font.index()),
            ));

        let no_mono_match = !all
            && !filter.trim().is_empty()
            && !rows
                .iter()
                .any(|r| matches!(r.kind, FontRowKind::Family { .. }));
        let list =
            div()
                .id("font-list")
                .flex()
                .flex_col()
                .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                    // A positive pixel delta moves the content down: toward the start.
                    let delta = event.delta.pixel_delta(px(LINE_HEIGHT)).y;
                    this.font_wheel(-f32::from(delta), cx);
                    cx.stop_propagation();
                }))
                .children((start > 0).then(|| {
                    note(format!("{start} more above"), &theme, &tokens).into_any_element()
                }))
                .children(rows[start..end].iter().enumerate().map(|(i, row)| {
                    self.render_font_option(
                        font,
                        row,
                        start + i,
                        row.value == current,
                        cursor.as_deref().is_some_and(|c| c == row.value),
                        &default_family,
                        cx,
                    )
                }))
                .children((end < rows.len()).then(|| {
                    note(format!("{} more below", rows.len() - end), &theme, &tokens)
                        .into_any_element()
                }))
                .children(loading.then(|| {
                    note("Reading the installed fonts…", &theme, &tokens).into_any_element()
                }))
                .children(no_mono_match.then(|| {
                    note(
                        "No monospaced font matches: turn on Show all to search every family.",
                        &theme,
                        &tokens,
                    )
                    .into_any_element()
                }));

        // The preview: the family under the cursor, at the setting's size.
        let previewed = rows.get(at).map(|r| match r.kind {
            FontRowKind::SystemDefault => default_family.clone(),
            _ => r.value.clone(),
        });
        let size = self.font_preview_size(font);
        let preview = div()
            .id("font-preview")
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(2.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing * 1.5))
            .rounded(px(tokens.radius))
            .bg(theme.accent_soft)
            .children(previewed.map(|name| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.muted)
                            .child(SharedString::from(family_caption(&name))),
                    )
                    .child(
                        div()
                            .font_family(SharedString::from(name))
                            .text_size(px(size))
                            .truncate()
                            .child(SAMPLE),
                    )
            }));

        menu_panel(
            SharedString::from(format!("font-picker-{}", font.key())),
            520.0,
            &theme,
            &tokens,
        )
        .w(px(520.0))
        .gap(px(tokens.spacing * 2.0))
        .p(px(tokens.spacing * 2.0))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing))
                .child(search)
                .child(show_all)
                .child(install),
        )
        .child(div().flex().flex_col().flex_none().child(list))
        .child(preview)
        .child(
            div()
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child("Type to search · ↑ ↓ choose · Return picks · Esc closes. A name that is not in the list can still be used."),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_font_option(
        &self,
        font: FontField,
        row: &FontRow,
        index: usize,
        is_current: bool,
        pointed: bool,
        default_family: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let label = match row.kind {
            FontRowKind::SystemDefault => system_default_label(default_family),
            _ => row.label.clone(),
        };
        // Each family in its own face; the lines that are not families in the interface's.
        let face = match row.kind {
            FontRowKind::Family { .. } | FontRowKind::Missing => {
                Some(SharedString::from(row.value.clone()))
            }
            FontRowKind::SystemDefault | FontRowKind::Typed { .. } => None,
        };
        let badge = match row.kind {
            FontRowKind::Family { monospaced: true } => Some("mono"),
            FontRowKind::Missing | FontRowKind::Typed { installed: false } => Some("not installed"),
            _ => None,
        };
        let value = row.value.clone();
        let (hover_value, click_value) = (value.clone(), value);
        menu_option(
            SharedString::from(format!("font-{}-{index}", font.key())),
            SharedString::from(label),
            is_current,
            pointed,
            &theme,
            &tokens,
        )
        .when_some(face, |s, face| s.font_family(face))
        .when_some(badge, |s, badge| {
            s.child(
                div()
                    .ml_auto()
                    .flex_none()
                    .font_family(super::ui::ui_family())
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(if badge == "mono" {
                        theme.faint
                    } else {
                        theme.warning
                    })
                    .child(badge),
            )
        })
        .child(probe(
            &self.regions,
            Region::SettingsItem("font-option", index),
        ))
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered {
                this.font_point_at(font, &hover_value, cx);
            }
        }))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.font_apply(font, &click_value, cx);
        }))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fam(name: &str, mono: bool) -> FontFamily {
        FontFamily::new(name, mono)
    }

    fn installed() -> Vec<FontFamily> {
        vec![
            fam("Avenir", false),
            fam("Courier New", true),
            fam("Fira Code", true),
            fam("Helvetica", false),
            fam("Menlo", true),
        ]
    }

    #[test]
    fn the_preview_caption_never_shows_an_internal_family_name() {
        assert_eq!(family_caption(".SystemUIFont"), "System font");
        assert_eq!(family_caption(".SF NS Mono"), "System font");
        assert_eq!(family_caption("Menlo"), "Menlo");
    }

    fn values(rows: &[FontRow]) -> Vec<&str> {
        rows.iter().map(|r| r.value.as_str()).collect()
    }

    #[test]
    fn only_monospaced_families_are_listed_after_system_default_until_show_all() {
        let rows = font_rows(&installed(), None, "", true);
        assert_eq!(values(&rows), ["", "Courier New", "Fira Code", "Menlo"]);
        assert_eq!(rows[0].kind, FontRowKind::SystemDefault);
        let all = font_rows(&installed(), None, "", false);
        assert_eq!(
            values(&all),
            [
                "",
                "Avenir",
                "Courier New",
                "Fira Code",
                "Helvetica",
                "Menlo"
            ]
        );
    }

    #[test]
    fn the_current_value_comes_first_even_when_it_is_not_monospaced() {
        let rows = font_rows(&installed(), Some("Helvetica"), "", true);
        assert_eq!(
            values(&rows),
            ["Helvetica", "", "Courier New", "Fira Code", "Menlo"]
        );
        assert_eq!(rows[0].kind, FontRowKind::Family { monospaced: false });
        // Matched without regard to case, and listed once.
        let rows = font_rows(&installed(), Some("menlo"), "", true);
        assert_eq!(values(&rows), ["menlo", "", "Courier New", "Fira Code"]);
    }

    #[test]
    fn a_current_family_that_is_not_installed_is_first_and_marked() {
        let rows = font_rows(&installed(), Some("Berkeley Mono"), "", true);
        assert_eq!(rows[0].value, "Berkeley Mono");
        assert_eq!(rows[0].kind, FontRowKind::Missing);
    }

    #[test]
    fn the_search_matches_inside_names_without_regard_to_case() {
        let rows = font_rows(&installed(), None, "ou", false);
        assert_eq!(values(&rows[..1]), ["Courier New"]);
        let rows = font_rows(&installed(), None, "fira c", true);
        assert_eq!(values(&rows), ["Fira Code"]);
        // The search finds System default too.
        let rows = font_rows(&installed(), None, "system", true);
        assert_eq!(rows[0].kind, FontRowKind::SystemDefault);
    }

    #[test]
    fn a_typed_name_that_no_family_fits_is_offered_and_marked_when_not_installed() {
        let rows = font_rows(&installed(), None, "  Berkeley Mono ", true);
        let typed = rows.last().unwrap();
        assert_eq!(typed.value, "Berkeley Mono");
        assert_eq!(typed.label, "Use “Berkeley Mono”");
        assert_eq!(typed.kind, FontRowKind::Typed { installed: false });
        // A proportional family hidden by the monospace filter is offered, installed.
        let rows = font_rows(&installed(), None, "Helvetica", true);
        assert_eq!(values(&rows), ["Helvetica"]);
        assert_eq!(rows[0].kind, FontRowKind::Typed { installed: true });
        // A family that fits the text is listed instead: no line for the text itself.
        let rows = font_rows(&installed(), None, "men", true);
        assert_eq!(values(&rows), ["Menlo"]);
        let rows = font_rows(&installed(), None, "menlo", true);
        assert_eq!(values(&rows), ["Menlo"]);
        assert!(matches!(rows[0].kind, FontRowKind::Family { .. }));
        // The current value typed again is not offered either.
        let rows = font_rows(&installed(), Some("Berkeley Mono"), "Berkeley Mono", true);
        assert_eq!(values(&rows), ["Berkeley Mono"]);
    }

    #[test]
    fn nothing_listed_yet_still_offers_the_typed_name() {
        let rows = font_rows(&[], None, "", true);
        assert_eq!(values(&rows), [""]);
        let rows = font_rows(&[], None, "Menlo", true);
        assert_eq!(values(&rows), ["Menlo"]);
    }

    #[test]
    fn the_pickers_belong_to_the_three_family_settings() {
        assert_eq!(
            FontField::of("terminal", "font_family"),
            Some(FontField::TerminalFamily)
        );
        assert_eq!(
            FontField::of("editor", "font_family"),
            Some(FontField::EditorFamily)
        );
        assert_eq!(
            FontField::of("appearance", "ui_font_family"),
            Some(FontField::UiFamily)
        );
        assert_eq!(FontField::of("terminal", "font_size"), None);
        assert_eq!(FontField::of("editor", "font_size"), None);
        assert_eq!(FontField::of("appearance", "font_family"), None);
    }
}
