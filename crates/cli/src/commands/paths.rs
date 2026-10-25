//! Which directory "here" is, and how a path is made absolute, for commands that send a path to
//! the daemon. The daemon resolves paths from its own working directory, so a client gives it
//! absolute ones. For a daemon on another machine (`--host`) the current directory here is not
//! a directory there: a command that would default to it, or resolve a relative path against
//! it, asks for an absolute path on the host instead.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::CliError;

/// The name of the `--host` this run talks to, set once at startup.
static REMOTE_HOST: OnceLock<String> = OnceLock::new();

/// Records that this run talks to the host `name`.
pub(super) fn set_remote(name: &str) {
    // A second call (the process runs one command) keeps the first.
    let _ = REMOTE_HOST.set(name.to_owned());
}

/// Whether this run talks to a daemon on another machine.
pub(super) fn is_remote() -> bool {
    REMOTE_HOST.get().is_some()
}

fn not_here(host: &str) -> CliError {
    CliError::Usage(format!(
        "this command would use the current directory, which is not a directory on host \
         `{host}`: give an absolute path on the host"
    ))
}

/// The current directory, for the local daemon.
pub(super) fn here() -> Result<PathBuf, CliError> {
    match REMOTE_HOST.get() {
        Some(host) => Err(not_here(host)),
        None => Ok(std::env::current_dir()?),
    }
}

/// `path` as the daemon wants it: as given when absolute, else relative to [`here`].
pub(super) fn absolute(path: impl AsRef<Path>) -> Result<PathBuf, CliError> {
    let path = path.as_ref();
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    Ok(here()?.join(path))
}
