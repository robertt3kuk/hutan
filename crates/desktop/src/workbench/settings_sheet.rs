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
use super::settings_keys::SheetFocus;
use super::settings_labels::{namespace_title, words};
use super::settings_pages::{preset_name, valid_env_name};
use super::settings_schema::{
    CatalogEntry, Field, FieldKind, catalog, number_range, page_fields, refine_kind, search,
    value_at,
};
use super::ui::StyledExt as _;
use super::view::Workbench;
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
}

impl SettingsSheet {
    /// The selected page (`terminal`, `panels.sessions`, `models`, `data`, …).
    pub fn namespace(&self) -> &str {
        self.namespace
    }

    pub fn query(&self) -> &str {
        &self.query
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
fn problem_text(e: &hutan_settings::SettingsError) -> String {
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

    /// The lines of the Problems footer of the selected page, in the order they are drawn: the
    /// settings files', then the daemon's (`daemon.toml` that cannot be read, a refused field,
    /// a provider error on the Models page).
    pub fn sheet_problems(&self) -> Vec<String> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        let mut lines: Vec<String> = self.settings.errors().iter().map(problem_text).collect();
        lines.extend(self.daemon_page_problems(sheet.namespace));
        lines
    }

    pub fn settings_sheet(&self) -> Option<&SettingsSheet> {
        self.sheet.as_ref()
    }

    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// Escape: cancels an edit or a dropdown first, then closes the sheet.
    pub(crate) fn sheet_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Under Vim mode the first Escape of a text field leaves Insert mode.
        if self.sheet_vim_escape(cx) {
            return;
        }
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        let abandoned = sheet.edit.take();
        let closed_dropdown = sheet.dropdown.take().is_some();
        if abandoned.is_some() || closed_dropdown {
            // Giving up an edit the daemon refused shows the setting as it is.
            if let Some(key) = abandoned
                .as_ref()
                .and_then(|e| e.field.strip_prefix(super::settings_daemon::EDIT_PREFIX))
            {
                self.daemon_settings.discard_refused(key);
            }
            cx.notify();
        } else {
            self.close_settings(window, cx);
        }
    }

    /// Escape in the sheet's text fields under Vim mode: `true` when Vim used it.
    fn sheet_vim_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.vim_config().enabled {
            return false;
        }
        let Some(sheet) = &self.sheet else {
            return false;
        };
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
            sheet.zone = super::settings_keys::SheetFocus::Categories;
            sheet.focus_row = None;
            sheet.cursor = None;
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
                sheet.focus_row = None;
                sheet.cursor = None;
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
            self.sheet_write(field, toml::Value::String(choice.to_owned()), cx);
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
    fn sheet_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else {
            return;
        };
        let editing = sheet.edit.is_some();
        if event.keystroke.key == "enter" && editing {
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

    // ---- rendering --------------------------------------------------------------------------

    pub(crate) fn render_settings_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet.as_ref()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let selected = sheet.namespace;
        let editing = sheet.edit.clone();
        let message = sheet.message.clone();
        let query = sheet.query.clone();
        let zone = sheet.zone;
        let pages = self.sheet_pages();
        let errors = self.settings.errors();

        let nav: Vec<AnyElement> = pages
            .iter()
            .map(|ns| {
                let ns: &'static str = ns;
                div()
                    .id(SharedString::from(format!("ns-{ns}")))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 3.0))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(ns == selected, move |s| s.bg(theme.accent_soft))
                    .when(ns == selected && zone == SheetFocus::Categories, move |s| {
                        s.border_1().border_color(theme.accent)
                    })
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_settings_namespace(ns, cx);
                    }))
                    .child(SharedString::from(namespace_title(ns)))
                    .into_any_element()
            })
            .collect();

        let problems: Vec<AnyElement> = errors
            .iter()
            .map(|e| {
                let color = if e.is_error() {
                    theme.danger
                } else {
                    theme.warning
                };
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.secondary_font_size))
                    .child(
                        div()
                            .flex_none()
                            .text_color(color)
                            .child(SharedString::from(e.severity.to_string())),
                    )
                    .child(div().min_w_0().child(SharedString::from(problem_text(e))))
                    .into_any_element()
            })
            .collect();
        let daemon_problems = self.daemon_page_problems(selected);
        let problems: Vec<AnyElement> = problems
            .into_iter()
            .chain(daemon_problems.iter().map(|message| {
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.secondary_font_size))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.danger)
                            .child(SharedString::from(Severity::Error.to_string())),
                    )
                    .child(div().min_w_0().child(SharedString::from(message.clone())))
                    .into_any_element()
            }))
            .collect();
        let problem_count = problems.len();
        let has_errors =
            !daemon_problems.is_empty() || errors.iter().any(|e| e.severity == Severity::Error);
        let no_match = !query.trim().is_empty() && pages.is_empty();
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
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .border_1()
            .border_color(if editing.is_none() && zone == SheetFocus::Search {
                theme.accent
            } else {
                theme.border
            })
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
                    .text_color(theme.faint)
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

        Some(
            div()
                .id("sheet-backdrop")
                .absolute()
                .inset_0()
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
                        .key_context("SettingsSheet")
                        .track_focus(&self.sheet_focus)
                        .w(px(860.0))
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
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_none()
                                        .w(px(220.0))
                                        .py(px(tokens.spacing * 3.0))
                                        .bg(theme.sidebar)
                                        .border_r_1()
                                        .border_color(theme.border)
                                        .child(search_box)
                                        .child(
                                            div()
                                                .id("sheet-nav")
                                                .flex_1()
                                                .overflow_y_scroll()
                                                .px(px(tokens.spacing))
                                                .children(nav),
                                        ),
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
                                                .items_center()
                                                .justify_between()
                                                .h(px(tokens.sidebar_row + 8.0))
                                                .px(px(tokens.spacing * 4.0))
                                                .border_b_1()
                                                .border_color(theme.border)
                                                .child(
                                                    div()
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
                                            div()
                                                .id("sheet-fields")
                                                .flex_1()
                                                .min_h_0()
                                                .overflow_y_scroll()
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
                                        .when_some(message, |s, message| {
                                            s.child(
                                                div()
                                                    .px(px(tokens.spacing * 4.0))
                                                    .py(px(tokens.spacing))
                                                    .text_color(theme.danger)
                                                    .child(SharedString::from(message)),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .id("sheet-errors")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .max_h(px(110.0))
                                .overflow_y_scroll()
                                .gap(px(2.0))
                                .px(px(tokens.spacing * 4.0))
                                .py(px(tokens.spacing * 2.0))
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .text_size(px(tokens.section_font_size))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(if has_errors {
                                            theme.danger
                                        } else {
                                            theme.muted
                                        })
                                        .child(SharedString::from(
                                            if no_match && problem_count == 0 {
                                                words::NO_MATCH_FOOTER.to_owned()
                                            } else if problem_count == 0 {
                                                words::PROBLEMS_NONE.to_owned()
                                            } else {
                                                format!("Problems · {problem_count}")
                                            },
                                        )),
                                )
                                .children(problems),
                        ),
                )
                .into_any_element(),
        )
    }
}
