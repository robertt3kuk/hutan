//! Live sessions: the connection bridge, attention, shared terminal attachments, the one agent
//! presentation every Agents surface reads, and the workspace ordering, filtering and tree the
//! sidebar uses.

pub mod agents;
pub mod agents_list;
pub mod agents_text;
pub mod attention;
pub mod calls;
pub mod card;
pub mod checkpoints;
pub mod folders;
pub mod fork;
pub mod launch;
pub mod models;
mod placement;
pub mod presentation;
mod store;
pub mod tasks;
mod terminal;
pub mod tree;
pub mod untracked;
pub mod usage;
pub mod workspaces;

pub use agents::{AgentAttentionChanged, AgentBadge, BlockedNode};
pub use attention::{
    Attention, AttentionEntry, AttentionKind, AttentionTracker, Marks, Presentation, TabMarks,
    TabState, Tone, Unread, agent_app, agent_note, agent_subtitle, classify, ended_text, present,
    tab_marks,
};
pub use placement::{PlacementBadge, placement_badge, row_placement_badge, works_in_checkout};
pub use presentation::{
    AgentPresentation, AgentRecord, Counts, Group, Link, Reason, ReasonKind, Role, Scope, State,
};
pub use store::{
    AccountsChanged, ActivityCause, Checkout, CheckoutActivity, CloseFailure, FS_RECOUNT_AFTER,
    FS_RECOUNT_EVERY, FilesRootChoice, FilesRootChoices, MawasFocus, MawasFolder, MemoryChanged,
    REPLAY_WINDOW, RecoverySummaryDue, RequestCalled, ScopeKind, ScopeLabel, SessionClosed,
    SessionsStore, StoreChanged, UI_OPEN_MAX_AGE_MS, UiOpenRequested, UsageSummaryChanged,
};
pub use terminal::TerminalSession;
pub use untracked::Untracked;
