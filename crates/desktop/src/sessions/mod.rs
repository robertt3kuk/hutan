//! Live sessions: the connection bridge, attention, shared terminal attachments, and the
//! workspace ordering, filtering and tree the sidebar uses.

pub mod agents;
pub mod attention;
pub mod launch;
mod placement;
mod store;
pub mod tasks;
mod terminal;
pub mod tree;
pub mod workspaces;

pub use agents::{AgentAttentionChanged, AgentBadge, BlockedNode};
pub use attention::{
    AttentionEntry, AttentionKind, AttentionTracker, Presentation, Tone, Unread, classify,
    ended_text, present,
};
pub use placement::{PlacementBadge, placement_badge};
pub use store::{
    AccountsChanged, CloseFailure, REPLAY_WINDOW, RecoverySummaryDue, SessionClosed, SessionsStore,
    StoreChanged, UI_OPEN_MAX_AGE_MS, UiOpenRequested,
};
pub use terminal::TerminalSession;
