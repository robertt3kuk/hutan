//! Agent control surface (protocol 0.21, capability `control`, ADR 0016): the desktop publishes
//! its action catalog, settings catalog and layout; the daemon routes `ui.run` /
//! `ui.set_setting` to it as the event `ui.command` and hands the answer (`ui.command_result`)
//! back to the caller. Layout stays desktop state (ADR 0010): the daemon only caches the last
//! published summary.

use hutan_core::{SessionId, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::ErrorBody;

/// Reserved `ui.command` action of `ui.set_setting`: the desktop writes a settings layer. It is
/// not part of the published catalog.
pub const SET_SETTING_ACTION: &str = "ui::SetSetting";

/// How long the daemon waits for a desktop to answer a `ui.command`, in milliseconds.
pub const COMMAND_TIMEOUT_MS: u64 = 5000;

/// One thing a window can do: the id of a palette command (`workbench::SplitRight`) or of a
/// parameterised action (`workbench::ApplyPreset` with `args_schema`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiAction {
    /// The GPUI action name.
    pub id: String,
    /// What the palette shows.
    pub title: String,
    /// A JSON Schema (`type: object`) of the arguments the action takes; absent for an action
    /// without arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_schema: Option<Value>,
}

/// One settings key the window can change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSetting {
    /// `terminal`, `panels.sessions`, …
    pub namespace: String,
    /// Dotted path inside the namespace (`cursor.style`).
    pub key: String,
    /// `bool`, `enum`, `integer`, `number`, `string`, `string_list`, or `unsupported` (shown, but
    /// not writable through `ui.set_setting`).
    #[serde(rename = "type")]
    pub kind: String,
    pub doc: String,
    /// The settings layers a value can be written to: `user`, and `workspace` / `project` for a
    /// namespace that a workspace may override.
    pub layers: Vec<String>,
    /// The allowed values of an `enum`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// The effective value right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Value>,
}

/// `ui.publish`: the desktop's catalog and layout. Sent on connect and whenever any of it
/// changes (debounced 250 ms); each publish replaces the previous one of the same
/// `layout.window_id`. Answered with `Empty`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiPublish {
    pub actions: Vec<UiAction>,
    pub settings: Vec<UiSetting>,
    pub layout: LayoutSummary,
}

/// What a window shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSummary {
    pub window_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// The preset applied last.
    pub preset: String,
    pub docks: Vec<DockSummary>,
    /// The center: a tree of splits and panes.
    pub panes: PaneSummary,
    pub focused: FocusSummary,
    /// The zoomed pane, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<String>,
    pub appearance: AppearanceSummary,
    /// The window is the active (frontmost) one. `ui.run` without a `window_id` goes to the
    /// window that was active most recently.
    #[serde(default)]
    pub window_active: bool,
}

/// A panel stack at a window edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockSummary {
    /// `left`, `right` or `bottom`.
    pub position: String,
    pub visible: bool,
    /// Panel ids, in tab order.
    pub panels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

/// The center's split tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum PaneSummary {
    /// `axis` is `horizontal` (children side by side) or `vertical` (stacked).
    Split {
        axis: String,
        children: Vec<PaneSummary>,
    },
    Pane {
        pane_id: String,
        items: Vec<PaneItem>,
        /// Index into `items`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active: Option<u32>,
    },
}

/// A tab of a pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneItem {
    /// `terminal`, `chat`, `panel`, `placeholder`, `changes` (the Changes view of a checkout,
    /// titled `Changes · <branch>`) or `file` (a file in the code view, titled with its name).
    pub kind: String,
    /// The session of a `terminal` or `chat` tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// The panel id of a `panel` tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    /// Protocol 0.35 (capability `finish`): the canonical absolute path of a `file` tab's file
    /// (what `ui.run OpenFile` takes); absent for every other kind and from older desktops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<std::path::PathBuf>,
    pub title: String,
}

/// What has keyboard focus: a center pane or a dock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FocusSummary {
    /// `pane` or `dock`.
    pub kind: String,
    /// The pane id, or the dock position.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppearanceSummary {
    /// `light`, `dark`, `system`, …: the chrome setting.
    pub chrome: String,
    pub density: String,
}

/// `ui.actions` and `ui.layout`: which window; absent means the one that was active most
/// recently.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UiTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
}

/// `ui.run`: run an action of the catalog. `args` is an object matching the action's
/// `args_schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiRun {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// `ui.set_setting`: write `value` to `layer` of `namespace.key`; the desktop validates it
/// against the settings schema and hot reload applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiSetSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    pub namespace: String,
    pub key: String,
    pub value: Value,
    /// `user`, `workspace` or `project`.
    pub layer: String,
}

/// `ui.command_result`: the desktop's answer to a `ui.command` event. Exactly one of `ok` and
/// `error`. Answered with `Empty`; a request id that is unknown (the daemon gave up waiting)
/// is `not_found`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiCommandResult {
    pub request_id: u64,
    /// The command ran; what it did, as JSON (an object; `{}` when there is nothing to say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// Result of `ui.actions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiActionList {
    pub window_id: String,
    pub actions: Vec<UiAction>,
    pub settings: Vec<UiSetting>,
}

/// Result of `ui.layout`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiLayoutResult {
    pub layout: LayoutSummary,
}

/// Result of `ui.run` and `ui.set_setting`: the window that ran it and what it answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiRan {
    pub window_id: String,
    #[serde(default)]
    pub result: Value,
}

/// `session.send_text`: types `text` into a live terminal session (and presses enter with
/// `enter`). The control surface's counterpart of terminal input frames, which carry no
/// response and cannot wait for an approval. A chat session takes `chat.send` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSendText {
    pub session_id: SessionId,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enter: bool,
}

/// Result of `session.send_text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTextSent {
    pub bytes: u64,
}
