//! Hutan's Language Server Protocol client: runs one language server as a child process and
//! talks LSP 3.17 to it. The daemon's `LspHub` (E-11b) owns one [`Server`] per
//! `(server, workspace root)`; this crate knows nothing about the daemon, the protocol between
//! Hutan's processes, or GPUI.
//!
//! # Pieces
//! - [`codec`]: `Content-Length` framing with a frame size cap ([`MAX_FRAME_BYTES`]).
//! - `rpc`: JSON-RPC over the server's stdio — request/response correlation, per-request
//!   timeouts, cancellation with `$/cancelRequest` (on timeout or when the caller drops the
//!   request), answers to the server's own requests (`workspace/configuration`,
//!   `window/workDoneProgress/create`, `client/registerCapability`, …).
//! - [`Server`]: lifecycle (`initialize` → `initialized` … `shutdown` → `exit`, then a kill),
//!   the [`ServerState`] machine (`starting → indexing ⇄ ready → failed | stopped`, indexing
//!   while the server reports work-done `$/progress`), restarts with exponential backoff
//!   ([`RestartPolicy`]: N failures within a window → `failed`), document sync, typed
//!   requests ([`requests`]).
//! - Document sync ([`Server::open`], [`Server::change`], …) with **coalescing**: changes are
//!   collected for [`ServerConfig::coalesce`] (or until the next request) and leave as one
//!   `didChange` per document — one range replacement when the server syncs incrementally,
//!   the whole text otherwise. Documents are opened again after a restart.
//! - [`position`]: positions in the negotiated encoding (UTF-8 preferred, UTF-16 otherwise)
//!   ↔ byte offsets of a [`hutan_buffer::Text`].
//! - [`events`]: diagnostics (replaced per file, as `publishDiagnostics` says), progress and log
//!   messages, kept as bounded state and announced to subscribers.
//! - `process`: the server runs in its own process group; dropping the [`Server`] kills the
//!   group (rust-analyzer's proc-macro servers included); [`ServerStatus::pid`] is the root
//!   of the tree whose RSS Resources show.
//!
//! # Bounds and overflow policy
//! Nothing grows with what a server does:
//! - Frames: a message over [`MAX_FRAME_BYTES`] is skipped as it streams (never buffered);
//!   outgoing messages over it are refused.
//! - To the server: at most [`OUTGOING_QUEUE`] messages and [`OUTGOING_BYTES`] bytes queued.
//!   Senders wait for room within their deadline; a server that stops reading slows its
//!   callers down. Document changes are coalesced while they wait, so a stalled server costs
//!   one pending range per open document. `$/cancelRequest` is dropped when the queue is full.
//! - From the server: no queue. One reader handles a message at a time: responses complete
//!   their request (at most [`MAX_PENDING`] wait), server requests are answered (the answer
//!   waits for room, so the reader stops reading the server: back-pressure), notifications
//!   update bounded state (see [`events`]) and are offered to each subscriber's queue of
//!   [`EVENT_QUEUE`] events; a full queue drops the event and the subscriber is told
//!   [`ServerEvent::Lagged`] once it caught up, to re-read the state.
//! - stderr: the last 20 lines of at most 4 KiB each.
//!
//! # Types
//! [`ls_types`] (re-exported): the maintained fork of `lsp-types 0.97`, the same API save for
//! `ReferencesOptions` → `ReferenceOptions` and a few 3.18 additions.

pub mod codec;
mod error;
pub mod events;
pub mod position;
mod process;
pub mod requests;
mod rpc;
mod server;
mod sync;

pub use codec::MAX_FRAME_BYTES;
pub use error::LspError;
pub use events::{
    EVENT_QUEUE, FileDiagnostics, LogEntry, ProgressPhase, ServerEvent, Subscription,
};
pub use ls_types;
pub use position::PositionEncoding;
pub use process::Launch;
pub use requests::{Latency, TypedRequest};
pub use rpc::{MAX_PENDING, OUTGOING_BYTES, OUTGOING_QUEUE, QueueStats};
pub use server::{RestartPolicy, Server, ServerConfig, ServerState, ServerStatus, Timeouts};
pub use sync::MAX_DOCUMENTS;
