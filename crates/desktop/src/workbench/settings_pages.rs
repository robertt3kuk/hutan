//! What the settings sheet adds to the generated fields: the pages and sections the app draws
//! itself, and what they do.
//!
//! - History: a real On/Off for indexing (`history.set_enabled`) with the sources and size.
//! - Data & logs: the files and folders Hutan keeps, with Reveal in Finder and Open, what they
//!   weigh, and Restart daemon / Stop daemon.
//! - Agent Integrations: the hooks of each agent, with Install, Repair and Uninstall.
//! - Start Sheet: the "Don't ask again" choices, with Revoke.
//! - Keyboard: the bindings of `keymap.toml`, with Open and Reload.
//! - Terminal: the rows of `terminal.env`. Layout: the saved presets.
//!
//! Every action asks first when it changes a file that is not Hutan's, or cannot be undone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_protocol::history::HistoryStatusInfo;
use hutan_settings::SettingsLayer;
use hutan_settings::keymap::{BindingSpec, format_keys};
use serde_json::Value as Json;

use super::harnesses::{Detection, HARNESSES, harness_name};
use super::integrations::hook_files;
use super::modal::{Modal, ModalChoice};
use super::model::PanelId;
use super::panels::{HistoryPanel, format_bytes};
use super::render::probe;
use super::settings_labels::{namespace_description, namespace_title, page_title, words};
use super::settings_rows::{SheetRow, note, sheet_row};
use super::settings_schema::{Field, Filters, PAGES};
use super::settings_sheet::SettingsSheet;
use super::ui::{self, Button, FocusRingExt as _, IconButton};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::integrations::IntegrationStatus;
use crate::settings::{EditorSettings, LayoutSettings};
use crate::theme::DensityExt as _;
use crate::workbench::model::LayoutPreset;

/// Whether a section of `page` shows daemon settings (the sheet then asks the daemon for them
/// when it opens the page).
pub(super) fn page_has_daemon_settings(page: &str) -> bool {
    PAGES.iter().find(|p| p.id == page).is_some_and(|p| {
        p.sections
            .iter()
            .any(|section| super::settings_daemon::is_daemon_page(section))
    })
}

/// An invisible child of a section's head that, when painted, scrolls the page so that the head
/// is at the top of it: the page's offset plus the distance between the top of the page and
/// of the head (both as this frame paints them), kept within the page.
fn anchor_probe(
    scroll: gpui::ScrollHandle,
    settled: std::rc::Rc<std::cell::Cell<u8>>,
) -> impl IntoElement {
    gpui::canvas(
        |bounds, _, _| bounds,
        move |_, head, window, _| {
            let view = scroll.bounds();
            let mut offset = scroll.offset();
            let most = scroll.max_offset().y;
            let y = (offset.y + view.top() - head.top()).clamp(-most, px(0.0));
            if (y - offset.y).abs() > px(0.5) {
                offset.y = y;
                scroll.set_offset(offset);
                settled.set(0);
                window.refresh();
            } else {
                let held = settled.get().saturating_add(1);
                settled.set(held);
                if held < 2 {
                    // One more frame to see it hold; the sheet then stops asking.
                    window.refresh();
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// What the sheet knows about the history index (`history.status`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HistoryIndex {
    /// The last answer; `None` before the daemon was asked or while it is unreachable.
    pub status: Option<HistoryStatusInfo>,
    /// The connected daemon predates `history.*`.
    pub unsupported: bool,
    /// An On/Off is on its way.
    pub busy: bool,
    pub error: Option<String>,
}

/// A place Hutan keeps something, as the Data & logs page lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataTarget {
    /// The folder that holds all of the below (except the logs).
    Folder,
    /// `settings.toml`
    Settings,
    /// `keymap.toml`
    Keymap,
    /// `daemon.toml`
    Daemon,
    /// The folder of `desktop.log` and `hutand.log`.
    Logs,
}

impl DataTarget {
    pub const ALL: [DataTarget; 5] = [
        DataTarget::Folder,
        DataTarget::Settings,
        DataTarget::Keymap,
        DataTarget::Daemon,
        DataTarget::Logs,
    ];

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|t| *t == self)
            .unwrap_or_default()
    }

    /// What a new file starts with when "Open" creates it (Hutan's own files only).
    fn starter(self) -> Option<&'static str> {
        match self {
            DataTarget::Settings => Some(words::STARTER_SETTINGS),
            DataTarget::Keymap => Some(words::STARTER_KEYMAP),
            _ => None,
        }
    }
}

