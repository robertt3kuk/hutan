//! MCP servers in Agents & tools (`mcp.<name>` of `daemon.toml`).
//!
//! The daemon lists one `mcp.<name>` entry per server and `daemon.settings.set` takes the whole
//! server as an object (`{command, args}` or `{url, transport}`) or `null` to remove it, so
//! the sheet manages them: a row per server with Edit and Remove (which asks first), and an
//! "Add server…" form with the name, a program with its arguments or a remote address with
//! its transport. The form is a few text boxes that share the sheet's single text editor: one
//! box is being typed in at a time, Return keeps what was typed and moves to the next box (the
//! last one saves), and Save keeps what is in the box being typed in. A refusal of the daemon
//! shows inside the form; a saved server closes it. The change applies after the daemon
//! restarts, which the sheet's banner says like for every other setting that needs one.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_protocol::daemon_settings::DaemonSetting;
use serde_json::{Value as Json, json};

use super::render::probe;
use super::settings_daemon::{EDIT_PREFIX, MCP_KEYS};
use super::settings_labels::{daemon_field_help, daemon_field_title, words};
use super::settings_rows::{SheetRow, note, small_button};
use super::ui::FocusRingExt as _;
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::theme::DensityExt as _;

/// What `Edit.field` of a text box of the form starts with (after [`EDIT_PREFIX`]).
const FORM_PREFIX: &str = "mcp-form:";

/// A text box of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpField {
    Name,
    Command,
    Args,
    Url,
}

impl McpField {
    fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Command => "command",
            Self::Args => "args",
            Self::Url => "url",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        [Self::Name, Self::Command, Self::Args, Self::Url]
            .into_iter()
            .find(|f| f.id() == id)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => words::MCP_NAME,
            Self::Command => words::MCP_COMMAND,
            Self::Args => words::MCP_ARGS,
            Self::Url => words::MCP_URL,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Name => words::MCP_NAME_HINT,
            Self::Command => words::MCP_COMMAND_HINT,
            Self::Args => words::MCP_ARGS_HINT,
            Self::Url => words::MCP_URL_HINT,
        }
    }
}

/// The form of one MCP server: a new one, or the one named by `editing`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpForm {
    /// The server being edited; its name cannot change.
    pub editing: Option<String>,
    pub name: String,
    /// A remote server (`url`) rather than a program (`command`).
    pub remote: bool,
    pub command: String,
    /// The arguments as one line (see [`split_args`]).
    pub args: String,
    pub url: String,
    /// The remote server talks SSE rather than HTTP.
    pub sse: bool,
    /// What is wrong with the form itself; the daemon's refusal is shown next to it.
    pub error: Option<String>,
}

impl McpForm {
    /// The form of the server `name` as the daemon lists it.
    fn of_server(name: &str, value: &Json) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let args: Vec<String> = value
            .get("args")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            editing: Some(name.to_owned()),
            name: name.to_owned(),
            remote: value.get("url").is_some(),
            command: text("command"),
            args: join_args(&args),
            url: text("url"),
            sse: value.get("transport").and_then(Json::as_str) == Some("sse"),
            error: None,
        }
    }

    /// The key the daemon knows the server by.
    pub fn key(&self) -> String {
        format!(
            "mcp.{}",
            self.editing.as_deref().unwrap_or(self.name.trim())
        )
    }

    /// The boxes of the form, in the order Return goes through them.
    pub fn fields(&self) -> Vec<McpField> {
        let mut fields = Vec::new();
        if self.editing.is_none() {
            fields.push(McpField::Name);
        }
        if self.remote {
            fields.push(McpField::Url);
        } else {
            fields.extend([McpField::Command, McpField::Args]);
        }
        fields
    }

    pub fn text(&self, field: McpField) -> &str {
        match field {
            McpField::Name => &self.name,
            McpField::Command => &self.command,
            McpField::Args => &self.args,
            McpField::Url => &self.url,
        }
    }

    fn set_text(&mut self, field: McpField, text: &str) {
        let target = match field {
            McpField::Name => &mut self.name,
            McpField::Command => &mut self.command,
            McpField::Args => &mut self.args,
            McpField::Url => &mut self.url,
        };
        *target = text.trim().to_owned();
        self.error = None;
    }

    /// The server as `daemon.settings.set` takes it, or what is missing. The daemon checks the
    /// rest (the name's letters, the address's scheme) and its refusal shows in the form.
    pub fn to_value(&self) -> Result<Json, String> {
        if self.editing.is_none() && self.name.trim().is_empty() {
            return Err(words::MCP_NEEDS_NAME.to_owned());
        }
        if self.remote {
            if self.url.trim().is_empty() {
                return Err(words::MCP_NEEDS_URL.to_owned());
            }
            let mut server = json!({"url": self.url.trim()});
            if self.sse {
                server["transport"] = json!("sse");
            }
            return Ok(server);
        }
        if self.command.trim().is_empty() {
            return Err(words::MCP_NEEDS_COMMAND.to_owned());
        }
        let args = split_args(&self.args)?;
        let mut server = json!({"command": self.command.trim()});
        if !args.is_empty() {
            server["args"] = json!(args);
        }
        Ok(server)
    }
}

