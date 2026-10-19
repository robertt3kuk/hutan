//! `Workbench`: the GPUI entity owning the layout model, registries, settings wiring, and
//! transient UI state (palette, context menu, modal, settings sheet, resize drags). Rendering
//! lives in `render.rs`; session and settings behavior in sibling modules.

use super::actions::{self, Command};
use super::agents::AgentPicker;
use super::chat::{ChatModel, ChatView};
use super::daemon::DaemonControl;
use super::mac::MacControl;
use super::modal::Modal;
use super::model::{
    DockMode, DockPosition, DropTarget, Edge, FocusTarget, ItemRef, ItemSource, LayoutPreset,
    PaneId, PanelDest, PanelId, Responsive, WorkbenchState,
};
use super::palette::PaletteState;
use super::panels::{builtin_panels, builtin_status_items};
use super::persistence;
use super::registry::{PanelEnv, PanelRegistry, StatusItemRegistry};
use super::settings_sheet::SettingsSheet;
use super::style::{TerminalStyle, UiStyle};
use super::terminals::TerminalItem;
use super::workspaces::PickerState;
use crate::cli_link::CliLink;
use crate::harness_install::PackageRunner;
use crate::integrations::IntegrationInstaller;
use crate::macos::MacServices;
use crate::sessions::{
    RecoverySummaryDue, SessionsStore, StoreChanged, TerminalSession, UiOpenRequested,
};
use crate::settings::{AppearanceSettings, LayoutSettings, PresetDef, TerminalSettings};
use crate::theme::{Density, DensityExt as _, Theme};
use gpui::Window;
use gpui::{
    AnyView, App, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, Pixels, Point,
    Render, SharedString, Subscription, Task, WindowAppearance, div, prelude::*, px,
};
use hutan_client::Connection;
use hutan_core::{ApprovalId, SessionId};
use hutan_settings::{AppearanceMode, SettingsStore, WatchGuard};
use hutan_terminal_view::DefaultColors;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// Wait this long after the last layout change before writing `desktop-state.json`.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// Keyboard resize steps.
const DOCK_STEP: f32 = 24.0;
const SPLIT_STEP: f32 = 0.05;

/// Everything needed to build a workbench.
#[derive(Debug)]
pub struct WorkbenchOptions {
    pub settings: SettingsStore,
    pub connection: Connection,
    pub panels: PanelRegistry,
    pub status_items: StatusItemRegistry,
    /// Where layout state is restored from and saved to; `None` disables persistence.
    pub state_path: Option<PathBuf>,
    /// Skip loading and start from this state (tests).
    pub initial_state: Option<WorkbenchState>,
    /// What the workbench uses to reach macOS: login item, menu bar item, Dock tile, global
    /// shortcut. Inert unless given real services (`app::run`) or fakes (tests).
    pub mac: MacServices,
    /// Show the first-run setup when `app.setup_done` is not set (`app::run`; tests ask for it).
    pub first_run_setup: bool,
    /// The link the setup's Command line step may make; `None` outside `Hutan.app`.
    pub cli_link: Option<CliLink>,
    /// Runs `hutan integration` for the agent integrations; `None` installs nothing.
    pub installer: Option<Arc<dyn IntegrationInstaller>>,
    /// Checks and installs missing harness packages (the "Install…" buttons); `None` offers
    /// none.
    pub package_runner: Option<Arc<dyn PackageRunner>>,
    /// Hutan runs with its own data directory (`HUTAN_HOME`): the agents' hooks still go into
    /// their real configuration directories, which the setup says.
    pub custom_home: bool,
}

impl WorkbenchOptions {
    /// Built-in panels and status items over `settings` and `connection`.
    pub fn builtin(
        settings: SettingsStore,
        connection: Connection,
        state_path: Option<PathBuf>,
    ) -> Self {
        Self {
            settings,
            connection,
            panels: builtin_panels(),
            status_items: builtin_status_items(),
            state_path,
            initial_state: None,
            mac: MacServices::inert(),
            first_run_setup: false,
            cli_link: None,
            installer: None,
            package_runner: None,
            custom_home: std::env::var_os("HUTAN_HOME").is_some_and(|home| !home.is_empty()),
        }
    }

    /// The same options with these macOS services.
    pub fn with_mac(mut self, mac: MacServices) -> Self {
        self.mac = mac;
        self
    }

    /// The same options showing the first-run setup until it is done (`app.setup_done`).
    pub fn with_first_run_setup(mut self) -> Self {
        self.first_run_setup = true;
        self
    }

    /// The same options with the link the Command line step may make.
    pub fn with_cli_link(mut self, link: Option<CliLink>) -> Self {
        self.cli_link = link;
        self
    }

    /// The same options offering to install missing harness packages through `runner`.
    pub fn with_package_runner(mut self, runner: Arc<dyn PackageRunner>) -> Self {
        self.package_runner = Some(runner);
        self
    }

    /// The same options installing agent integrations through `installer`.
    pub fn with_installer(mut self, installer: Arc<dyn IntegrationInstaller>) -> Self {
        self.installer = Some(installer);
        self
    }

    /// The same options saying (or not) that Hutan runs from a custom `HUTAN_HOME`.
    pub fn with_custom_home(mut self, custom_home: bool) -> Self {
        self.custom_home = custom_home;
        self
    }
}

/// Where a session's agent badge is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BadgeSite {
    /// A row of the sessions panel.
    Row,
    /// A tab of a center pane.
    Tab,
    /// A card of the overview.
    Card,
}

