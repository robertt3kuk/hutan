//! The desktop's settings namespaces (design.md § Settings): `app`, `terminal`, `layout`,
//! `panels.sessions`, `panels.inspector`, `panels.files`, `workspaces`, `history`, `chat`,
//! `editor`, `agents`.
//! `appearance` lives in `hutan-settings`.
//!
//! Numbers are `f64` so values read back from TOML and the schema defaults stay free of `f32`
//! rounding noise (`1.2`, not `1.2000000476837158`).

use std::collections::BTreeMap;
use std::path::Path;

use hutan_protocol::messages::TerminalSpec;
pub use hutan_settings::AppearanceSettings;
use hutan_settings::themes::{self, TerminalTheme};
use hutan_settings::{Setting, SettingsStore};
use hutan_terminal_view::{
    BlockSettings, CursorSettings, CursorStyle, OptionAsMeta, TerminalPalette, TerminalViewSettings,
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

/// What ⌘-click on a file path in terminal output opens it with (`terminal.open_files_with`).
/// `editor` by default since E-10 (it was `system` before).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OpenFilesWith {
    /// The file's default application, as a double click in Finder.
    System,
    /// Hutan's own file viewer, in a tab, at the clicked line and column.
    #[default]
    Editor,
}

/// Where the quick terminal's first session starts (`terminal.quick_start_in`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuickStart {
    /// The Home folder.
    #[default]
    Home,
    /// The folder of the project that is active in the main window when the panel first shows.
    Project,
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
    /// What ⌘-click on a file path in terminal output opens it with: `editor` (a file tab at
    /// the line, the default) or `system` (the file's default application).
    pub open_files_with: OpenFilesWith,
    /// Ask before a multi-line paste into a program that did not turn on bracketed paste (a
    /// shell would run every line at once). Programs that handle bracketed paste are never
    /// asked about.
    pub confirm_multiline_paste: bool,
    /// Dim the text of the terminal splits of a tab that do not have the keyboard by a few
    /// percent, so the focused one is not told apart by its top edge alone.
    pub dim_unfocused_splits: bool,
    /// The quick terminal: a terminal that drops down from the top of the screen on a
    /// system-wide shortcut (`quick_hotkey`). Off, the shortcut is released.
    pub quick_terminal: bool,
    /// The shortcut that shows and hides the quick terminal from any app, written like a
    /// keystroke (`ctrl-shift-grave`, `alt-space`, `cmd-shift-k`); at least one of `cmd`, `ctrl`,
    /// `alt` is needed. Empty turns it off.
    pub quick_hotkey: String,
    /// How much of the screen's height the quick terminal covers, in percent.
    pub quick_height: f64,
    /// Where the quick terminal's first session starts: `home` or `project`.
    pub quick_start_in: QuickStart,
    /// Escape hides the quick terminal. A program that has the alternate screen (vim, less) keeps
    /// its Escape either way.
    pub quick_escape_hides: bool,
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
            open_files_with: OpenFilesWith::Editor,
            confirm_multiline_paste: true,
            dim_unfocused_splits: true,
            quick_terminal: true,
            quick_hotkey: "ctrl-shift-grave".to_owned(),
            quick_height: 40.0,
            quick_start_in: QuickStart::Home,
            quick_escape_hides: true,
        }
    }
}

impl Setting for TerminalSettings {
    const NAMESPACE: &'static str = "terminal";
    const WORKSPACE_OVERRIDABLE: bool = true;
}

/// What the active color pack says about the terminal's font, used where the user has not set
/// the key themselves (`terminal.font_family`, `.font_size`, `.line_height`).
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalPackDefaults {
    pub font_family: String,
    pub font_size: f64,
    pub line_height: f64,
}

impl TerminalPackDefaults {
    /// The pack's terminal font size and line height, and `family` (the first installed of its
    /// `[fonts] mono` list, which only the text system can tell).
    pub fn of(theme: &themes::ResolvedTheme, family: String) -> Self {
        Self {
            font_family: family,
            font_size: f64::from(theme.metric(themes::Metric::TerminalFontSize)),
            line_height: f64::from(theme.metric(themes::Metric::TerminalLineHeight)),
        }
    }
}

/// Which font keys the user wrote in some settings layer. Only those override the pack: the
/// serialized `TerminalSettings::default()` is not a user choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalExplicit {
    pub font_family: bool,
    pub font_size: bool,
    pub line_height: bool,
}

impl TerminalExplicit {
    /// Reads the provenance of the three keys from the store.
    pub fn of(store: &SettingsStore) -> Self {
        let set = |key: &str| store.source_of(&format!("terminal.{key}")).is_some();
        Self {
            font_family: set("font_family"),
            font_size: set("font_size"),
            line_height: set("line_height"),
        }
    }

    /// Every key is the user's own (tests, and callers without a pack).
    pub const ALL: Self = Self {
        font_family: true,
        font_size: true,
        line_height: true,
    };
}

impl TerminalSettings {
    /// The font sizes (points) a terminal view accepts; larger or smaller values are clamped.
    pub const MIN_FONT_SIZE: f64 = 4.0;
    pub const MAX_FONT_SIZE: f64 = 72.0;
    /// The line heights (multiples of the font size) a terminal view accepts.
    pub const MIN_LINE_HEIGHT: f64 = 0.8;
    pub const MAX_LINE_HEIGHT: f64 = 3.0;
    /// The share of the screen's height (percent) the quick terminal may cover.
    pub const MIN_QUICK_HEIGHT: f64 = 20.0;
    pub const MAX_QUICK_HEIGHT: f64 = 90.0;

    /// `quick_height` as a fraction of the screen's height, kept within the accepted range.
    pub fn quick_height_fraction(&self) -> f32 {
        let percent = if self.quick_height.is_finite() {
            self.quick_height
        } else {
            Self::default().quick_height
        };
        (percent.clamp(Self::MIN_QUICK_HEIGHT, Self::MAX_QUICK_HEIGHT) / 100.0) as f32
    }

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

