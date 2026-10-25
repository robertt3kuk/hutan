//! Async client for the hutand protocol (ADR 0002).
//!
//! Two layers:
//! * [`Client`] is one connection: [`Client::connect`] dials the daemon (optionally spawning
//!   it first), performs the handshake, and starts a reader task that matches responses to
//!   calls and fans events and terminal frames out to subscribers. It does not reconnect.
//! * [`Connection`] is what UIs use: it owns a runtime thread, reconnects with backoff, keeps a
//!   [`StoreState`] of workspaces and sessions current (replay or snapshot resync), reports
//!   transient [`Notice`]s, and hands out [`TerminalHandle`]s that survive reconnects. All of
//!   its methods work from any executor or plain thread.

mod blocks;
mod chat;
mod client;
mod connection;
mod endpoint;
mod error;
mod fs;
pub mod identity;
mod lsp;
pub mod recovery;
mod socket;
pub mod start_failure;
mod store;
mod supervisor;
mod terminal;
pub mod time;

pub use blocks::BlockList;
pub use chat::{ChatUpdate, CheckpointUpdate};
pub use client::{AUTOSTART_GIVE_UP, Client, ConnectOptions, Resumed};
pub use connection::Connection;
pub use endpoint::Endpoint;
pub use error::ClientError;
pub use fs::FsText;
pub use identity::{DaemonIdentity, Mismatch};
pub use socket::default_socket_path;
pub use start_failure::{
    DaemonFiles, MovedData, ResetError, StartErrorKind, StartErrorRecord, StartFailure,
    StartOutcome, move_data_aside,
};
pub use store::{ConnectionState, Notice, StoreState};
pub use terminal::{GoneReason, TerminalHandle, TerminalUpdate};
