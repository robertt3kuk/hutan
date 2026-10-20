//! The workbench: GPUI-free layout model plus the GPUI shell around it.

pub mod actions;
pub(crate) mod agents;
mod agents_page;
mod assistant;
mod changes;
pub mod chat;
mod closing;
mod daemon;
mod daemon_popover;
mod docks;
mod harness_install;
pub(crate) mod harnesses;
mod history;
mod integrations;
mod launcher;
mod launcher_ui;
mod mac;
mod modal;
pub mod model;
mod monitor;
mod notify;
pub mod palette;
pub mod panels;
pub mod persistence;
mod projects;
mod providers;
mod recovery_summary;
pub mod regions;
pub mod registry;
mod remote;
mod render;
mod resources;
mod review;
pub(crate) mod send_key;
mod settings_assistant;
mod settings_daemon;
mod settings_keys;
pub mod settings_labels;
mod settings_layers;
pub mod settings_mcp;
mod settings_pages;
mod settings_rows;
pub mod settings_schema;
mod settings_sheet;
mod setup;
mod sidebars;
pub mod style;
mod terminals;
pub mod text_field;
pub mod ui;
mod view;
pub mod vim_ui;
mod workspaces;
mod worktrees;

pub use agents::{AgentPicker, CODEX, Models};
pub use changes::{
    ChangesEvent, ChangesView, CleanupDefault, DeliveryForm, Mark, ReviewComment, Selection,
    SendTarget, SnippetLine, hunk_ranges,
};
pub use daemon::{DaemonBusy, log_file as daemon_log_file};
pub use docks::{COLLAPSE_DRAG, RESIZE_CONTEXT};
pub use harnesses::{Detection, HARNESSES, Harness, HarnessDirectory, Listing};
pub use launcher::{
    HarnessChoice, LaunchAgent, LaunchWhere, Launcher, LauncherField, LauncherPreset, LauncherStop,
    ProjectChoice, Readiness, StartConfirm, WorktreeList,
};
pub use modal::{Modal, ModalCheckbox, ModalChoice};
pub use panels::{HistoryConfirm, HistoryPanel, StatusTone, connection_label};
pub use registry::{PanelEnv, StatusBarLayout, StatusData};
pub use review::ReviewHandoff;
pub use settings_assistant::{AssistantDefaults, MawasGrants, Running as AssistantRunning};
pub use settings_daemon::DaemonSettingsState;
pub use settings_keys::SheetFocus;
pub use settings_layers::FieldSource;
pub use settings_pages::{DataTarget, HistoryIndex};
pub use settings_sheet::{Edit, SettingsSheet};
pub use setup::{NotificationUi, SetupFinish, SetupState, SetupStep};
pub use sidebars::{NavEntry, NavTarget, strip_split};
pub use terminals::session_of_item;
pub use view::{
    BadgeSite, DragPreview, DraggedItem, Region, SAVE_DEBOUNCE, Workbench, WorkbenchOptions,
    preset_label,
};
pub use workspaces::{HistoryState, PickerEntry, PickerKind, PickerState};