    /// The view configuration for this namespace: the pack's terminal colors and block roles,
    /// and its font defaults for the keys `explicit` says the user did not set.
    pub fn view_settings(
        &self,
        theme: &TerminalTheme,
        pack: &TerminalPackDefaults,
        explicit: TerminalExplicit,
    ) -> TerminalViewSettings {
        let (family, size, line) = (
            if explicit.font_family {
                &self.font_family
            } else {
                &pack.font_family
            },
            if explicit.font_size {
                self.font_size
            } else {
                pack.font_size
            },
            if explicit.line_height {
                self.line_height
            } else {
                pack.line_height
            },
        );
        TerminalViewSettings {
            font_family: family.clone(),
            font_size: size.clamp(Self::MIN_FONT_SIZE, Self::MAX_FONT_SIZE) as f32,
            line_height: line.clamp(Self::MIN_LINE_HEIGHT, Self::MAX_LINE_HEIGHT) as f32,
            palette: TerminalPalette::from(theme),
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
            confirm_multiline_paste: self.confirm_multiline_paste,
            // Padding follows the density; the workbench fills it in.
            padding: 0.0,
            // The gutter width, colors and motion follow the density and the chrome theme; the
            // workbench fills them in.
            blocks: BlockSettings {
                enabled: self.blocks,
                ..BlockSettings::default()
            },
            // The find tints follow the chrome theme's editor colors; the workbench fills
            // them in.
            find: hutan_terminal_view::FindSettings::default(),
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
    /// Shown as its rail only: the left sidebar's 44 pt icon rail, the right side's activity
    /// rail without the panel body. Ignored on the bottom dock.
    pub collapsed: bool,
    /// Panels of `panels` the rail lists in its `⋯` menu instead of as icons (shown anyway while
    /// active or while they have a badge), e.g. `["inspector", "resources"]`.
    pub more: Vec<String>,
}

impl Default for DockSetting {
    fn default() -> Self {
        Self {
            panels: Vec::new(),
            size: 240.0,
            visible: false,
            collapsed: false,
            more: Vec::new(),
        }
    }
}

impl DockSetting {
    fn from_dock(dock: &Dock) -> Self {
        Self {
            panels: dock.panels.iter().map(|p| p.to_string()).collect(),
            size: f64::from(dock.size),
            visible: dock.visible,
            collapsed: dock.collapsed,
            more: dock.more.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn to_dock(&self) -> Dock {
        // The old Overview board is the Agents page now (saved presets keep working).
        fn renamed(panel: &str) -> &str {
            match panel {
                "overview" => crate::workbench::model::AGENTS_PAGE,
                other => other,
            }
        }
        let mut panels: Vec<&str> = Vec::new();
        for panel in &self.panels {
            let panel = renamed(panel);
            if !panels.contains(&panel) {
                panels.push(panel);
            }
        }
        let more: Vec<&str> = self
            .more
            .iter()
            .map(String::as_str)
            .map(renamed)
            .filter(|p| panels.contains(p))
            .collect();
        let mut dock = Dock::new(&panels, self.size as f32, self.visible).with_more(&more);
        dock.collapsed = self.collapsed;
        dock
    }
}

/// Status items per side, by id (`connection`, `workspace`, `working`, `sessions`, `attention`,
/// `approvals`, `density`, `theme`, `palette_hint`, the editor's items, and the dock buttons
/// `dock_left`, `dock_bottom`, `dock_right`); unknown ids are skipped. A side left out takes the
/// built-in bar's.
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
    /// Collapse the right panel (below 1180 pt of window width) and then the left sidebar (below
    /// 900 pt) to their rails when the window is narrow, and bring them back when it grows. The
    /// collapse is temporary and never changes what you chose.
    pub auto_collapse: bool,
    /// Offer the quieter default layout (three icons on the right rail, the rest under `⋯`) once
    /// to a layout saved before it existed: one line under the title bar with *Use new default*
    /// and *Keep mine*. Off after you answered, so it is asked once; nothing changes unless you
    /// choose it (View → Use New Default Layout does it any time).
    pub offer_new_default: bool,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            preset: LayoutPreset::DEFAULT.to_owned(),
            presets: BTreeMap::new(),
            auto_collapse: true,
            offer_new_default: true,
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
    /// Show the Requests, Failed and Ready for review groups on top.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SidebarSettings {
    /// `workspaces` (user workspaces → their projects → sessions) or `projects` (a flat list of
    /// projects → sessions).
    pub organize: SidebarOrganize,
    /// List every container's sessions at once (the old tree). Off, only the container you work
    /// in is expanded: its sessions sit directly under it and the others show their counts.
    pub show_all_sessions: bool,
    /// The rows of the sidebar's nav, in order: `agents`, `assistant` (Mawas), `memory`,
    /// `tasks`. A row left out is hidden; the row of the page you are on shows anyway.
    pub nav_rows: Vec<String>,
    /// Show the list of projects and sessions under the nav.
    pub show_projects: bool,
}

impl SidebarSettings {
    /// The rows the nav can show, in the order it shows them by default.
    pub const NAV_ROWS: [&'static str; 4] = ["agents", "assistant", "memory", "tasks"];
}

impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            organize: SidebarOrganize::default(),
            show_all_sessions: false,
            nav_rows: Self::NAV_ROWS.map(str::to_owned).to_vec(),
            show_projects: true,
        }
    }
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

/// The `panels.files` namespace: the Files panel's tree of the active project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct FilesPanelSettings {
    /// List what `.gitignore` and `.ignore` rules match (dimmed) too.
    pub show_ignored: bool,
    /// Select the file of the focused tab in the tree (its folders open) when the tab changes.
    pub auto_reveal: bool,
}

impl Default for FilesPanelSettings {
    fn default() -> Self {
        Self {
            show_ignored: false,
            auto_reveal: true,
        }
    }
}

impl Setting for FilesPanelSettings {
    const NAMESPACE: &'static str = "panels.files";
}

/// The least serious diagnostics the Problems panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemsSeverity {
    /// Errors, warnings, information and hints.
    #[default]
    All,
    /// Errors and warnings.
    Warning,
    /// Errors only.
    Error,
}

