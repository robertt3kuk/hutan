//! Named workbench actions, the command table behind the command palette, and the default keymaps.
//!
//! One list feeds three consumers (ADR 0010): the palette, the menus, and the keymap. Every
//! action that has a key by default is parameterless so `keymap.toml` can name it
//! (`workbench::SplitDown`); actions with data (`ApplyPreset(name)`, `OpenSession(id)`, …) are
//! reachable from the palette, menus and panels.

use super::model::{Direction, DockPosition, Edge, LayoutPreset, PanelDest};
use super::panels::{HistoryResume, HutanTarget};
use super::registry::PanelRegistry;
use crate::settings::{KeymapBase, SidebarOrganize, WorkspaceSort};
use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, SharedString, actions};
use hutan_settings::keymap::{BindingSpec, KeymapLoad, format_keys};

actions!(
    workbench,
    [
        SplitRight,
        SplitDown,
        SplitLeft,
        SplitUp,
        SplitTerminalRight,
        SplitTerminalDown,
        FocusSplitLeft,
        FocusSplitRight,
        FocusSplitUp,
        FocusSplitDown,
        ResizeSplitLeft,
        ResizeSplitRight,
        ResizeSplitUp,
        ResizeSplitDown,
        ToggleSplitZoom,
        CloseSplit,
        CloseTerminalTab,
        FindInTerminal,
        FindNextInTerminal,
        FindPreviousInTerminal,
        MoveSplitToNewTab,
        EqualizeSplits,
        CloseAllDocks,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        RedrawTerminal,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock,
        HideLeftSidebar,
        HideRightRail,
        CancelDockResize,
        ClosePeek,
        ToggleOverview,
        ShowAgentsPage,
        ClosePaneItem,
        ToggleZoom,
        TogglePalette,
        NewTerminal,
        ToggleQuickTerminal,
        NewAgent,
        NewChat,
        StopSession,
        CloseSession,
        RenameSession,
        RestartSession,
        CopySessionId,
        JumpToNextAttention,
        ReviewRequest,
        DismissAttentionToasts,
        /// ⌘Z while the "Moved … · Undo" toast of a layout change shows.
        UndoLayoutChange,
        SaveLayoutAsPreset,
        ResetLayout,
        UseNewDefaultLayout,
        OpenSettings,
        About,
        Quit,
        MinimizeWindow,
        ZoomWindow,
        ToggleDensity,
        ToggleVimMode,
        ToggleSendKey,
        SetDensityCompact,
        SetDensityComfortable,
        ResetTheme,
        OpenActivePanelAsTab,
        FocusNeighborLeft,
        FocusNeighborRight,
        FocusNeighborUp,
        FocusNeighborDown,
        ResizeLeft,
        ResizeRight,
        ResizeUp,
        ResizeDown,
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateTab9,
        ActivatePreviousTab,
        ActivateNextTab,
        ApplyPresetNumber1,
        ApplyPresetNumber2,
        ApplyPresetNumber3,
        ApplyPresetNumber4,
        ApplyPresetNumber5,
        ApplyPresetNumber6,
        ApplyPresetNumber7,
        ApplyPresetNumber8,
        ApplyPresetNumber9,
        OpenProject,
        OpenFolder,
        OpenJump,
        GoHome,
        NextContainer,
        PreviousContainer,
        PinActiveWorkspace,
        RenameActiveWorkspace,
        AddFolderToWorkspace,
        CloseActiveWorkspace,
        NewWorkspace,
        RevealWorkspace,
        ToggleHideEmptyWorkspaces,
        ToggleHideSleepingWorkspaces,
        ToggleHideCliWorkspaces,
        SearchHistory,
        ReviewApprovals,
        ShowAgentMonitor,
        ShowOnlyBlockedAgents,
        ShowResources,
        ToggleResources,
        ManageProviders,
        ManageAccounts,
        SwitchChatModel,
        OpenAssistant,
        NewMawasChat,
        OpenMawasMemory,
        NewTerminalInWorktree,
        RemoveWorktree,
        /// List the worktrees the cleanup policy offers and remove the ones the user confirms.
        CleanUpWorktrees,
        NewTask,
        ShowTasksPanel,
        ReviewChanges,
        RunSetup,
        OpenFileDialog,
        OpenFileInSplitRight,
        OpenFileInSplitDown,
        QuickOpen,
        ShowFiles,
        /// Ask to put the Home folder under version control (`project.git_init`): a sheet
        /// explains what happens first.
        TrackHomeChanges,
        RevealActiveFile,
        /// Give the keyboard back to what the focused tab shows (Esc in the Files tree); the
        /// panel stays open.
        FocusEditor,
        /// Close every folder of the Files tree but the root.
        FilesCollapseAll,
        /// List the Files tree's open folders again and read the git marks now.
        FilesRefresh,
        /// New file in the folder the Files tree has selected (an inline name, Return makes it).
        FilesNewFile,
        /// New folder in the folder the Files tree has selected.
        FilesNewFolder,
        /// Rename the selected row of the Files tree in place, the stem of its name selected.
        FilesRename,
        /// Copy the selected row next to itself as `name copy`.
        FilesDuplicate,
        /// Move the selected row to the Trash (a sheet asks first).
        FilesTrash,
        /// Remember the selected row to paste it elsewhere in the tree.
        FilesCopy,
        /// Remember the selected row to move it by pasting.
        FilesCut,
        /// Paste what ⌘C / ⌘X remembered, or the files Finder copied, into the selected folder.
        FilesPaste,
        /// Undo the last file operation of this window's Files tree (a batch is one step).
        FilesUndo,
        /// Redo the file operation the last undo took back.
        FilesRedo,
        /// Put the full path of the selected rows on the clipboard.
        FilesCopyPath,
        /// Put the paths of the selected rows, relative to the tree's root, on the clipboard.
        FilesCopyRelativePath,
        /// Show the selected rows in Finder (a daemon on this Mac).
        FilesRevealInFinder,
        /// Start a terminal in the selected folder (a file's folder).
        FilesOpenInTerminal,
        /// Attach the selected files to the chat that is open (else copy `@path` text).
        FilesAttachToChat,
        /// Search the selected folder (a file's folder): the Search panel with its include glob.
        FilesFindInFolder,
        /// Remember the selected file as the left side of a compare.
        FilesSelectForCompare,
        /// Compare the selected file with the one *Select for Compare* remembered.
        FilesCompareWithSelected,
        /// Compare the selected file with its version in HEAD.
        FilesCompareWithHead,
        /// Put the keyboard in the Files panel's filter field (⌘F in the tree).
        FilesFilter,
        /// Filter the folder of the Files tree's selected row (the root when nothing is
        /// selected) instead of the whole project.
        FilesFilterInFolder,
        /// Show the history of the focused file tab's file in a tab.
        ShowFileHistory,
        RewindFocusedChat,
        UndoLastRestore,
        /// Show the Problems panel (the language servers' diagnostics) with the keyboard in it.
        ShowProblems,
        /// Show the language server's trust bar of the focused file's folder again (the status
        /// bar's `untrusted`).
        ReviewLspTrust,
        /// Show the Search panel (find in the active project's files) with the keyboard in its
        /// query; the focused file tab's selection becomes the query.
        ShowSearch,
        /// [`ShowSearch`] with the replace field shown.
        ShowSearchReplace,
        /// Show the Instructions panel: the files Codex, Claude Code and OMP read.
        ShowInstructions,
        /// Make AGENTS.md the one file every agent reads (a sheet shows what is written).
        ShareInstructions,
        /// Create AGENTS.md in the active project.
        CreateAgentsMd,
    ]
);

/// Palette-local actions, bound only inside the `Palette` key context.
pub mod palette {
    use gpui::actions;
    actions!(
        palette,
        [SelectNext, SelectPrev, Confirm, ConfirmInSplit, Dismiss]
    );
}

/// Actions of the prompt/confirm dialog (`Modal` key context).
pub mod modal {
    use gpui::actions;
    actions!(modal, [Confirm, Cancel]);
}

/// Actions of the agent chat view (`HutanChat` key context).
pub mod chat {
    use gpui::actions;
    actions!(chat, [SendMessage, InterruptTurn, SendNow]);
}

/// Actions of the settings sheet (`SettingsSheet` key context).
pub mod sheet {
    use gpui::actions;
    actions!(settings_sheet, [Dismiss]);
}

/// Actions of the first-run setup (`Setup` key context).
pub mod setup {
    use gpui::actions;
    actions!(setup, [Continue, Skip]);
}

/// Daemon controls (design.md § Daemon lifecycle). The `Daemon` menu, the status-bar popover
/// and the palette all dispatch these, so there is one implementation. They are handled
/// app-wide (`workbench::daemon::register_app_actions`), so the menu finds them whatever has
/// focus, or when no window does.
pub mod daemon {
    use gpui::actions;
    actions!(
        daemon,
        [
            Status,
            Restart,
            Stop,
            Start,
            OpenLogs,
            ToggleKeepRunning,
            ToggleStartAtLogin,
            OpenLoginItems,
            // When the daemon could not start (`daemon_start_failure`):
            StartWithEmptyData,
            ShowStuckDaemon,
            StopStuckDaemon,
            CopyStartError,
            CopyUpdateCommand,
            // When the daemon is not this app's build (`daemon_mismatch`):
            UseOwnDaemon,
            DismissMismatch,
            // What a `Daemon` menu item that cannot act now carries: nothing handles it, so
            // the menu's validation (`App::is_action_available`, which AppKit asks instead of
            // reading the item's `disabled` flag) shows the item disabled.
            Unavailable
        ]
    );
}

