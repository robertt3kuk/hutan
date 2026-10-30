//! `hutan mcp` as an MCP server of the agents Hutan starts (ADR 0016, protocol 0.21).
//!
//! The chat manager owns one [`McpWiring`] (the `hutan` executable, the daemon's socket, and
//! `[control] mcp`) and hands [`McpWiring::server_for`] to whatever starts an agent: Codex gets
//! it as `-c mcp_servers.hutan…` overrides ([`McpServerSpec::codex_overrides`]); the ACP path of
//! The ACP harness path puts [`McpServerSpec::acp_server`] into its `mcpServers` list. The server runs inside the agent's
//! session, so it carries `HUTAN_SESSION_ID` and its requests are agent-originated.

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hutan_core::SessionId;
use tokio::sync::OnceCell;
use tracing::warn;

/// The name the server is registered under in the agent's configuration.
pub const SERVER_NAME: &str = "hutan";

/// How to start one MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// How long `hutan mcp --read-only-tools` may take.
const READS_TIMEOUT: Duration = Duration::from_secs(10);

/// What the daemon needs to describe its own MCP server.
#[derive(Debug, Clone)]
pub struct McpWiring {
    /// `[control] mcp`.
    pub enabled: bool,
    /// The `hutan` CLI (the one sessions get as `HUTAN_BIN`).
    pub hutan_bin: PathBuf,
    /// The daemon's own socket, so the server reaches this daemon whatever the environment.
    pub socket: PathBuf,
    /// A private folder (0700) under the data directory for the files a Mawas harness is
    /// launched with ([`Self::write_private`]); never the user's own harness configuration.
    pub private_dir: PathBuf,
    /// The tools of `hutan mcp` that only read, asked of the CLI once (a failure is not kept).
    reads: Arc<OnceCell<Vec<String>>>,
}

/// Whether `name` is a tool name as the table writes them: it goes into a harness's
/// configuration as a key, so nothing else is accepted.
fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Runs `hutan mcp --read-only-tools`: the table of `hutan mcp` is the one list of which tools
/// only read (`readOnlyHint`), and the CLI is what owns it.
async fn list_read_only_tools(hutan_bin: &Path) -> Result<Vec<String>, String> {
    let mut command = tokio::process::Command::new(hutan_bin);
    command
        .args(["mcp", "--read-only-tools"])
        .env_remove("HUTAN_SESSION_ID")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(READS_TIMEOUT, command.output())
        .await
        .map_err(|_| "it did not answer in time".to_owned())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("it exited with {}", output.status));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "its answer is not UTF-8")?;
    let names: Vec<String> = text.lines().map(str::to_owned).collect();
    if names.is_empty() {
        return Err("it listed no tool".to_owned());
    }
    if let Some(bad) = names.iter().find(|name| !is_tool_name(name)) {
        return Err(format!("it listed `{bad}`, which is not a tool name"));
    }
    Ok(names)
}
impl McpWiring {
    pub fn new(enabled: bool, hutan_bin: PathBuf, socket: PathBuf, private_dir: PathBuf) -> Self {
        Self {
            enabled,
            hutan_bin,
            socket,
            private_dir,
            reads: Arc::default(),
        }
    }

    /// The tools of `hutan mcp` that only read, as its own table lists them; empty (and logged)
    /// when the CLI cannot be asked, in which case nothing is pre-allowed and the harness keeps
    /// its own prompts.
    pub async fn read_only_tools(&self) -> Vec<String> {
        self.reads
            .get_or_try_init(|| list_read_only_tools(&self.hutan_bin))
            .await
            .cloned()
            .unwrap_or_else(|reason| {
                warn!(bin = %self.hutan_bin.display(), %reason, "could not list the read-only Hutan tools; the harness keeps asking about them");
                Vec::new()
            })
    }

    /// Writes `content` to `file` in [`Self::private_dir`] (0700; the file 0600, replaced
    /// atomically), and returns its path.
    pub fn write_private(&self, file: &str, content: &str) -> std::io::Result<PathBuf> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.private_dir)?;
        let path = self.private_dir.join(file);
        let tmp = self
            .private_dir
            .join(format!(".{file}.{}.tmp", std::process::id()));
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        out.write_all(content.as_bytes())?;
        drop(out);
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The server for the agent of `session`; `None` under `[control] mcp = false`.
    pub fn server_for(&self, session: SessionId) -> Option<McpServerSpec> {
        self.enabled.then(|| McpServerSpec {
            name: SERVER_NAME.to_owned(),
            command: self.hutan_bin.clone(),
            args: vec!["mcp".to_owned()],
            env: vec![
                ("HUTAN_SESSION_ID".to_owned(), session.to_string()),
                (
                    "HUTAN_SOCKET".to_owned(),
                    self.socket.to_string_lossy().into_owned(),
                ),
            ],
        })
    }
}