/// The `panels.problems` namespace: the Problems panel (the language servers' diagnostics,
/// E-12a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ProblemsPanelSettings {
    /// The least serious diagnostics listed (the header's filter).
    pub severity: ProblemsSeverity,
}

impl Setting for ProblemsPanelSettings {
    const NAMESPACE: &'static str = "panels.problems";
}

/// The `panels.instructions` namespace: the Instructions panel (the files the agents read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct InstructionsPanelSettings {
    /// List the user's own instruction files (`~/.codex`, `~/.claude`, `~/.omp/agent`) under
    /// the project's. Off, the panel never looks at them.
    pub show_user_files: bool,
}

impl Default for InstructionsPanelSettings {
    fn default() -> Self {
        Self {
            show_user_files: true,
        }
    }
}

impl Setting for InstructionsPanelSettings {
    const NAMESPACE: &'static str = "panels.instructions";
}

/// The `panels.search` namespace: the Search panel (find and replace across the active
/// project, E-13b). The header's toggles write the three switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SearchPanelSettings {
    /// Match upper and lower case exactly; off, a query with an upper-case letter still does
    /// (smart case).
    pub case_sensitive: bool,
    /// Match whole words only.
    pub whole_word: bool,
    /// Read the query as a regular expression (Rust `regex` syntax; `$1` in the replacement
    /// names a group).
    pub regex: bool,
    /// Most matching lines one search lists (1 to 1 000, the daemon's bound).
    pub max_results: u32,
    /// How long typing must pause before the search runs, in milliseconds.
    pub debounce_ms: u64,
}

impl Default for SearchPanelSettings {
    fn default() -> Self {
        Self {
            case_sensitive: false,
            whole_word: false,
            regex: false,
            max_results: hutan_protocol::fs::SEARCH_MAX,
            debounce_ms: 150,
        }
    }
}

impl Setting for SearchPanelSettings {
    const NAMESPACE: &'static str = "panels.search";
}

/// The `status` namespace: the options of the status bar's items, one table per item (the
/// item's own context menu writes them; `layout.presets.<name>.status_bar` says which items
/// show and where).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatusSettings {
    /// The `workspace` item: the project and its branch.
    pub workspace: StatusWorkspaceSettings,
    /// The `connection` item: the daemon's state.
    pub connection: StatusConnectionSettings,
}

/// `status.workspace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatusWorkspaceSettings {
    /// Show the branch checked out in the project, after its name.
    pub show_branch: bool,
}

impl Default for StatusWorkspaceSettings {
    fn default() -> Self {
        Self { show_branch: true }
    }
}

/// `status.connection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StatusConnectionSettings {
    /// Say which version of the daemon is running (`hutand 0.2.0`) while it is up; its
    /// tooltip has the version either way.
    pub show_version: bool,
}

impl Setting for StatusSettings {
    const NAMESPACE: &'static str = "status";
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
    /// Workspaces with the most requests waiting first, then by activity.
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

/// Which key sends a message from every composer (the chat composer, the Start sheet's prompt,
/// review comments, free-text answers in question cards). ⌘Return always sends as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SendKey {
    /// ⌘Return sends; Return adds a line.
    #[default]
    CmdEnter,
    /// Return sends; ⇧Return (or ⌥Return) adds a line.
    Enter,
}

impl SendKey {
    pub const ALL: [SendKey; 2] = [SendKey::CmdEnter, SendKey::Enter];

    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            SendKey::CmdEnter => "cmd-enter",
            SendKey::Enter => "enter",
        }
    }

    /// The choice as the quick-switch menu and the settings sheet word it.
    pub fn label(self) -> &'static str {
        match self {
            SendKey::CmdEnter => "⌘Return (Return adds a line)",
            SendKey::Enter => "Return (⇧Return adds a line)",
        }
    }

    /// The short form of the choice, for the one-line first-run question.
    pub fn short_label(self) -> &'static str {
        match self {
            SendKey::CmdEnter => "⌘Return",
            SendKey::Enter => "Return",
        }
    }

    /// The keys that send, as a button label shows them.
    pub fn combo(self) -> &'static str {
        match self {
            SendKey::CmdEnter => "⌘↩",
            SendKey::Enter => "↩",
        }
    }

    /// The hint line of a composer whose Return does `verb` (`send`, `start`, `comment`).
    pub fn hint(self, verb: &str) -> String {
        match self {
            SendKey::CmdEnter => format!("⌘↩ {verb} · ↩ newline"),
            SendKey::Enter => format!("↩ {verb} · ⇧↩ newline"),
        }
    }

    /// The other choice.
    pub fn other(self) -> SendKey {
        match self {
            SendKey::CmdEnter => SendKey::Enter,
            SendKey::Enter => SendKey::CmdEnter,
        }
    }

    /// What the toast says after a toggle to this choice.
    pub fn toast(self) -> &'static str {
        match self {
            SendKey::CmdEnter => "⌘Return now sends",
            SendKey::Enter => "Return now sends",
        }
    }

    /// Whether a Return without ⌘ or ⌃ sends. ⇧Return and ⌥Return always add a line;
    /// ⌘Return sends under both choices and never gets here.
    pub fn return_sends(self, shift: bool, alt: bool) -> bool {
        self == SendKey::Enter && !shift && !alt
    }
}

