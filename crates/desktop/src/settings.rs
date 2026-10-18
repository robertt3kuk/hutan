//! The desktop's settings namespaces (design.md § Settings): `app`, `terminal`, `layout`,
//! `panels.sessions`, `panels.inspector`, `panels.overview`, `workspaces`, `history`, `chat`,
//! `editor`, `agents`.
//! `appearance` lives in `hutan-settings`.
//!
//! Numbers are `f64` so values read back from TOML and the schema defaults stay free of `f32`
//! rounding noise (`1.2`, not `1.2000000476837158`).

use std::collections::BTreeMap;
use std::path::Path;

use hutan_protocol::messages::TerminalSpec;
pub use hutan_settings::AppearanceSettings;
use hutan_settings::themes::{self, SystemAppearance};
use hutan_settings::{Setting, SettingsStore};
use hutan_terminal_view::{
    BlockSettings, CursorSettings, CursorStyle, OptionAsMeta, Rgb, TerminalPalette,
    TerminalViewSettings,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::workbench::StatusBarLayout;
use crate::workbench::model::{CenterLayout, Dock, Docks, LayoutPreset, PanelId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyleSetting {
    #[default]
    Block,
    Bar,
    Underline,
}

/// `terminal.cursor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct CursorConfig {
    /// `block`, `bar` or `underline`; applications that ask for a shape win.
    pub style: CursorStyleSetting,
    /// Whether the cursor blinks while the terminal has focus.
    pub blink: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            style: CursorStyleSetting::Block,
            blink: true,
        }
    }
}

/// Which Option key acts as Meta. GPUI cannot tell the two keys apart, so every value except
/// `none` makes both act as Meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsMetaSetting {
    None,
    #[default]
    Left,
    Right,
    Both,
}

/// What the × of a terminal tab does (`terminal.tab_close`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TabClose {
    /// Closes the tab only: the session keeps running and stays in the sidebar.
    #[default]
    Detach,
    /// Closes the session (the sidebar's × does the same): asks first when something runs in
    /// it, and Reopen in History brings it back.
    CloseSession,
}

/// The `terminal` namespace: font, theme and behavior of every terminal view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TerminalSettings {
    /// Font family of terminal text (monospace).
    pub font_family: String,
    /// Font size in points.
    pub font_size: f64,
    /// Line height as a multiple of the font size.
    pub line_height: f64,
    /// `auto` (follows the system appearance), `hutan-dark`, `hutan-light`.
    pub theme: String,
    pub cursor: CursorConfig,
    pub option_as_meta: OptionAsMetaSetting,
    /// Copy the selection to the clipboard when the mouse is released.
    pub copy_on_select: bool,
    /// Jump back to the live screen when typing while scrolled into history.
    pub scroll_to_bottom_on_input: bool,
    /// Draw prompt blocks over the terminal: gutter bars, status chips, hover actions, folds.
    /// Off, the terminal looks as if blocks did not exist.
    pub blocks: bool,
    /// The shell new terminals run (absolute path or a name on `PATH`); empty = the account's
    /// login shell. Sent to the daemon as the session's `SHELL`.
    pub shell: String,
    /// Extra environment variables of new terminals, merged over the daemon's login environment
    /// (`[terminal] env_inherit` in `daemon.toml`). A workspace's `terminal.env` overrides the
    /// user's per variable. Values may be secrets: they go to the daemon and nowhere else.
    pub env: BTreeMap<String, String>,
    /// What the × of a terminal tab does: `detach` (the session keeps running) or
    /// `close_session`.
    pub tab_close: TabClose,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "Menlo".to_owned(),
            font_size: 13.0,
            line_height: 1.2,
            theme: themes::AUTO.to_owned(),
            cursor: CursorConfig::default(),
            option_as_meta: OptionAsMetaSetting::Left,
            copy_on_select: false,
            scroll_to_bottom_on_input: true,
            blocks: true,
            shell: String::new(),
            env: BTreeMap::new(),
            tab_close: TabClose::Detach,
        }
    }
}

impl Setting for TerminalSettings {
    const NAMESPACE: &'static str = "terminal";
    const WORKSPACE_OVERRIDABLE: bool = true;
}

fn rgb(c: themes::Rgb) -> Rgb {
    Rgb(c.r, c.g, c.b)
}

impl TerminalSettings {
    /// The font sizes (points) a terminal view accepts; larger or smaller values are clamped.
    pub const MIN_FONT_SIZE: f64 = 4.0;
    pub const MAX_FONT_SIZE: f64 = 72.0;
    /// The line heights (multiples of the font size) a terminal view accepts.
    pub const MIN_LINE_HEIGHT: f64 = 0.8;
    pub const MAX_LINE_HEIGHT: f64 = 3.0;

    /// Puts `shell` and `env` into the session `spec` a new terminal is created with: `env`, and
    /// a non-empty `shell` as `SHELL` (over `env.SHELL`). Variables `spec` already carries win
    /// over the settings; an explicit `SHELL` in `spec.env` is what the daemon then runs (its
    /// `default_shell` rule).
    pub fn apply_to_spec(&self, spec: &mut TerminalSpec) {
        let shell = (!self.shell.is_empty()).then_some(("SHELL", self.shell.as_str()));
        let settings = self
            .env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .filter(|(name, _)| shell.is_none() || *name != "SHELL")
            .chain(shell);
        for (name, value) in settings {
            spec.env
                .entry(name.to_owned())
                .or_insert_with(|| value.to_owned());
        }
    }