impl McpServerSpec {
    /// The ACP `McpServer` object (stdio form) for `mcpServers` of `session/new` and
    /// `session/load`: `env` is a list of `{name, value}`.
    pub fn acp_server(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "command": self.command.to_string_lossy(),
            "args": self.args,
            "env": self
                .env
                .iter()
                .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
                .collect::<Vec<_>>(),
        })
    }

    /// Arguments for `codex app-server` that register this server for the session
    /// (`-c mcp_servers.<name>.command=…`, `.args`, `.env`); values are TOML.
    pub fn codex_overrides(&self) -> Vec<String> {
        let key = |field: &str| format!("mcp_servers.{}.{field}", self.name);
        let command = toml::Value::String(self.command.to_string_lossy().into_owned());
        let args = toml::Value::Array(self.args.iter().cloned().map(toml::Value::String).collect());
        let env = toml::Value::Table(
            self.env
                .iter()
                .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
                .collect(),
        );
        let mut out = Vec::new();
        for (field, value) in [("command", command), ("args", args), ("env", env)] {
            out.push("-c".to_owned());
            out.push(format!("{}={}", key(field), value));
        }
        out
    }
}

/// The OMP overlay (`omp --config <file>`) that lets `names` run without asking:
/// `tools.approval.<tool>: allow`, honoured in every approval mode and for this process only.
/// OMP names an MCP tool `mcp__<server>_<tool>`.
pub fn omp_overlay(names: &[String]) -> String {
    let mut out = String::from(
        "# Written by Hutan for Mawas's OMP: the read-only tools of Hutan's MCP server run without\n\
         # asking (docs/control.md, Reads are free). Replaced at every start.\n\
         tools:\n  approval:\n",
    );
    for name in names {
        out.push_str(&format!("    mcp__{SERVER_NAME}_{name}: allow\n"));
    }
    out
}

/// What Claude Code's `allowedTools` (SDK option, through `_meta.claudeCode.options` of
/// `session/new` and `session/load`) takes to run `names` without asking:
/// `mcp__<server>__<tool>`.
pub fn claude_allowed_tools(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| format!("mcp__{SERVER_NAME}__{name}"))
        .collect()
}