/// The `chat` namespace: the agent chat view's composer and what "Send to agent" attaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ChatSettings {
    /// `cmd-enter` (default: ⌘Return sends, Return adds a line) or `enter` (Return sends,
    /// ⇧Return adds a line). Applies to every place a message is written. Files written before
    /// this key existed call it `send_on_enter` (a boolean; `true` is `enter`).
    pub send_key: SendKey,
    /// Ask once, in the first composer you focus, which key should send. Off after you answer
    /// or dismiss the question (or chose in the first-run setup).
    pub ask_send_key: bool,
    /// Most bytes of a terminal block's output that "Send to agent" attaches to the composer;
    /// longer output is cut (the chip says so).
    pub attach_max_bytes: u64,
}

impl Default for ChatSettings {
    fn default() -> Self {
        Self {
            send_key: SendKey::CmdEnter,
            ask_send_key: true,
            attach_max_bytes: 64 * 1024,
        }
    }
}

impl Setting for ChatSettings {
    const NAMESPACE: &'static str = "chat";

    /// The boolean `send_on_enter` of earlier versions (`true` is `enter`) counts as `send_key`
    /// unless the layer sets `send_key` itself.
    fn upgrade(layer: &mut toml::Table) {
        let Some(old) = layer.remove("send_on_enter") else {
            return;
        };
        if let Some(on) = old.as_bool() {
            let key = if on {
                SendKey::Enter
            } else {
                SendKey::CmdEnter
            };
            layer
                .entry("send_key")
                .or_insert_with(|| toml::Value::String(key.key().to_owned()));
        }
    }
}

/// When file tabs save by themselves (`editor.autosave`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Autosave {
    /// Only ⌘S saves.
    #[default]
    Off,
    /// When the file's view loses the keyboard or the window stops being the active one.
    OnFocusChange,
    /// `editor.autosave_delay_ms` after the last change.
    AfterDelay,
}

/// The `editor` namespace: how Hutan's text inputs (the chat composer, the palette, prompts and
/// the other single-line fields) edit, and how file tabs (the code view) draw and save.
/// Terminals are not text inputs: programs there do their own thing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EditorSettings {
    /// Vim's modal editing (Normal, Insert, Visual) in text inputs. Fields open in Insert; Escape
    /// goes to Normal, and a second Escape does what it always did (closes the palette, …).
    pub vim_mode: bool,
    /// Yanks and deletes in Vim mode also go to the system clipboard, and `p` pastes from it.
    /// On by default, as Zed's `use_system_clipboard = "always"`; off, as in Vim itself, they
    /// use a register private to the field.
    pub vim_use_system_clipboard: bool,
    /// Your own Vim mappings, per mode: keys you type → the keys Vim reads instead, like Vim's
    /// `inoremap` / `nnoremap` / `xnoremap` (what a mapping sends is never mapped again). Keys
    /// are written as in Vim: `jk`, `<Esc>`, `<C-w>`, `<CR>`, `<Space>`, `<lt>`; `<Nop>` maps
    /// to nothing.
    pub vim_mappings: VimMappings,
    /// How long a mapping that has begun waits for its next key (ms, Vim's `timeoutlen`):
    /// after that the keys typed so far count as themselves.
    pub vim_timeout_ms: u64,
    /// Font size of code in file tabs, in points (the family is the color pack's monospace).
    pub font_size: f64,
    /// Line height of code in file tabs, as a multiple of the font size.
    pub line_height: f64,
    /// Files larger than this (bytes) open only after a confirmation, read-only and without
    /// syntax highlighting (large-file mode).
    pub large_file_bytes: u64,
    /// File tabs save by themselves: `off`, `on_focus_change` or `after_delay`. A file with a
    /// conflict on disk is never saved by itself.
    pub autosave: Autosave,
    /// For `autosave = "after_delay"`: milliseconds after the last change.
    pub autosave_delay_ms: u64,
    /// Highlight the lines an agent's current turn changed in file tabs (a tint and a gutter
    /// bar), with per-change Reject. Off, nothing is diffed or drawn; the agent's writes still
    /// reach the file and the pending ledger.
    pub agent_highlight: bool,
    /// Show the git blame of the caret's line (author, date, message) after the end of the line.
    /// Off by default; the gutter's blame column is the *Toggle Blame* command.
    pub inline_blame: bool,
}

/// `editor.large_file_bytes` by default: 16 MiB.
pub const DEFAULT_LARGE_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// `editor.vim_mappings`: one table per mode, keys typed → keys Vim reads.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VimMappings {
    /// Insert mode (`inoremap`): `jk = "<Esc>"`.
    pub insert: BTreeMap<String, String>,
    /// Normal mode (`nnoremap`): `Y = "y$"`.
    pub normal: BTreeMap<String, String>,
    /// Visual modes (`xnoremap`).
    pub visual: BTreeMap<String, String>,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            vim_mode: false,
            vim_use_system_clipboard: true,
            vim_mappings: VimMappings::default(),
            vim_timeout_ms: 1000,
            font_size: 13.0,
            line_height: 1.45,
            large_file_bytes: DEFAULT_LARGE_FILE_BYTES,
            autosave: Autosave::Off,
            autosave_delay_ms: 1000,
            agent_highlight: true,
            inline_blame: false,
        }
    }
}

impl Setting for EditorSettings {
    const NAMESPACE: &'static str = "editor";
}

/// Which agent sessions the contextual Agents panel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentsScope {
    /// The workspace the window shows (it follows the selected workspace).
    #[default]
    ActiveWorkspace,
    /// Only the session in the focused tab (never the panel's own focus).
    Session,
    /// Every agent session of this host, pinned.
    All,
}

impl AgentsScope {
    /// The value in `settings.toml`.
    pub fn key(self) -> &'static str {
        match self {
            AgentsScope::ActiveWorkspace => "active_workspace",
            AgentsScope::Session => "session",
            AgentsScope::All => "all",
        }
    }
}