/// The size of a file; `None` when there is none.
fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// A name for a preset: letters, digits, `-` and `_` (anything else becomes `-`).
pub(super) fn preset_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Whether `name` can be an environment variable: letters, digits and `_`, not starting with a
/// digit.
pub(super) fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Workbench {
    // ---- opening a page ------------------------------------------------------------------------

    /// A page was selected: asks for what its sections show that is not in the settings files.
    pub(super) fn settings_page_opened(&mut self, page: &str, cx: &mut Context<Self>) {
        // A form that was not finished (an MCP server) does not outlive the page.
        self.daemon_settings.mcp_form = None;
        self.daemon_settings.account_form = None;
        let Some(page) = PAGES.iter().find(|p| p.id == page) else {
            return;
        };
        // The daemon's settings are asked once, whatever number of sections show them.
        let mut daemon = false;
        for section in page.sections {
            match *section {
                "models" => self.refresh_providers(cx),
                "accounts" => self.refresh_accounts(cx),
                "history" => {
                    self.refresh_history_index(cx);
                    daemon = true;
                }
                "data" => {
                    self.refresh_history_index(cx);
                    self.refresh_daemon_status(cx);
                }
                "integrations" => self.refresh_all_integration_statuses(cx),
                "automation" => self.refresh_hooks(cx),
                "assistant" => {
                    daemon = true;
                    self.refresh_assistant_defaults(cx);
                    self.refresh_mawas_grants(cx);
                    self.refresh_providers(cx);
                }
                "notifications" | "sessions_restore" | "control" | "agents_tools" => {
                    daemon = true;
                }
                _ => {}
            }
        }
        if daemon {
            self.refresh_daemon_settings(cx);
        }
    }

    /// What the problems footer of `page` lists besides the settings files' own: what the
    /// daemon's settings and the providers say is wrong in any of its sections.
    pub(super) fn sheet_page_problems(&self, page: &str) -> Vec<String> {
        let mut problems: Vec<String> = Vec::new();
        for section in PAGES
            .iter()
            .find(|p| p.id == page)
            .into_iter()
            .flat_map(|p| p.sections.iter())
        {
            for problem in self.daemon_page_problems(section) {
                // A file that cannot be read is a problem of every section that shows it.
                if !problems.contains(&problem) {
                    problems.push(problem);
                }
            }
        }
        problems
    }

    /// What a search of the sheet matches on `section` besides its title, fields and keywords:
    /// the bindings of the Keyboard section, the wording of the daemon settings of a daemon
    /// section.
    pub(super) fn page_search_text(&self, section: &str) -> String {
        if section != "keymap" {
            return super::settings_daemon::search_text(section);
        }
        self.custom_bindings()
            .iter()
            .map(binding_text)
            .chain(self.vim_key_rows().iter().map(VimKeyRow::text))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether `field` passes the filters of the search box: `@modified` wants a key some
    /// settings file sets (what a ↺ takes back), `@restart` a setting that waits for the
    /// daemon to start, and the app's own settings never do.
    pub(super) fn field_passes(&self, f: &Field, filters: Filters) -> bool {
        (!filters.modified || self.settings.source_of(&f.key_path()).is_some()) && !filters.restart
    }

    /// Whether `section` has rows to show under `filters`: always without a filter, else a
    /// setting of the app or the daemon that passes it (what the sheet draws itself, a
    /// provider or a hook, is not a setting a filter picks).
    pub(super) fn section_has_rows(&self, section: &str, filters: Filters) -> bool {
        if !filters.any() {
            return true;
        }
        self.sheet
            .as_ref()
            .and_then(|sheet| sheet.sections.get(section))
            .is_some_and(|fields| fields.iter().any(|f| self.field_passes(f, filters)))
            || self.daemon_section_passes(section, filters)
    }

    /// The sections of `page` as the sheet draws them: each under its own head (a page of one
    /// section, or a section called like its page, has none), what used to be the page below
    /// it. The head of each is probed (`Region::SettingsSection`), which is where a deep link
    /// scrolls to.
    pub(super) fn render_page_sections(
        &mut self,
        page: &str,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, bool) {
        let Some(nav) = PAGES.iter().find(|p| p.id == page) else {
            return (Vec::new(), false);
        };
        // The head the page is scrolled to while it is being filled: it moves the page where
        // it is painted (`anchor_probe`).
        let anchor = self
            .sheet
            .as_ref()
            .filter(|s| s.anchor > 0 && s.popover.is_none())
            .map(|s| (s.namespace, s.scroll.clone(), s.anchor_settled.clone()));
        let mut anchor_drawn = false;
        let (theme, tokens) = (self.theme, self.density.ui());
        let filters = self
            .sheet
            .as_ref()
            .map(SettingsSheet::filters)
            .unwrap_or_default();
        let headed = nav.sections.len() > 1;
        let mut out: Vec<AnyElement> = Vec::new();
        // What is wrong with the daemon's settings is said once per page, over the section the
        // sheet is at when it has daemon settings (else the first that has some); the others
        // are left out while that stands in for them.
        let daemon_sections = || {
            nav.sections
                .iter()
                .copied()
                .filter(|s| super::settings_daemon::is_daemon_page(s))
        };
        let at = self.sheet.as_ref().map_or("", |s| s.namespace);
        let owner = daemon_sections()
            .find(|s| *s == at)
            .or_else(|| daemon_sections().next());
        for section in nav.sections {
            if !self.section_has_rows(section, filters) {
                continue;
            }
            let daemon_section = super::settings_daemon::is_daemon_page(section);
            if daemon_section && owner != Some(*section) && self.daemon_gate(cx).is_err() {
                continue;
            }
            if headed && namespace_title(section) != page_title(page) {
                out.push(
                    super::ui::page_section_head(
                        namespace_title(section),
                        namespace_description(section),
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .child(probe(&self.regions, Region::SettingsSection(section)))
                    .children(
                        anchor
                            .as_ref()
                            .filter(|(target, _, _)| target == section)
                            .map(|(_, scroll, settled)| {
                                anchor_probe(scroll.clone(), settled.clone())
                            }),
                    )
                    .into_any_element(),
                );
                anchor_drawn |= anchor
                    .as_ref()
                    .is_some_and(|(target, _, _)| target == section);
            }
            match *section {
                "models" => out.push(self.render_providers(cx)),
                "accounts" => out.push(self.render_accounts(cx)),
                "data" => {
                    let page = self.render_data_page(cx);
                    out.push(super::ui::card_section(vec![page], &theme, &tokens));
                }
                section => {
                    let rows =
                        self.render_section_rows(section, filters, owner == Some(section), cx);
                    out.extend(super::ui::group_sections(rows, &theme, &tokens));
                }
            }
        }
        (out, anchor_drawn)
    }

    /// The rows of a section that is generated from the schema, with what the app adds.
    pub(super) fn render_section_rows(
        &mut self,
        section: &str,
        filters: Filters,
        banner: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let fields: Vec<Field> = self
            .sheet
            .as_ref()
            .and_then(|s| s.sections.get(section))
            .map(|fields| {
                fields
                    .iter()
                    .filter(|f| self.field_passes(f, filters))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let mut rows = Vec::new();
        // What the app draws itself comes with no filter only: a filter lists settings.
        let own = !filters.any();
        match section {
            "app" if own => rows.push(self.render_run_setup_row(cx)),
            "history" => {
                if own {
                    rows.push(self.render_history_indexing(cx));
                }
                rows.extend(self.render_daemon_page("history", filters, banner, cx));
                if !fields.is_empty() {
                    rows.push(self.section_heading(words::SEC_HISTORY_PANEL, ""));
                }
            }
            "assistant" if own => rows.push(self.section_heading(words::SEC_ASSISTANT_PANEL, "")),
            _ => {}
        }
        rows.extend(fields.iter().map(|f| self.render_field_row(f, cx)));
        match section {
            "integrations" if own => rows.push(self.render_integrations(cx)),
            "automation" if own => rows.extend(self.render_hooks(cx)),
            "launcher" if own => rows.push(self.render_exemptions(cx)),
            "keymap" if own => {
                rows.push(self.render_keybindings(cx));
                rows.push(self.render_vim_keys());
            }
            "assistant" if own => rows.extend(self.render_assistant_daemon(cx)),
            "assistant" => rows.extend(self.render_daemon_page("assistant", filters, banner, cx)),
            "experimental" if own => rows.push(self.render_experimental_disclaimer(cx)),
            section if super::settings_daemon::is_daemon_page(section) && section != "history" => {
                rows.extend(self.render_daemon_page(section, filters, banner, cx));
            }
            _ => {}
        }
        rows
    }

    // ---- Experimental --------------------------------------------------------------------------

    /// The disclaimer under the Experimental toggles, with the way to the docs.
    fn render_experimental_disclaimer(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let docs = Button::new(
            "experimental-docs",
            words::EXPERIMENTAL_DOCS,
            &theme,
            &tokens,
        )
        .compact()
        .on_click(cx.listener(|_, _, _, cx| {
            cx.open_url(hutan_settings::experimental::REMOTE_HOSTS_DOCS_URL);
        }))
        .child(probe(&self.regions, Region::Settings("experimental-docs")));
        div()
            .id("experimental-disclaimer")
            .flex()
            .flex_col()
            .items_start()
            .gap(px(tokens.spacing))
            .py(px(tokens.spacing))
            .child(
                note(
                    hutan_settings::experimental::REMOTE_HOSTS_DISCLAIMER,
                    &theme,
                    &tokens,
                )
                .child(probe(
                    &self.regions,
                    Region::Settings("experimental-disclaimer"),
                )),
            )
            .child(docs)
            .into_any_element()
    }

    // ---- History -------------------------------------------------------------------------------

    /// What the sheet knows about the history index.
    pub fn history_index(&self) -> &HistoryIndex {
        &self.history_index
    }

    /// Reads `history.status`.
    pub fn refresh_history_index(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        let connection = store.connection().clone();
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        if !connection.supports_history() {
            self.history_index.unsupported = true;
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_status().await })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(info) => {
                        this.history_index.status = Some(info);
                        this.history_index.error = None;
                    }
                    Err(error) => this.history_index.error = Some(error.to_string()),
                }
                this.setup_adopt_history();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Turns indexing of session history on or off (`history.set_enabled`: the daemon records
    /// it in `daemon.toml`). Turning it off keeps the index file.
    pub fn set_history_indexing(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.history_index.busy {
            return;
        }
        let connection = self.connection(cx);
        self.history_index.busy = true;
        self.history_index.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_set_enabled(enabled).await })
                .await;
            this.update(cx, |this, cx| {
                this.history_index.busy = false;
                match result {
                    Ok(info) => this.history_index.status = Some(info),
                    Err(error) => {
                        this.history_index.error =
                            Some(words::history_toggle_failed(enabled, &error.to_string()));
                    }
                }
                this.refresh_history_panel(cx);
                this.refresh_daemon_settings(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The History panel, when it was opened, reads the status again.
    pub(crate) fn refresh_history_panel(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.panel_views.get(&PanelId::new("history")).cloned()
            && let Ok(panel) = view.downcast::<HistoryPanel>()
        {
            panel.update(cx, |panel, cx| panel.refresh_status(cx));
        }
    }

    fn render_history_indexing(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let index = self.history_index.clone();
        let connected = self.daemon_connected(cx);
        let on = index.status.as_ref().is_some_and(|s| s.enabled);
        let mut under = Vec::new();
        let summary = match (&index.status, index.unsupported, connected) {
            (_, true, _) => words::HISTORY_UNSUPPORTED.to_owned(),
            (_, _, false) => words::HISTORY_NO_DAEMON.to_owned(),
            (None, _, _) => words::HISTORY_ASKING.to_owned(),
            (Some(status), _, _) if !status.enabled => words::HISTORY_OFF.to_owned(),
            (Some(status), _, _) => {
                let sources: Vec<(String, u64)> = status
                    .sources
                    .iter()
                    .map(|s| (s.name.clone(), s.rows))
                    .collect();
                words::history_indexed(&sources, &format_bytes(status.db_bytes))
            }
        };
        under.push(note(summary, &theme, &tokens).into_any_element());
        if let Some(error) = &index.error {
            under.push(
                note(error.clone(), &theme, &tokens)
                    .text_color(theme.danger)
                    .into_any_element(),
            );
        }
        let usable = connected && !index.unsupported && index.status.is_some() && !index.busy;
        let toggle = Button::new(
            "toggle-history-index",
            if on { "On" } else { "Off" },
            &theme,
            &tokens,
        )
        .compact()
        .selected(on)
        .disabled(!usable)
        .on_click(cx.listener(move |this, _, _, cx| this.set_history_indexing(!on, cx)))
        .child(probe(&self.regions, Region::Settings("history-index")));
        sheet_row(
            "field-history-index",
            words::HISTORY_INDEX_TITLE,
            words::HISTORY_INDEX_HELP,
            under,
            toggle.into_any_element(),
            &theme,
            &tokens,
        )
    }

    // ---- Data & logs ---------------------------------------------------------------------------

    /// Where `target` is: the data folder is the one that holds `settings.toml` (and the
    /// daemon's own files), the logs are where the daemon logs.
    pub fn data_path(&self, target: DataTarget) -> Option<PathBuf> {
        let paths = self.settings.paths()?;
        let folder = paths.user.parent()?.to_path_buf();
        match target {
            DataTarget::Folder => Some(folder),
            DataTarget::Settings => Some(paths.user),
            DataTarget::Keymap => Some(paths.keymap),
            DataTarget::Daemon => Some(folder.join("daemon.toml")),
            DataTarget::Logs => self
                .daemon
                .log_path
                .as_ref()?
                .parent()
                .map(Path::to_path_buf),
        }
    }

    /// "Reveal in Finder": shows `target` in Finder through the reveal handler. Something that
    /// does not exist yet is said, not created.
    pub fn reveal_data(&mut self, target: DataTarget, cx: &mut Context<Self>) {
        match self.data_path(target) {
            Some(path) if path.exists() => (self.daemon.reveal.clone())(&path, cx),
            Some(path) => self.set_notice(words::not_there_yet(&path), cx),
            None => self.set_notice(words::CANNOT_LOCATE, cx),
        }
    }

    /// "Open": opens `target` with the system (an editor for the files, Finder for the
    /// folders). Hutan's own `settings.toml` and `keymap.toml` are created first when they do
    /// not exist, with a comment that says what they are; `daemon.toml` is the daemon's.
    pub fn open_data(&mut self, target: DataTarget, cx: &mut Context<Self>) {
        let Some(path) = self.data_path(target) else {
            self.set_notice(words::CANNOT_LOCATE, cx);
            return;
        };
        if !path.exists() {
            let Some(starter) = target.starter() else {
                self.set_notice(words::not_there_yet(&path), cx);
                return;
            };
            if let Err(error) = std::fs::write(&path, starter) {
                self.set_notice(words::cannot_create(&path, &error.to_string()), cx);
                return;
            }
        }
        (self.daemon.open.clone())(&path, cx);
    }

    /// The size of the daemon's database (`hutan.db` and its write-ahead log).
    pub fn database_bytes(&self) -> Option<u64> {
        let folder = self.data_path(DataTarget::Folder)?;
        let main = file_size(&folder.join("hutan.db"))?;
        let wal = file_size(&folder.join("hutan.db-wal")).unwrap_or(0);
        Some(main + wal)
    }

    pub(super) fn render_data_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let index = self.history_index.clone();
        let busy = self.daemon.busy.is_some();
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            note(words::DATA_INTRO, &theme, &tokens)
                .py(px(tokens.spacing * 2.0))
                .into_any_element(),
        );
        for target in DataTarget::ALL {
            let i = target.index();
            let path = self.data_path(target);
            let shown = path.as_ref().map_or_else(
                || words::CANNOT_LOCATE.to_owned(),
                |p| p.display().to_string(),
            );
            let reveal = Button::new(
                SharedString::from(format!("data-reveal-{i}")),
                words::REVEAL,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.reveal_data(target, cx)))
            .child(probe(&self.regions, Region::SettingsItem("reveal", i)));
            let open = Button::new(
                SharedString::from(format!("data-open-{i}")),
                words::OPEN,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open_data(target, cx)))
            .child(probe(&self.regions, Region::SettingsItem("open", i)));
            let (title, help) = words::data_target(i);
            rows.push(sheet_row(
                SharedString::from(format!("data-{i}")),
                title,
                help,
                vec![note(shown, &theme, &tokens).into_any_element()],
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(reveal)
                    .child(open)
                    .into_any_element(),
                &theme,
                &tokens,
            ));
        }

        // Sizes, from what the daemon reports and the file the daemon keeps.
        let database = self
            .database_bytes()
            .map_or_else(|| words::SIZE_UNKNOWN.to_owned(), format_bytes);
        let history = match (&index.status, index.unsupported) {
            (_, true) => words::HISTORY_UNSUPPORTED.to_owned(),
            (Some(status), _) if status.enabled => format_bytes(status.db_bytes),
            (Some(_), _) => words::HISTORY_OFF.to_owned(),
            (None, _) => words::SIZE_UNKNOWN.to_owned(),
        };
        let daemon = self.daemon_status().map_or_else(
            || words::DAEMON_NOT_RUNNING.to_owned(),
            |info| {
                words::daemon_summary(
                    &info.daemon_version,
                    info.pid,
                    info.uptime_ms,
                    info.sessions,
                )
            },
        );
        rows.push(sheet_row(
            "data-sizes",
            words::SIZES_TITLE,
            words::SIZES_HELP,
            vec![
                note(
                    words::size_line(words::DATABASE_NAME, &database),
                    &theme,
                    &tokens,
                )
                .into_any_element(),
                note(
                    words::size_line(words::HISTORY_NAME, &history),
                    &theme,
                    &tokens,
                )
                .into_any_element(),
                note(daemon, &theme, &tokens).into_any_element(),
            ],
            div().into_any_element(),
            &theme,
            &tokens,
        ));

        // The daemon's lifetime: the Daemon menu's flows, with their confirmations.
        let restart = Button::new(
            "data-restart-daemon",
            words::RESTART_DAEMON,
            &theme,
            &tokens,
        )
        .disabled(busy)
        .when(!busy, |s| {
            s.on_click(cx.listener(|this, _, window, cx| this.restart_daemon(window, cx)))
        })
        .child(probe(&self.regions, Region::Settings("restart-daemon")));
        let stop = Button::new("data-stop-daemon", words::STOP_DAEMON, &theme, &tokens)
            .disabled(busy)
            .when(!busy, |s| {
                s.on_click(cx.listener(|this, _, window, cx| this.stop_daemon(window, cx)))
            })
            .child(probe(&self.regions, Region::Settings("stop-daemon")));
        rows.push(sheet_row(
            "data-daemon",
            words::DAEMON_TITLE,
            words::DAEMON_HELP,
            Vec::new(),
            div()
                .flex()
                .gap(px(tokens.spacing * 2.0))
                .child(restart)
                .child(stop)
                .into_any_element(),
            &theme,
            &tokens,
        ));
        div().flex().flex_col().children(rows).into_any_element()
    }

    // ---- Agent Integrations --------------------------------------------------------------------

    fn render_integrations(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let available = self.integrations_available();
        let busy = self.integration_busy();
        let mut rows: Vec<AnyElement> = vec![
            note(words::INTEGRATIONS_NOTE, &theme, &tokens)
                .py(px(tokens.spacing * 2.0))
                .into_any_element(),
        ];
        if !available {
            rows.push(note(words::INTEGRATIONS_UNAVAILABLE, &theme, &tokens).into_any_element());
        }
        for (index, harness) in HARNESSES.iter().enumerate() {
            let id = harness.id;
            let status = self.integration_status(id);
            let detection = self.env.harnesses.read(cx).detection(id);
            let (status_text, status_color) = match status {
                None => (words::HOOKS_CHECKING, theme.muted),
                Some(IntegrationStatus::Installed) => (words::HOOKS_INSTALLED, theme.success),
                Some(IntegrationStatus::Partial) => (words::HOOKS_OUTDATED, theme.warning),
                Some(IntegrationStatus::NotInstalled) => (words::HOOKS_NOT_INSTALLED, theme.muted),
            };
            let (_, place) = hook_files(id);
            let mut under = vec![
                note(status_text, &theme, &tokens)
                    .text_color(status_color)
                    .into_any_element(),
                note(words::hook_place(place), &theme, &tokens).into_any_element(),
            ];
            if matches!(detection, Detection::NotFound(_)) {
                under.push(note(words::AGENT_NOT_FOUND, &theme, &tokens).into_any_element());
            }
            let mut buttons = div().flex().gap(px(tokens.spacing * 2.0));
            let usable = available && !busy;
            let button = |region: &'static str,
                          label: &'static str,
                          run: fn(
                &mut Workbench,
                &'static str,
                &mut Window,
                &mut Context<Workbench>,
            )| {
                Button::new(
                    SharedString::from(format!("hooks-{region}-{id}")),
                    label,
                    &theme,
                    &tokens,
                )
                .disabled(!usable)
                .when(region == "uninstall", |b| b.destructive())
                .when(usable, |s| {
                    s.on_click(cx.listener(move |this, _, window, cx| run(this, id, window, cx)))
                })
                .child(probe(
                    &self.regions,
                    Region::SettingsItem(
                        match region {
                            "install" => "hooks-install",
                            "repair" => "hooks-repair",
                            _ => "hooks-uninstall",
                        },
                        index,
                    ),
                ))
            };
            match status {
                Some(IntegrationStatus::NotInstalled) => {
                    buttons = buttons.child(button(
                        "install",
                        words::INSTALL_HOOKS,
                        Self::confirm_install_hooks,
                    ));
                }
                Some(IntegrationStatus::Partial) => {
                    buttons = buttons
                        .child(button(
                            "repair",
                            words::REPAIR_HOOKS,
                            Self::confirm_install_hooks,
                        ))
                        .child(button(
                            "uninstall",
                            words::UNINSTALL_HOOKS,
                            Self::confirm_uninstall_hooks,
                        ));
                }
                Some(IntegrationStatus::Installed) => {
                    buttons = buttons.child(button(
                        "uninstall",
                        words::UNINSTALL_HOOKS,
                        Self::confirm_uninstall_hooks,
                    ));
                }
                None => {}
            }
            rows.push(sheet_row(
                SharedString::from(format!("hooks-{id}")),
                harness.name,
                words::hooks_help(harness.name),
                under,
                buttons.into_any_element(),
                &theme,
                &tokens,
            ));
        }
        div().flex().flex_col().children(rows).into_any_element()
    }

    /// Install or Repair: asks first, naming what is written and where.
    pub fn confirm_install_hooks(
        &mut self,
        harness: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repair = self.integration_status(harness) == Some(IntegrationStatus::Partial);
        let (writes, place) = hook_files(harness);
        let name = harness_name(harness);
        self.open_confirm(
            words::install_title(&name, repair),
            words::install_message(writes, place),
            if repair {
                words::REPAIR_HOOKS_CONFIRM
            } else {
                words::INSTALL_HOOKS_CONFIRM
            },
            move |this, _, cx| this.install_integration(harness, cx),
            window,
            cx,
        );
    }

    /// Uninstall: asks first, naming what is removed and from where.
    pub fn confirm_uninstall_hooks(
        &mut self,
        harness: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (_, place) = hook_files(harness);
        let name = harness_name(harness);
        self.open_destructive(
            words::uninstall_title(&name),
            words::uninstall_message(place),
            words::UNINSTALL_HOOKS_CONFIRM,
            move |this, _, cx| this.uninstall_integration(harness, cx),
            window,
            cx,
        );
    }

    // ---- Start Sheet: remembered choices -------------------------------------------------------

    /// The "Don't ask again" choices remembered for agent chats, `<agent>/<model>`, sorted.
    pub fn start_exemptions(&self) -> Vec<String> {
        self.launcher_memory.confirmed.iter().cloned().collect()
    }

    /// An exemption as a person reads it: "OMP · gpt-5" or "a provider · default model".
    pub fn exemption_label(&self, key: &str) -> String {
        let (agent, model) = key.split_once('/').unwrap_or((key, "default"));
        let agent = match agent.strip_prefix("direct:") {
            Some(provider) => self
                .providers
                .providers
                .iter()
                .find(|p| p.id == provider)
                .map_or_else(|| provider.to_owned(), |p| p.name.clone()),
            None => harness_name(agent),
        };
        words::exemption(&agent, model)
    }

    /// Asks again before an agent chat with this agent and model sends its prompt.
    pub fn revoke_start_exemption(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.launcher_memory.confirmed.remove(key) {
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// Forgets every remembered choice.
    pub fn revoke_all_start_exemptions(&mut self, cx: &mut Context<Self>) {
        if !self.launcher_memory.confirmed.is_empty() {
            self.launcher_memory.confirmed.clear();
            self.schedule_save(cx);
            cx.notify();
        }
    }

    /// The click on a toggle of the sheet. Turning "Ask before an agent chat sends its prompt"
    /// back on while choices are remembered asks whether to forget them too, since they would
    /// keep the question from being asked.
    pub fn sheet_toggle_in(&mut self, field: &str, window: &mut Window, cx: &mut Context<Self>) {
        let turning_on = self.sheet_field(field).is_some_and(|(ns, _, value)| {
            (ns, field) == ("launcher", "confirm_agent_start") && value.as_bool() == Some(false)
        });
        let remembered = self.launcher_memory.confirmed.len();
        if !turning_on || remembered == 0 {
            self.sheet_toggle(field, cx);
            return;
        }
        let turn_on = |this: &mut Workbench, cx: &mut Context<Workbench>| {
            if let Err(err) = this
                .settings
                .write_user("launcher.confirm_agent_start", toml::Value::Boolean(true))
            {
                this.set_notice(format!("Cannot save the setting: {err}"), cx);
            }
        };
        self.open_modal(
            Modal {
                title: words::CONFIRM_ON_TITLE.into(),
                message: words::confirm_on_message(remembered).into(),
                rows: Vec::new(),
                confirm_label: words::CONFIRM_ON_FORGET.into(),
                input: None,
                secure_input: false,
                destructive: false,
                choice: Some(ModalChoice {
                    destructive: false,
                    label: words::CONFIRM_ON_KEEP.into(),
                    run: Rc::new(move |this, _, _, cx| turn_on(this, cx)),
                }),
                checkbox: None,
                on_confirm: Rc::new(move |this, _, _, cx| {
                    turn_on(this, cx);
                    this.revoke_all_start_exemptions(cx);
                }),
            },
            window,
            cx,
        );
    }

    fn render_exemptions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let keys = self.start_exemptions();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, key) in keys.iter().enumerate() {
            let k = key.clone();
            let revoke = Button::new(
                SharedString::from(format!("revoke-{index}")),
                words::REVOKE,
                &theme,
                &tokens,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.revoke_start_exemption(&k, cx)))
            .child(probe(&self.regions, Region::SettingsItem("revoke", index)));
            rows.push(
                div()
                    .id(SharedString::from(format!("exemption-{index}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing))
                    .child(div().child(SharedString::from(self.exemption_label(key))))
                    .child(revoke)
                    .into_any_element(),
            );
        }
        let all = Button::new("revoke-all", words::REVOKE_ALL, &theme, &tokens)
            .disabled(keys.is_empty())
            .when(!keys.is_empty(), |s| {
                s.on_click(cx.listener(|this, _, _, cx| this.revoke_all_start_exemptions(cx)))
            })
            .child(probe(&self.regions, Region::Settings("revoke-all")));
        let list = if keys.is_empty() {
            vec![note(words::EXEMPTIONS_EMPTY, &theme, &tokens).into_any_element()]
        } else {
            rows
        };
        sheet_row(
            "field-exemptions",
            words::EXEMPTIONS_TITLE,
            words::EXEMPTIONS_HELP,
            list,
            all.into_any_element(),
            &theme,
            &tokens,
        )
    }

    // ---- Keyboard: keymap.toml -----------------------------------------------------------------

    /// The bindings `keymap.toml` adds: what the file holds, not the built-in ones.
    pub fn custom_bindings(&self) -> Vec<BindingSpec> {
        self.settings.keymap(&[]).bindings
    }

    /// Reads `keymap.toml` and the other settings files again and rebinds the keys.
    pub fn reload_keymap(&mut self, cx: &mut Context<Self>) {
        self.settings.reload();
        self.rebind_keys(cx);
    }

    fn render_keybindings(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let query = self
            .sheet
            .as_ref()
            .map(|s| s.query().trim().to_lowercase())
            .unwrap_or_default();
        let all = self.custom_bindings();
        // A search narrows the list to what it finds there; one that names the page (and no
        // binding) leaves the list whole.
        let matching: Vec<&BindingSpec> = all
            .iter()
            .filter(|b| !query.is_empty() && binding_text(b).to_lowercase().contains(&query))
            .collect();
        let shown: Vec<&BindingSpec> = if matching.is_empty() {
            all.iter().collect()
        } else {
            matching
        };
        let mut under: Vec<AnyElement> = Vec::new();
        if shown.is_empty() {
            under.push(note(words::BINDINGS_EMPTY, &theme, &tokens).into_any_element());
        }
        for (index, binding) in shown.iter().enumerate() {
            under.push(
                div()
                    .id(SharedString::from(format!("binding-{index}")))
                    .flex()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(2.0))
                    .child(
                        div()
                            .w(px(130.0))
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(format_keys(&binding.keys))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(SharedString::from(binding.action.clone())),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .flex_none()
                            .text_color(theme.muted)
                            .child(SharedString::from(
                                binding
                                    .context
                                    .clone()
                                    .unwrap_or_else(|| words::BINDING_EVERYWHERE.to_owned()),
                            )),
                    )
                    .into_any_element(),
            );
        }
        let open = Button::new("keymap-open", words::OPEN_KEYMAP, &theme, &tokens)
            .on_click(cx.listener(|this, _, _, cx| this.open_data(DataTarget::Keymap, cx)))
            .child(probe(&self.regions, Region::Settings("open-keymap")));
        let reload = Button::new("keymap-reload", words::RELOAD, &theme, &tokens)
            .on_click(cx.listener(|this, _, _, cx| this.reload_keymap(cx)))
            .child(probe(&self.regions, Region::Settings("reload-keymap")));
        sheet_row(
            "field-bindings",
            words::BINDINGS_TITLE,
            words::BINDINGS_HELP,
            under,
            div()
                .flex()
                .gap(px(tokens.spacing * 2.0))
                .child(open)
                .child(reload)
                .into_any_element(),
            &theme,
            &tokens,
        )
    }

    // ---- Keyboard: Vim's keys and the user's mappings ------------------------------------------

    /// The Vim keys of the Keyboard page: the mappings of `editor.vim_mappings` (one that
    /// cannot be used says why), then Vim's own control keys.
    pub fn vim_key_rows(&self) -> Vec<VimKeyRow> {
        let editor = self.settings.get::<EditorSettings>();
        let (good, problems) = super::vim_keys::compile(&editor.vim_mappings);
        let mut rows: Vec<VimKeyRow> = good
            .iter()
            .map(|m| VimKeyRow {
                keys: m.from_text.clone(),
                modes: m.mode.label().to_owned(),
                what: format!("→ {}", m.to_text),
                mapping: true,
                problem: false,
            })
            .collect();
        rows.extend(problems.into_iter().map(|p| VimKeyRow {
            keys: String::new(),
            modes: p.mode.label().to_owned(),
            what: format!("{} {}", words::VIM_MAPPING_BAD, p.message),
            mapping: true,
            problem: true,
        }));
        rows.extend(super::vim_keys::BUILTIN_KEYS.iter().map(|k| VimKeyRow {
            keys: k.keys.to_owned(),
            modes: k.modes.to_owned(),
            what: k.what.to_owned(),
            mapping: false,
            problem: false,
        }));
        rows
    }

    /// The Vim keys the page shows: the ones the sheet's search finds among them, else all.
    pub fn shown_vim_key_rows(&self) -> Vec<VimKeyRow> {
        let query = self
            .sheet
            .as_ref()
            .map(|s| s.query().trim().to_lowercase())
            .unwrap_or_default();
        let rows = self.vim_key_rows();
        let matching: Vec<VimKeyRow> = rows
            .iter()
            .filter(|r| !query.is_empty() && r.text().to_lowercase().contains(&query))
            .cloned()
            .collect();
        if matching.is_empty() { rows } else { matching }
    }

    fn render_vim_keys(&self) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let rows = self.shown_vim_key_rows();
        let mut under: Vec<AnyElement> = Vec::new();
        if !self.vim_config().enabled {
            under.push(note(words::VIM_KEYS_OFF, &theme, &tokens).into_any_element());
        }
        if !rows.iter().any(|r| r.mapping) {
            under.push(note(words::VIM_MAPPINGS_EMPTY, &theme, &tokens).into_any_element());
        }
        let mut builtin_heading = false;
        for (index, row) in rows.iter().enumerate() {
            if !row.mapping && !builtin_heading {
                builtin_heading = true;
                under.push(
                    div()
                        .pt(px(tokens.spacing * 2.0))
                        .child(ui::section_label(words::VIM_BUILTIN, &theme, &tokens))
                        .into_any_element(),
                );
            }
            under.push(
                div()
                    .id(SharedString::from(format!("vim-key-{index}")))
                    .flex()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(2.0))
                    .when(row.problem, |s| s.text_color(theme.danger))
                    .child(
                        div()
                            .w(px(130.0))
                            .flex_none()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(row.keys.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(SharedString::from(row.what.clone())),
                    )
                    .child(
                        div()
                            .w(px(130.0))
                            .flex_none()
                            .text_color(theme.muted)
                            .child(SharedString::from(row.modes.clone())),
                    )
                    .into_any_element(),
            );
        }
        SheetRow::new(
            "field-vim-keys",
            words::VIM_KEYS_TITLE,
            words::VIM_KEYS_HELP,
            div().into_any_element(),
        )
        .under(under)
        .under_full()
        .build(&theme, &tokens)
    }

    // ---- terminal.env: rows of variables -------------------------------------------------------

    pub(super) fn render_map_control(
        &self,
        field: &Field,
        value: &Json,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = field.dotted();
        let rows: BTreeMap<String, String> = value
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let editing = sheet.edit.clone().filter(|e| e.field == key);
        let layer = self.edit_layer_of(field);
        let mut lines: Vec<AnyElement> = Vec::new();
        for (index, (name, text)) in rows.iter().enumerate() {
            let this_row = editing
                .as_ref()
                .is_some_and(|e| e.entry.as_deref() == Some(name));
            let (k_edit, n_edit) = (key.clone(), name.clone());
            let (k_remove, n_remove) = (key.clone(), name.clone());
            let value_box = if this_row {
                div()
                    .flex()
                    .items_center()
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.control_height - 2.0))
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(if editing.as_ref().is_some_and(|e| e.error.is_some()) {
                        theme.danger
                    } else {
                        theme.border
                    })
                    .focus_ring(true, &theme, &tokens)
                    .min_w(px(120.0))
                    .children(self.overlay_parts(OverlayField::SheetValue, "", cx))
                    .into_any_element()
            } else {
                div()
                    .id(SharedString::from(format!("env-value-{index}")))
                    .cursor_text()
                    .px(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .hover(move |s| s.bg(theme.surface_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet_begin_edit_entry(&k_edit, &n_edit, cx);
                    }))
                    .child(SharedString::from(if text.is_empty() {
                        words::ENV_EMPTY_VALUE.to_owned()
                    } else {
                        text.clone()
                    }))
                    .into_any_element()
            };
            // A variable that another layer sets cannot be removed from this one.
            let elsewhere = self
                .settings
                .layer_value(layer, &format!("{}.{name}", field.key_path()))
                .is_none();
            lines.push(
                div()
                    .id(SharedString::from(format!("env-row-{index}")))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(SharedString::from(format!("{name} ="))),
                    )
                    .child(value_box)
                    .child(
                        IconButton::new(
                            SharedString::from(format!("env-remove-{index}")),
                            crate::icons::Icon::ActionClose,
                            "Remove",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .disabled(elsewhere)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sheet_map_remove(&k_remove, &n_remove, cx);
                        }))
                        .child(probe(
                            &self.regions,
                            Region::SettingsItem("env-remove", index),
                        )),
                    )
                    .into_any_element(),
            );
        }
        let adding = editing.as_ref().filter(|e| e.list_add);
        let add: AnyElement = match adding {
            Some(edit) => div()
                .flex()
                .items_center()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(if edit.error.is_some() {
                    theme.danger
                } else {
                    theme.border
                })
                .focus_ring(true, &theme, &tokens)
                .min_w(px(180.0))
                .children(self.overlay_parts(OverlayField::SheetValue, words::ENV_ADD_HINT, cx))
                .into_any_element(),
            None => {
                let k = key.clone();
                Button::new(
                    SharedString::from(format!("add-{key}")),
                    words::ENV_ADD,
                    &theme,
                    &tokens,
                )
                .compact()
                .on_click(cx.listener(move |this, _, _, cx| this.sheet_begin_edit(&k, cx)))
                .child(probe(&self.regions, Region::Settings("env-add")))
                .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(360.0))
            .when(rows.is_empty(), |s| {
                s.child(note(words::ENV_NONE, &theme, &tokens))
            })
            .children(lines)
            .child(add)
            .when_some(editing.and_then(|e| e.error), |s, error| {
                s.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }

    // ---- layout.presets ------------------------------------------------------------------------

    /// The presets saved in the user's file, by name.
    pub fn saved_layout_presets(&self) -> Vec<String> {
        self.settings
            .get::<LayoutSettings>()
            .presets
            .keys()
            .cloned()
            .collect()
    }

    /// Renames the saved preset `from` to `to` (a name of letters, digits, `-` and `_`). The
    /// layout in use follows when it is the one renamed.
    pub fn rename_layout_preset(&mut self, from: &str, to: &str, cx: &mut Context<Self>) -> bool {
        let to = preset_name(to);
        let saved = self.saved_layout_presets();
        let problem = if to.is_empty() {
            Some(words::PRESET_NEEDS_NAME.to_owned())
        } else if to == from {
            return true;
        } else if saved.contains(&to) {
            Some(words::preset_exists(&to))
        } else {
            None
        };
        if let Some(problem) = problem {
            self.set_notice(problem, cx);
            return false;
        }
        let Some(value) = self
            .settings
            .layer_value(SettingsLayer::User, &format!("layout.presets.{from}"))
        else {
            self.set_notice(words::preset_missing(from), cx);
            return false;
        };
        let was_current = self.settings.get::<LayoutSettings>().preset == from;
        let result = self
            .settings
            .write_user(&format!("layout.presets.{to}"), value)
            .and_then(|()| {
                if was_current {
                    self.settings
                        .write_user("layout.preset", toml::Value::String(to.clone()))
                } else {
                    Ok(())
                }
            })
            .and_then(|()| {
                self.settings
                    .remove(SettingsLayer::User, "layout", &format!("presets.{from}"))
                    .map(drop)
            });
        match result {
            Ok(()) => {
                if self.state.preset == from {
                    self.state.preset.clone_from(&to);
                }
                self.set_notice(words::preset_renamed(from, &to), cx);
                true
            }
            Err(err) => {
                self.set_notice(format!("Cannot rename the preset: {err}"), cx);
                false
            }
        }
    }

    /// Rename…: asks for the new name.
    pub fn ask_rename_layout_preset(
        &mut self,
        from: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let from = from.to_owned();
        let initial = from.clone();
        self.open_prompt(
            super::modal::PromptSpec {
                title: words::PRESET_RENAME_TITLE,
                message: words::PRESET_NAME_PROMPT,
                initial: &initial,
                confirm_label: words::PRESET_RENAME_CONFIRM,
            },
            move |this, name, _window, cx| {
                this.rename_layout_preset(&from, &name, cx);
            },
            window,
            cx,
        );
    }

    /// Removes the saved preset `name` from the user's file. The layout in use cannot go.
    pub fn delete_layout_preset(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        if self.state.preset == name || self.settings.get::<LayoutSettings>().preset == name {
            self.set_notice(words::PRESET_IN_USE, cx);
            return false;
        }
        match self
            .settings
            .remove(SettingsLayer::User, "layout", &format!("presets.{name}"))
        {
            Ok(true) => {
                self.set_notice(words::preset_deleted(name), cx);
                true
            }
            Ok(false) => {
                self.set_notice(words::preset_missing(name), cx);
                false
            }
            Err(err) => {
                self.set_notice(format!("Cannot delete the preset: {err}"), cx);
                false
            }
        }
    }

    /// Delete…: asks first.
    pub fn ask_delete_layout_preset(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.to_owned();
        let builtin = LayoutPreset::builtin().iter().any(|p| p.name == name);
        self.open_destructive(
            words::preset_delete_title(&name, builtin),
            words::preset_delete_message(builtin),
            if builtin {
                words::PRESET_RESTORE
            } else {
                words::PRESET_DELETE_CONFIRM
            },
            move |this, _, cx| {
                this.delete_layout_preset(&name, cx);
            },
            window,
            cx,
        );
    }

    pub(super) fn render_presets_control(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let layout = self.settings.get::<LayoutSettings>();
        let builtin: Vec<String> = LayoutPreset::builtin()
            .into_iter()
            .map(|p| p.name)
            .collect();
        let mut names: Vec<String> = builtin.clone();
        names.extend(
            layout
                .presets
                .keys()
                .filter(|n| !builtin.contains(n))
                .cloned(),
        );
        let mut lines: Vec<AnyElement> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let saved = layout.presets.contains_key(name);
            let in_use = self.state.preset == *name;
            let editable = saved && preset_name(name) == *name;
            let tag = match (saved, builtin.contains(name)) {
                (false, _) => words::PRESET_BUILTIN,
                (true, true) => words::PRESET_REPLACED,
                (true, false) => words::PRESET_SAVED,
            };
            let mut buttons = div().flex().items_center().gap(px(tokens.spacing * 2.0));
            if in_use {
                buttons = buttons
                    .child(note(words::PRESET_CURRENT, &theme, &tokens).text_color(theme.accent));
            }
            if editable && !builtin.contains(name) {
                let n = name.clone();
                buttons = buttons.child(
                    Button::new(
                        SharedString::from(format!("preset-rename-{index}")),
                        words::PRESET_RENAME,
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_rename_layout_preset(&n, window, cx);
                    }))
                    .child(probe(
                        &self.regions,
                        Region::SettingsItem("preset-rename", index),
                    )),
                );
            }
            if editable && !in_use {
                let n = name.clone();
                buttons = buttons.child(
                    Button::new(
                        SharedString::from(format!("preset-delete-{index}")),
                        if builtin.contains(name) {
                            words::PRESET_RESTORE_SHORT
                        } else {
                            words::PRESET_DELETE
                        },
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .destructive()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_delete_layout_preset(&n, window, cx);
                    }))
                    .child(probe(
                        &self.regions,
                        Region::SettingsItem("preset-delete", index),
                    )),
                );
            }
            lines.push(
                div()
                    .id(SharedString::from(format!("preset-{index}")))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(tokens.spacing * 3.0))
                    .child(div().child(SharedString::from(name.clone())))
                    .child(note(tag, &theme, &tokens))
                    .child(buttons)
                    .into_any_element(),
            );
        }
        let save = Button::new("preset-save", words::PRESET_SAVE, &theme, &tokens)
            .compact()
            .on_click(cx.listener(|this, _, window, cx| this.save_layout_as_preset(window, cx)))
            .child(probe(&self.regions, Region::Settings("preset-save")));
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(420.0))
            .children(lines)
            .child(save)
            .into_any_element()
    }
}

/// Everything a search can match in a binding: its keys, its action and its context.
fn binding_text(binding: &BindingSpec) -> String {
    format!(
        "{} {} {}",
        format_keys(&binding.keys),
        binding.action,
        binding.context.as_deref().unwrap_or_default()
    )
}

/// One line of the Keyboard page's Vim keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VimKeyRow {
    /// As written (`jk`, `⌃W s`); empty for a mapping that cannot be used.
    pub keys: String,
    /// Where it applies (`Insert`, `Normal (file tabs)`).
    pub modes: String,
    /// What it does (`→ <Esc>`), or why a mapping is not used.
    pub what: String,
    /// One of `editor.vim_mappings` rather than Vim's own.
    pub mapping: bool,
    pub problem: bool,
}

impl VimKeyRow {
    /// Everything a search can match in it, `vim` included.
    fn text(&self) -> String {
        format!("vim {} {} {}", self.keys, self.what, self.modes)
    }
}