    /// The view configuration for this namespace with the theme resolved against `system`.
    pub fn view_settings(&self, system: SystemAppearance) -> TerminalViewSettings {
        let theme = themes::resolve(&self.theme, system);
        let mut ansi = [Rgb(0, 0, 0); 16];
        for (slot, color) in ansi.iter_mut().zip(theme.ansi) {
            *slot = rgb(color);
        }
        TerminalViewSettings {
            font_family: self.font_family.clone(),
            font_size: self
                .font_size
                .clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE) as f32,
            line_height: self
                .line_height
                .clamp(Self::MIN_LINE_HEIGHT, Self::MAX_LINE_HEIGHT)
                as f32,
            palette: TerminalPalette {
                ansi,
                foreground: rgb(theme.foreground),
                background: rgb(theme.background),
                cursor: rgb(theme.cursor),
                selection: rgb(theme.selection),
            },
            cursor: CursorSettings {
                style: match self.cursor.style {
                    CursorStyleSetting::Block => CursorStyle::Block,
                    CursorStyleSetting::Bar => CursorStyle::Bar,
                    CursorStyleSetting::Underline => CursorStyle::Underline,
                },
                blink: self.cursor.blink,
            },
            option_as_meta: match self.option_as_meta {
                OptionAsMetaSetting::None => OptionAsMeta::None,
                OptionAsMetaSetting::Left => OptionAsMeta::Left,
                OptionAsMetaSetting::Right => OptionAsMeta::Right,
                OptionAsMetaSetting::Both => OptionAsMeta::Both,
            },
            copy_on_select: self.copy_on_select,
            scroll_to_bottom_on_input: self.scroll_to_bottom_on_input,
            // Padding follows the density; the workbench fills it in.
            padding: 0.0,
            // The gutter width, colors and motion follow the density and the chrome theme; the
            // workbench fills them in.
            blocks: BlockSettings {
                enabled: self.blocks,
                ..BlockSettings::default()
            },
        }
    }
}

/// `layout.presets.<name>.left` and friends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DockSetting {
    /// Panel ids stacked in the dock, e.g. `["sessions"]`.
    pub panels: Vec<String>,
    /// Width (left/right) or height (bottom) in pixels.
    pub size: f64,
    pub visible: bool,
}

impl Default for DockSetting {
    fn default() -> Self {
        Self {
            panels: Vec::new(),
            size: 240.0,
            visible: false,
        }
    }
}

impl DockSetting {
    fn from_dock(dock: &Dock) -> Self {
        Self {
            panels: dock.panels.iter().map(|p| p.to_string()).collect(),
            size: f64::from(dock.size),
            visible: dock.visible,
        }
    }

    fn to_dock(&self) -> Dock {
        let panels: Vec<&str> = self.panels.iter().map(String::as_str).collect();
        Dock::new(&panels, self.size as f32, self.visible)
    }
}

/// Status items per side, by id (`connection`, `sessions`, `attention`, `density`, `theme`,
/// `palette_hint`, and the dock buttons `dock_left`, `dock_bottom`, `dock_right`); unknown ids
/// are skipped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatusBarSetting {
    pub left: Vec<String>,
    pub right: Vec<String>,
}

impl Default for StatusBarSetting {
    fn default() -> Self {
        let layout = StatusBarLayout::default();
        Self {
            left: layout.left,
            right: layout.right,
        }
    }
}

/// One entry of `layout.presets`: docks and status bar. The center keeps the current panes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PresetSetting {
    pub left: DockSetting,
    pub right: DockSetting,
    pub bottom: DockSetting,
    pub status_bar: StatusBarSetting,
}

/// The `layout` namespace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LayoutSettings {
    /// Preset applied on a fresh start and whenever this value changes.
    pub preset: String,
    /// User presets by name; `default`, `focus` and `overview` always exist and can be
    /// overridden here. Edit `settings.toml` to change these.
    pub presets: BTreeMap<String, PresetSetting>,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            preset: LayoutPreset::DEFAULT.to_owned(),
            presets: BTreeMap::new(),
        }
    }
}

impl Setting for LayoutSettings {
    const NAMESPACE: &'static str = "layout";
}

/// A layout preset plus the status bar it shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetDef {
    pub layout: LayoutPreset,
    pub status_bar: StatusBarLayout,
}

impl LayoutSettings {
    /// Built-in presets followed by user presets; a user preset named like a built-in replaces
    /// its docks and status bar (the built-in's center behavior stays).
    pub fn preset_defs(&self) -> Vec<PresetDef> {
        let mut defs: Vec<PresetDef> = LayoutPreset::builtin()
            .into_iter()
            .map(|layout| PresetDef {
                layout,
                status_bar: StatusBarLayout::default(),
            })
            .collect();
        for (name, setting) in &self.presets {
            let docks = Docks {
                left: setting.left.to_dock(),
                right: setting.right.to_dock(),
                bottom: setting.bottom.to_dock(),
            };
            let status_bar = StatusBarLayout {
                left: setting.status_bar.left.clone(),
                right: setting.status_bar.right.clone(),
            };
            match defs.iter_mut().find(|d| d.layout.name == *name) {
                Some(existing) => {
                    existing.layout.docks = docks;
                    existing.status_bar = status_bar;
                }
                None => defs.push(PresetDef {
                    layout: LayoutPreset {
                        name: name.clone(),
                        docks,
                        center: CenterLayout::Keep,
                        zoom_active: false,
                    },
                    status_bar,
                }),
            }
        }
        defs
    }

