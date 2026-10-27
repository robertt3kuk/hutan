//! The window as something agents can drive (protocol 0.21, capability `control`, ADR 0016,
//! `docs/control.md`).
//!
//! - **Publishing.** The window tells the daemon what it can do and what it shows: the action
//!   catalog (every palette command by GPUI action name, plus the parameterised ones), the
//!   settings catalog (every key of the settings schema with its current value and the layers it
//!   can be written to) and a summary of the layout. `ui.publish` goes out when the connection is
//!   made again (a daemon forgets a connection's publish with the connection) and whenever
//!   anything of it changed, at most 250 ms after the first change.
//! - **Executing.** `Notice::UiCommand` (the daemon's `ui.command` event, routed here by
//!   `ui.run` / `ui.set_setting`) runs through the same paths as the keyboard and the palette:
//!   an action is dispatched at the focused element, so what an agent triggers is what the user
//!   could have triggered. The answer goes back as `ui.command_result`.
//!
//! The daemon only caches what is published; layout stays desktop state (ADR 0010).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{Action, AsyncWindowContext, Context, FocusHandle, Task, WeakEntity, Window};
use hutan_client::{ConnectionState, Notice};
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::ui::{
    AppearanceSummary, DockSummary, FocusSummary, LayoutSummary, PaneItem, PaneSummary,
    SET_SETTING_ACTION, UiAction, UiCommandResult, UiPublish, UiSetting,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::actions::{self, Command};
use super::model::{DockPosition, FocusTarget, ItemRef, PaneTree};
use super::settings_schema::{
    FieldKind, catalog, fields_of, json_to_toml, namespace_schema, number_range, refine_kind,
    value_at,
};
use super::view::Workbench;
use crate::sessions::SessionsStore;

/// How long after the first change the window publishes (later changes ride along).
pub const PUBLISH_DEBOUNCE: Duration = Duration::from_millis(250);

/// The settings layers a key takes: the user's `settings.toml` always, and for a namespace a
/// workspace may override also `workspace` and `project`, both of which are the
/// `.hutan/settings.toml` in the first folder of the active workspace (the file the settings
/// store layers over the user's; a project is the folder a workspace root points at).
fn layers_of(workspace_overridable: bool) -> Vec<String> {
    let names: &[&str] = if workspace_overridable {
        &["user", "workspace", "project"]
    } else {
        &["user"]
    };
    names.iter().map(|l| (*l).to_owned()).collect()
}

/// What the window keeps for the control surface.
pub(crate) struct RemoteControl {
    /// The id the daemon knows this window by: unique among the windows of every desktop
    /// process.
    window_id: String,
    /// What the daemon holds for this window, and on which connection it was sent.
    published: Option<(u64, UiPublish)>,
    /// The debounce timer of the next publish.
    pending: Option<Task<()>>,
    /// The `ui.publish` in flight.
    publishing: Option<Task<()>>,
    _commands: Task<()>,
}

impl std::fmt::Debug for RemoteControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteControl")
            .field("window_id", &self.window_id)
            .finish_non_exhaustive()
    }
}

