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
use super::panels::{AssistantPanel, builtin_panels, builtin_status_items};
use super::persistence;
use super::registry::{PanelEnv, PanelRegistry, StatusItemRegistry};
use super::settings_sheet::SettingsSheet;
use super::style::{TerminalStyle, UiStyle};
use super::terminals::TerminalItem;
use super::workspaces::PickerState;
use crate::cli_link::CliLink;
use crate::harness_install::PackageRunner;
use crate::install::Install;
use crate::integrations::IntegrationInstaller;
use crate::macos::MacServices;
use crate::sessions::{
    RecoverySummaryDue, SessionsStore, StoreChanged, TerminalSession, UiOpenRequested,
};
use crate::settings::{AppearanceSettings, LayoutSettings, PresetDef, TerminalSettings};
use crate::theme::{Density, DensityExt as _, Theme, resolve_frame};
use gpui::Window;
use gpui::{
    AnyView, App, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, Pixels, Point,
    Render, SharedString, Subscription, Task, WindowAppearance, div, prelude::*, px,
};
use hutan_client::Connection;
use hutan_core::{ApprovalId, SessionId};
use hutan_settings::{AppearanceMode, SettingsStore, ThemeRegistry, WatchGuard};
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
    /// The app bundle this desktop runs from, to tell a reinstall or an update from a launch;
    /// `None` for a development build.
    pub install: Option<Install>,
    /// Runs `hutan integration` for the agent integrations; `None` installs nothing.
    pub installer: Option<Arc<dyn IntegrationInstaller>>,
    /// Checks and installs missing harness packages (the "Install…" buttons); `None` offers
    /// none.
    pub package_runner: Option<Arc<dyn PackageRunner>>,
    /// Hutan runs with its own data directory (`HUTAN_HOME`): the agents' hooks still go into
    /// their real configuration directories, which the setup says.
    pub custom_home: bool,
    /// The `hutand` this app starts, and its build (`app::own_daemon`): a daemon of another
    /// build or from elsewhere is restarted onto it or reported (`lifecycle.rs`). `None` checks
    /// nothing.
    pub own_daemon: Option<hutan_client::DaemonIdentity>,
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
            install: None,
            installer: None,
            package_runner: None,
            custom_home: std::env::var_os("HUTAN_HOME").is_some_and(|home| !home.is_empty()),
            own_daemon: None,
        }
    }

    /// The same options with these macOS services.
    pub fn with_mac(mut self, mac: MacServices) -> Self {
        self.mac = mac;
        self
    }

    /// The same options checking the daemon against `own`, the `hutand` this app starts.
    pub fn with_own_daemon(mut self, own: hutan_client::DaemonIdentity) -> Self {
        self.own_daemon = Some(own);
        self
    }

    /// The same options showing the first-run setup until it is done (`app.setup_done`).
    pub fn with_first_run_setup(mut self) -> Self {
        self.first_run_setup = true;
        self
    }

    /// The same options with the install this desktop runs from (`Install::current()`);
    /// `None`, a development build, never offers anything.
    pub fn with_install(mut self, install: Option<Install>) -> Self {
        self.install = install;
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
}

