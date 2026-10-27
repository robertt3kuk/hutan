//! Panel and status-item registries (ADR 0010: every UI feature is a registered component).

use super::model::{DockPosition, PanelId};
use crate::sessions::SessionsStore;
use crate::theme::{Density, Theme};
use gpui::{AnyElement, AnyView, App, Entity, SharedString, Window};
use hutan_settings::SettingsStore;
use std::rc::Rc;

/// What panels need from the app: the live sessions, the settings, the harnesses (installed,
/// logged in, their models) and where to record the painted bounds of their interactive rows.
#[derive(Clone)]
pub struct PanelEnv {
    pub sessions: Entity<SessionsStore>,
    pub settings: SettingsStore,
    pub harnesses: Entity<super::harnesses::HarnessDirectory>,
    pub usage: Entity<super::usage::UsageDirectory>,
    pub regions: super::regions::Regions,
    /// The folders of Codex, Claude Code and OMP whose instruction files the `instructions`
    /// panel lists; `None` lists none (the workbench sets them, tests replace them).
    pub instruction_homes: Rc<std::cell::RefCell<Option<crate::instructions::HarnessHomes>>>,
    /// The open folders the Files panel remembers per root (saved with the layout).
    pub files_memory: super::panels::FilesMemory,
}

impl std::fmt::Debug for PanelEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelEnv").finish_non_exhaustive()
    }
}

/// Builds the view of a panel. Called once per workbench; the view is cached and re-rendered.
pub type PanelFactory = Rc<dyn Fn(&PanelEnv, &mut Window, &mut App) -> AnyView>;

#[derive(Clone)]
pub struct PanelSpec {
    pub id: PanelId,
    pub title: SharedString,
    /// Stable semantic icon; the selected pack supplies the artwork.
    pub icon: crate::icons::Icon,
    pub default_dock: DockPosition,
    pub factory: PanelFactory,
}

impl std::fmt::Debug for PanelSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelSpec")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("default_dock", &self.default_dock)
            .finish_non_exhaustive()
    }
}

/// id → panel spec, in registration order.
#[derive(Clone, Debug, Default)]
pub struct PanelRegistry {
    specs: Vec<PanelSpec>,
}

impl PanelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a panel; a later registration with the same id replaces the earlier one.
    pub fn register(&mut self, spec: PanelSpec) {
        match self.specs.iter_mut().find(|s| s.id == spec.id) {
            Some(existing) => *existing = spec,
            None => self.specs.push(spec),
        }
    }

    pub fn get(&self, id: &PanelId) -> Option<&PanelSpec> {
        self.specs.iter().find(|s| s.id == *id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PanelSpec> {
        self.specs.iter()
    }

    /// Title for `id`, falling back to the id itself for unregistered (e.g. removed) panels.
    pub fn title(&self, id: &PanelId) -> SharedString {
        self.get(id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| SharedString::from(id.to_string()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusSide {
    Left,
    Right,
}

/// One value per dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PerDock<T> {
    pub left: T,
    pub right: T,
    pub bottom: T,
}

impl<T> PerDock<T> {
    pub fn get(&self, pos: DockPosition) -> &T {
        match pos {
            DockPosition::Left => &self.left,
            DockPosition::Right => &self.right,
            DockPosition::Bottom => &self.bottom,
        }
    }
}

/// What the dock button of the status bar shows about its dock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DockState {
    /// The dock is showing: the button is drawn pressed.
    pub visible: bool,
    /// The dock holds at least one panel. A hidden dock without panels has nothing to show, so
    /// its button is disabled.
    pub has_panels: bool,
}

/// How much room the window leaves the status bar (FX-STATUSBAR): the `workspace` item shows
/// only its branch below 900 px, and below 700 px only the `connection` dot and `attention`
/// stay on the built-in bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarWidth {
    /// 900 px and wider: everything.
    Wide,
    /// 700 to 900 px: `workspace` is its branch.
    Medium,
    /// Under 700 px.
    Narrow,
}

impl BarWidth {
    /// Below this the `workspace` item shows only the branch.
    pub const MEDIUM_BELOW: f32 = 900.0;
    /// Below this only `connection` and `attention` stay.
    pub const NARROW_BELOW: f32 = 700.0;

    /// The tier of a window `width` px wide.
    pub fn of(width: f32) -> Self {
        if width < Self::NARROW_BELOW {
            Self::Narrow
        } else if width < Self::MEDIUM_BELOW {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

/// Everything a status item needs to draw itself.
#[derive(Debug)]
pub struct StatusContext<'a> {
    pub theme: &'a Theme,
    pub density: Density,
    /// Key hint of the palette action ("⌘⇧P"), from the live keymap.
    pub palette_hint: SharedString,
    /// Key hint of the "jump to next attention" action.
    pub attention_hint: SharedString,
    /// Key hint of each dock's toggle action, from the live keymap (empty when unbound).
    pub dock_hints: PerDock<SharedString>,
    pub data: &'a StatusData,
    /// The tier of the window's width.
    pub width: BarWidth,
    /// The views of `editor.position` and `editor.mode`, which follow the focused code view.
    pub editor_items: &'a super::editor_status::EditorItems,
    /// Height of the status bar.
    pub height: f32,
}

/// Live numbers the status items show.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusData {
    /// The `connection` item: the daemon's state as a mark, a word and a tooltip.
    pub connection: super::panels::ConnectionChip,
    pub sessions: usize,
    /// Name of the project the user works in (else the workspace the window shows).
    pub workspace: Option<String>,
    /// The branch checked out there, once the daemon said.
    pub branch: Option<String>,
    /// Sessions with a request waiting (the *Requests* group).
    pub requests: usize,
    /// Agent sessions at work (the *Working* group): a turn runs, or the agent says so.
    pub working: usize,
    /// Permission requests of agent chats waiting for an answer.
    pub approvals: usize,
    /// Name of the terminal theme in effect.
    pub terminal_theme: String,
    /// Visibility and content of the three docks.
    pub docks: PerDock<DockState>,
    /// The focused file tab's language and whether Vim is on in it; `None` when no file tab
    /// has focus.
    pub editor: Option<EditorStatus>,
    /// The `usage` item: the tightest limit of any harness; `None` until one reported.
    pub usage: Option<crate::sessions::usage::StatusText>,
}

/// What the editor items know of the focused file tab at the workbench's level. Its line and
/// column and its Vim mode follow the caret, so `editor.position` and `editor.mode` read them
/// from the code view themselves ([`EditorItems`](super::editor_status::EditorItems)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorStatus {
    /// `Rust`, `Plain Text · large file`.
    pub language: String,
    /// `editor.vim_mode` is on: `editor.mode` shows.
    pub vim: bool,
    /// The tab wraps long lines: `editor.wrap` shows.
    pub wrap: bool,
    /// The file's diagnostics and its language server (`editor.diagnostics`, E-12a).
    pub diagnostics: EditorDiagnostics,
}

/// What `editor.diagnostics` shows for the focused file tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorDiagnostics {
    /// How many of each severity, once the file has a language server.
    pub counts: Option<hutan_code_view::DiagnosticCounts>,
    /// The file's language server and its state.
    pub server: Option<EditorServer>,
}

/// A language server as the status item shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorServer {
    pub name: String,
    pub state: hutan_protocol::lsp::LspServerState,
    /// Why (a failure's last error, idle, the setting to change), for the tooltip.
    pub reason: Option<String>,
}

