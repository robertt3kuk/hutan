//! The settings sheet (⌘,): generated from the settings store's schemas.
//!
//! Left: a searchable list of pages. Right: the fields of the selected page, one control per
//! schema type (toggle, dropdown, number/text box, list editor, key/value editor) with "Reset"
//! to the inherited value. Bottom: the store's diagnostics. Every change goes through the
//! settings store, which edits `settings.toml` (or the workspace's or project's file) in place
//! and keeps its comments.
//!
//! Each field says which layer its value comes from (`settings_layers`: default, you,
//! workspace, project). A control edits the layer its value comes from when that is a
//! workspace or project file, else the user's file; the header offers a switch. Reset removes
//! the key from the layer being edited instead of writing the default.

use crate::theme::DensityExt as _;
use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_settings::{SettingsLayer, Severity};
use serde_json::Value as Json;

use super::actions::sheet as sheet_actions;
use super::modal::PromptSpec;
use super::render::probe;
use super::settings_keys::SheetFocus;
use super::settings_labels::{namespace_title, words};
use super::settings_pages::{preset_name, valid_env_name};
use super::settings_schema::{
    CatalogEntry, Field, FieldKind, NavGroup, catalog, number_range, page_fields, refine_kind,
    search, value_at,
};
use super::style::TextTooltip;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::ui::{FocusRingExt as _, StyledExt as _};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::settings::LayoutSettings;

/// A text box being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Dotted field path inside the namespace.
    pub field: String,
    pub text: String,
    /// The text is a new list item (or a new `NAME=value` of a map) rather than the field's
    /// whole value.
    pub list_add: bool,
    /// The map entry whose value is being edited (`terminal.env` variables).
    pub entry: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SettingsSheet {
    /// The selected page: the namespace of its first entry.
    pub(super) namespace: &'static str,
    pub(super) query: String,
    pub(super) edit: Option<Edit>,
    pub(super) dropdown: Option<String>,
    pub(super) message: Option<String>,
    pub(super) entries: Vec<CatalogEntry>,
    pub(super) pages: BTreeMap<&'static str, Vec<Field>>,
    /// The layer the user chose to edit; `None`: each field edits the layer it comes from.
    pub(super) layer: Option<SettingsLayer>,
    /// Where the keyboard is: the search box, the list of pages or the controls of the page.
    pub(super) zone: super::settings_keys::SheetFocus,
    /// The row of the page the keyboard is on while `zone` is the controls.
    pub(super) focus_row: Option<String>,
    /// The option of the open dropdown the arrow keys are on: the dropdown and the value.
    pub(super) cursor: Option<(String, String)>,
    /// What is typed into the search box of the open theme or icon picker.
    pub(super) pack_filter: String,
    /// The open picker's preview: the pack its cursor is on, drawn in the picker only.
    pub(super) pack_preview: Option<super::settings_themes::PackPreview>,
    /// The scroll of the page's rows: an opened picker scrolls its row into view.
    pub(super) scroll: gpui::ScrollHandle,
    /// `editor.vim_mode` as it was when the sheet opened: its search and value fields keep the
    /// way they type while the sheet is open, so turning Vim on in it does not make its own
    /// Escape leave Insert instead of closing it.
    pub(super) vim: bool,
    /// The Problems footer's list is open (over the page, which keeps its size).
    pub(super) problems_open: bool,
    /// The Advanced group of the page list is open (it is also open while it holds the
    /// selected page or a search is on).
    pub(super) advanced_open: bool,
    /// The `⋯` menu of the open theme picker, for this picker.
    pub(super) pack_menu: Option<super::settings_themes::PackField>,
    /// What had the keyboard when the sheet opened; it gets it back when the sheet closes.
    pub(super) return_focus: Option<gpui::WeakFocusHandle>,
    /// The sheet is the compact popover of one panel's settings (`customize.rs`): only that
    /// namespace's rows, hanging near the panel's ⋯, without the page list.
    pub(super) popover: Option<super::customize::PanelPopover>,
}