impl RemoteControl {
    /// Starts following the daemon's `ui.command` events for this window.
    pub(crate) fn start(
        sessions: &gpui::Entity<SessionsStore>,
        window: &mut Window,
        cx: &mut Context<Workbench>,
    ) -> Self {
        let window_id = format!(
            "{}-{}",
            std::process::id(),
            window.window_handle().window_id().as_u64()
        );
        let connection = sessions.read(cx).connection().clone();
        let mut notices = connection.notices();
        let own = window_id.clone();
        let commands = cx.spawn_in(window, async move |this, cx| {
            loop {
                match notices.recv().await {
                    Ok(Notice::UiCommand {
                        request_id,
                        window_id,
                        action,
                        args,
                    }) if window_id == own => {
                        let outcome = run_command(&this, cx, &action, args).await;
                        let result = match outcome {
                            Ok(ok) => UiCommandResult {
                                request_id,
                                ok: Some(ok),
                                error: None,
                            },
                            Err(error) => UiCommandResult {
                                request_id,
                                ok: None,
                                error: Some(error),
                            },
                        };
                        if let Err(error) = connection.ui_command_result(result).await {
                            tracing::warn!(%error, request_id, "could not answer a ui.command");
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(missed)) => {
                        // The daemon answers the caller with a timeout.
                        tracing::warn!(missed, "missed daemon notices, a ui.command may be lost");
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
        Self {
            window_id,
            published: None,
            pending: None,
            publishing: None,
            _commands: commands,
        }
    }

    /// The id the daemon knows this window by.
    pub(crate) fn window_id(&self) -> &str {
        &self.window_id
    }
}

// ---- executing ------------------------------------------------------------------------------

/// What step one of a command decided.
enum Prepared {
    /// It already ran (a setting).
    Done(Value),
    /// Dispatch this action at the focused element.
    Dispatch {
        action: Box<dyn Action>,
        fallback_focus: FocusHandle,
    },
}

fn gone() -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, "the window is closing")
}

/// Runs one `ui.command`: validates it and does it on the window (the workbench is only
/// borrowed for the validation: dispatching an action re-enters it), and answers what happened.
async fn run_command(
    this: &WeakEntity<Workbench>,
    cx: &mut AsyncWindowContext,
    action: &str,
    args: Value,
) -> Result<Value, ErrorBody> {
    let name = action.to_owned();
    let prepared = this
        .update_in(cx, |this, _, cx| this.prepare_command(&name, &args, cx))
        .map_err(|_| gone())??;
    match prepared {
        Prepared::Done(value) => {
            // A setting can change what the window offers (`assistant.enabled` adds and removes
            // an action): the window follows and publishes before it answers, so the caller's
            // next command sees the new catalog instead of racing the debounce.
            let publishing = this
                .update_in(cx, |this, window, cx| {
                    let changed = this.settings.changes().borrow().clone();
                    this.on_settings_changed(&changed, window, cx);
                    this.publish_task(cx)
                })
                .map_err(|_| gone())?;
            if let Some(publishing) = publishing {
                publishing.await;
            }
            Ok(value)
        }
        Prepared::Dispatch {
            action: boxed,
            fallback_focus,
        } => {
            let id = boxed.name();
            let dispatched = cx
                .update(|window, cx| {
                    // A focus the window no longer draws (its element went away with a view
                    // that changed) reaches no handler; the workbench's own handle does.
                    let focus = window
                        .focused(cx)
                        .filter(|focus| window.is_action_available_in(boxed.as_ref(), focus))
                        .unwrap_or(fallback_focus);
                    if !window.is_action_available_in(boxed.as_ref(), &focus) {
                        return false;
                    }
                    focus.dispatch_action(boxed.as_ref(), window, cx);
                    true
                })
                .map_err(|_| gone())?;
            if !dispatched {
                return Err(ErrorBody::new(
                    ErrorCode::InvalidState,
                    format!("`{id}` has no handler where the window's focus is now"),
                ));
            }
            // Some actions finish after the dispatch (a split starts a session first): the
            // caller reads the outcome with `ui.layout`, which follows within the debounce.
            Ok(json!({ "ran": id }))
        }
    }
}

#[derive(Deserialize)]
struct SetArgs {
    namespace: String,
    key: String,
    value: Value,
    layer: String,
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn not_found(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, message)
}

/// A string argument of an action.
fn arg_str<'a>(args: &'a Value, key: &str, action: &str) -> Result<&'a str, ErrorBody> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        bad_request(format!(
            "`{action}` takes {{\"{key}\": <string>}}; `hutan ui actions` shows its args_schema"
        ))
    })
}

