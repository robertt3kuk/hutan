//! The workbench: GPUI-free layout model plus the GPUI shell around it.

pub mod actions;
pub mod agent_edits;
pub(crate) mod agents;
mod agents_page;
mod assistant;
mod attention_toast;
mod changes;
pub mod chat;
mod closing;
mod daemon;
mod daemon_popover;
mod docks;
pub mod editor_status;
mod file_ex;
mod file_sync;
pub mod file_view;
pub mod files;
mod first_run;
mod harness_install;
pub(crate) mod harnesses;
mod history;
mod integrations;
mod launcher;
mod launcher_ui;
mod layout_offer;
mod lifecycle;
pub mod lsp;
mod lsp_bar;
mod lsp_complete;
mod lsp_nav;
mod mac;
mod modal;
pub mod model;
mod monitor;
mod notify;
pub mod open_link;
pub mod palette;
pub mod panels;
pub mod persistence;
mod projects;
mod providers;
pub mod quick_open;
mod recovery_summary;
pub mod regions;
pub mod registry;
mod reinstall;
mod remote;
mod render;
mod resources;
mod review;
pub mod rewind;
mod rewind_view;
pub mod search_replace;
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
mod settings_theme_preview;
mod settings_themes;
mod setup;
mod sidebars;
mod start_failure;
pub mod style;
mod terminals;
pub mod text_field;
pub mod ui;
mod view;
mod vim_keys;
pub mod vim_ui;
mod workspaces;
mod worktrees;

pub use agents::{AgentPicker, CODEX, Models};
pub use changes::{
    ChangesEvent, ChangesView, CleanupDefault, DeliveryForm, IndexFilter, Mark, ReviewComment,
    Selection, SendTarget, SnippetLine, StepTone, finish_step_lines, hunk_ranges, index_mark,
};
pub use daemon::{DaemonBusy, log_file as daemon_log_file};
pub use docks::{COLLAPSE_DRAG, RESIZE_CONTEXT};
pub use files::{BufferStore, FileOpen};
pub use first_run::NotifyOffer;
pub use harnesses::{Detection, HARNESSES, Harness, HarnessDirectory, Listing};
pub use launcher::{
    HarnessChoice, LaunchAgent, LaunchWhere, Launcher, LauncherField, LauncherPreset, LauncherStop,
    ProjectChoice, Readiness, StartConfirm, WorktreeList,
};
pub use modal::{Modal, ModalCheckbox, ModalChoice};
pub use panels::{HistoryConfirm, HistoryPanel, StatusTone, connection_label};
pub use registry::{PanelEnv, StatusBarLayout, StatusData};
pub use render::TabLook;
pub use review::ReviewHandoff;
pub use settings_assistant::{AssistantDefaults, MawasGrants, Running as AssistantRunning};
pub use settings_daemon::DaemonSettingsState;
pub use settings_keys::SheetFocus;
pub use settings_layers::FieldSource;
pub use settings_pages::{DataTarget, HistoryIndex, VimKeyRow};
pub use settings_sheet::{Edit, SettingsSheet};
pub use settings_themes::{PackField, PackPreview, PickerRow};
pub use setup::{NotificationUi, SetupFinish, SetupState, SetupStep};
pub use sidebars::{NavEntry, NavTarget, rail_split, strip_split};
pub use start_failure::StartFailureKind;
pub use terminals::session_of_item;
pub use view::{
    BadgeSite, DragPreview, DraggedItem, Region, SAVE_DEBOUNCE, Workbench, WorkbenchOptions,
    preset_label,
};
pub use workspaces::{HistoryState, PickerEntry, PickerKind, PickerState};