impl SettingsSheet {
    /// The selected page (`terminal`, `panels.sessions`, `models`, `data`, …).
    pub fn namespace(&self) -> &str {
        self.namespace
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The scroll of the page's rows (tests drive and read it).
    pub fn scroll(&self) -> &gpui::ScrollHandle {
        &self.scroll
    }

    /// Whether the Problems list is open.
    pub fn problems_open(&self) -> bool {
        self.problems_open
    }

    /// Whether the Advanced group was opened by hand.
    pub fn advanced_open(&self) -> bool {
        self.advanced_open
    }

    /// The picker whose `⋯` menu is open.
    pub fn pack_menu(&self) -> Option<super::settings_themes::PackField> {
        self.pack_menu
    }

    pub fn edit(&self) -> Option<&Edit> {
        self.edit.as_ref()
    }

    /// The open dropdown: the field's key, `add:<key>` for the menu that adds to a list,
    /// `daemon:<key>` for a daemon setting.
    pub fn dropdown(&self) -> Option<&str> {
        self.dropdown.as_deref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The layer the user chose to edit, if they chose one.
    pub fn layer(&self) -> Option<SettingsLayer> {
        self.layer
    }

    /// The fields of the selected page.
    pub fn fields(&self) -> &[Field] {
        self.pages.get(self.namespace).map_or(&[], Vec::as_slice)
    }

    /// The field of the selected page whose key inside its namespace is `dotted`.
    pub(super) fn field(&self, dotted: &str) -> Option<&Field> {
        self.fields().iter().find(|f| f.dotted() == dotted)
    }

    /// The catalog entry of the settings namespace `namespace`.
    pub(super) fn entry_of(&self, namespace: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.namespace == namespace)
    }
}

/// One problem of the files, as a line of the Problems footer.
pub(super) fn problem_text(e: &hutan_settings::SettingsError) -> String {
    match (&e.file, e.key_path.is_empty()) {
        (Some(file), false) => format!("{} · {}: {}", file.display(), e.key_path, e.message),
        (Some(file), true) => format!("{}: {}", file.display(), e.message),
        (None, false) => format!("{}: {}", e.key_path, e.message),
        (None, true) => e.message.clone(),
    }
}

pub(super) fn display_value(json: &Json) -> String {
    match json {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

impl Workbench {
    // ---- open / close -----------------------------------------------------------------------

    /// Opens the sheet at `namespace` (the page of that namespace; the first page when `None`
    /// or unknown).
    pub fn open_settings(
        &mut self,
        namespace: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = catalog();
        let pages = page_fields(&entries, &self.settings.schema());
        let selected = entries
            .iter()
            .find(|e| Some(e.namespace) == namespace)
            .or_else(|| {
                self.sheet
                    .as_ref()
                    .and_then(|s| entries.iter().find(|e| e.namespace == s.namespace))
            })
            .or_else(|| entries.first())
            .map_or("appearance", |e| e.page);
        self.palette = None;
        self.context_menu = None;
        // Opening another page keeps what the sheet gives the keyboard back to.
        let return_focus = match &mut self.sheet {
            Some(sheet) => sheet.return_focus.take(),
            None => window
                .focused(cx)
                .filter(|f| *f != self.sheet_focus)
                .map(|f| f.downgrade()),
        };
        self.sheet = Some(SettingsSheet {
            namespace: selected,
            query: String::new(),
            edit: None,
            dropdown: None,
            message: None,
            entries,
            pages,
            layer: None,
            zone: super::settings_keys::SheetFocus::Search,
            focus_row: None,
            cursor: None,
            pack_filter: String::new(),
            pack_preview: None,
            scroll: gpui::ScrollHandle::new(),
            vim: self.vim_config().enabled,
            problems_open: false,
            advanced_open: false,
            pack_menu: None,
            return_focus,
            popover: None,
        });
        window.focus(&self.sheet_focus, cx);
        self.edits.sheet_query.reset();
        self.edits.sheet_value.reset();
        self.settings_page_opened(selected, cx);
        if !super::settings_daemon::is_daemon_page(selected) {
            // The banner of a pending restart is on every page.
            self.refresh_daemon_settings(cx);
        }
        cx.notify();
    }

    /// The problems of the settings files: the store's, then the Vim mappings that cannot be
    /// used (`editor.vim_mappings`, a key Hutan does not know).
    pub fn settings_problems(&self) -> Vec<hutan_settings::SettingsError> {
        let mut errors = self.settings.errors();
        let editor = self.settings.get::<crate::settings::EditorSettings>();
        errors.extend(
            super::vim_keys::compile(&editor.vim_mappings)
                .1
                .into_iter()
                .map(|p| hutan_settings::SettingsError::error(None, p.key_path, p.message)),
        );
        errors
    }

    /// The lines of the Problems footer of the selected page, in the order they are drawn: the
    /// settings files', then the daemon's (`daemon.toml` that cannot be read, a refused field,
    /// a provider error on the Models page).
    pub fn sheet_problems(&self) -> Vec<String> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        let mut lines: Vec<String> = self.settings_problems().iter().map(problem_text).collect();
        lines.extend(self.daemon_page_problems(sheet.namespace));
        lines
    }

    pub fn settings_sheet(&self) -> Option<&SettingsSheet> {
        self.sheet.as_ref()
    }

    /// Closes the sheet; the keyboard goes back to what had it before (the code of a file tab
    /// with its caret, a terminal, a panel). A click on the sidebar's Settings button moved it
    /// to the sidebar itself (or the window): then the focused pane's content takes it.
    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut sheet) = self.sheet.take() else {
            return;
        };
        let back = sheet
            .return_focus
            .take()
            .and_then(|f| f.upgrade())
            .filter(|f| *f != self.focus_handle && !self.dock_focus.contains(f));
        match back {
            Some(handle) => window.focus(&handle, cx),
            None => {
                let pane = self.state.active_pane_id();
                self.focus_pane(pane, window, cx);
            }
        }
        self.leave_sheet(sheet, cx);
        cx.notify();
    }

