//! `hutan proxy`: the bridge `ssh HOST hutan proxy` runs on a host (ADR 0022).

use hutan_cli::proxy::{self, Ended};

use super::{Ctx, find_hutand};
use crate::error::CliError;

impl Ctx {
    /// Bridges standard input and output to this machine's daemon socket until either closes;
    /// a daemon that is not running is started first, like for any client of the host. Never
    /// prints to standard output (every byte there is protocol), and never returns: the
    /// blocking read of standard input would hold the runtime open after the bridge is done.
    pub(super) async fn proxy(&self) -> Result<(), CliError> {
        let hutand = find_hutand().ok();
        let stream =
            hutan_client::dial_local(&self.socket, hutand.as_deref(), self.socket_arg.is_some())
                .await
                .map_err(|e| self.fail(e))?;
        let code = match proxy::bridge(tokio::io::stdin(), tokio::io::stdout(), stream).await {
            Ok(Ended::Input | Ended::Daemon | Ended::Output) => 0,
            Err(error) => {
                eprintln!("hutan: proxy: {error}");
                1
            }
        };
        std::process::exit(code)
    }
}
