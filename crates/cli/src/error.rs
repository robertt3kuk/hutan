use std::io;
use std::path::Path;

use hutan_client::{ClientError, StartFailure};
use hutan_protocol::ErrorBody;

pub const EXIT_REMOTE: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_UNREACHABLE: u8 = 3;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    /// hutand is not running or went away.
    #[error("{0}")]
    Unreachable(String),
    #[error("{}: {}", code_name(.0), .0.message)]
    Remote(ErrorBody),
    /// The command already printed its error on stdout; exit 1 without another message.
    #[error("remote error")]
    Reported,
    #[error("{0}")]
    Failed(String),
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    /// `attach` was ended by a termination signal; exits with the shell's 128 + signal.
    #[error("terminated by {name}")]
    Signalled { name: &'static str, code: u8 },
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Unreachable(_) => EXIT_UNREACHABLE,
            Self::Signalled { code, .. } => *code,
            Self::Remote(_) | Self::Reported | Self::Failed(_) | Self::Io(_) => EXIT_REMOTE,
        }
    }

    /// A closed stdout (`hutan events | head`) is not worth a message.
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Io(e) if e.kind() == io::ErrorKind::BrokenPipe)
    }

    pub fn from_client(error: ClientError, socket: &Path) -> Self {
        match error {
            ClientError::Remote(body) => Self::Remote(body),
            ClientError::Io(e) if is_unreachable(&e) => Self::Unreachable(format!(
                "cannot connect to hutand at {} ({e}); start it with `hutan daemon start`",
                socket.display()
            )),
            ClientError::DaemonStart(failure) => Self::Unreachable(start_failure_text(&failure)),
            ClientError::Disconnected => {
                Self::Unreachable("lost the connection to hutand".to_owned())
            }
            ClientError::NoSocketPath => Self::Usage(ClientError::NoSocketPath.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }

    /// Like [`from_client`](Self::from_client) for a daemon on host `host` reached over SSH.
    pub fn from_host(error: ClientError, host: &str) -> Self {
        match error {
            ClientError::Remote(body) => Self::Remote(body),
            ClientError::Ssh(text) => Self::Unreachable(text),
            ClientError::Disconnected => {
                Self::Unreachable(format!("lost the connection to host `{host}`"))
            }
            other => Self::Failed(other.to_string()),
        }
    }
}

/// What `hutan` prints when the daemon it started did not come up: the sentence, what the
/// daemon recorded or printed, and where to look further.
pub fn start_failure_text(failure: &StartFailure) -> String {
    format!(
        "{}\nRun `hutan daemon why` for the record and the end of the daemon's log.",
        failure.details().trim_end()
    )
}

pub fn is_unreachable(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// The error code as it appears on the wire (`not_found`, ...).
fn code_name(body: &ErrorBody) -> String {
    serde_json::to_value(body.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}
