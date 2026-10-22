//! `hutan lsp ls|status|restart|stop|trust` and `hutan diagnostics` (protocol 0.39,
//! `docs/lsp.md`): the language servers of the daemon, and what they say about a file.
//!
//! `hutan diagnostics FILE` needs no window: it reads the file through the daemon, opens it as a
//! document (`doc.open`), waits for the server to start and publish, prints the diagnostics and
//! closes the document again.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::Client;
use hutan_protocol::fs::FsKind;
use hutan_protocol::lsp::{
    LspDiagnosticsRequest, LspServerInfo, LspSeverity, LspStatus, LspStatusRequest, LspTrust,
    LspTrustRequest,
};

use super::Ctx;
use crate::args::{DiagnosticsArgs, LspArgs, LspCommand, LspTrustArgs};
use crate::diag::{self, Collected};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn lsp(&self, args: LspArgs) -> Result<(), CliError> {
        let client = self.lsp_client().await?;
        match args.command.unwrap_or(LspCommand::Ls) {
            LspCommand::Ls => self.lsp_ls(&client, false).await,
            LspCommand::Status => self.lsp_ls(&client, true).await,
            LspCommand::Restart(args) => {
                let info = client
                    .lsp_restart(&args.server)
                    .await
                    .map_err(|e| self.fail(e))?;
                self.print(&info, || {
                    format!(
                        "{} {}: {}",
                        info.name,
                        info.root.display(),
                        state_line(&info)
                    )
                })
            }
            LspCommand::Stop(args) => {
                let info = client
                    .lsp_stop(&args.server)
                    .await
                    .map_err(|e| self.fail(e))?;
                self.print(&info, || {
                    format!(
                        "{} {}: {}",
                        info.name,
                        info.root.display(),
                        state_line(&info)
                    )
                })
            }
            LspCommand::Trust(args) => self.lsp_trust(&client, args).await,
        }
    }

    async fn lsp_client(&self) -> Result<Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_lsp() || !client.supports_doc_sync() {
            return Err(CliError::Failed(
                "this hutand has no language servers (it is older than protocol 0.39); restart it \
                 with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    async fn lsp_ls(&self, client: &Client, long: bool) -> Result<(), CliError> {
        let status = client
            .lsp_status(LspStatusRequest::default())
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&status, || render_status(&status, long))
    }

    async fn lsp_trust(&self, client: &Client, args: LspTrustArgs) -> Result<(), CliError> {
        let folder = match args.folder {
            Some(folder) => absolute(&folder)?,
            None => std::env::current_dir()?,
        };
        let decision = if args.deny {
            LspTrust::Denied
        } else if args.forget {
            LspTrust::Ask
        } else {
            LspTrust::Trusted
        };
        let result = client
            .lsp_trust(LspTrustRequest { folder, decision })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&result, || {
            let mut text = match result.decision {
                LspTrust::Trusted => format!(
                    "trusted {}: language servers may start there and run the project's build \
                     scripts and procedural macros (rust-analyzer does not sandbox them)",
                    result.folder.display()
                ),
                LspTrust::Denied => format!(
                    "denied {}: no language server will start there",
                    result.folder.display()
                ),
                LspTrust::Ask => format!(
                    "forgot the decision about {}: it is asked about again",
                    result.folder.display()
                ),
            };
            for server in &result.servers {
                text.push_str(&format!(
                    "\n  {} {}: {}",
                    server.name,
                    server.root.display(),
                    state_line(server)
                ));
            }
            text
        })
    }

    pub(super) async fn diagnostics(&self, args: DiagnosticsArgs) -> Result<(), CliError> {
        let client = self.lsp_client().await?;
        let min_severity = args.severity.as_deref().map(parse_severity).transpose()?;
        let target = match &args.path {
            Some(path) => absolute(path)?,
            None => std::env::current_dir()?,
        };
        let entry = client
            .fs_stat(&target, true)
            .await
            .map_err(|e| self.fail(e))?;
        let target = entry.path.clone();
        let collected = if entry.kind == FsKind::File {
            diag::collect(
                &client,
                std::slice::from_ref(&target),
                min_severity,
                Duration::from_secs(args.wait),
            )
            .await
            .map_err(|e| self.fail(e))?
        } else {
            let result = client
                .lsp_diagnostics(LspDiagnosticsRequest {
                    min_severity,
                    ..LspDiagnosticsRequest::default()
                })
                .await
                .map_err(|e| self.fail(e))?;
            Collected::of_folder(&target, result)
        };
        let mut json = collected.json();
        json["path"] = serde_json::json!(target);
        self.print(&json, || collected.render())
    }
}

/// `Ready`, or what is wrong, in a line.
fn state_line(info: &LspServerInfo) -> String {
    let mut text = info.state.as_str().to_owned();
    if let Some(reason) = &info.reason {
        text.push_str(&format!(" ({reason})"));
    }
    if let Some(install) = &info.install {
        text.push_str(&format!("; install: {install}"));
    }
    if let Some(setting) = &info.setting {
        text.push_str(&format!("; or set {setting} in daemon.toml"));
    }
    text
}

fn mem_text(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB * KIB {
        format!("{:.0}K", value / KIB)
    } else if value < KIB * KIB * KIB {
        format!("{:.1}M", value / (KIB * KIB))
    } else {
        format!("{:.2}G", value / (KIB * KIB * KIB))
    }
}