/// Whether `value` is something a field of `kind` takes.
fn check_value(kind: &FieldKind, value: &Value) -> Result<(), String> {
    match kind {
        FieldKind::Bool if value.is_boolean() => Ok(()),
        FieldKind::Bool => Err("expected true or false".to_owned()),
        FieldKind::Enum(values) => match value.as_str() {
            Some(text) if values.iter().any(|v| v == text) => Ok(()),
            _ => Err(format!("expected one of: {}", values.join(", "))),
        },
        FieldKind::Integer if value.is_i64() || value.is_u64() => Ok(()),
        FieldKind::Integer => Err("expected an integer".to_owned()),
        FieldKind::Number if value.is_number() => Ok(()),
        FieldKind::Number => Err("expected a number".to_owned()),
        FieldKind::String if value.is_string() => Ok(()),
        FieldKind::String => Err("expected a string".to_owned()),
        FieldKind::StringList
            if value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)) =>
        {
            Ok(())
        }
        FieldKind::StringList => Err("expected a list of strings".to_owned()),
        FieldKind::ChoiceList(values)
            if value.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|i| values.iter().any(|v| v == i)))
            }) =>
        {
            Ok(())
        }
        FieldKind::ChoiceList(values) => Err(format!("expected a list of: {}", values.join(", "))),
        FieldKind::StringMap => {
            Err("cannot be set here (edit the variables in Settings)".to_owned())
        }
        FieldKind::Unsupported(hint) => Err(format!("cannot be set here ({hint})")),
    }
}

/// `app.start_at_login` registers the launch agent and `app.keep_running` is saved together
/// with it, which the `Daemon` menu's actions own.
fn writable_kind(namespace: &str, key: &str, kind: &FieldKind) -> FieldKind {
    match (namespace, key) {
        ("app", "keep_running") => {
            FieldKind::Unsupported("use the action daemon::ToggleKeepRunning".to_owned())
        }
        ("app", "start_at_login") => {
            FieldKind::Unsupported("use the action daemon::ToggleStartAtLogin".to_owned())
        }
        _ => refine_kind(namespace, key, kind),
    }
}

/// `{ "type": "object", "properties": { key: { "type": "string", "enum": [...] } } }` with `key`
/// required.
fn choice_schema(key: &str, description: &str, values: Vec<String>) -> Value {
    json!({
        "type": "object",
        "properties": { key: { "type": "string", "enum": values, "description": description } },
        "required": [key],
        "additionalProperties": false,
    })
}

fn free_schema(key: &str, description: &str) -> Value {
    json!({
        "type": "object",
        "properties": { key: { "type": "string", "description": description } },
        "required": [key],
        "additionalProperties": false,
    })
}