/// A laid-out area of the workbench whose painted bounds are recorded every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Region {
    Titlebar,
    StatusBar,
    /// A status item, by registry id (`connection`, `sessions`, …).
    StatusItem(&'static str),
    /// A control of the "Moved … · Undo" toast of a layout change (`undo`).
    LayoutToast(&'static str),
    /// A control of the usage warning toast (`toast`, `open`, `dismiss`).
    UsageToast(&'static str),
    /// A control of the "Saved the image … · Reveal" toast of an image paste (`toast`,
    /// `reveal`, `dismiss`).
    PasteToast(&'static str),
    /// The ⋯ of a panel's header, by where the panel is (`left`, `right`, `bottom`, `center`).
    PanelMenu(&'static str),
    /// A control of the panel settings popover (`popover`, `open`, `close`).
    PanelSettings(&'static str),
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
    /// `memory`, `projects` for the back affordance of a page) or by panel id for any other page.
    Nav(&'static str),
    /// A control of the left sidebar's bottom toolbar (`new`, `settings`).
    Toolbar(&'static str),
    /// A control of the Agents page, by name (`search`, `scope`, `group`, `finished`, `start`,
    /// `integrations`, `clear`, `clear-group`, `daemon`, `offline`, `list`, `details`).
    AgentsPage(&'static str),
    /// The row of harness `index` in the Usage section of the Agents page.
    UsageRow(usize),
    /// The filled part of limit `limit` of harness `harness` in the Usage section: its width is
    /// the share of the window used.
    UsageBar(usize, usize),
    /// The row of a session on the Agents page.
    AgentRow(hutan_core::SessionId),
    /// A control of a session's row on the Agents page: `primary`, `more`, `interrupt`.
    AgentAction(hutan_core::SessionId, &'static str),
    /// Option `index` of the expanded request of a session's row: the only way to answer it.
    AgentOption(hutan_core::SessionId, usize),
    /// The heading of a group of the Agents page.
    AgentGroup(crate::sessions::Group),
    /// The row of subagent `index` of a session on the Agents page.
    AgentChild(hutan_core::SessionId, usize),
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
    /// A worktree row of the sessions panel, by [`Region::worktree`].
    WorktreeRow(u64),
    /// The "No workspace" label of the sessions panel: where a project is dropped to leave its
    /// workspace.
    NoWorkspace,
    /// A control of the sessions panel's header, by name (`plus`, `sort`).
    SidebarControl(&'static str),
    /// The scope chip of a scoped panel's header, by panel (`files`, `changes`, `search`,
    /// `problems`, `agents`).
    Scope(&'static str),
    /// A control of the history panel, by name (`search`, `enable`, `disable`, `clear`,
    /// `rebuild`, `confirm`).
    History(&'static str),
    /// A repeated control of the history panel: its name (`row`, `chevron`, `more`, `group`,
    /// `menu`, `source`, `action`, `show-more`) and index (the row, group, entry or button
    /// counting from 0).
    HistoryItem(&'static str, usize),
    /// A control of the chat view, by name (`composer`, `send`, `interrupt`, `restart`,
    /// `header`, `banner`, `context`, `jump-latest`).
    Chat(&'static str),
    /// Entry `index` of a chat's transcript.
    ChatEntry(usize),
    /// A button of a permission request that waits for an answer: `request * 8 + option`, the
    /// requests counted oldest first.
    ChatOption(usize),
    /// A control of a question card that waits for an answer (protocol 0.24): `0` Submit, `1`
    /// Decline, `2` Cancel, `3` Copy (a question that ended with typed answers), `8 + n` field `n`, `100 + n * 20 + k` option `k` of field `n`.
    ChatQuestion(usize),
    /// Context chip `index` of the composer.
    ChatChip(usize),
    /// Structured context chip `index` of the composer (assistant chats).
    ChatContextChip(usize),
    /// A control of queued message `index` above the composer (protocol 0.36): `chip`, `edit`,
    /// `now`, `remove`.
    ChatQueue(&'static str, usize),
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
    /// A control of the Memory page (a body of the left sidebar), by name (`panel`, `header`,
    /// `filters`, `add`, `scope-all`, `scope-global`, `scope-workspace`, `archived`, `editor`,
    /// `save`, `cancel`, `error`, …).
    Memory(&'static str),
    /// A repeated control of the Memory page: its name (`entry` the one-line row, `block` the
    /// row with everything under it, `details`, `edit`, `archive`, `archive-yes`,
    /// `archive-cancel`, `restore`, `confirm`, `history`, `proposal`, `approve`, `edit-approve`,
    /// `reject`) and the index of its row (entries and proposals count separately, in the order
    /// drawn).
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
    /// The head of a menu of the settings sheet (a choice, an add menu, a theme picker): the
    /// hash of the dropdown's tag. A click on it is not "outside" the menu that hangs from it.
    SettingsMenu(u64),
    /// Entry `index` of a list of the Start… sheet (`project`, `provider`, `model`, `worktree`,
    /// `after`), top to bottom.
    LauncherItem(&'static str, usize),
    /// The hover × of a session's row in the sidebar or card in the Agent Dashboard.
    SessionClose(hutan_core::SessionId),
    /// A session's row in the Projects sidebar (where its title can be edited in place).
    SessionRow(hutan_core::SessionId),
    /// The muted marker of a session's row (its notifications are muted, `notify.mute`).
    SessionMuted(hutan_core::SessionId),
    /// The unread dot of a session's row (6 px of amber inside its ring).
    SessionUnread(hutan_core::SessionId),
    /// The warning glyph of a session's row: another live agent works in the same checkout.
    SharedCheckout(hutan_core::SessionId),
    /// The *Changes to review* link of a session's row (its seen work waits for review).
    SessionReview(hutan_core::SessionId),
    /// A control of the "Closed … · Undo" toast (`undo`).
    CloseToast(&'static str),
    /// The ▷ Start shortcut of project row `workspace` in the sidebar (shown on hover).
    ProjectStart(hutan_core::WorkspaceId),
    /// A control of the Accounts page: harness id (or `*`) and the control (`login`, `logout`, `card`, `refresh`).
    Account(&'static str, &'static str),
    /// A control of the first-run setup, by name (`continue`, `back`, `skip`, `appearance-dark`,
    /// `keep-running`, `start-at-login`, `notifications`, `cli-link`, …).
    Setup(&'static str),
    /// A control of the first screen (`open-folder`, `new-terminal`, `screen`).
    FirstRun(&'static str),
    /// A control of an agent on the first screen: harness id and `login`, `install`, `guide`.
    FirstRunAgent(&'static str, &'static str),
    /// A control of the one-line offers: `notify-yes`, `notify-no`, `notify-done`, `notify` (the
    /// line), `hooks-offer`, `hooks-install`, `hooks-not-now`.
    Offer(&'static str),
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
    /// A cell of line `index` of the resources panel's table, by column (`name`, `pid`, `cpu`,
    /// `memory`, `threads`, `command`), and the runs of text inside the name column
    /// (`name.0`, `name.1`, …): where each one is laid out.
    ResourceCell(usize, &'static str),
    /// A header cell of the resources panel's table, by column (as the cells).
    ResourceHeader(&'static str),
    /// Entry `index` of the open context menu, top to bottom.
    MenuEntry(usize),
    /// The chip a restarted session wears (`respawned`, `resumed`, `interrupted`, `not
    /// restarted`) at one of its places.
    RecoveryChip(BadgeSite, SessionId),
    /// A control of the restart summary, by name (`sheet`, `close`, `dismiss-all`).
    Recovery(&'static str),
    /// A control of the "agent not recognised" strip of a terminal (`strip`, `track`).
    Untracked(&'static str),
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
    /// The changed-files badge of a project row of the sidebar (a project the window is not
    /// working in).
    ProjectChanged(hutan_core::WorkspaceId),
    /// The fold chevron of group `index` of the Changes tab.
    ChangesFold(usize),
    /// The *Finish…* button of group `index` of the Changes tab.
    ChangesFinish(usize),
    /// File `file` of group `group` of the Changes tab (top to bottom, folded groups have none).
    ChangesPanelFile(usize, usize),
    /// A control of the Changes view, by name (`toggle`, `refresh`, `merge`, `merge-confirm`,
    /// `merge-cancel`, `strategy-merge`, `strategy-squash`, `strategy-rebase`, `message`,
    /// `remove`, `keep`; delivery: `delivery`, `accept`, `request-changes`, `request-note`,
    /// `request-send`, `request-cancel`, `commit-all`, `commit-message`, `commit-confirm`,
    /// `commit-cancel`).
    Changes(&'static str),
    /// File `index` of the Changes view's list, top to bottom.
    ChangesFile(usize),
    /// A control of a file history tab, by name (`refresh`, `more`, `open-revision`, `copy`,
    /// `root`, `detail`).
    FileHistory(&'static str),
    /// Commit `index` of a file history tab's list, newest first.
    FileHistoryRow(usize),
    /// Commit `index` of the Changes view's History list, newest first.
    ChangesCommit(usize),
    /// File `index` of the selected commit in the Changes view's History segment.
    ChangesHistoryFile(usize),
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
    /// The *Open* button of file `index` of the Changes view's list (at its first hunk).
    ChangesOpen(usize),
    /// The *Open* button of hunk header `index` of the shown file's diff (at that hunk).
    ChangesHunkOpen(usize),
    /// The *Open* button of a file row of chat entry `index`: part `part` (as `ChatPart`).
    ChatOpen(usize, u32),
    /// A control of a file tab's agent bar (`reject`, `accept`, `follow`, `previous`, `next`).
    FileAgent(&'static str),
    /// The *Rewind…* (or the `Restored — Undo` *Undo*) of the footer of chat entry `index`.
    ChatRewind(usize),
    /// The checkpoint line (`3 files changed · +41 −7`) of the footer of chat entry `index`.
    ChatCheckpoint(usize),
    /// A control of the Rewind preview sheet (`sheet`, `cancel`, `restore`, `restore-all`,
    /// `interrupt`, `note`, `diff`, `notice`, `busy`).
    Rewind(&'static str),
    /// The checkbox of file `index` of the Rewind preview sheet, top to bottom.
    RewindFile(usize),
    /// Row `index` of the Checkpoints list (newest first); `CheckpointRewind` its *Rewind…*.
    CheckpointRow(usize),
    CheckpointRewind(usize),
    /// A control of the Checkpoints list (`sheet`, `close`).
    Checkpoints(&'static str),
    /// A control of the "Restored N files · Undo" toast (`toast`, `undo`).
    RestoreToast(&'static str),
    /// A control of the request toast at `index` (oldest first): `toast`, `review`, `dismiss`.
    AttentionToast(usize, &'static str),
    /// The dot on the tab at `index` of a pane: a request called for its session and the user has
    /// not looked yet.
    TabCall(PaneId, usize),
    /// The terminal of a split of a tab of terminal splits, by its session.
    TermSplit(hutan_core::SessionId),
    /// Divider `index` of a tab's terminal splits (every divider of the visible tree, in tree
    /// order).
    TermDivider(usize),
    /// The tab-count badge (`⊞ 3`) of the tab at `index` of a pane.
    TabSplits(PaneId, usize),
    /// The attention dot (not a request called for the user) of the tab at `index` of a pane.
    TabAttention(PaneId, usize),
    /// A control of a file tab's language-server bar (`trust`, `not-now`, `copy`, `dismiss`).
    FileLsp(&'static str),
    /// Diagnostic row `index` of the Problems panel (the listed diagnostics, top to bottom,
    /// file headers not counted).
    ProblemsRow(usize),
    /// A control of the Instructions panel (`refresh`, `share`, `create`).
    Instructions(&'static str),
    /// Row `index` of the Instructions panel's list (project files, then the user's).
    InstructionRow(usize),
    /// A control of the "Shared one file with every agent · Undo" toast (`toast`, `undo`).
    InstructionsToast(&'static str),
    /// A control of the Problems panel's header (`severity`, `refresh`).
    Problems(&'static str),
    /// The Restart button of the Problems panel's server line `index` (top to bottom).
    ProblemsServerRestart(usize),
    /// Row `index` of the Search panel's list (file headers and matches, top to bottom).
    SearchRow(usize),
    /// The checkbox of row `index` of the Search panel's list (a file's or a match's).
    SearchCheck(usize),
    /// A control of the Search panel (`query`, `replace`, `include`, `exclude`, `case`,
    /// `word`, `regex`, `toggle-replace`, `replace-all`, `confirm`, `cancel`).
    Search(&'static str),
}

impl Region {
    /// The region of the sidebar's worktree row of the worktree at `path`.
    pub fn worktree(path: &std::path::Path) -> Self {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        Self::WorktreeRow(hasher.finish())
    }
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
        // A floating chip: the popover's surface and shadow, no accent edge.
        let mut chip = div()
            .px(px(10.0))
            .py(px(4.0))
            .rounded(px(self.style.density.ui().radius))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.fg)
            .text_size(px(self.style.density.ui().font_size))
            .child(self.label.clone());
        chip.style().box_shadow = Some(theme.shadow());
        chip
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
    TermSplit {
        pane: PaneId,
        path: Vec<usize>,
        divider: usize,
        horizontal: bool,
        origin: f32,
        extent: f32,
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
    /// Opens another menu; drawn with `action.disclose`, not encoded in the accessible label.
    pub submenu: bool,
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
    /// The catalog of color packs, one per application.
    pub(crate) themes: ThemeRegistry,
    /// The chrome pack in effect; the style is only republished when this changes.
    pub(crate) resolved_theme: Arc<hutan_settings::themes::ResolvedTheme>,
    /// Id of the pack the terminal's palette comes from (the status bar shows it).
    pub(crate) terminal_theme_name: String,
    /// Which `terminal` font keys the user has written; a key appearing or disappearing changes
    /// what the pack's defaults do even when the effective value stays the same.
    pub(crate) terminal_explicit: crate::settings::TerminalExplicit,
    /// What the theme settings and packs last raised as Problems.
    pub(crate) theme_notes: Vec<hutan_settings::SettingsError>,
    /// Set when hot reload of packs could not start: a Problem that says so, so that
    /// "Validate / Reload" is not a mystery.
    pub(crate) theme_watch_note: Option<hutan_settings::SettingsError>,
    pub(crate) sessions: Entity<SessionsStore>,
    pub(crate) env: PanelEnv,
    pub(crate) terminals: HashMap<(PaneId, SessionId), TerminalItem>,
    /// The find bars that are open (⌘F in a terminal), one per session.
    pub(crate) terminal_finds: HashMap<(PaneId, SessionId), super::terminals::FindItem>,
    /// The query and options the last find bar ended with: the next one starts there.
    pub(crate) last_terminal_find: super::terminal_find::FindMemory,
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
    /// The observation of Mawas Home's page that keeps Files, Search and Problems on its scope
    /// (`sync_mawas_focus`), with the entity it watches.
    pub(crate) mawas_watch: Option<(gpui::EntityId, gpui::Subscription)>,
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
    /// The Automation page: the user's hooks as the daemon last said.
    pub(crate) hooks_page: super::settings_hooks::HooksPage,
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
    /// What each chat's composer holds, saved with the layout (see
    /// [`persistence::ChatDrafts`]).
    pub(crate) chat_drafts: persistence::ChatDrafts,
    /// What the user did to the attention queue (marked unread, snoozed): kept in the state file
    /// so it survives a restart, and told to the store at start. The store owns the truth; this
    /// is its last saved copy.
    pub(crate) attention_marks: crate::sessions::Marks,
    /// The files opened last, saved with the layout (see [`persistence::RecentFiles`]).
    pub(crate) recent_files: persistence::RecentFiles,
    /// ⌘P's `fs.find` in flight (dropping it cancels it) and its listener of `fs.generation`
    /// (`quick_open.rs`).
    pub(crate) quick_open: super::quick_open::QuickOpenTasks,
    /// The file of the focused tab the Files panel was last told about.
    pub(crate) revealed_file: Option<String>,
    /// The Search panel's *Replace All* in progress (E-13b): files waiting for their buffer or
    /// their save.
    pub(crate) replace_job: Option<super::search_replace::ReplaceJob>,
    /// What tells the workbench a chat's composer changed, per chat model.
    pub(crate) chat_draft_watch: HashMap<SessionId, Subscription>,
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
    pub(crate) daemon: DaemonControl,
    /// The branch of the active project, for the `workspace` status item.
    pub(crate) status_branch: super::status_branch::StatusBranch,
    pub(crate) mac: MacControl,
    /// The quick terminal's panel and shortcut (`quick_terminal.rs`).
    pub(crate) quick: super::quick_terminal::QuickControl,
    pub(crate) setup: super::setup::SetupControl,
    pub(crate) first_run: super::first_run::FirstRunControl,
    /// This install against the one the desktop last saw (`reinstall.rs`).
    pub(crate) install: super::reinstall::InstallControl,
    /// The one-time offer of the clean default layout is on screen (`layout_offer.rs`).
    pub(crate) layout_offer: bool,
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
    /// The scroll of each pane's tab strip, and which tab the strip was last scrolled to show
    /// (`(active index, tabs)`): a changed active tab or tab count scrolls it into view once.
    pub(crate) tab_scrolls: std::collections::HashMap<PaneId, gpui::ScrollHandle>,
    pub(crate) tab_revealed: std::collections::HashMap<PaneId, (usize, usize)>,
    /// The tabs of each pane that wore an attention mark (a call dot, a warning) in the last
    /// frame: a tab that gets one is scrolled into view, so the mark is never cut off.
    pub(crate) tab_marked: std::collections::HashMap<PaneId, Vec<usize>>,
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
    /// The history view of every file a history tab shows (see `file_history.rs`), by path.
    pub(crate) history_views: HashMap<String, Entity<super::file_history::FileHistoryView>>,
    pub(crate) history_subscriptions: HashMap<String, Subscription>,
    /// The read-only views of files at a commit, by path and commit.
    pub(crate) revision_views: HashMap<(String, String), Entity<super::file_history::RevisionView>>,
    /// The buffers of the files tabs show (see `files.rs`).
    pub(crate) buffers: super::files::BufferStore,
    /// The pending ledger of agents' edits (see `agent_edits.rs`).
    pub(crate) agent_ledger: super::agent_edits::AgentLedger,
    /// The file tabs' language servers: documents, diagnostics, servers, jumps (`lsp.rs`).
    pub(crate) lsp: Entity<super::lsp::LspStore>,
    /// The view of every file tab, by pane and path.
    pub(crate) file_views: HashMap<(PaneId, String), Entity<super::file_view::FileView>>,
    pub(crate) file_subscriptions: HashMap<(PaneId, String), Subscription>,
    pub(crate) rendered_files: std::collections::HashSet<(PaneId, String)>,
    pub(crate) pruned_files: Vec<Entity<super::file_view::FileView>>,
    /// The status items that follow the focused code view, made at the first status bar.
    pub(crate) editor_items: Option<super::editor_status::EditorItems>,
    pub(crate) notice: Option<SharedString>,
    /// "Closed … · Undo" (`closing.rs`).
    pub(crate) close_toast: Option<super::closing::CloseToast>,
    pub(crate) toast_rev: u64,
    /// "Shared one file with every agent · Undo" (`instructions.rs`) and the folders of the
    /// harnesses the Instructions panel lists user-level files of.
    pub(crate) instructions: super::instructions::InstructionsState,
    pub(crate) instructions_toast_exit:
        super::ui::motion::Ghost<super::instructions::InstructionsToast>,
    pub(crate) instruction_homes: Rc<std::cell::RefCell<Option<crate::instructions::HarnessHomes>>>,
    /// "Moved Files to the left · Undo" and what undoes it (`customize.rs`).
    pub(crate) layout_toast: Option<super::customize::LayoutToast>,
    pub(crate) layout_undo: Option<super::customize::LayoutUndo>,
    pub(crate) layout_toast_exit: super::ui::motion::Ghost<super::customize::LayoutToast>,
    /// The usage warning toast (`usage.rs`) and its exit.
    pub(crate) usage_toast: Option<super::usage::UsageToast>,
    pub(crate) usage_toast_exit: super::ui::motion::Ghost<super::usage::UsageToast>,
    /// The toast of a pasted image (`paste_toast.rs`) and its exit.
    pub(crate) paste_toast: Option<super::paste_toast::PasteToast>,
    pub(crate) paste_toast_exit: super::ui::motion::Ghost<super::paste_toast::PasteToast>,
    /// The tab the × just closed, while it fades (`closing.rs`).
    pub(crate) closing_tab: Option<super::closing::ClosingTab>,
    /// Overlays while they leave (`ui::motion::Presences`).
    pub(crate) overlays: super::ui::motion::Presences,
    /// The overlays that closed and are still fading out (`overlay_exit.rs`).
    pub(crate) exits: super::overlay_exit::OverlayExits,
    /// The toasts while they leave (`ui::motion::Ghost`): the state is cleared at dismissal, the
    /// exit animation still needs the content.
    pub(crate) close_toast_exit: super::ui::motion::Ghost<super::closing::CloseToast>,
    pub(crate) restore_toast_exit: super::ui::motion::Ghost<super::rewind::RestoreToast>,
    /// The toasts of requests that call for sessions not on screen (`attention_toast.rs`).
    pub(crate) attention_toasts: super::attention_toast::AttentionToasts,
    /// The Rewind preview sheet, the Checkpoints list and the "Restored … · Undo" toast
    /// (`rewind.rs`).
    pub(crate) rewind: super::rewind::RewindState,
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
    _theme_watch: Option<hutan_settings::themes::ThemeWatchGuard>,
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
        let (install, install_changed) = super::reinstall::InstallControl::start(
            options.install,
            options
                .state_path
                .as_deref()
                .and_then(persistence::load_install),
            settings.get::<crate::settings::AppSettings>().setup_done,
        );
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
        // The very first launch of an installation (a layout file is expected and does not exist
        // yet): the right sidebar waits on its rail, so that nothing opens on a panel the user
        // has not asked for. A layout that is saved, or handed in, is never touched.
        let first_launch = fresh && options.state_path.is_some();
        let saved = options.initial_state.is_none() && !fresh;
        let mut state = match (options.initial_state, &options.state_path) {
            (Some(state), _) => state,
            (None, Some(path)) => persistence::load_state(path),
            (None, None) => WorkbenchState::default(),
        };
        if first_launch {
            state.docks.right.collapsed = true;
        }
        // A layout saved before the clean default stays as it is, and is offered the new one
        // once (`layout_offer.rs`); a new installation starts with it.
        let layout_offer = saved
            && layout_settings.offer_new_default
            && preset_defs
                .iter()
                .find(|d| d.layout.name == LayoutPreset::DEFAULT)
                .is_some_and(|def| state.predates(&def.layout));
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
        let themes = match settings.paths().as_ref().and_then(|p| p.user.parent()) {
            Some(data_dir) => ThemeRegistry::new(data_dir),
            None => ThemeRegistry::builtin_only(),
        };
        let mut frame = resolve_frame(
            &themes,
            &themes.snapshot(),
            &appearance_settings,
            &terminal_settings.theme,
            crate::theme::polarity(appearance_settings.appearance, window_appearance),
        );
        let (icons, icon_notes) = themes.select_icons(
            &appearance_settings.icon_theme,
            crate::theme::polarity(appearance_settings.appearance, window_appearance),
        );
        crate::icons::install(icons);
        frame
            .diagnostics
            .extend(themes.icon_snapshot().diagnostics.iter().cloned());
        frame.diagnostics.extend(icon_notes);
        if !frame.diagnostics.is_empty() {
            settings.set_notes("themes", frame.diagnostics.clone());
        }
        crate::theme::install(&frame.resolved);
        super::ui::apply_fonts(cx, frame.resolved.fonts());
        let theme = Theme::from_resolved(&frame.resolved);
        let terminal_explicit = crate::settings::TerminalExplicit::of(&settings);
        super::ui::motion::set_setting(cx, appearance_settings.reduce_motion);
        let terminal_style = super::terminals::terminal_style(
            &terminal_settings,
            &frame.terminal,
            &frame.resolved,
            super::terminals::mono_family(cx, &frame.resolved),
            crate::settings::TerminalExplicit::of(&settings),
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
            // The models' names as last learned: a chat reads by name before any harness is
            // asked again (none is, just for that).
            store.use_model_names_file(
                options
                    .state_path
                    .as_deref()
                    .map(|p| p.with_file_name(crate::sessions::models::MODEL_NAMES_FILE)),
                cx,
            );
        });
        let regions = super::regions::Regions::default();
        let lsp = cx.new(|cx| super::lsp::LspStore::new(sessions.read(cx).connection().clone()));
        let harnesses = cx.new(|cx| super::harnesses::HarnessDirectory::new(sessions.clone(), cx));
        let instruction_homes = Rc::new(std::cell::RefCell::new(
            crate::instructions::HarnessHomes::from_env(),
        ));
        let usage = cx.new(|cx| super::usage::UsageDirectory::new(sessions.clone(), cx));
        let env = PanelEnv {
            sessions: sessions.clone(),
            settings: settings.clone(),
            harnesses: harnesses.clone(),
            usage: usage.clone(),
            regions: regions.clone(),
            instruction_homes: instruction_homes.clone(),
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

        let mut theme_watch_note = None;
        let theme_watch = match themes.start_watching() {
            Ok(guard) => Some(guard),
            Err(err) => {
                tracing::warn!("theme hot reload is off: {err}");
                // Only a data folder has packs to watch; the built-ins alone need no reload.
                if themes.data_dir().is_some() {
                    let note = hutan_settings::SettingsError::warning(
                        None,
                        "themes",
                        format!(
                            "changes to theme and icon packs are not picked up automatically ({err}); use Validate / Reload in Settings > Appearance"
                        ),
                    );
                    frame.diagnostics.push(note.clone());
                    settings.set_notes("themes", frame.diagnostics.clone());
                    theme_watch_note = Some(note);
                }
                None
            }
        };
        let mut theme_changes = themes.changes();
        let theme_task = cx.spawn_in(window, async move |this, cx| {
            while theme_changes.changed().await.is_ok() {
                let alive = this.update_in(cx, |this, _window, cx| this.refresh_theme(cx));
                if alive.is_err() {
                    break;
                }
            }
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
        // Where focus came from, for the keyboard focus ring (`ui::keyboard_focus`): a key that
        // walks focus or a selection, or any keystroke after which another element has focus,
        // makes it the keyboard; a mouse press anywhere (`ui::keyboard_focus_observer`) ends it.
        // An interceptor sees every keystroke, also those a view's key handler consumes.
        let focus_modality = {
            let window_handle = window.window_handle();
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != window_handle {
                    return;
                }
                super::ui::motion::note_input(true);
                if super::ui::navigates(&event.keystroke) {
                    if super::ui::set_keyboard_focus(true) {
                        window.refresh();
                    }
                    return;
                }
                let before = window.focused(cx);
                window.defer(cx, move |window, cx| {
                    if window.focused(cx) != before && super::ui::set_keyboard_focus(true) {
                        window.refresh();
                    }
                });
            })
        };
        let vim_keys = super::vim_keys::install(settings.clone(), window, cx);
        let checkpoint_store = sessions.read(cx).checkpoints().clone();
        let subscriptions = vec![
            cx.subscribe(&lsp, |this, _, event: &super::lsp::LspStoreEvent, cx| {
                this.on_lsp_event(event, cx);
            }),
            modal_guard,
            focus_modality,
            vim_keys,
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
            // Reduce Motion changed (macOS or the setting): the terminal's block gutter pulse
            // follows it.
            cx.observe_global::<super::ui::motion::Inputs>(|this, cx| {
                this.refresh_terminal_style(cx);
            }),
            cx.on_app_quit(|this, cx| this.on_platform_quit(cx)),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, change: &StoreChanged, window, cx| {
                    if change.intersects(StoreChanged::ATTENTION) {
                        this.sync_attention_marks(cx);
                        this.prune_attention_toasts(cx);
                    }
                    if change.intersects(StoreChanged::ATTENTION | StoreChanged::AGENTS) {
                        this.maybe_offer_notifications(cx);
                    }
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
                    } else if change.intersects(
                        StoreChanged::ATTENTION | StoreChanged::TASKS | StoreChanged::UNTRACKED,
                    ) {
                        if change.intersects(StoreChanged::TASKS) {
                            // Work that starts waiting for review while its chat or Changes
                            // view is on screen is seen at once.
                            this.sync_session_focus(cx);
                        }
                        cx.notify();
                    } else {
                        return;
                    }
                    this.schedule_publish(cx);
                },
            ),
            cx.observe(&harnesses, |this, _, cx| this.on_harnesses_changed(cx)),
            cx.observe(&usage, |this, _, cx| this.on_usage_changed(cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                this.window_active = window.is_window_active();
                this.sync_session_focus(cx);
                this.refresh_login_status(cx);
                this.schedule_publish(cx);
                if this.window_active {
                    this.sessions.update(cx, |store, cx| {
                        store.scan_agents(cx);
                        // Files may have changed while another app had the focus.
                        store.refresh_checkouts(cx);
                    });
                    if this.first_screen_open(cx) {
                        this.refresh_first_screen_agents(true, cx);
                    }
                }
            }),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, event: &UiOpenRequested, window, cx| {
                    this.on_ui_open(event, window, cx);
                },
            ),
            cx.subscribe(&sessions, |this, _, _: &RecoverySummaryDue, cx| {
                this.on_recovery_due(cx);
            }),
            // A command or a turn ended in a checkout: the branch may have changed.
            cx.subscribe(
                &sessions,
                |this, sessions, _: &crate::sessions::CheckoutActivity, cx| {
                    this.status_branch.mark_stale();
                    // The sidebar's change badges follow what the session did.
                    sessions.update(cx, |store, cx| store.refresh_checkouts(cx));
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, event: &crate::sessions::RequestCalled, window, cx| {
                    this.on_request_called(event, window, cx);
                },
            ),
            cx.subscribe_in(
                &sessions,
                window,
                |this, _, event: &crate::sessions::SessionClosed, window, cx| {
                    this.on_session_closed(event.session_id, window, cx);
                },
            ),
            // A turn ended: a preview that the running turn held up can go now.
            cx.subscribe(
                &checkpoint_store,
                |this, _, event: &crate::sessions::checkpoints::TurnEnded, cx| {
                    this.on_turn_ended(event.0, cx);
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
            themes,
            resolved_theme: frame.resolved,
            terminal_theme_name: frame.terminal.name,
            terminal_explicit,
            theme_notes: frame.diagnostics,
            theme_watch_note,
            sessions,
            env,
            terminals: HashMap::new(),
            terminal_finds: HashMap::new(),
            last_terminal_find: super::terminal_find::FindMemory::default(),
            chat_models: HashMap::new(),
            chats: HashMap::new(),
            rendered_chats: std::collections::HashSet::new(),
            pruned_chats: Vec::new(),
            last_chat: None,
            assistant_last: false,
            mawas_watch: None,
            known_approvals: std::collections::HashSet::new(),
            agent_picker: None,
            launcher: None,
            launcher_focus,
            providers: super::providers::ProvidersState::default(),
            history_index: super::settings_pages::HistoryIndex::default(),
            daemon_settings: super::settings_daemon::DaemonSettingsState::default(),
            assistant_defaults: super::settings_assistant::AssistantDefaults::default(),
            mawas_grants: super::settings_assistant::MawasGrants::default(),
            hooks_page: super::settings_hooks::HooksPage::default(),
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
            attention_marks: options
                .state_path
                .as_deref()
                .map(persistence::load_marks)
                .unwrap_or_default(),
            recent_files: options
                .state_path
                .as_deref()
                .map(persistence::load_recent_files)
                .unwrap_or_default(),
            quick_open: super::quick_open::QuickOpenTasks::default(),
            revealed_file: None,
            replace_job: None,
            chat_drafts: options
                .state_path
                .as_deref()
                .map(persistence::load_chat_drafts)
                .unwrap_or_default(),
            chat_draft_watch: HashMap::new(),
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
            daemon: DaemonControl::new(options.own_daemon),
            status_branch: super::status_branch::StatusBranch::default(),
            mac,
            quick: super::quick_terminal::QuickControl::new(cx),
            setup: super::setup::SetupControl::new(cx.focus_handle(), options.cli_link),
            first_run: super::first_run::FirstRunControl::default(),
            install,
            layout_offer,
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
            tab_scrolls: std::collections::HashMap::new(),
            tab_revealed: std::collections::HashMap::new(),
            tab_marked: std::collections::HashMap::new(),
            responsive: Responsive::default(),
            dock_modes_seen,
            dock_focus,
            focus_dock_seen: None,
            panel_views: HashMap::new(),
            changes_views: HashMap::new(),
            changes_subscriptions: Vec::new(),
            history_views: HashMap::new(),
            history_subscriptions: HashMap::new(),
            revision_views: HashMap::new(),
            buffers: super::files::BufferStore::default(),
            agent_ledger: super::agent_edits::AgentLedger::for_state(options.state_path.as_deref()),
            lsp,
            file_views: HashMap::new(),
            editor_items: None,
            file_subscriptions: HashMap::new(),
            rendered_files: std::collections::HashSet::new(),
            pruned_files: Vec::new(),
            notice: None,
            close_toast: None,
            toast_rev: 0,
            instructions: super::instructions::InstructionsState::default(),
            instructions_toast_exit: super::ui::motion::Ghost::default(),
            instruction_homes,
            layout_toast: None,
            layout_undo: None,
            layout_toast_exit: super::ui::motion::Ghost::default(),
            usage_toast: None,
            usage_toast_exit: super::ui::motion::Ghost::default(),
            paste_toast: None,
            paste_toast_exit: super::ui::motion::Ghost::default(),
            closing_tab: None,
            overlays: super::ui::motion::Presences::default(),
            exits: super::overlay_exit::OverlayExits::default(),
            close_toast_exit: super::ui::motion::Ghost::default(),
            restore_toast_exit: super::ui::motion::Ghost::default(),
            attention_toasts: super::attention_toast::AttentionToasts::default(),
            rewind: super::rewind::RewindState::new(cx),
            recovery_open: false,
            recovery_review: false,
            last_title: String::new(),
            regions,
            state_path: options.state_path,
            save_task: None,
            _watch: watch,
            _theme_watch: theme_watch,
            remote,
            _tasks: vec![settings_task, theme_task],
            _subscriptions: subscriptions,
        };
        this.mac.events_task = events_task;
        this.watch_quick_events(window, cx);
        this.start_agent_ledger(cx);
        // The first run is a first screen in the centre, not a modal walk-through; "Run setup
        // again" opens the walk-through.
        if first_run_setup
            && !this
                .settings
                .get::<crate::settings::AppSettings>()
                .setup_done
        {
            this.begin_first_run();
        }
        if install_changed {
            this.schedule_save(cx);
        }
        if !this.attention_marks.is_empty() {
            let marks = this.attention_marks.clone();
            this.sessions
                .update(cx, |store, cx| store.restore_marks(&marks, cx));
        }
        this.schedule_publish(cx);
        this.rebind_keys(cx);
        super::daemon::register_app_actions(cx);
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

    /// The chrome colors in effect.
    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// The color pack in effect for the chrome (the slot of the current polarity, the user's
    /// overrides applied).
    pub fn resolved_theme(&self) -> &Arc<hutan_settings::themes::ResolvedTheme> {
        &self.resolved_theme
    }

    /// The theme registry: the catalog of built-in and user packs.
    pub fn theme_registry(&self) -> &hutan_settings::ThemeRegistry {
        &self.themes
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

    /// What the title bar and the window name besides the app: the project (else the named
    /// workspace) the window shows, never the title of the active tab, which the tab strip
    /// right under it already says. `None` when there is no such name or it is the tab's own.
    pub fn window_label(&self, cx: &App) -> Option<String> {
        let store = self.sessions.read(cx);
        let info = store.active_project().or_else(|| {
            store
                .active_workspace_id()
                .and_then(|id| store.workspace(id))
                .filter(|w| w.kind != hutan_protocol::messages::WorkspaceKind::Home)
        })?;
        let name = info.name.trim();
        (!name.is_empty() && !name.eq_ignore_ascii_case(self.active_title(cx).trim()))
            .then(|| name.to_owned())
    }

    pub fn item_icon_and_title(
        &self,
        item: &ItemRef,
        cx: &App,
    ) -> (crate::icons::Icon, SharedString) {
        match item {
            ItemRef::Terminal { .. } | ItemRef::Chat { .. } | ItemRef::TerminalSplit { .. } => {
                let info = item
                    .session()
                    .and_then(|session| session.parse::<SessionId>().ok())
                    .and_then(|id| self.sessions.read(cx).session(id).cloned());
                match info {
                    Some(info) => (
                        crate::workbench::panels::session_icon(&info),
                        info.title.into(),
                    ),
                    None => (crate::icons::Icon::SessionTerminal, "Terminal".into()),
                }
            }
            // Mawas Home is the tab of a Mawas conversation: it wears that conversation's title
            // and icon (the scope follows the name after ` · `), as every other surface does.
            ItemRef::Panel { panel } if panel.as_str() == "assistant" => {
                let shown = self
                    .panel::<AssistantPanel>("assistant")
                    .and_then(|view| view.read(cx).session(cx))
                    .and_then(|session| self.sessions.read(cx).session(session).cloned());
                match shown {
                    Some(info) => (
                        crate::workbench::panels::session_icon(&info),
                        info.title.into(),
                    ),
                    None => (
                        self.panels
                            .get(panel)
                            .map(|s| s.icon)
                            .unwrap_or(crate::icons::Icon::StateInfo),
                        self.panels.title(panel),
                    ),
                }
            }
            ItemRef::Panel { panel } => (
                self.panels
                    .get(panel)
                    .map(|s| s.icon)
                    .unwrap_or(crate::icons::Icon::StateInfo),
                self.panels.title(panel),
            ),
            ItemRef::Placeholder { title } => (crate::icons::Icon::StateInfo, title.clone().into()),
            ItemRef::Changes { path } => (
                crate::icons::Icon::PanelChanges,
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
            ItemRef::File { path } => (
                crate::icons::Icon::ItemFile,
                hutan_code_view::file_name(std::path::Path::new(path)).into(),
            ),
            ItemRef::History { path } => (
                crate::icons::Icon::PanelHistory,
                format!(
                    "History · {}",
                    hutan_code_view::file_name(std::path::Path::new(path))
                )
                .into(),
            ),
            ItemRef::Revision { path, rev } => (
                crate::icons::Icon::ItemFile,
                format!(
                    "{} @ {}",
                    hutan_code_view::file_name(std::path::Path::new(path)),
                    rev.get(..7).unwrap_or(rev)
                )
                .into(),
            ),
        }
    }

    /// What hovering the tab of `item` says: the session's title with its placement badge
    /// (`main` / `wt <branch>`) and the checkout. `None` for panels and for sessions outside git.
    pub fn item_tooltip(&self, item: &ItemRef, cx: &App) -> Option<String> {
        if let ItemRef::File { path } = item {
            return Some(path.clone());
        }
        let id = match item {
            ItemRef::Terminal { .. } | ItemRef::Chat { .. } | ItemRef::TerminalSplit { .. } => {
                item.session()?.parse::<SessionId>().ok()?
            }
            _ => return None,
        };
        let store = self.sessions.read(cx);
        let info = store.session(id)?;
        let folder = crate::sessions::tree::session_folder(info).map(|path| path.display());
        // A short proposed title is the first clause of the prompt: the prompt, in full, is here.
        let prompt = store.tasks().prompt_of(id, &info.title);
        let head = prompt.clone().unwrap_or_else(|| info.title.clone());
        match (crate::sessions::placement_badge(info), folder) {
            (Some(badge), _) => Some(format!("{head} · {}\n{}", badge.text, badge.tooltip)),
            // Without a checkout the tab still says where the session is, in full.
            (None, Some(folder)) => Some(format!("{head}\n{folder}")),
            (None, None) => prompt,
        }
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

    pub(crate) fn after_change(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.note_dock_modes();
        self.attach_open_items(cx);
        self.sync_session_focus(cx);
        // View → Layout Preset checks the preset on screen.
        self.sync_menus(cx);
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

    /// Write the layout (and the agent ledger) now if it changed since the last write. Called
    /// by the debounce timer, on quit, and by tests.
    pub fn flush_save(&mut self) {
        self.flush_agent_ledger();
        let Some(path) = &self.state_path else {
            return;
        };
        let json = match persistence::state_json(
            &self.state,
            &self.collapsed_blocks,
            &self.review_comments,
            &self.launcher_memory,
            &self.chat_drafts,
            &self.attention_marks,
            &self.recent_files,
            self.install.record.as_ref(),
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
            // The slot of the new polarity, and `terminal.theme = "auto"`, follow it.
            self.refresh_theme(cx);
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
            .chain(self.file_views.values().map(Entity::entity_id))
            .chain(self.history_views.values().map(Entity::entity_id))
            .chain(self.revision_views.values().map(Entity::entity_id))
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
        if actions::is_mawas_page(id) && !self.assistant_enabled() {
            return;
        }
        let id = PanelId::new(id);
        // Memory is a body of the left sidebar (it replaces Projects until ‹ Projects), never a
        // tab of the center: it comes back to the left dock from wherever it was left.
        if id.as_str() == super::model::MEMORY_PAGE {
            self.mutate(cx, |s| s.open_panel_in_dock(&id, DockPosition::Left));
            return;
        }
        // Mawas Home and the Agents page are pages of the center: a tab of the active pane
        // (one instance, shown where it is when it is open), not a dock's panel.
        if actions::is_mawas_page(id.as_str()) || id.as_str() == super::model::AGENTS_PAGE {
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
    pub fn drop_item(
        &mut self,
        dragged: &DraggedItem,
        target: DropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A panel that leaves a dock or arrives in one is a change of the arrangement: kept in
        // the current preset, with an Undo (`customize.rs`).
        match dragged.item.as_panel() {
            Some(panel)
                if matches!(dragged.source, ItemSource::Dock { .. })
                    || matches!(target, DropTarget::Dock { .. }) =>
            {
                self.drop_panel(panel, dragged.source.clone(), target, cx);
            }
            _ => {
                self.try_mutate(cx, |s| s.move_item(dragged.source.clone(), target));
            }
        }
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
        self.palette = Some(PaletteState::default());
        self.refilter_palette(cx);
        self.edits.palette.reset();
        self.context_menu = None;
        self.picker = None;
        window.focus(&self.palette_focus, cx);
        cx.notify();
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.palette.take() {
            self.leave_palette(state, cx);
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub fn set_palette_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if let Some(palette) = &mut self.palette {
            palette.query = query.to_owned();
            self.refilter_palette(cx);
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
            submenu: false,
            run: Rc::new(run),
        }
    }

    pub(crate) fn submenu_entry(
        label: impl Into<SharedString>,
        run: impl Fn(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
    ) -> MenuEntry {
        MenuEntry {
            submenu: true,
            ..Self::entry(label, run)
        }
    }

    /// A greyed-out entry that shows why it does nothing.
    pub(crate) fn disabled_entry(label: impl Into<SharedString>) -> MenuEntry {
        MenuEntry {
            label: label.into(),
            checked: false,
            disabled: true,
            submenu: false,
            run: Rc::new(|_, _, _| {}),
        }
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
                    move |this, _, cx| this.place_panel(&panel, PanelDest::Dock(pos), cx),
                ));
            }
            if super::customize::panel_settings_page(panel.as_str()).is_some() {
                let at = self.regions.get(Region::Tab(pane, index));
                entries.push(Self::entry("Panel settings…", move |this, window, cx| {
                    let anchor = at.map_or_else(Default::default, |b| b.bottom_left());
                    this.open_panel_settings(&panel, anchor, window, cx);
                }));
            }
        }
        let file = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .and_then(|i| i.as_file().map(str::to_owned));
        if let Some(path) = file {
            entries.extend(self.file_tab_entries(pane, &path));
        }
        entries.push(Self::entry("Close", move |this, window, cx| {
            this.close_item_checked(pane, index, window, cx);
        }));
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
            Resizing::TermSplit {
                pane,
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
                let min = self.term_split_min_share(horizontal, extent);
                self.mutate(cx, |s| {
                    let _ = s.set_term_divider(pane, &path, divider, position, min);
                });
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