    /// The setting that reproduces `layout` and `status_bar` (for "Save layout as preset…").
    pub fn preset_from(docks: &Docks, status_bar: &StatusBarLayout) -> PresetSetting {
        PresetSetting {
            left: DockSetting::from_dock(&docks.left),
            right: DockSetting::from_dock(&docks.right),
            bottom: DockSetting::from_dock(&docks.bottom),
            status_bar: StatusBarSetting {
                left: status_bar.left.clone(),
                right: status_bar.right.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    #[default]
    Workspace,
    State,
    None,
}

/// The `panels.sessions` namespace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SessionsPanelSettings {
    /// `workspace`, `state` or `none`.
    pub group_by: GroupBy,
    /// Show the "Needs you" section on top.
    pub needs_you: bool,
    /// Fields of a row, in order: `state`, `kind`, `title`, `age` on the first line, `folder`
    /// (where the session is now) and `placement` (`main` / `wt <branch>`) on the second.
    pub row: Vec<String>,
}

impl Default for SessionsPanelSettings {
    fn default() -> Self {
        Self {
            group_by: GroupBy::Workspace,
            needs_you: true,
            row: ["state", "kind", "title", "folder", "placement", "age"]
                .map(str::to_owned)
                .to_vec(),
        }
    }
}

impl Setting for SessionsPanelSettings {
    const NAMESPACE: &'static str = "panels.sessions";
}

/// How the sidebar organizes sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SidebarOrganize {
    /// User workspaces → their projects → sessions, then the projects in no workspace, then
    /// Home.
    #[default]
    Workspaces,
    /// A flat list of projects (pinned first) → sessions, then Home.
    Projects,
}

impl SidebarOrganize {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            SidebarOrganize::Workspaces => "workspaces",
            SidebarOrganize::Projects => "projects",
        }
    }

    /// The mode as the sidebar menu shows it.
    pub fn label(self) -> &'static str {
        match self {
            SidebarOrganize::Workspaces => "Workspaces → Projects",
            SidebarOrganize::Projects => "Projects",
        }
    }
}

/// The `sidebar` namespace: how the Projects panel organizes what the daemon knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SidebarSettings {
    /// `workspaces` (user workspaces → their projects → sessions) or `projects` (a flat list of
    /// projects → sessions).
    pub organize: SidebarOrganize,
}

impl Setting for SidebarSettings {
    const NAMESPACE: &'static str = "sidebar";
}

/// The `panels.inspector` namespace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct InspectorPanelSettings {
    /// Tabs shown, in order: `approvals`, `changes`, `info`.
    pub tabs: Vec<String>,
    /// Reveal the inspector when an approval arrives.
    pub reveal_on_approval: bool,
}

impl Default for InspectorPanelSettings {
    fn default() -> Self {
        Self {
            tabs: ["approvals", "changes", "info"].map(str::to_owned).to_vec(),
            reveal_on_approval: true,
        }
    }
}

impl Setting for InspectorPanelSettings {
    const NAMESPACE: &'static str = "panels.inspector";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CardSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl CardSize {
    /// Card width and terminal preview height in pixels.
    pub fn dimensions(self) -> (f32, f32) {
        match self {
            CardSize::Small => (220.0, 110.0),
            CardSize::Medium => (300.0, 160.0),
            CardSize::Large => (420.0, 240.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PreviewMode {
    /// Attach every running session and draw its terminal live.
    #[default]
    Live,
    /// Draw only terminals that are attached anyway (open in a pane); never attach for a card.
    Static,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    /// Sessions that need you first.
    #[default]
    Attention,
    /// Newest first.
    Recent,
    Name,
}

/// The `panels.overview` namespace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OverviewPanelSettings {
    /// `small`, `medium` or `large`.
    pub card_size: CardSize,
    /// `live`, `static` or `off`.
    pub preview: PreviewMode,
    /// `attention`, `recent` or `name`.
    pub sort: SortOrder,
}

impl Setting for OverviewPanelSettings {
    const NAMESPACE: &'static str = "panels.overview";
}

/// How the sidebar orders workspaces, and the projects inside each level (pinned ones always
/// come first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSort {
    /// The order the user dragged them into.
    #[default]
    Manual,
    /// Most recent session activity first.
    Recent,
    /// Workspaces with sessions that need you first, then by activity.
    Attention,
    /// Alphabetical.
    Name,
}

impl WorkspaceSort {
    pub const ALL: [WorkspaceSort; 4] = [
        WorkspaceSort::Manual,
        WorkspaceSort::Recent,
        WorkspaceSort::Attention,
        WorkspaceSort::Name,
    ];

    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            WorkspaceSort::Manual => "manual",
            WorkspaceSort::Recent => "recent",
            WorkspaceSort::Attention => "attention",
            WorkspaceSort::Name => "name",
        }
    }

    /// The mode as the sort menu shows it.
    pub fn label(self) -> &'static str {
        match self {
            WorkspaceSort::Manual => "Manual",
            WorkspaceSort::Recent => "Recent activity",
            WorkspaceSort::Attention => "Attention first",
            WorkspaceSort::Name => "Name",
        }
    }
}