pub type StatusRender = Rc<dyn Fn(&StatusContext<'_>) -> AnyElement>;

#[derive(Clone)]
pub struct StatusItemSpec {
    pub id: &'static str,
    pub default_side: StatusSide,
    pub render: StatusRender,
}

impl std::fmt::Debug for StatusItemSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusItemSpec")
            .field("id", &self.id)
            .field("default_side", &self.default_side)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct StatusItemRegistry {
    specs: Vec<StatusItemSpec>,
}

impl StatusItemRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: StatusItemSpec) {
        match self.specs.iter_mut().find(|s| s.id == spec.id) {
            Some(existing) => *existing = spec,
            None => self.specs.push(spec),
        }
    }

    pub fn get(&self, id: &str) -> Option<&StatusItemSpec> {
        self.specs.iter().find(|s| s.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &StatusItemSpec> {
        self.specs.iter()
    }
}

/// Ordered status item ids per side (`layout.presets.<name>.status_bar` in design.md). Unknown ids
/// are skipped at render time so a layout can reference items that are not registered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarLayout {
    pub left: Vec<String>,
    pub right: Vec<String>,
}

impl StatusBarLayout {
    /// Adds `id` on the right, before `working` or the palette's key, unless the bar names it.
    pub fn with_item(&mut self, id: &str) {
        if self.left.iter().chain(&self.right).any(|item| item == id) {
            return;
        }
        let at = self
            .right
            .iter()
            .position(|item| item == "working" || item == "palette_hint")
            .unwrap_or(self.right.len());
        self.right.insert(at, id.to_owned());
    }
}

impl Default for StatusBarLayout {
    /// The status bar of the built-in presets (FX-STATUSBAR): the daemon's state, the project
    /// and its branch and the requests that wait on the left; the editor's items while a file
    /// tab has focus, the agents at work and the palette's key on the right. The other items
    /// (`sessions`, `approvals`, `density`, `theme`, the dock buttons) stay registered for
    /// presets that name them.
    fn default() -> Self {
        Self {
            left: ["connection", "workspace", "attention"]
                .map(str::to_owned)
                .to_vec(),
            right: [
                "editor.diagnostics",
                "editor.mode",
                "editor.position",
                "editor.wrap",
                "editor.language",
                "working",
                "palette_hint",
            ]
            .map(str::to_owned)
            .to_vec(),
        }
    }
}
