//! Agent adapters (ADR 0005, ADR 0015). One [`AgentAdapter`] trait, two kinds of adapter:
//!
//! - [`codex`]: `codex app-server` over stdio JSON-RPC, mapped to the unified
//!   [`hutan_core::AgentEvent`] model.
//! - [`acp`]: any agent that speaks the Agent Client Protocol v1 over stdio (`omp acp`, Claude
//!   Code's ACP adapter), mapped to the same model.
//! - [`adapter`]: the trait and the types around it.
//! - [`jsonrpc`]: newline-delimited JSON-RPC over a child process.
//! - [`fixture`]: record / scrub / replay tooling for Codex transcripts (the fake app server the
//!   tests run against), and the scripted fake ACP agent.
//!
//! Nothing here knows about sessions, storage or the wire protocol: the daemon owns those.

pub mod acp;
pub mod adapter;
pub mod codex;
pub mod elicit;
pub mod error;
pub mod fixture;
pub mod jsonrpc;
pub mod usage;

pub use acp::{AcpAdapter, AcpConfig, AcpSession};
pub use adapter::{
    AdapterMessage, AgentAdapter, ContextBlock, ExitInfo, ImageBlock, MessageExtras,
    ReportedSafety, SessionMode, StartSpec, Started, TurnRef,
};
pub use codex::{CodexAccount, CodexAppServerAdapter, CodexConfig, CodexSession};
pub use error::AgentError;
pub use usage::{RateLimits, RateWindow};