fn render_status(status: &LspStatus, long: bool) -> String {
    let mut out = String::new();
    if !status.enabled {
        out.push_str("language servers are off ([lsp] enabled = false in daemon.toml)\n");
    }
    if status.servers.is_empty() {
        out.push_str("no language server runs\n");
    } else {
        out.push_str("STATE          SERVER                      DOCS      RSS  ROOT\n");
    }
    for server in &status.servers {
        out.push_str(&format!(
            "{:<14} {:<27} {:>4} {:>8}  {}\n",
            server.state.as_str(),
            server.name,
            server.documents,
            server.rss_bytes.map_or_else(|| "-".to_owned(), mem_text),
            server.root.display()
        ));
        let mut note = Vec::new();
        if server.trust_pending {
            note.push(format!(
                "not trusted yet: `hutan lsp trust {}` lets it run the project's build scripts",
                server.root.display()
            ));
        } else if let Some(reason) = &server.reason {
            note.push(reason.clone());
        }
        if let Some(install) = &server.install {
            note.push(format!("install: {install}"));
        }
        if let Some(setting) = &server.setting {
            note.push(format!("or set {setting} in daemon.toml"));
        }
        if server.restarts > 0 {
            note.push(format!("restarted {} time(s)", server.restarts));
        }
        if !note.is_empty() {
            out.push_str(&format!("{:>14} {}\n", "", note.join("; ")));
        }
        if long {
            out.push_str(&format!("{:>14} id {}\n", "", server.server_id));
            if let Some(pid) = server.pid {
                out.push_str(&format!("{:>14} pid {pid}\n", ""));
            }
            if !server.capabilities.is_empty() {
                out.push_str(&format!(
                    "{:>14} can {}\n",
                    "",
                    server.capabilities.join(", ")
                ));
            }
        }
    }
    if long {
        out.push_str(&format!(
            "\ntrust default: {}, at most {} server(s)\n",
            serde_json::to_value(status.trust_policy)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            status.max_servers
        ));
        if status.trust.is_empty() {
            out.push_str("no folder decided yet\n");
        }
        for entry in &status.trust {
            out.push_str(&format!(
                "{:<8} {}  (by {})\n",
                entry.decision.as_str(),
                entry.folder.display(),
                entry.decided_by
            ));
        }
    }
    out.trim_end().to_owned()
}

fn parse_severity(text: &str) -> Result<LspSeverity, CliError> {
    match text.to_ascii_lowercase().as_str() {
        "error" => Ok(LspSeverity::Error),
        "warning" | "warn" => Ok(LspSeverity::Warning),
        "info" | "information" => Ok(LspSeverity::Information),
        "hint" => Ok(LspSeverity::Hint),
        other => Err(CliError::Failed(format!(
            "`{other}` is not a severity: error, warning, info or hint"
        ))),
    }
}

fn absolute(path: &Path) -> Result<PathBuf, CliError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use hutan_protocol::lsp::{LspServerState, LspTrustEntry, LspTrustPolicy};

    use super::*;

    fn info(state: LspServerState) -> LspServerInfo {
        LspServerInfo {
            server_id: "rust-analyzer:/repo".into(),
            name: "rust-analyzer".into(),
            root: "/repo".into(),
            languages: vec!["rust".into()],
            state,
            reason: None,
            pid: None,
            restarts: 0,
            rss_bytes: None,
            documents: 2,
            install: None,
            setting: None,
            trust_pending: false,
            capabilities: vec![],
            completion_triggers: vec![],
            signature_triggers: vec![],
        }
    }

    #[test]
    fn the_status_says_what_to_do_about_each_server_that_cannot_run() {
        let mut ready = info(LspServerState::Ready);
        ready.pid = Some(7);
        ready.rss_bytes = Some(1_468_006_400);
        let mut untrusted = info(LspServerState::Untrusted);
        untrusted.root = "/other".into();
        untrusted.trust_pending = true;
        let mut missing = info(LspServerState::NotInstalled);
        missing.root = "/third".into();
        missing.install = Some("rustup component add rust-analyzer".into());
        missing.setting = Some("lsp.servers.rust-analyzer.command".into());
        let status = LspStatus {
            enabled: true,
            trust_policy: LspTrustPolicy::Ask,
            max_servers: 4,
            servers: vec![ready, untrusted, missing],
            trust: vec![LspTrustEntry {
                folder: "/repo".into(),
                decision: LspTrust::Trusted,
                decided_at_ms: 1,
                decided_by: "cli".into(),
            }],
        };
        let text = render_status(&status, true);
        assert!(text.contains("ready"), "{text}");
        assert!(text.contains("1.37G"), "{text}");
        assert!(text.contains("`hutan lsp trust /other`"), "{text}");
        assert!(
            text.contains("install: rustup component add rust-analyzer; or set lsp.servers.rust-analyzer.command in daemon.toml"),
            "{text}"
        );
        assert!(text.contains("trust default: ask, at most 4 server(s)"));
        assert!(text.contains("trusted  /repo  (by cli)"), "{text}");
        let short = render_status(&status, false);
        assert!(!short.contains("trust default"));
    }
}
