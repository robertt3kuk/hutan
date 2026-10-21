//! Language servers in the daemon (protocol 0.39, `docs/lsp.md`).
//!
//! - [`catalog`]: the servers Hutan knows (built in, merged with `[lsp.servers.<id>]`) and the
//!   language of a file name. Servers are found on the login `PATH`, never fetched.
//! - [`roots`]: the topmost root marker above a file, within its project, with a per-project
//!   override.
//! - [`trust`]: what the user decided about folders; a server never starts in an untrusted one.
//! - [`convert`]: positions and diagnostics between a server's encoding and the wire.
//! - [`hub`]: [`LspHub`], one server per (server, root) shared by every client, the mirrors of
//!   the open documents, idle stop, `max_servers`, restart and the requests of the protocol.

mod catalog;
mod convert;
mod hub;
mod roots;
mod trust;

pub use hub::{Inputs, LspHub, LspTuning};
