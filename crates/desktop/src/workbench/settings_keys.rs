//! The keyboard in the settings sheet.
//!
//! Tab and Shift-Tab move between three places: the search box, the list of pages and the
//! controls of the page. In the list, Up and Down (Home and End) select the previous or next
//! page. In the controls, Up and Down move between the rows and Return (or Space) does what
//! the row's control does: turns a toggle over, opens a choice, starts a text box, asks for a
//! path. An open choice is moved through with Up and Down; Return picks the option and Escape
//! closes it. Typing anywhere but the search box goes back to the search box.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use hutan_protocol::daemon_settings::DaemonSettingKind;

use super::settings_daemon::{MCP_KEYS, choice_text, choices_of, sections};
use super::settings_labels::{value_label, words};
use super::settings_schema::FieldKind;
use super::view::Workbench;
use super::vim_ui::OverlayField;
use crate::theme::DensityExt as _;

/// Where the keyboard is in the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetFocus {
    Search,
    Categories,
    Controls,
}

const ROW_FIELD: &str = "f:";
const ROW_DAEMON: &str = "d:";

/// The options of an open dropdown, `(value, label)`, and the value that is current.
pub(super) type DropdownOptions = (Vec<(String, String)>, Option<String>);

impl Workbench {
    // ---- reading -------------------------------------------------------------------------

    /// Where the keyboard is.
    pub fn sheet_focus_zone(&self) -> Option<SheetFocus> {
        self.sheet.as_ref().map(|s| s.zone)
    }

    /// The row of the page the keyboard is on (`f:<field>` for a field of the app, `d:<key>`
    /// for a daemon setting), while the keyboard is in the controls.
    pub fn sheet_focused_row(&self) -> Option<&str> {
        let sheet = self.sheet.as_ref()?;
        if sheet.zone == SheetFocus::Controls {
            sheet.focus_row.as_deref()
        } else {
            None
        }
    }

    /// The option of the open dropdown the arrow keys are on, if it was moved.
    pub fn sheet_dropdown_cursor(&self) -> Option<&str> {
        let sheet = self.sheet.as_ref()?;
        let (dropdown, value) = sheet.cursor.as_ref()?;
        (sheet.dropdown.as_ref() == Some(dropdown)).then_some(value.as_str())
    }

    /// Whether the arrow keys are on option `value` of the dropdown `tag`.
    pub(super) fn dropdown_cursor_at(&self, tag: &str, value: &str) -> bool {
        self.sheet.as_ref().is_some_and(|s| {
            s.cursor
                .as_ref()
                .is_some_and(|(t, v)| t == tag && v == value)
        })
    }

    /// The rows of the page the keyboard can be on, in the order they are drawn.
    pub fn sheet_focus_rows(&self, cx: &gpui::App) -> Vec<String> {
        let Some(sheet) = &self.sheet else {
            return Vec::new();
        };
        // No page is shown while the search matches nothing.
        if !sheet.query.trim().is_empty() && self.sheet_pages().is_empty() {
            return Vec::new();
        }
        let page = sheet.namespace;
        let fields: Vec<String> = sheet
            .fields()
            .iter()
            .filter(|f| !matches!(self.sheet_kind_of(f), FieldKind::Unsupported(_)))
            .map(|f| format!("{ROW_FIELD}{}", f.dotted()))
            .collect();
        let mut daemon: Vec<String> = Vec::new();
        let ready = self.daemon_connected(cx)
            && !self.daemon_settings.unsupported()
            && self.daemon_settings.settings().is_some();
        if ready {
            for section in sections(page) {
                for key in section.keys {
                    if *key == MCP_KEYS {
                        daemon.extend(
                            self.daemon_settings
                                .settings()
                                .into_iter()
                                .flat_map(|s| s.entries.iter())
                                .filter(|e| e.key.starts_with("mcp."))
                                .map(|e| format!("{ROW_DAEMON}{}", e.key)),
                        );
                        daemon.push(format!("{ROW_DAEMON}{MCP_KEYS}"));
                    } else if self.daemon_settings.entry(key).is_some() {
                        daemon.push(format!("{ROW_DAEMON}{key}"));
                    }
                }
            }
        }
        if page == "history" {
            daemon.into_iter().chain(fields).collect()
        } else {
            fields.into_iter().chain(daemon).collect()
        }
    }

