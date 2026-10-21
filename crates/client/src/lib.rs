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
mod error;
mod fs;
pub mod recovery;
mod socket;
mod store;
mod supervisor;
mod terminal;
pub mod time;

pub use blocks::BlockList;
pub use chat::ChatUpdate;
pub use client::{Client, ConnectOptions, Resumed};
pub use connection::Connection;
pub use error::ClientError;
pub use fs::FsText;
pub use socket::default_socket_path;
pub use store::{ConnectionState, Notice, StoreState};
pub use terminal::{GoneReason, TerminalHandle, TerminalUpdate};
