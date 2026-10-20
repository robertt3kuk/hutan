//! What the desktop's agent chat view shows, without GPUI: the [`Transcript`] that folds a
//! session's [`AgentEvent`](hutan_core::AgentEvent)s (journal and live, in any mix) into
//! entries, the [`markdown`] subset of messages, and unified [`diff`]s.
//!
//! The view itself (cards, composer, approvals) lives in `hutan-desktop`, which owns the
//! chrome theme and density; this crate is what is worth testing without a window.

pub mod diff;
pub mod markdown;
pub mod model;

pub use model::{
    Approval, ApprovalState, Body, ContextUse, Entry, Footer, Message, Plan, Question,
    QuestionState, Reasoning, Tool, Transcript, TurnDiff, TurnOutcome, TurnUsage,
};