    /// Escape: closes what is open over the page, innermost first (the `⋯` menu of a picker, a
    /// dropdown or picker, a text edit, the Problems list), then the sheet. The keyboard stays
    /// on the row the dropdown was opened from.
    pub(crate) fn sheet_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Under Vim mode the first Escape of a text field leaves Insert mode.
        if self.sheet_vim_escape(cx) {
            return;
        }
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        if sheet.pack_menu.take().is_some() {
            cx.notify();
            return;
        }
        let abandoned = sheet.edit.take();
        let closed_dropdown = sheet.dropdown.take().is_some();
        if closed_dropdown {
            // Escape cancels a picker: nothing was applied, so there is nothing to restore.
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
        }
        if abandoned.is_some() || closed_dropdown {
            // Giving up an edit the daemon refused shows the setting as it is.
            if let Some(key) = abandoned
                .as_ref()
                .and_then(|e| e.field.strip_prefix(super::settings_daemon::EDIT_PREFIX))
            {
                self.daemon_settings.discard_refused(key);
            }
            cx.notify();
        } else if std::mem::take(&mut sheet.problems_open) {
            cx.notify();
        } else {
            self.close_settings(window, cx);
        }
    }

    /// Escape in the sheet's text fields under Vim mode (as it was when the sheet opened):
    /// `true` when Vim used it.
    fn sheet_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(sheet) = &self.sheet else {
            return false;
        };
        if !sheet.vim || !self.vim_config().enabled {
            return false;
        }
        if sheet.dropdown.is_some() {
            return false;
        }
        let used = match &sheet.edit {
            Some(edit) => self.edits.sheet_value.escape(&edit.text),
            None => self.edits.sheet_query.escape(&sheet.query),
        };
        if used {
            cx.notify();
        }
        used
    }

    pub fn select_settings_namespace(&mut self, namespace: &str, cx: &mut Context<Self>) {
        let Some(page) = self
            .sheet
            .as_ref()
            .and_then(|s| s.entry_of(namespace))
            .map(|e| e.page)
        else {
            return;
        };
        if let Some(sheet) = &mut self.sheet {
            sheet.namespace = page;
            sheet.edit = None;
            sheet.dropdown = None;
            sheet.message = None;
            sheet.layer = None;
            sheet.problems_open = false;
            sheet.zone = super::settings_keys::SheetFocus::Categories;
            sheet.focus_row = None;
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            // A page starts at its top, whatever the previous one was scrolled to.
            sheet.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        }
        self.settings_page_opened(page, cx);
        cx.notify();
    }

    /// Types `query` into the search box: the pages that match stay in the list, and the
    /// selected page moves to the first of them when it is not one.
    pub fn set_settings_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        let hits: Vec<&'static str> = search(&sheet.entries, &sheet.pages, query, &|page| {
            self.page_search_text(page)
        })
        .into_iter()
        .map(|e| e.namespace)
        .collect();
        let mut moved = None;
        if let Some(sheet) = &mut self.sheet {
            sheet.query = query.to_owned();
            if let Some(first) = hits.first()
                && !hits.contains(&sheet.namespace)
            {
                sheet.namespace = first;
                sheet.edit = None;
                sheet.dropdown = None;
                sheet.layer = None;
                sheet.problems_open = false;
                sheet.focus_row = None;
                sheet.cursor = None;
                sheet.pack_filter.clear();
                sheet.pack_preview = None;
                sheet.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                moved = Some(*first);
            }
        }
        if let Some(page) = moved {
            self.settings_page_opened(page, cx);
        }
        cx.notify();
    }

    // ---- reading ----------------------------------------------------------------------------

    /// The pages of the list: all of them, or the ones the search leaves, in order.
    pub fn sheet_pages(&self) -> Vec<&'static str> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        search(&sheet.entries, &sheet.pages, &sheet.query, &|page| {
            self.page_search_text(page)
        })
        .into_iter()
        .map(|e| e.namespace)
        .collect()
    }

    pub(super) fn sheet_field(&self, field: &str) -> Option<(&'static str, Field, Json)> {
        let sheet = self.sheet.as_ref()?;
        let f = sheet.field(field)?.clone();
        let current = sheet.entry_of(f.namespace)?.current(&self.settings);
        let value = value_at(&current, &f.path).cloned().unwrap_or(Json::Null);
        Some((f.namespace, f, value))
    }

    /// The current value of `field` in the selected page, as JSON.
    pub fn sheet_value(&self, field: &str) -> Option<Json> {
        self.sheet_field(field).map(|(_, _, value)| value)
    }

    /// How `f` is edited: its schema type, where the app knows the set of values (a theme, a
    /// layout preset, the parts of a row, the tabs of the inspector) a choice.
    pub(super) fn sheet_kind_of(&self, f: &Field) -> FieldKind {
        if (f.namespace, f.dotted().as_str()) == ("layout", "preset") {
            let names = self
                .settings
                .get::<LayoutSettings>()
                .preset_defs()
                .into_iter()
                .map(|d| d.layout.name)
                .collect();
            return FieldKind::Enum(names);
        }
        if let Some(pack) = super::settings_themes::PackField::of(f.namespace, &f.dotted()) {
            return FieldKind::Enum(self.pack_values(pack));
        }
        refine_kind(f.namespace, &f.dotted(), &f.kind)
    }

    /// How the field `field` of the selected page is edited (see [`Self::sheet_kind_of`]).
    pub fn sheet_kind(&self, field: &str) -> Option<FieldKind> {
        let f = self.sheet.as_ref()?.field(field)?;
        Some(self.sheet_kind_of(f))
    }

    /// The range a number field works within, shown beside its box and applied when it is
    /// typed.
    pub fn sheet_range(&self, field: &str) -> Option<(f64, f64)> {
        let f = self.sheet.as_ref()?.field(field)?;
        number_range(f.namespace, &f.dotted())
    }

    // ---- writing ----------------------------------------------------------------------------

    /// Writes `value` to `field` of the selected page in the file of the layer being edited
    /// (see [`Self::sheet_edit_layer`]).
    pub fn sheet_write(&mut self, field: &str, value: toml::Value, cx: &mut Context<Self>) {
        self.write_field(field, None, value, cx);
    }

    /// [`Self::sheet_write`] for the key `sub` inside the map `field`.
    fn write_field(
        &mut self,
        field: &str,
        sub: Option<&str>,
        value: toml::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(f) = self.sheet.as_ref().and_then(|s| s.field(field)).cloned() else {
            return;
        };
        let layer = self.edit_layer_of(&f);
        let key_path = match sub {
            Some(sub) => format!("{}.{sub}", f.key_path()),
            None => f.key_path(),
        };
        let message = match self.settings.write(layer, &key_path, value) {
            Ok(()) => None,
            Err(err) => Some(err.to_string()),
        };
        if let Some(sheet) = &mut self.sheet {
            sheet.message = message;
            sheet.dropdown = None;
        }
        cx.notify();
    }

    pub fn sheet_toggle(&mut self, field: &str, cx: &mut Context<Self>) {
        if let Some((namespace, f, value)) = self.sheet_field(field)
            && f.kind == FieldKind::Bool
        {
            // Both daemon toggles are the workbench's: keep running is saved together with
            // `start_at_login` (see `set_keep_running`), starting at login registers the
            // launch agent.
            if namespace == "app" && field == "keep_running" {
                self.toggle_keep_running(cx);
                return;
            }
            if namespace == "app" && field == "start_at_login" {
                self.toggle_start_at_login(cx);
                return;
            }
            let on = value.as_bool().unwrap_or(false);
            self.sheet_write(field, toml::Value::Boolean(!on), cx);
        }
    }

    pub fn sheet_choose(&mut self, field: &str, choice: &str, cx: &mut Context<Self>) {
        if let Some(FieldKind::Enum(values)) = self.sheet_kind(field)
            && values.iter().any(|v| v == choice)
        {
            // A theme or icon pack: written once, and the default is a removed key.
            let pack = self
                .sheet
                .as_ref()
                .and_then(|s| s.field(field))
                .and_then(|f| super::settings_themes::PackField::of(f.namespace, &f.dotted()));
            match pack {
                Some(pack) => self.pack_apply(pack, choice, cx),
                None => self.sheet_write(field, toml::Value::String(choice.to_owned()), cx),
            }
        }
    }

    /// Removes the key of `field` from the layer being edited, so the value falls back to the
    /// layer below (the default, when no other layer sets it). Nothing is written.
    pub fn sheet_reset(&mut self, field: &str, cx: &mut Context<Self>) {
        let Some(f) = self.sheet.as_ref().and_then(|s| s.field(field)).cloned() else {
            return;
        };
        let layer = self.edit_layer_of(&f);
        let message = self
            .settings
            .remove(layer, f.namespace, &f.dotted())
            .err()
            .map(|err| err.to_string());
        if let Some(sheet) = &mut self.sheet {
            sheet.message = message;
            sheet.dropdown = None;
            sheet.edit = None;
        }
        cx.notify();
    }

    /// The strings of the list `field`; `None` when `field` is not a list.
    fn sheet_list(&self, field: &str) -> Option<(FieldKind, Vec<String>)> {
        let (_, f, value) = self.sheet_field(field)?;
        let kind = self.sheet_kind_of(&f);
        matches!(kind, FieldKind::StringList | FieldKind::ChoiceList(_)).then(|| {
            let items = value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            (kind, items)
        })
    }

    fn sheet_write_list(&mut self, field: &str, items: Vec<String>, cx: &mut Context<Self>) {
        self.sheet_write(
            field,
            toml::Value::Array(items.into_iter().map(toml::Value::String).collect()),
            cx,
        );
    }

    pub fn sheet_remove_list_item(&mut self, field: &str, index: usize, cx: &mut Context<Self>) {
        let Some((_, mut items)) = self.sheet_list(field) else {
            return;
        };
        if index < items.len() {
            items.remove(index);
            self.sheet_write_list(field, items, cx);
        }
    }

    /// Adds `item` at the end of the list `field`. A list of choices takes only its choices,
    /// each once.
    pub fn sheet_add_list_item(&mut self, field: &str, item: &str, cx: &mut Context<Self>) {
        let Some((kind, mut items)) = self.sheet_list(field) else {
            return;
        };
        if let FieldKind::ChoiceList(choices) = &kind
            && (!choices.iter().any(|c| c == item) || items.iter().any(|i| i == item))
        {
            return;
        }
        items.push(item.to_owned());
        self.sheet_write_list(field, items, cx);
    }

    /// Moves item `index` of a string list by `delta` places (negative = up), keeping the
    /// order the user wants (e.g. the fields of `panels.sessions.row`).
    pub fn sheet_move_list_item(
        &mut self,
        field: &str,
        index: usize,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let Some((_, mut items)) = self.sheet_list(field) else {
            return;
        };
        let Some(target) = index
            .checked_add_signed(delta)
            .filter(|t| *t < items.len() && index < items.len())
        else {
            return;
        };
        items.swap(index, target);
        self.sheet_write_list(field, items, cx);
    }

    // ---- maps (`terminal.env`) ----------------------------------------------------------------

    /// The rows of the map `field`, by name.
    pub fn sheet_map(&self, field: &str) -> Option<BTreeMap<String, String>> {
        let (_, f, value) = self.sheet_field(field)?;
        (f.kind == FieldKind::StringMap).then(|| {
            value
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// Sets row `name` of the map `field` to `value` in the layer being edited.
    pub fn sheet_map_set(
        &mut self,
        field: &str,
        name: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.sheet_map(field).is_none() {
            return Err("this field is not a list of variables".to_owned());
        }
        if !valid_env_name(name) {
            return Err(words::ENV_BAD_NAME.to_owned());
        }
        if value.contains('\0') {
            return Err(words::ENV_BAD_VALUE.to_owned());
        }
        self.write_field(field, Some(name), toml::Value::String(value.to_owned()), cx);
        Ok(())
    }

    /// Removes row `name` of the map `field` from the layer being edited.
    pub fn sheet_map_remove(&mut self, field: &str, name: &str, cx: &mut Context<Self>) {
        let Some(f) = self.sheet.as_ref().and_then(|s| s.field(field)).cloned() else {
            return;
        };
        if f.kind != FieldKind::StringMap {
            return;
        }
        let layer = self.edit_layer_of(&f);
        let message =
            match self
                .settings
                .remove(layer, f.namespace, &format!("{}.{name}", f.dotted()))
            {
                Ok(true) => None,
                Ok(false) => Some(words::map_row_elsewhere(name, &self.layer_label(layer, cx))),
                Err(err) => Some(err.to_string()),
            };
        if let Some(sheet) = &mut self.sheet {
            sheet.message = message;
        }
        cx.notify();
    }

    // ---- text editing -----------------------------------------------------------------------

    /// Starts editing `field` (a number/string box, "add item" of a list, "add variable" of a
    /// map).
    pub fn sheet_begin_edit(&mut self, field: &str, cx: &mut Context<Self>) {
        let Some((_, f, value)) = self.sheet_field(field) else {
            return;
        };
        let (text, list_add) = match self.sheet_kind_of(&f) {
            FieldKind::Integer | FieldKind::Number | FieldKind::String => {
                (display_value(&value), false)
            }
            FieldKind::StringList | FieldKind::ChoiceList(_) | FieldKind::StringMap => {
                (String::new(), true)
            }
            _ => return,
        };
        self.start_edit(field, text, list_add, None, cx);
    }

    /// Starts editing the value of row `name` of the map `field`.
    pub fn sheet_begin_edit_entry(&mut self, field: &str, name: &str, cx: &mut Context<Self>) {
        let Some(rows) = self.sheet_map(field) else {
            return;
        };
        let Some(text) = rows.get(name).cloned() else {
            return;
        };
        self.start_edit(field, text, false, Some(name.to_owned()), cx);
    }

    fn start_edit(
        &mut self,
        field: &str,
        text: String,
        list_add: bool,
        entry: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.edits.sheet_value.reset();
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.edit = Some(Edit {
                field: field.to_owned(),
                text,
                list_add,
                entry,
                error: None,
            });
            cx.notify();
        }
    }

    /// Replaces the text of the edit in progress.
    pub fn sheet_set_edit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(Edit {
            text: current,
            error,
            ..
        }) = self.sheet.as_mut().and_then(|s| s.edit.as_mut())
        {
            *current = text.to_owned();
            *error = None;
            cx.notify();
        }
    }

    /// Validates and writes the edit in progress (Enter). A value that does not fit the field
    /// stays in the box with an error; a number outside the range the app works within is
    /// kept inside it.
    pub fn sheet_commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.sheet.as_ref().and_then(|s| s.edit.clone()) else {
            return;
        };
        if let Some(key) = edit.field.strip_prefix(super::settings_daemon::EDIT_PREFIX) {
            self.daemon_commit_edit(key, &edit, cx);
            return;
        }
        let Some((_, f, _)) = self.sheet_field(&edit.field) else {
            return;
        };
        let kind = self.sheet_kind_of(&f);
        let range = number_range(f.namespace, &f.dotted());
        let text = edit.text.trim();
        let mut clamped = None;
        let parsed: Result<toml::Value, String> = match (&kind, edit.list_add) {
            (FieldKind::Integer, _) => text
                .parse::<i64>()
                .map_err(|_| "expected a whole number".to_owned())
                .map(|n| match range {
                    Some((low, high)) => {
                        // The ends are whole numbers; a value inside is kept as typed.
                        let kept = n.clamp(low as i64, high as i64);
                        if kept != n {
                            clamped = Some((low, high));
                        }
                        toml::Value::Integer(kept)
                    }
                    None => toml::Value::Integer(n),
                }),
            (FieldKind::Number, _) => text
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .map(|n| match range {
                    Some((low, high)) => {
                        let kept = n.clamp(low, high);
                        if kept != n {
                            clamped = Some((low, high));
                        }
                        toml::Value::Float(kept)
                    }
                    None => toml::Value::Float(n),
                })
                .ok_or_else(|| "expected a number".to_owned()),
            (FieldKind::String, _) => Ok(toml::Value::String(edit.text.clone())),
            (FieldKind::StringList | FieldKind::ChoiceList(_), true) => {
                let items = self.sheet_list(&edit.field).map(|(_, items)| items);
                match (&kind, items) {
                    _ if text.is_empty() => Err("type an item first".to_owned()),
                    (FieldKind::ChoiceList(choices), _) if !choices.iter().any(|c| c == text) => {
                        Err(format!("one of: {}", choices.join(", ")))
                    }
                    (FieldKind::ChoiceList(_), Some(items)) if items.iter().any(|i| i == text) => {
                        Err("already in the list".to_owned())
                    }
                    (_, items) => {
                        let mut items = items.unwrap_or_default();
                        items.push(text.to_owned());
                        Ok(toml::Value::Array(
                            items.into_iter().map(toml::Value::String).collect(),
                        ))
                    }
                }
            }
            (FieldKind::StringMap, true) => match edit.text.split_once('=') {
                Some((name, value)) => {
                    // A variable is its own key: written below, not as the whole map.
                    let (name, value) = (name.trim(), value.to_owned());
                    match self.sheet_map_set(&edit.field, name, &value, cx) {
                        Ok(()) => {
                            if let Some(sheet) = &mut self.sheet {
                                sheet.edit = None;
                            }
                            cx.notify();
                            return;
                        }
                        Err(message) => Err(message),
                    }
                }
                None => Err(words::ENV_ADD_FORMAT.to_owned()),
            },
            (FieldKind::StringMap, false) => match &edit.entry {
                Some(name) => match self.sheet_map_set(&edit.field, name, &edit.text, cx) {
                    Ok(()) => {
                        if let Some(sheet) = &mut self.sheet {
                            sheet.edit = None;
                        }
                        cx.notify();
                        return;
                    }
                    Err(message) => Err(message),
                },
                None => Err("this field cannot be edited here".to_owned()),
            },
            _ => Err("this field cannot be edited here".to_owned()),
        };
        match parsed {
            Ok(value) => {
                if let Some(sheet) = &mut self.sheet {
                    sheet.edit = None;
                }
                self.sheet_write(&edit.field, value, cx);
                if let Some((low, high)) = clamped {
                    self.set_notice(
                        words::kept_within(&words::field_name(f.namespace, &f.dotted()), low, high),
                        cx,
                    );
                }
            }
            Err(message) => {
                if let Some(Edit { error, .. }) = self.sheet.as_mut().and_then(|s| s.edit.as_mut())
                {
                    *error = Some(message);
                }
                cx.notify();
            }
        }
    }

    /// A key in the sheet: the search box or the value being edited takes it. Typed text arrives
    /// through the field's platform text input.
    pub(super) fn sheet_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sheet) = &self.sheet else {
            return;
        };
        let editing = sheet.edit.is_some();
        if event.keystroke.key == "enter" && editing {
            // A box of the Add account form may start the harness's login, which needs the window.
            let form_field = sheet
                .edit
                .as_ref()
                .and_then(|e| e.field.strip_prefix(super::settings_daemon::EDIT_PREFIX))
                .and_then(|f| f.strip_prefix(super::settings_accounts::FORM_PREFIX))
                .and_then(super::settings_accounts::AccountField::parse);
            if let Some(field) = form_field {
                self.account_commit_field(field, window, cx);
                return;
            }
            self.sheet_commit_edit(cx);
            return;
        }
        if self.sheet_navigation_key(event, window, cx) {
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "enter" {
            return;
        }
        let which = if editing {
            OverlayField::SheetValue
        } else {
            OverlayField::SheetQuery
        };
        if self.overlay_key(which, event, cx) {
            cx.stop_propagation();
        }
    }

    // ---- save layout as preset --------------------------------------------------------------

    /// Prompts for a name and writes the current docks and status bar as `layout.presets.<name>`.
    pub fn save_layout_as_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_prompt(
            PromptSpec {
                title: "Save layout as preset",
                message: "Preset name (letters, digits, - and _):",
                initial: "",
                confirm_label: "Save",
            },
            |this, name, _window, cx| this.write_layout_preset(&name, cx),
            window,
            cx,
        );
    }

    /// Writes the current layout as the user preset `name` and makes it the current preset
    /// (`layout.preset`, so the next start opens on it too).
    pub fn write_layout_preset(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = preset_name(name);
        if name.is_empty() {
            self.set_notice("A preset needs a name", cx);
            return;
        }
        let preset = LayoutSettings::preset_from(&self.state.docks, &self.status_bar());
        let value = match toml::Value::try_from(preset) {
            Ok(value) => value,
            Err(err) => {
                self.set_notice(format!("Cannot save the preset: {err}"), cx);
                return;
            }
        };
        let saved = self
            .settings
            .write_user(&format!("layout.presets.{name}"), value)
            .and_then(|()| {
                self.settings
                    .write_user("layout.preset", toml::Value::String(name.clone()))
            });
        match saved {
            Ok(()) => {
                // The preset is built from what is on screen: it is the current one now.
                self.state.preset = name.clone();
                self.set_notice(format!("Saved layout preset “{name}”"), cx);
            }
            Err(err) => self.set_notice(format!("Cannot save the preset: {err}"), cx),
        }
    }

    // ---- the page list and the footer ---------------------------------------------------------

    /// The page list as drawn: each group with the pages the search leaves, and whether its pages
    /// show. Advanced is closed until it was opened, holds the selected page or a search is on.
    pub fn sheet_nav(&self) -> Vec<(NavGroup, bool, Vec<&'static str>)> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        let pages = self.sheet_pages();
        NavGroup::ALL
            .into_iter()
            .filter_map(|group| {
                let own: Vec<&'static str> = pages
                    .iter()
                    .copied()
                    .filter(|page| NavGroup::of(page) == group)
                    .collect();
                if own.is_empty() {
                    return None;
                }
                let open = group != NavGroup::Advanced
                    || sheet.advanced_open
                    || !sheet.query.trim().is_empty()
                    || NavGroup::of(sheet.namespace) == group;
                Some((group, open, own))
            })
            .collect()
    }

    /// Opens or closes the Advanced group of the page list (what the user chose; a search or
    /// the selected page keep it open regardless).
    pub fn sheet_toggle_advanced(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.advanced_open = !sheet.advanced_open;
            cx.notify();
        }
    }

    /// Opens or closes the list of problems over the page. Nothing to open without problems.
    pub fn sheet_toggle_problems(&mut self, cx: &mut Context<Self>) {
        if self.sheet_problems().is_empty() {
            return;
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.problems_open = !sheet.problems_open;
            cx.notify();
        }
    }

    /// The problem lines of the selected page as drawn, in the order of
    /// [`Self::sheet_problems`]: severity, the file (its folder and name; the whole path is
    /// its hover), the key, and the reason with every token id in mono.
    fn problem_lines(&self, mono: &SharedString) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        let line = |severity: &str,
                    tone: gpui::Hsla,
                    file: Option<&std::path::Path>,
                    key: &str,
                    message: &str| {
            let mut parts: Vec<AnyElement> = Vec::new();
            if let Some(file) = file {
                let full: SharedString = file.display().to_string().into();
                parts.push(
                    div()
                        .id(SharedString::from(format!("problem-file-{full}")))
                        .text_color(theme.muted)
                        .tooltip({
                            let full = full.clone();
                            move |_, cx| cx.new(|_| TextTooltip(full.clone())).into()
                        })
                        .child(SharedString::from(short_path(file)))
                        .into_any_element(),
                );
            }
            if !key.is_empty() {
                parts.push(
                    div()
                        .font_family(mono.clone())
                        .child(SharedString::from(key.to_owned()))
                        .into_any_element(),
                );
            }
            // The reason is one flow of text, the ids in mono; the dots only part file, key and
            // reason.
            parts.push(
                div()
                    .flex()
                    .flex_wrap()
                    .children(token_runs(message).into_iter().map(|(text, id)| {
                        let piece = div().child(SharedString::from(text));
                        if id {
                            piece.font_family(mono.clone()).into_any_element()
                        } else {
                            piece.into_any_element()
                        }
                    }))
                    .into_any_element(),
            );
            let mut spans: Vec<AnyElement> = Vec::new();
            for (i, part) in parts.into_iter().enumerate() {
                if i > 0 {
                    spans.push(div().text_color(theme.faint).child("·").into_any_element());
                }
                spans.push(part);
            }
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(tokens.spacing))
                .py(px(2.0))
                .text_size(px(tokens.secondary_font_size))
                .child(
                    div()
                        .flex_none()
                        .w(px(56.0))
                        .text_color(tone)
                        .child(SharedString::from(severity.to_owned())),
                )
                .children(spans)
                .into_any_element()
        };
        let mut lines: Vec<AnyElement> = self
            .settings_problems()
            .iter()
            .map(|e| {
                let tone = if e.is_error() {
                    theme.danger
                } else {
                    theme.warning
                };
                line(
                    &e.severity.to_string(),
                    tone,
                    e.file.as_deref(),
                    &e.key_path,
                    &e.message,
                )
            })
            .collect();
        lines.extend(
            self.daemon_page_problems(sheet.namespace)
                .iter()
                .map(|message| {
                    line(
                        &Severity::Error.to_string(),
                        theme.danger,
                        None,
                        "",
                        message,
                    )
                }),
        );
        lines
    }

    // ---- rendering --------------------------------------------------------------------------

    pub(crate) fn render_settings_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.sync_dropdown_exit(cx);
        if self.sheet.is_some() {
            self.exits.sheet.cancel();
            return self.build_settings_sheet(true, cx);
        }
        let now = cx.background_executor().now();
        motion::while_leaving(
            self,
            now,
            |this| &mut this.sheet,
            |this| &mut this.exits.sheet,
            |this| this.build_settings_sheet(false, cx),
        )
        .flatten()
    }

    /// `wanted` is false while the sheet leaves: it takes no input.
    fn build_settings_sheet(&mut self, wanted: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.sheet.as_ref()?;
        // A catalog generation or a settings change since the last frame refreshes the preview.
        if wanted {
            self.sync_pack_preview();
        }
        if self.sheet.as_ref().is_some_and(|s| s.popover.is_some()) {
            return self.build_panel_popover(wanted, cx);
        }
        let sheet = self.sheet.as_ref()?;
        let scroll = sheet.scroll.clone();
        let theme = self.theme;
        let tokens = self.density.ui();
        let selected = sheet.namespace;
        let editing = sheet.edit.clone();
        let message = sheet.message.clone();
        let query = sheet.query.clone();
        let zone = sheet.zone;
        let problems_open = sheet.problems_open;
        let pages = self.sheet_pages();
        let errors = self.settings_problems();
        let daemon_problems = self.daemon_page_problems(selected);
        let problem_count = errors.len() + daemon_problems.len();
        let has_errors =
            !daemon_problems.is_empty() || errors.iter().any(|e| e.severity == Severity::Error);
        let no_match = !query.trim().is_empty() && pages.is_empty();

        // The page list: group labels over the pages, Advanced behind its own header.
        let mut nav: Vec<AnyElement> = Vec::new();
        for (index, (group, open, own)) in self.sheet_nav().into_iter().enumerate() {
            let forced = group == NavGroup::Advanced
                && (!query.trim().is_empty() || NavGroup::of(selected) == group);
            let label = div()
                .id(SharedString::from(format!("nav-group-{}", group.title())))
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .px(px(tokens.spacing * 2.0))
                .pt(px(if index == 0 {
                    2.0
                } else {
                    tokens.spacing * 3.0
                }))
                .pb(px(2.0))
                .child(super::ui::section_label(group.title(), &theme, &tokens))
                .when(group == NavGroup::Advanced, |s| {
                    s.child(
                        div()
                            .text_size(px(tokens.section_font_size))
                            .text_color(theme.muted)
                            .child(if open { "▾" } else { "▸" }),
                    )
                    .when(!forced, |s| {
                        s.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                            this.sheet_toggle_advanced(cx);
                        }))
                    })
                });
            nav.push(label.into_any_element());
            let pages: Vec<AnyElement> = own
                .into_iter()
                .map(|ns| {
                    div()
                        .id(SharedString::from(format!("ns-{ns}")))
                        .flex()
                        .items_center()
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius))
                        .cursor_pointer()
                        .when(ns == selected, |s| s.bg(theme.sidebar_accent))
                        .when(ns != selected, |s| {
                            s.hover(move |s| s.bg(theme.sidebar_hover))
                        })
                        // The keyboard on the category list: the focus ring, not an outline.
                        .focus_ring(
                            ns == selected && zone == SheetFocus::Categories,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_settings_namespace(ns, cx);
                        }))
                        .child(SharedString::from(namespace_title(ns)))
                        .into_any_element()
                })
                .collect();
            if group == NavGroup::Advanced {
                // Advanced folds and unfolds: its pages are always built, the fold shows them.
                nav.push(
                    div()
                        .relative()
                        .child(motion::expand(
                            "nav-advanced",
                            open,
                            div().flex().flex_col().children(pages),
                        ))
                        .child(probe(&self.regions, Region::Settings("nav-advanced")))
                        .into_any_element(),
                );
            } else if open {
                nav.extend(pages);
            }
        }

        let mono = SharedString::from(super::terminals::mono_family(cx, &self.resolved_theme));
        let problem_lines = self.problem_lines(&mono);
        let rows = if no_match {
            vec![self.render_no_match(cx)]
        } else {
            match selected {
                "models" => vec![self.render_providers(cx)],
                "accounts" => vec![self.render_accounts(cx)],
                "data" => vec![self.render_data_page(cx)],
                page => self.render_page_rows(page, cx),
            }
        };
        let banner = self.render_restart_banner(cx);
        let actions = if no_match {
            div().into_any_element()
        } else {
            self.render_sheet_actions(cx)
        };

        let search_box = div()
            .id("sheet-search")
            .flex()
            .items_center()
            .h(px(tokens.control_height))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .outlined(&theme, &tokens)
            .focus_ring(
                // An open theme picker has the keyboard (its filter draws the ring).
                editing.is_none()
                    && zone == SheetFocus::Search
                    && self.open_pack_picker().is_none(),
                &theme,
                &tokens,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(sheet) = &mut this.sheet {
                    sheet.edit = None;
                    sheet.zone = super::settings_keys::SheetFocus::Search;
                    cx.notify();
                }
            }))
            .child(if editing.is_none() {
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .children(self.overlay_parts(OverlayField::SheetQuery, "Search settings", cx))
                    .into_any_element()
            } else if query.is_empty() {
                div()
                    .text_color(theme.muted)
                    .flex()
                    .items_center()
                    .gap(px(4.0))
                    .child(crate::icons::glyph(crate::icons::Icon::ActionSearch))
                    .child("Search settings")
                    .into_any_element()
            } else {
                div()
                    .child(SharedString::from(query.clone()))
                    .into_any_element()
            });

        // One muted line, 32 px whatever it says: the list of problems opens over the page. It is
        // there only when there is something to say: problems, or no match for the search.
        let show_footer = problem_count > 0 || no_match;
        let footer_text = if no_match && problem_count == 0 {
            words::NO_MATCH_FOOTER.to_owned()
        } else {
            format!(
                "{} · {problem_count} {}",
                words::PROBLEMS_COUNT,
                if problems_open { "▾" } else { "▸" }
            )
        };
        let footer = div()
            .id("sheet-footer")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(px(32.0))
            .px(px(tokens.spacing * 4.0))
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(tokens.secondary_font_size))
            .text_color(if has_errors {
                theme.danger
            } else {
                theme.muted
            })
            .when(problem_count > 0, |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(theme.surface_hover))
                    .on_click(cx.listener(|this, _, _, cx| this.sheet_toggle_problems(cx)))
            })
            .child(SharedString::from(footer_text))
            .child(probe(&self.regions, Region::Settings("sheet-footer")));

        Some(
            div()
                .id("sheet-backdrop")
                .absolute()
                .inset_0()
                .when(wanted, |s| s.shield_behind())
                .flex()
                .justify_center()
                .items_start()
                .pt(px(48.0))
                .bg(theme.scrim())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                )
                .child(
                    div()
                        .id("settings-sheet")
                        .key_context(super::vim_ui::vim_key_context(
                            "SettingsSheet",
                            self.overlay_vim_mode(OverlayField::SheetValue)
                                .or_else(|| self.overlay_vim_mode(OverlayField::SheetQuery)),
                        ))
                        .when(wanted, |s| s.track_focus(&self.sheet_focus))
                        .relative()
                        .w(px(880.0))
                        .h(px(600.0))
                        .flex()
                        .flex_col()
                        .popover(&theme, &tokens)
                        .p(px(0.0))
                        .overflow_hidden()
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        // A click inside the sheet puts the keyboard back in it, whatever had it.
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                window.focus(&this.sheet_focus, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            this.sheet_key(event, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &sheet_actions::Dismiss, window, cx| {
                            this.sheet_dismiss(window, cx);
                        }))
                        .child(
                            div()
                                .relative()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_none()
                                        .w(px(200.0))
                                        .pt(px(tokens.spacing * 3.0))
                                        .bg(theme.sidebar)
                                        .border_r_1()
                                        .border_color(theme.border)
                                        .child(search_box)
                                        .child(hutan_ui::scroll_y(
                                            "sheet-nav-scroll",
                                            div()
                                                .id("sheet-nav")
                                                .flex_1()
                                                .px(px(tokens.spacing * 2.0))
                                                .pb(px(tokens.spacing * 2.0))
                                                .children(nav),
                                        )),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_none()
                                                .items_center()
                                                .justify_between()
                                                .h(px(tokens.sidebar_row + 18.0))
                                                .px(px(tokens.spacing * 4.0))
                                                .border_b_1()
                                                .border_color(theme.border)
                                                .child(
                                                    div()
                                                        .text_size(px(15.0))
                                                        .font_weight(gpui::FontWeight::MEDIUM)
                                                        .child(SharedString::from(if no_match {
                                                            "Search".to_owned()
                                                        } else {
                                                            namespace_title(selected)
                                                        })),
                                                )
                                                .child(actions),
                                        )
                                        .children(banner)
                                        .child(
                                            hutan_ui::scroll_y(
                                                "sheet-fields-scroll",
                                                div()
                                                    .id("sheet-fields")
                                                    .flex_1()
                                                    .min_h_0()
                                                    .px(px(tokens.spacing * 4.0))
                                                    .when(rows.is_empty(), |s| {
                                                        s.child(
                                                            div()
                                                                .py(px(tokens.spacing * 3.0))
                                                                .text_color(theme.muted)
                                                                .child("No editable settings."),
                                                        )
                                                    })
                                                    .children(rows),
                                            )
                                            .tracking(&scroll),
                                        )
                                        .when_some(message, |s, message| {
                                            s.child(
                                                div()
                                                    .px(px(tokens.spacing * 4.0))
                                                    .py(px(tokens.spacing))
                                                    .text_color(theme.danger)
                                                    .child(SharedString::from(message)),
                                            )
                                        }),
                                )
                                .child(probe(&self.regions, Region::Settings("sheet-body"))),
                        )
                        .children(show_footer.then_some(footer))
                        // The list of problems: in front of the page, above the footer; the page
                        // and the footer keep their size.
                        .when(problems_open && problem_count > 0, |s| {
                            s.child(
                                hutan_ui::scroll_y(
                                    "sheet-problems-scroll",
                                    div()
                                        .id("sheet-problems")
                                        .absolute()
                                        .left_0()
                                        .right_0()
                                        .bottom(px(32.0))
                                        .max_h(px(220.0))
                                        .occlude()
                                        .px(px(tokens.spacing * 4.0))
                                        .py(px(tokens.spacing * 2.0))
                                        .bg(theme.surface)
                                        .border_t_1()
                                        .border_color(theme.border)
                                        .children(problem_lines),
                                )
                                .presented(
                                    "sheet-problems-motion",
                                    motion::Kind::Popover,
                                    true,
                                    motion::Slide::None,
                                ),
                            )
                        })
                        .presented(
                            "settings-sheet-motion",
                            motion::Kind::Sheet,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .presented(
                    "settings-scrim",
                    motion::Kind::Sheet,
                    wanted,
                    motion::Slide::None,
                )
                .inert(!wanted)
                .into_any_element(),
        )
    }
}

