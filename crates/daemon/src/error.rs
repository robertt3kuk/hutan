use std::io;
use std::path::PathBuf;

use hutan_storage::StorageError;

use crate::paths::PathsError;

/// Longest `sun_path` that fits `sockaddr_un` on macOS (104 bytes including the NUL).
pub const MAX_SOCKET_PATH_BYTES: usize = 103;

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("another hutand is already running (lock {} is held)", lock.display())]
    AlreadyRunning { lock: PathBuf },
    #[error(
        "socket path {} is {len} bytes; the limit is {MAX_SOCKET_PATH_BYTES}",
        path.display()
    )]
    SocketPathTooLong { path: PathBuf, len: usize },
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    /// The daemon refuses to run without its database: continuing would lose sessions.
    #[error("cannot use the database {}: {source}", path.display())]
    Storage {
        path: PathBuf,
        #[source]
        source: Box<StorageError>,
    },
    #[error("invalid daemon config {}: {source}", path.display())]
    Config {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("cannot set up the model gateway: {0}")]
    Gateway(String),
    #[error("cannot restore the tasks: {0}")]
    Tasks(String),
    #[error("invalid language server settings: {0}")]
    Lsp(String),
    #[error("daemon task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

impl DaemonError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    pub(crate) fn storage(path: impl Into<PathBuf>) -> impl FnOnce(StorageError) -> Self {
        let path = path.into();
        move |source| Self::Storage {
            path,
            source: Box::new(source),
        }
    }
}