/// `workbench::MoveActivePanel` → "Move Active Panel".
fn humanize(action_name: &str) -> String {
    let short = action_name.rsplit("::").next().unwrap_or(action_name);
    let mut out = String::new();
    for (i, c) in short.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

impl Workbench {
    fn apply_preset_id() -> &'static str {
        <actions::ApplyPreset as Action>::name_for_type()
    }

    fn open_panel_id() -> &'static str {
        <actions::OpenPanel as Action>::name_for_type()
    }

    fn open_session_id() -> &'static str {
        <actions::OpenSession as Action>::name_for_type()
    }

    fn open_file_id() -> &'static str {
        <actions::OpenFile as Action>::name_for_type()
    }

    fn open_workspace_id() -> &'static str {
        <actions::OpenWorkspace as Action>::name_for_type()
    }

    /// The panels `open_panel` accepts: every registered one, the Assistant only while
    /// `assistant.enabled` (it opens as the left sidebar's page).
    fn openable_panels(&self) -> Vec<String> {
        self.panels
            .iter()
            .filter(|s| self.assistant_enabled() || !actions::is_mawas_page(s.id.as_str()))
            .map(|s| s.id.to_string())
            .collect()
    }

    /// Every action the window offers: each palette command by action name, the variants of one
    /// action collapsed into a single entry that takes the variant's title as `choice`, and the
    /// parameterised ones (apply preset, open panel, open or focus a session, show a workspace).
    pub(crate) fn action_catalog(&self) -> Vec<UiAction> {
        let special = [
            Self::apply_preset_id(),
            Self::open_panel_id(),
            Self::open_session_id(),
            Self::open_workspace_id(),
        ];
        let mut order: Vec<&'static str> = Vec::new();
        let mut groups: HashMap<&'static str, Vec<&Command>> = HashMap::new();
        for command in self.commands.iter() {
            let name = command.action.name();
            if special.contains(&name) {
                continue;
            }
            groups.entry(name).or_default().push(command);
            if !order.contains(&name) {
                order.push(name);
            }
        }
        let mut out = Vec::new();
        for name in order {
            let group = &groups[name];
            match group.as_slice() {
                [only] => out.push(UiAction {
                    id: name.to_owned(),
                    title: only.title.to_string(),
                    args_schema: None,
                }),
                variants => out.push(UiAction {
                    id: name.to_owned(),
                    title: humanize(name),
                    args_schema: Some(choice_schema(
                        "choice",
                        "which variant: the title it has in the command palette",
                        variants.iter().map(|c| c.title.to_string()).collect(),
                    )),
                }),
            }
        }
        out.push(UiAction {
            id: Self::apply_preset_id().to_owned(),
            title: "Layout: Apply Preset".to_owned(),
            args_schema: Some(choice_schema(
                "name",
                "the layout preset",
                self.presets.iter().map(|p| p.name.clone()).collect(),
            )),
        });
        out.push(UiAction {
            id: Self::open_panel_id().to_owned(),
            title: "Panel: Open".to_owned(),
            args_schema: Some(choice_schema("id", "the panel id", self.openable_panels())),
        });
        out.push(UiAction {
            id: Self::open_session_id().to_owned(),
            title: "Session: Open or Focus".to_owned(),
            args_schema: Some(free_schema("id", "the session id (`hutan ls`)")),
        });
        out.push(UiAction {
            id: Self::open_workspace_id().to_owned(),
            title: "Workspace: Show".to_owned(),
            args_schema: Some(free_schema("id", "the workspace id (`hutan workspace ls`)")),
        });
        out.push(UiAction {
            id: Self::open_file_id().to_owned(),
            title: "File: Open".to_owned(),
            args_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "absolute path of the file (below a workspace root)" },
                    "line": { "type": "integer", "minimum": 1, "description": "1-based line to show" },
                    "column": { "type": "integer", "minimum": 1, "description": "1-based column" },
                },
                "required": ["path"],
                "additionalProperties": false,
            })),
        });
        out
    }

    /// Every key of the settings schema with what it holds right now.
    pub(crate) fn setting_catalog(&self) -> Vec<UiSetting> {
        let schema = self.settings.schema();
        let mut out = Vec::new();
        for entry in catalog() {
            let Some(namespace_schema) = namespace_schema(&schema, entry.namespace) else {
                continue;
            };
            let current = entry.current(&self.settings);
            for field in fields_of(&namespace_schema) {
                let key = field.dotted();
                let kind = writable_kind(entry.namespace, &key, &field.kind);
                let (type_name, values) = match &kind {
                    FieldKind::Bool => ("bool", Vec::new()),
                    FieldKind::Enum(values) => ("enum", values.clone()),
                    FieldKind::Integer => ("integer", Vec::new()),
                    FieldKind::Number => ("number", Vec::new()),
                    FieldKind::String => ("string", Vec::new()),
                    FieldKind::StringList => ("string_list", Vec::new()),
                    FieldKind::ChoiceList(values) => ("string_list", values.clone()),
                    FieldKind::StringMap | FieldKind::Unsupported(_) => ("unsupported", Vec::new()),
                };
                out.push(UiSetting {
                    namespace: entry.namespace.to_owned(),
                    key,
                    kind: type_name.to_owned(),
                    doc: field.doc.clone(),
                    layers: if matches!(kind, FieldKind::Unsupported(_) | FieldKind::StringMap) {
                        Vec::new()
                    } else {
                        layers_of(entry.workspace_overridable)
                    },
                    values,
                    current: value_at(&current, &field.path).cloned(),
                });
            }
        }
        out
    }

    /// What the window shows: the layout of the focused workspace.
    pub(crate) fn layout_summary(&self, cx: &gpui::App) -> LayoutSummary {
        let item = |item: &ItemRef| -> PaneItem {
            let session = |id: &str| id.parse::<SessionId>().ok();
            let (kind, session_id, panel, path) = match item {
                ItemRef::Terminal { session: id } => ("terminal", session(id), None, None),
                // A tab of splits reports its focused split.
                ItemRef::TerminalSplit { split } => (
                    "terminal",
                    split.focused_session().and_then(session),
                    None,
                    None,
                ),
                ItemRef::Chat { session: id } => ("chat", session(id), None, None),
                ItemRef::Panel { panel } => ("panel", None, Some(panel.to_string()), None),
                ItemRef::Placeholder { .. } => ("placeholder", None, None, None),
                ItemRef::Changes { .. } => ("changes", None, None, None),
                ItemRef::File { path } => ("file", None, None, Some(PathBuf::from(path))),
                ItemRef::History { path } => ("history", None, None, Some(PathBuf::from(path))),
                ItemRef::Revision { path, .. } => {
                    ("revision", None, None, Some(PathBuf::from(path)))
                }
                ItemRef::Compare { left, .. } => ("compare", None, None, Some(PathBuf::from(left))),
                ItemRef::CompareHead { path } => ("compare", None, None, Some(PathBuf::from(path))),
            };
            PaneItem {
                kind: kind.to_owned(),
                session_id,
                panel,
                path,
                title: self.item_title(item, cx).to_string(),
            }
        };
        fn tree(node: &PaneTree, item: &dyn Fn(&ItemRef) -> PaneItem) -> PaneSummary {
            match node {
                PaneTree::Split { axis, children, .. } => PaneSummary::Split {
                    axis: enum_name(axis),
                    children: children.iter().map(|c| tree(c, item)).collect(),
                },
                PaneTree::Pane(pane) => PaneSummary::Pane {
                    pane_id: pane.id.to_string(),
                    items: pane.items.iter().map(item).collect(),
                    active: (!pane.items.is_empty())
                        .then(|| u32::try_from(pane.active).unwrap_or(0)),
                },
            }
        }
        let docks = DockPosition::ALL
            .into_iter()
            .map(|position| {
                let dock = self.state.docks.get(position);
                DockSummary {
                    position: enum_name(&position),
                    visible: self.dock_body_shown(position),
                    panels: dock.panels.iter().map(ToString::to_string).collect(),
                    active: dock.active.as_ref().map(ToString::to_string),
                }
            })
            .collect();
        let focused = match self.state.focus {
            FocusTarget::Pane(id) => FocusSummary {
                kind: "pane".to_owned(),
                target: id.to_string(),
            },
            FocusTarget::Dock(position) => FocusSummary {
                kind: "dock".to_owned(),
                target: enum_name(&position),
            },
        };
        LayoutSummary {
            window_id: self.remote.window_id().to_owned(),
            workspace_id: self
                .state
                .workspace()
                .and_then(|id| id.parse::<WorkspaceId>().ok()),
            preset: self.state.preset.clone(),
            docks,
            panes: tree(&self.state.center, &item),
            focused,
            zoomed: self.state.zoomed.map(|id| id.to_string()),
            appearance: AppearanceSummary {
                chrome: enum_name(&self.chrome),
                density: enum_name(&self.density),
            },
            window_active: self.window_active,
        }
    }

    /// The whole publish.
    pub(crate) fn ui_publish(&self, cx: &gpui::App) -> UiPublish {
        UiPublish {
            actions: self.action_catalog(),
            settings: self.setting_catalog(),
            layout: self.layout_summary(cx),
        }
    }

    // ---- publishing ---------------------------------------------------------------------------

    /// Something the daemon may hold stale changed: publish soon (once for a burst of changes).
    pub(crate) fn schedule_publish(&mut self, cx: &mut Context<Self>) {
        if self.remote.pending.is_some() {
            return;
        }
        self.remote.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PUBLISH_DEBOUNCE).await;
            this.update(cx, |this, cx| this.publish_now(cx)).ok();
        }));
    }

    /// Sends `ui.publish` when connected to a daemon that has the control surface and when what
    /// it holds for this window (on this connection) differs from what the window is now.
    pub(crate) fn publish_now(&mut self, cx: &mut Context<Self>) {
        self.remote.pending = None;
        if let Some(task) = self.publish_task(cx) {
            self.remote.publishing = Some(task);
        }
    }

    /// The `ui.publish` [`Self::publish_now`] sends, as a task that ends when the daemon has it;
    /// `None` when there is nothing to send.
    fn publish_task(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let (connection, connected) = {
            let store = self.sessions.read(cx);
            (
                store.connection().clone(),
                store.connection_state() == ConnectionState::Connected,
            )
        };
        if !connected || !connection.supports_control() {
            return None;
        }
        let publish = self.ui_publish(cx);
        let generation = connection.generation();
        if self.remote.published.as_ref() == Some(&(generation, publish.clone())) {
            return None;
        }
        Some(cx.spawn(async move |this, cx| {
            match connection.ui_publish(publish.clone()).await {
                Ok(sent_on) => {
                    this.update(cx, |this, _| {
                        this.remote.published = Some((sent_on, publish));
                    })
                    .ok();
                }
                // A reconnect publishes again.
                Err(error) => tracing::debug!(%error, "could not publish the window to the daemon"),
            }
        }))
    }

    // ---- executing ----------------------------------------------------------------------------

    /// Validates a `ui.command` and, for a setting, performs it; for an action, builds what to
    /// dispatch.
    fn prepare_command(
        &mut self,
        action: &str,
        args: &Value,
        cx: &mut Context<Self>,
    ) -> Result<Prepared, ErrorBody> {
        if action == SET_SETTING_ACTION {
            return self.set_setting(args, cx).map(Prepared::Done);
        }
        let boxed: Box<dyn Action> = if action == Self::apply_preset_id() {
            let name = arg_str(args, "name", action)?;
            if !self.presets.iter().any(|p| p.name == name) {
                let known: Vec<&str> = self.presets.iter().map(|p| p.name.as_str()).collect();
                return Err(not_found(format!(
                    "no layout preset `{name}`; known: {}",
                    known.join(", ")
                )));
            }
            Box::new(actions::ApplyPreset(name.to_owned()))
        } else if action == Self::open_panel_id() {
            let id = arg_str(args, "id", action)?;
            if self.panels.get(&super::model::PanelId::new(id)).is_none() {
                return Err(not_found(format!(
                    "no panel `{id}`; known: {}",
                    self.openable_panels().join(", ")
                )));
            }
            Box::new(actions::OpenPanel(id.to_owned()))
        } else if action == Self::open_session_id() {
            let id = arg_str(args, "id", action)?;
            let session: SessionId = id
                .parse()
                .map_err(|_| bad_request(format!("`{id}` is not a session id")))?;
            if self.sessions.read(cx).session(session).is_none() {
                return Err(not_found(format!("no session {id}")));
            }
            Box::new(actions::OpenSession(id.to_owned()))
        } else if action == Self::open_workspace_id() {
            let id = arg_str(args, "id", action)?;
            let workspace: WorkspaceId = id
                .parse()
                .map_err(|_| bad_request(format!("`{id}` is not a workspace id")))?;
            if self.sessions.read(cx).workspace(workspace).is_none() {
                return Err(not_found(format!("no workspace {id}")));
            }
            Box::new(actions::OpenWorkspace(id.to_owned()))
        } else if action == Self::open_file_id() {
            let path = arg_str(args, "path", action)?;
            if !std::path::Path::new(path).is_absolute() {
                return Err(bad_request(format!("`{path}` is not an absolute path")));
            }
            let number = |key: &str| -> Result<Option<u32>, ErrorBody> {
                match args.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(value) => value
                        .as_u64()
                        .filter(|n| *n >= 1)
                        .and_then(|n| u32::try_from(n).ok())
                        .map(Some)
                        .ok_or_else(|| bad_request(format!("`{key}` must be a positive integer"))),
                }
            };
            Box::new(actions::OpenFile(actions::FileTarget {
                path: path.to_owned(),
                line: number("line")?,
                column: number("column")?,
            }))
        } else {
            let variants: Vec<&Command> = self
                .commands
                .iter()
                .filter(|c| c.action.name() == action)
                .collect();
            match variants.as_slice() {
                [] => return Err(not_found(format!("unknown action `{action}`"))),
                [only] => only.action.boxed_clone(),
                several => {
                    let choice = arg_str(args, "choice", action)?;
                    match several.iter().find(|c| c.title.as_ref() == choice) {
                        Some(command) => command.action.boxed_clone(),
                        None => {
                            let titles: Vec<&str> =
                                several.iter().map(|c| c.title.as_ref()).collect();
                            return Err(bad_request(format!(
                                "`{action}` has no variant `{choice}`; choose one of: {}",
                                titles.join("; ")
                            )));
                        }
                    }
                }
            }
        };
        Ok(Prepared::Dispatch {
            action: boxed,
            fallback_focus: self.focus_handle.clone(),
        })
    }

    /// The folder of the project the window focuses: the project of the session in the focused
    /// pane (`SessionInfo.workspace_id`), else the shown workspace when it is a project itself.
    fn focused_project_root(&self, cx: &gpui::App) -> Option<std::path::PathBuf> {
        let store = self.sessions.read(cx);
        let of_session = self
            .state
            .active_item()
            .as_ref()
            .and_then(super::terminals::session_of_item)
            .and_then(|session| store.session(session))
            .and_then(|session| store.workspace(session.workspace_id))
            .and_then(|project| project.project.as_ref())
            .map(|project| project.root.clone());
        of_session.or_else(|| {
            self.active_workspace()
                .and_then(|id| store.workspace(id))
                .and_then(|w| w.project.as_ref())
                .map(|project| project.root.clone())
        })
    }

    /// `ui.set_setting`: validates the key and the value against the settings schema and writes
    /// the user layer; hot reload applies it like an edit of the file.
    fn set_setting(&mut self, args: &Value, cx: &gpui::App) -> Result<Value, ErrorBody> {
        let req: SetArgs = serde_json::from_value(args.clone())
            .map_err(|e| bad_request(format!("bad ui.set_setting arguments: {e}")))?;
        if !matches!(req.layer.as_str(), "user" | "workspace" | "project") {
            return Err(bad_request(format!(
                "unknown layer `{}`; use user, workspace or project",
                req.layer
            )));
        }
        let Some(entry) = catalog().into_iter().find(|e| e.namespace == req.namespace) else {
            return Err(not_found(format!("unknown namespace `{}`", req.namespace)));
        };
        let schema = self.settings.schema();
        let fields = namespace_schema(&schema, entry.namespace)
            .map(|s| fields_of(&s))
            .unwrap_or_default();
        let Some(field) = fields.iter().find(|f| f.dotted() == req.key) else {
            return Err(not_found(format!(
                "unknown setting `{}.{}`",
                req.namespace, req.key
            )));
        };
        let kind = writable_kind(entry.namespace, &req.key, &field.kind);
        check_value(&kind, &req.value)
            .map_err(|why| bad_request(format!("`{}.{}`: {why}", req.namespace, req.key)))?;
        if let (Some((low, high)), Some(n)) =
            (number_range(entry.namespace, &req.key), req.value.as_f64())
            && !(low..=high).contains(&n)
        {
            return Err(bad_request(format!(
                "`{}.{}`: must be between {low} and {high}; the app keeps it inside that range",
                req.namespace, req.key
            )));
        }
        let value = json_to_toml(&req.value).ok_or_else(|| {
            bad_request(format!(
                "`{}.{}`: the value cannot be written as TOML",
                req.namespace, req.key
            ))
        })?;
        let key_path = format!("{}.{}", req.namespace, req.key);
        if req.layer == "user" {
            self.settings
                .write_user(&key_path, value)
                .map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
        } else {
            if !entry.workspace_overridable {
                return Err(bad_request(format!(
                    "`{}` cannot be overridden per workspace; it takes: user",
                    req.namespace
                )));
            }
            if req.layer == "project" {
                // The project of what the window focuses: its folder's own settings file.
                let Some(root) = self.focused_project_root(cx) else {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        "no project is in focus in this window: open a session or a project \
                         first (`workbench::OpenSession`, `workbench::OpenWorkspace`)",
                    ));
                };
                self.settings
                    .write_project(&root, &key_path, value)
                    .map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
            } else {
                if self.workspace_root.is_none() {
                    return Err(ErrorBody::new(
                        ErrorCode::InvalidState,
                        "no workspace folder is active in this window: open a workspace with a \
                         folder first (`workbench::OpenWorkspace`)",
                    ));
                }
                self.settings
                    .write_workspace(&key_path, value)
                    .map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
            }
        }
        Ok(json!({
            "namespace": req.namespace,
            "key": req.key,
            "layer": req.layer,
            "value": req.value,
        }))
    }
}