/// The `workspaces` namespace: how the sidebar sorts workspaces and projects, and which
/// projects the filters hide. Workspaces, Home and pinned projects are never hidden by the
/// filters; hidden projects stay reachable through ⌘J.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WorkspacesSettings {
    /// `manual`, `recent`, `attention` or `name`.
    pub sort: WorkspaceSort,
    /// Hide projects without any session.
    pub hide_empty: bool,
    /// Hide projects without a live session.
    pub hide_sleeping: bool,
    /// Hide projects the `hutan` CLI (or an agent) created.
    pub hide_cli_created: bool,
    /// Keep the project and workspace you are in visible whatever the filters say.
    pub always_show_active: bool,
}

impl Default for WorkspacesSettings {
    fn default() -> Self {
        Self {
            sort: WorkspaceSort::Manual,
            hide_empty: true,
            hide_sleeping: false,
            hide_cli_created: false,
            always_show_active: true,
        }
    }
}

impl Setting for WorkspacesSettings {
    const NAMESPACE: &'static str = "workspaces";
}

/// How far back the History panel searches by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum HistoryRange {
    /// The last 24 hours.
    #[serde(rename = "24h")]
    Day,
    /// The last 7 days.
    #[serde(rename = "7d")]
    Week,
    /// The last 30 days.
    #[serde(rename = "30d")]
    Month,
    /// Everything the index holds.
    #[default]
    #[serde(rename = "all")]
    All,
}

impl HistoryRange {
    pub const ALL: [HistoryRange; 4] = [
        HistoryRange::Day,
        HistoryRange::Week,
        HistoryRange::Month,
        HistoryRange::All,
    ];

    /// The value in `settings.toml` and on the chip.
    pub fn key(self) -> &'static str {
        match self {
            HistoryRange::Day => "24h",
            HistoryRange::Week => "7d",
            HistoryRange::Month => "30d",
            HistoryRange::All => "all",
        }
    }

    /// Length of the window in milliseconds; `None` for everything.
    pub fn window_ms(self) -> Option<u64> {
        const HOUR: u64 = 3_600_000;
        match self {
            HistoryRange::Day => Some(24 * HOUR),
            HistoryRange::Week => Some(7 * 24 * HOUR),
            HistoryRange::Month => Some(30 * 24 * HOUR),
            HistoryRange::All => None,
        }
    }
}

/// Which projects the History panel searches by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HistoryScope {
    /// Only sessions that ran inside the active workspace's first folder.
    Workspace,
    /// Every project.
    #[default]
    All,
}

/// The `history` namespace (desktop side of the session history index; whether indexing runs
/// and where the transcripts are is the daemon's `[history]` section in `daemon.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HistorySettings {
    /// Sources the panel searches by default: any of `hutan`, `claude`, `codex`, `omp`; empty
    /// means all of them.
    pub sources: Vec<String>,
    /// Roles the panel searches by default: any of `user`, `assistant`, `tool`, `command`,
    /// `output`; empty means all of them.
    pub roles: Vec<String>,
    /// How far back to search: `24h`, `7d`, `30d` or `all`.
    pub range: HistoryRange,
    /// `workspace` (the active workspace) or `all` projects.
    pub project: HistoryScope,
    /// Show the matching text under each result.
    pub show_snippets: bool,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            roles: Vec::new(),
            range: HistoryRange::All,
            project: HistoryScope::All,
            show_snippets: true,
        }
    }
}

impl Setting for HistorySettings {
    const NAMESPACE: &'static str = "history";
}

/// The `chat` namespace: the agent chat view's composer and what "Send to agent" attaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChatSettings {
    /// Whether Return sends the message (Shift-Return then inserts a newline). Off, Return
    /// inserts a newline and ⌘-Return sends.
    pub send_on_enter: bool,
    /// Most bytes of a terminal block's output that "Send to agent" attaches to the composer;
    /// longer output is cut (the chip says so).
    pub attach_max_bytes: u64,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            send_on_enter: false,
            attach_max_bytes: 64 * 1024,
        }
    }
}

impl Setting for ChatSettings {
    const NAMESPACE: &'static str = "chat";
}

/// The `editor` namespace: how Hutan's text inputs (the chat composer, the palette, prompts and
/// the other single-line fields) edit. Terminals are not text inputs: programs there do their
/// own thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EditorSettings {
    /// Vim's modal editing (Normal, Insert, Visual) in text inputs. Fields open in Insert; Escape
    /// goes to Normal, and a second Escape does what it always did (closes the palette, …).
    pub vim_mode: bool,
    /// Yanks and deletes in Vim mode also go to the system clipboard, and `p` pastes from it.
    /// Off, they use a register private to the field.
    pub vim_use_system_clipboard: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            vim_mode: false,
            vim_use_system_clipboard: true,
        }
    }
}

impl Setting for EditorSettings {
    const NAMESPACE: &'static str = "editor";
}

/// Which sessions the agent monitor lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsScope {
    /// Only sessions of the workspace the window shows.
    #[default]
    ActiveWorkspace,
    /// Every session that has agents.
    All,
}

impl AgentsScope {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            AgentsScope::ActiveWorkspace => "active_workspace",
            AgentsScope::All => "all",
        }
    }
}

/// The `agents` namespace: what the agent monitor panel lists. Blocked nodes are always listed.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AgentsSettings {
    /// List agents that have finished (dimmed, their subtree folded into one row). Off, only
    /// agents that are still around (and the ones above them) are listed.
    pub show_finished: bool,
    /// List only blocked agents (and the ones above them).
    pub only_blocked: bool,
    /// `active_workspace` or `all`.
    pub scope: AgentsScope,
}

