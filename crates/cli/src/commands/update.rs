//! `hutan update status|check|install`: whether a newer Hutan exists and updating it the way it
//! was installed (protocol 0.60, capability `update`, ADR 0024; `docs/packaging.md` § Updates).
//!
//! `status` contacts nothing. `check` is the one request to GitHub's releases that the user asks
//! for. `install` has the daemon run Homebrew in a visible terminal session; nothing here ever
//! runs `brew` itself.

use hutan_protocol::messages::Request;
use hutan_protocol::messages::{Empty, Event, Subscribed};
use hutan_protocol::update::{
    CheckOutcome, InstallMethod, InstallState, UpdateChannel, UpdateCheck, UpdateInstall,
    UpdateStatus,
};
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{UpdateCommand, UpdateInstallArgs};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn update(&self, command: UpdateCommand) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_update() {
            return Err(CliError::Failed(
                "this hutand does not know updates (it is older than protocol 0.60); restart it \
                 with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        match command {
            UpdateCommand::Status => {
                let status = client.update_status().await.map_err(|e| self.fail(e))?;
                self.print(&status, || status_text(&status, now_ms()))
            }
            UpdateCommand::Check => {
                let status = client.update_check().await.map_err(|e| self.fail(e))?;
                self.print(&status, || status_text(&status, now_ms()))
            }
            UpdateCommand::Install(args) => self.update_install(&client, args).await,
        }
    }

    async fn update_install(
        &self,
        client: &hutan_client::Client,
        args: UpdateInstallArgs,
    ) -> Result<(), CliError> {
        let status = client.update_status().await.map_err(|e| self.fail(e))?;
        // Subscribed before the install starts: its end cannot be missed.
        let mut events = args.wait.then(|| client.events());
        if events.is_some() {
            let _: Subscribed = self
                .call(client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let started = client
            .update_install(status.method)
            .await
            .map_err(|e| self.fail(e))?;
        let install = started.install.clone();
        let session = started.session.id;
        if !args.wait {
            return self.print(&started, || {
                format!(
                    "updating to Hutan {}: running `{}` in session {session}\n\
                     watch it with `hutan attach {session}`; `hutan update status` tells how it ended",
                    install.version, install.command
                )
            });
        }
        if !self.json {
            super::print_line(&format!(
                "updating to Hutan {}: running `{}` in session {session}",
                install.version, install.command
            ))?;
        }
        let Some(events) = events.as_mut() else {
            return Ok(());
        };
        let done = loop {
            match events.recv().await {
                Ok((_, Event::UpdateInstallChanged { install }))
                    if install.session_id == session && install.state != InstallState::Running =>
                {
                    break *install;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {
                    return Err(CliError::Unreachable(
                        "lost the connection to hutand".into(),
                    ));
                }
            }
        };
        self.print(&done, || install_text(&done))?;
        match done.state {
            InstallState::Succeeded => Ok(()),
            _ => Err(CliError::Reported),
        }
    }
}

fn install_text(install: &UpdateInstall) -> String {
    match install.state {
        InstallState::Succeeded => format!(
            "Hutan {} is installed; relaunch the app to use it (an idle daemon moves onto the \
             new build by itself)",
            install.version
        ),
        InstallState::Failed => match install.exit_code {
            Some(code) => format!(
                "the update to Hutan {} failed: `{}` exited with {code}",
                install.version, install.command
            ),
            None => format!(
                "the update to Hutan {} did not finish: `{}` was stopped",
                install.version, install.command
            ),
        },
        InstallState::Running => format!("updating to Hutan {}…", install.version),
    }
}

fn method_text(status: &UpdateStatus) -> String {
    let what = match status.method {
        InstallMethod::Cask => "Homebrew cask (robertt3kuk/hutan/hutan)",
        InstallMethod::Formula => "Homebrew formula (CLI and daemon only)",
        InstallMethod::Bundle => "a Hutan.app copied by hand",
        InstallMethod::Unknown => {
            "not a known install (a development build, or an unwritable folder)"
        }
    };
    match &status.install_path {
        Some(path) => format!("{what} at {path}"),
        None => what.to_owned(),
    }
}

fn status_text(status: &UpdateStatus, now_ms: u64) -> String {
    let mut lines = vec![
        format!("Hutan {}", status.version),
        format!("installed as {}", method_text(status)),
        format!(
            "checks: {}, channel: {}",
            match status.check {
                UpdateCheck::Auto =>
                    "automatic (at most every 24 h; `hutan config set updates.check off` turns it off)",
                UpdateCheck::Off => "only when asked (`hutan update check`)",
            },
            match status.channel {
                UpdateChannel::Prerelease => "prerelease (every release)",
                UpdateChannel::Stable => "stable (pre-releases are ignored)",
            }
        ),
    ];
    if let Some(check) = &status.last_check {
        let ago = age_text(now_ms.saturating_sub(check.at_ms) / 1000);
        lines.push(match (check.outcome, &check.message) {
            (CheckOutcome::Checked, _) => format!("last check: {ago} ago"),
            (_, Some(message)) => format!("last check: {ago} ago, could not finish: {message}"),
            (_, None) => format!("last check: {ago} ago, could not finish"),
        });
    }
    match &status.available {
        Some(release) => {
            let skipped = status.skipped.as_deref() == Some(release.version.as_str());
            lines.push(format!(
                "update available: Hutan {}{}{}",
                release.version,
                if release.prerelease {
                    " (pre-release)"
                } else {
                    ""
                },
                if skipped { " — skipped" } else { "" },
            ));
            lines.push(format!("  release notes: {}", release.notes_url));
            match (&status.command, status.method) {
                (Some(_), _) => lines.push("  update with: hutan update install".to_owned()),
                (None, InstallMethod::Bundle) => lines.push(format!(
                    "  download Hutan-{}-arm64.zip from the release page, or install Hutan with Homebrew",
                    release.version
                )),
                (None, _) => lines.push(
                    "  update it the way you installed it, or download the release from the page above"
                        .to_owned(),
                ),
            }
        }
        None if status.last_check.is_none() => {
            lines.push("no check yet (`hutan update check` asks now)".to_owned());
        }
        None => lines.push("up to date as far as the last check knows".to_owned()),
    }
    if let Some(install) = &status.install {
        lines.push(format!("install: {}", install_text(install)));
    }
    lines.join("\n")
}
