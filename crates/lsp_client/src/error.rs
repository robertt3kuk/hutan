use std::io;

use crate::server::ServerState;

/// What can go wrong talking to a language server.
#[derive(Debug, thiserror::Error)]
pub enum LspError {
    /// The server's executable could not be started (missing, not executable). Never retried:
    /// the server is `failed` and the caller shows "not installed".
    #[error("cannot start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    /// No server process to talk to right now (starting, restarting, failed or stopped).
    #[error("the language server is not running ({state:?})")]
    NotRunning { state: ServerState },
    /// The process ended or closed its output while the request waited.
    #[error("the language server exited{}", detail_suffix(.detail))]
    Closed { detail: String },
    /// The request was not answered in time; `$/cancelRequest` was sent.
    #[error("`{method}` timed out")]
    Timeout { method: String },
    /// The server answered with a JSON-RPC error.
    #[error("`{method}` failed ({code}): {message}")]
    Rpc {
        method: String,
        code: i64,
        message: String,
    },
    /// The server did not announce the capability the request needs; nothing was sent.
    #[error("the language server does not support `{method}`")]
    Unsupported { method: String },
    /// A message did not have the shape its method promises.
    #[error("malformed `{method}` message: {source}")]
    Malformed {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    /// The server chose a position encoding the client did not offer.
    #[error("the language server chose the position encoding `{0}`, which was not offered")]
    Encoding(String),
    /// A message to the server is larger than a frame may be.
    #[error("a `{method}` message of {len} bytes is over the {max}-byte frame limit")]
    TooLarge {
        method: String,
        len: usize,
        max: usize,
    },
    /// Too many requests wait for an answer already.
    #[error("{0} requests are already waiting for the language server")]
    Busy(usize),
    /// The document is not open (or already open) in this client.
    #[error("{0}")]
    Document(String),
    /// Too many open documents or event subscribers.
    #[error("limit reached: {0}")]
    Limit(&'static str),
    /// A position or offset does not fit the text.
    #[error(transparent)]
    Position(#[from] hutan_buffer::BufferError),
}

fn detail_suffix(detail: &str) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}
