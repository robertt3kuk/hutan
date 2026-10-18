//! The daemon's own settings in the sheet (`daemon.toml`, protocol 0.28, capability
//! `daemon.settings`).
//!
//! The daemon owns `daemon.toml`: `daemon.settings.get` lists every option with its value, its
//! default and whether a change applies at once or after a restart, and `daemon.settings.set`
//! validates, writes the file (comments and order kept) and answers the list again. The pages
//! of the sheet that show these options (Notifications, Sessions & restore, Agent control &
//! security, Agents & tools, and sections of History and Assistant) are drawn from that list
//! with the same controls as the settings of the app: a toggle, a range-checked number, a
//! choice, a path with "Choose…", a list, a table of variables, and the MCP servers (a row with
//! Edit and Remove each, and a form to add one: `settings_mcp.rs`).
//!
//! The window keeps nothing but the last answer. A change is sent and the answer replaces the
//! list; a refusal (`bad_request`) shows under the field, a `daemon.toml` the daemon cannot
//! read (`invalid_state`) as a banner with "Open daemon.toml". While some option's file value
//! is not what the running daemon uses, the sheet says so at its top with "Restart now".

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, PathPromptOptions,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::Epoch;
use hutan_protocol::control::ErrorCode;
use hutan_protocol::daemon_settings::{Applies, DaemonSetting, DaemonSettingKind, DaemonSettings};
use serde_json::{Value as Json, json};

use super::render::probe;
use super::settings_labels::{
    daemon_field_help, daemon_field_search_text, daemon_field_title, daemon_value_label, words,
};
use super::settings_mcp::McpForm;
use super::settings_pages::{DataTarget, valid_env_name};
use super::settings_rows::{note, sheet_row, small_button};
use super::settings_sheet::{Edit, display_value};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::theme::{DensityExt as _, Theme, Tokens};

/// The prefix of the `Edit.field` of a text box that edits a daemon setting.
pub(super) const EDIT_PREFIX: &str = "daemon:";

/// A part of a page: a heading and the daemon settings under it. `mcp.*` stands for every MCP
/// server the file defines.
pub(super) struct Section {
    pub title: &'static str,
    pub help: &'static str,
    pub keys: &'static [&'static str],
}

pub(super) const MCP_KEYS: &str = "mcp.*";

/// The sections of a page that shows daemon settings; empty for the other pages.
pub(super) fn sections(page: &str) -> &'static [Section] {
    match page {
        "notifications" => &[Section {
            title: "",
            help: "",
            keys: &[
                "notify.enabled",
                "notify.native",
                "notify.kinds",
                "notify.min_turn_secs",
                "notify.expire_secs",
            ],
        }],
        "sessions_restore" => &[Section {
            title: "",
            help: "",
            keys: &[
                "daemon.restore_sessions",
                "workspaces.home_folder",
                "terminal.env_inherit",
                "projects.auto",
            ],
        }],
        "control" => &[Section {
            title: "",
            help: "",
            keys: &[
                "control.agents",
                "control.mcp",
                "control.approval_timeout_secs",
            ],
        }],
        "history" => &[
            Section {
                title: words::SEC_HISTORY,
                help: "",
                keys: &[
                    "history.hutan",
                    "history.claude",
                    "history.codex",
                    "history.omp",
                    "history.range_days",
                ],
            },
            Section {
                title: words::SEC_HISTORY_FOLDERS,
                help: "",
                keys: &[
                    "history.roots.claude",
                    "history.roots.codex",
                    "history.roots.omp",
                ],
            },
        ],
        "agents_tools" => &[
            Section {
                title: words::SEC_OMP,
                help: "",
                keys: &[
                    "harness.omp.bin",
                    "harness.omp.args",
                    "harness.omp.env",
                    "harness.omp.cli",
                ],
            },
            Section {
                title: words::SEC_CLAUDE,
                help: "",
                keys: &[
                    "harness.claude.bin",
                    "harness.claude.args",
                    "harness.claude.env",
                    "harness.claude.cli",
                ],
            },
            Section {
                title: words::SEC_CODEX,
                help: "",
                keys: &["codex.bin", "codex.args"],
            },
            Section {
                title: words::SEC_MCP,
                help: words::MCP_HELP,
                keys: &[MCP_KEYS],
            },
            Section {
                title: words::SEC_LIMITS,
                help: "",
                keys: &["gateway.max_context_chars", "tasks.max_parallel"],
            },
        ],
        "assistant" => &[
            Section {
                title: words::SEC_ASSISTANT_DEFAULT,
                help: "",
                keys: &["assistant.backend", "assistant.provider", "assistant.model"],
            },
            Section {
                title: words::SEC_ASSISTANT_PROJECTS,
                help: "",
                keys: &[
                    "assistant.context.shell",
                    "assistant.context.agents",
                    "assistant.context.files",
                    "assistant.context.search",
                ],
            },
            Section {
                title: words::SEC_ASSISTANT_HOME,
                help: "",
                keys: &[
                    "assistant.global_context.shell",
                    "assistant.global_context.agents",
                    "assistant.global_context.files",
                    "assistant.global_context.search",
                ],
            },
            Section {
                title: words::SEC_ASSISTANT_LIMITS,
                help: "",
                keys: &["assistant.context_budget_chars", "assistant.idle_stop_secs"],
            },
        ],
        _ => &[],
    }
}

/// Whether the page shows daemon settings.
pub(super) fn is_daemon_page(page: &str) -> bool {
    !sections(page).is_empty()
}

