//! The color, icon and terminal-theme pickers of the Appearance and Terminal pages (TH-3).
//!
//! A picker is a dropdown of the settings sheet with a richer body: every pack of the registry
//! with its name, author, license and origin, the invalid ones disabled with their errors, a
//! search box, a preview and the pack actions (Open pack folder, Validate / Reload, Duplicate as
//! new pack, Reset to embedded default).
//!
//! **The preview is derived, never applied.** While a picker is open, the pack its cursor is on
//! (arrow keys, or the mouse pointing at it) is drawn in the picker's own preview: sample chrome,
//! state cards, a diff and a terminal ANSI row for colors; a grid of nav, action, state and dock
//! icons with a visible fallback for icons. Nothing is written and nothing global changes: the
//! workbench's style, the installed icons and the settings file stay as they were, so another
//! window is unaffected, and Escape, a click outside the picker, closing Settings or quitting
//! simply drop the preview and what is on screen is, by construction, the current persisted
//! state (a system-mode or settings-file change made meanwhile included). Return, or a click on a
//! pack, writes the selection once.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_settings::themes::author::{self, Origin, PackInfo, PackKind};
use hutan_settings::themes::icons::{IconId, ResolvedIcons};
use hutan_settings::themes::{
    AUTO, DARK, LIGHT, OverrideMemory, ResolvedTheme, SelectionRequest, TerminalTheme,
};
use hutan_settings::{AppearanceSettings, SystemAppearance};

use super::render::probe;
use super::settings_labels::{field_help, field_title, words};
use super::settings_rows::{SheetRow, menu_option, menu_panel, note};
use super::settings_schema::Field;
use super::settings_theme_preview as sample;
use super::style::TextTooltip;
use super::ui::{Button, FocusRingExt as _, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use crate::theme::{DensityExt as _, Theme, Tokens};

/// A setting that is picked from the registry's packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackField {
    /// `appearance.light_theme`
    LightTheme,
    /// `appearance.dark_theme`
    DarkTheme,
    /// `appearance.icon_theme`
    IconTheme,
    /// `terminal.theme`: Automatic, or any valid color pack
    TerminalTheme,
}

impl PackField {
    pub const ALL: [PackField; 4] = [
        Self::LightTheme,
        Self::DarkTheme,
        Self::IconTheme,
        Self::TerminalTheme,
    ];

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
            Self::LightTheme | Self::DarkTheme | Self::IconTheme => "appearance",
            Self::TerminalTheme => "terminal",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::LightTheme => "light_theme",
            Self::DarkTheme => "dark_theme",
            Self::IconTheme => "icon_theme",
            Self::TerminalTheme => "theme",
        }
    }

    pub const fn kind(self) -> PackKind {
        match self {
            Self::IconTheme => PackKind::Icon,
            _ => PackKind::Color,
        }
    }

    /// The polarity a pack must have to fill this slot.
    const fn polarity(self) -> Option<SystemAppearance> {
        match self {
            Self::LightTheme => Some(SystemAppearance::Light),
            Self::DarkTheme => Some(SystemAppearance::Dark),
            _ => None,
        }
    }

    /// What the setting is without a key in the file: the embedded pack, or Automatic.
    pub const fn default_value(self) -> &'static str {
        match self {
            Self::LightTheme => LIGHT,
            Self::DarkTheme => DARK,
            Self::IconTheme => hutan_settings::themes::icons::DEFAULT,
            Self::TerminalTheme => AUTO,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::IconTheme => "icon theme",
            _ => "color theme",
        }
    }
}

/// One line of a picker: a pack, or Automatic.
#[derive(Debug, Clone)]
pub struct PickerRow {
    /// The value written when it is picked; `None` for a pack that cannot be selected.
    pub value: Option<String>,
    pub title: String,
    pub info: Option<PackInfo>,
}

/// What the picker's preview shows: the pack under the cursor, drawn from its own values.
#[derive(Debug, Clone)]
pub struct PackPreview {
    pub field: PackField,
    pub id: String,
    pub title: String,
    /// The pack's warnings (contrast, license, a bad asset) and what the selection would say,
    /// one line each.
    pub notes: Vec<String>,
    body: PreviewBody,
    /// The catalog generation an icon preview was read at.
    generation: u64,
}

