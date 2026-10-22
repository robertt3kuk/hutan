//! Live sessions: the connection bridge, attention, shared terminal attachments, the one agent
//! presentation every Agents surface reads, and the workspace ordering, filtering and tree the
//! sidebar uses.

pub mod agents;
pub mod agents_list;
pub mod agents_text;
pub mod attention;
pub mod calls;
pub mod checkpoints;
pub mod launch;
pub mod models;
mod placement;
pub mod presentation;
mod store;
pub mod tasks;
mod terminal;
pub mod tree;
pub mod untracked;
pub mod workspaces;

pub use agents::{AgentAttentionChanged, AgentBadge, BlockedNode};
pub use attention::{
    Attention, AttentionEntry, AttentionKind, AttentionTracker, Marks, Presentation, Tone, Unread,
    classify, ended_text, present,
};
pub use placement::{PlacementBadge, placement_badge, row_placement_badge};
pub use presentation::{
    AgentPresentation, AgentRecord, Counts, Group, Link, Reason, ReasonKind, Role, Scope, State,
};
pub use store::{
    AccountsChanged, ActivityCause, Checkout, CheckoutActivity, CloseFailure, MemoryChanged,
    REPLAY_WINDOW, RecoverySummaryDue, RequestCalled, ScopeKind, ScopeLabel, SessionClosed,
    SessionsStore, StoreChanged, UI_OPEN_MAX_AGE_MS, UiOpenRequested,
};
pub use terminal::TerminalSession;
pub use untracked::Untracked;
