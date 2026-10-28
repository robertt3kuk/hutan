//! Hutan terminal: PTY ownership, the VT engine and render-state extraction (ADR 0004).
//!
//! The daemon is the only VT parser. [`Terminal`] owns a child process in a PTY and keeps the
//! canonical screen state; clients get [`hutan_protocol::render`] DTOs produced per client
//! through a [`RenderTracker`]. [`Emulator`] is the same engine without a PTY (corpus replay,
//! recorder, benchmarks). Engine types never appear in this crate's public API. [`procs`] reads
//! and signals single processes for the resource manager.

mod agent_status;
#[cfg(test)]
mod behavior;
#[cfg(test)]
mod block_tests;
mod blocks;
mod emulator;
pub mod env;
#[cfg(test)]
mod equivalence;
mod error;
mod login_env;
pub mod procs;
mod queries;
mod render;
mod scrollback;
#[cfg(test)]
mod search_tests;
pub mod shell_integration;
mod sniffer;
mod sys;
mod terminal;

pub use agent_status::{AgentPhase, AgentStatus, MAX_APP_LEN, MAX_KIND_LEN, MAX_MESSAGE_CHARS};
pub use blocks::{BlockText, BlockTextError};
pub use emulator::{
    DefaultColors, Emulator, LineScope, LogicalLine, LogicalLines, ScanFlow, ScanProgress,
    SearchLine,
};
pub use env::EnvInherit;
pub use error::TerminalError;
pub use render::RenderTracker;
pub use sniffer::{PromptMark, TerminalMeta};
pub use terminal::{ExitStatus, Terminal, TerminalConfig, default_shell};