/// The `agents` namespace: what the Agents panel and the Agents page list. Blocked agents are
/// always listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AgentsSettings {
    /// Panel: list subagents that have finished (dimmed). Off, only the ones still around
    /// (and the ones above them) are listed.
    pub show_finished: bool,
    /// Panel: list only blocked subagents (and the ones above them).
    pub only_blocked: bool,
    /// Panel: `active_workspace`, `session` or `all`.
    pub scope: AgentsScope,
    /// Page: group the list by workspace, with the state groups inside each.
    pub page_group_by_workspace: bool,
    /// Page: show the sessions that finished (they are folded under their heading otherwise).
    pub page_show_finished: bool,
    /// Page: the Usage section — per harness its limit windows with reset times, and the tokens
    /// and cost of the turns Hutan ran today and this week.
    pub page_usage: bool,
    /// Status bar: show the `usage` item — the tightest limit of any harness, `Codex 72% ·
    /// resets 3h` — even when the layout's status bar does not name it.
    pub usage_in_status_bar: bool,
}

impl Default for AgentsSettings {
    fn default() -> Self {
        Self {
            show_finished: false,
            only_blocked: false,
            scope: AgentsScope::default(),
            page_group_by_workspace: false,
            page_show_finished: false,
            page_usage: true,
            usage_in_status_bar: false,
        }
    }
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
    /// The first run is over: a folder was opened (or a terminal started) from the first
    /// screen, or the setup was finished or skipped. Off shows the first screen; "Run Setup
    /// Again…" (the Hutan menu, Settings, the command palette) opens the setup whatever this
    /// says.
    pub setup_done: bool,
    /// Offer notifications once, the first time something wants you (an approval, a finished
    /// run) while Hutan is not the frontmost app: one line with *Notify me* and *No thanks*.
    /// Off after you answered or dismissed it, so it is asked once and never repeats; the
    /// system's own permission prompt only appears when you press *Notify me*.
    pub ask_notifications: bool,
    /// Offer to install an agent's hooks the first time that agent runs in a Hutan terminal
    /// without having reported (the *Track as* strip). Off after the offer was answered or
    /// dismissed: it is asked once and never repeats; nothing is written unless you accept.
    pub ask_hooks: bool,
    /// When the last window closes: `quit` (Quit Hutan, like ⌘Q: asks first while sessions run
    /// and keep running is off) or `menu_bar` (keep Hutan in the menu bar, without a Dock icon;
    /// the daemon keeps running until you quit; needs `menu_bar`).
    pub on_last_window_close: LastWindowClose,
    /// Show a menu bar item with the daemon state, a count of the sessions with a request waiting or an unseen failure and a
    /// menu to jump to them. Hides this item only, and only while Hutan runs: when no Hutan is
    /// connected to the daemon (it keeps running after Hutan quit) the daemon shows an item of
    /// its own, which is not optional.
    pub menu_bar: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            keep_running: false,
            start_at_login: false,
            setup_done: false,
            ask_notifications: true,
            ask_hooks: true,
            on_last_window_close: LastWindowClose::Quit,
            menu_bar: true,
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

/// The `notifications` namespace: how Hutan calls you from the desktop. What the *daemon* does
/// with a notification (the system's banner, its sound, whether a finished turn gets one) is in
/// `daemon.toml` `[notify]` and is shown on the same Settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NotificationsSettings {
    /// Show the number of sessions with a request waiting (an approval, a question, a plan) as a
    /// badge on the Dock icon. Only requests count: failures and finished work never do.
    pub dock_badge: bool,
    /// A toast when a harness's limit window passes 80 % and 95 % (once per window).
    pub usage_warnings: bool,
}

impl Default for NotificationsSettings {
    fn default() -> Self {
        Self {
            dock_badge: true,
            usage_warnings: true,
        }
    }
}

impl Setting for NotificationsSettings {
    const NAMESPACE: &'static str = "notifications";
}