macro_rules! data_actions {
    ($($(#[$meta:meta])* $name:ident($ty:ty);)*) => {
        $(
            $(#[$meta])*
            #[derive(Clone, PartialEq, Debug, gpui::Action)]
            #[action(namespace = workbench, no_json)]
            pub struct $name(pub $ty);
        )*
    };
}

/// A session to open next to the focused pane.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionSplit {
    pub session: String,
    pub edge: Edge,
}

/// Where to open the context menu of an agent node of a session's observation tree.
#[derive(Clone, PartialEq, Debug)]
pub struct AgentNodeAt {
    pub session: String,
    pub node: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Where to open the context menu of a line of the resources panel: a session (no `pid`, no
/// `node`), a process (`pid`) or an agent without a process of its own (`node`).
#[derive(Clone, PartialEq, Debug)]
pub struct ResourceMenuAt {
    pub session: String,
    pub pid: Option<u32>,
    pub node: Option<String>,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Where to open the context menu of a session.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionMenuAt {
    pub session: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// A checkpoint of a chat session (ADR 0019): the turn to rewind, or the restore to undo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CheckpointAt {
    pub session: hutan_core::SessionId,
    pub checkpoint: hutan_core::CheckpointId,
}

/// What a "▶ Start" button opens the Start… sheet with.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LauncherOpen {
    /// The project to start in (an id); none: the one in focus.
    pub project: Option<String>,
    /// "Queue as task" instead of "Start now".
    pub queue: bool,
}

data_actions! {
    /// Apply the named layout preset (`default`, `focus`, `overview`, …).
    ApplyPreset(String);
    /// Reveal the panel with this id, opening it in its default dock if it is closed.
    OpenPanel(String);
    /// Offer to install the missing package of this harness (`omp`, `claude`, `codex`) from its
    /// original source (`docs/design.md` § First-run setup).
    InstallHarness(String);
    /// Switch the next turn's direct-chat model.
    SwitchChatModelById(String);
    /// Open a write-only credential prompt for a provider.
    SetProviderKey(String);
    /// Move the focused panel (dock tab or panel tab) to a dock or into the center.
    MoveActivePanel(PanelDest);
    /// Move the active tab into a new split at this edge of its pane.
    MoveActiveTabToSplit(Edge);
    /// Open (or focus the tab of) the session with this id.
    OpenSession(String);
    /// Open the session in a new split of the focused pane.
    OpenSessionInSplit(SessionSplit);
    /// Show the context menu of an agent node of the monitor (Open session, Open transcript,
    /// Copy node id).
    ShowAgentNodeMenu(AgentNodeAt);
    /// Show the context menu of a line of the resources panel (signals, stop / kill session,
    /// interrupt agent).
    ShowResourceMenu(ResourceMenuAt);
    /// Reveal the resources panel with this session's processes in view.
    ShowInResources(String);
    /// Open the Agents page, with a session selected (and its request expanded), a state
    /// filter or a scope when the request says so.
    OpenAgentsPageWith(super::panels::AgentsRequest);
    /// Show the menu of a row of the Agents page or panel (Reveal session, Open worktree, Show
    /// changes, Copy session ID, Open transcript, Interrupt turn, Stop session…).
    ShowAgentMenu(SessionMenuAt);
    /// Show the menu of the Agents page's scope button at this position.
    ShowAgentsScopeMenu(gpui::Point<gpui::Pixels>);
    /// Show the Agents page's ⋯ menu at this position: Group by workspace, Show finished and,
    /// below 640 pt where the toolbar folds, the scope.
    ShowAgentsPageMenu(gpui::Point<gpui::Pixels>);
    /// Show the right Agents panel's ⋯ menu at this position: Show finished, Blocked only, All
    /// agents and the panel's settings.
    ShowAgentsPanelMenu(gpui::Point<gpui::Pixels>);
    /// Interrupt the turn in progress of this session (an agent chat the daemon runs).
    InterruptSessionById(String);
    /// Show this session in the Projects sidebar (its workspace and row).
    RevealSessionById(String);
    /// Reveal the checkout folder of this session in Finder.
    OpenWorktreeOf(String);
    /// Reveal the transcript file of this session's main agent in Finder.
    OpenTranscriptOf(String);
    /// Open the Changes view of this session's checkout.
    ShowAgentChanges(String);
    /// Preview restoring the files of a chat turn's checkpoint (the turn footer's *Rewind…*,
    /// the Checkpoints list).
    RewindCheckpoint(CheckpointAt);
    /// Restore a restore's checkpoint: Undo of `Restored — Undo` and of the toast.
    UndoRestore(CheckpointAt);
    /// The Checkpoints list of a chat session (Agents page → ⋯ → Checkpoints…).
    ShowCheckpoints(hutan_core::SessionId);
    StopSessionById(String);
    /// Close the session (asks first when something runs in it); History's Reopen is
    /// `ReopenSessionById`.
    CloseSessionById(String);
    ReopenSessionById(String);
    RenameSessionById(String);
    RestartSessionById(String);
    CopySessionIdOf(String);
    /// Show the session's context menu (Open in split, Rename, Restart, Stop, Copy id).
    ShowSessionMenu(SessionMenuAt);
    /// Leave the Agents overview (restoring the layout it replaced) and focus this session.
    OpenSessionFromOverview(String);
    /// Open the settings sheet at a namespace (`terminal`, `panels.sessions`, …).
    OpenSettingsFor(String);
    /// Scroll the terminal of the session to a block and highlight it.
    JumpToBlock(BlockRef);
    /// Show the workspace with this id (switching the center to its layout).
    OpenWorkspace(String);
    /// Show a worktree of a project (its row in the sidebar): the project becomes the active
    /// one and Files and Search follow the worktree.
    OpenCheckout(CheckoutOpen);
    /// Open the Start… sheet in a project (the ▶ buttons, the empty states, the Tasks panel).
    OpenLauncher(LauncherOpen);
    /// Show the context menu of a workspace row.
    ShowWorkspaceMenu(WorkspaceMenuAt);
    /// Show the menu of workspace sort modes at this position.
    ShowSortMenu(gpui::Point<gpui::Pixels>);
    /// Sort the sidebar's workspaces and projects (`workspaces.sort`).
    SetWorkspaceSort(WorkspaceSort);
    /// A workspace row was dropped onto another one (manual reordering).
    MoveWorkspaceOnto(WorkspaceDrop);
    /// Show the menu of the sidebar's "+" button at this position.
    ShowPlusMenu(gpui::Point<gpui::Pixels>);
    /// Show the context menu of a project row (and of the projects selected with it).
    ShowProjectMenu(ProjectMenuAt);
    /// A project row was dropped onto a workspace row, a project row, or the "No workspace"
    /// area.
    DropProjectOnto(ProjectDrop);
    /// Organize the sidebar by workspaces or by projects (`sidebar.organize`).
    SetSidebarOrganize(SidebarOrganize);
    /// The inline rename of a workspace or project row was confirmed.
    RenameWorkspaceTo(RenameTo);
    /// The inline rename of a session row was confirmed (an empty title only returns focus).
    RenameSessionTo(SessionRenameTo);
    /// Resume a session found in the history index: its command in a new terminal session.
    ResumeHistoryHit(HistoryResume);
    /// Copy a resume command line to the clipboard.
    CopyHistoryCommand(String);
    /// Show the Hutan session (and block) a history hit points at.
    OpenHistoryHit(HutanTarget);
    /// Reveal the transcript file of a history hit of another tool in Finder.
    RevealHistoryFile(String);
    /// Show a user-level instruction file (`~/.claude/CLAUDE.md`, …) in Finder.
    RevealInstructionFile(String);
    /// Open the transcript file of a history session with the system (its default app).
    OpenHistoryFile(String);
    /// Show the workspace that holds a history session's working directory (found or created).
    OpenHistoryDirectory(String);
    /// Show a line in the workbench's notice area (panels report results with it).
    Notify(String);
    /// Open the Changes view of the checkout a task runs in (the task's id).
    ReviewTaskChanges(String);
    /// Show the menu of a session's placement badge (Review Changes…, Copy checkout path).
    ShowPlacementMenu(SessionMenuAt);
    /// Show the Stop menu of a chat whose queue holds messages (Stop and clear the queue / Stop,
    /// keep the queue).
    ShowStopMenu(SessionMenuAt);
    /// Show the context menu of a row of the Files panel (a file or a folder, by its path) at a
    /// position: *Show File History* for a file.
    ShowFilesMenu(FilesMenuAt);
    /// Show the Files panel's ⋯ menu (hidden and ignored files, folding, indent guides, expand
    /// and collapse all, refresh) at a position.
    ShowFilesMoreMenu(gpui::Point<gpui::Pixels>);
    /// Ask before moving these files and folders (absolute paths) to the Trash: the sheet lists
    /// them, the tabs with unsaved changes and an agent working there; *Move to Trash* runs it.
    FilesAskTrash(Vec<String>);
    /// Ask before the first drag of a session moves rows into another folder.
    FilesAskMove(FilesMove);
    /// Files or folders were renamed or moved on disk (`(from, to)`, absolute): the tabs, buffers
    /// (unsaved text included), watches and language-server documents follow them.
    FilesTabsMoved(Vec<(String, String)>);
    /// These paths went to the Trash: their tabs say the file is deleted and keep their text.
    FilesTabsTrashed(Vec<String>);
    /// These paths came back from the Trash: their tabs find their file again.
    FilesTabsRestored(Vec<String>);
    /// Open the Changes view of the checkout at this path (a worktree or a main checkout).
    ReviewChangesAt(String);
    /// Open the Changes view of the checkout of a changed file with that file selected (a file
    /// row of the Changes panel).
    ReviewFileAt(ChangedFileAt);
    /// Show a changed file in the Files panel, scoped to its checkout (the Changes view's *Show
    /// in Files*): the sidebar picks that checkout, so Files follows it, and the file is selected.
    ShowInFiles(ChangedFileAt);
    /// Open the Changes view of the checkout at this path with its Finish form (a group header
    /// of the Changes panel).
    FinishChangesAt(String);
    /// Show the confirmation sheet for the setup commands the repository asks to run in the
    /// worktree at this path.
    ReviewWorktreeSetup(String);
    /// Show the *Setup* terminal session of the worktree at this path.
    ShowWorktreeSetupOutput(String);
    /// Run the setup of the worktree at this path again.
    RerunWorktreeSetup(String);
    /// Show the menu of the Assistant page's scope dropdown (Global / the workspace) at this
    /// position.
    ShowAssistantScopeMenu(gpui::Point<gpui::Pixels>);
    /// Show the menu of the Assistant page's backend and model chip (Switch model…) at this
    /// position.
    ShowAssistantModelMenu(gpui::Point<gpui::Pixels>);
    /// Show the menu of Mawas Home's mode chip (Ask before acting / Advisor) at this position.
    ShowAssistantModeMenu(gpui::Point<gpui::Pixels>);
    /// Show Mawas Home's ⋯ menu at this position: New chat, Stop, Today and the backend; below
    /// 640 pt it holds what the header folded.
    ShowAssistantMoreMenu(gpui::Point<gpui::Pixels>);
    /// Show the Memory page's ⋯ menu (Export…, Import…, Show files) at this position.
    ShowMemoryMenu(gpui::Point<gpui::Pixels>);
    /// Show the menu behind a composer's send-key control (⌘Return or Return sends) at this
    /// position.
    ShowSendKeyMenu(gpui::Point<gpui::Pixels>);
    /// Open the Memory page with the editor of this memory proposal (its id) open: "Edit &
    /// approve" of a card in Today.
    EditMemoryProposal(String);
    /// Open a file in a tab (or focus its tab) at a line: `ui.run workbench::OpenFile`, `hutan
    /// open`. The daemon reads it only below the workspace roots (an agent may ask for this).
    OpenFile(FileTarget);
    /// Open a file the user chose (the Files panel, ⌘P) in the focused pane, a split or as a
    /// preview that leaves the keyboard where it is. Read as picked.
    OpenChosenFile(ChosenFile);
    /// Open a file in the focused pane (or focus its tab) with the caret at a language server's
    /// position: the Problems panel's rows.
    OpenAtPosition(PositionTarget);
    /// Replace the chosen matches of the Search panel, one undo step per file (E-13b).
    ReplaceInFiles(super::search_replace::ReplaceRequest);
}

/// A Files panel row's context menu: whose, and where.
#[derive(Clone, PartialEq, Debug)]
pub struct FilesMenuAt {
    /// Absolute.
    pub path: String,
    /// Whether the row is a file (a folder has no history of its own to show in a tab).
    pub file: bool,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Rows of the Files tree to move or copy into a folder (a drag, a paste of what ⌘X remembered).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FilesMove {
    /// Absolute.
    pub items: Vec<String>,
    /// Absolute: the folder they go into.
    pub dir: String,
    /// Copy instead of move (⌥ while dropping).
    pub copy: bool,
}

/// A file to open, and where in it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileTarget {
    /// Absolute.
    pub path: String,
    /// 1-based.
    pub line: Option<u32>,
    /// 1-based, in characters.
    pub column: Option<u32>,
}

/// A file the user chose and where to show it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChosenFile {
    /// Absolute.
    pub path: String,
    /// 1-based.
    pub line: Option<u32>,
    /// 1-based, in characters.
    pub column: Option<u32>,
    pub place: super::files::FilePlace,
}

/// A file and a position in it as language servers name it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PositionTarget {
    /// Absolute.
    pub path: String,
    /// Zero-based.
    pub line: u32,
    /// Zero-based, in UTF-16 code units.
    pub character: u32,
}

/// Where to open the context menu of a workspace.
#[derive(Clone, PartialEq, Debug)]
pub struct WorkspaceMenuAt {
    pub workspace: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// Where to open the context menu of a project row, and the projects selected when it opened
/// (project ids, the clicked one included).
#[derive(Clone, PartialEq, Debug)]
pub struct ProjectMenuAt {
    pub project: String,
    pub selection: Vec<String>,
    pub position: gpui::Point<gpui::Pixels>,
}

/// A dragged workspace row and the row it was dropped on (workspace ids).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceDrop {
    pub dragged: String,
    pub target: String,
}

/// A dragged project row and where it was dropped: on a workspace or project row (`target` is
/// its id) or on the "No workspace" area (`None`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectDrop {
    pub dragged: String,
    pub target: Option<String>,
}

/// A project (an id) and one of its worktrees (a folder).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CheckoutOpen {
    pub project: String,
    pub path: String,
}

