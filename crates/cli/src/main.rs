//! `hutan`: command-line client for hutand.

mod args;
mod attach_spec;
mod commands;
mod diag;
mod error;
mod hook;
mod integration;
mod mcp;
mod tail;

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;

use crate::args::Cli;

fn main() -> ExitCode {
    // clap exits with 2 on usage errors and 0 for --help/--version.
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("hutan: starting the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(commands::run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !error.is_broken_pipe() && !matches!(error, error::CliError::Reported) {
                // Not `eprintln!`: that panics when stderr is gone (attach after a hangup).
                let _ = writeln!(std::io::stderr(), "hutan: {error}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}