/// A laid-out area of the workbench whose painted bounds are recorded every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Titlebar,
    StatusBar,
    /// A status item, by registry id (`connection`, `sessions`, …).
    StatusItem(&'static str),
    /// A control of the daemon popover, by element id (`daemon-stop`, `keep_running`, …).
    Popover(&'static str),
    Center,
    Dock(DockPosition),
    Pane(PaneId),
    /// Tab `index` of a center pane.
    Tab(PaneId, usize),
    /// Panel tab `index` of a dock (for the right dock: icon `index` of its strip).
    DockTab(DockPosition, usize),
    /// An entry of the left sidebar's nav, by name (`search`, `tasks`, `agents`, `assistant`,
    /// `projects` for the back affordance of a page) or by panel id for any other page.
    Nav(&'static str),
    /// A control of the left sidebar's bottom toolbar (`new`, `settings`).
    Toolbar(&'static str),
    /// The Agent Dashboard drawer.
    AgentsDrawer,
    /// The header of a column of the drawer's board.
    DrawerColumn(super::agents_drawer::DashboardColumn),
    /// The card of a session in the drawer's board.
    DrawerCard(hutan_core::SessionId),
    /// The right strip's More menu button.
    StripMore,
    /// The rail of a side dock (the left 44 pt icon rail, the right 40 pt activity rail).
    Rail(DockPosition),
    /// The titlebar button of a side dock (the sidebar toggle at the traffic lights, the right
    /// dock's).
    DockToggle(DockPosition),
    /// The preview of a drag-to-collapse that will collapse the dock to its rail on release.
    CollapsePreview(DockPosition),
    /// A container row (workspace, project or Home) of the sessions panel.
    WorkspaceRow(hutan_core::WorkspaceId),
    /// The "No workspace" label of the sessions panel: where a project is dropped to leave its
    /// workspace.
    NoWorkspace,
    /// A control of the sessions panel's header, by name (`plus`, `sort`).
    SidebarControl(&'static str),
    /// A control of the history panel, by name (`search`, `enable`, `disable`, `clear`,
    /// `rebuild`, `confirm`).
    History(&'static str),
    /// A repeated control of the history panel: its name (`row`, `chevron`, `more`, `group`,
    /// `menu`, `source`, `action`, `show-more`) and index (the row, group, entry or button
    /// counting from 0).
    HistoryItem(&'static str, usize),
    /// A control of the chat view, by name (`composer`, `send`, `interrupt`, `restart`,
    /// `header`, `banner`).
    Chat(&'static str),
    /// Entry `index` of a chat's transcript.
    ChatEntry(usize),
    /// A button of a permission request that waits for an answer: `request * 8 + option`, the
    /// requests counted oldest first.
    ChatOption(usize),
    /// A control of a question card that waits for an answer (protocol 0.24): `0` Submit, `1`
    /// Decline, `2` Cancel, `8 + n` field `n`, `100 + n * 20 + k` option `k` of field `n`.
    ChatQuestion(usize),
    /// Context chip `index` of the composer.
    ChatChip(usize),
    /// Structured context chip `index` of the composer (assistant chats).
    ChatContextChip(usize),
    /// A control of the attach form of an assistant chat, by name (`button`, `close`, `field`,
    /// `confirm`, `mention`, `shell`, `agent`, `file`, `search`, …).
    Attach(&'static str),
    /// Row `index` of the attach form's list (terminals, blocks, transcripts), top to bottom.
    AttachRow(usize),
    /// A control of Mawas Home, by name (`scope`, `backend`, `mode`, `new`, `stop`, `rail`,
    /// `retry`, …).
    Assistant(&'static str),
    /// A repeated control of Mawas Home's Today rail: its name (`today-row`, `today-option`,
    /// `today-open`, `today-working`) and index (the row; for an option `row * 8 + option`).
    AssistantItem(&'static str, usize),
    /// A control of the Memory page, by name (`add`, `scope-all`, `scope-global`,
    /// `scope-workspace`, `archived`, `editor`, `save`, `cancel`, `error`, …).
    Memory(&'static str),
    /// A repeated control of the Memory page: its name (`entry`, `edit`, `archive`, `restore`,
    /// `confirm`, `history`, `proposal`, `approve`, `edit-approve`, `reject`) and the index of
    /// its row (entries and proposals count separately, in the order drawn).
    MemoryItem(&'static str, usize),
    /// The control that opens or closes part `part` of entry `index` (a command's output, a
    /// file's diff, reasoning).
    ChatPart(usize, u32),
    /// The Copy button of code block `n` of entry `index`.
    ChatCopy(usize, usize),
    /// An answer button of the inspector's approvals tab, counting the requests oldest first
    /// like `ChatOption`.
    InspectorOption(usize),
    /// A control of the Switch model dialog, by name (`start`, `cancel`).
    AgentPicker(&'static str),
    /// A control of the Start… sheet, by name (`project`, `prompt`, `title`, `command`, `branch`,
    /// `base`, `system`, `model`, `more`, `shared-warning`, `add-provider`, `start`,
    /// `start-new`, `cancel`).
    Launcher(&'static str),
    /// An option of a row of the Start… sheet: the row (`what`, `agent`, `install`, `login`,
    /// `where`, `mode`) and its key (a kind, a harness id, `main`, `new`, `now`, `queue`).
    LauncherChoice(&'static str, &'static str),
    /// A control of the settings sheet's pages, by name (`reset-page`, `history-index`,
    /// `restart-daemon`, `stop-daemon`, `revoke-all`, `open-keymap`, `reload-keymap`,
    /// `env-add`, `preset-save`).
    Settings(&'static str),
    /// Row `index` of a list of the settings sheet: `layer` (the header's switch), `reveal` and
    /// `open` (the Data & logs page, per `DataTarget`), `hooks-install`, `hooks-repair` and
    /// `hooks-uninstall` (per agent), `revoke` (per remembered choice), `env-remove`,
    /// `preset-rename`, `preset-delete`.
    SettingsItem(&'static str, usize),
    /// Entry `index` of a list of the Start… sheet (`project`, `provider`, `model`, `worktree`,
    /// `after`), top to bottom.
    LauncherItem(&'static str, usize),
    /// The hover × of a session's row in the sidebar or card in the Agent Dashboard.
    SessionClose(hutan_core::SessionId),
    /// A control of the "Closed … · Undo" toast (`undo`).
    CloseToast(&'static str),
    /// The ▶ Start button of project row `workspace` in the sidebar.
    ProjectStart(hutan_core::WorkspaceId),
    /// A control of the Accounts page: harness id (or `*`) and the control (`login`, `logout`, `card`, `refresh`).
    Account(&'static str, &'static str),
    /// A control of the first-run setup, by name (`continue`, `back`, `skip`, `appearance-dark`,
    /// `keep-running`, `start-at-login`, `notifications`, `cli-link`, …).
    Setup(&'static str),
    /// A control of the setup's Agents step for harness `id`: `login`, `install-guide`,
    /// `uninstall`, `backend`.
    SetupHarness(&'static str, &'static str),
    /// Model row `index` of the Switch model dialog.
    AgentModel(usize),
    /// Node row `index` of the agent monitor, counting every session's rows top to bottom.
    MonitorRow(usize),
    /// The fold control of node row `index` of the agent monitor.
    MonitorFold(usize),
    /// A filter chip of the agent monitor (`finished`, `blocked`, `scope`).
    MonitorChip(&'static str),
    /// Line `index` of the resources panel's table (the `hutand` row, sessions, processes and
    /// agents, top to bottom).
    ResourceRow(usize),
    /// The `⋯` menu button of line `index` of the resources panel.
    ResourceMenu(usize),
    /// The fold control of line `index` of the resources panel.
    ResourceFold(usize),
    /// A control of the resources panel (`panel`, `filter`, `sort-cpu`, `sort-memory`,
    /// `sort-name`).
    ResourceControl(&'static str),
    /// Entry `index` of the open context menu, top to bottom.
    MenuEntry(usize),
    /// The chip a restarted session wears (`respawned`, `resumed`, `interrupted`, `not
    /// restarted`) at one of its places.
    RecoveryChip(BadgeSite, SessionId),
    /// A control of the restart summary, by name (`sheet`, `close`, `dismiss-all`).
    Recovery(&'static str),
    /// A control of row `index` of the restart summary: `open`, `action` (Retry last message or
    /// Restart), `dismiss`.
    RecoveryRow(usize, &'static str),
    /// The agent badge of a session at one of its places.
    AgentBadge(BadgeSite, SessionId),
    /// The placement badge (`main` / `wt <branch>`) of a session at one of its places.
    PlacementBadge(BadgeSite, SessionId),
    /// A control of the tasks panel, by name (`new`, `chip-done`, `chip-scope`, `form-title`,
    /// `form-prompt`, `form-command`, `form-branch`, `form-create`, `form-cancel`, `form-auto`,
    /// `run-terminal`, `run-codex`, `run-omp`, `run-claude`, `place-main`, `place-worktree`, `cascade-yes`,
    /// `cascade-no`, `empty-new`).
    Tasks(&'static str),
    /// Row `index` of the tasks panel's tree, top to bottom.
    TaskRow(usize),
    /// An action button of row `index` of the tasks panel (`start`, `done`, `cancel`, `reset`,
    /// `delete`, `changes`).
    TaskAction(usize, &'static str),
    /// Entry `index` of the new-task form's "After" list.
    TaskDep(usize),
    /// The delivery pill of row `index` of the tasks panel's tree (`Awaiting review`, …).
    TaskDelivery(usize),
    /// Row `index` of the right sidebar's Changes tab (the checkouts, top to bottom).
    ChangesCheckout(usize),
    /// A control of the Changes view, by name (`toggle`, `refresh`, `merge`, `merge-confirm`,
    /// `merge-cancel`, `strategy-merge`, `strategy-squash`, `strategy-rebase`, `message`,
    /// `remove`, `keep`; delivery: `delivery`, `accept`, `request-changes`, `request-note`,
    /// `request-send`, `request-cancel`, `commit-all`, `commit-message`, `commit-confirm`,
    /// `commit-cancel`).
    Changes(&'static str),
    /// File `index` of the Changes view's list, top to bottom.
    ChangesFile(usize),
    /// Hunk header `index` of the shown file's diff, top to bottom.
    ChangesHunk(usize),
    /// The number gutter of diff line `index` (an index into the shown file's diff lines) of
    /// the Changes view: clicking it selects the line.
    ChangesLine(usize),
    /// A control of comment `index` of the Changes view's review (in the order of the Review
    /// tray): `card`, `edit`, `delete`, `send`, `resolve`.
    ChangesComment(usize, &'static str),
    /// Row `index` of the Review tray's list of comments.
    ChangesTray(usize),
    /// Sent batch `index` of the Review tray's list.
    ChangesBatch(usize),
    /// Entry `index` of the "Send to agent ▾" menu's agents.
    ChangesTarget(usize),
}

/// A pane tab or dock tab being dragged.
#[derive(Clone, Debug)]
pub struct DraggedItem {
    pub source: ItemSource,
    pub item: ItemRef,
    pub label: SharedString,
}

/// The floating chip that follows the cursor during a drag.
#[derive(Debug)]
pub struct DragPreview {
    pub label: SharedString,
    pub style: UiStyle,
}

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.style.theme;
        div()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(self.style.density.ui().radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.accent)
            .text_color(theme.fg)
            .text_size(px(self.style.density.ui().font_size))
            .child(self.label.clone())
    }
}

/// An in-progress drag of a splitter or dock edge.
#[derive(Clone, Debug)]
pub(crate) enum Resizing {
    Dock {
        pos: DockPosition,
        start_mouse: f32,
        start_size: f32,
        /// A side dock was dragged 24 pt below its minimum: releasing collapses it to its rail.
        armed: bool,
    },
    Split {
        path: Vec<usize>,
        divider: usize,
        horizontal: bool,
        origin: f32,
        extent: f32,
    },
}

pub(crate) type MenuAction = Rc<dyn Fn(&mut Workbench, &mut Window, &mut Context<Workbench>)>;

#[derive(Clone)]
pub(crate) struct MenuEntry {
    pub label: SharedString,
    pub checked: bool,
    pub disabled: bool,
    pub run: MenuAction,
}

#[derive(Clone)]
pub(crate) struct ContextMenu {
    pub position: Point<Pixels>,
    pub entries: Vec<MenuEntry>,
}

pub struct Workbench {
    pub(crate) state: WorkbenchState,
    pub(crate) panels: PanelRegistry,
    pub(crate) status_items: StatusItemRegistry,
    pub(crate) preset_defs: Vec<PresetDef>,
    pub(crate) presets: Vec<LayoutPreset>,
    pub(crate) commands: Rc<Vec<Command>>,
    pub(crate) density: Density,
    pub(crate) chrome: AppearanceMode,
    pub(crate) appearance: WindowAppearance,
    pub(crate) theme: Theme,
    pub(crate) settings: SettingsStore,
    pub(crate) layout_seen: Arc<LayoutSettings>,
    pub(crate) terminal_settings: Arc<TerminalSettings>,
    pub(crate) terminal_style: hutan_terminal_view::TerminalViewSettings,
    pub(crate) sessions: Entity<SessionsStore>,
    pub(crate) env: PanelEnv,
    pub(crate) terminals: HashMap<(PaneId, SessionId), TerminalItem>,
    /// The transcript and composer of every chat a tab shows (see `agents.rs`).
    pub(crate) chat_models: HashMap<SessionId, Entity<ChatModel>>,
    pub(crate) chats: HashMap<(PaneId, SessionId), Entity<ChatView>>,
    pub(crate) rendered_chats: std::collections::HashSet<(PaneId, SessionId)>,
    pub(crate) pruned_chats: Vec<Entity<ChatView>>,
    /// The chat the user looked at last: where "Send to agent" goes.
    pub(crate) last_chat: Option<SessionId>,
    /// The user opened or focused the Assistant panel after the last chat tab they looked at:
    /// "Send to agent" goes to the assistant the panel shows.
    pub(crate) assistant_last: bool,
    /// Permission requests this window has already seen (a new one may reveal the inspector).
    pub(crate) known_approvals: std::collections::HashSet<ApprovalId>,
    /// The Switch model dialog of a running chat.
    pub(crate) agent_picker: Option<AgentPicker>,
    /// The Start… sheet (`launcher.rs`).
    pub(crate) launcher: Option<super::launcher::Launcher>,
    pub(crate) launcher_focus: FocusHandle,
    pub(crate) providers: super::providers::ProvidersState,
    /// What the settings sheet knows about the history index.
    pub(crate) history_index: super::settings_pages::HistoryIndex,
    /// What the settings sheet knows about `daemon.toml` (`settings_daemon.rs`).
    pub(crate) daemon_settings: super::settings_daemon::DaemonSettingsState,
    /// What the Assistant page knows about the backends of the assistants (`settings_assistant.rs`).
    pub(crate) assistant_defaults: super::settings_assistant::AssistantDefaults,
    /// What Mawas may do without asking (`control.grants.list`), for Settings → Mawas.
    pub(crate) mawas_grants: super::settings_assistant::MawasGrants,
    pub(crate) agent_focus: FocusHandle,
    pub(crate) held: HashMap<SessionId, Entity<TerminalSession>>,
    /// Folded blocks per terminal, saved with the layout (see [`persistence::CollapsedBlocks`]).
    pub(crate) collapsed_blocks: persistence::CollapsedBlocks,
    /// Review comments of the Changes views per checkout directory, saved with the layout (see
    /// [`persistence::ReviewComments`]).
    pub(crate) review_comments: persistence::ReviewComments,
    /// What the Start… sheet remembers, saved with the layout (see
    /// [`persistence::LauncherMemory`]).
    pub(crate) launcher_memory: persistence::LauncherMemory,
    /// What tells the workbench a terminal's folds changed, per held session.
    pub(crate) collapse_watch: HashMap<SessionId, Subscription>,
    pub(crate) window_active: bool,
    pub(crate) rendered_terminals: std::collections::HashSet<(PaneId, SessionId)>,
    pub(crate) pruned: Vec<Entity<hutan_terminal_view::TerminalView>>,
    /// The root whose `.hutan/settings.toml` is the workspace layer (the active workspace's
    /// first project, or Home's folder).
    pub(crate) workspace_root: Option<PathBuf>,
    /// The root whose `.hutan/settings.toml` is the project layer.
    pub(crate) project_root: Option<PathBuf>,
    /// The project the user last chose (a project row, a session): the active project while no
    /// focused session says otherwise.
    pub(crate) chosen_project: Option<hutan_core::WorkspaceId>,
    /// The project last chosen inside each workspace, for when the window comes back to it.
    pub(crate) remembered_projects: HashMap<hutan_core::WorkspaceId, hutan_core::WorkspaceId>,
    /// The session that had focus when the active project was last derived from it.
    pub(crate) focus_seen: Option<SessionId>,
    /// The container the active project was last derived for: a focus change that comes with a
    /// switch of container (the restored layout's focused tab) says nothing about the project.
    pub(crate) synced_container: Option<hutan_core::WorkspaceId>,
    /// A workspace this window just opened whose events have not reached the store yet; it is
    /// shown anyway instead of falling back to Home.
    pub(crate) pending_workspace: Option<hutan_core::WorkspaceId>,
    /// The ⌘O project picker or ⌘J jump palette.
    pub(crate) picker: Option<PickerState>,
    /// The ⌘J `?` history search in flight (dropping it cancels it).
    pub(crate) picker_search: Option<gpui::Task<()>>,
    pub(crate) picker_focus: FocusHandle,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) palette_focus: FocusHandle,
    pub(crate) menu_focus: FocusHandle,
    pub(crate) modal_focus: FocusHandle,
    pub(crate) sheet_focus: FocusHandle,
    pub(crate) daemon_focus: FocusHandle,
    /// Holds the keyboard while the Agent Dashboard drawer is open, so Escape closes it.
    pub(crate) drawer_focus: FocusHandle,
    /// The Agent Dashboard drawer (the Agents nav row) is open.
    pub(crate) agents_drawer: bool,
    pub(crate) daemon: DaemonControl,
    pub(crate) mac: MacControl,
    pub(crate) setup: super::setup::SetupControl,
    pub(crate) integrations: super::integrations::IntegrationControl,
    pub(crate) harness_install: super::harness_install::HarnessInstallControl,
    pub(crate) palette: Option<PaletteState>,
    /// Vim state of the text fields of the overlays below (`editor.vim_mode`).
    pub(crate) edits: super::vim_ui::OverlayEdits,
    pub(crate) context_menu: Option<ContextMenu>,
    pub(crate) modal: Option<Modal>,
    /// Where the keyboard was when the open dialog took it; restored when it closes.
    pub(crate) modal_return: Option<gpui::WeakFocusHandle>,
    pub(crate) sheet: Option<SettingsSheet>,
    pub(crate) resizing: Option<Resizing>,
    /// What the window itself does to the side docks: the transient auto-collapse
    /// (`layout.auto_collapse`), never saved.
    pub(crate) responsive: Responsive,
    /// The mode every dock had after the last model change, to see which one the user opened.
    pub(crate) dock_modes_seen: [DockMode; 3],
    /// One handle per dock (left, right, bottom) around what it renders: tells whether the
    /// keyboard is inside a dock whose body is no longer drawn.
    pub(crate) dock_focus: [FocusHandle; 3],
    /// The dock the model's focus was in after the last model change.
    pub(crate) focus_dock_seen: Option<DockPosition>,
    pub(crate) panel_views: HashMap<PanelId, AnyView>,
    /// The Changes view of every checkout a tab shows (see `review.rs`), by directory.
    pub(crate) changes_views: HashMap<String, Entity<super::changes::ChangesView>>,
    pub(crate) changes_subscriptions: Vec<Subscription>,
    pub(crate) notice: Option<SharedString>,
    /// "Closed … · Undo" (`closing.rs`).
    pub(crate) close_toast: Option<super::closing::CloseToast>,
    pub(crate) toast_rev: u64,
    /// The restart summary is on screen (`recovery_summary.rs`).
    pub(crate) recovery_open: bool,
    /// A large sidebar's one-row summary was opened into the full list ("Review").
    pub(crate) recovery_review: bool,
    pub(crate) last_title: String,
    pub(crate) regions: super::regions::Regions,
    state_path: Option<PathBuf>,
    last_saved: Option<String>,
    save_task: Option<Task<()>>,
    _watch: Option<WatchGuard>,
    /// What publishes this window to the daemon's control surface and executes its commands
    /// (`remote.rs`).
    pub(crate) remote: super::remote::RemoteControl,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Workbench {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workbench")
            .field("state", &self.state)
            .field("density", &self.density)
            .field("palette", &self.palette)
            .finish_non_exhaustive()
    }
}

impl Focusable for Workbench {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workbench {
    pub fn new(options: WorkbenchOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = options.settings;
        let first_run_setup = options.first_run_setup;
        crate::settings::register_all(&settings);
        let (mac, mac_events) = MacControl::new(options.mac);
        let appearance_settings = settings.get::<AppearanceSettings>();
        let layout_settings = settings.get::<LayoutSettings>();
        let terminal_settings = settings.get::<TerminalSettings>();
        let preset_defs = layout_settings.preset_defs();
        let presets: Vec<LayoutPreset> = preset_defs.iter().map(|d| d.layout.clone()).collect();

        let fresh = options.initial_state.is_none()
            && options
                .state_path
                .as_ref()
                .is_none_or(|path| !path.exists());
        let mut state = match (options.initial_state, &options.state_path) {
            (Some(state), _) => state,
            (None, Some(path)) => persistence::load_state(path),
            (None, None) => WorkbenchState::default(),
        };
        if fresh
            && layout_settings.preset != LayoutPreset::DEFAULT
            && let Some(def) = preset_defs
                .iter()
                .find(|d| d.layout.name == layout_settings.preset)
        {
            let presets: Vec<LayoutPreset> = preset_defs.iter().map(|d| d.layout.clone()).collect();
            let _ = state.switch_preset(&def.layout.name, &presets);
        }

        let window_appearance = window.appearance();
        let theme = Theme::for_chrome(appearance_settings.appearance, window_appearance);
        let terminal_style = super::terminals::terminal_style(
            &terminal_settings,
            theme.system_appearance(),
            &theme,
            appearance_settings.density,
        );
        cx.set_global(UiStyle {
            theme,
            density: appearance_settings.density,
        });
        cx.set_global(TerminalStyle(terminal_style.clone()));

        let sessions = cx.new(|cx| SessionsStore::new(options.connection, cx));
        sessions.update(cx, |store, cx| {
            store.set_terminal_colors(DefaultColors::from(&terminal_style.palette), cx);
        });
        let regions = super::regions::Regions::default();
        let harnesses = cx.new(|cx| super::harnesses::HarnessDirectory::new(sessions.clone(), cx));
        let env = PanelEnv {
            sessions: sessions.clone(),
            settings: settings.clone(),
            harnesses: harnesses.clone(),
            regions: regions.clone(),
        };

        let commands = Rc::new(actions::commands(
            &presets,
            &options.panels,
            settings.get::<crate::settings::AssistantSettings>().enabled,
        ));
        let focus_handle = cx.focus_handle();
        let palette_focus = cx.focus_handle();
        let picker_focus = cx.focus_handle();
        let menu_focus = cx.focus_handle();
        let modal_focus = cx.focus_handle();
        let sheet_focus = cx.focus_handle();
        let daemon_focus = cx.focus_handle();
        let drawer_focus = cx.focus_handle();
        let dock_focus = [cx.focus_handle(), cx.focus_handle(), cx.focus_handle()];
        let dock_modes_seen = [
            state.docks.left.mode(),
            state.docks.right.mode(),
            state.docks.bottom.mode(),
        ];
        let agent_focus = cx.focus_handle();
        let launcher_focus = cx.focus_handle();
        window.focus(&focus_handle, cx);
        // Closing the last window starts the quit flow (the window stays until it is decided);
        // either way pending layout changes are written now instead of after the debounce.
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.window_close_requested(window, cx))
                .unwrap_or(true)
        });

        let watch = match settings.start_watching() {
            Ok(guard) => Some(guard),
            Err(err) => {
                tracing::warn!("settings hot reload is off: {err}");
                None
            }
        };
        let mut changes = settings.changes();
        let settings_task = cx.spawn_in(window, async move |this, cx| {
            while changes.changed().await.is_ok() {
                let changed = changes.borrow_and_update().clone();
                let alive = this.update_in(cx, |this, window, cx| {
                    this.on_settings_changed(&changed, window, cx);
                    this.schedule_publish(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let events_task = mac_events.map(|mut events| {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(event) = events.recv().await {
                    let alive = this.update_in(cx, |this, window, cx| {
                        this.on_mac_event(event, window, cx);
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
        });

        let modal_guard = {
            let this = cx.weak_entity();
            let window_handle = window.window_handle();
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != window_handle {
                    return;
                }
                let modifiers = &event.keystroke.modifiers;
                let shortcut = modifiers.platform || modifiers.control || modifiers.alt;
                // ⌘Q stays: quitting must always work.
                let quit = modifiers.platform && event.keystroke.key == "q";
                if !shortcut || quit {
                    return;
                }
                let setup_open = this
                    .upgrade()
                    .is_some_and(|wb| wb.read(cx).setup_blocks_shortcuts());
                if setup_open {
                    cx.stop_propagation();
                }
            })
        };
        let subscriptions = vec![
            modal_guard,
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            cx.on_app_quit(|this, cx| this.on_platform_quit(cx)),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, change: &StoreChanged, window, cx| {
                    // The layout follows sessions, workspaces, agents and the connection;
                    // attention and tasks only change what the chrome counts and
                    // shows; answers given here and the like are for whoever shows them.
                    if change.intersects(
                        StoreChanged::CONNECTION
                            | StoreChanged::SESSIONS
                            | StoreChanged::SESSION_INFO
                            | StoreChanged::WORKSPACES
                            | StoreChanged::AGENTS
                            | StoreChanged::ACTIVE
                            | StoreChanged::FOCUS,
                    ) {
                        this.on_sessions_changed(window, cx);
                    } else if change.intersects(StoreChanged::ATTENTION | StoreChanged::TASKS) {
                        cx.notify();
                    } else {
                        return;
                    }
                    this.schedule_publish(cx);
                },
            ),
            cx.observe(&harnesses, |this, _, cx| this.on_harnesses_changed(cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                this.window_active = window.is_window_active();
                this.sync_session_focus(cx);
                this.refresh_login_status(cx);
                this.schedule_publish(cx);
            }),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, event: &UiOpenRequested, window, cx| {
                    this.on_ui_open(event, window, cx);
                },
            ),
            cx.subscribe(&sessions, |this, _, _: &RecoverySummaryDue, cx| {
                this.show_recovery_summary(cx);
            }),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, event: &crate::sessions::SessionClosed, window, cx| {
                    this.on_session_closed(event.session_id, window, cx);
                },
            ),
        ];

        let remote = super::remote::RemoteControl::start(&sessions, window, cx);
        let mut this = Self {
            last_saved: options
                .state_path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok()),
            state,
            panels: options.panels,
            status_items: options.status_items,
            preset_defs,
            presets,
            commands,
            density: appearance_settings.density,
            chrome: appearance_settings.appearance,
            appearance: window_appearance,
            theme,
            settings,
            layout_seen: layout_settings,
            terminal_settings,
            terminal_style,
            sessions,
            env,
            terminals: HashMap::new(),
            chat_models: HashMap::new(),
            chats: HashMap::new(),
            rendered_chats: std::collections::HashSet::new(),
            pruned_chats: Vec::new(),
            last_chat: None,
            assistant_last: false,
            known_approvals: std::collections::HashSet::new(),
            agent_picker: None,
            launcher: None,
            launcher_focus,
            providers: super::providers::ProvidersState::default(),
            history_index: super::settings_pages::HistoryIndex::default(),
            daemon_settings: super::settings_daemon::DaemonSettingsState::default(),
            assistant_defaults: super::settings_assistant::AssistantDefaults::default(),
            mawas_grants: super::settings_assistant::MawasGrants::default(),
            agent_focus,
            held: HashMap::new(),
            collapsed_blocks: options
                .state_path
                .as_deref()
                .map(persistence::load_collapsed)
                .unwrap_or_default(),
            review_comments: options
                .state_path
                .as_deref()
                .map(persistence::load_review_comments)
                .unwrap_or_default(),
            launcher_memory: options
                .state_path
                .as_deref()
                .map(persistence::load_launcher)
                .unwrap_or_default(),
            collapse_watch: HashMap::new(),
            window_active: window.is_window_active(),
            rendered_terminals: std::collections::HashSet::new(),
            pruned: Vec::new(),
            workspace_root: None,
            project_root: None,
            chosen_project: None,
            remembered_projects: HashMap::new(),
            focus_seen: None,
            synced_container: None,
            pending_workspace: None,
            picker: None,
            picker_search: None,
            focus_handle,
            palette_focus,
            picker_focus,
            menu_focus,
            modal_focus,
            sheet_focus,
            daemon_focus,
            drawer_focus,
            agents_drawer: false,
            daemon: DaemonControl::new(),
            mac,
            setup: super::setup::SetupControl::new(cx.focus_handle(), options.cli_link),
            integrations: super::integrations::IntegrationControl::new(
                options.installer,
                options.custom_home,
            ),
            harness_install: super::harness_install::HarnessInstallControl::new(
                options.package_runner,
            ),
            palette: None,
            edits: super::vim_ui::OverlayEdits::default(),
            context_menu: None,
            modal: None,
            modal_return: None,
            sheet: None,
            resizing: None,
            responsive: Responsive::default(),
            dock_modes_seen,
            dock_focus,
            focus_dock_seen: None,
            panel_views: HashMap::new(),
            changes_views: HashMap::new(),
            changes_subscriptions: Vec::new(),
            notice: None,
            close_toast: None,
            toast_rev: 0,
            recovery_open: false,
            recovery_review: false,
            last_title: String::new(),
            regions,
            state_path: options.state_path,
            save_task: None,
            _watch: watch,
            remote,
            _tasks: vec![settings_task],
            _subscriptions: subscriptions,
        };
        this.mac.events_task = events_task;
        if first_run_setup
            && !this
                .settings
                .get::<crate::settings::AppSettings>()
                .setup_done
        {
            this.open_setup(window, cx);
        }
        this.schedule_publish(cx);
        this.rebind_keys(cx);
        this.refresh_menus(cx);
        this.sync_session_focus(cx);
        this.sync_mac_all(cx);
        if this.mac.launchd_daemon {
            let grace = this.mac.launchd_grace;
            this.mac.grace_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(grace).await;
                this.update(cx, |this, cx| this.launchd_grace_over(cx)).ok();
            }));
        }
        this.maybe_auto_install(cx);
        this
    }

    // ---- read access (tests, wave 2) -------------------------------------------------------

    pub fn state(&self) -> &WorkbenchState {
        &self.state
    }

    pub fn density(&self) -> Density {
        self.density
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn palette(&self) -> Option<&PaletteState> {
        self.palette.as_ref()
    }

    /// Titles of the palette rows currently shown, in order.
    pub fn palette_titles(&self) -> Vec<SharedString> {
        self.palette
            .as_ref()
            .map(|p| {
                p.matches
                    .iter()
                    .filter_map(|i| self.commands.get(*i))
                    .map(|c| c.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Painted bounds of `region` from the last frame (window coordinates), if it was drawn.
    pub fn region_bounds(&self, region: Region) -> Option<Bounds<Pixels>> {
        self.regions.get(region)
    }

    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    /// Dismisses the notice line.
    pub fn clear_notice(&mut self, cx: &mut Context<Self>) {
        if self.notice.take().is_some() {
            cx.notify();
        }
    }

    pub fn panel_registry(&self) -> &PanelRegistry {
        &self.panels
    }

    /// The sessions store entity (tests, inspection).
    pub fn sessions(&self) -> &Entity<SessionsStore> {
        &self.sessions
    }

    /// The settings store the workbench runs on.
    pub fn settings_store(&self) -> &SettingsStore {
        &self.settings
    }

    /// The cached view of panel `id` as its concrete type, once it has been drawn.
    pub fn panel<T: Render>(&self, id: &str) -> Option<Entity<T>> {
        self.panel_views
            .get(&PanelId::new(id))
            .and_then(|view| view.clone().downcast::<T>().ok())
    }

    /// The terminal view settings currently in effect.
    pub fn terminal_view_settings(&self) -> &hutan_terminal_view::TerminalViewSettings {
        &self.terminal_style
    }

    /// Edits the layout model directly (then re-renders and saves). For embedding code and
    /// tests that need layouts the UI has no gesture for.
    pub fn edit_layout(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState),
    ) {
        self.mutate(cx, change);
    }

    /// Title shown in the title bar and window title: the active item of the center (a session
    /// or tab), never the page a sidebar shows.
    pub fn active_title(&self, cx: &App) -> String {
        match self.state.active_center_item() {
            Some(item) => self.item_title(&item, cx).to_string(),
            None => "Hutan".to_owned(),
        }
    }

    pub fn item_icon_and_title(&self, item: &ItemRef, cx: &App) -> (SharedString, SharedString) {
        match item {
            ItemRef::Terminal { session } | ItemRef::Chat { session } => {
                let info = session
                    .parse::<SessionId>()
                    .ok()
                    .and_then(|id| self.sessions.read(cx).session(id).cloned());
                match info {
                    Some(info) => (
                        crate::workbench::panels::kind_icon(info.kind).into(),
                        info.title.into(),
                    ),
                    None => ("❯".into(), "Terminal".into()),
                }
            }
            ItemRef::Panel { panel } => (
                self.panels
                    .get(panel)
                    .map(|s| s.icon.clone())
                    .unwrap_or_else(|| "▫".into()),
                self.panels.title(panel),
            ),
            ItemRef::Placeholder { title } => ("▫".into(), title.clone().into()),
            ItemRef::Changes { path } => (
                "±".into(),
                match self.changes_views.get(path) {
                    Some(view) => view.read(cx).title().into(),
                    None => format!(
                        "Changes · {}",
                        std::path::Path::new(path)
                            .file_name()
                            .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned())
                    )
                    .into(),
                },
            ),
        }
    }

    /// What hovering the tab of `item` says: the session's title with its placement badge
    /// (`main` / `wt <branch>`) and the checkout. `None` for panels and for sessions outside git.
    pub fn item_tooltip(&self, item: &ItemRef, cx: &App) -> Option<String> {
        let (ItemRef::Terminal { session } | ItemRef::Chat { session }) = item else {
            return None;
        };
        let id = session.parse::<SessionId>().ok()?;
        let store = self.sessions.read(cx);
        let info = store.session(id)?;
        let badge = crate::sessions::placement_badge(info)?;
        Some(format!(
            "{} · {}\n{}",
            info.title, badge.text, badge.tooltip
        ))
    }

    pub fn item_title(&self, item: &ItemRef, cx: &App) -> SharedString {
        self.item_icon_and_title(item, cx).1
    }

    // ---- state mutation ---------------------------------------------------------------------

    /// Apply a model change, then re-render and schedule a save.
    pub(crate) fn mutate(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState),
    ) {
        change(&mut self.state);
        self.after_change(cx);
    }

    /// Like [`Self::mutate`] for fallible model operations; failures are logged, not shown.
    pub(crate) fn try_mutate<T, E: std::fmt::Display>(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut WorkbenchState) -> Result<T, E>,
    ) -> Option<T> {
        let result = match change(&mut self.state) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!("layout operation rejected: {err}");
                None
            }
        };
        self.after_change(cx);
        result
    }

    fn after_change(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.note_dock_modes();
        self.attach_open_items(cx);
        self.sync_session_focus(cx);
        cx.notify();
        self.schedule_save(cx);
        self.schedule_publish(cx);
    }

    pub(crate) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.state_path.is_none() {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            this.update(cx, |this, _| this.flush_save()).ok();
        }));
    }

    /// Write the layout now if it changed since the last write. Called by the debounce timer,
    /// on quit, and by tests.
    pub fn flush_save(&mut self) {
        let Some(path) = &self.state_path else {
            return;
        };
        let json = match persistence::state_json(
            &self.state,
            &self.collapsed_blocks,
            &self.review_comments,
            &self.launcher_memory,
        ) {
            Ok(json) => json,
            Err(err) => {
                tracing::error!("cannot serialize desktop layout: {err}");
                return;
            }
        };
        if self.last_saved.as_deref() == Some(json.as_str()) {
            return;
        }
        match persistence::save_json(path, &json) {
            Ok(()) => self.last_saved = Some(json),
            Err(err) => tracing::warn!(path = %path.display(), "cannot save desktop layout: {err}"),
        }
    }

    // ---- theme & density ---------------------------------------------------------------------

    /// Re-derive the theme if the system appearance changed. Called from `render`.
    pub(crate) fn sync_appearance(&mut self, window: &Window, cx: &mut Context<Self>) {
        let appearance = window.appearance();
        if appearance != self.appearance {
            self.appearance = appearance;
            self.theme = Theme::for_chrome(self.chrome, appearance);
            self.publish_style(cx);
            // `terminal.theme = "auto"` follows the system appearance.
            self.refresh_terminal_style(cx);
        }
    }

    pub(crate) fn publish_style(&self, cx: &mut Context<Self>) {
        cx.set_global(UiStyle {
            theme: self.theme,
            density: self.density,
        });
        self.invalidate_cached_views(cx);
    }

    /// Views drawn through `AnyView::cached` are re-rendered only when they notify: whatever
    /// they read from outside their own entity and the sessions store (the theme, the density,
    /// the settings) has to wake them itself.
    pub(crate) fn invalidate_cached_views(&self, cx: &mut Context<Self>) {
        let ids = self
            .panel_views
            .values()
            .map(AnyView::entity_id)
            .chain(self.chats.values().map(Entity::entity_id))
            .chain(self.changes_views.values().map(Entity::entity_id))
            .chain(self.terminals.values().map(|item| item.view.entity_id()));
        for id in ids {
            gpui::App::notify(cx, id);
        }
    }

    /// Switches density and writes it back to `appearance.density` in the user's settings.
    pub fn set_density(&mut self, density: Density, cx: &mut Context<Self>) {
        if self.density == density {
            return;
        }
        self.density = density;
        self.publish_style(cx);
        self.refresh_terminal_style(cx);
        let value = match density {
            Density::Compact => "compact",
            Density::Comfortable => "comfortable",
        };
        if let Err(err) = self
            .settings
            .write_user("appearance.density", toml::Value::String(value.to_owned()))
        {
            tracing::warn!("cannot save the density: {err}");
        }
        cx.notify();
    }

    // ---- panels ------------------------------------------------------------------------------

    pub(crate) fn panel_view(
        &mut self,
        id: &PanelId,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyView> {
        if let Some(view) = self.panel_views.get(id) {
            return Some(view.clone());
        }
        let factory = self.panels.get(id)?.factory.clone();
        let view = factory(&self.env, window, cx);
        self.panel_views.insert(id.clone(), view.clone());
        Some(view)
    }

    // ---- action handlers ---------------------------------------------------------------------

    /// Splits the focused pane; the new pane starts a terminal (see
    /// [`Workbench::split_pane_with_terminal`]).
    pub(crate) fn split_active(&mut self, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        self.split_pane_with_terminal(pane, edge, window, cx);
    }

    /// An empty split showing a placeholder (no daemon to start a terminal on).
    pub(crate) fn split_with_placeholder(
        &mut self,
        pane: PaneId,
        edge: Edge,
        cx: &mut Context<Self>,
    ) {
        self.try_mutate(cx, |s| {
            s.split_pane(pane, edge, ItemRef::placeholder("New pane"))
        });
    }

    /// ⌘⇧1…9: the `n`-th preset (1-based).
    pub(crate) fn apply_preset_number(
        &mut self,
        n: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = n
            .checked_sub(1)
            .and_then(|i| self.presets.get(i))
            .map(|p| p.name.clone())
        {
            self.apply_preset_named(&name, cx);
        }
    }

    pub(crate) fn apply_preset_named(&mut self, name: &str, cx: &mut Context<Self>) {
        let presets = self.presets.clone();
        self.try_mutate(cx, |s| s.switch_preset(name, &presets));
    }

    pub(crate) fn toggle_overview(&mut self, cx: &mut Context<Self>) {
        let presets = self.presets.clone();
        self.try_mutate(cx, |s| s.toggle_preset(LayoutPreset::OVERVIEW, &presets));
    }

    /// The panel the user means by "this panel": the focused dock's active panel, else the
    /// focused pane's active tab when that is a panel.
    pub(crate) fn focused_panel(&self) -> Option<PanelId> {
        match self.state.focus {
            FocusTarget::Dock(pos) => self.state.docks.get(pos).active.clone(),
            FocusTarget::Pane(_) => self
                .state
                .active_pane()
                .and_then(|p| p.active_item())
                .and_then(|i| i.as_panel().cloned()),
        }
    }

    pub(crate) fn move_focused_panel(&mut self, dest: PanelDest, cx: &mut Context<Self>) {
        match self.focused_panel() {
            Some(panel) => {
                self.try_mutate(cx, |s| s.move_panel(&panel, dest));
            }
            None => self.set_notice("Focus a panel first", cx),
        }
    }

    pub(crate) fn move_active_tab_to_split(&mut self, edge: Edge, cx: &mut Context<Self>) {
        let pane = self.state.active_pane_id();
        let Some(active) = self.state.pane(pane).map(|p| p.active) else {
            return;
        };
        self.try_mutate(cx, |s| {
            s.move_item(
                ItemSource::Pane {
                    pane,
                    index: active,
                },
                DropTarget::Split { pane, edge },
            )
        });
    }

    pub(crate) fn open_panel(&mut self, id: &str, cx: &mut Context<Self>) {
        // The board is the center of the overview preset, no longer a page of the left sidebar.
        if id == "overview" && self.state.find_panel(&PanelId::new("overview")).is_none() {
            self.toggle_overview(cx);
            return;
        }
        if actions::is_mawas_page(id) && !self.assistant_enabled() {
            return;
        }
        let id = PanelId::new(id);
        // Mawas Home and its Memory are pages of the center: a tab of the active pane, not a
        // dock's panel.
        if actions::is_mawas_page(id.as_str()) {
            self.mutate(cx, |s| s.open_panel_in_center(&id));
            return;
        }
        let dock = self
            .panels
            .get(&id)
            .map(|s| s.default_dock)
            .unwrap_or(DockPosition::Right);
        self.mutate(cx, |s| s.open_panel(&id, dock));
    }

    pub(crate) fn set_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        cx.notify();
    }

    pub(crate) fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.mutate(cx, |s| s.reset_layout());
    }

    /// Drop a dragged tab/panel on a target.
    pub(crate) fn drop_item(
        &mut self,
        dragged: &DraggedItem,
        target: DropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.try_mutate(cx, |s| s.move_item(dragged.source.clone(), target));
        self.focus_root(window, cx);
    }

    pub(crate) fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.try_mutate(cx, |s| s.focus_pane(pane));
        self.focus_root(window, cx);
    }

    // ---- palette -----------------------------------------------------------------------------

    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    pub(crate) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_palette(window, cx);
        } else {
            self.open_palette(window, cx);
        }
    }

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut state = PaletteState::default();
        state.refilter(self.commands.iter().map(|c| c.title.as_ref()));
        self.palette = Some(state);
        self.edits.palette.reset();
        self.context_menu = None;
        self.picker = None;
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub fn set_palette_query(&mut self, query: &str, cx: &mut Context<Self>) {
        let commands = Rc::clone(&self.commands);
        if let Some(palette) = &mut self.palette {
            palette.query = query.to_owned();
            palette.refilter(commands.iter().map(|c| c.title.as_ref()));
            cx.notify();
        }
    }

    pub(crate) fn palette_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            if next {
                palette.select_next();
            } else {
                palette.select_prev();
            }
            cx.notify();
        }
    }

    /// Close the palette and run the highlighted command (or the one at `index` in the filtered
    /// list). The action is dispatched through the normal action path, after focus returns to the
    /// workbench, so palette and keyboard share one code path.
    pub(crate) fn palette_confirm(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = &self.palette else {
            return;
        };
        let chosen = match index {
            Some(i) => palette.matches.get(i).copied(),
            None => palette.selected_command(),
        };
        let action = chosen
            .and_then(|i| self.commands.get(i))
            .map(|c| c.action.boxed_clone());
        self.close_palette(window, cx);
        if let Some(action) = action {
            window.dispatch_action(action, cx);
        }
    }

    // ---- context menus -----------------------------------------------------------------------

    pub(crate) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        entries: Vec<MenuEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenu { position, entries });
        // Menus open on right-button *down*, and the workbench root re-focuses itself on every
        // mouse down later in the same dispatch; focus the menu once the event has finished.
        cx.defer_in(window, |this, window, cx| {
            if this.context_menu.is_some() {
                window.focus(&this.menu_focus, cx);
            }
        });
        cx.notify();
    }

    /// Open the title bar's layout-preset switcher at `position`.
    pub fn show_preset_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.preset_menu();
        self.open_menu(position, entries, window, cx);
    }

    /// Labels (with check state) of the open context menu, in order.
    pub fn context_menu_entries(&self) -> Option<Vec<(SharedString, bool)>> {
        self.context_menu.as_ref().map(|m| {
            m.entries
                .iter()
                .map(|e| (e.label.clone(), e.checked))
                .collect()
        })
    }

    /// Labels of the open context menu's entries that are greyed out.
    pub fn context_menu_disabled(&self) -> Vec<SharedString> {
        self.context_menu
            .as_ref()
            .map(|m| {
                m.entries
                    .iter()
                    .filter(|e| e.disabled)
                    .map(|e| e.label.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Close the context menu and run its `index`-th entry (a greyed-out one only closes it).
    pub fn run_menu_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let run = self
            .context_menu
            .as_ref()
            .and_then(|m| m.entries.get(index))
            .filter(|e| !e.disabled)
            .map(|e| Rc::clone(&e.run));
        self.close_menu(window, cx);
        if let Some(run) = run {
            run(self, window, cx);
        }
    }

    pub(crate) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            // A menu opened from the Start sheet (its send-key control) gives the keyboard back
            // to the sheet.
            if self.launcher.is_some() {
                self.launcher_refocus(window, cx);
            } else {
                self.focus_root(window, cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn entry(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> MenuEntry {
        MenuEntry {
            label: label.into(),
            checked: false,
            disabled: false,
            run: Rc::new(run),
        }
    }

    /// A greyed-out entry that shows why it does nothing.
    pub(crate) fn disabled_entry(label: impl Into<SharedString>) -> MenuEntry {
        MenuEntry {
            label: label.into(),
            checked: false,
            disabled: true,
            run: Rc::new(|_, _, _| {}),
        }
    }

    /// Menu for a panel tab (in a dock or open as a center tab).
    pub(crate) fn panel_menu(&self, panel: &PanelId) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        let here = self.state.find_panel(panel);
        for pos in DockPosition::ALL {
            let panel = panel.clone();
            let already = matches!(&here, Some(ItemSource::Dock { pos: p, .. }) if *p == pos);
            let mut e = Self::entry(
                format!("Move to {} Dock", pos.label()),
                move |this, _, cx| {
                    this.try_mutate(cx, |s| s.move_panel(&panel, PanelDest::Dock(pos)));
                },
            );
            e.checked = already;
            entries.push(e);
        }
        let p = panel.clone();
        entries.push(Self::entry("Open as Tab", move |this, _, cx| {
            this.try_mutate(cx, |s| s.move_panel(&p, PanelDest::Center));
        }));
        let p = panel.clone();
        entries.push(Self::entry("Close", move |this, _, cx| {
            this.try_mutate(cx, |s| s.close_panel(&p));
        }));
        entries
    }

    /// Menu for a center tab.
    pub(crate) fn tab_menu(&self, pane: PaneId, index: usize) -> Vec<MenuEntry> {
        let mut entries = vec![
            Self::entry("Split Right", move |this, _, cx| {
                this.try_mutate(cx, |s| {
                    s.move_item(
                        ItemSource::Pane { pane, index },
                        DropTarget::Split {
                            pane,
                            edge: Edge::Right,
                        },
                    )
                });
            }),
            Self::entry("Split Down", move |this, _, cx| {
                this.try_mutate(cx, |s| {
                    s.move_item(
                        ItemSource::Pane { pane, index },
                        DropTarget::Split {
                            pane,
                            edge: Edge::Bottom,
                        },
                    )
                });
            }),
        ];
        let panel = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(|i| i.as_panel().cloned());
        if let Some(panel) = panel {
            for pos in DockPosition::ALL {
                let panel = panel.clone();
                entries.push(Self::entry(
                    format!("Move to {} Dock", pos.label()),
                    move |this, _, cx| {
                        this.try_mutate(cx, |s| s.move_panel(&panel, PanelDest::Dock(pos)));
                    },
                ));
            }
        }
        entries.push(Self::entry("Close", move |this, _, cx| {
            this.try_mutate(cx, |s| s.close_item(pane, index));
        }));
        entries
    }

    /// Menu of layout presets (title bar "Layout" switcher).
    pub(crate) fn preset_menu(&self) -> Vec<MenuEntry> {
        let mut entries: Vec<MenuEntry> = self
            .presets
            .iter()
            .map(|preset| {
                let name = preset.name.clone();
                let mut e = Self::entry(preset_label(&preset.name), move |this, _, cx| {
                    this.apply_preset_named(&name, cx);
                });
                e.checked = self.state.preset == preset.name;
                e
            })
            .collect();
        entries.push(Self::entry(
            "Save Layout as Preset…",
            |this, window, cx| {
                this.save_layout_as_preset(window, cx);
            },
        ));
        entries
    }

    // ---- resize drags ------------------------------------------------------------------------

    pub(crate) fn on_mouse_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(resizing) = self.resizing.clone() else {
            return;
        };
        if !event.dragging() {
            self.resizing = None;
            return;
        }
        match resizing {
            Resizing::Dock {
                pos,
                start_mouse,
                start_size,
                ..
            } => {
                let mouse = match pos {
                    DockPosition::Bottom => f32::from(event.position.y),
                    _ => f32::from(event.position.x),
                };
                let delta = mouse - start_mouse;
                let raw = match pos {
                    DockPosition::Left => start_size + delta,
                    DockPosition::Right | DockPosition::Bottom => start_size - delta,
                };
                // Below the minimum the dock stays at it; 24 pt further arms the collapse to the
                // rail (previewed, committed on release).
                let (size, armed) = self.drag_size(pos, raw, window);
                if let Some(Resizing::Dock { armed: a, .. }) = &mut self.resizing {
                    *a = armed;
                }
                self.mutate(cx, |s| s.resize_dock(pos, size));
            }
            Resizing::Split {
                path,
                divider,
                horizontal,
                origin,
                extent,
            } => {
                if extent <= 0.0 {
                    return;
                }
                let mouse = if horizontal {
                    f32::from(event.position.x)
                } else {
                    f32::from(event.position.y)
                };
                let position = (mouse - origin) / extent;
                self.mutate(cx, |s| {
                    let _ = s.set_divider(&path, divider, position);
                });
            }
        }
    }

    pub(crate) fn start_dock_resize(&mut self, pos: DockPosition, mouse: Point<Pixels>) {
        let start_mouse = match pos {
            DockPosition::Bottom => f32::from(mouse.y),
            _ => f32::from(mouse.x),
        };
        self.resizing = Some(Resizing::Dock {
            pos,
            start_mouse,
            start_size: self.state.docks.get(pos).size,
            armed: false,
        });
    }
}

/// `default` → "Default".
pub fn preset_label(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Keyboard resize steps (dock pixels, split ratio) shared with the action handlers.
pub(crate) const fn steps() -> (f32, f32) {
    (DOCK_STEP, SPLIT_STEP)
}