/// The keys the sheet's search matches on `page`: their wording, and the headings.
pub(super) fn search_text(page: &str) -> String {
    let mut text = String::new();
    for section in sections(page) {
        text.push_str(section.title);
        text.push(' ');
        for key in section.keys.iter().filter(|k| **k != MCP_KEYS) {
            text.push_str(&daemon_field_search_text(key));
            text.push(' ');
        }
    }
    text
}

/// What the window knows about `daemon.toml`.
#[derive(Debug, Default)]
pub struct DaemonSettingsState {
    settings: Option<DaemonSettings>,
    /// The connected daemon predates protocol 0.28.
    unsupported: bool,
    /// The daemon cannot read `daemon.toml`: its message, with the file, line and column.
    problem: Option<String>,
    /// What the daemon (or the text box) refused, by key.
    pub(super) errors: BTreeMap<String, String>,
    /// The form of an MCP server that is being added or edited (`settings_mcp.rs`).
    pub(super) mcp_form: Option<McpForm>,
    requested: u64,
    applied: u64,
    /// The daemon run the list was asked of.
    epoch: Option<Epoch>,
}

impl DaemonSettingsState {
    /// The last answer; `None` before the daemon was asked, or while `daemon.toml` is broken
    /// and nothing was known before.
    pub fn settings(&self) -> Option<&DaemonSettings> {
        self.settings.as_ref()
    }

    pub fn entry(&self, key: &str) -> Option<&DaemonSetting> {
        self.settings
            .as_ref()?
            .entries
            .iter()
            .find(|e| e.key == key)
    }

    /// The value of `key` as the file says it (else its default).
    pub fn value(&self, key: &str) -> Option<&Json> {
        self.entry(key).map(|e| &e.value)
    }

    /// The connected daemon has no `daemon.settings`.
    pub fn unsupported(&self) -> bool {
        self.unsupported
    }

    /// Why `daemon.toml` cannot be used, in the daemon's words.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Why the last change of `key` was refused.
    pub fn error(&self, key: &str) -> Option<&str> {
        self.errors.get(key).map(String::as_str)
    }

    /// Some option's file value is not what the running daemon uses.
    pub fn restart_pending(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.restart_pending)
    }

    fn next_request(&mut self) -> u64 {
        self.requested += 1;
        self.requested
    }
}

/// What the daemon answered, in the words of the sheet.
fn refusal(error: &ClientError) -> String {
    match error {
        ClientError::Remote(body) => body.message.clone(),
        other => other.to_string(),
    }
}

/// The values a choice offers: `(value as text, label)`.
pub(super) fn choices_of(e: &DaemonSetting) -> Option<Vec<(String, String)>> {
    let key = e.key.as_str();
    let label = |value: &str| daemon_value_label(key, value);
    match &e.kind {
        DaemonSettingKind::Enum { choices } => {
            let mut all: Vec<(String, String)> = Vec::new();
            // A setting whose default is empty has no choice made: "Automatic".
            if e.default == json!("") {
                all.push((String::new(), words::AUTOMATIC.to_owned()));
            }
            all.extend(choices.iter().map(|c| (c.clone(), label(c))));
            Some(all)
        }
        DaemonSettingKind::Int { .. } if key == "history.range_days" => Some(
            ["0", "30", "90"]
                .into_iter()
                .map(|c| (c.to_owned(), label(c)))
                .collect(),
        ),
        _ => None,
    }
}

/// `value` of a choice as text (`"ask"`, `30`).
pub(super) fn choice_text(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn strings_of(value: &Json) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn heading(title: &str, help: &str, theme: &Theme, tokens: &Tokens) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .pt(px(tokens.spacing * 4.0))
        .pb(px(tokens.spacing))
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(SharedString::from(title.to_owned())),
        )
        .when(!help.is_empty(), |s| {
            s.child(note(help.to_owned(), theme, tokens))
        })
        .into_any_element()
}

impl Workbench {
    // ---- reading -------------------------------------------------------------------------

    /// What the window knows about `daemon.toml`.
    pub fn daemon_settings(&self) -> &DaemonSettingsState {
        &self.daemon_settings
    }

    /// Where `daemon.toml` is: what the daemon said, else next to `settings.toml`.
    pub fn daemon_toml_path(&self) -> Option<PathBuf> {
        match self.daemon_settings.settings() {
            Some(settings) => Some(PathBuf::from(&settings.file)),
            None => self.data_path(DataTarget::Daemon),
        }
    }

    // ---- asking the daemon ---------------------------------------------------------------