impl Setting for AgentsSettings {
    const NAMESPACE: &'static str = "agents";
}

/// What the resources panel sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourcesSort {
    /// Busiest first.
    #[default]
    Cpu,
    /// Largest resident memory first.
    Memory,
    /// A to Z.
    Name,
}

impl ResourcesSort {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            ResourcesSort::Cpu => "cpu",
            ResourcesSort::Memory => "memory",
            ResourcesSort::Name => "name",
        }
    }
}

/// Which tasks the Tasks panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TasksScope {
    /// Only tasks of the workspace the window shows.
    #[default]
    Workspace,
    /// The tasks of every workspace.
    All,
}

impl TasksScope {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            TasksScope::Workspace => "workspace",
            TasksScope::All => "all",
        }
    }
}

/// The `resources` namespace: the resources panel (processes per session, CPU and memory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ResourcesSettings {
    /// How often the daemon samples the process table while the panel is visible, in
    /// milliseconds (at least 1000). Nothing is sampled while the panel is hidden.
    pub interval_ms: u32,
    /// `cpu`, `memory` or `name`: how sessions, and processes among their siblings, are ordered.
    /// The column headers change it.
    pub sort: ResourcesSort,
}

impl Default for ResourcesSettings {
    fn default() -> Self {
        Self {
            interval_ms: 2000,
            sort: ResourcesSort::Cpu,
        }
    }
}

/// The `tasks` namespace: what the Tasks panel lists (`docs/design.md` § Tasks panel).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TasksSettings {
    /// List tasks that are done or cancelled. Off, they are hidden and the tasks that waited
    /// for them move up (failed tasks always stay).
    pub show_done: bool,
    /// `workspace` or `all`.
    pub scope: TasksScope,
}

impl Default for TasksSettings {
    fn default() -> Self {
        Self {
            show_done: true,
            scope: TasksScope::Workspace,
        }
    }
}

impl Setting for ResourcesSettings {
    const NAMESPACE: &'static str = "resources";
}

impl Setting for TasksSettings {
    const NAMESPACE: &'static str = "tasks";
}

/// What closing the last window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LastWindowClose {
    /// Quit Hutan exactly like ⌘Q.
    #[default]
    Quit,
    /// Keep Hutan in the menu bar (no Dock icon) until a window is opened again or Hutan is
    /// quit. Needs `app.menu_bar`.
    MenuBar,
}

impl LastWindowClose {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            LastWindowClose::Quit => "quit",
            LastWindowClose::MenuBar => "menu_bar",
        }
    }
}

/// The `app` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AppSettings {
    /// Keep the daemon running after Hutan quits: quitting leaves `hutand` and its sessions
    /// running, without asking and without any login item. It stops when you stop it (Daemon
    /// menu, `hutan daemon stop`) or when you log out. Off (the default), the daemon runs while
    /// Hutan runs and stops when it quits, like Orca or Paseo.
    pub keep_running: bool,
    /// Start Hutan's daemon when you log in: registers the bundled launch agent (macOS Login
    /// Items) so `hutand` starts at login and is restarted after a crash. While it is
    /// registered and allowed, quitting Hutan never stops the daemon. Off by default; only
    /// available when the app runs from Hutan.app. A `settings.toml` written before this key
    /// existed with `keep_running = true` (which then meant this) is migrated when it is first
    /// read.
    pub start_at_login: bool,
    /// The first-run setup has been completed or skipped. Off shows it once; "Run setup
    /// again" (Settings, the command palette's "Hutan: Run Setup…") opens it whatever this says.
    pub setup_done: bool,
    /// When the last window closes: `quit` (Quit Hutan, like ⌘Q: asks first while sessions run
    /// and keep running is off) or `menu_bar` (keep Hutan in the menu bar, without a Dock icon;
    /// the daemon keeps running until you quit; needs `menu_bar`).
    pub on_last_window_close: LastWindowClose,
    /// Show a menu bar item with the daemon state, a count of the sessions that need you and a
    /// menu to jump to them. Hides this item only, and only while Hutan runs: when no Hutan is
    /// connected to the daemon (it keeps running after Hutan quit) the daemon shows an item of
    /// its own, which is not optional.
    pub menu_bar: bool,
    /// Show the number of sessions that need you (blocked, waiting for an approval, failed and
    /// unread) as a badge on the Dock icon.
    pub dock_badge: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            keep_running: false,
            start_at_login: false,
            setup_done: false,
            on_last_window_close: LastWindowClose::Quit,
            menu_bar: true,
            dock_badge: true,
        }
    }
}

impl AppSettings {
    /// What closing the last window really does: `menu_bar` needs the menu bar item (nothing
    /// would be left to reopen Hutan from), so without it the window close quits.
    pub fn effective_last_window_close(&self) -> LastWindowClose {
        match self.on_last_window_close {
            LastWindowClose::MenuBar if self.menu_bar => LastWindowClose::MenuBar,
            _ => LastWindowClose::Quit,
        }
    }

    /// The diagnostic for a setting that cannot take effect, if any.
    pub fn diagnostics(&self) -> Vec<hutan_settings::SettingsError> {
        if self.on_last_window_close == LastWindowClose::MenuBar && !self.menu_bar {
            return vec![hutan_settings::SettingsError::warning(
                None,
                "app.on_last_window_close",
                "\"menu_bar\" needs app.menu_bar = true (nothing would be left to reopen Hutan \
                 from); closing the last window quits instead",
            )];
        }
        Vec::new()
    }
}