/// The serde name of a plain enum (`Axis::Horizontal` → `horizontal`).
fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_are_named_for_humans() {
        assert_eq!(humanize("workbench::MoveActivePanel"), "Move Active Panel");
        assert_eq!(humanize("SplitRight"), "Split Right");
    }

    #[test]
    fn values_are_checked_against_the_field_kind() {
        assert!(check_value(&FieldKind::Bool, &json!(true)).is_ok());
        assert!(check_value(&FieldKind::Bool, &json!("true")).is_err());
        let choice = FieldKind::Enum(vec!["block".into(), "bar".into()]);
        assert!(check_value(&choice, &json!("bar")).is_ok());
        let err = check_value(&choice, &json!("beam")).unwrap_err();
        assert!(err.contains("block, bar"), "{err}");
        assert!(check_value(&FieldKind::Integer, &json!(13)).is_ok());
        assert!(check_value(&FieldKind::Integer, &json!(1.5)).is_err());
        assert!(check_value(&FieldKind::Number, &json!(1.5)).is_ok());
        assert!(check_value(&FieldKind::String, &json!(1)).is_err());
        assert!(check_value(&FieldKind::StringList, &json!(["a", "b"])).is_ok());
        assert!(check_value(&FieldKind::StringList, &json!(["a", 1])).is_err());
        assert!(check_value(&FieldKind::Unsupported("map".into()), &json!({})).is_err());
    }

    #[test]
    fn the_daemon_settings_are_not_writable_here() {
        for key in ["keep_running", "start_at_login"] {
            let kind = writable_kind("app", key, &FieldKind::Bool);
            assert!(matches!(kind, FieldKind::Unsupported(_)), "{key}");
        }
        assert_eq!(
            writable_kind("app", "setup_done", &FieldKind::Bool),
            FieldKind::Bool
        );
        assert_eq!(
            writable_kind("terminal", "font_size", &FieldKind::Integer),
            FieldKind::Integer
        );
    }

    #[test]
    fn choice_schemas_are_json_schema_objects() {
        let schema = choice_schema("name", "the preset", vec!["default".into(), "focus".into()]);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["name"]));
        assert_eq!(
            schema["properties"]["name"]["enum"],
            json!(["default", "focus"])
        );
    }
}