/// The words of `line` as arguments: separated by white space, with `'…'` and `"…"` to keep
/// white space inside one (inside `"…"` a backslash makes the next character plain) and a
/// backslash outside quotes doing the same.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(words::MCP_QUOTE_OPEN.to_owned()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c) => current.push(c),
                            None => return Err(words::MCP_QUOTE_OPEN.to_owned()),
                        },
                        Some(c) => current.push(c),
                        None => return Err(words::MCP_QUOTE_OPEN.to_owned()),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some(c) => current.push(c),
                    None => current.push('\\'),
                }
            }
            c if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            c => {
                started = true;
                current.push(c);
            }
        }
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

/// `args` as one line that [`split_args`] reads back.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if arg.is_empty() {
                "\"\"".to_owned()
            } else if arg
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\'))
            {
                format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a server is, in a line: the program with its arguments, or the address and transport.
pub fn mcp_summary(value: &Json) -> String {
    if let Some(url) = value.get("url").and_then(Json::as_str) {
        let sse = value.get("transport").and_then(Json::as_str) == Some("sse");
        return if sse {
            format!("{url} (SSE)")
        } else {
            url.to_owned()
        };
    }
    let command = value
        .get("command")
        .and_then(Json::as_str)
        .unwrap_or_default();
    let args: Vec<String> = value
        .get("args")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if args.is_empty() {
        command.to_owned()
    } else {
        format!("{command} {}", join_args(&args))
    }
}

impl Workbench {
    // ---- reading -------------------------------------------------------------------------

    /// The form that is open, if any.
    pub fn mcp_form(&self) -> Option<&McpForm> {
        self.daemon_settings.mcp_form.as_ref()
    }

    /// The servers the daemon lists, in list order.
    fn mcp_servers(&self) -> Vec<DaemonSetting> {
        self.daemon_settings
            .settings()
            .into_iter()
            .flat_map(|s| s.entries.iter())
            .filter(|e| e.key.starts_with("mcp."))
            .cloned()
            .collect()
    }

    // ---- the form ------------------------------------------------------------------------

    /// "Add server…": an empty form, with the name box ready to type in.
    pub fn mcp_begin_add(&mut self, cx: &mut Context<Self>) {
        if self.sheet.is_none() {
            return;
        }
        self.daemon_settings.mcp_form = Some(McpForm::default());
        self.mcp_begin_field(McpField::Name, cx);
    }

    /// "Edit" on the server `name`: its form, with the first box ready to type in.
    pub fn mcp_begin_edit(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.sheet.is_none() {
            return;
        }
        let Some(value) = self
            .daemon_settings
            .value(&format!("mcp.{name}"))
            .filter(|v| v.is_object())
            .cloned()
        else {
            return;
        };
        let form = McpForm::of_server(name, &value);
        let first = form.fields().first().copied();
        self.daemon_settings.errors.remove(&form.key());
        self.daemon_settings.mcp_form = Some(form);
        match first {
            Some(field) => self.mcp_begin_field(field, cx),
            None => cx.notify(),
        }
    }

    /// Keeps what is typed in the box being typed in, if it is one of the form's.
    fn mcp_keep_typed(&mut self) {
        let Some(edit) = self.sheet.as_ref().and_then(|s| s.edit.clone()) else {
            return;
        };
        let Some(field) = edit
            .field
            .strip_prefix(EDIT_PREFIX)
            .and_then(|f| f.strip_prefix(FORM_PREFIX))
            .and_then(McpField::parse)
        else {
            return;
        };
        if let Some(form) = &mut self.daemon_settings.mcp_form {
            form.set_text(field, &edit.text);
        }
        if let Some(sheet) = &mut self.sheet {
            sheet.edit = None;
        }
    }

    /// Starts typing in `field` of the form (what was typed in another box is kept).
    pub fn mcp_begin_field(&mut self, field: McpField, cx: &mut Context<Self>) {
        self.mcp_keep_typed();
        let Some(form) = &self.daemon_settings.mcp_form else {
            return;
        };
        if !form.fields().contains(&field) {
            return;
        }
        let text = form.text(field).to_owned();
        self.daemon_start_edit(
            &format!("{FORM_PREFIX}{}", field.id()),
            text,
            false,
            None,
            cx,
        );
    }

    /// Return in a box of the form: keeps the text and moves to the next box; after the last
    /// one the server is saved.
    pub(super) fn mcp_commit_field(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(field) = McpField::parse(id) else {
            return;
        };
        self.mcp_keep_typed();
        let next = self.daemon_settings.mcp_form.as_ref().and_then(|form| {
            let fields = form.fields();
            let at = fields.iter().position(|f| *f == field)?;
            fields.get(at + 1).copied()
        });
        match next {
            Some(next) => self.mcp_begin_field(next, cx),
            None => self.mcp_save(cx),
        }
    }

    /// A program (`remote: false`) or a remote server.
    pub fn mcp_set_remote(&mut self, remote: bool, cx: &mut Context<Self>) {
        self.mcp_keep_typed();
        if let Some(form) = &mut self.daemon_settings.mcp_form {
            form.remote = remote;
            form.error = None;
            cx.notify();
        }
    }

    /// The transport of a remote server.
    pub fn mcp_set_sse(&mut self, sse: bool, cx: &mut Context<Self>) {
        self.mcp_keep_typed();
        if let Some(form) = &mut self.daemon_settings.mcp_form {
            form.sse = sse;
            form.error = None;
            cx.notify();
        }
    }

    /// Save: what is missing shows in the form; the rest goes to the daemon, whose refusal shows
    /// in the form too and whose answer closes it.
    pub fn mcp_save(&mut self, cx: &mut Context<Self>) {
        self.mcp_keep_typed();
        let Some(form) = self.daemon_settings.mcp_form.clone() else {
            return;
        };
        match form.to_value() {
            Ok(value) => self.daemon_setting_set(&form.key(), Some(value), cx),
            Err(message) => {
                if let Some(form) = &mut self.daemon_settings.mcp_form {
                    form.error = Some(message);
                }
                cx.notify();
            }
        }
    }

    /// Cancel: the form closes and nothing is sent.
    pub fn mcp_cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self.daemon_settings.mcp_form.take() {
            self.daemon_settings.errors.remove(&form.key());
        }
        if let Some(sheet) = &mut self.sheet
            && sheet
                .edit
                .as_ref()
                .is_some_and(|e| e.field.starts_with(&format!("{EDIT_PREFIX}{FORM_PREFIX}")))
        {
            sheet.edit = None;
        }
        cx.notify();
    }

    /// The daemon accepted a change of `key`: the form of that server is done.
    pub(super) fn mcp_changed(&mut self, key: &str) {
        if self
            .daemon_settings
            .mcp_form
            .as_ref()
            .is_some_and(|form| form.key() == key)
        {
            self.daemon_settings.mcp_form = None;
        }
    }

    /// "Remove…": asks, then takes the server out of `daemon.toml`.
    pub fn mcp_remove(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let key = format!("mcp.{name}");
        self.open_destructive(
            words::mcp_remove_title(name),
            words::MCP_REMOVE_MESSAGE,
            words::MCP_REMOVE_CONFIRM,
            move |this, _, cx| {
                this.mcp_changed(&key);
                this.daemon_setting_set(&key, None, cx);
            },
            window,
            cx,
        );
    }

    // ---- rendering -----------------------------------------------------------------------

    /// The section's rows: a row per server, then the form or "Add server…".
    pub(super) fn render_mcp_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = self.theme;
        let tokens = self.density.ui();
        let servers = self.mcp_servers();
        let mut rows = Vec::new();
        if servers.is_empty() && self.daemon_settings.mcp_form.is_none() {
            rows.push(
                note(words::MCP_NONE, &theme, &tokens)
                    .py(px(tokens.spacing * 2.0))
                    .into_any_element(),
            );
        }
        for (index, server) in servers.iter().enumerate() {
            rows.push(self.render_mcp_server(index, server, cx));
        }
        rows.push(match &self.daemon_settings.mcp_form {
            Some(form) => self.render_mcp_form(form, cx),
            None => {
                let add = small_button("mcp-add", words::MCP_ADD, false, &theme, &tokens)
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.mcp_begin_add(cx)))
                    .child(probe(&self.regions, Region::Settings("mcp-add")));
                let row = div()
                    .id("mcp-add-row")
                    .flex()
                    .py(px(tokens.spacing * 2.0))
                    .child(add)
                    .into_any_element();
                self.sheet_row_focus(&format!("d:{MCP_KEYS}"), row)
            }
        });
        rows
    }

    /// One server: its title, where it stands, what it runs and Edit / Remove….
    fn render_mcp_server(
        &self,
        index: usize,
        server: &DaemonSetting,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let key = server.key.clone();
        let name = key.strip_prefix("mcp.").unwrap_or(&key).to_owned();
        let present = server.value.is_object();
        let mut under: Vec<AnyElement> = Vec::new();
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
        let summary = if present {
            mcp_summary(&server.value)
        } else {
            words::MCP_REMOVED.to_owned()
        };
        let buttons = present.then(|| {
            let (edit_name, remove_name) = (name.clone(), name.clone());
            div()
                .flex()
                .gap(px(tokens.spacing))
                .child(
                    small_button(
                        SharedString::from(format!("mcp-edit-{name}")),
                        words::MCP_EDIT,
                        false,
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.mcp_begin_edit(&edit_name, cx)),
                    )
                    .child(probe(
                        &self.regions,
                        Region::SettingsItem("mcp-edit", index),
                    )),
                )
                .child(
                    small_button(
                        SharedString::from(format!("mcp-remove-{name}")),
                        words::MCP_REMOVE,
                        false,
                        &theme,
                        &tokens,
                    )
                    .relative()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.mcp_remove(&remove_name, window, cx);
                    }))
                    .child(probe(
                        &self.regions,
                        Region::SettingsItem("mcp-remove", index),
                    )),
                )
        });
        let control = div()
            .flex()
            .flex_col()
            .items_end()
            .gap(px(tokens.spacing))
            .max_w(px(360.0))
            .child(
                div()
                    .id(SharedString::from(format!("dtable-{key}")))
                    .text_color(theme.muted)
                    .child(SharedString::from(summary)),
            )
            .children(buttons)
            .into_any_element();
        let row_tag = format!("d:{key}");
        let row = SheetRow::new(
            SharedString::from(format!("dfield-{key}")),
            daemon_field_title(&key),
            daemon_field_help(&key),
            control,
        )
        .under(under)
        .chip(self.daemon_chip(server))
        .full_help(self.row_has_keys(&row_tag) && super::ui::keyboard_focus())
        .top()
        .build(&theme, &tokens);
        self.sheet_row_focus(&row_tag, row)
    }

    /// The form: its boxes, the kind of server, the error, Save and Cancel.
    fn render_mcp_form(&self, form: &McpForm, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let active = self
            .sheet
            .as_ref()
            .and_then(|s| s.edit.as_ref())
            .and_then(|e| e.field.strip_prefix(EDIT_PREFIX))
            .and_then(|f| f.strip_prefix(FORM_PREFIX))
            .and_then(McpField::parse);
        let mut lines: Vec<AnyElement> = Vec::new();
        let line = |label: &str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 3.0))
                .child(
                    div()
                        .w(px(120.0))
                        .flex_none()
                        .text_color(theme.muted)
                        .child(SharedString::from(label.to_owned())),
                )
                .child(div().flex_1().min_w_0().child(control))
                .into_any_element()
        };
        let text_box = |this: &Self, field: McpField, index: usize, cx: &mut Context<Self>| {
            let base = div()
                .flex()
                .items_center()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height - 2.0))
                .rounded(px(tokens.radius))
                .border_1()
                .relative();
            let probed = probe(&this.regions, Region::SettingsItem("mcp-field", index));
            if active == Some(field) {
                base.border_color(theme.border)
                    .focus_ring(true, &theme, &tokens)
                    .children(this.overlay_parts(OverlayField::SheetValue, field.hint(), cx))
                    .child(probed)
                    .into_any_element()
            } else {
                let text = form.text(field);
                base.id(SharedString::from(format!("mcp-field-{}", field.id())))
                    .border_color(theme.border)
                    .cursor_text()
                    .text_color(if text.is_empty() {
                        theme.faint
                    } else {
                        theme.fg
                    })
                    .child(SharedString::from(if text.is_empty() {
                        field.hint().to_owned()
                    } else {
                        text.to_owned()
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.mcp_begin_field(field, cx)))
                    .child(probed)
                    .into_any_element()
            }
        };
        let fields = form.fields();
        if let Some(name) = &form.editing {
            lines.push(line(
                words::MCP_NAME,
                div()
                    .child(SharedString::from(name.clone()))
                    .into_any_element(),
            ));
        }
        let index_of = |field: McpField| fields.iter().position(|f| *f == field).unwrap_or(0);
        if form.editing.is_none() {
            lines.push(line(
                McpField::Name.label(),
                text_box(self, McpField::Name, index_of(McpField::Name), cx),
            ));
        }
        let kind_button = |id: &'static str,
                           label: &'static str,
                           on: bool,
                           remote: bool,
                           cx: &mut Context<Self>| {
            small_button(id, label, on, &theme, &tokens)
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| this.mcp_set_remote(remote, cx)))
                .child(probe(&self.regions, Region::Settings(id)))
        };
        lines.push(line(
            words::MCP_KIND,
            div()
                .flex()
                .gap(px(tokens.spacing))
                .child(kind_button(
                    "mcp-kind-program",
                    words::MCP_KIND_PROGRAM,
                    !form.remote,
                    false,
                    cx,
                ))
                .child(kind_button(
                    "mcp-kind-remote",
                    words::MCP_KIND_REMOTE,
                    form.remote,
                    true,
                    cx,
                ))
                .into_any_element(),
        ));
        if form.remote {
            lines.push(line(
                McpField::Url.label(),
                text_box(self, McpField::Url, index_of(McpField::Url), cx),
            ));
            let transport = |id: &'static str,
                             label: &'static str,
                             on: bool,
                             sse: bool,
                             cx: &mut Context<Self>| {
                small_button(id, label, on, &theme, &tokens)
                    .relative()
                    .on_click(cx.listener(move |this, _, _, cx| this.mcp_set_sse(sse, cx)))
                    .child(probe(&self.regions, Region::Settings(id)))
            };
            lines.push(line(
                words::MCP_TRANSPORT,
                div()
                    .flex()
                    .gap(px(tokens.spacing))
                    .child(transport(
                        "mcp-transport-http",
                        words::MCP_HTTP,
                        !form.sse,
                        false,
                        cx,
                    ))
                    .child(transport(
                        "mcp-transport-sse",
                        words::MCP_SSE,
                        form.sse,
                        true,
                        cx,
                    ))
                    .into_any_element(),
            ));
        } else {
            lines.push(line(
                McpField::Command.label(),
                text_box(self, McpField::Command, index_of(McpField::Command), cx),
            ));
            lines.push(line(
                McpField::Args.label(),
                text_box(self, McpField::Args, index_of(McpField::Args), cx),
            ));
        }
        let error = form
            .error
            .clone()
            .or_else(|| self.daemon_settings.error(&form.key()).map(str::to_owned));
        let save_label = if form.editing.is_some() {
            words::MCP_SAVE
        } else {
            words::MCP_CREATE
        };
        div()
            .id("mcp-form")
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .my(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(if form.editing.is_some() {
                        words::MCP_EDIT_TITLE
                    } else {
                        words::MCP_ADD_TITLE
                    })),
            )
            .children(lines)
            .when_some(error, |s, message| {
                s.child(
                    div()
                        .id("mcp-form-error")
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.danger)
                        .child(SharedString::from(message)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        small_button("mcp-save", save_label, true, &theme, &tokens)
                            .relative()
                            .on_click(cx.listener(|this, _, _, cx| this.mcp_save(cx)))
                            .child(probe(&self.regions, Region::Settings("mcp-save"))),
                    )
                    .child(
                        small_button("mcp-cancel", words::MCP_CANCEL, false, &theme, &tokens)
                            .relative()
                            .on_click(cx.listener(|this, _, _, cx| this.mcp_cancel(cx)))
                            .child(probe(&self.regions, Region::Settings("mcp-cancel"))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_line_is_split_into_arguments_with_quotes_keeping_spaces() {
        assert_eq!(
            split_args("-y @scope/server --root /tmp").unwrap(),
            args(&["-y", "@scope/server", "--root", "/tmp"])
        );
        assert_eq!(
            split_args(r#"--name "my server" 'a b' x\ y"#).unwrap(),
            args(&["--name", "my server", "a b", "x y"])
        );
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        assert_eq!(split_args(r#"a "" b"#).unwrap(), args(&["a", "", "b"]));
        assert_eq!(
            split_args(r#""say \"hi\"""#).unwrap(),
            args(&[r#"say "hi""#])
        );
        assert!(split_args(r#"a "b"#).is_err());
        assert!(split_args("a 'b").is_err());
    }

    #[test]
    fn joining_arguments_is_read_back_the_same() {
        for items in [
            args(&["-y", "docs"]),
            args(&[
                "--name",
                "my server",
                "",
                "it's",
                r#"say "hi""#,
                r"back\slash",
            ]),
            args(&[]),
        ] {
            assert_eq!(split_args(&join_args(&items)).unwrap(), items);
        }
        assert_eq!(join_args(&args(&["-y", "a b"])), r#"-y "a b""#);
    }

    #[test]
    fn a_server_is_one_line() {
        assert_eq!(
            mcp_summary(&json!({"command": "npx", "args": ["-y", "docs server"]})),
            r#"npx -y "docs server""#
        );
        assert_eq!(mcp_summary(&json!({"command": "uvx"})), "uvx");
        assert_eq!(
            mcp_summary(&json!({"url": "https://x.test/mcp"})),
            "https://x.test/mcp"
        );
        assert_eq!(
            mcp_summary(&json!({"url": "https://x.test/mcp", "transport": "sse"})),
            "https://x.test/mcp (SSE)"
        );
    }

    #[test]
    fn the_form_builds_what_the_daemon_takes_and_says_what_is_missing() {
        let mut form = McpForm::default();
        assert_eq!(form.to_value().unwrap_err(), words::MCP_NEEDS_NAME);
        form.name = "docs".into();
        assert_eq!(form.to_value().unwrap_err(), words::MCP_NEEDS_COMMAND);
        form.command = "npx".into();
        form.args = r#"-y "docs server""#.into();
        assert_eq!(
            form.to_value().unwrap(),
            json!({"command": "npx", "args": ["-y", "docs server"]})
        );
        form.args = "\"open".into();
        assert_eq!(form.to_value().unwrap_err(), words::MCP_QUOTE_OPEN);
        form.remote = true;
        assert_eq!(form.to_value().unwrap_err(), words::MCP_NEEDS_URL);
        form.url = "https://x.test/mcp".into();
        assert_eq!(
            form.to_value().unwrap(),
            json!({"url": "https://x.test/mcp"})
        );
        form.sse = true;
        assert_eq!(
            form.to_value().unwrap(),
            json!({"url": "https://x.test/mcp", "transport": "sse"})
        );
        assert_eq!(form.key(), "mcp.docs");
    }

    #[test]
    fn the_form_of_a_listed_server_reads_back_into_the_same_server() {
        for server in [
            json!({"command": "npx", "args": ["-y", "docs server"]}),
            json!({"command": "uvx"}),
            json!({"url": "https://x.test/mcp"}),
            json!({"url": "https://x.test/mcp", "transport": "sse"}),
        ] {
            let form = McpForm::of_server("docs", &server);
            assert_eq!(form.editing.as_deref(), Some("docs"));
            assert_eq!(form.to_value().unwrap(), server);
            assert_eq!(form.key(), "mcp.docs");
            assert!(!form.fields().contains(&McpField::Name), "the name stays");
        }
    }
}
