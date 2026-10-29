//! Built-in panels and status items. Each panel is registered under its id with a settings
//! namespace `panels.<id>`; the layout decides where it lives.

mod agents;
mod agents_page;
mod assistant;
mod assistant_rail;
mod changes;
pub(crate) mod common;
mod context;
pub mod context_model;
mod files;
mod files_git;
mod files_names;
mod files_tree;
mod history;
mod history_catalog;
mod history_model;
mod history_view;
mod inspector;
mod instructions;
mod memory;
mod problems;
mod resources;
mod resources_model;
mod search;
mod sessions;
mod status;
mod tasks;

use super::model::DockPosition;
use super::registry::{PanelRegistry, PanelSpec};
use super::style::UiStyle;
use crate::theme::DensityExt as _;
use gpui::{
    AnyView, AppContext as _, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};
use std::rc::Rc;

pub use agents::{AgentsPanel, RowView, SectionView, TICK as AGENTS_TICK};
pub use agents_page::{AgentsPage, AgentsRequest, TICK as AGENTS_PAGE_TICK};
pub use assistant::{
    AssistantPanel, Choice as AssistantChoice, Failure, MawasMode, Opened, ScopeEntry, ScopeKey,
};
pub use assistant_rail::{
    RailOption, Section as TodaySection, Today, TodayKind, TodayRow, WorkingRow, today,
};
pub use changes::{
    ChangesPanel, Checkouts as ChangesCheckouts, Counts as ChangesCounts,
    REFRESH_AFTER as CHANGES_REFRESH,
};
pub use common::{
    age_label, agent_badge, harness_icon, placement_badge, recovery_chip, session_icon,
    session_icon_with, shared_checkout_chip, started_as, state_dot, time_tooltip, tone_color,
};
pub(crate) use common::{edit_text, field_box};
pub use context::{
    ContextPanel, Empty as ContextEmpty, FOLD_WIDTH as CONTEXT_FOLD_WIDTH,
    ForkGate as ContextForkGate, Load as ContextLoad, Measure as ContextMeasure,
    RewindOffer as ContextRewindOffer,
};
pub use files::{
    AgentFile, DIAGNOSTICS_READ_EVERY as FILES_DIAGNOSTICS_READ_EVERY,
    DiagCounts as FilesDiagCounts, DiagMarks as FilesDiagMarks, DraggedRows as FilesDragged,
    EditKind as FilesEditKind, ExplorerFeed, FILES_AUTOSCROLL_EVERY, FILES_SPRING_AFTER,
    FILTER_DEBOUNCE as FILES_FILTER_DEBOUNCE, FilesCommand, FilesMemory, FilesPanel,
    FilterStatus as FilesFilterStatus, GIT_READ_EVERY as FILES_GIT_READ_EVERY,
    MAX_AGENT_ROWS as FILES_MAX_AGENT_ROWS, MAX_CACHED_ROOTS as FILES_MAX_CACHED_ROOTS,
    MAX_OPEN_ROWS as FILES_MAX_OPEN_ROWS, MAX_WATCHED_DIRS as FILES_MAX_WATCHED_DIRS,
    NameEditView as FilesNameEdit, OpenEditor, RELIST_EVERY as FILES_RELIST_EVERY,
    Section as FilesSection,
};
pub use files_git::{GitMark as FileGitMark, GitStatus as FileGitStatus};
pub use files_names::{
    EDGE as FILES_EDGE, Journal as FilesJournal, Step as FilesStep, copy_name as files_copy_name,
    edge_scroll as files_edge_scroll, listed_names as files_listed_names,
    stem_range as files_stem_range,
};
pub use files_tree::{
    ExpandRefusal as FileExpandRefusal, MORE_ROW as FILES_MORE_ROW, Row as FileRow,
    RowKind as FileRowKind, Tree as FileTree, TreeOptions as FileTreeOptions, file_icon,
    natural_cmp,
};
pub(crate) use history::chip;
pub use history::{Confirm as HistoryConfirm, HistoryPanel, Mode as HistoryMode};
pub use history_catalog::{
    ActionEntry as HistoryActionEntry, Entry as HistoryEntry, GroupBy as HistoryGroupBy,
    RowAction as HistoryRowAction, SessionItem as HistorySessionItem,
};
pub use history_model::{
    BlockTarget, DEBOUNCE as HISTORY_DEBOUNCE, Filters as HistoryFilters, HistoryResume,
    HutanTarget, PICKER_LIMIT as HISTORY_PICKER_LIMIT, Snippet, format_bytes, hit_title,
    hutan_target, parse_snippet, shell_join, short_path, source_icon, source_label,
    transcript_path,
};
pub use inspector::{BlockRow, InspectorPanel};
pub use instructions::{Instructions, InstructionsPanel};
pub use memory::{
    Cursor as MemoryCursor, EditorTarget as MemoryEditorTarget, MemoryPanel,
    ScopeChoice as MemoryScopeChoice,
};
pub use problems::{
    Problem, ProblemRow, Problems, ProblemsPanel, filter_label as problems_filter_label,
};
pub use resources::{
    Link as ResourcesLink, MAX_INTERVAL_MS, REFRESH_AFTER as RESOURCES_REFRESH, ResourcesPanel,
    WATCH as RESOURCES_WATCH,
};
pub use resources_model::{
    AgentLine as ResourceAgentLine, Command as ResourceCommand,
    Confirmation as ResourceConfirmation, Fold as ResourceFold, Interrupt as ResourceInterrupt,
    Item as ResourceItem, Line as ResourceLine, ProcessLine as ResourceProcessLine,
    SessionLine as ResourceSessionLine, Target as ResourceTarget,
};
pub use search::{ReplaceCount, ResultFile, SearchField, SearchPanel, SearchRow, SearchStatus};
pub use sessions::{DraggedProject, DraggedWorkspace, RowKind, SessionsPanel, SidebarRow};
pub use status::{
    CHIP_HINT, ChipKind, ConnectionChip, START_HINT, Tone as StatusTone, builtin_status_items,
    chip_of_connection, connection_label,
};
pub use tasks::{CascadeAsk, TICK as TASKS_TICK, TasksPanel};

