//! `hutan integration install | uninstall | status`: local file operations on the agents'
//! configuration, no daemon involved. With `--mcp` they manage the `hutan mcp` server entry
//! instead of the hooks.

use super::print_line;
use crate::args::IntegrationCommand;
use crate::error::CliError;
use crate::integration;

pub(super) fn run(json: bool, command: IntegrationCommand) -> Result<(), CliError> {
    let (action, args) = match command {
        IntegrationCommand::Install(args) => ("install", args),
        IntegrationCommand::Uninstall(args) => ("uninstall", args),
        IntegrationCommand::Status(args) => ("status", args),
    };
    let dir = integration::config_dir(args.target, args.dir.as_deref())?;
    let report = if args.mcp {
        match action {
            "install" => {
                let bin = std::env::current_exe()?;
                integration::mcp::install(args.target, &dir, &bin)?
            }
            "uninstall" => integration::mcp::uninstall(args.target, &dir)?,
            _ => integration::mcp::status(args.target, &dir)?,
        }
    } else {
        match action {
            "install" => {
                let port = if args.telemetry {
                    Some(integration::telemetry::persisted_port().ok_or_else(|| {
                        let agent = integration::name(args.target);
                        CliError::Failed(format!(
                            "the receiver has no port yet: run `hutan telemetry enable {agent}` \
                             first (it picks a port once and keeps it in daemon.toml), then \
                             install again"
                        ))
                    })?)
                } else {
                    None
                };
                integration::install(args.target, &dir, args.statusline, port)?
            }
            "uninstall" => integration::uninstall(args.target, &dir)?,
            _ => integration::status(args.target, &dir)?,
        }
    };
    if json {
        let text = serde_json::to_string(&report)
            .map_err(|e| CliError::Failed(format!("encoding the report: {e}")))?;
        print_line(&text)
    } else {
        print_line(&integration::describe(action, &report))
    }
}
