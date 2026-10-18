use std::io;
use std::path::PathBuf;

use hutan_protocol::{ErrorBody, ProtocolError};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// The daemon answered with an error response.
    #[error("daemon error {:?}: {}", .0.code, .0.message)]
    Remote(ErrorBody),
    /// The connection closed before (or while) the call completed.
    #[error("disconnected from the daemon")]
    Disconnected,
    #[error("handshake failed: {0}")]
    Handshake(String),
    #[error("hutand did not start accepting connections on {} in time", .socket.display())]
    AutostartTimeout { socket: PathBuf },
    #[error("cannot determine the hutand socket path; set HUTAN_SOCKET or HUTAN_HOME")]
    NoSocketPath,
}

impl ClientError {
    /// What runs in the session a `session.close` was refused for (`invalid_state` with
    /// `data.busy`); `None` for any other error.
    pub fn busy(&self) -> Option<hutan_protocol::messages::SessionBusy> {
        let Self::Remote(error) = self else {
            return None;
        };
        if error.code != hutan_protocol::ErrorCode::InvalidState {
            return None;
        }
        serde_json::from_value(error.data.as_ref()?.get("busy")?.clone()).ok()
    }
}