/// A panel that renders its name and a short description.
#[derive(Debug)]
pub struct PlaceholderPanel {
    title: SharedString,
    description: SharedString,
}

impl PlaceholderPanel {
    pub fn new(title: impl Into<SharedString>, description: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
        }
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let tokens = style.density.ui();
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 3.0))
            .text_size(px(tokens.font_size))
            .text_color(style.theme.fg)
            .child(
                div()
                    .text_size(px(tokens.section_font_size))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(style.theme.muted)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .text_color(style.theme.muted)
                    .child(self.description.clone()),
            )
    }
}

/// `sessions`, `files`, `history`, `changes`, `inspector` (titled Requests), `agents` (the
/// compact projection of the Agents page), `agents-dashboard` (the Agents page), `resources`,
/// `tasks`, `assistant` (Mawas Home), `problems` (the language servers' diagnostics),
/// `search` (find and replace in the project's files), `instructions` (the files the agents read),
/// `notifications`. `sessions`
/// (Projects), `files`, `tasks` and `search` open as pages of the left sidebar, `history`,
/// `changes`, `inspector`, `agents` and `resources` as tabs of the right sidebar's strip,
/// `assistant` and `agents-dashboard` as tabs of the center, `problems` in the bottom dock.
pub fn builtin_panels() -> PanelRegistry {
    let mut registry = PanelRegistry::new();
    registry.register(PanelSpec {
        id: "sessions".into(),
        title: "Projects".into(),
        icon: crate::icons::Icon::PanelSessions,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| SessionsPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "files".into(),
        title: "Files".into(),
        icon: crate::icons::Icon::PanelFiles,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| FilesPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "history".into(),
        title: "History".into(),
        icon: crate::icons::Icon::PanelHistory,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| HistoryPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "changes".into(),
        title: "Changes".into(),
        icon: crate::icons::Icon::PanelChanges,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, window, cx| {
            let env = env.clone();
            let panel = cx.new(|cx| ChangesPanel::new(env, cx));
            panel.update(cx, |panel, cx| panel.follow_window(window, cx));
            AnyView::from(panel)
        }),
    });
    registry.register(PanelSpec {
        id: "context".into(),
        title: "Context".into(),
        icon: crate::icons::Icon::PanelContext,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| ContextPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "inspector".into(),
        title: "Requests".into(),
        icon: crate::icons::Icon::PanelInspector,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| InspectorPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: super::model::AGENTS_PAGE.into(),
        title: "Agents".into(),
        icon: crate::icons::Icon::PanelAgentsDashboard,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| AgentsPage::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "agents".into(),
        title: "Agents".into(),
        icon: crate::icons::Icon::PanelAgents,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| AgentsPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "resources".into(),
        title: "Resources".into(),
        icon: crate::icons::Icon::PanelResources,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| ResourcesPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "tasks".into(),
        title: "Tasks".into(),
        icon: crate::icons::Icon::PanelTasks,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| TasksPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "assistant".into(),
        title: "Mawas".into(),
        icon: crate::icons::Icon::PanelAssistant,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| AssistantPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "mawas-memory".into(),
        title: "Memory".into(),
        icon: crate::icons::Icon::PanelMawasMemory,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| MemoryPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "problems".into(),
        title: "Problems".into(),
        icon: crate::icons::Icon::StatusAttention,
        default_dock: DockPosition::Bottom,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| ProblemsPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "instructions".into(),
        title: "Instructions".into(),
        icon: crate::icons::Icon::PanelInstructions,
        default_dock: DockPosition::Right,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| InstructionsPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "search".into(),
        title: "Search".into(),
        icon: crate::icons::Icon::ActionSearch,
        default_dock: DockPosition::Left,
        factory: Rc::new(|env, _window, cx| {
            let env = env.clone();
            AnyView::from(cx.new(|cx| SearchPanel::new(env, cx)))
        }),
    });
    registry.register(PanelSpec {
        id: "notifications".into(),
        title: "Notifications".into(),
        icon: crate::icons::Icon::PanelNotifications,
        default_dock: DockPosition::Bottom,
        factory: Rc::new(|_env, _window, cx| {
            AnyView::from(cx.new(|_| {
                PlaceholderPanel::new(
                    "Notifications",
                    "Toasts and unread attention events collect here (arrives with M6).",
                )
            }))
        }),
    });
    registry
}