#[derive(Debug, Clone)]
enum PreviewBody {
    Color(Arc<ResolvedTheme>),
    Terminal(Box<TerminalTheme>),
    Icons {
        icons: Arc<ResolvedIcons>,
        /// The ids the pack replaces; the rest fall back to the embedded pack.
        own: BTreeSet<IconId>,
    },
}

impl PackPreview {
    /// `color`, `terminal` or `icons`.
    pub fn body_kind(&self) -> &'static str {
        match &self.body {
            PreviewBody::Color(_) => "color",
            PreviewBody::Terminal(_) => "terminal",
            PreviewBody::Icons { .. } => "icons",
        }
    }

    /// The previewed chrome pack's resolved background, as `#rrggbb`: what a test compares with
    /// the pack's file.
    pub fn background(&self) -> Option<String> {
        match &self.body {
            PreviewBody::Color(theme) => Some(
                theme
                    .color(hutan_settings::themes::Color::Background)
                    .to_string(),
            ),
            PreviewBody::Terminal(terminal) => Some(terminal.background.to_string()),
            PreviewBody::Icons { .. } => None,
        }
    }

    /// The ids of an icon preview that the pack draws itself, and how many fall back.
    pub fn icon_split(&self) -> Option<(usize, usize)> {
        match &self.body {
            PreviewBody::Icons { own, .. } => Some((own.len(), IconId::COUNT - own.len())),
            _ => None,
        }
    }
}

fn matches_filter(title: &str, info: Option<&PackInfo>, filter: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let mut haystack = vec![title.to_lowercase()];
    if let Some(info) = info {
        haystack.extend(info.id.iter().map(|id| id.to_lowercase()));
        haystack.extend(info.authors.iter().map(|a| a.to_lowercase()));
    }
    filter
        .split_whitespace()
        .all(|word| haystack.iter().any(|h| h.contains(word)))
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::Embedded => "Embedded",
        Origin::User => "User",
        Origin::UserOverride => "User override",
        Origin::Imported => "Imported",
    }
}

/// The first line of a pack's problems for its row: file, key and reason, like the Problems
/// footer says them.
fn error_line(error: &hutan_settings::SettingsError) -> String {
    super::settings_sheet::problem_text(error)
}

impl Workbench {
    // ---- reading ------------------------------------------------------------------------------

    /// The picker open on the selected page, if one is.
    pub fn open_pack_picker(&self) -> Option<PackField> {
        let sheet = self.sheet.as_ref()?;
        let tag = sheet.dropdown.as_deref()?;
        let field = sheet.field(tag)?;
        PackField::of(field.namespace, &field.dotted())
    }

    /// The pack the picker's cursor is on (keys or pointer); `None` while it has not moved.
    pub fn pack_cursor(&self) -> Option<&str> {
        let sheet = self.sheet.as_ref()?;
        let (tag, value) = sheet.cursor.as_ref()?;
        (sheet.dropdown.as_ref() == Some(tag)).then_some(value.as_str())
    }

    /// The text typed into the picker's search box.
    pub fn pack_filter(&self) -> &str {
        self.sheet.as_ref().map_or("", |s| s.pack_filter.as_str())
    }

    /// The preview of the open picker's cursor, if there is one.
    pub fn pack_preview(&self) -> Option<&PackPreview> {
        self.sheet.as_ref()?.pack_preview.as_ref()
    }

    /// The lines of `field`'s picker for the search `filter`: Automatic (terminal), the packs
    /// that fit the slot, and the invalid ones, disabled.
    pub fn pack_rows(&self, field: PackField, filter: &str) -> Vec<PickerRow> {
        let mut rows = Vec::new();
        if field == PackField::TerminalTheme
            && matches_filter("Automatic auto follows the color theme", None, filter)
        {
            rows.push(PickerRow {
                value: Some(AUTO.to_owned()),
                title: "Automatic".to_owned(),
                info: None,
            });
        }
        let (packs, _) = self.themes.listing(field.kind());
        for info in packs {
            if let (Some(slot), Some(appearance)) = (field.polarity(), info.appearance)
                && slot != appearance
            {
                continue;
            }
            if !matches_filter(&info.name, Some(&info), filter) {
                continue;
            }
            rows.push(PickerRow {
                value: info.valid.then(|| info.id.clone()).flatten(),
                title: info.name.clone(),
                info: Some(info),
            });
        }
        rows
    }