/// The last two parts of a path (`canopy-day/theme.toml`): where a problem is, short; the whole
/// path is the hover.
fn short_path(path: &std::path::Path) -> String {
    let parts: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let from = parts.len().saturating_sub(2);
    parts[from..].join("/")
}

/// Whether `word` is a token id (`diff.hunk.foreground`, `ui.accent`): lowercase dotted
/// segments that start with a letter, none only digits. A number (`4.5`), a version (`v1.0`)
/// or a sentence end is not.
fn is_token_id(word: &str) -> bool {
    let mut segments = word.split('.');
    let Some(first) = segments.next() else {
        return false;
    };
    let rest: Vec<&str> = segments.collect();
    !rest.is_empty()
        && first.starts_with(|c: char| c.is_ascii_lowercase())
        && std::iter::once(first).chain(rest).all(|segment| {
            !segment.is_empty()
                && !segment.chars().all(|c| c.is_ascii_digit())
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        })
}

/// A message split into runs of its own text, the token ids apart: `(text, is_token_id)`. The
/// plain runs keep their spaces and the punctuation after an id (`a.b:`), so the runs put
/// together are the message again.
pub(super) fn token_runs(message: &str) -> Vec<(String, bool)> {
    let mut runs: Vec<(String, bool)> = Vec::new();
    let mut plain_from = 0;
    let mut word_from: Option<usize> = None;
    let ends = message.len();
    for (at, c) in message.char_indices().chain(std::iter::once((ends, ' '))) {
        match (c.is_whitespace(), word_from) {
            (false, None) => word_from = Some(at),
            (true, Some(from)) => {
                word_from = None;
                let word = &message[from..at];
                let bare = word.trim_end_matches([',', ':', ';', '.']);
                if is_token_id(bare) {
                    if plain_from < from {
                        runs.push((message[plain_from..from].to_owned(), false));
                    }
                    runs.push((bare.to_owned(), true));
                    plain_from = from + bare.len();
                }
            }
            _ => {}
        }
    }
    if plain_from < ends {
        runs.push((message[plain_from..].to_owned(), false));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ids_are_set_apart_from_the_reason() {
        assert_eq!(
            token_runs(
                "diff.hunk.foreground on diff.hunk.background: contrast 4.26:1, below 4.5:1"
            ),
            vec![
                ("diff.hunk.foreground".to_owned(), true),
                (" on ".to_owned(), false),
                ("diff.hunk.background".to_owned(), true),
                (": contrast 4.26:1, below 4.5:1".to_owned(), false),
            ]
        );
        // The runs are the message.
        assert_eq!(
            token_runs("a.b and c.d.")
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<String>(),
            "a.b and c.d."
        );
        // A number, a version and a sentence end are words.
        assert_eq!(
            token_runs("v1.0 is not 4.5 enough."),
            vec![("v1.0 is not 4.5 enough.".to_owned(), false)]
        );
        assert!(is_token_id("ui.accent"));
        assert!(is_token_id("terminal.ansi-black"));
        assert!(!is_token_id("accent"));
        assert!(!is_token_id("Ui.accent"));
        assert!(!is_token_id("a..b"));
    }

    #[test]
    fn a_path_is_shortened_to_its_folder_and_file() {
        assert_eq!(
            short_path(std::path::Path::new(
                "/Users/a/home/themes/canopy-day/theme.toml"
            )),
            "canopy-day/theme.toml"
        );
        assert_eq!(
            short_path(std::path::Path::new("settings.toml")),
            "settings.toml"
        );
    }
}
