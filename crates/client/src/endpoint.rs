use std::path::{Path, PathBuf};

use crate::socket::default_socket_path;

/// Where a daemon is reached (ADR 0022, slice R-1).
///
/// Only the local Unix socket exists today. A daemon on another machine will be one more variant
/// (reached over a byte stream the client does not open itself, see
/// [`Client::connect_stream`](crate::Client::connect_stream)); matching on an endpoint is how the
/// dialer decides what it may do, and **only [`Endpoint::Local`] may start `hutand`** (autostart
/// spawns a process on this machine, which is meaningless for any other endpoint).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Endpoint {
    /// The `hutand` of this machine, listening on this Unix socket.
    Local(PathBuf),
}

impl Endpoint {
    /// The local daemon at its default socket (`HUTAN_SOCKET`, else `$HUTAN_HOME/hutand.sock`,
    /// else the standard location); `None` when none of them can be determined.
    pub fn default_local() -> Option<Self> {
        default_socket_path().map(Self::Local)
    }

    /// The Unix socket of a local endpoint.
    pub fn local_socket(&self) -> Option<&Path> {
        match self {
            Self::Local(socket) => Some(socket),
        }
    }

    /// Whether a missing daemon behind this endpoint may be started by the client.
    pub fn may_autostart(&self) -> bool {
        match self {
            Self::Local(_) => true,
        }
    }
}

impl From<PathBuf> for Endpoint {
    fn from(socket: PathBuf) -> Self {
        Self::Local(socket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoint_carries_its_socket_and_may_autostart() {
        let endpoint = Endpoint::from(PathBuf::from("/tmp/h.sock"));
        assert_eq!(endpoint.local_socket(), Some(Path::new("/tmp/h.sock")));
        assert!(endpoint.may_autostart());
    }
}