    /// The values `field` can be set to (unfiltered).
    pub(super) fn pack_values(&self, field: PackField) -> Vec<String> {
        self.pack_rows(field, "")
            .into_iter()
            .filter_map(|row| row.value)
            .collect()
    }

    /// The options of the open picker for the arrow keys, `(value, label)`: the filtered,
    /// selectable rows.
    pub(super) fn pack_options(&self, field: PackField) -> Vec<(String, String)> {
        let filter = self.pack_filter().to_owned();
        self.pack_rows(field, &filter)
            .into_iter()
            .filter_map(|row| row.value.map(|value| (value, row.title)))
            .collect()
    }

    fn pack_current(&self, field: PackField) -> Option<String> {
        self.sheet_value(field.key())
            .and_then(|v| v.as_str().map(str::to_owned))
    }

    // ---- the cursor and the preview -------------------------------------------------------------------

    /// Opens or closes the picker of `field`. Either way the keyboard is on the field's row
    /// afterwards.
    pub fn toggle_pack_picker(&mut self, field: PackField, cx: &mut Context<Self>) {
        let open = self.open_pack_picker() == Some(field);
        if let Some(sheet) = &mut self.sheet {
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            sheet.pack_menu = None;
            sheet.dropdown = (!open).then(|| field.key().to_owned());
            sheet.edit = None;
            sheet.zone = super::settings_keys::SheetFocus::Controls;
            sheet.focus_row = Some(format!("f:{}", field.key()));
        }
        if !open {
            self.scroll_to_pack_row(field);
        }
        cx.notify();
    }

    /// Scrolls the page so that the row of `field` (and the picker that opens below it) shows.
    fn scroll_to_pack_row(&self, field: PackField) {
        if let Some(sheet) = &self.sheet
            && let Some(at) = sheet
                .page_fields()
                .iter()
                .position(|f| PackField::of(f.namespace, &f.dotted()) == Some(field))
        {
            self.scroll_row_into_view(at);
        }
    }

    /// Puts the picker's cursor on the pack `value` (the pointer arriving on it).
    pub fn pack_point_at(&mut self, field: PackField, value: &str, cx: &mut Context<Self>) {
        if self.open_pack_picker() != Some(field) || self.pack_cursor() == Some(value) {
            return;
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.cursor = Some((field.key().to_owned(), value.to_owned()));
        }
        self.sync_pack_preview();
        cx.notify();
    }

    /// Closes the open picker without writing anything: Escape, a click outside it.
    pub fn close_pack_picker(&mut self, cx: &mut Context<Self>) {
        self.close_dropdown(cx);
    }

