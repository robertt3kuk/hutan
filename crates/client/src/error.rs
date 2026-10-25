use std::io;

use hutan_protocol::{ErrorBody, ProtocolError};

use crate::start_failure::StartFailure;

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
    /// An autostarted `hutand` exited before its socket answered, or did not answer in time;
    /// carries the daemon's start-error record or what it printed.
    #[error("{0}")]
    DaemonStart(Box<StartFailure>),
    /// A host reached over SSH could not be reached or did not speak the protocol; the text
    /// says which host and what `ssh` or the host's shell reported.
    #[error("{0}")]
    Ssh(String),
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

    /// The version a refused `fs.write` found on disk (`conflict` with `data.current`); `None`
    /// for any other error, and when the file is gone.
    pub fn conflict_current(&self) -> Option<hutan_protocol::fs::FileVersion> {
        let Self::Remote(error) = self else {
            return None;
        };
        if error.code != hutan_protocol::ErrorCode::Conflict {
            return None;
        }
        serde_json::from_value(error.data.as_ref()?.get("current")?.clone()).ok()
    }
}
