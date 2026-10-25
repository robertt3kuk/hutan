//! Who may talk to the daemon: the local Unix acceptor and the [`AuthenticatedPeer`] it produces.
//!
//! [`conn::serve`](crate::conn::serve) runs the protocol over any byte stream, so it must not
//! decide who is on the other end. That decision belongs to the acceptor that produced the
//! stream, and its result is a value, [`AuthenticatedPeer`], that `serve` requires: a stream
//! nobody authenticated cannot be served. The only acceptor is [`LocalAcceptor`] (a Unix socket
//! whose peer must be the daemon's own user, checked by the kernel's peer credentials before
//! the first byte is read). There is no TCP or other listener (`tests/it/no_network_listener.rs`
//! guards that); a future acceptor must produce an `AuthenticatedPeer` by its own means.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use tokio::net::{UnixListener, UnixStream};
use tracing::warn;

/// Proof that the acceptor of a stream authenticated its peer. Private field, no public
/// constructor: [`LocalAcceptor`] is the only producer (tests and the `test-support` feature
/// have [`unchecked_for_tests`](Self::unchecked_for_tests)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatedPeer {
    how: Authentication,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authentication {
    /// A Unix-socket peer whose credentials say it runs as this user, who is the daemon's.
    LocalUser { uid: u32 },
    /// A test stream (in-memory duplex, socket pair): nothing was checked.
    #[cfg(any(test, feature = "test-support"))]
    Unchecked,
}

impl AuthenticatedPeer {
    /// For tests that serve a stream that has no credentials (an in-memory duplex). Never
    /// reachable from the daemon binary.
    #[cfg(any(test, feature = "test-support"))]
    pub fn unchecked_for_tests() -> Self {
        Self {
            how: Authentication::Unchecked,
        }
    }

    /// The user id the acceptor verified, if it verified one (for logs).
    pub fn uid(&self) -> Option<u32> {
        match self.how {
            Authentication::LocalUser { uid } => Some(uid),
            #[cfg(any(test, feature = "test-support"))]
            Authentication::Unchecked => None,
        }
    }
}

/// Why a local connection is not served.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    #[error("peer runs as another user (uid {peer_uid})")]
    OtherUser { peer_uid: u32 },
    #[error("peer credentials unavailable: {0}")]
    NoCredentials(io::Error),
}

/// Checks the credentials of a Unix-socket peer against `daemon_uid`.
pub fn authenticate_local(
    stream: &UnixStream,
    daemon_uid: u32,
) -> Result<AuthenticatedPeer, Rejected> {
    match stream.peer_cred() {
        Ok(cred) if cred.uid() == daemon_uid => Ok(AuthenticatedPeer {
            how: Authentication::LocalUser { uid: cred.uid() },
        }),
        Ok(cred) => Err(Rejected::OtherUser {
            peer_uid: cred.uid(),
        }),
        Err(error) => Err(Rejected::NoCredentials(error)),
    }
}

/// The daemon's Unix-socket listener: accepts a connection only from the daemon's own user.
#[derive(Debug)]
pub struct LocalAcceptor {
    listener: UnixListener,
    daemon_uid: u32,
}

impl LocalAcceptor {
    /// Serves peers running as `daemon_uid`, the owner of the socket at `socket` (the daemon's
    /// own user: the socket was just created by this process).
    pub fn new(listener: UnixListener, socket: &Path) -> io::Result<Self> {
        let daemon_uid = std::fs::metadata(socket)?.uid();
        Ok(Self {
            listener,
            daemon_uid,
        })
    }

    /// The next connection. `Ok(None)`: a connection arrived and was rejected (logged and
    /// closed before one byte was read); the caller just accepts again.
    pub async fn accept(&self) -> io::Result<Option<(UnixStream, AuthenticatedPeer)>> {
        let (stream, _) = self.listener.accept().await?;
        match authenticate_local(&stream, self.daemon_uid) {
            Ok(peer) => Ok(Some((stream, peer))),
            Err(rejected) => {
                warn!(error = %rejected, "rejecting connection");
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own_uid() -> u32 {
        // The owner of a file this process just created.
        let dir = tempfile::tempdir().unwrap();
        std::fs::metadata(dir.path()).unwrap().uid()
    }

    #[tokio::test]
    async fn same_user_is_authenticated() {
        let (a, _b) = UnixStream::pair().unwrap();
        let peer = authenticate_local(&a, own_uid()).unwrap();
        assert_eq!(peer.uid(), Some(own_uid()));
    }

    #[tokio::test]
    async fn another_user_is_rejected_before_any_byte() {
        let (a, _b) = UnixStream::pair().unwrap();
        let other = own_uid().wrapping_add(1);
        match authenticate_local(&a, other) {
            Err(Rejected::OtherUser { peer_uid }) => assert_eq!(peer_uid, own_uid()),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acceptor_closes_a_rejected_connection_without_serving_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut acceptor = LocalAcceptor::new(listener, &socket).unwrap();
        // Pretend the daemon is someone else: the real peer (this user) is a stranger.
        acceptor.daemon_uid = own_uid().wrapping_add(1);

        let client = tokio::spawn({
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.unwrap() }
        });
        assert!(acceptor.accept().await.unwrap().is_none());
        drop(client.await.unwrap());
    }

    #[tokio::test]
    async fn acceptor_hands_over_a_same_user_connection_with_its_proof() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let acceptor = LocalAcceptor::new(listener, &socket).unwrap();

        let client = tokio::spawn({
            let socket = socket.clone();
            async move { UnixStream::connect(socket).await.unwrap() }
        });
        let (_stream, peer) = acceptor.accept().await.unwrap().unwrap();
        assert_eq!(peer.uid(), Some(own_uid()));
        drop(client.await.unwrap());
    }
}