/// A checkout (its folder) and one of its changed files (relative to it): a file row of the
/// Changes panel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangedFileAt {
    pub checkout: String,
    pub file: String,
}

/// A workspace or project (`workspace` is its id) and the name the user typed for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RenameTo {
    pub workspace: String,
    pub name: String,
}

/// A session (`session` is its id) and the title the user typed for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionRenameTo {
    pub session: String,
    pub title: String,
}

/// A block of a session's terminal (`block` is the daemon's block id).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockRef {
    pub session: String,
    pub block: u64,
}

/// A palette entry: human title, the action it runs, and the action whose key binding is shown
/// as its hint.
pub struct Command {
    pub title: SharedString,
    pub action: Box<dyn Action>,
    pub hint: Box<dyn Action>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("title", &self.title)
            .field("action", &self.action.name())
            .finish_non_exhaustive()
    }
}

fn command<A: Action + Clone>(title: impl Into<SharedString>, action: A) -> Command {
    Command {
        title: title.into(),
        hint: Box::new(action.clone()),
        action: Box::new(action),
    }
}

fn dir_name(dir: Direction) -> &'static str {
    match dir {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "Left",
        Edge::Right => "Right",
        Edge::Top => "Up",
        Edge::Bottom => "Down",
    }
}

/// The parameterless action for focusing the neighbor in `dir`.
pub fn focus_neighbor_action(dir: Direction) -> Box<dyn Action> {
    match dir {
        Direction::Left => Box::new(FocusNeighborLeft),
        Direction::Right => Box::new(FocusNeighborRight),
        Direction::Up => Box::new(FocusNeighborUp),
        Direction::Down => Box::new(FocusNeighborDown),
    }
}

/// The parameterless action for resizing towards `dir`.
pub fn resize_action(dir: Direction) -> Box<dyn Action> {
    match dir {
        Direction::Left => Box::new(ResizeLeft),
        Direction::Right => Box::new(ResizeRight),
        Direction::Up => Box::new(ResizeUp),
        Direction::Down => Box::new(ResizeDown),
    }
}

/// The parameterless action selecting the `n`-th (1-based) preset.
pub fn preset_number_action(n: usize) -> Option<Box<dyn Action>> {
    Some(match n {
        1 => Box::new(ApplyPresetNumber1),
        2 => Box::new(ApplyPresetNumber2),
        3 => Box::new(ApplyPresetNumber3),
        4 => Box::new(ApplyPresetNumber4),
        5 => Box::new(ApplyPresetNumber5),
        6 => Box::new(ApplyPresetNumber6),
        7 => Box::new(ApplyPresetNumber7),
        8 => Box::new(ApplyPresetNumber8),
        9 => Box::new(ApplyPresetNumber9),
        _ => return None,
    })
}

/// Every palette command. Order is the order shown for an empty query.
pub fn commands(presets: &[LayoutPreset], panels: &PanelRegistry, assistant: bool) -> Vec<Command> {
    let mut out = vec![
        command("Session: New Terminal", NewTerminal),
        command("Terminal: Toggle Quick Terminal", ToggleQuickTerminal),
        command("Start Agent or Task…", NewAgent),
        command("Chat: New Direct Chat…", NewChat),
        command("Session: Rename…", RenameSession),
        command("Session: Restart", RestartSession),
        command("Session: Stop…", StopSession),
        command("Session: Close…", CloseSession),
        command("Session: Copy Session ID", CopySessionId),
        command("Session: Jump to Next Attention", JumpToNextAttention),
        command("Session: Review Request", ReviewRequest),
        command("Layout: Split Right", SplitRight),
        command("Layout: Split Down", SplitDown),
        command("Layout: Split Left", SplitLeft),
        command("Layout: Split Up", SplitUp),
        command("Layout: Toggle Zoom Pane", ToggleZoom),
        command("Layout: Reset Layout", ResetLayout),
        command("Layout: Use New Default", UseNewDefaultLayout),
        command("Layout: Save Layout as Preset…", SaveLayoutAsPreset),
        command("Tab: Close (Detach)", ClosePaneItem),
        command("Tab: Open Panel as Tab", OpenActivePanelAsTab),
        command("Tab: Previous", ActivatePreviousTab),
        command("Tab: Next", ActivateNextTab),
        command("View: Toggle Left Dock", ToggleLeftDock),
        command("View: Toggle Right Dock", ToggleRightDock),
        command("View: Toggle Bottom Dock", ToggleBottomDock),
        command("View: Close All Docks", CloseAllDocks),
        command("View: Hide Left Sidebar", HideLeftSidebar),
        command("View: Hide Right Rail", HideRightRail),
        command("View: Increase Terminal Font Size", IncreaseFontSize),
        command("View: Decrease Terminal Font Size", DecreaseFontSize),
        command("View: Reset Terminal Font Size", ResetFontSize),
        command("Terminal: Redraw", RedrawTerminal),
        command("Terminal: Split Right", SplitTerminalRight),
        command("Terminal: Split Down", SplitTerminalDown),
        command("Terminal: Close Split", CloseSplit),
        command("Terminal: Close Tab (All Splits)", CloseTerminalTab),
        command("Terminal: Find…", FindInTerminal),
        command("Terminal: Scroll to Top", hutan_terminal_view::ScrollToTop),
        command(
            "Terminal: Scroll to Bottom",
            hutan_terminal_view::ScrollToBottom,
        ),
        command("Terminal: Find Next", FindNextInTerminal),
        command("Terminal: Find Previous", FindPreviousInTerminal),
        command("Terminal: Move Split to New Tab", MoveSplitToNewTab),
        command("Terminal: Toggle Zoom Split", ToggleSplitZoom),
        command("Terminal: Equalize Splits", EqualizeSplits),
        command("Terminal: Focus Split Left", FocusSplitLeft),
        command("Terminal: Focus Split Right", FocusSplitRight),
        command("Terminal: Focus Split Up", FocusSplitUp),
        command("Terminal: Focus Split Down", FocusSplitDown),
        command("View: Agents Overview", ToggleOverview),
        command("Agents: Open Page", ShowAgentsPage),
        command("View: Density Compact", SetDensityCompact),
        command("View: Density Comfortable", SetDensityComfortable),
        command("View: Toggle Density", ToggleDensity),
        command("View: Reset Theme and Icons to Default", ResetTheme),
        command("Editor: Toggle Vim Mode", ToggleVimMode),
        command("Editor: Next Change", hutan_code_view::NextGitHunk),
        command("Editor: Previous Change", hutan_code_view::PreviousGitHunk),
        command(
            "Editor: Show Change at Caret",
            hutan_code_view::ToggleGitPeek,
        ),
        command(
            "Editor: Revert Change at Caret",
            hutan_code_view::RevertGitHunk,
        ),
        command(
            "Editor: Stage Change at Caret",
            hutan_code_view::StageGitHunk,
        ),
        command("Editor: Next Agent Change", hutan_code_view::NextAgentHunk),
        command(
            "Editor: Previous Agent Change",
            hutan_code_view::PreviousAgentHunk,
        ),
        command(
            "Editor: Reject Agent Change at Caret",
            hutan_code_view::RejectAgentHunk,
        ),
        command(
            "Editor: Accept Agent Changes in File",
            hutan_code_view::AcceptAgentChanges,
        ),
        command(
            "Editor: Toggle Follow Agent",
            hutan_code_view::ToggleFollowAgent,
        ),
        command("Editor: Toggle Blame", hutan_code_view::ToggleBlame),
        command("Editor: Toggle Soft Wrap", hutan_code_view::ToggleSoftWrap),
        command("Editor: Go to Definition", hutan_code_view::GoToDefinition),
        command("Editor: Go Back", hutan_code_view::GoBack),
        command("Editor: Show Hover", hutan_code_view::ShowHover),
        command("Editor: Next Problem", hutan_code_view::NextDiagnostic),
        command(
            "Editor: Previous Problem",
            hutan_code_view::PreviousDiagnostic,
        ),
        command("Problems: Show Problems Panel", ShowProblems),
        command("Editor: Review Language Server Trust", ReviewLspTrust),
        command("App: Command Palette", TogglePalette),
        command("App: Settings…", OpenSettings),
        command("App: About Hutan", About),
        command("Hutan: Run Setup Again…", RunSetup),
        command("Window: Minimize", MinimizeWindow),
        command("Window: Zoom", ZoomWindow),
        command("App: Quit Hutan", Quit),
        command("Daemon: Status", daemon::Status),
        command("Daemon: Restart Daemon", daemon::Restart),
        command("Daemon: Stop Daemon", daemon::Stop),
        command("Daemon: Start Daemon", daemon::Start),
        command("Daemon: Open Logs", daemon::OpenLogs),
        command(
            "Daemon: Toggle Keep Daemon Running After Hutan Quits",
            daemon::ToggleKeepRunning,
        ),
        command(
            "Daemon: Toggle Start Daemon at Login",
            daemon::ToggleStartAtLogin,
        ),
        command("Daemon: Open Login Items", daemon::OpenLoginItems),
        command("Daemon: Start with Empty Data…", daemon::StartWithEmptyData),
        command("Daemon: Show the Stuck Daemon", daemon::ShowStuckDaemon),
        command("Daemon: Stop the Stuck Daemon…", daemon::StopStuckDaemon),
        command("Daemon: Copy Start Error Details", daemon::CopyStartError),
        command("Daemon: Copy Update Command", daemon::CopyUpdateCommand),
        command("Daemon: Use This App's Service", daemon::UseOwnDaemon),
    ];
    out.extend([
        command("Workspace: New Workspace…", NewWorkspace),
        command("Workspace: Open Project…", OpenProject),
        command("Workspace: Open Folder…", OpenFolder),
        command("Workspace: Jump to Workspace or Session…", OpenJump),
        command("File: Go to File…", QuickOpen),
        command("Files: Show Files Panel", ShowFiles),
        command(
            "Changes: Track Changes in the Home Folder…",
            TrackHomeChanges,
        ),
        command("Files: Reveal Active File", RevealActiveFile),
        command("Files: Collapse All", FilesCollapseAll),
        command("Files: Refresh", FilesRefresh),
        command("Files: New File", FilesNewFile),
        command("Files: New Folder", FilesNewFolder),
        command("Files: Rename", FilesRename),
        command("Files: Duplicate", FilesDuplicate),
        command("Files: Move to Trash", FilesTrash),
        command("Files: Copy", FilesCopy),
        command("Files: Cut", FilesCut),
        command("Files: Paste", FilesPaste),
        command("Files: Undo", FilesUndo),
        command("Files: Redo", FilesRedo),
        command("Files: Copy Path", FilesCopyPath),
        command("Files: Copy Relative Path", FilesCopyRelativePath),
        command("Files: Reveal in Finder", FilesRevealInFinder),
        command("Files: Open in Terminal", FilesOpenInTerminal),
        command("Files: Attach to Chat", FilesAttachToChat),
        command("Files: Find in Folder", FilesFindInFolder),
        command("Files: Select for Compare", FilesSelectForCompare),
        command("Files: Compare with Selected", FilesCompareWithSelected),
        command("Files: Compare with HEAD", FilesCompareWithHead),
        command("Files: Filter", FilesFilter),
        command("Files: Filter in Folder", FilesFilterInFolder),
        command("Git: Show File History", ShowFileHistory),
        command("Search: Find in Files", ShowSearch),
        command("Search: Replace in Files", ShowSearchReplace),
        command("Instructions: Show Instruction Files", ShowInstructions),
        command(
            "Instructions: Share One File with Every Agent…",
            ShareInstructions,
        ),
        command("Instructions: Create AGENTS.md", CreateAgentsMd),
        command("Workspace: Go to Home", GoHome),
        command("Workspace: Next Container", NextContainer),
        command("Workspace: Previous Container", PreviousContainer),
        command("File: Open File…", OpenFileDialog),
        command("File: Open in Split Right", OpenFileInSplitRight),
        command("File: Open in Split Down", OpenFileInSplitDown),
        command(
            "Workspace: Pin / Unpin Active Workspace",
            PinActiveWorkspace,
        ),
        command("Workspace: Rename Active Workspace…", RenameActiveWorkspace),
        command(
            "Workspace: Add Folder to Active Workspace…",
            AddFolderToWorkspace,
        ),
        command(
            "Workspace: Delete or Close Active Workspace",
            CloseActiveWorkspace,
        ),
        command(
            "Workspace: Reveal Active Workspace in Finder",
            RevealWorkspace,
        ),
    ]);
    out.extend(WorkspaceSort::ALL.map(|mode| {
        command(
            format!("Workspace: Sort — {}", mode.label()),
            SetWorkspaceSort(mode),
        )
    }));
    out.extend([
        command(
            "Sidebar: Toggle Hide Empty Projects",
            ToggleHideEmptyWorkspaces,
        ),
        command(
            "Sidebar: Toggle Hide Sleeping Projects",
            ToggleHideSleepingWorkspaces,
        ),
        command(
            "Sidebar: Toggle Hide CLI-Created Projects",
            ToggleHideCliWorkspaces,
        ),
    ]);
    out.extend([
        command(
            "Sidebar: Organize by Workspaces",
            SetSidebarOrganize(SidebarOrganize::Workspaces),
        ),
        command(
            "Sidebar: Organize by Projects",
            SetSidebarOrganize(SidebarOrganize::Projects),
        ),
    ]);
    out.push(command("History: Search History…", SearchHistory));
    out.extend([
        command("Agent: Review Approvals…", ReviewApprovals),
        command("Agent: Send Message", chat::SendMessage),
        command("Agent: Interrupt Turn", chat::InterruptTurn),
        command("Agent: Send Now", chat::SendNow),
        command("Agents: Show Monitor", ShowAgentMonitor),
        command("Agents: Show Only Blocked", ShowOnlyBlockedAgents),
        command("Resources: Show Panel", ShowResources),
        command("Resources: Toggle Panel", ToggleResources),
        command("Models: Manage Providers…", ManageProviders),
        command("Accounts…", ManageAccounts),
        command("Chat: Switch Model…", SwitchChatModel),
        command("Chat: Toggle Send Key", ToggleSendKey),
        command("Chat: Rewind Files to Before This Turn…", RewindFocusedChat),
        command("Chat: Undo Last Restore", UndoLastRestore),
    ]);
    if assistant {
        out.push(command("Mawas: Open", OpenAssistant));
        out.push(command("Mawas: New chat", NewMawasChat));
        out.push(command("Mawas: Memory", OpenMawasMemory));
    }
    out.extend([
        command("Worktree: New Terminal in Worktree…", NewTerminalInWorktree),
        command("Worktree: Remove…", RemoveWorktree),
        command("Worktree: Clean Up…", CleanUpWorktrees),
        command("Worktree: Review Changes…", ReviewChanges),
    ]);
    out.extend([
        command("Tasks: New Task…", NewTask),
        command("Tasks: Show Panel", ShowTasksPanel),
    ]);
    out.extend([
        command(
            "Block: Previous Prompt",
            hutan_terminal_view::JumpToPreviousPrompt,
        ),
        command("Block: Next Prompt", hutan_terminal_view::JumpToNextPrompt),
        command(
            "Block: Select Output",
            hutan_terminal_view::SelectBlockOutput,
        ),
        command("Block: Copy Output", hutan_terminal_view::CopyBlockOutput),
        command("Block: Copy Command", hutan_terminal_view::CopyBlockCommand),
        command(
            "Block: Collapse / Expand",
            hutan_terminal_view::ToggleBlockCollapse,
        ),
        command("Block: Rerun", hutan_terminal_view::RerunBlock),
        command(
            "Block: Send to Agent",
            hutan_terminal_view::SendBlockToAgent,
        ),
    ]);
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(Command {
            title: format!("Focus: Neighbor {}", dir_name(dir)).into(),
            action: focus_neighbor_action(dir),
            hint: focus_neighbor_action(dir),
        });
    }
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(Command {
            title: format!("Layout: Resize {}", dir_name(dir)).into(),
            action: resize_action(dir),
            hint: resize_action(dir),
        });
    }
    for edge in [Edge::Right, Edge::Bottom, Edge::Left, Edge::Top] {
        out.push(command(
            format!("Layout: Move Tab to New Split {}", edge_name(edge)),
            MoveActiveTabToSplit(edge),
        ));
    }
    for pos in DockPosition::ALL {
        out.push(command(
            format!("Panel: Move to {} Dock", pos.label()),
            MoveActivePanel(PanelDest::Dock(pos)),
        ));
    }
    for (i, preset) in presets.iter().enumerate() {
        let apply = ApplyPreset(preset.name.clone());
        let title = format!("Layout: Apply Preset {}", preset.name);
        match preset_number_action(i + 1) {
            // ⌘⇧1…9 switch presets by position.
            Some(number) => out.push(Command {
                title: title.into(),
                action: Box::new(apply),
                hint: number,
            }),
            None => out.push(command(title, apply)),
        }
    }
    for spec in panels
        .iter()
        .filter(|s| assistant || !is_mawas_page(s.id.as_str()))
        // The Agents page has its own commands.
        .filter(|s| s.id.as_str() != super::model::AGENTS_PAGE)
    {
        out.push(command(
            format!("Panel: Open {}", spec.title),
            OpenPanel(spec.id.to_string()),
        ));
    }
    out
}