/// Codex overrides (`-c key=value`) that approve `names` without asking:
/// `mcp_servers.<server>.tools.<tool>.approval_mode = "approve"`.
pub fn codex_read_overrides(names: &[String]) -> Vec<String> {
    names
        .iter()
        .flat_map(|name| {
            [
                "-c".to_owned(),
                format!("mcp_servers.{SERVER_NAME}.tools.{name}.approval_mode=\"approve\""),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiring(enabled: bool) -> McpWiring {
        McpWiring::new(
            enabled,
            PathBuf::from("/Applications/Hutan.app/Contents/MacOS/hutan"),
            PathBuf::from("/tmp/h/hutand.sock"),
            PathBuf::from("/tmp/h/mcp"),
        )
    }

    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("hutan");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn names() -> Vec<String> {
        vec!["agents_board".to_owned(), "question_list".to_owned()]
    }

    #[test]
    fn each_harness_is_told_the_names_in_its_own_spelling() {
        let overlay = omp_overlay(&names());
        let body: Vec<&str> = overlay.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            body,
            [
                "tools:",
                "  approval:",
                "    mcp__hutan_agents_board: allow",
                "    mcp__hutan_question_list: allow",
            ]
        );
        assert_eq!(
            claude_allowed_tools(&names()),
            ["mcp__hutan__agents_board", "mcp__hutan__question_list"]
        );
        let codex = codex_read_overrides(&names());
        assert_eq!(codex.len(), 4);
        assert_eq!(codex[0], "-c");
        assert_eq!(
            codex[1],
            "mcp_servers.hutan.tools.agents_board.approval_mode=\"approve\""
        );
        for pair in codex.chunks(2) {
            let parsed: Result<toml::Table, _> = pair[1].parse();
            assert!(parsed.is_ok(), "{}: {parsed:?}", pair[1]);
        }
    }

    #[test]
    fn only_plain_tool_names_are_accepted() {
        assert!(is_tool_name("agents_board") && is_tool_name("lsp_status"));
        for bad in [
            "", "Agents", "a b", "a-b", "a: allow", "a\nb", "a.b", "../x",
        ] {
            assert!(!is_tool_name(bad), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn the_list_comes_from_the_cli_and_a_failure_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "printf 'agents_board\\nquestion_list\\n'");
        let w = McpWiring::new(true, bin.clone(), PathBuf::new(), dir.path().join("p"));
        assert_eq!(w.read_only_tools().await, names());
        // Garbage, a failing exit and an empty list all mean "nothing is pre-allowed" ...
        let failing = tempfile::tempdir().unwrap();
        let w = McpWiring::new(
            true,
            script(failing.path(), "printf 'a: allow\\n'"),
            PathBuf::new(),
            dir.path().join("p"),
        );
        assert!(w.read_only_tools().await.is_empty());
        // ... and the failure is not kept: the same wiring asks again once the CLI works.
        script(failing.path(), "printf 'agents_board\\n'");
        assert_eq!(w.read_only_tools().await, ["agents_board"]);
        for body in ["exit 3", "true"] {
            let dir = tempfile::tempdir().unwrap();
            let w = McpWiring::new(
                true,
                script(dir.path(), body),
                PathBuf::new(),
                dir.path().join("p"),
            );
            assert!(w.read_only_tools().await.is_empty(), "{body}");
        }
        let w = McpWiring::new(
            true,
            dir.path().join("missing"),
            PathBuf::new(),
            dir.path().join("p"),
        );
        assert!(w.read_only_tools().await.is_empty());
    }

    #[test]
    fn private_files_are_0600_in_a_0700_folder_and_replaced_whole() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let w = McpWiring::new(true, PathBuf::new(), PathBuf::new(), dir.path().join("mcp"));
        let path = w.write_private("a.yml", "one").unwrap();
        let again = w.write_private("a.yml", "two").unwrap();
        assert_eq!(path, again);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("mcp")), 0o700);
        assert_eq!(
            std::fs::read_dir(dir.path().join("mcp")).unwrap().count(),
            1
        );
    }

    #[test]
    fn a_disabled_wiring_names_no_server() {
        assert_eq!(wiring(false).server_for(SessionId::new()), None);
    }

    #[test]
    fn the_server_runs_hutan_mcp_inside_the_session() {
        let session = SessionId::new();
        let spec = wiring(true).server_for(session).unwrap();
        assert_eq!(spec.name, "hutan");
        assert_eq!(spec.args, ["mcp"]);
        assert!(
            spec.env
                .contains(&("HUTAN_SESSION_ID".to_owned(), session.to_string()))
        );
        assert!(
            spec.env
                .contains(&("HUTAN_SOCKET".to_owned(), "/tmp/h/hutand.sock".to_owned()))
        );
    }

    #[test]
    fn an_acp_harness_gets_the_stdio_object_with_the_sessions_environment() {
        let session = SessionId::new();
        let object = wiring(true).server_for(session).unwrap().acp_server();
        assert_eq!(object["name"], "hutan");
        assert_eq!(object["args"], serde_json::json!(["mcp"]));
        let env = object["env"].as_array().unwrap();
        assert!(
            env.iter()
                .any(|e| e["name"] == "HUTAN_SESSION_ID"
                    && e["value"] == session.to_string().as_str())
        );
    }

    #[test]
    fn codex_gets_toml_overrides() {
        let session = SessionId::new();
        let spec = wiring(true).server_for(session).unwrap();
        let args = spec.codex_overrides();
        assert_eq!(args.len(), 6);
        assert_eq!(args[0], "-c");
        assert_eq!(
            args[1],
            "mcp_servers.hutan.command=\"/Applications/Hutan.app/Contents/MacOS/hutan\""
        );
        assert_eq!(args[3], "mcp_servers.hutan.args=[\"mcp\"]");
        // Every override is valid TOML on its own.
        for pair in args.chunks(2) {
            let parsed: Result<toml::Table, _> = pair[1].parse();
            assert!(parsed.is_ok(), "{}: {parsed:?}", pair[1]);
        }
        assert!(args[5].starts_with("mcp_servers.hutan.env="));
        assert!(args[5].contains(&session.to_string()));
    }
}