impl Setting for AppSettings {
    const NAMESPACE: &'static str = "app";
}

/// Settings written before `app.start_at_login` existed: `app.keep_running = true` meant "the
/// launch agent is registered" (it started the daemon at login). Such a file gets
/// `start_at_login = true` too, so that what the user had keeps working; `keep_running` stays
/// on because the daemon outliving the app is what it always did. A file that names
/// `start_at_login` (whatever the value) has been written by a Hutan that knows both and is
/// left alone, so this runs once. `user_file` is the file `store` reads. `true` when the file
/// was migrated. Nothing is registered by this: the launch agent already is.
pub fn migrate_start_at_login(store: &SettingsStore, user_file: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(user_file) else {
        return false;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return false;
    };
    let Some(app) = table.get("app").and_then(toml::Value::as_table) else {
        return false;
    };
    if app.get("keep_running").and_then(toml::Value::as_bool) != Some(true)
        || app.contains_key("start_at_login")
    {
        return false;
    }
    if let Err(error) = store.write_user("app.start_at_login", toml::Value::Boolean(true)) {
        tracing::warn!("cannot migrate app.keep_running to app.start_at_login: {error}");
        return false;
    }
    store.set_notes(
        MIGRATION_NOTE,
        vec![hutan_settings::SettingsError::warning(
            Some(user_file),
            "app.start_at_login",
            "app.keep_running used to start the daemon at login. That is now app.start_at_login, \
             which was set to true to keep your setup; app.keep_running only leaves the daemon \
             running after Hutan quits",
        )],
    );
    true
}

/// `assistant.enabled` used to default to true and is off for new users now. A user who already
/// had Hutan (a layout saved by the version before the Orca-style relayout, `version` 1 in
/// `desktop-state.json`) and never named the key keeps what they had: `assistant.enabled = true`
/// is written. A user file that names the key (whatever the value) is left alone; a user with
/// no saved layout, or one written by a newer Hutan, is new. Run before the layout is loaded
/// and saved again. `true` when the key was written.
pub fn migrate_assistant_default(
    store: &SettingsStore,
    user_file: &Path,
    state_file: &Path,
) -> bool {
    let saved_by_old_hutan = std::fs::read_to_string(state_file)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.get("version").and_then(serde_json::Value::as_u64))
        == Some(1);
    if !saved_by_old_hutan {
        return false;
    }
    let names_key = std::fs::read_to_string(user_file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| {
            table
                .get("assistant")
                .and_then(toml::Value::as_table)
                .map(|assistant| assistant.contains_key("enabled"))
        })
        .unwrap_or(false);
    if names_key {
        return false;
    }
    if let Err(error) = store.write_user("assistant.enabled", toml::Value::Boolean(true)) {
        tracing::warn!("cannot keep assistant.enabled on for your existing setup: {error}");
        return false;
    }
    true
}

/// The diagnostics source of [`migrate_start_at_login`]'s note.
const MIGRATION_NOTE: &str = "app.migration";

/// The `assistant` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AssistantSettings {
    /// Whether Hutan has an assistant at all. Off (the default for a new settings file) hides the
    /// Assistant page of the left sidebar, removes its commands from the palette and
    /// unregisters the system-wide shortcut; the daemon keeps its assistant conversations. On
    /// adds *Assistant* to the left sidebar's nav; the page opens inside the left sidebar and
    /// never in the right dock or the center. A settings file written before the default
    /// changed keeps the assistant on (see [`migrate_assistant_default`]).
    pub enabled: bool,
    /// System-wide shortcut that brings Hutan forward and opens the Assistant, for example
    /// `alt-space` or `cmd-shift-space` (modifiers `cmd`, `ctrl`, `alt`, `shift`, at least one
    /// of the first three). Empty (the default) turns it off. If another app already owns the shortcut,
    /// Hutan says so once and leaves the setting alone; the in-app keybinding is separate.
    pub global_shortcut: String,
}

impl Setting for AssistantSettings {
    const NAMESPACE: &'static str = "assistant";
}

/// The `integrations` namespace: Hutan's hooks in the agents (`docs/integrations.md`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct IntegrationsSettings {
    /// Install Hutan's hooks into Codex, Claude Code and OMP every time Hutan starts: for each
    /// of them that is installed and whose integration is not (`hutan integration install`,
    /// idempotent, only Hutan-marked entries are written, the files are backed up first). The
    /// hooks do nothing outside a Hutan session. **Off by default**: the hooks go into the
    /// agents' own configuration (`~/.codex`, `~/.claude`, `~/.omp/agent`, even with a custom
    /// `HUTAN_HOME`), so Hutan writes there only after you asked, once in the setup's Agents
    /// step (Install hooks for …) or here for every start. Turning this off stops future
    /// installs; it does not remove what is installed (`hutan integration uninstall`, or the
    /// setup's Agents step).
    pub auto_install: bool,
}

impl Setting for IntegrationsSettings {
    const NAMESPACE: &'static str = "integrations";
}

/// The `launcher` namespace: the Start… sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LauncherSettings {
    /// Ask once before an agent chat that starts with a prompt sends it to a model: the first
    /// time for each agent and model, with "Don't ask again for this model" to stop asking for
    /// it. Off: Start sends at once. Plain terminals, commands and queued tasks never ask.
    pub confirm_agent_start: bool,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            confirm_agent_start: true,
        }
    }
}

