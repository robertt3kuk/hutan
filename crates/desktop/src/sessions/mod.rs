//! Live sessions: the connection bridge, shared terminal attachments, the store that feeds the
//! one agent presentation every Agents surface reads (the reducer, attention and model names are
//! `hutan-agents-view`'s, GPUI-free and shared with the daemon), what the window draws of it,
//! and the workspace ordering, filtering and tree the sidebar uses.

pub mod agents;
pub mod agents_list;
pub mod agents_text;
pub mod calls;
pub mod card;
pub mod checkpoints;
pub mod folders;
pub mod fork;
pub mod launch;
mod placement;
mod store;
pub mod tasks;
mod terminal;
pub mod tree;
pub mod untracked;
pub mod update;
pub mod usage;
pub mod workspaces;

pub use agents::AgentAttentionChanged;
pub use placement::{PlacementBadge, placement_badge, row_placement_badge, works_in_checkout};
pub use store::{
    AccountsChanged, ActivityCause, Checkout, CheckoutActivity, CloseFailure, FS_RECOUNT_AFTER,
    FS_RECOUNT_EVERY, FilesRootChoice, FilesRootChoices, MawasFocus, MawasFolder, MemoryChanged,
    REPLAY_WINDOW, RecoverySummaryDue, RequestCalled, ScopeKind, ScopeLabel, SessionClosed,
    SessionsStore, StoreChanged, UI_OPEN_MAX_AGE_MS, UiOpenRequested, UpdateChanged,
    UsageSummaryChanged,
};
pub use terminal::TerminalSession;
pub use untracked::Untracked;