    /// A key typed into the open picker's search box. `true` when the key was used.
    pub(super) fn pack_picker_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(field) = self.open_pack_picker() else {
            return false;
        };
        let mods = event.keystroke.modifiers;
        if mods.platform || mods.control || mods.alt {
            return false;
        }
        let key = event.keystroke.key.as_str();
        let typed = match (key, &event.keystroke.key_char) {
            ("backspace", _) => None,
            ("enter" | "tab" | "escape", _) => return false,
            (_, Some(text)) if !text.chars().any(char::is_control) => Some(text.clone()),
            _ => return false,
        };
        let changed = match (&mut self.sheet, typed) {
            (Some(sheet), Some(text)) => {
                sheet.pack_filter.push_str(&text);
                true
            }
            (Some(sheet), None) => sheet.pack_filter.pop().is_some(),
            _ => false,
        };
        if changed {
            // The preview follows the first pack that fits.
            let first = self.pack_options(field).into_iter().next().map(|(v, _)| v);
            if let Some(sheet) = &mut self.sheet {
                sheet.cursor = first.map(|value| (field.key().to_owned(), value));
            }
            self.sync_pack_preview();
            cx.notify();
        }
        true
    }

    /// Brings the preview in line with the open picker's cursor: the pack under it, read at the
    /// current catalog generation. Colors are cheap and always rebuilt (the user's overrides
    /// may have changed); an icon pack is read from disk once per generation.
    pub(super) fn sync_pack_preview(&mut self) {
        let target = self
            .open_pack_picker()
            .zip(self.pack_cursor().map(str::to_owned));
        let Some((field, id)) = target else {
            if let Some(sheet) = &mut self.sheet {
                sheet.pack_preview = None;
            }
            return;
        };
        let generation = self.themes.generation();
        let keep = self.sheet.as_ref().is_some_and(|s| {
            s.pack_preview.as_ref().is_some_and(|p| {
                p.field == field
                    && p.id == id
                    && p.generation == generation
                    && matches!(p.body, PreviewBody::Icons { .. })
            })
        });
        if keep {
            return;
        }
        let preview = self.build_pack_preview(field, &id, generation);
        if let Some(sheet) = &mut self.sheet {
            sheet.pack_preview = preview;
        }
    }

    fn build_pack_preview(
        &self,
        field: PackField,
        id: &str,
        generation: u64,
    ) -> Option<PackPreview> {
        let mut notes: Vec<String> = Vec::new();
        let (packs, _) = self.themes.listing(field.kind());
        let info = packs
            .iter()
            .rev()
            .find(|p| p.valid && p.id.as_deref() == Some(id));
        if let Some(info) = info {
            notes.extend(info.diagnostics.iter().map(error_line));
        }
        let title = info.map_or_else(|| "Automatic".to_owned(), |i| i.name.clone());
        let body = match field.kind() {
            PackKind::Icon => {
                let catalog = self.themes.icon_snapshot();
                let pack = catalog
                    .packs
                    .iter()
                    .rev()
                    .find(|p| p.declaration.meta.id == id)?;
                let polarity = crate::theme::polarity(self.chrome, self.appearance);
                let (icons, selection) = catalog.select(id, polarity, None);
                notes.extend(selection.iter().map(error_line));
                PreviewBody::Icons {
                    icons,
                    own: pack.declaration.icons.keys().copied().collect(),
                }
            }
            PackKind::Color if field == PackField::TerminalTheme && id == AUTO => {
                PreviewBody::Terminal(Box::new(self.resolved_theme.terminal()))
            }
            PackKind::Color => {
                let snapshot = self.themes.snapshot();
                let pack = snapshot.pack(id)?;
                let appearance = self.settings.get::<AppearanceSettings>();
                let selected = snapshot.select(
                    SelectionRequest {
                        polarity: pack.descriptor.appearance,
                        light_theme: id,
                        dark_theme: id,
                        overrides: &appearance.theme_overrides,
                    },
                    &mut OverrideMemory::default(),
                );
                notes.extend(selected.diagnostics.iter().map(error_line));
                if field == PackField::TerminalTheme {
                    PreviewBody::Terminal(Box::new(selected.theme.terminal()))
                } else {
                    PreviewBody::Color(selected.theme)
                }
            }
        };
        Some(PackPreview {
            field,
            id: id.to_owned(),
            title,
            notes,
            body,
            generation,
        })
    }

    // ---- applying -----------------------------------------------------------------------------

    /// Return (or a click) on the pack `choice`: closes the picker and writes the selection
    /// once. Picking what is already in effect writes nothing; picking the embedded default
    /// removes the key instead of writing a redundant value.
    pub fn pack_apply(&mut self, field: PackField, choice: &str, cx: &mut Context<Self>) {
        let current = self.pack_current(field);
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            sheet.pack_menu = None;
        }
        if current.as_deref() == Some(choice) {
            cx.notify();
            return;
        }
        if choice == field.default_value() {
            self.sheet_reset(field.key(), cx);
            // Another layer may still set it (a workspace's terminal theme).
            if self.pack_current(field).as_deref() != Some(choice) {
                self.sheet_write(field.key(), toml::Value::String(choice.to_owned()), cx);
            }
        } else {
            self.sheet_write(field.key(), toml::Value::String(choice.to_owned()), cx);
        }
    }

    // ---- the pack actions -----------------------------------------------------------------------

    /// "Open pack folder": the folder of this kind of pack, created when it is not there yet.
    pub fn pack_open_folder(&mut self, kind: PackKind, cx: &mut Context<Self>) {
        let Some(data) = self.themes.data_dir().map(Path::to_path_buf) else {
            self.set_notice(words::CANNOT_LOCATE, cx);
            return;
        };
        let folder = data.join(kind.folder());
        if let Err(error) = std::fs::create_dir_all(&folder) {
            self.set_notice(words::cannot_create(&folder, &error.to_string()), cx);
            return;
        }
        (self.daemon.open.clone())(&folder, cx);
    }

    /// "Validate / Reload": reads the pack folders again now (the watcher does it on every
    /// change; this is for when it cannot) and says what it found.
    pub fn pack_reload(&mut self, cx: &mut Context<Self>) {
        self.themes.rescan();
        self.refresh_theme(cx);
        let (colors, color_notes) = self.themes.listing(PackKind::Color);
        let (icons, icon_notes) = self.themes.listing(PackKind::Icon);
        let problems = color_notes
            .iter()
            .chain(&icon_notes)
            .filter(|d| d.is_error())
            .count();
        let warnings = color_notes.len() + icon_notes.len() - problems;
        let text = format!(
            "Checked {} color and {} icon themes: {}, {}",
            colors.iter().filter(|p| p.valid).count(),
            icons.iter().filter(|p| p.valid).count(),
            plural(problems, "error"),
            plural(warnings, "warning"),
        );
        self.set_notice(text, cx);
        self.sync_pack_preview();
    }

    /// "Duplicate as new pack": a copy of the highlighted pack (the current one when the cursor
    /// has not moved) in your folder under a free id, ready to edit.
    pub fn pack_duplicate(&mut self, field: PackField, cx: &mut Context<Self>) {
        let source = self
            .pack_cursor()
            .map(str::to_owned)
            .or_else(|| self.pack_current(field))
            .filter(|id| id != AUTO)
            .or_else(|| {
                // Automatic is the active theme's own colors.
                (field == PackField::TerminalTheme).then(|| self.resolved_theme.id.clone())
            });
        let Some(source) = source else {
            return;
        };
        match author::duplicate(field.kind(), &self.themes, &source) {
            Ok((new_id, written)) => {
                self.themes.rescan();
                self.refresh_theme(cx);
                let text = format!(
                    "Copied {source} to {new_id}: edit {} (it reloads as you save)",
                    written.manifest.display()
                );
                if let Some(sheet) = &mut self.sheet {
                    sheet.message = None;
                    sheet.dropdown = Some(field.key().to_owned());
                    sheet.pack_filter.clear();
                    sheet.cursor = Some((field.key().to_owned(), new_id));
                }
                self.sync_pack_preview();
                self.scroll_to_pack_row(field);
                self.set_notice(text, cx);
            }
            Err(error) => {
                if let Some(sheet) = &mut self.sheet {
                    sheet.message = Some(error.to_string());
                }
                cx.notify();
            }
        }
    }

    /// "Reset to embedded default": removes the key, so the embedded pack applies.
    pub fn pack_reset(&mut self, field: PackField, cx: &mut Context<Self>) {
        self.close_pack_picker(cx);
        self.sheet_reset(field.key(), cx);
    }

    /// Removes every theme selection and color override from the user's settings: the
    /// embedded themes and icons apply again. The escape hatch of a theme that cannot be read
    /// (palette command and View menu; neither depends on what is drawn).
    pub fn reset_theme(&mut self, cx: &mut Context<Self>) {
        let mut removed = Vec::new();
        for (namespace, key) in [
            ("appearance", "light_theme"),
            ("appearance", "dark_theme"),
            ("appearance", "icon_theme"),
            ("appearance", "theme_overrides"),
            ("terminal", "theme"),
        ] {
            match self
                .settings
                .remove(hutan_settings::SettingsLayer::User, namespace, key)
            {
                Ok(true) => removed.push(format!("{namespace}.{key}")),
                Ok(false) => {}
                Err(error) => {
                    self.set_notice(format!("Could not reset the theme: {error}"), cx);
                    return;
                }
            }
        }
        self.close_pack_picker(cx);
        self.refresh_theme(cx);
        let text = if removed.is_empty() {
            "The embedded theme and icons are already in use".to_owned()
        } else {
            format!(
                "Theme reset to the embedded defaults (removed {} from settings.toml)",
                removed.join(", ")
            )
        };
        self.set_notice(text, cx);
    }

    // ---- rendering ----------------------------------------------------------------------------

    /// The row of a picker setting: its title, one line of help, the `You` chip and ↺, and the
    /// head, a button with the current pack's name, from which the picker hangs.
    pub(super) fn render_pack_row(
        &self,
        pack_field: PackField,
        field: &Field,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let ns = field.namespace;
        let open = self.dropdown_drawn(pack_field.key(), cx);
        let current = self.pack_current(pack_field).unwrap_or_default();
        let (packs, _) = self.themes.listing(pack_field.kind());
        let current_info = packs
            .iter()
            .rev()
            .find(|p| p.valid && p.id.as_deref() == Some(current.as_str()));
        let head_label = match (&current_info, current.as_str()) {
            (Some(info), _) => info.name.clone(),
            (None, AUTO) => "Automatic".to_owned(),
            (None, other) => words::not_available(other),
        };

        // The pack's authors, license and version are in the picker; the row only speaks up
        // when the pack asked for is not there.
        let mut under: Vec<AnyElement> = Vec::new();
        if current_info.is_none() && current != AUTO {
            under.push(
                note(
                    format!(
                        "`{current}` is not installed or is invalid: the embedded {} is used until it works (see Problems)",
                        pack_field.noun()
                    ),
                    &theme,
                    &tokens,
                )
                .text_color(theme.warning)
                .into_any_element(),
            );
        }

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
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_pack_picker(pack_field, cx)))
        .child(probe(
            &self.regions,
            Region::SettingsItem("pack-picker", pack_field.index()),
        ));
        let panel = open.then(|| self.render_pack_picker(pack_field, &current, cx));
        let control = self.menu_anchor(pack_field.key(), head, panel, Self::close_dropdown, cx);
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

    /// The picker: a popover with the search box and the `⋯` menu of pack actions on top, the
    /// list of packs on the left, the preview of the one the cursor is on on the right, and the
    /// keys underneath.
    fn render_pack_picker(
        &self,
        pack_field: PackField,
        current: &str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let filter = self.pack_filter().to_owned();
        let rows = self.pack_rows(pack_field, &filter);
        let cursor = self.pack_cursor().map(str::to_owned);
        let at = cursor
            .as_deref()
            .and_then(|c| rows.iter().position(|r| r.value.as_deref() == Some(c)))
            .or_else(|| {
                rows.iter()
                    .position(|r| r.value.as_deref().is_some_and(|v| v == current))
            })
            .unwrap_or(0);
        // The pack card above the preview: the highlighted pack's authors, license and version.
        let summary = rows
            .get(at)
            .and_then(|r| r.info.as_ref())
            .map(pack_summary)
            .filter(|s| !s.is_empty());
        // Six lines at a time, following the cursor.
        const SHOWN: usize = 6;
        let start = at
            .saturating_sub(SHOWN / 2)
            .min(rows.len().saturating_sub(SHOWN));
        let end = (start + SHOWN).min(rows.len());
        let search = div()
            .id("pack-filter")
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
                    .child("Search by name, id or author")
            } else {
                div().child(SharedString::from(filter.clone()))
            });
        let menu_open = self
            .sheet
            .as_ref()
            .is_some_and(|s| s.pack_menu == Some(pack_field));
        let more_tag = format!("pack-more-{}", pack_field.key());
        let more_head = IconButton::new(
            SharedString::from(more_tag.clone()),
            crate::icons::Icon::ActionMore,
            words::PACK_MORE_TIP,
            &theme,
            &tokens,
        )
        .toggled(menu_open)
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(sheet) = &mut this.sheet {
                sheet.pack_menu = (sheet.pack_menu != Some(pack_field)).then_some(pack_field);
                cx.notify();
            }
        }))
        .child(probe(
            &self.regions,
            Region::SettingsItem("pack-more", pack_field.index()),
        ));
        let more = self.menu_anchor(
            &more_tag,
            more_head,
            menu_open.then(|| self.render_pack_actions(pack_field, cx)),
            Self::close_pack_menu,
            cx,
        );
        let list = div()
            .id(SharedString::from(format!(
                "pack-list-{}",
                pack_field.key()
            )))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .children(
                (start > 0).then(|| {
                    note(format!("{start} more above"), &theme, &tokens).into_any_element()
                }),
            )
            .children(rows[start..end].iter().enumerate().map(|(i, row)| {
                self.render_pack_option(pack_field, row, start + i, current, cursor.as_deref(), cx)
            }))
            .children((end < rows.len()).then(|| {
                note(format!("{} more below", rows.len() - end), &theme, &tokens).into_any_element()
            }))
            .children(rows.is_empty().then(|| {
                note(
                    format!("No {} matches “{filter}”.", pack_field.noun()),
                    &theme,
                    &tokens,
                )
                .into_any_element()
            }));
        let preview = match self.pack_preview() {
            Some(preview) if preview.field == pack_field => {
                self.render_pack_preview(preview, summary, cx)
            }
            _ => note(
                "↑ ↓ or the pointer preview a theme here without changing anything.",
                &theme,
                &tokens,
            )
            .into_any_element(),
        };
        menu_panel(
            SharedString::from(format!("pack-picker-{}", pack_field.key())),
            460.0,
            &theme,
            &tokens,
        )
        .w(px(620.0))
        .min_h(px(240.0))
        .gap(px(tokens.spacing * 2.0))
        .p(px(tokens.spacing * 2.0))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing))
                .child(search)
                .child(more),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .gap(px(tokens.spacing * 3.0))
                .child(div().flex().flex_col().flex_none().w(px(264.0)).child(list))
                .child(div().flex().flex_col().flex_1().min_w_0().child(preview)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(words::PACK_HINT),
        )
    }

    /// Closes the picker's `⋯` menu, and nothing else.
    fn close_pack_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.pack_menu = None;
        }
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pack_option(
        &self,
        pack_field: PackField,
        row: &PickerRow,
        index: usize,
        current: &str,
        cursor: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let is_current = row.value.as_deref() == Some(current);
        let pointed = row.value.is_some() && row.value.as_deref() == cursor;
        let mut badges: Vec<&'static str> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut id_text = String::new();
        if let Some(info) = &row.info {
            badges.push(origin_label(info.origin));
            if info.kept_previous {
                badges.push("Last good version");
            }
            if let (PackKind::Icon, Some(appearance)) = (info.kind, info.appearance) {
                badges.push(match appearance {
                    SystemAppearance::Light => "Light only",
                    SystemAppearance::Dark => "Dark only",
                });
            }
            if let Some(id) = &info.id {
                id_text = id.clone();
            }
            errors = info.errors().map(error_line).collect();
            if !info.valid {
                badges.push("Disabled");
            }
        }
        let value = row.value.clone();
        let mut element = div()
            .id(SharedString::from(format!(
                "pack-{}-{index}",
                pack_field.key()
            )))
            .relative()
            .flex()
            .flex_col()
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius))
            // The highlighted pack is the list selection's fill; the current one says so with a
            // check, not an outline.
            .when(pointed, |s| s.bg(theme.accent_soft))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if row.value.is_some() {
                                theme.fg
                            } else {
                                theme.faint
                            })
                            .child(SharedString::from(row.title.clone())),
                    )
                    .when(is_current, |s| {
                        s.child(
                            div()
                                .ml_auto()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing))
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.fg)
                                .child(crate::icons::glyph(crate::icons::Icon::StateCompleted))
                                .child("Current"),
                        )
                    }),
            )
            .when(row.info.is_some(), |s| {
                s.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(id_text))
                        .children(
                            badges
                                .into_iter()
                                .map(|badge| super::ui::counter(badge, &theme, &tokens)),
                        ),
                )
            })
            .when(row.info.is_none(), |s| {
                s.child(note(
                    "The active color theme's own terminal colors",
                    &theme,
                    &tokens,
                ))
            })
            .children(errors.into_iter().take(2).map(|line| {
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .child(SharedString::from(line))
            }))
            .child(probe(
                &self.regions,
                Region::SettingsItem("pack-option", index),
            ));
        if let Some(value) = value {
            let (hover_value, click_value) = (value.clone(), value);
            element = element
                .cursor_pointer()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.pack_point_at(pack_field, &hover_value, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pack_apply(pack_field, &click_value, cx);
                }));
        } else {
            element = element.opacity(0.75);
        }
        element.into_any_element()
    }

    fn render_pack_preview(
        &self,
        preview: &PackPreview,
        summary: Option<String>,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let body = match &preview.body {
            PreviewBody::Color(resolved) => {
                let candidate = Theme::from_resolved(resolved);
                let candidate_tokens = Tokens::of(resolved, self.density);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(tokens.spacing * 2.0))
                    .child(sample::chrome(&candidate, &candidate_tokens))
                    .child(sample::state_cards(&candidate, &candidate_tokens))
                    .child(sample::diff(&candidate, &candidate_tokens))
                    .child(sample::terminal(&resolved.terminal()))
                    .into_any_element()
            }
            PreviewBody::Terminal(terminal) => sample::terminal(terminal).into_any_element(),
            PreviewBody::Icons { icons, own } => {
                sample::icon_grid(icons, own, &theme, &tokens).into_any_element()
            }
        };
        div()
            .id("pack-preview")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .text_size(px(tokens.section_font_size))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.muted)
                            .child(SharedString::from(format!("Preview · {}", preview.title))),
                    )
                    .child(
                        div()
                            .text_size(px(tokens.secondary_font_size))
                            .text_color(theme.faint)
                            .child("not applied"),
                    ),
            )
            .children(summary.map(|summary| note(summary, &theme, &tokens)))
            .child(body)
            .children(preview.notes.iter().take(4).map(|line| {
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.warning)
                    .child(SharedString::from(line.clone()))
            }))
            .into_any_element()
    }

    /// The pack actions: the `⋯` menu of the picker.
    fn render_pack_actions(
        &self,
        pack_field: PackField,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let kind = pack_field.kind();
        let folder_tip: SharedString = self
            .themes
            .data_dir()
            .map(|d| d.join(kind.folder()).display().to_string())
            .unwrap_or_else(|| "no data folder".to_owned())
            .into();
        let item = |id: &'static str, label: &'static str| {
            menu_option(id, label, false, false, &theme, &tokens).relative()
        };
        menu_panel(
            SharedString::from(format!("pack-actions-{}", pack_field.key())),
            220.0,
            &theme,
            &tokens,
        )
        .min_w(px(210.0))
        .child(
            item("pack-open-folder", words::PACK_OPEN_FOLDER)
                .tooltip(move |_, cx| cx.new(|_| TextTooltip::new(folder_tip.clone())).into())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_pack_menu(cx);
                    this.pack_open_folder(kind, cx);
                }))
                .child(probe(
                    &self.regions,
                    Region::SettingsItem("pack-open-folder", pack_field.index()),
                )),
        )
        .child(
            item("pack-reload", words::PACK_RELOAD)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_pack_menu(cx);
                    this.pack_reload(cx);
                }))
                .child(probe(
                    &self.regions,
                    Region::SettingsItem("pack-reload", pack_field.index()),
                )),
        )
        .child(
            item("pack-duplicate", words::PACK_DUPLICATE)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_pack_menu(cx);
                    this.pack_duplicate(pack_field, cx);
                }))
                .child(probe(
                    &self.regions,
                    Region::SettingsItem("pack-duplicate", pack_field.index()),
                )),
        )
        .child(
            item("pack-reset", words::PACK_RESET)
                .on_click(cx.listener(move |this, _, _, cx| this.pack_reset(pack_field, cx)))
                .child(probe(
                    &self.regions,
                    Region::SettingsItem("pack-reset", pack_field.index()),
                )),
        )
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The line under a pack's name: authors, license, version and what it replaces.
fn pack_summary(info: &PackInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !info.authors.is_empty() {
        parts.push(info.authors.join(", "));
    }
    if !info.license.is_empty() {
        parts.push(info.license.clone());
    }
    if let Some(version) = &info.version {
        parts.push(format!("v{version}"));
    }
    if let Some(icons) = info.icons {
        parts.push(format!(
            "{icons} icon{} of its own",
            if icons == 1 { "" } else { "s" }
        ));
    }
    if !info.valid {
        return "Cannot be selected until its errors are fixed".to_owned();
    }
    parts.join(" · ")
}