impl Setting for LauncherSettings {
    const NAMESPACE: &'static str = "launcher";
}

/// Which default key table the app starts from; `keymap.toml` is merged on top of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum KeymapBase {
    /// Zed's macOS defaults where Hutan has the action: ⌘⇧P palette, ⌘P jump, ⌘B / ⌘R / ⌘J
    /// docks, ⌘K ←→↑↓ for splits. Hutan-only actions keep keys Zed leaves free.
    #[default]
    Zed,
    /// Hutan's original table (⌘K palette, ⌘J jump, ⌘⌥B / ⌘⌥J docks, ⌘D / ⌘⇧D splits).
    Hutan,
}

impl KeymapBase {
    pub const ALL: [KeymapBase; 2] = [KeymapBase::Zed, KeymapBase::Hutan];

    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            KeymapBase::Zed => "zed",
            KeymapBase::Hutan => "hutan",
        }
    }
}

/// The `keymap` namespace (the bindings you add or remove live in `keymap.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct KeymapSettings {
    /// The default key table: `zed` (Zed's macOS keys: ⌘⇧P palette, ⌘P jump, ⌘B / ⌘R / ⌘J
    /// docks) or `hutan` (the original Hutan keys: ⌘K palette, ⌘J jump). `keymap.toml` is
    /// merged on top of either. Changing it rebinds at once.
    pub base: KeymapBase,
}

impl Setting for KeymapSettings {
    const NAMESPACE: &'static str = "keymap";
}

/// Registers every namespace the desktop owns (plus `appearance`) so the schema and diagnostics
/// know them from the start.
pub fn register_all(store: &SettingsStore) {
    store.register::<AppearanceSettings>();
    store.register::<AppSettings>();
    store.register::<AssistantSettings>();
    store.register::<KeymapSettings>();
    store.register::<IntegrationsSettings>();
    store.register::<LauncherSettings>();
    store.register::<TerminalSettings>();
    store.register::<LayoutSettings>();
    store.register::<SessionsPanelSettings>();
    store.register::<InspectorPanelSettings>();
    store.register::<OverviewPanelSettings>();
    store.register::<WorkspacesSettings>();
    store.register::<SidebarSettings>();
    store.register::<HistorySettings>();
    store.register::<ChatSettings>();
    store.register::<EditorSettings>();
    store.register::<AgentsSettings>();
    store.register::<ResourcesSettings>();
    store.register::<TasksSettings>();
}