/// The pages of Mawas (Home, Memory): they go away with `assistant.enabled`.
pub fn is_mawas_page(panel: &str) -> bool {
    matches!(panel, "assistant" | "mawas-memory")
}

fn bind<A: Action>(keys: &str, context: Option<&str>) -> BindingSpec {
    match BindingSpec::new(keys, A::name_for_type(), context) {
        Ok(spec) => spec,
        // The defaults are literals; a typo is a bug caught by `default_bindings_parse`.
        Err(err) => {
            tracing::error!("invalid default key `{keys}`: {err}");
            BindingSpec {
                keys: Vec::new(),
                action: A::name_for_type().to_owned(),
                context: context.map(str::to_owned),
            }
        }
    }
}

/// The key context of everything that is not a terminal: where ⌘K opens the palette in the Zed
/// base.
const OUTSIDE_TERMINAL: &str = "!HutanTerminal";

/// The key bindings of `base`; `keymap.toml` is merged on top. Where an action has several keys
/// the preferred one (shown in palette rows and tooltips) comes last: the keymap gives later
/// bindings precedence.
pub fn default_bindings(base: KeymapBase) -> Vec<BindingSpec> {
    let mut out = match base {
        KeymapBase::Zed => zed_global_bindings(),
        KeymapBase::Hutan => hutan_global_bindings(),
    };
    out.extend(context_bindings());
    out.extend(numbered_bindings());
    out
}

/// Hutan's original global table (`keymap.base = "hutan"`).
fn hutan_global_bindings() -> Vec<BindingSpec> {
    vec![
        bind::<TogglePalette>("cmd-k", None),
        bind::<OpenSettings>("cmd-,", None),
        bind::<NewTerminal>("cmd-t", None),
        bind::<NewAgent>("cmd-shift-t", None),
        bind::<NewChat>("cmd-alt-t", None),
        bind::<ActivatePreviousTab>("cmd-{", None),
        bind::<ActivateNextTab>("cmd-}", None),
        bind::<ActivatePreviousTab>("cmd-shift-[", None),
        bind::<ActivateNextTab>("cmd-shift-]", None),
        bind::<ActivatePreviousTab>("ctrl-shift-tab", None),
        bind::<ActivateNextTab>("ctrl-tab", None),
        bind::<JumpToNextAttention>("cmd-shift-j", None),
        bind::<ReviewRequest>("cmd-shift-r", None),
        bind::<SplitRight>("cmd-d", None),
        bind::<SplitDown>("cmd-shift-d", None),
        bind::<ClosePaneItem>("cmd-w", None),
        bind::<StopSession>("cmd-shift-w", None),
        bind::<ToggleZoom>("cmd-shift-enter", None),
        bind::<ToggleLeftDock>("cmd-b", None),
        bind::<ToggleRightDock>("cmd-alt-b", None),
        bind::<ToggleRightDock>("cmd-l", None),
        bind::<ToggleBottomDock>("cmd-alt-j", None),
        bind::<OpenJump>("cmd-j", None),
        bind::<QuickOpen>("cmd-p", None),
        bind::<ShowFiles>("cmd-shift-e", None),
        bind::<ShowProblems>("cmd-shift-m", None),
        bind::<ShowSearch>("cmd-shift-f", None),
        bind::<ShowSearchReplace>("cmd-shift-h", None),
        bind::<OpenProject>("cmd-o", None),
        bind::<ToggleOverview>("cmd-shift-o", None),
        bind::<ToggleResources>("cmd-alt-r", None),
        bind::<MinimizeWindow>("cmd-m", None),
        bind::<Quit>("cmd-q", None),
        bind::<FocusNeighborLeft>("cmd-alt-left", None),
        bind::<FocusNeighborRight>("cmd-alt-right", None),
        bind::<FocusNeighborUp>("cmd-alt-up", None),
        bind::<FocusNeighborDown>("cmd-alt-down", None),
        bind::<ResizeLeft>("ctrl-cmd-left", None),
        bind::<ResizeRight>("ctrl-cmd-right", None),
        bind::<ResizeUp>("ctrl-cmd-up", None),
        bind::<ResizeDown>("ctrl-cmd-down", None),
        bind::<NextContainer>("ctrl-alt-down", None),
        bind::<PreviousContainer>("ctrl-alt-up", None),
    ]
}

/// The key context of the Files tree (`panels/files.rs`).
pub const FILES_KEY_CONTEXT: &str = "FilesPanel";

