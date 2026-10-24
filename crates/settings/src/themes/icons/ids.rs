//! Stable semantic product-icon catalog (schema v1). Artwork is not the public API.

macro_rules! catalog {
    ($( $variant:ident => ($id:literal, $file:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(usize)]
        pub enum IconId { $( $variant ),+ }
        impl IconId {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const COUNT: usize = Self::ALL.len();
            pub const fn index(self) -> usize { self as usize }
            pub const fn id(self) -> &'static str { match self { $(Self::$variant => $id),+ } }
            pub fn from_id(id: &str) -> Option<Self> { match id { $($id => Some(Self::$variant)),+, _ => None } }
        }
    };
}

impl IconId {
    pub fn for_panel(panel: &str) -> Self {
        match panel {
            "sessions" => Self::PanelSessions,
            "history" => Self::PanelHistory,
            "changes" => Self::PanelChanges,
            "inspector" => Self::PanelInspector,
            "agents" => Self::PanelAgents,
            "agents-dashboard" => Self::PanelAgentsDashboard,
            "resources" => Self::PanelResources,
            "tasks" => Self::PanelTasks,
            "assistant" => Self::PanelAssistant,
            "mawas-memory" => Self::PanelMawasMemory,
            "notifications" => Self::PanelNotifications,
            "files" => Self::PanelFiles,
            "instructions" => Self::PanelInstructions,
            _ => Self::StateInfo,
        }
    }
}

catalog! {
    NavSearch => ("nav.search", "magnifying-glass.svg"),
    ActionSearch => ("action.search", "magnifying-glass.svg"),
    NavTasks => ("nav.tasks", "list-checks.svg"),
    PanelTasks => ("panel.tasks", "list-checks.svg"),
    NavAgents => ("nav.agents", "robot.svg"),
    PanelAgentsDashboard => ("panel.agents-dashboard", "robot.svg"),
    NavMawas => ("nav.mawas", "sparkle.svg"),
    PanelAssistant => ("panel.assistant", "sparkle.svg"),
    PanelHistory => ("panel.history", "clock-counter-clockwise.svg"),
    PanelChanges => ("panel.changes", "git-diff.svg"),
    ActionShowChanges => ("action.show-changes", "git-diff.svg"),
    PanelInspector => ("panel.inspector", "hand-palm.svg"),
    PanelAgents => ("panel.agents", "robot.svg"),
    PanelResources => ("panel.resources", "gauge.svg"),
    ActionGroup => ("action.group", "rows.svg"),
    ActionFilter => ("action.filter", "funnel.svg"),
    ActionRefresh => ("action.refresh", "arrow-clockwise.svg"),
    ActionSettings => ("action.settings", "gear-six.svg"),
    NavSettings => ("nav.settings", "gear-six.svg"),
    ActionExpand => ("action.expand", "caret-down.svg"),
    ActionMenuDown => ("action.menu-down", "caret-down.svg"),
    ActionCollapse => ("action.collapse", "caret-up.svg"),
    ActionSortAscending => ("action.sort-ascending", "caret-up.svg"),
    ActionBack => ("action.back", "caret-left.svg"),
    ActionMore => ("action.more", "dots-three.svg"),
    ActionAdd => ("action.add", "plus.svg"),
    ActionNewSession => ("action.new-session", "plus.svg"),
    ActionSort => ("action.sort", "arrows-down-up.svg"),
    PanelSessions => ("panel.sessions", "folders.svg"),
    NavProjects => ("nav.projects", "folders.svg"),
    PanelMawasMemory => ("panel.mawas-memory", "brain.svg"),
    NavMemory => ("nav.memory", "brain.svg"),
    PanelNotifications => ("panel.notifications", "bell.svg"),
    StatusNotifications => ("status.notifications", "bell.svg"),
    NavHome => ("nav.home", "house.svg"),
    NavWorkspace => ("nav.workspace", "stack.svg"),
    StatusWorkspace => ("status.workspace", "stack.svg"),
    NavProjectRepository => ("nav.project.repository", "git-branch.svg"),
    NavProjectFolder => ("nav.project.folder", "folder.svg"),
    SessionTerminal => ("session.terminal", "terminal.svg"),
    HarnessHutan => ("harness.hutan", "terminal.svg"),
    SessionHarness => ("session.harness", "robot.svg"),
    SessionDirect => ("session.direct", "chat-circle.svg"),
    HarnessOmp => ("harness.omp", "terminal.svg"),
    HarnessClaude => ("harness.claude", "chat-circle.svg"),
    HarnessCodex => ("harness.codex", "robot.svg"),
    HarnessUnknown => ("harness.unknown", "robot.svg"),
    VcsBranch => ("vcs.branch", "git-branch.svg"),
    VcsWorktree => ("vcs.worktree", "tree-structure.svg"),
    VcsMainCheckout => ("vcs.main-checkout", "folder.svg"),
    VcsDirty => ("vcs.dirty", "circle.svg"),
    VcsClean => ("vcs.clean", "check-circle.svg"),
    VcsSharedWarning => ("vcs.shared-warning", "warning.svg"),
    StateWorking => ("state.working", "circle-notch.svg"),
    StateBlocked => ("state.blocked", "warning.svg"),
    StateWarning => ("state.warning", "warning.svg"),
    StateIdle => ("state.idle", "circle.svg"),
    StateTodo => ("state.todo", "circle.svg"),
    StateReady => ("state.ready", "lightning.svg"),
    StateRunning => ("state.running", "play.svg"),
    StateCompleted => ("state.completed", "check-circle.svg"),
    StateFailed => ("state.failed", "x-circle.svg"),
    StateInterrupted => ("state.interrupted", "prohibit.svg"),
    StateCancelled => ("state.cancelled", "prohibit.svg"),
    StateDisconnected => ("state.disconnected", "stop-circle.svg"),
    StateStopped => ("state.stopped", "stop-circle.svg"),
    StateUnknown => ("state.unknown", "question.svg"),
    StateUnread => ("state.unread", "circle.svg"),
    StateMuted => ("state.muted", "bell-slash.svg"),
    StateApproval => ("state.approval", "hand-palm.svg"),
    StatusApprovals => ("status.approvals", "hand-palm.svg"),
    StateQuestion => ("state.question", "question.svg"),
    StateReview => ("state.review", "eye.svg"),
    StatePlan => ("state.plan", "list-checks.svg"),
    StateInfo => ("state.info", "info.svg"),
    StatusAttention => ("status.attention", "warning.svg"),
    StatusConnected => ("status.connected", "plug.svg"),
    StatusConnecting => ("status.connecting", "circle-notch.svg"),
    StatusDisconnected => ("status.disconnected", "plugs.svg"),
    StatusSessions => ("status.sessions", "terminal.svg"),
    StatusDensity => ("status.density", "rows.svg"),
    StatusTheme => ("status.theme", "sun.svg"),
    StatusPalette => ("status.palette", "command.svg"),
    DockLeft => ("dock.left", "sidebar-simple.svg"),
    DockRight => ("dock.right", "sidebar-right.svg"),
    DockBottom => ("dock.bottom", "rows.svg"),
    DockCompact => ("dock.compact", "sidebar-simple.svg"),
    DockHide => ("dock.hide", "arrows-in.svg"),
    DockExpand => ("dock.expand", "arrows-out.svg"),
    ActionDisclose => ("action.disclose", "caret-right.svg"),
    ActionSortDescending => ("action.sort-descending", "caret-down.svg"),
    ActionStart => ("action.start", "play.svg"),
    ActionClose => ("action.close", "x.svg"),
    ActionInterrupt => ("action.interrupt", "stop-fill.svg"),
    ActionStop => ("action.stop", "stop-circle.svg"),
    ActionApprove => ("action.approve", "check-circle.svg"),
    ActionDeny => ("action.deny", "prohibit.svg"),
    ActionOpenSession => ("action.open-session", "arrow-square-out.svg"),
    ActionRevealSession => ("action.reveal-session", "eye.svg"),
    ActionOpenWorktree => ("action.open-worktree", "folder-open.svg"),
    ActionCopy => ("action.copy", "copy.svg"),
    ActionRename => ("action.rename", "pencil.svg"),
    ActionDelete => ("action.delete", "trash.svg"),
    ActionPin => ("action.pin", "push-pin.svg"),
    ActionRecover => ("action.recover", "arrow-clockwise.svg"),
    ActionSubagents => ("action.subagents", "tree-structure.svg"),
    ActionForward => ("action.forward", "arrow-up.svg"),
    ActionAddContext => ("action.add-context", "plus.svg"),
    BrandHutan => ("brand.hutan", "hutan.svg"),
    BrandMawas => ("brand.mawas", "sparkle.svg"),
    ItemFile => ("item.file", "file-code.svg"),
    ItemFolder => ("item.folder", "folder.svg"),
    ItemFolderOpen => ("item.folder-open", "folder-open.svg"),
    PanelFiles => ("panel.files", "file-code.svg"),
    PanelInstructions => ("panel.instructions", "file-code.svg"),
}

pub(super) fn embedded(path: &str) -> Option<&'static [u8]> {
    match path {
        "arrow-clockwise.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrow-clockwise.svg"
        )),
        "arrow-up.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrow-up.svg"
        )),
        "arrow-square-out.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrow-square-out.svg"
        )),
        "arrows-down-up.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrows-down-up.svg"
        )),
        "arrows-in.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrows-in.svg"
        )),
        "arrows-out.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/arrows-out.svg"
        )),
        "bell-slash.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/bell-slash.svg"
        )),
        "bell.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/bell.svg"
        )),
        "brain.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/brain.svg"
        )),
        "caret-down.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/caret-down.svg"
        )),
        "caret-left.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/caret-left.svg"
        )),
        "caret-right.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/caret-right.svg"
        )),
        "caret-up.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/caret-up.svg"
        )),
        "chat-circle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/chat-circle.svg"
        )),
        "check-circle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/check-circle.svg"
        )),
        "circle-notch.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/circle-notch.svg"
        )),
        "circle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/circle.svg"
        )),
        "clock-counter-clockwise.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/clock-counter-clockwise.svg"
        )),
        "command.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/command.svg"
        )),
        "copy.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/copy.svg"
        )),
        "dots-three.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/dots-three.svg"
        )),
        "eye.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/eye.svg"
        )),
        "file-code.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/file-code.svg"
        )),
        "folder-open.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/folder-open.svg"
        )),
        "folder.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/folder.svg"
        )),
        "folders.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/folders.svg"
        )),
        "funnel.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/funnel.svg"
        )),
        "gauge.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/gauge.svg"
        )),
        "gear-six.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/gear-six.svg"
        )),
        "git-branch.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/git-branch.svg"
        )),
        "git-diff.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/git-diff.svg"
        )),
        "hand-palm.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/hand-palm.svg"
        )),
        "house.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/house.svg"
        )),
        "hutan.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/hutan.svg"
        )),
        "info.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/info.svg"
        )),
        "lightning.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/lightning.svg"
        )),
        "list-checks.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/list-checks.svg"
        )),
        "magnifying-glass.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/magnifying-glass.svg"
        )),
        "pencil.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/pencil.svg"
        )),
        "play.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/play.svg"
        )),
        "plug.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/plug.svg"
        )),
        "plugs.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/plugs.svg"
        )),
        "plus.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/plus.svg"
        )),
        "prohibit.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/prohibit.svg"
        )),
        "push-pin.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/push-pin.svg"
        )),
        "question.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/question.svg"
        )),
        "robot.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/robot.svg"
        )),
        "rows.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/rows.svg"
        )),
        "sidebar-right.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/sidebar-right.svg"
        )),
        "sidebar-simple.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/sidebar-simple.svg"
        )),
        "sparkle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/sparkle.svg"
        )),
        "stack.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/stack.svg"
        )),
        "stop-circle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/stop-circle.svg"
        )),
        "stop-fill.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/stop-fill.svg"
        )),
        "sun.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/sun.svg"
        )),
        "terminal.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/terminal.svg"
        )),
        "trash.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/trash.svg"
        )),
        "tree-structure.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/tree-structure.svg"
        )),
        "warning.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/warning.svg"
        )),
        "x-circle.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/x-circle.svg"
        )),
        "x.svg" => Some(include_bytes!(
            "../../../../../assets/icon-themes/phosphor/x.svg"
        )),
        _ => None,
    }
}