/// Panels use this for `PanelId` lookups without importing the model.
pub fn panel_ids(names: &[String]) -> Vec<PanelId> {
    names.iter().map(|n| PanelId::new(n.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_shell_and_env_go_into_the_session_spec() {
        let mut settings = TerminalSettings::default();
        let mut spec = TerminalSpec::default();
        settings.apply_to_spec(&mut spec);
        assert!(spec.env.is_empty(), "defaults add nothing: {:?}", spec.env);

        settings.shell = "/opt/homebrew/bin/fish".to_owned();
        settings.env.insert("EDITOR".into(), "hx".into());
        settings.env.insert("SHELL".into(), "/bin/ignored".into());
        let mut spec = TerminalSpec::default();
        settings.apply_to_spec(&mut spec);
        assert_eq!(spec.env["EDITOR"], "hx");
        assert_eq!(
            spec.env["SHELL"], "/opt/homebrew/bin/fish",
            "`shell` beats env.SHELL"
        );

        // What the caller already asked for stays.
        let mut spec = TerminalSpec::default();
        spec.env.insert("EDITOR".into(), "ed".into());
        spec.env.insert("SHELL".into(), "/bin/sh".into());
        settings.apply_to_spec(&mut spec);
        assert_eq!(spec.env["EDITOR"], "ed");
        assert_eq!(spec.env["SHELL"], "/bin/sh");
    }

    #[test]
    fn builtin_presets_are_always_present_and_user_presets_extend_them() {
        let mut layout = LayoutSettings::default();
        let names: Vec<_> = layout
            .preset_defs()
            .into_iter()
            .map(|d| d.layout.name)
            .collect();
        assert_eq!(names, ["default", "focus", "overview"]);

        layout.presets.insert(
            "wide".into(),
            PresetSetting {
                left: DockSetting {
                    panels: vec!["sessions".into()],
                    size: 300.0,
                    visible: true,
                },
                ..PresetSetting::default()
            },
        );
        layout.presets.insert(
            "overview".into(),
            PresetSetting {
                right: DockSetting {
                    panels: vec!["inspector".into()],
                    size: 280.0,
                    visible: true,
                },
                ..PresetSetting::default()
            },
        );
        let defs = layout.preset_defs();
        assert_eq!(defs.len(), 4);
        let overview = defs.iter().find(|d| d.layout.name == "overview").unwrap();
        assert!(
            matches!(&overview.layout.center, CenterLayout::Items(_)),
            "the overview keeps its center behavior"
        );
        assert!(
            !overview.layout.docks.left.visible,
            "docks come from the user preset"
        );
        assert_eq!(overview.layout.docks.right.size, 280.0);
        let wide = defs.iter().find(|d| d.layout.name == "wide").unwrap();
        assert_eq!(wide.layout.docks.left.size, 300.0);
    }

    #[test]
    fn terminal_settings_resolve_the_theme_and_clamp_numbers() {
        let settings = TerminalSettings {
            font_size: 500.0,
            theme: "auto".into(),
            ..TerminalSettings::default()
        };
        let dark = settings.view_settings(SystemAppearance::Dark);
        let light = settings.view_settings(SystemAppearance::Light);
        assert_eq!(dark.palette.background, Rgb(0x26, 0x26, 0x24));
        assert_eq!(light.palette.background, Rgb(0xfa, 0xf9, 0xf5));
        assert_eq!(dark.font_size, 72.0);
        let pinned = TerminalSettings {
            theme: "hutan-light".into(),
            ..TerminalSettings::default()
        };
        assert_eq!(
            pinned.view_settings(SystemAppearance::Dark).palette,
            light.palette
        );
    }

    #[test]
    fn chat_settings_default_to_newline_on_return_and_a_64_kib_attachment() {
        let defaults = ChatSettings::default();
        assert!(!defaults.send_on_enter);
        assert_eq!(defaults.attach_max_bytes, 65_536);
        let parsed: ChatSettings = toml::from_str("send_on_enter = true").unwrap();
        assert!(parsed.send_on_enter);
        assert_eq!(parsed.attach_max_bytes, 65_536);
    }

    #[test]
    fn closing_the_last_window_quits_by_default_and_reads_the_documented_values() {
        assert_eq!(
            AppSettings::default().on_last_window_close,
            LastWindowClose::Quit
        );
        for mode in [LastWindowClose::Quit, LastWindowClose::MenuBar] {
            let parsed: AppSettings =
                toml::from_str(&format!("on_last_window_close = \"{}\"", mode.key())).unwrap();
            assert_eq!(parsed.on_last_window_close, mode);
        }
        assert!(toml::from_str::<AppSettings>("on_last_window_close = \"hide\"").is_err());
    }

    #[test]
    fn the_menu_bar_mode_needs_the_menu_bar_item() {
        let mut app = AppSettings {
            on_last_window_close: LastWindowClose::MenuBar,
            ..AppSettings::default()
        };
        assert_eq!(app.effective_last_window_close(), LastWindowClose::MenuBar);
        assert!(app.diagnostics().is_empty());
        app.menu_bar = false;
        assert_eq!(app.effective_last_window_close(), LastWindowClose::Quit);
        let diagnostics = app.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("app.menu_bar"));
        app.on_last_window_close = LastWindowClose::Quit;
        assert!(app.diagnostics().is_empty());
    }

    #[test]
    fn the_mac_integrations_default_as_documented() {
        let app = AppSettings::default();
        assert!(
            !app.keep_running && !app.start_at_login,
            "nothing outlives the app and nothing is registered with the system by default"
        );
        assert!(!app.setup_done, "the first-run setup is still to come");
        assert!(app.menu_bar && app.dock_badge);
        let parsed: AppSettings = toml::from_str("keep_running = true\nmenu_bar = false").unwrap();
        assert!(parsed.keep_running && !parsed.start_at_login);
        assert!(!parsed.menu_bar && parsed.dock_badge);
        assert!(AssistantSettings::default().global_shortcut.is_empty());
        let off: AssistantSettings = toml::from_str("global_shortcut = \"\"").unwrap();
        assert!(off.global_shortcut.is_empty());
    }

    fn migrated(text: &str) -> (bool, AppSettings, Vec<hutan_settings::SettingsError>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
        std::fs::write(&paths.user, text).unwrap();
        let store = SettingsStore::new(paths.clone());
        register_all(&store);
        let done = migrate_start_at_login(&store, &paths.user);
        let notes = store
            .errors()
            .into_iter()
            .filter(|e| e.key_path == "app.start_at_login")
            .collect();
        (done, (*store.get::<AppSettings>()).clone(), notes)
    }

    #[test]
    fn the_old_keep_running_meaning_becomes_start_at_login_once() {
        // Before: keep_running was the login item. Now both, and the user is told.
        let (done, app, notes) = migrated("[app]\nkeep_running = true\n");
        assert!(done);
        assert!(app.keep_running && app.start_at_login);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].message.contains("app.start_at_login"));

        // A file that already names start_at_login, either way, is not touched again.
        for text in [
            "[app]\nkeep_running = true\nstart_at_login = false\n",
            "[app]\nkeep_running = true\nstart_at_login = true\n",
        ] {
            let (done, _, notes) = migrated(text);
            assert!(!done, "{text}");
            assert!(notes.is_empty());
        }

        // Nothing to migrate: off, absent, no file content, broken file.
        for text in [
            "[app]\nkeep_running = false\n",
            "[app]\nmenu_bar = false\n",
            "",
            "[app\nkeep_running = true",
        ] {
            let (done, app, _) = migrated(text);
            assert!(!done && !app.start_at_login, "{text}");
        }
    }

    #[test]
    fn defaults_round_trip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(hutan_settings::SettingsPaths::in_dir(dir.path()));
        register_all(&store);
        assert!(store.errors().is_empty(), "{:?}", store.errors());
        assert_eq!(
            *store.get::<TerminalSettings>(),
            TerminalSettings::default()
        );
        let schema = store.schema();
        for ns in [
            "app",
            "appearance",
            "terminal",
            "layout",
            "panels.sessions",
            "panels.inspector",
            "panels.overview",
            "workspaces",
            "history",
            "chat",
            "editor",
            "agents",
            "resources",
            "tasks",
        ] {
            assert!(
                schema["namespaces"][ns].is_object(),
                "{ns} missing from the schema"
            );
        }
    }
}