/// A `settings.toml` that turned the Dock badge off as `app.dock_badge` gets
/// `notifications.dock_badge = false`, so that what the user chose keeps working. A file that
/// already names `notifications.dock_badge` is left alone. `true` when the file was migrated.
pub fn migrate_dock_badge(store: &SettingsStore, user_file: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(user_file) else {
        return false;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return false;
    };
    let old = table
        .get("app")
        .and_then(toml::Value::as_table)
        .and_then(|app| app.get("dock_badge"))
        .and_then(toml::Value::as_bool);
    let named = table
        .get("notifications")
        .and_then(toml::Value::as_table)
        .is_some_and(|notifications| notifications.contains_key("dock_badge"));
    if old != Some(false) || named {
        return false;
    }
    if let Err(error) = store.write_user("notifications.dock_badge", toml::Value::Boolean(false)) {
        tracing::warn!("cannot migrate app.dock_badge to notifications.dock_badge: {error}");
        return false;
    }
    true
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

/// The diagnostics source of [`migrate_start_at_login`]'s note.
const MIGRATION_NOTE: &str = "app.migration";

/// The status bar of the built-in presets from CL-CHROME until FX-STATUSBAR: the left side and
/// the right side, as *Save Layout as Preset* wrote them into `layout.presets.<name>.status_bar`.
const CL_CHROME_STATUS_BAR: (&[&str], &[&str]) = (
    &["attention", "connection"],
    &[
        "editor.diagnostics",
        "editor.mode",
        "editor.position",
        "editor.language",
        "palette_hint",
    ],
);

/// A preset saved while the built-in status bar was the CL-CHROME one (`attention`,
/// `connection`, the editor's items, `palette_hint`) did not choose that bar: it was written
/// out with the layout. Such a `layout.presets.<name>.status_bar` is removed from
/// `settings.toml`, so the preset follows the built-in default again (FX-STATUSBAR: the
/// connection always, the project and branch, the agents at work). A status bar that differs
/// in any item or its order was edited by the user and is left alone; a side the table does not
/// name was never chosen and counts as the old default. Runs once: the key is gone afterwards.
/// `user_file` is the file `store` reads. Returns the presets that were moved.
pub fn migrate_status_bar(store: &SettingsStore, user_file: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(user_file) else {
        return Vec::new();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(presets) = table
        .get("layout")
        .and_then(toml::Value::as_table)
        .and_then(|layout| layout.get("presets"))
        .and_then(toml::Value::as_table)
    else {
        return Vec::new();
    };
    let mut moved = Vec::new();
    for (name, preset) in presets {
        let Some(bar) = preset
            .as_table()
            .and_then(|preset| preset.get("status_bar"))
            .and_then(toml::Value::as_table)
        else {
            continue;
        };
        let is_old = |side: &str, old: &[&str]| match bar.get(side) {
            None => true,
            Some(toml::Value::Array(items)) => items
                .iter()
                .map(toml::Value::as_str)
                .eq(old.iter().copied().map(Some)),
            Some(_) => false,
        };
        if !(is_old("left", CL_CHROME_STATUS_BAR.0) && is_old("right", CL_CHROME_STATUS_BAR.1)) {
            continue;
        }
        match store.remove(
            hutan_settings::SettingsLayer::User,
            "layout",
            &format!("presets.{name}.status_bar"),
        ) {
            Ok(true) => {
                tracing::info!(
                    preset = %name,
                    "layout.presets.{name}.status_bar was the CL-CHROME default: the preset now \
                     follows the built-in status bar (FX-STATUSBAR)"
                );
                moved.push(name.clone());
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!("cannot move layout.presets.{name}.status_bar: {error}");
            }
        }
    }
    moved
}

/// The `assistant` namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AssistantSettings {
    /// Whether Hutan has Mawas, the assistant, at all. On (the default; Mawas is a headline
    /// feature and the first run does not ask for it) adds *Mawas* to the left sidebar's nav;
    /// Mawas Home opens as a page in the center. Off hides Mawas from the left sidebar,
    /// removes its commands from the palette and unregisters the system-wide shortcut; the
    /// daemon keeps its conversations.
    pub enabled: bool,
    /// System-wide shortcut that brings Hutan forward and opens Mawas, for example
    /// `alt-space` or `cmd-shift-space` (modifiers `cmd`, `ctrl`, `alt`, `shift`, at least one
    /// of the first three). Empty (the default) turns it off. If another app already owns the shortcut,
    /// Hutan says so once and leaves the setting alone; the in-app keybinding is separate.
    pub global_shortcut: String,
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            global_shortcut: String::new(),
        }
    }
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
    store.register::<NotificationsSettings>();
    store.register::<AssistantSettings>();
    store.register::<KeymapSettings>();
    store.register::<IntegrationsSettings>();
    store.register::<LauncherSettings>();
    store.register::<TerminalSettings>();
    store.register::<LayoutSettings>();
    store.register::<SessionsPanelSettings>();
    store.register::<InspectorPanelSettings>();
    store.register::<FilesPanelSettings>();
    store.register::<ProblemsPanelSettings>();
    store.register::<InstructionsPanelSettings>();
    store.register::<SearchPanelSettings>();
    store.register::<StatusSettings>();
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
                    collapsed: false,
                    more: Vec::new(),
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
                    collapsed: false,
                    more: Vec::new(),
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

    fn builtin_terminal(builtin: themes::Builtin) -> (TerminalTheme, TerminalPackDefaults) {
        let theme = themes::builtin_theme(builtin);
        let defaults = TerminalPackDefaults::of(&theme, "Menlo".into());
        (theme.terminal(), defaults)
    }

    #[test]
    fn terminal_settings_use_the_pack_theme_and_clamp_numbers() {
        let settings = TerminalSettings {
            font_size: 500.0,
            theme: "auto".into(),
            ..TerminalSettings::default()
        };
        let (dark, pack) = builtin_terminal(themes::Builtin::HutanDark);
        let (light, _) = builtin_terminal(themes::Builtin::HutanLight);
        let dark = settings.view_settings(&dark, &pack, TerminalExplicit::ALL);
        let light = settings.view_settings(&light, &pack, TerminalExplicit::ALL);
        assert_eq!(
            dark.palette.background,
            hutan_terminal_view::Rgb(0x26, 0x26, 0x24)
        );
        assert_eq!(
            light.palette.background,
            hutan_terminal_view::Rgb(0xfa, 0xf9, 0xf5)
        );
        assert_eq!(dark.font_size, 72.0);
        // Selection foreground and the cursor glyph are not set by the built-ins.
        assert_eq!(
            (
                dark.palette.selection_foreground,
                dark.palette.cursor_foreground
            ),
            (None, None)
        );
    }

    #[test]
    fn only_the_fonts_the_user_wrote_beat_the_pack_defaults() {
        let (terminal, _) = builtin_terminal(themes::Builtin::HutanDark);
        let pack = TerminalPackDefaults {
            font_family: "Fira Code".into(),
            font_size: 15.0,
            line_height: 1.4,
        };
        // Serialized defaults are not a user choice: the pack's values apply.
        let plain = TerminalSettings::default().view_settings(
            &terminal,
            &pack,
            TerminalExplicit::default(),
        );
        assert_eq!(
            (
                plain.font_family.as_str(),
                plain.font_size,
                plain.line_height
            ),
            ("Fira Code", 15.0, 1.4)
        );
        // One key written by the user wins for that key only.
        let user = TerminalSettings {
            font_size: 20.0,
            ..TerminalSettings::default()
        };
        let explicit = TerminalExplicit {
            font_size: true,
            ..TerminalExplicit::default()
        };
        let mixed = user.view_settings(&terminal, &pack, explicit);
        assert_eq!(
            (
                mixed.font_family.as_str(),
                mixed.font_size,
                mixed.line_height
            ),
            ("Fira Code", 20.0, 1.4)
        );
        // The user's value equal to the built-in default still counts as written.
        let same =
            TerminalSettings::default().view_settings(&terminal, &pack, TerminalExplicit::ALL);
        assert_eq!(
            (same.font_family.as_str(), same.font_size, same.line_height),
            ("Menlo", 13.0, 1.2)
        );
        // The pack's numbers are clamped like the user's.
        let wild = TerminalPackDefaults {
            font_size: 500.0,
            ..pack
        };
        assert_eq!(
            TerminalSettings::default()
                .view_settings(&terminal, &wild, TerminalExplicit::default())
                .font_size,
            72.0
        );
    }

    #[test]
    fn paste_and_link_rules_reach_the_view_and_the_opener() {
        let (terminal, _) = builtin_terminal(themes::Builtin::HutanDark);
        let pack = TerminalPackDefaults {
            font_family: "Menlo".to_owned(),
            font_size: 13.0,
            line_height: 1.2,
        };
        let on = TerminalSettings::default();
        assert!(on.confirm_multiline_paste, "the question is on by default");
        assert_eq!(
            on.open_files_with,
            OpenFilesWith::Editor,
            "a file tab at the line by default (E-10)"
        );
        assert!(
            on.view_settings(&terminal, &pack, TerminalExplicit::ALL)
                .confirm_multiline_paste
        );
        let off: TerminalSettings =
            toml::from_str("confirm_multiline_paste = false\nopen_files_with = \"system\"")
                .unwrap();
        assert!(
            !off.view_settings(&terminal, &pack, TerminalExplicit::ALL)
                .confirm_multiline_paste
        );
        assert_eq!(off.open_files_with, OpenFilesWith::System);
    }

    #[test]
    fn the_quick_terminal_is_on_by_default_and_its_height_stays_in_range() {
        let defaults = TerminalSettings::default();
        assert!(defaults.quick_terminal);
        assert_eq!(defaults.quick_hotkey, "ctrl-shift-grave");
        assert_eq!(defaults.quick_start_in, QuickStart::Home);
        assert!(defaults.quick_escape_hides);
        assert!((defaults.quick_height_fraction() - 0.4).abs() < 1e-6);
        let parse = |text: &str| -> TerminalSettings { toml::from_str(text).unwrap() };
        let custom = parse(
            "quick_terminal = false\nquick_hotkey = \"alt-space\"\nquick_start_in = \"project\"\n\
             quick_escape_hides = false\nquick_height = 55",
        );
        assert!(!custom.quick_terminal && !custom.quick_escape_hides);
        assert_eq!(custom.quick_start_in, QuickStart::Project);
        assert!((custom.quick_height_fraction() - 0.55).abs() < 1e-6);
        // A value the panel cannot use is clamped, and the default stands for not a number.
        assert!((parse("quick_height = 5").quick_height_fraction() - 0.2).abs() < 1e-6);
        assert!((parse("quick_height = 400").quick_height_fraction() - 0.9).abs() < 1e-6);
        assert!((parse("quick_height = nan").quick_height_fraction() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn provenance_comes_from_the_settings_layers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
        std::fs::write(&paths.user, "[terminal]\nfont_size = 14.0\n").unwrap();
        let store = SettingsStore::new(paths);
        register_all(&store);
        assert_eq!(
            TerminalExplicit::of(&store),
            TerminalExplicit {
                font_size: true,
                ..TerminalExplicit::default()
            }
        );
    }

    #[test]
    fn chat_settings_default_to_cmd_enter_and_a_64_kib_attachment() {
        let defaults = ChatSettings::default();
        assert_eq!(defaults.send_key, SendKey::CmdEnter);
        assert!(defaults.ask_send_key);
        assert_eq!(defaults.attach_max_bytes, 65_536);
        let parsed: ChatSettings = toml::from_str("send_key = \"enter\"").unwrap();
        assert_eq!(parsed.send_key, SendKey::Enter);
        assert_eq!(parsed.attach_max_bytes, 65_536);
        let empty: ChatSettings = toml::from_str("").unwrap();
        assert_eq!(empty, defaults);
        assert!(toml::from_str::<ChatSettings>("send_key = \"ctrl-enter\"").is_err());
    }

    #[test]
    fn the_old_send_on_enter_reads_as_send_key_and_send_key_wins() {
        let send_key = |text: &str| {
            let dir = tempfile::tempdir().unwrap();
            let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
            std::fs::write(&paths.user, text).unwrap();
            let store = SettingsStore::new(paths);
            register_all(&store);
            assert!(store.errors().is_empty(), "{text}: {:?}", store.errors());
            store.get::<ChatSettings>().send_key
        };
        assert_eq!(send_key(""), SendKey::CmdEnter);
        assert_eq!(send_key("[chat]\nsend_on_enter = true\n"), SendKey::Enter);
        assert_eq!(
            send_key("[chat]\nsend_on_enter = false\n"),
            SendKey::CmdEnter
        );
        assert_eq!(send_key("[chat]\nsend_key = \"enter\"\n"), SendKey::Enter);
        assert_eq!(
            send_key("[chat]\nsend_on_enter = true\nsend_key = \"cmd-enter\"\n"),
            SendKey::CmdEnter,
            "the new key wins"
        );
    }

    #[test]
    fn writing_the_send_key_writes_send_key_and_overrides_the_old_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
        std::fs::write(&paths.user, "[chat]\nsend_on_enter = true\n").unwrap();
        let store = SettingsStore::new(paths.clone());
        register_all(&store);
        assert_eq!(store.get::<ChatSettings>().send_key, SendKey::Enter);
        store
            .write_user(
                "chat.send_key",
                toml::Value::String(SendKey::CmdEnter.key().to_owned()),
            )
            .unwrap();
        let text = std::fs::read_to_string(&paths.user).unwrap();
        assert!(text.contains("send_key = \"cmd-enter\""), "{text}");
        assert_eq!(store.get::<ChatSettings>().send_key, SendKey::CmdEnter);
        assert!(
            store
                .errors()
                .iter()
                .all(|e| e.severity != hutan_settings::Severity::Error),
            "{:?}",
            store.errors()
        );
    }

    #[test]
    fn only_return_sends_under_the_enter_key_and_never_with_shift_or_alt() {
        for (key, shift, alt, sends) in [
            (SendKey::CmdEnter, false, false, false),
            (SendKey::CmdEnter, true, false, false),
            (SendKey::Enter, false, false, true),
            (SendKey::Enter, true, false, false),
            (SendKey::Enter, false, true, false),
        ] {
            assert_eq!(key.return_sends(shift, alt), sends, "{key:?} {shift} {alt}");
        }
        assert_eq!(SendKey::CmdEnter.other(), SendKey::Enter);
        assert_eq!(SendKey::Enter.other(), SendKey::CmdEnter);
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
        assert!(app.menu_bar);
        assert!(NotificationsSettings::default().dock_badge);
        let parsed: AppSettings = toml::from_str("keep_running = true\nmenu_bar = false").unwrap();
        assert!(parsed.keep_running && !parsed.start_at_login);
        assert!(!parsed.menu_bar);
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
    fn a_dock_badge_turned_off_under_app_stays_off_under_notifications() {
        let run = |text: &str| {
            let dir = tempfile::tempdir().unwrap();
            let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
            std::fs::write(&paths.user, text).unwrap();
            let store = SettingsStore::new(paths.clone());
            register_all(&store);
            let done = migrate_dock_badge(&store, &paths.user);
            (done, store.get::<NotificationsSettings>().dock_badge)
        };
        assert_eq!(run("[app]\ndock_badge = false\n"), (true, false));
        // On is the default; a file that already names the new key is left alone.
        assert_eq!(run("[app]\ndock_badge = true\n"), (false, true));
        assert_eq!(
            run("[app]\ndock_badge = false\n[notifications]\ndock_badge = true\n"),
            (false, true)
        );
        assert_eq!(run(""), (false, true));
    }

    /// Runs [`migrate_status_bar`] over `text`; the presets moved, the file afterwards and the
    /// status bar each preset ends up with.
    fn status_bars_migrated(text: &str) -> (Vec<String>, String, Vec<(String, StatusBarLayout)>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = hutan_settings::SettingsPaths::in_dir(dir.path());
        std::fs::write(&paths.user, text).unwrap();
        let store = SettingsStore::new(paths.clone());
        register_all(&store);
        let moved = migrate_status_bar(&store, &paths.user);
        let bars = store
            .get::<LayoutSettings>()
            .preset_defs()
            .into_iter()
            .map(|def| (def.layout.name, def.status_bar))
            .collect();
        (moved, std::fs::read_to_string(&paths.user).unwrap(), bars)
    }

    const OLD_BAR: &str = r#"status_bar = { left = ["attention", "connection"], right = ["editor.diagnostics", "editor.mode", "editor.position", "editor.language", "palette_hint"] }"#;

    #[test]
    fn a_preset_saved_with_the_cl_chrome_bar_gets_the_new_default_once() {
        let text =
            format!("[layout.presets.mine]\n{OLD_BAR}\n[layout.presets.mine.left]\nsize = 300\n");
        let (moved, file, bars) = status_bars_migrated(&text);
        assert_eq!(moved, ["mine"]);
        assert!(!file.contains("status_bar"), "{file}");
        assert!(
            file.contains("size = 300"),
            "the rest of the preset stays: {file}"
        );
        let mine = bars.iter().find(|(name, _)| name == "mine").unwrap();
        assert_eq!(mine.1, StatusBarLayout::default());
        assert_eq!(mine.1.left, ["connection", "workspace", "attention"]);

        // Once: the second run finds nothing.
        let (again, _, _) = status_bars_migrated(&file);
        assert!(again.is_empty());
    }

    #[test]
    fn a_side_the_table_does_not_name_counts_as_the_old_default() {
        let text =
            "[layout.presets.mine]\nstatus_bar = { left = [\"attention\", \"connection\"] }\n";
        let (moved, file, _) = status_bars_migrated(text);
        assert_eq!(moved, ["mine"]);
        assert!(!file.contains("status_bar"), "{file}");
    }

    #[test]
    fn a_status_bar_the_user_chose_is_left_alone() {
        for text in [
            // Another item on the left.
            "[layout.presets.mine]\nstatus_bar = { left = [\"connection\", \"sessions\", \"attention\"], right = [\"palette_hint\"] }\n",
            // The old items in another order.
            "[layout.presets.mine]\nstatus_bar = { left = [\"connection\", \"attention\"] }\n",
            // Old left, customised right.
            "[layout.presets.mine]\nstatus_bar = { left = [\"attention\", \"connection\"], right = [\"density\"] }\n",
            // The pre-CL-CHROME bar.
            "[layout.presets.mine]\nstatus_bar = { left = [\"connection\", \"workspace\", \"sessions\", \"attention\", \"approvals\"], right = [\"density\", \"theme\", \"palette_hint\"] }\n",
            // Nothing to migrate, no presets, a broken file.
            "[layout.presets.mine]\nsize = 1\n",
            "",
            "[layout.presets.mine\nstatus_bar = 1",
        ] {
            let (moved, file, _) = status_bars_migrated(text);
            assert!(moved.is_empty(), "{text}");
            assert_eq!(file, text, "the file is untouched");
        }
        let text = "[layout.presets.mine]\nstatus_bar = { left = [\"connection\", \"sessions\"], right = [\"palette_hint\"] }\n";
        let (_, _, bars) = status_bars_migrated(text);
        let mine = bars.iter().find(|(name, _)| name == "mine").unwrap();
        assert_eq!(
            (mine.1.left.clone(), mine.1.right.clone()),
            (
                vec!["connection".to_owned(), "sessions".to_owned()],
                vec!["palette_hint".to_owned()]
            )
        );
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
            "panels.files",
            "panels.problems",
            "panels.instructions",
            "panels.search",
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