/// Bindings inside one key context (terminal blocks, palette, dialogs, chat, setup); the same
/// in both bases.
fn context_bindings() -> Vec<BindingSpec> {
    // The editor's own keys that Vim's Normal and Visual modes answer themselves.
    const TYPING: &str = hutan_code_view::TYPING_CONTEXT;
    const VIM_INSERT: &str = "CodeView && vim_mode == insert && !showing_completions";
    const COMPLETING: &str = "CodeView && showing_completions";
    const SNIPPET: &str = "CodeView && in_snippet && !showing_completions";
    vec![
        // The Files tree (EX-7): only while the keyboard is in the tree itself (its context is
        // the innermost one), never in its inline name field or an editor tab.
        bind::<FilesNewFile>("cmd-n", Some(FILES_KEY_CONTEXT)),
        bind::<FilesNewFolder>("cmd-alt-n", Some(FILES_KEY_CONTEXT)),
        bind::<FilesRename>("f2", Some(FILES_KEY_CONTEXT)),
        bind::<FilesDuplicate>("cmd-d", Some(FILES_KEY_CONTEXT)),
        bind::<FilesTrash>("cmd-backspace", Some(FILES_KEY_CONTEXT)),
        bind::<FilesCopy>("cmd-c", Some(FILES_KEY_CONTEXT)),
        bind::<FilesCut>("cmd-x", Some(FILES_KEY_CONTEXT)),
        bind::<FilesPaste>("cmd-v", Some(FILES_KEY_CONTEXT)),
        bind::<FilesUndo>("cmd-z", Some(FILES_KEY_CONTEXT)),
        bind::<FilesRedo>("cmd-shift-z", Some(FILES_KEY_CONTEXT)),
        bind::<FilesCopyPath>("cmd-alt-c", Some(FILES_KEY_CONTEXT)),
        bind::<FilesCopyRelativePath>("cmd-alt-shift-c", Some(FILES_KEY_CONTEXT)),
        bind::<FilesFilter>("cmd-f", Some(FILES_KEY_CONTEXT)),
        bind::<hutan_terminal_view::Copy>("cmd-c", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<hutan_terminal_view::Paste>("cmd-v", Some(hutan_terminal_view::KEY_CONTEXT)),
        // ⌘↑ / ⌘↓ scroll to the top / bottom of the scrollback, as in Terminal.app and iTerm; the
        // prompt jumps of the blocks are ⌘⇧↑ / ⌘⇧↓.
        bind::<hutan_terminal_view::ScrollToTop>("cmd-up", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<hutan_terminal_view::ScrollToBottom>(
            "cmd-down",
            Some(hutan_terminal_view::KEY_CONTEXT),
        ),
        bind::<hutan_terminal_view::JumpToPreviousPrompt>(
            "cmd-shift-up",
            Some(hutan_terminal_view::KEY_CONTEXT),
        ),
        bind::<hutan_terminal_view::JumpToNextPrompt>(
            "cmd-shift-down",
            Some(hutan_terminal_view::KEY_CONTEXT),
        ),
        bind::<hutan_terminal_view::SelectBlockOutput>(
            "cmd-shift-a",
            Some(hutan_terminal_view::KEY_CONTEXT),
        ),
        bind::<hutan_terminal_view::CopyBlockOutput>(
            "cmd-shift-c",
            Some(hutan_terminal_view::KEY_CONTEXT),
        ),
        // Terminal splits (TERM-SPLITS): with the keyboard in a terminal these keys act on the
        // splits of its tab; they fall back to what the key does everywhere else (a pane's
        // split, neighbour, resize, zoom) where the tab has none.
        bind::<SplitTerminalRight>("cmd-d", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<SplitTerminalDown>("cmd-shift-d", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FocusSplitLeft>("cmd-alt-left", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FocusSplitRight>("cmd-alt-right", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FocusSplitUp>("cmd-alt-up", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FocusSplitDown>("cmd-alt-down", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<ResizeSplitLeft>("ctrl-cmd-left", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<ResizeSplitRight>("ctrl-cmd-right", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<ResizeSplitUp>("ctrl-cmd-up", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<ResizeSplitDown>("ctrl-cmd-down", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<ToggleSplitZoom>("cmd-shift-enter", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<CloseTerminalTab>("cmd-shift-w", Some(hutan_terminal_view::KEY_CONTEXT)),
        // Find in the terminal's output (TERM-FIND); the bar's own field has the keys too.
        bind::<FindInTerminal>("cmd-f", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FindNextInTerminal>("cmd-g", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FindPreviousInTerminal>("cmd-shift-g", Some(hutan_terminal_view::KEY_CONTEXT)),
        bind::<FindInTerminal>("cmd-f", Some(super::terminal_find::FIND_CONTEXT)),
        bind::<FindNextInTerminal>("cmd-g", Some(super::terminal_find::FIND_CONTEXT)),
        bind::<FindPreviousInTerminal>("cmd-shift-g", Some(super::terminal_find::FIND_CONTEXT)),
        bind::<palette::SelectNext>("down", Some("Palette")),
        bind::<palette::SelectNext>("ctrl-n", Some("Palette")),
        bind::<palette::SelectPrev>("up", Some("Palette")),
        bind::<palette::SelectPrev>("ctrl-p", Some("Palette")),
        bind::<palette::Confirm>("enter", Some("Palette")),
        bind::<palette::Dismiss>("escape", Some("Palette")),
        bind::<palette::Dismiss>("escape", Some("ContextMenu")),
        bind::<modal::Confirm>("enter", Some("Modal")),
        bind::<modal::Cancel>("escape", Some("Modal")),
        bind::<sheet::Dismiss>("escape", Some("SettingsSheet")),
        bind::<palette::Dismiss>("escape", Some("DaemonPopover")),
        // The quick terminal's panel (`quick_terminal.rs`): ⎋ hides it unless the program in the
        // terminal has the alternate screen; ⌘⇧↩ (a split zoom in the terminal) moves the tab
        // into the main window. ⌘T, ⌘W and the tab keys are the main window's own.
        bind::<super::quick_terminal::HideOnEscape>(
            "escape",
            Some(super::quick_terminal::KEY_CONTEXT),
        ),
        bind::<super::quick_terminal::MoveToMain>(
            "cmd-shift-enter",
            Some(super::quick_terminal::KEY_CONTEXT),
        ),
        bind::<chat::SendMessage>("cmd-enter", Some(super::chat::KEY_CONTEXT)),
        bind::<chat::InterruptTurn>("cmd-.", Some(super::chat::KEY_CONTEXT)),
        // Send now (UX-QUEUE-STEER): the send key queues while a turn runs; ⌃↩ does not wait.
        bind::<chat::SendNow>("ctrl-enter", Some(super::chat::KEY_CONTEXT)),
        bind::<palette::SelectNext>("down", Some("AgentPicker")),
        bind::<palette::SelectNext>("ctrl-n", Some("AgentPicker")),
        bind::<palette::SelectPrev>("up", Some("AgentPicker")),
        bind::<palette::SelectPrev>("ctrl-p", Some("AgentPicker")),
        bind::<palette::Confirm>("enter", Some("AgentPicker")),
        bind::<palette::Dismiss>("escape", Some("AgentPicker")),
        bind::<palette::SelectNext>("down", Some("Picker")),
        bind::<palette::SelectNext>("ctrl-n", Some("Picker")),
        bind::<palette::SelectPrev>("up", Some("Picker")),
        bind::<palette::SelectPrev>("ctrl-p", Some("Picker")),
        bind::<palette::Confirm>("enter", Some("Picker")),
        bind::<palette::ConfirmInSplit>("cmd-enter", Some("Picker")),
        bind::<palette::Dismiss>("escape", Some("Picker")),
        bind::<setup::Continue>("enter", Some("Setup")),
        bind::<setup::Skip>("escape", Some("Setup")),
        bind::<CancelDockResize>("escape", Some(super::docks::RESIZE_CONTEXT)),
        bind::<ClosePeek>("escape", Some(super::docks::PEEK_CONTEXT)),
        bind::<DismissAttentionToasts>("escape", Some(super::attention_toast::TOAST_CONTEXT)),
        bind::<UndoLayoutChange>("cmd-z", Some(super::customize::LAYOUT_TOAST_CONTEXT)),
        bind::<hutan_code_view::MoveLeft>("left", Some(TYPING)),
        bind::<hutan_code_view::MoveRight>("right", Some(TYPING)),
        bind::<hutan_code_view::MoveUp>("up", Some(TYPING)),
        bind::<hutan_code_view::MoveDown>("down", Some(TYPING)),
        bind::<hutan_code_view::MoveWordLeft>("alt-left", Some(TYPING)),
        bind::<hutan_code_view::MoveWordRight>("alt-right", Some(TYPING)),
        bind::<hutan_code_view::MoveLineStart>("home", Some(TYPING)),
        // ⌃A / ⌃E are Vim's in all its modes (Insert: last insert, char below; FX-VIM-2).
        bind::<hutan_code_view::MoveLineStart>("ctrl-a", Some(hutan_code_view::PLAIN_CONTEXT)),
        bind::<hutan_code_view::MoveLineStart>("cmd-left", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::MoveLineEnd>("end", Some(TYPING)),
        bind::<hutan_code_view::MoveLineEnd>("ctrl-e", Some(hutan_code_view::PLAIN_CONTEXT)),
        bind::<hutan_code_view::MoveLineEnd>("cmd-right", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::MoveToStart>("cmd-up", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::MoveToEnd>("cmd-down", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::PageUp>("pageup", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::PageDown>("pagedown", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::SelectLeft>("shift-left", Some(TYPING)),
        bind::<hutan_code_view::SelectRight>("shift-right", Some(TYPING)),
        bind::<hutan_code_view::SelectUp>("shift-up", Some(TYPING)),
        bind::<hutan_code_view::SelectDown>("shift-down", Some(TYPING)),
        bind::<hutan_code_view::SelectWordLeft>("alt-shift-left", Some(TYPING)),
        bind::<hutan_code_view::SelectWordRight>("alt-shift-right", Some(TYPING)),
        bind::<hutan_code_view::SelectLineStart>("shift-home", Some(TYPING)),
        bind::<hutan_code_view::SelectLineStart>(
            "cmd-shift-left",
            Some(hutan_code_view::KEY_CONTEXT),
        ),
        bind::<hutan_code_view::SelectLineEnd>("shift-end", Some(TYPING)),
        bind::<hutan_code_view::SelectLineEnd>(
            "cmd-shift-right",
            Some(hutan_code_view::KEY_CONTEXT),
        ),
        bind::<hutan_code_view::SelectToStart>("cmd-shift-up", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::SelectToEnd>("cmd-shift-down", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::SelectPageUp>("shift-pageup", Some(TYPING)),
        bind::<hutan_code_view::SelectPageDown>("shift-pagedown", Some(TYPING)),
        bind::<hutan_code_view::SelectAll>("cmd-a", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::SelectNextOccurrence>("cmd-d", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::SelectAllOccurrences>(
            "cmd-shift-l",
            Some(hutan_code_view::KEY_CONTEXT),
        ),
        bind::<hutan_code_view::Cancel>("escape", Some(hutan_code_view::PLAIN_CONTEXT)),
        bind::<hutan_code_view::Newline>("shift-enter", Some(TYPING)),
        bind::<hutan_code_view::Newline>("enter", Some(TYPING)),
        bind::<hutan_code_view::Tab>("tab", Some(TYPING)),
        bind::<hutan_code_view::Outdent>("shift-tab", Some(TYPING)),
        bind::<hutan_code_view::Indent>("cmd-]", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Outdent>("cmd-[", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::DeleteBackward>("shift-backspace", Some(TYPING)),
        bind::<hutan_code_view::DeleteBackward>("backspace", Some(TYPING)),
        bind::<hutan_code_view::DeleteForward>("delete", Some(TYPING)),
        bind::<hutan_code_view::DeleteWordBackward>("alt-backspace", Some(TYPING)),
        bind::<hutan_code_view::DeleteWordForward>("alt-delete", Some(TYPING)),
        bind::<hutan_code_view::DeleteToLineStart>(
            "cmd-backspace",
            Some(hutan_code_view::KEY_CONTEXT),
        ),
        bind::<hutan_code_view::DeleteToLineEnd>("ctrl-k", Some(TYPING)),
        bind::<hutan_code_view::DeleteToLineEnd>("cmd-delete", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::DeleteLine>("cmd-shift-k", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::DuplicateLineUp>("alt-shift-up", Some(TYPING)),
        bind::<hutan_code_view::DuplicateLineDown>("alt-shift-down", Some(TYPING)),
        bind::<hutan_code_view::MoveLineUp>("alt-up", Some(TYPING)),
        bind::<hutan_code_view::MoveLineDown>("alt-down", Some(TYPING)),
        bind::<hutan_code_view::ToggleComment>("cmd-/", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Cut>("cmd-x", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Copy>("cmd-c", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Paste>("cmd-v", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Undo>("cmd-z", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Redo>("cmd-shift-z", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Save>("cmd-s", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::Find>("cmd-f", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::FindReplace>("cmd-alt-f", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::FindNext>("cmd-g", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::FindPrevious>("cmd-shift-g", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::RejectAgentHunk>("cmd-alt-z", Some(super::file_view::KEY_CONTEXT)),
        // View → Toggle Soft Wrap: VS Code's key.
        bind::<hutan_code_view::ToggleSoftWrap>("alt-z", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::NextAgentHunk>("cmd-alt-]", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::PreviousAgentHunk>(
            "cmd-alt-[",
            Some(super::file_view::KEY_CONTEXT),
        ),
        // The git gutter's changes (VS Code's keys; Vim has `]c` `[c`).
        bind::<hutan_code_view::NextGitHunk>("alt-f5", Some(super::file_view::KEY_CONTEXT)),
        bind::<hutan_code_view::PreviousGitHunk>(
            "shift-alt-f5",
            Some(super::file_view::KEY_CONTEXT),
        ),
        // Language servers (E-12a); Vim's Normal mode has `gd`, `K`, `]d` `[d` and `⌃O` itself.
        // Hover is ⌘⌥I in both bases (Zed's chord ⌘K ⌘I is gone: ⌘K opens the palette).
        bind::<hutan_code_view::ShowHover>("cmd-alt-i", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::GoToDefinition>("f12", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::NextDiagnostic>("f8", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::PreviousDiagnostic>("shift-f8", Some(hutan_code_view::KEY_CONTEXT)),
        bind::<hutan_code_view::GoBack>("ctrl-o", Some(hutan_code_view::PLAIN_CONTEXT)),
        // Completion (E-12b): ⌃Space asks; in Vim's Insert mode ⌃N / ⌃P ask too. While the
        // popup shows its keys come last so that they win over the editor's ↑ ↓ Return Tab.
        bind::<hutan_code_view::ShowCompletions>("ctrl-space", Some(TYPING)),
        bind::<hutan_code_view::ShowCompletions>("ctrl-n", Some(VIM_INSERT)),
        bind::<hutan_code_view::ShowCompletions>("ctrl-p", Some(VIM_INSERT)),
        bind::<hutan_code_view::NextTabStop>("tab", Some(SNIPPET)),
        bind::<hutan_code_view::PreviousTabStop>("shift-tab", Some(SNIPPET)),
        bind::<hutan_code_view::CompletionNext>("down", Some(COMPLETING)),
        bind::<hutan_code_view::CompletionNext>("ctrl-n", Some(COMPLETING)),
        bind::<hutan_code_view::CompletionPrevious>("up", Some(COMPLETING)),
        bind::<hutan_code_view::CompletionPrevious>("ctrl-p", Some(COMPLETING)),
        bind::<hutan_code_view::ConfirmCompletion>("enter", Some(COMPLETING)),
        bind::<hutan_code_view::ConfirmCompletion>("tab", Some(COMPLETING)),
    ]
}

/// ⌘1…⌘9 select the tab, ⌘⇧1…⌘⇧9 the layout preset.
fn numbered_bindings() -> Vec<BindingSpec> {
    let mut out = Vec::new();
    let tabs: [&str; 9] = [
        ActivateTab1::name_for_type(),
        ActivateTab2::name_for_type(),
        ActivateTab3::name_for_type(),
        ActivateTab4::name_for_type(),
        ActivateTab5::name_for_type(),
        ActivateTab6::name_for_type(),
        ActivateTab7::name_for_type(),
        ActivateTab8::name_for_type(),
        ActivateTab9::name_for_type(),
    ];
    let presets: [&str; 9] = [
        ApplyPresetNumber1::name_for_type(),
        ApplyPresetNumber2::name_for_type(),
        ApplyPresetNumber3::name_for_type(),
        ApplyPresetNumber4::name_for_type(),
        ApplyPresetNumber5::name_for_type(),
        ApplyPresetNumber6::name_for_type(),
        ApplyPresetNumber7::name_for_type(),
        ApplyPresetNumber8::name_for_type(),
        ApplyPresetNumber9::name_for_type(),
    ];
    for n in 1..=9usize {
        for (keys, action) in [
            (format!("cmd-{n}"), tabs[n - 1]),
            (format!("cmd-shift-{n}"), presets[n - 1]),
        ] {
            match BindingSpec::new(&keys, action, None) {
                Ok(spec) => out.push(spec),
                Err(err) => tracing::error!("invalid default key `{keys}`: {err}"),
            }
        }
    }
    out
}

/// Zed's macOS keys where Hutan has the matching action (`keymap.base = "zed"`, the default),
/// read from Zed's published `default-macos.json`; Hutan-only actions keep keys Zed leaves
/// free:
///
/// - ⌘⇧P palette (and ⌘K, outside terminals: Zed's ⌘K chords are not bound, so the key is not a
///   prefix that waits for a second stroke), ⌘P quick open
///   (files, then the jump list's workspaces and sessions), ⌘⇧E the Files panel, ⌘⇧F / ⌘⇧H
///   find / replace in files (Zed's project search), ⌘O open, ⌘, settings, ⌘⇧N new workspace,
///   ⌘Y search history (Safari's history key: ⌘⇧F is project search here, as in Zed).
/// - ⌘B left sidebar; ⌘⌥B / ⌘R / ⌘L right sidebar (⌘L is Orca's key and the one the tooltips
///   show; Zed's ⌘L is "go to line", which Hutan has no use for); ⌘J / ⌃\` bottom dock; ⌘⌥Y
///   close all docks.
/// - ⌘\\ splits right; ⌘⌥←→↑↓ focus the neighbor pane; ⌘D / ⌘⇧D stay (they are Zed's split keys
///   in the terminal). Zed's ⌘K ←→↑↓ (split) and ⌘K ⌘←→↑↓ (focus) chords and its ⌘K ⌘I hover
///   chord are gone (CL-POLISH-4): ⌘K opens the palette, which lists *Layout: Split Left / Up*
///   and the rest; hover is ⌘⌥I.
/// - ⇧⎋ zoom pane, ⌘= / ⌘+ / ⌘- / ⌘0 terminal font size.
/// - Kept from Hutan: ⌘T / ⌘⇧T / ⌘⌥T new session, ⌘⇧W stop, ⌘⇧J next attention, ⌘⇧O
///   overview, ⌘⌥R resources, ⌘⌥← … focus a neighbor, ⌘1…⌘9 tabs.
fn zed_global_bindings() -> Vec<BindingSpec> {
    vec![
        // ⌘K opens the palette too, except with the keyboard in a terminal (the preferred key,
        // shown in rows and tooltips, is ⌘⇧P: the last binding).
        bind::<TogglePalette>("cmd-k", Some(OUTSIDE_TERMINAL)),
        bind::<TogglePalette>("cmd-shift-p", None),
        bind::<QuickOpen>("cmd-p", None),
        bind::<ShowFiles>("cmd-shift-e", None),
        bind::<ShowProblems>("cmd-shift-m", None),
        bind::<ShowSearch>("cmd-shift-f", None),
        bind::<ShowSearchReplace>("cmd-shift-h", None),
        bind::<OpenProject>("cmd-o", None),
        bind::<OpenSettings>("cmd-,", None),
        bind::<NewWorkspace>("cmd-shift-n", None),
        bind::<SearchHistory>("cmd-y", None),
        bind::<NewTerminal>("cmd-t", None),
        bind::<NewAgent>("cmd-shift-t", None),
        bind::<NewChat>("cmd-alt-t", None),
        bind::<ActivatePreviousTab>("cmd-{", None),
        bind::<ActivateNextTab>("cmd-}", None),
        bind::<ActivatePreviousTab>("cmd-shift-[", None),
        bind::<ActivateNextTab>("cmd-shift-]", None),
        bind::<ActivatePreviousTab>("ctrl-shift-tab", None),
        bind::<ActivateNextTab>("ctrl-tab", None),
        bind::<JumpToNextAttention>("cmd-shift-j", None),
        bind::<ReviewRequest>("cmd-shift-r", None),
        bind::<SplitRight>("cmd-\\", None),
        bind::<SplitRight>("cmd-d", None),
        bind::<SplitDown>("cmd-shift-d", None),
        bind::<ClosePaneItem>("cmd-w", None),
        bind::<StopSession>("cmd-shift-w", None),
        bind::<ToggleZoom>("cmd-shift-enter", None),
        bind::<ToggleZoom>("shift-escape", None),
        bind::<ToggleLeftDock>("cmd-b", None),
        bind::<ToggleRightDock>("cmd-alt-b", None),
        bind::<ToggleRightDock>("cmd-r", None),
        bind::<ToggleRightDock>("cmd-l", None),
        bind::<ToggleBottomDock>("ctrl-`", None),
        bind::<ToggleBottomDock>("cmd-j", None),
        bind::<CloseAllDocks>("cmd-alt-y", None),
        bind::<ToggleOverview>("cmd-shift-o", None),
        bind::<ToggleResources>("cmd-alt-r", None),
        bind::<IncreaseFontSize>("cmd-=", None),
        bind::<IncreaseFontSize>("cmd-+", None),
        bind::<DecreaseFontSize>("cmd--", None),
        bind::<ResetFontSize>("cmd-0", None),
        bind::<MinimizeWindow>("cmd-m", None),
        bind::<Quit>("cmd-q", None),
        bind::<FocusNeighborLeft>("cmd-alt-left", None),
        bind::<FocusNeighborRight>("cmd-alt-right", None),
        bind::<FocusNeighborUp>("cmd-alt-up", None),
        bind::<FocusNeighborDown>("cmd-alt-down", None),
        bind::<ResizeLeft>("ctrl-cmd-left", None),
        bind::<ResizeRight>("ctrl-cmd-right", None),
        bind::<ResizeUp>("ctrl-cmd-up", None),
        bind::<ResizeDown>("ctrl-cmd-down", None),
        bind::<NextContainer>("ctrl-alt-down", None),
        bind::<PreviousContainer>("ctrl-alt-up", None),
    ]
}

/// Replace every key binding of the app with `load.bindings`. Bindings naming an unknown action
/// or an unparsable context are skipped with a warning.
pub fn apply_keymap(cx: &mut App, load: &KeymapLoad) {
    let mut bindings = Vec::with_capacity(load.bindings.len());
    for spec in &load.bindings {
        if spec.keys.is_empty() {
            continue;
        }
        let action = match cx.build_action(&spec.action, None) {
            Ok(action) => action,
            Err(err) => {
                tracing::warn!("keymap: action `{}` cannot be bound: {err}", spec.action);
                continue;
            }
        };
        let predicate = match spec
            .context
            .as_deref()
            .map(KeyBindingContextPredicate::parse)
        {
            None => None,
            Some(Ok(predicate)) => Some(predicate.into()),
            Some(Err(err)) => {
                tracing::warn!(
                    "keymap: context `{}` of `{}` is invalid: {err}",
                    spec.context.as_deref().unwrap_or_default(),
                    spec.action
                );
                continue;
            }
        };
        match KeyBinding::load(
            &format_keys(&spec.keys),
            action,
            predicate,
            false,
            None,
            cx.keyboard_mapper().as_ref(),
        ) {
            Ok(binding) => bindings.push(binding),
            Err(err) => tracing::warn!("keymap: `{}`: {err}", format_keys(&spec.keys)),
        }
    }
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn both() -> [(KeymapBase, Vec<BindingSpec>); 2] {
        KeymapBase::ALL.map(|base| (base, default_bindings(base)))
    }

    /// `keys | action | context` of every binding, in table order.
    fn rendered(bindings: &[BindingSpec]) -> Vec<String> {
        bindings
            .iter()
            .map(|b| {
                format!(
                    "{} | {} | {}",
                    format_keys(&b.keys),
                    b.action,
                    b.context.as_deref().unwrap_or("-")
                )
            })
            .collect()
    }

    fn bound(bindings: &[BindingSpec], action: &str) -> Vec<String> {
        bindings
            .iter()
            .filter(|b| b.action == action && b.context.is_none())
            .map(|b| format_keys(&b.keys))
            .collect()
    }

    #[test]
    fn both_bases_parse_and_have_no_key_bound_twice_in_a_context() {
        for (base, defaults) in both() {
            assert!(
                defaults.iter().all(|b| !b.keys.is_empty()),
                "{base:?}: a default key failed to parse"
            );
            let mut seen: BTreeMap<(String, Option<String>), &str> = BTreeMap::new();
            for binding in &defaults {
                let key = (format_keys(&binding.keys), binding.context.clone());
                if let Some(first) = seen.insert(key.clone(), &binding.action) {
                    panic!(
                        "{base:?}: `{}` in {:?} is bound to both {first} and {}",
                        key.0, key.1, binding.action
                    );
                }
            }
            let load = hutan_settings::keymap::load_str(&defaults, "", None).unwrap();
            assert!(
                load.diagnostics.is_empty(),
                "{base:?}: {:?}",
                load.diagnostics
            );
            assert_eq!(load.bindings.len(), defaults.len(), "{base:?}");
        }
    }

    #[test]
    fn zed_base_binds_the_zed_keys() {
        let zed = default_bindings(KeymapBase::Zed);
        let keys = |action: &str| bound(&zed, action);
        let last = |action: &str| keys(action).pop().unwrap_or_default();
        assert_eq!(last(TogglePalette::name_for_type()), "shift-cmd-p");
        // ⌘K opens the palette outside terminals (CL-POLISH-4); ⌘⇧P stays the key it shows.
        assert_eq!(keys(TogglePalette::name_for_type()), ["shift-cmd-p"]);
        // ⌘P is quick open: files first, then the jump list (E-13a, ADR 0012 amendment 8).
        assert_eq!(last(QuickOpen::name_for_type()), "cmd-p");
        assert!(keys(OpenJump::name_for_type()).is_empty());
        assert_eq!(last(ShowFiles::name_for_type()), "shift-cmd-e");
        assert_eq!(last(ToggleLeftDock::name_for_type()), "cmd-b");
        assert_eq!(last(ToggleRightDock::name_for_type()), "cmd-l");
        assert!(keys(ToggleRightDock::name_for_type()).contains(&"cmd-r".to_owned()));
        assert_eq!(last(ToggleBottomDock::name_for_type()), "cmd-j");
        assert_eq!(last(CloseAllDocks::name_for_type()), "alt-cmd-y");
        assert!(keys(ToggleBottomDock::name_for_type()).contains(&"ctrl-`".to_owned()));
        // No ⌘K chord is left: ⌘K is the palette and does not wait for a second stroke.
        assert!(
            zed.iter()
                .all(|b| format_keys(&b.keys) != "cmd-k"
                    || b.action == TogglePalette::name_for_type()),
            "only the palette has ⌘K"
        );
        assert!(zed.iter().any(|b| format_keys(&b.keys) == "cmd-k"));
        assert!(
            zed.iter()
                .all(|b| !format_keys(&b.keys).starts_with("cmd-k ")),
            "no chord starts with ⌘K"
        );
        // What the chords did is in the palette and on keys of its own: ⌘⌥ arrows focus the
        // neighbor, ⌘\\ ⌘D ⌘⇧D split (Left and Up are palette commands).
        for (action, key) in [
            (FocusNeighborLeft::name_for_type(), "alt-cmd-left"),
            (FocusNeighborRight::name_for_type(), "alt-cmd-right"),
            (FocusNeighborUp::name_for_type(), "alt-cmd-up"),
            (FocusNeighborDown::name_for_type(), "alt-cmd-down"),
            (SplitRight::name_for_type(), "cmd-d"),
            (SplitDown::name_for_type(), "shift-cmd-d"),
        ] {
            assert!(keys(action).contains(&key.to_owned()), "{action}");
        }
        assert!(keys(SplitLeft::name_for_type()).is_empty());
        assert!(keys(SplitUp::name_for_type()).is_empty());
        assert_eq!(last(IncreaseFontSize::name_for_type()), "cmd-+");
        assert_eq!(last(DecreaseFontSize::name_for_type()), "cmd--");
        assert_eq!(last(ResetFontSize::name_for_type()), "cmd-0");
        // Hutan-only actions keep their keys.
        assert_eq!(last(NewAgent::name_for_type()), "shift-cmd-t");
        assert_eq!(last(JumpToNextAttention::name_for_type()), "shift-cmd-j");
        assert_eq!(last(ReviewRequest::name_for_type()), "shift-cmd-r");
        assert_eq!(last(ToggleResources::name_for_type()), "alt-cmd-r");
        // Zed's project search (E-13b); the session history moved from ⌘⇧F to ⌘Y.
        assert_eq!(last(ShowSearch::name_for_type()), "shift-cmd-f");
        assert_eq!(last(ShowSearchReplace::name_for_type()), "shift-cmd-h");
        assert_eq!(keys(SearchHistory::name_for_type()), ["cmd-y"]);
    }

    /// Today's table, kept for people who learned it: `keymap.base = "hutan"` only gains keys
    /// that were free (⌘P quick open and ⌘⇧E Files, E-13a; ⌘⇧F / ⌘⇧H search, E-13b; ⌘⌥I
    /// hover in the editor, CL-POLISH-4); none it had moves.
    #[test]
    fn hutan_base_is_the_original_table() {
        let mut expected: Vec<String> = [
            "cmd-k | workbench::TogglePalette | -",
            "cmd-, | workbench::OpenSettings | -",
            "cmd-t | workbench::NewTerminal | -",
            "shift-cmd-t | workbench::NewAgent | -",
            "alt-cmd-t | workbench::NewChat | -",
            "cmd-{ | workbench::ActivatePreviousTab | -",
            "cmd-} | workbench::ActivateNextTab | -",
            "shift-cmd-[ | workbench::ActivatePreviousTab | -",
            "shift-cmd-] | workbench::ActivateNextTab | -",
            "ctrl-shift-tab | workbench::ActivatePreviousTab | -",
            "ctrl-tab | workbench::ActivateNextTab | -",
            "shift-cmd-j | workbench::JumpToNextAttention | -",
            "shift-cmd-r | workbench::ReviewRequest | -",
            "cmd-d | workbench::SplitRight | -",
            "shift-cmd-d | workbench::SplitDown | -",
            "cmd-w | workbench::ClosePaneItem | -",
            "shift-cmd-w | workbench::StopSession | -",
            "shift-cmd-enter | workbench::ToggleZoom | -",
            "cmd-b | workbench::ToggleLeftDock | -",
            "alt-cmd-b | workbench::ToggleRightDock | -",
            "cmd-l | workbench::ToggleRightDock | -",
            "alt-cmd-j | workbench::ToggleBottomDock | -",
            "cmd-j | workbench::OpenJump | -",
            "cmd-p | workbench::QuickOpen | -",
            "shift-cmd-e | workbench::ShowFiles | -",
            "shift-cmd-m | workbench::ShowProblems | -",
            "shift-cmd-f | workbench::ShowSearch | -",
            "shift-cmd-h | workbench::ShowSearchReplace | -",
            "cmd-o | workbench::OpenProject | -",
            "shift-cmd-o | workbench::ToggleOverview | -",
            "alt-cmd-r | workbench::ToggleResources | -",
            "cmd-m | workbench::MinimizeWindow | -",
            "cmd-q | workbench::Quit | -",
            "alt-cmd-left | workbench::FocusNeighborLeft | -",
            "alt-cmd-right | workbench::FocusNeighborRight | -",
            "alt-cmd-up | workbench::FocusNeighborUp | -",
            "alt-cmd-down | workbench::FocusNeighborDown | -",
            "ctrl-cmd-left | workbench::ResizeLeft | -",
            "ctrl-cmd-right | workbench::ResizeRight | -",
            "ctrl-cmd-up | workbench::ResizeUp | -",
            "ctrl-cmd-down | workbench::ResizeDown | -",
            "ctrl-alt-down | workbench::NextContainer | -",
            "ctrl-alt-up | workbench::PreviousContainer | -",
            "cmd-n | workbench::FilesNewFile | FilesPanel",
            "alt-cmd-n | workbench::FilesNewFolder | FilesPanel",
            "f2 | workbench::FilesRename | FilesPanel",
            "cmd-d | workbench::FilesDuplicate | FilesPanel",
            "cmd-backspace | workbench::FilesTrash | FilesPanel",
            "cmd-c | workbench::FilesCopy | FilesPanel",
            "cmd-x | workbench::FilesCut | FilesPanel",
            "cmd-v | workbench::FilesPaste | FilesPanel",
            "cmd-z | workbench::FilesUndo | FilesPanel",
            "shift-cmd-z | workbench::FilesRedo | FilesPanel",
            "alt-cmd-c | workbench::FilesCopyPath | FilesPanel",
            "alt-shift-cmd-c | workbench::FilesCopyRelativePath | FilesPanel",
            "cmd-f | workbench::FilesFilter | FilesPanel",
            "cmd-c | hutan_terminal_view::Copy | HutanTerminal",
            "cmd-v | hutan_terminal_view::Paste | HutanTerminal",
            "cmd-up | hutan_terminal_view::ScrollToTop | HutanTerminal",
            "cmd-down | hutan_terminal_view::ScrollToBottom | HutanTerminal",
            "shift-cmd-up | hutan_terminal_view::JumpToPreviousPrompt | HutanTerminal",
            "shift-cmd-down | hutan_terminal_view::JumpToNextPrompt | HutanTerminal",
            "shift-cmd-a | hutan_terminal_view::SelectBlockOutput | HutanTerminal",
            "shift-cmd-c | hutan_terminal_view::CopyBlockOutput | HutanTerminal",
            "cmd-d | workbench::SplitTerminalRight | HutanTerminal",
            "shift-cmd-d | workbench::SplitTerminalDown | HutanTerminal",
            "alt-cmd-left | workbench::FocusSplitLeft | HutanTerminal",
            "alt-cmd-right | workbench::FocusSplitRight | HutanTerminal",
            "alt-cmd-up | workbench::FocusSplitUp | HutanTerminal",
            "alt-cmd-down | workbench::FocusSplitDown | HutanTerminal",
            "ctrl-cmd-left | workbench::ResizeSplitLeft | HutanTerminal",
            "ctrl-cmd-right | workbench::ResizeSplitRight | HutanTerminal",
            "ctrl-cmd-up | workbench::ResizeSplitUp | HutanTerminal",
            "ctrl-cmd-down | workbench::ResizeSplitDown | HutanTerminal",
            "shift-cmd-enter | workbench::ToggleSplitZoom | HutanTerminal",
            "shift-cmd-w | workbench::CloseTerminalTab | HutanTerminal",
            "cmd-f | workbench::FindInTerminal | HutanTerminal",
            "cmd-g | workbench::FindNextInTerminal | HutanTerminal",
            "shift-cmd-g | workbench::FindPreviousInTerminal | HutanTerminal",
            "cmd-f | workbench::FindInTerminal | TerminalFind",
            "cmd-g | workbench::FindNextInTerminal | TerminalFind",
            "shift-cmd-g | workbench::FindPreviousInTerminal | TerminalFind",
            "down | palette::SelectNext | Palette",
            "ctrl-n | palette::SelectNext | Palette",
            "up | palette::SelectPrev | Palette",
            "ctrl-p | palette::SelectPrev | Palette",
            "enter | palette::Confirm | Palette",
            "escape | palette::Dismiss | Palette",
            "escape | palette::Dismiss | ContextMenu",
            "enter | modal::Confirm | Modal",
            "escape | modal::Cancel | Modal",
            "escape | settings_sheet::Dismiss | SettingsSheet",
            "escape | palette::Dismiss | DaemonPopover",
            "escape | quick_terminal::HideOnEscape | QuickTerminal",
            "shift-cmd-enter | quick_terminal::MoveToMain | QuickTerminal",
            "cmd-enter | chat::SendMessage | HutanChat",
            "cmd-. | chat::InterruptTurn | HutanChat",
            "ctrl-enter | chat::SendNow | HutanChat",
            "down | palette::SelectNext | AgentPicker",
            "ctrl-n | palette::SelectNext | AgentPicker",
            "up | palette::SelectPrev | AgentPicker",
            "ctrl-p | palette::SelectPrev | AgentPicker",
            "enter | palette::Confirm | AgentPicker",
            "escape | palette::Dismiss | AgentPicker",
            "down | palette::SelectNext | Picker",
            "ctrl-n | palette::SelectNext | Picker",
            "up | palette::SelectPrev | Picker",
            "ctrl-p | palette::SelectPrev | Picker",
            "enter | palette::Confirm | Picker",
            "cmd-enter | palette::ConfirmInSplit | Picker",
            "escape | palette::Dismiss | Picker",
            "enter | setup::Continue | Setup",
            "escape | setup::Skip | Setup",
            "escape | workbench::CancelDockResize | DockResize",
            "escape | workbench::ClosePeek | Peek",
            "escape | workbench::DismissAttentionToasts | AttentionToast",
            "cmd-z | workbench::UndoLayoutChange | LayoutToast",
            "left | code_view::MoveLeft | CodeView && vim_mode != normal && vim_mode != visual",
            "right | code_view::MoveRight | CodeView && vim_mode != normal && vim_mode != visual",
            "up | code_view::MoveUp | CodeView && vim_mode != normal && vim_mode != visual",
            "down | code_view::MoveDown | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-left | code_view::MoveWordLeft | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-right | code_view::MoveWordRight | CodeView && vim_mode != normal && vim_mode != visual",
            "home | code_view::MoveLineStart | CodeView && vim_mode != normal && vim_mode != visual",
            "ctrl-a | code_view::MoveLineStart | CodeView && !vim_mode",
            "cmd-left | code_view::MoveLineStart | CodeView",
            "end | code_view::MoveLineEnd | CodeView && vim_mode != normal && vim_mode != visual",
            "ctrl-e | code_view::MoveLineEnd | CodeView && !vim_mode",
            "cmd-right | code_view::MoveLineEnd | CodeView",
            "cmd-up | code_view::MoveToStart | CodeView",
            "cmd-down | code_view::MoveToEnd | CodeView",
            "pageup | code_view::PageUp | CodeView",
            "pagedown | code_view::PageDown | CodeView",
            "shift-left | code_view::SelectLeft | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-right | code_view::SelectRight | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-up | code_view::SelectUp | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-down | code_view::SelectDown | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-shift-left | code_view::SelectWordLeft | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-shift-right | code_view::SelectWordRight | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-home | code_view::SelectLineStart | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-cmd-left | code_view::SelectLineStart | CodeView",
            "shift-end | code_view::SelectLineEnd | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-cmd-right | code_view::SelectLineEnd | CodeView",
            "shift-cmd-up | code_view::SelectToStart | CodeView",
            "shift-cmd-down | code_view::SelectToEnd | CodeView",
            "shift-pageup | code_view::SelectPageUp | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-pagedown | code_view::SelectPageDown | CodeView && vim_mode != normal && vim_mode != visual",
            "cmd-a | code_view::SelectAll | CodeView",
            "cmd-d | code_view::SelectNextOccurrence | CodeView",
            "shift-cmd-l | code_view::SelectAllOccurrences | CodeView",
            "escape | code_view::Cancel | CodeView && !vim_mode",
            "shift-enter | code_view::Newline | CodeView && vim_mode != normal && vim_mode != visual",
            "enter | code_view::Newline | CodeView && vim_mode != normal && vim_mode != visual",
            "tab | code_view::Tab | CodeView && vim_mode != normal && vim_mode != visual",
            "shift-tab | code_view::Outdent | CodeView && vim_mode != normal && vim_mode != visual",
            "cmd-] | code_view::Indent | CodeView",
            "cmd-[ | code_view::Outdent | CodeView",
            "shift-backspace | code_view::DeleteBackward | CodeView && vim_mode != normal && vim_mode != visual",
            "backspace | code_view::DeleteBackward | CodeView && vim_mode != normal && vim_mode != visual",
            "delete | code_view::DeleteForward | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-backspace | code_view::DeleteWordBackward | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-delete | code_view::DeleteWordForward | CodeView && vim_mode != normal && vim_mode != visual",
            "cmd-backspace | code_view::DeleteToLineStart | CodeView",
            "ctrl-k | code_view::DeleteToLineEnd | CodeView && vim_mode != normal && vim_mode != visual",
            "cmd-delete | code_view::DeleteToLineEnd | CodeView",
            "shift-cmd-k | code_view::DeleteLine | CodeView",
            "alt-shift-up | code_view::DuplicateLineUp | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-shift-down | code_view::DuplicateLineDown | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-up | code_view::MoveLineUp | CodeView && vim_mode != normal && vim_mode != visual",
            "alt-down | code_view::MoveLineDown | CodeView && vim_mode != normal && vim_mode != visual",
            "cmd-/ | code_view::ToggleComment | CodeView",
            "cmd-x | code_view::Cut | CodeView",
            "cmd-c | code_view::Copy | CodeView",
            "cmd-v | code_view::Paste | CodeView",
            "cmd-z | code_view::Undo | CodeView",
            "shift-cmd-z | code_view::Redo | CodeView",
            "cmd-s | code_view::Save | FileView",
            "cmd-f | code_view::Find | FileView",
            "alt-cmd-f | code_view::FindReplace | FileView",
            "cmd-g | code_view::FindNext | FileView",
            "shift-cmd-g | code_view::FindPrevious | FileView",
            "alt-cmd-z | code_view::RejectAgentHunk | FileView",
            "alt-z | code_view::ToggleSoftWrap | FileView",
            "alt-cmd-] | code_view::NextAgentHunk | FileView",
            "alt-cmd-[ | code_view::PreviousAgentHunk | FileView",
            "alt-f5 | code_view::NextGitHunk | FileView",
            "alt-shift-f5 | code_view::PreviousGitHunk | FileView",
            "alt-cmd-i | code_view::ShowHover | CodeView",
            "f12 | code_view::GoToDefinition | CodeView",
            "f8 | code_view::NextDiagnostic | CodeView",
            "shift-f8 | code_view::PreviousDiagnostic | CodeView",
            "ctrl-o | code_view::GoBack | CodeView && !vim_mode",
            "ctrl-space | code_view::ShowCompletions | CodeView && vim_mode != normal && vim_mode != visual",
            "ctrl-n | code_view::ShowCompletions | CodeView && vim_mode == insert && !showing_completions",
            "ctrl-p | code_view::ShowCompletions | CodeView && vim_mode == insert && !showing_completions",
            "tab | code_view::NextTabStop | CodeView && in_snippet && !showing_completions",
            "shift-tab | code_view::PreviousTabStop | CodeView && in_snippet && !showing_completions",
            "down | code_view::CompletionNext | CodeView && showing_completions",
            "ctrl-n | code_view::CompletionNext | CodeView && showing_completions",
            "up | code_view::CompletionPrevious | CodeView && showing_completions",
            "ctrl-p | code_view::CompletionPrevious | CodeView && showing_completions",
            "enter | code_view::ConfirmCompletion | CodeView && showing_completions",
            "tab | code_view::ConfirmCompletion | CodeView && showing_completions",
        ]
        .map(str::to_owned)
        .to_vec();
        for n in 1..=9 {
            expected.push(format!("cmd-{n} | workbench::ActivateTab{n} | -"));
            expected.push(format!(
                "shift-cmd-{n} | workbench::ApplyPresetNumber{n} | -"
            ));
        }
        assert_eq!(rendered(&default_bindings(KeymapBase::Hutan)), expected);
    }

    /// Context bindings are the same in both bases, except the Zed base's ⌘K for the palette,
    /// which the keyboard in a terminal does not have (the Hutan base's ⌘K has no context).
    #[test]
    fn the_two_bases_share_every_context_binding() {
        let contextual = |bindings: &[BindingSpec]| -> Vec<BindingSpec> {
            bindings
                .iter()
                .filter(|b| b.context.is_some())
                .filter(|b| b.action != TogglePalette::name_for_type())
                .cloned()
                .collect()
        };
        assert_eq!(
            contextual(&default_bindings(KeymapBase::Zed)),
            contextual(&default_bindings(KeymapBase::Hutan))
        );
        for base in [KeymapBase::Zed, KeymapBase::Hutan] {
            assert!(
                default_bindings(base)
                    .iter()
                    .any(|b| b.action == hutan_code_view::ShowHover::name_for_type()
                        && format_keys(&b.keys) == "alt-cmd-i"),
                "{base:?} has ⌘⌥I for hover"
            );
        }
    }

    /// ⌘K opens the palette in the Zed base wherever the keyboard is not in a terminal: its
    /// context says so in the terminal's own key context.
    #[test]
    fn zed_cmd_k_is_the_palette_outside_terminals_only() {
        let zed = default_bindings(KeymapBase::Zed);
        let cmd_k = zed
            .iter()
            .find(|b| format_keys(&b.keys) == "cmd-k")
            .expect("⌘K is bound in the Zed base");
        assert_eq!(cmd_k.action, TogglePalette::name_for_type());
        assert_eq!(
            cmd_k.context.as_deref(),
            Some(format!("!{}", hutan_terminal_view::KEY_CONTEXT).as_str())
        );
        assert_eq!(
            OUTSIDE_TERMINAL,
            format!("!{}", hutan_terminal_view::KEY_CONTEXT)
        );
        let predicate = gpui::KeyBindingContextPredicate::parse(OUTSIDE_TERMINAL).unwrap();
        let mut terminal = gpui::KeyContext::default();
        terminal.add(hutan_terminal_view::KEY_CONTEXT);
        let mut chat = gpui::KeyContext::default();
        chat.add("Chat");
        assert!(
            !predicate.eval(&[terminal]),
            "not with the keyboard in a terminal"
        );
        assert!(predicate.eval(&[chat]), "everywhere else");
    }

    #[test]
    fn every_palette_command_that_has_a_key_in_the_hutan_base_keeps_one_in_zed() {
        // Whatever the old base bound globally is still reachable by some key in the Zed base,
        // except the keys Zed gives to other things (moved on purpose: jump, bottom dock). The
        // jump list is the Sessions section of Zed's ⌘P (`QuickOpen`), so `OpenJump` itself
        // has no key there.
        let zed = default_bindings(KeymapBase::Zed);
        for binding in default_bindings(KeymapBase::Hutan)
            .iter()
            .filter(|b| b.context.is_none())
            .filter(|b| b.action != OpenJump::name_for_type())
        {
            assert!(
                zed.iter()
                    .any(|z| z.action == binding.action && z.context.is_none()),
                "{} lost its key in the Zed base",
                binding.action
            );
        }
        assert!(zed.iter().any(|z| z.action == QuickOpen::name_for_type()));
    }
}