    /// `daemon.settings.get`. A daemon without the capability is remembered as such; a
    /// daemon that is not connected is left alone (the last answer stays).
    pub fn refresh_daemon_settings(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        let connection = store.connection().clone();
        self.daemon_settings.epoch = store.state().epoch;
        if !connection.supports_daemon_settings() {
            self.daemon_settings.unsupported = true;
            self.daemon_settings.settings = None;
            self.daemon_settings.problem = None;
            cx.notify();
            return;
        }
        self.daemon_settings.unsupported = false;
        let seq = self.daemon_settings.next_request();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { connection.daemon_settings().await })
                .await;
            this.update(cx, |this, cx| {
                this.daemon_settings_arrived(seq, None, result, cx)
            })
            .ok();
        })
        .detach();
    }

    /// The connection came back (a restarted daemon is a new run): asks again once per run.
    pub(crate) fn daemon_settings_follow_run(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            return;
        }
        let epoch = store.state().epoch;
        if epoch.is_none() || epoch == self.daemon_settings.epoch {
            return;
        }
        self.refresh_daemon_settings(cx);
    }

    /// Changes `key` in `daemon.toml` (`Some`) or removes it, back to the default (`None`).
    /// Whatever the daemon answers is shown: the new list, or why it refused.
    pub fn daemon_setting_set(&mut self, key: &str, value: Option<Json>, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        let connection = store.connection().clone();
        if store.connection_state() != ConnectionState::Connected {
            self.daemon_settings
                .errors
                .insert(key.to_owned(), words::DAEMON_NOT_RUNNING.to_owned());
            cx.notify();
            return;
        }
        self.daemon_settings.errors.remove(key);
        let seq = self.daemon_settings.next_request();
        let key = key.to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let asked = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move { connection.daemon_setting_set(&asked, value).await })
                .await;
            this.update(cx, |this, cx| {
                this.daemon_settings_arrived(seq, Some(key), result, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Back to the default: removes `key` from `daemon.toml`.
    pub fn daemon_setting_reset(&mut self, key: &str, cx: &mut Context<Self>) {
        self.daemon_setting_set(key, None, cx);
    }

    fn daemon_settings_arrived(
        &mut self,
        seq: u64,
        key: Option<String>,
        result: Result<DaemonSettings, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(settings) => {
                if seq > self.daemon_settings.applied {
                    self.daemon_settings.applied = seq;
                    // The Assistant panel says "Automatic" unless the file names a backend.
                    let in_file = |key: &str| {
                        settings
                            .entries
                            .iter()
                            .any(|e| e.key == key && e.set_in_file && e.value != json!(""))
                    };
                    let named = in_file("assistant.backend") || in_file("assistant.provider");
                    self.sessions
                        .update(cx, |store, cx| store.set_assistant_file_choice(named, cx));
                    self.daemon_settings.settings = Some(settings);
                    self.daemon_settings.problem = None;
                }
                if let Some(key) = &key {
                    self.daemon_settings.errors.remove(key);
                    if let Some(sheet) = &mut self.sheet {
                        if sheet
                            .edit
                            .as_ref()
                            .is_some_and(|e| e.field.strip_prefix(EDIT_PREFIX) == Some(key))
                        {
                            sheet.edit = None;
                        }
                        sheet.dropdown = None;
                    }
                    self.after_daemon_change(key, cx);
                }
            }
            Err(ClientError::Disconnected) => {}
            Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                self.daemon_settings.problem = Some(body.message);
            }
            Err(error) => match key {
                Some(key) => {
                    self.daemon_settings.errors.insert(key, refusal(&error));
                    if let Some(sheet) = &mut self.sheet {
                        sheet.dropdown = None;
                    }
                }
                None => self.daemon_settings.problem = Some(refusal(&error)),
            },
        }
        cx.notify();
    }

    /// What a change of `key` touches outside the list.
    fn after_daemon_change(&mut self, key: &str, cx: &mut Context<Self>) {
        if key.starts_with("history.") {
            self.refresh_history_index(cx);
            self.refresh_history_panel(cx);
        }
        if key.starts_with("mcp.") {
            self.mcp_changed(key);
        }
    }

    // ---- changing ------------------------------------------------------------------------

    /// Flips a boolean setting.
    pub fn daemon_toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(on) = self
            .daemon_settings
            .entry(key)
            .filter(|e| e.kind == DaemonSettingKind::Bool)
            .map(|e| e.value.as_bool().unwrap_or(false))
        {
            self.daemon_setting_set(key, Some(Json::Bool(!on)), cx);
        }
    }

    /// Picks `choice` (as text: `ask`, `30`) of a choice setting.
    pub fn daemon_choose(&mut self, key: &str, choice: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.daemon_settings.entry(key) else {
            return;
        };
        let Some(choices) = choices_of(entry) else {
            return;
        };
        if !choices.iter().any(|(value, _)| value == choice) {
            return;
        }
        let value = match entry.kind {
            DaemonSettingKind::Int { .. } => match choice.parse::<i64>() {
                Ok(n) => json!(n),
                Err(_) => return,
            },
            _ => Json::String(choice.to_owned()),
        };
        self.daemon_setting_set(key, Some(value), cx);
    }

    /// The items of the list `key`.
    pub fn daemon_list(&self, key: &str) -> Vec<String> {
        self.daemon_settings
            .value(key)
            .map(strings_of)
            .unwrap_or_default()
    }

    fn daemon_list_write(&mut self, key: &str, items: Vec<String>, cx: &mut Context<Self>) {
        self.daemon_setting_set(key, Some(json!(items)), cx);
    }

    pub fn daemon_list_remove(&mut self, key: &str, index: usize, cx: &mut Context<Self>) {
        let mut items = self.daemon_list(key);
        if index < items.len() {
            items.remove(index);
            self.daemon_list_write(key, items, cx);
        }
    }

    /// Adds `item` at the end of the list `key`. A list of choices takes only its choices,
    /// each once.
    pub fn daemon_list_add(&mut self, key: &str, item: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.daemon_settings.entry(key) else {
            return;
        };
        let mut items = strings_of(&entry.value);
        if let DaemonSettingKind::StringList {
            choices: Some(choices),
        } = &entry.kind
            && (!choices.iter().any(|c| c == item) || items.iter().any(|i| i == item))
        {
            return;
        }
        items.push(item.to_owned());
        self.daemon_list_write(key, items, cx);
    }

    /// Moves item `index` of the list `key` by `delta` places (the order of arguments counts).
    pub fn daemon_list_move(
        &mut self,
        key: &str,
        index: usize,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let mut items = self.daemon_list(key);
        let Some(target) = index
            .checked_add_signed(delta)
            .filter(|t| *t < items.len() && index < items.len())
        else {
            return;
        };
        items.swap(index, target);
        self.daemon_list_write(key, items, cx);
    }

    /// The variables of the table `key`.
    pub fn daemon_map(&self, key: &str) -> BTreeMap<String, String> {
        self.daemon_settings
            .value(key)
            .and_then(Json::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Sets variable `name` of the table `key`.
    pub fn daemon_map_set(&mut self, key: &str, name: &str, value: &str, cx: &mut Context<Self>) {
        if !valid_env_name(name) {
            self.daemon_settings
                .errors
                .insert(key.to_owned(), words::ENV_BAD_NAME.to_owned());
            cx.notify();
            return;
        }
        let mut map = self.daemon_map(key);
        map.insert(name.to_owned(), value.to_owned());
        self.daemon_setting_set(key, Some(json!(map)), cx);
    }

    pub fn daemon_map_remove(&mut self, key: &str, name: &str, cx: &mut Context<Self>) {
        let mut map = self.daemon_map(key);
        if map.remove(name).is_some() {
            self.daemon_setting_set(key, Some(json!(map)), cx);
        }
    }

    /// "Choose…": the system's open panel for a file (`bin`, `cli`) or a folder, then the
    /// daemon checks the path.
    pub fn daemon_choose_path(&mut self, key: &str, cx: &mut Context<Self>) {
        let files = key.ends_with(".bin") || key.ends_with(".cli");
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files,
            directories: !files,
            multiple: false,
            prompt: Some(words::CHOOSE_PROMPT.into()),
        });
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update(cx, |this, cx| {
                    this.daemon_setting_set(&key, Some(json!(path.to_string_lossy())), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// "Open daemon.toml": in the editor the system opens it with.
    pub fn open_daemon_toml(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.daemon_toml_path() else {
            self.set_notice(words::CANNOT_LOCATE, cx);
            return;
        };
        if !path.exists() {
            self.set_notice(words::not_there_yet(&path), cx);
            return;
        }
        (self.daemon.open.clone())(&path, cx);
    }

    // ---- text boxes ----------------------------------------------------------------------

    /// Starts editing the number, text or "add" box of `key`.
    pub fn daemon_begin_edit(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.daemon_settings.entry(key) else {
            return;
        };
        let (text, list_add) = match &entry.kind {
            DaemonSettingKind::Int { .. } | DaemonSettingKind::String => {
                (display_value(&entry.value), false)
            }
            DaemonSettingKind::StringList { .. } | DaemonSettingKind::StringMap => {
                (String::new(), true)
            }
            _ => return,
        };
        self.daemon_start_edit(key, text, list_add, None, cx);
    }

    /// Starts editing the value of variable `name` of the table `key`.
    pub fn daemon_begin_edit_entry(&mut self, key: &str, name: &str, cx: &mut Context<Self>) {
        let Some(text) = self.daemon_map(key).get(name).cloned() else {
            return;
        };
        self.daemon_start_edit(key, text, false, Some(name.to_owned()), cx);
    }

    pub(super) fn daemon_start_edit(
        &mut self,
        key: &str,
        text: String,
        list_add: bool,
        entry: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.edits.sheet_value.reset();
        self.daemon_settings.errors.remove(key);
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.edit = Some(Edit {
                field: format!("{EDIT_PREFIX}{key}"),
                text,
                list_add,
                entry,
                error: None,
            });
            cx.notify();
        }
    }

    /// Enter in a text box of a daemon setting: what fits is sent (the daemon checks the rest
    /// and answers with the reason under the box); what cannot be parsed stays in the box.
    pub(super) fn daemon_commit_edit(&mut self, key: &str, edit: &Edit, cx: &mut Context<Self>) {
        if let Some(field) = key.strip_prefix("mcp-form:") {
            self.mcp_commit_field(field, cx);
            return;
        }
        let Some(entry) = self.daemon_settings.entry(key) else {
            return;
        };
        let text = edit.text.trim();
        let outcome: Result<Json, String> = match (&entry.kind, edit.list_add, &edit.entry) {
            (DaemonSettingKind::Int { .. }, _, _) => text
                .parse::<i64>()
                .map(|n| json!(n))
                .map_err(|_| "expected a whole number".to_owned()),
            (DaemonSettingKind::String, _, _) => Ok(Json::String(edit.text.clone())),
            (DaemonSettingKind::StringList { choices }, true, _) => {
                let mut items = strings_of(&entry.value);
                match choices {
                    _ if text.is_empty() => Err("type an item first".to_owned()),
                    Some(choices) if !choices.iter().any(|c| c == text) => {
                        Err(format!("one of: {}", choices.join(", ")))
                    }
                    Some(_) if items.iter().any(|i| i == text) => {
                        Err("already in the list".to_owned())
                    }
                    _ => {
                        items.push(text.to_owned());
                        Ok(json!(items))
                    }
                }
            }
            (DaemonSettingKind::StringMap, true, _) => match edit.text.split_once('=') {
                Some((name, value)) => {
                    let (name, value) = (name.trim().to_owned(), value.to_owned());
                    self.daemon_map_set(key, &name, &value, cx);
                    return;
                }
                None => Err(words::ENV_ADD_FORMAT.to_owned()),
            },
            (DaemonSettingKind::StringMap, false, Some(name)) => {
                let (name, value) = (name.clone(), edit.text.clone());
                self.daemon_map_set(key, &name, &value, cx);
                return;
            }
            _ => Err("this setting cannot be edited here".to_owned()),
        };
        match outcome {
            Ok(value) => self.daemon_setting_set(key, Some(value), cx),
            Err(message) => {
                self.daemon_settings.errors.insert(key.to_owned(), message);
                cx.notify();
            }
        }
    }

    // ---- rendering: the state of the file ------------------------------------------------

    /// The banner at the top of the sheet while a restart is pending.
    pub(super) fn render_restart_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.daemon_settings.restart_pending() || !self.daemon_connected(cx) {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let action = match self.daemon.busy {
            Some(busy) => note(busy.label(), &theme, &tokens).into_any_element(),
            None => small_button("restart-now", words::RESTART_NOW, false, &theme, &tokens)
                .relative()
                .on_click(cx.listener(|this, _, window, cx| this.restart_daemon(window, cx)))
                .child(probe(&self.regions, Region::Settings("restart-now")))
                .into_any_element(),
        };
        Some(
            div()
                .id("restart-banner")
                .flex()
                .items_center()
                .justify_between()
                .gap(px(tokens.spacing * 3.0))
                .px(px(tokens.spacing * 4.0))
                .py(px(tokens.spacing * 2.0))
                .bg(theme.accent_soft)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_color(theme.warning)
                        .child(SharedString::from(words::RESTART_BANNER)),
                )
                .child(action)
                .into_any_element(),
        )
    }

    /// The banner of a `daemon.toml` the daemon cannot read.
    fn render_file_banner(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        div()
            .id("daemon-file-banner")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .my(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.danger)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.danger)
                    .child(SharedString::from(words::DAEMON_FILE_BROKEN)),
            )
            .child(note(message.to_owned(), &theme, &tokens))
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        small_button(
                            "open-daemon-toml",
                            words::OPEN_DAEMON_TOML,
                            false,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.open_daemon_toml(cx)))
                        .child(probe(&self.regions, Region::Settings("open-daemon-toml"))),
                    )
                    .child(
                        small_button(
                            "check-daemon-toml",
                            words::CHECK_AGAIN,
                            false,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.refresh_daemon_settings(cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    /// A heading with a line of help above a group of rows.
    pub(super) fn section_heading(&self, title: &str, help: &str) -> AnyElement {
        heading(title, help, &self.theme, &self.density.ui())
    }

    /// What stands in place of the settings when the daemon cannot show them, or the banner
    /// to put above them. `Ok`: the settings can be shown.
    pub(super) fn daemon_gate(
        &self,
        cx: &mut Context<Self>,
    ) -> Result<Vec<AnyElement>, Vec<AnyElement>> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let state = &self.daemon_settings;
        if !self.daemon_connected(cx) {
            return Err(vec![
                note(words::DAEMON_NOT_RUNNING, &theme, &tokens).into_any_element(),
            ]);
        }
        if state.unsupported {
            let mut out = vec![
                div()
                    .py(px(tokens.spacing * 2.0))
                    .child(SharedString::from(words::DAEMON_UNSUPPORTED))
                    .into_any_element(),
            ];
            if let Some(path) = self.daemon_toml_path() {
                out.push(
                    note(
                        words::daemon_file_line(&path.display().to_string()),
                        &theme,
                        &tokens,
                    )
                    .into_any_element(),
                );
            }
            out.push(
                div()
                    .flex()
                    .py(px(tokens.spacing * 2.0))
                    .child(
                        small_button(
                            "open-daemon-toml",
                            words::OPEN_DAEMON_TOML,
                            false,
                            &theme,
                            &tokens,
                        )
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| this.open_daemon_toml(cx)))
                        .child(probe(&self.regions, Region::Settings("open-daemon-toml"))),
                    )
                    .into_any_element(),
            );
            return Err(out);
        }
        if let Some(problem) = state.problem.clone() {
            let banner = self.render_file_banner(&problem, cx);
            return if state.settings.is_some() {
                Ok(vec![banner])
            } else {
                Err(vec![banner])
            };
        }
        if state.settings.is_none() {
            return Err(vec![
                note(words::DAEMON_ASKING, &theme, &tokens).into_any_element(),
            ]);
        }
        Ok(Vec::new())
    }

    /// The daemon settings of `page`: what stands in for them while they cannot be shown, else
    /// the sections.
    pub(super) fn render_daemon_page(
        &mut self,
        page: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match self.daemon_gate(cx) {
            Err(instead) => instead,
            Ok(mut rows) => {
                for section in sections(page) {
                    rows.extend(self.render_daemon_section(section, cx));
                }
                rows
            }
        }
    }

    /// One section: its heading and a row per setting the daemon lists.
    pub(super) fn render_daemon_section(
        &self,
        section: &Section,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let mut rows = Vec::new();
        if !section.title.is_empty() {
            rows.push(heading(section.title, section.help, &theme, &tokens));
        }
        for key in section.keys {
            if *key == MCP_KEYS {
                rows.extend(self.render_mcp_rows(cx));
            } else if let Some(entry) = self.daemon_settings.entry(key).cloned() {
                rows.push(self.render_daemon_row(&entry, cx));
            }
        }
        rows
    }

    // ---- rendering: one setting ----------------------------------------------------------

    /// `value` of `e` as a person reads it.
    fn daemon_value_text(&self, e: &DaemonSetting, value: &Json) -> String {
        match &e.kind {
            DaemonSettingKind::Bool => if value.as_bool().unwrap_or(false) {
                "On"
            } else {
                "Off"
            }
            .to_owned(),
            DaemonSettingKind::Enum { .. } => match value.as_str() {
                Some("") | None => words::AUTOMATIC.to_owned(),
                Some(s) => daemon_value_label(&e.key, s),
            },
            DaemonSettingKind::Int { .. } if e.key == "history.range_days" => {
                daemon_value_label(&e.key, &choice_text(value))
            }
            DaemonSettingKind::Int { .. } => choice_text(value),
            DaemonSettingKind::String => match value.as_str() {
                Some("") | None => words::NONE_SET.to_owned(),
                Some(s) => s.to_owned(),
            },
            DaemonSettingKind::Path => match value.as_str() {
                Some("") | None => if e.key.ends_with(".bin") || e.key.ends_with(".cli") {
                    words::LOOKED_UP
                } else {
                    words::USUAL_PLACE
                }
                .to_owned(),
                Some(s) => s.to_owned(),
            },
            DaemonSettingKind::StringList { .. } => {
                let items = strings_of(value);
                if items.is_empty() {
                    words::NONE_SET.to_owned()
                } else {
                    items
                        .iter()
                        .map(|i| daemon_value_label(&e.key, i))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            }
            DaemonSettingKind::StringMap => {
                let count = value.as_object().map_or(0, serde_json::Map::len);
                if count == 0 {
                    words::NONE_SET.to_owned()
                } else {
                    format!("{count} set")
                }
            }
            DaemonSettingKind::Table => String::new(),
        }
    }

    /// The default as the source line names it: like [`Self::daemon_value_text`], but a list of
    /// more than three is counted ("all 8", "5 of 8") instead of spelled out, so the line fits
    /// under the help and the chips beside it show the items.
    fn daemon_default_text(&self, e: &DaemonSetting) -> String {
        if let DaemonSettingKind::StringList { choices } = &e.kind {
            let items = strings_of(&e.default);
            if items.len() > 3 {
                return match choices {
                    Some(choices) if choices.len() == items.len() => {
                        format!("all {}", items.len())
                    }
                    Some(choices) => format!("{} of {}", items.len(), choices.len()),
                    None => format!("{} items", items.len()),
                };
            }
        }
        self.daemon_value_text(e, &e.default)
    }

    /// "Default: On · applies at once": whether the file sets it and when a change counts.
    pub(super) fn daemon_source_line(&self, e: &DaemonSetting) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let table = e.kind == DaemonSettingKind::Table;
        let default = self.daemon_default_text(e);
        let source = match (e.set_in_file, table) {
            (true, true) => words::SET_IN_FILE.to_owned(),
            (true, false) => words::set_with_default(&default),
            (false, true) => words::NONE_SET.to_owned(),
            (false, false) => words::default_is(&default),
        };
        let applies = match (e.pending_restart, e.applies) {
            (true, _) => words::PENDING_RESTART,
            (false, Applies::Live) => words::APPLIES_LIVE,
            (false, Applies::Restart) => words::APPLIES_RESTART,
        };
        div()
            .id(SharedString::from(format!("dsource-{}", e.key)))
            .flex()
            .flex_wrap()
            .gap(px(tokens.spacing))
            .text_size(px(tokens.secondary_font_size))
            .child(
                div()
                    .text_color(if e.set_in_file {
                        theme.accent
                    } else {
                        theme.faint
                    })
                    .child(SharedString::from(source)),
            )
            .child(div().text_color(theme.faint).child("·"))
            .child(
                div()
                    .text_color(if e.pending_restart {
                        theme.warning
                    } else {
                        theme.faint
                    })
                    .child(SharedString::from(applies)),
            )
            .into_any_element()
    }

    /// One row: title and help, where the value comes from and when a change counts, the
    /// control of its kind, Reset when the file sets it, and what the daemon refused.
    pub(super) fn render_daemon_row(
        &self,
        e: &DaemonSetting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        let mut under = vec![self.daemon_source_line(e)];
        if let Some(error) = self.daemon_settings.error(&key) {
            under.push(
                div()
                    .id(SharedString::from(format!("derror-{key}")))
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.danger)
                    .child(SharedString::from(error.to_owned()))
                    .into_any_element(),
            );
        }
        if e.kind == DaemonSettingKind::StringMap {
            under.push(
                note(words::ENV_WARNING, &theme, &tokens)
                    .text_color(theme.warning)
                    .into_any_element(),
            );
        }
        let control = self.render_daemon_control(e, cx);
        let reset = (e.set_in_file && e.kind != DaemonSettingKind::Table).then(|| {
            let k = key.clone();
            small_button(
                SharedString::from(format!("dreset-{key}")),
                if e.kind == DaemonSettingKind::Path {
                    words::USE_DEFAULT
                } else {
                    words::RESET
                },
                false,
                &theme,
                &tokens,
            )
            .text_color(theme.muted)
            .on_click(cx.listener(move |this, _, _, cx| this.daemon_setting_reset(&k, cx)))
        });
        let row = sheet_row(
            SharedString::from(format!("dfield-{key}")),
            daemon_field_title(&key),
            daemon_field_help(&key),
            under,
            div()
                .flex()
                .items_start()
                .gap(px(tokens.spacing * 2.0))
                .children(reset)
                .child(control)
                .into_any_element(),
            &theme,
            &tokens,
        );
        self.focus_ring(&format!("d:{key}"), row)
    }

    fn render_daemon_control(&self, e: &DaemonSetting, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        if let Some(options) = choices_of(e) {
            return self.render_daemon_dropdown(e, options, cx);
        }
        match &e.kind {
            DaemonSettingKind::Bool => {
                let on = e.value.as_bool().unwrap_or(false);
                let k = key.clone();
                small_button(
                    SharedString::from(format!("dtoggle-{key}")),
                    if on { "On" } else { "Off" },
                    on,
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.daemon_toggle(&k, cx)))
                .into_any_element()
            }
            DaemonSettingKind::Int { min, max } => {
                let hint = match (min, max) {
                    (Some(low), Some(high)) => Some(words::range_hint(*low as f64, *high as f64)),
                    (Some(low), None) => Some(words::at_least(*low)),
                    _ => None,
                };
                self.render_daemon_box(e, hint, cx)
            }
            DaemonSettingKind::String => self.render_daemon_box(e, None, cx),
            DaemonSettingKind::Path => {
                let (k_choose, shown) = (key.clone(), self.daemon_value_text(e, &e.value));
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("dvalue-{key}")))
                            .max_w(px(240.0))
                            .overflow_hidden()
                            .text_color(if e.set_in_file { theme.fg } else { theme.muted })
                            .child(SharedString::from(shown)),
                    )
                    .child(
                        small_button(
                            SharedString::from(format!("dchoose-{key}")),
                            words::CHOOSE_PATH,
                            false,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.daemon_choose_path(&k_choose, cx);
                        })),
                    )
                    .into_any_element()
            }
            DaemonSettingKind::StringList { choices } => {
                self.render_daemon_list(e, choices.clone(), cx)
            }
            DaemonSettingKind::StringMap => self.render_daemon_map(e, cx),
            // An MCP server has its own rows (`settings_mcp.rs`).
            DaemonSettingKind::Table => div().into_any_element(),
            DaemonSettingKind::Enum { .. } => div().into_any_element(),
        }
    }

    fn render_daemon_dropdown(
        &self,
        e: &DaemonSetting,
        options: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        let tag = format!("{EDIT_PREFIX}{key}");
        let open = self
            .sheet
            .as_ref()
            .is_some_and(|s| s.dropdown.as_deref() == Some(tag.as_str()));
        let current = choice_text(&e.value);
        let label = match options.iter().find(|(v, _)| *v == current) {
            Some((_, label)) => label.clone(),
            None => words::not_available(&current),
        };
        let k = key.clone();
        let head = small_button(
            SharedString::from(format!("ddropdown-{key}")),
            SharedString::from(format!("{label} ▾")),
            open,
            &theme,
            &tokens,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(sheet) = &mut this.sheet {
                sheet.dropdown = if sheet.dropdown.as_deref() == Some(tag.as_str()) {
                    None
                } else {
                    Some(tag.clone())
                };
                cx.notify();
            }
        }));
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .child(head)
            .when(open, |s| {
                s.children(options.iter().map(|(value, label)| {
                    let (k, v) = (k.clone(), value.clone());
                    small_button(
                        SharedString::from(format!("doption-{key}-{value}")),
                        SharedString::from(label.clone()),
                        *value == current,
                        &theme,
                        &tokens,
                    )
                    .when(
                        self.dropdown_cursor_at(&format!("{EDIT_PREFIX}{key}"), value),
                        |b| b.bg(theme.surface_hover),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.daemon_choose(&k, &v, cx)))
                }))
            })
            .into_any_element()
    }

    /// A number or text box.
    fn render_daemon_box(
        &self,
        e: &DaemonSetting,
        hint: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        let field = format!("{EDIT_PREFIX}{key}");
        let active = self
            .sheet
            .as_ref()
            .and_then(|s| s.edit.as_ref())
            .is_some_and(|edit| edit.field == field);
        let failed = self.daemon_settings.error(&key).is_some();
        let text = self.daemon_value_text(e, &e.value);
        let k = key.clone();
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(2.0))
            .child(
                div()
                    .id(SharedString::from(format!("dinput-{key}")))
                    .flex()
                    .items_center()
                    .min_w(px(150.0))
                    .h(px(tokens.control_height))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .bg(theme.bg)
                    .border_1()
                    .border_color(if failed {
                        theme.danger
                    } else if active {
                        theme.accent
                    } else {
                        theme.border
                    })
                    .cursor_text()
                    .on_click(cx.listener(move |this, _, _, cx| this.daemon_begin_edit(&k, cx)))
                    .children(if active {
                        self.overlay_parts(OverlayField::SheetValue, "", cx)
                    } else {
                        vec![SharedString::from(text).into_any_element()]
                    }),
            )
            .when_some(hint, |s, hint| s.child(note(hint, &theme, &tokens)))
            .into_any_element()
    }

    /// The items of a list as chips, and a way to add: a menu of the choices that are not in
    /// the list yet, or a text box for a list without a known set.
    fn render_daemon_list(
        &self,
        e: &DaemonSetting,
        choices: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        let items = strings_of(&e.value);
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let field = format!("{EDIT_PREFIX}{key}");
        let adding = sheet.edit.as_ref().filter(|edit| edit.field == field);
        let menu = format!("add:{field}");
        let menu_open = sheet.dropdown.as_deref() == Some(menu.as_str());
        let ordered = choices.is_none();
        let last = items.len().saturating_sub(1);
        let chips = items.iter().enumerate().map(|(index, item)| {
            let (k_up, k_down, k_remove) = (key.clone(), key.clone(), key.clone());
            let step = |id: String, label: &'static str, enabled: bool| {
                div()
                    .id(SharedString::from(id))
                    .text_color(if enabled { theme.fg } else { theme.border })
                    .when(enabled, |s| s.cursor_pointer())
                    .child(label)
            };
            let shown = if choices.is_some() {
                daemon_value_label(&key, item)
            } else {
                item.clone()
            };
            div()
                .id(SharedString::from(format!("ditem-{key}-{index}")))
                .flex()
                .items_center()
                .gap(px(tokens.spacing))
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.border)
                .child(SharedString::from(shown))
                .when(ordered, |s| {
                    s.child(step(format!("dup-{key}-{index}"), "↑", index > 0).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.daemon_list_move(&k_up, index, -1, cx)
                        }),
                    ))
                    .child(
                        step(format!("ddown-{key}-{index}"), "↓", index < last).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.daemon_list_move(&k_down, index, 1, cx);
                            }),
                        ),
                    )
                })
                .child(
                    div()
                        .id(SharedString::from(format!("dremove-{key}-{index}")))
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.daemon_list_remove(&k_remove, index, cx);
                        }))
                        .child("×"),
                )
        });
        let add: AnyElement = if let Some(choices) = &choices {
            let remaining: Vec<&String> = choices.iter().filter(|c| !items.contains(c)).collect();
            if remaining.is_empty() {
                div().into_any_element()
            } else {
                let toggle = menu.clone();
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(tokens.spacing))
                    .child(
                        small_button(
                            SharedString::from(format!("dadd-{key}")),
                            words::ADD_CHOICE,
                            menu_open,
                            &theme,
                            &tokens,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(sheet) = &mut this.sheet {
                                sheet.dropdown =
                                    if sheet.dropdown.as_deref() == Some(toggle.as_str()) {
                                        None
                                    } else {
                                        Some(toggle.clone())
                                    };
                                cx.notify();
                            }
                        })),
                    )
                    .when(menu_open, |s| {
                        s.children(remaining.iter().map(|choice| {
                            let (k, c) = (key.clone(), (*choice).clone());
                            small_button(
                                SharedString::from(format!("dchoose-item-{key}-{choice}")),
                                SharedString::from(daemon_value_label(&key, choice)),
                                false,
                                &theme,
                                &tokens,
                            )
                            .when(self.dropdown_cursor_at(&menu, choice), |b| {
                                b.bg(theme.surface_hover)
                            })
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.daemon_list_add(&k, &c, cx);
                                },
                            ))
                        }))
                    })
                    .into_any_element()
            }
        } else {
            match adding {
                Some(_) => div()
                    .flex()
                    .items_center()
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.control_height - 2.0))
                    .rounded(px(tokens.radius))
                    .border_1()
                    .border_color(if self.daemon_settings.error(&key).is_some() {
                        theme.danger
                    } else {
                        theme.accent
                    })
                    .min_w(px(80.0))
                    .children(self.overlay_parts(OverlayField::SheetValue, "", cx))
                    .into_any_element(),
                None => {
                    let k = key.clone();
                    small_button(
                        SharedString::from(format!("dadd-{key}")),
                        "+ add",
                        false,
                        &theme,
                        &tokens,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.daemon_begin_edit(&k, cx)))
                    .into_any_element()
                }
            }
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(320.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(tokens.spacing))
                    .children(chips),
            )
            .child(add)
            .into_any_element()
    }

    /// The variables of a table of text as rows, and "+ add variable".
    fn render_daemon_map(&self, e: &DaemonSetting, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = e.key.clone();
        let rows = self.daemon_map(&key);
        let Some(sheet) = &self.sheet else {
            return div().into_any_element();
        };
        let field = format!("{EDIT_PREFIX}{key}");
        let editing = sheet.edit.as_ref().filter(|edit| edit.field == field);
        let box_of = |this: &Self, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .border_color(theme.accent)
                .min_w(px(160.0))
                .children(this.overlay_parts(OverlayField::SheetValue, "", cx))
        };
        let lines = rows
            .iter()
            .map(|(name, value)| {
                let (k_edit, k_remove) = (key.clone(), key.clone());
                let (n_edit, n_remove) = (name.clone(), name.clone());
                let editing_this = editing.is_some_and(|edit| edit.entry.as_deref() == Some(name));
                div()
                    .id(SharedString::from(format!("dvar-{key}-{name}")))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(tokens.spacing * 2.0))
                    .child(SharedString::from(name.clone()))
                    .child(if editing_this {
                        box_of(self, cx).into_any_element()
                    } else {
                        div()
                            .id(SharedString::from(format!("dvar-value-{key}-{name}")))
                            .cursor_text()
                            .text_color(theme.muted)
                            .child(SharedString::from(if value.is_empty() {
                                words::ENV_EMPTY_VALUE.to_owned()
                            } else {
                                value.clone()
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.daemon_begin_edit_entry(&k_edit, &n_edit, cx);
                            }))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("dvar-remove-{key}-{name}")))
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.daemon_map_remove(&k_remove, &n_remove, cx);
                            }))
                            .child("×"),
                    )
            })
            .collect::<Vec<_>>();
        let add = match editing.filter(|edit| edit.entry.is_none()) {
            Some(_) => box_of(self, cx).into_any_element(),
            None => {
                let k = key.clone();
                small_button(
                    SharedString::from(format!("dadd-{key}")),
                    words::ENV_ADD,
                    false,
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.daemon_begin_edit(&k, cx)))
                .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(360.0))
            .children(lines)
            .child(add)
            .into_any_element()
    }
}