    /// Scrolls the page so that the row the keyboard is on shows. Pages whose app-drawn rows
    /// come before the fields (General, History, Mawas) keep their scroll.
    fn scroll_focus_row_into_view(&self) {
        let Some(sheet) = &self.sheet else { return };
        if matches!(sheet.namespace, "app" | "history" | "assistant") {
            return;
        }
        let Some(field) = sheet
            .focus_row
            .as_deref()
            .and_then(|row| row.strip_prefix(ROW_FIELD))
        else {
            return;
        };
        if let Some(at) = sheet.fields().iter().position(|f| f.dotted() == field) {
            sheet.scroll.scroll_to_item(at);
        }
    }

    /// `el` with a mark when the keyboard is on the row `row`.
    pub(super) fn focus_ring(&self, row: &str, el: AnyElement) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        if self.sheet_focused_row() != Some(row) {
            return el;
        }
        div()
            .border_l_2()
            .border_color(theme.accent)
            .pl(px(tokens.spacing))
            .child(el)
            .into_any_element()
    }

    // ---- moving --------------------------------------------------------------------------

    /// Tab (`backwards`: Shift-Tab): the next place of the sheet. A place with nothing to be on
    /// (no page matches the search, a page without controls) is skipped.
    pub fn sheet_tab(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        let mut places = vec![SheetFocus::Search];
        if !self.sheet_pages().is_empty() {
            places.push(SheetFocus::Categories);
        }
        if !self.sheet_focus_rows(cx).is_empty() {
            places.push(SheetFocus::Controls);
        }
        let at = places.iter().position(|p| *p == sheet.zone).unwrap_or(0);
        let next = if backwards {
            (at + places.len() - 1) % places.len()
        } else {
            (at + 1) % places.len()
        };
        let zone = places[next];
        let rows = self.sheet_focus_rows(cx);
        if let Some(sheet) = &mut self.sheet {
            sheet.zone = zone;
            sheet.edit = None;
            sheet.dropdown = None;
            sheet.cursor = None;
            if zone == SheetFocus::Controls
                && !sheet
                    .focus_row
                    .as_ref()
                    .is_some_and(|row| rows.contains(row))
            {
                sheet.focus_row = rows.first().cloned();
            }
        }
        cx.notify();
    }

    /// Up (`-1`) or Down (`1`) in the list of pages or the rows of the page.
    pub fn sheet_arrow(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        match sheet.zone {
            SheetFocus::Search => {}
            SheetFocus::Categories => {
                let pages = self.sheet_pages();
                let at = pages
                    .iter()
                    .position(|p| *p == sheet.namespace)
                    .unwrap_or(0);
                if let Some(next) = at
                    .checked_add_signed(delta)
                    .and_then(|next| pages.get(next).copied())
                {
                    self.select_settings_namespace(next, cx);
                }
            }
            SheetFocus::Controls => {
                let rows = self.sheet_focus_rows(cx);
                let at = sheet
                    .focus_row
                    .as_ref()
                    .and_then(|row| rows.iter().position(|r| r == row));
                let next = match at {
                    Some(at) => at
                        .checked_add_signed(delta)
                        .filter(|next| *next < rows.len())
                        .unwrap_or(at),
                    None => 0,
                };
                if let Some(sheet) = &mut self.sheet {
                    sheet.focus_row = rows.get(next).cloned();
                }
                self.scroll_focus_row_into_view();
                cx.notify();
            }
        }
    }

    /// Home or End in the list of pages.
    pub fn sheet_arrow_end(&mut self, last: bool, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        if sheet.zone != SheetFocus::Categories {
            return;
        }
        let pages = self.sheet_pages();
        let target = if last { pages.last() } else { pages.first() };
        if let Some(page) = target.copied() {
            self.select_settings_namespace(page, cx);
        }
    }

    // ---- the controls --------------------------------------------------------------------

    /// Return or Space on the focused row: what its control does.
    pub fn sheet_activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.sheet_focused_row().map(str::to_owned) else {
            return;
        };
        if let Some(field) = row.strip_prefix(ROW_FIELD) {
            let Some(kind) = self.sheet_kind(field) else {
                return;
            };
            match kind {
                FieldKind::Bool => self.sheet_toggle_in(field, window, cx),
                FieldKind::Enum(_) => {
                    let pack = self
                        .sheet
                        .as_ref()
                        .and_then(|s| s.field(field))
                        .and_then(|f| {
                            super::settings_themes::PackField::of(f.namespace, &f.dotted())
                        });
                    match pack {
                        Some(pack) => self.toggle_pack_picker(pack, cx),
                        None => self.open_dropdown(field.to_owned(), cx),
                    }
                }
                FieldKind::ChoiceList(_) => self.open_dropdown(format!("add:{field}"), cx),
                FieldKind::Integer
                | FieldKind::Number
                | FieldKind::String
                | FieldKind::StringList
                | FieldKind::StringMap => self.sheet_begin_edit(field, cx),
                FieldKind::Unsupported(_) => {}
            }
        } else if let Some(key) = row.strip_prefix(ROW_DAEMON) {
            if key == MCP_KEYS {
                self.mcp_begin_add(cx);
                return;
            }
            let Some(entry) = self.daemon_settings.entry(key).cloned() else {
                return;
            };
            if choices_of(&entry).is_some() {
                self.open_dropdown(format!("daemon:{key}"), cx);
                return;
            }
            match &entry.kind {
                DaemonSettingKind::Bool => self.daemon_toggle(key, cx),
                DaemonSettingKind::StringList {
                    choices: Some(_), ..
                } => self.open_dropdown(format!("add:daemon:{key}"), cx),
                DaemonSettingKind::Int { .. }
                | DaemonSettingKind::String
                | DaemonSettingKind::StringList { .. }
                | DaemonSettingKind::StringMap => self.daemon_begin_edit(key, cx),
                DaemonSettingKind::Path => self.daemon_choose_path(key, cx),
                DaemonSettingKind::Table => {
                    self.mcp_begin_edit(key.strip_prefix("mcp.").unwrap_or(key), cx);
                }
                DaemonSettingKind::Enum { .. } => {}
            }
        }
    }

    fn open_dropdown(&mut self, tag: String, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = Some(tag);
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
            cx.notify();
        }
    }

    // ---- an open dropdown ----------------------------------------------------------------

    /// The options of the open dropdown `tag`, and the value that is current.
    fn dropdown_options(&self, tag: &str, cx: &gpui::App) -> Option<DropdownOptions> {
        if let Some(key) = tag.strip_prefix("add:daemon:") {
            let entry = self.daemon_settings.entry(key)?;
            let DaemonSettingKind::StringList {
                choices: Some(choices),
            } = &entry.kind
            else {
                return None;
            };
            let items = self.daemon_list(key);
            let options = choices
                .iter()
                .filter(|c| !items.contains(c))
                .map(|c| {
                    (
                        c.clone(),
                        super::settings_labels::daemon_value_label(key, c),
                    )
                })
                .collect();
            return Some((options, None));
        }
        if let Some(key) = tag.strip_prefix("daemon:") {
            let entry = self.daemon_settings.entry(key)?;
            return Some((choices_of(entry)?, Some(choice_text(&entry.value))));
        }
        if let Some(scope) = tag.strip_prefix("assistant-model:") {
            return self.assistant_model_options(scope, cx);
        }
        if let Some(field) = tag.strip_prefix("add:") {
            let FieldKind::ChoiceList(choices) = self.sheet_kind(field)? else {
                return None;
            };
            let (namespace, _, value) = self.sheet_field(field)?;
            let items: Vec<String> = value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let options = choices
                .iter()
                .filter(|c| !items.contains(c))
                .map(|c| (c.clone(), value_label(namespace, field, c)))
                .collect();
            return Some((options, None));
        }
        let FieldKind::Enum(values) = self.sheet_kind(tag)? else {
            return None;
        };
        let (namespace, f, value) = self.sheet_field(tag)?;
        if let Some(pack) = super::settings_themes::PackField::of(namespace, &f.dotted()) {
            // The packs that fit the search; what the arrow keys move through.
            return Some((self.pack_options(pack), value.as_str().map(str::to_owned)));
        }
        let options = values
            .iter()
            .map(|v| (v.clone(), value_label(namespace, tag, v)))
            .collect();
        Some((options, value.as_str().map(str::to_owned)))
    }

    /// Up or Down in the open dropdown: the next option, starting from the current value.
    pub fn sheet_dropdown_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(tag) = self.sheet.as_ref().and_then(|s| s.dropdown.clone()) else {
            return;
        };
        let Some((options, current)) = self.dropdown_options(&tag, cx) else {
            return;
        };
        if options.is_empty() {
            return;
        }
        let moved = self
            .sheet
            .as_ref()
            .and_then(|s| s.cursor.as_ref())
            .filter(|(t, _)| *t == tag)
            .map(|(_, v)| v.clone());
        let at = moved
            .or(current)
            .and_then(|value| options.iter().position(|(v, _)| *v == value));
        let next = match at {
            Some(at) => at
                .checked_add_signed(delta)
                .filter(|next| *next < options.len())
                .unwrap_or(at),
            None if delta < 0 => options.len() - 1,
            None => 0,
        };
        if let Some(sheet) = &mut self.sheet {
            sheet.cursor = Some((tag, options[next].0.clone()));
        }
        // A picker previews the pack the cursor moved to.
        self.sync_pack_preview();
        cx.notify();
    }

    /// Return in the open dropdown: picks the option the arrow keys are on (the current one
    /// when they were not used), which closes it.
    pub fn sheet_dropdown_select(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = self.sheet.as_ref().and_then(|s| s.dropdown.clone()) else {
            return;
        };
        let Some((options, current)) = self.dropdown_options(&tag, cx) else {
            return;
        };
        let value = self
            .sheet
            .as_ref()
            .and_then(|s| s.cursor.as_ref())
            .filter(|(t, _)| *t == tag)
            .map(|(_, v)| v.clone())
            .or(current)
            .filter(|v| options.iter().any(|(o, _)| o == v));
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.cursor = None;
            sheet.pack_filter.clear();
            sheet.pack_preview = None;
        }
        let Some(value) = value else {
            cx.notify();
            return;
        };
        if let Some(key) = tag.strip_prefix("add:daemon:") {
            self.daemon_list_add(key, &value, cx);
        } else if let Some(key) = tag.strip_prefix("daemon:") {
            self.daemon_choose(key, &value, cx);
        } else if let Some(scope) = tag.strip_prefix("assistant-model:") {
            self.assistant_choose_model_in(scope, &value, cx);
        } else if let Some(field) = tag.strip_prefix("add:") {
            self.sheet_add_list_item(field, &value, cx);
        } else {
            self.sheet_choose(&tag, &value, cx);
        }
        cx.notify();
    }

    // ---- keys ----------------------------------------------------------------------------

    /// A key in the sheet. `true` when it was used.
    pub(super) fn sheet_navigation_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(sheet) = &self.sheet else {
            return false;
        };
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if mods.platform || mods.control || mods.alt {
            return false;
        }
        // A text box being edited takes everything but Tab.
        if sheet.edit.is_some() {
            return key == "tab";
        }
        if sheet.dropdown.is_some() {
            // A theme picker's search box takes the typed characters.
            if self.pack_picker_key(event, cx) {
                return true;
            }
            match key {
                "down" => self.sheet_dropdown_move(1, cx),
                "up" => self.sheet_dropdown_move(-1, cx),
                "enter" => self.sheet_dropdown_select(cx),
                "tab" => self.sheet_tab(mods.shift, cx),
                _ => return false,
            }
            return true;
        }
        match (key, sheet.zone) {
            ("tab", _) => self.sheet_tab(mods.shift, cx),
            ("down", SheetFocus::Categories | SheetFocus::Controls) => self.sheet_arrow(1, cx),
            ("up", SheetFocus::Categories | SheetFocus::Controls) => self.sheet_arrow(-1, cx),
            ("home", SheetFocus::Categories) => self.sheet_arrow_end(false, cx),
            ("end", SheetFocus::Categories) => self.sheet_arrow_end(true, cx),
            ("enter" | "space", SheetFocus::Controls) => self.sheet_activate(window, cx),
            ("enter", SheetFocus::Categories) => {
                // Into the page that was chosen.
                if !self.sheet_focus_rows(cx).is_empty() {
                    self.sheet_tab(false, cx);
                }
            }
            (_, SheetFocus::Categories | SheetFocus::Controls)
                if event.keystroke.key_char.is_some() =>
            {
                // Typing is searching: the keyboard goes back to the search box, which takes
                // the key.
                if let Some(sheet) = &mut self.sheet {
                    sheet.zone = SheetFocus::Search;
                }
                cx.notify();
                return self.overlay_key(OverlayField::SheetQuery, event, cx);
            }
            _ => return false,
        }
        true
    }

    /// The note that stands in the page's place while no page matches the search.
    pub(super) fn render_no_match(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let query = self
            .sheet
            .as_ref()
            .map(|s| s.query.trim().to_owned())
            .unwrap_or_default();
        div()
            .id("sheet-no-match")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing * 4.0))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(gpui::SharedString::from(words::no_match(&query))),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .child(gpui::SharedString::from(words::NO_MATCH_HINT)),
            )
            .child(
                div().flex().child(
                    super::settings_rows::small_button(
                        "clear-search",
                        words::CLEAR_SEARCH,
                        false,
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_settings_query("", cx);
                        this.edits.sheet_query.reset();
                    }))
                    .child(super::render::probe(
                        &self.regions,
                        super::view::Region::Settings("clear-search"),
                    )),
                ),
            )
            .into_any_element()
    }
}
