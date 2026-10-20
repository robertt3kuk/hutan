//! `hutan integration install | uninstall | status <agent> --mcp` against temporary agent
//! configuration: Hutan's marked entry goes in and out, everything else stays, the file is
//! backed up first, a foreign entry is never overwritten, and without `--mcp` no MCP entry is
//! written. Every command gets `--dir` and a temporary `HOME`: nothing here can reach the
//! user's own agents.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{HUTAN, Out, command, finish};
use serde_json::{Value, json};
use tempfile::TempDir;

async fn integration(verb: &str, agent: &str, dir: &Path, extra: &[&str]) -> Out {
    let mut args = vec!["integration", verb, agent, "--dir", dir.to_str().unwrap()];
    args.extend_from_slice(extra);
    args.push("--json");
    let mut command = command(Path::new(HUTAN), None, &args);
    command.env("HOME", dir);
    finish(command).await
}

fn backups(dir: &Path, prefix: &str) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(prefix)
        })
        .count()
}

/// Where each agent keeps its MCP servers inside `root` (the agent's `--dir`).
struct Agent {
    name: &'static str,
    dir: PathBuf,
    config: PathBuf,
    toml: bool,
}

fn agents(root: &TempDir) -> Vec<Agent> {
    let codex = root.path().join("codex");
    let claude = root.path().join(".claude");
    let omp = root.path().join("omp");
    for dir in [&codex, &claude, &omp] {
        fs::create_dir_all(dir).unwrap();
    }
    vec![
        Agent {
            name: "codex",
            config: codex.join("config.toml"),
            dir: codex,
            toml: true,
        },
        Agent {
            name: "claude",
            // `.claude.json` sits next to `.claude/`.
            config: root.path().join(".claude.json"),
            dir: claude,
            toml: false,
        },
        Agent {
            name: "omp",
            config: omp.join("mcp.json"),
            dir: omp,
            toml: false,
        },
    ]
}

fn has_our_entry(agent: &Agent) -> bool {
    let text = fs::read_to_string(&agent.config).unwrap_or_default();
    if agent.toml {
        let doc: toml_edit::DocumentMut = text.parse().unwrap_or_default();
        doc.get("mcp_servers")
            .and_then(|s| s.get("hutan"))
            .and_then(|e| e.get("env"))
            .and_then(|e| e.get("HUTAN_MCP_ENTRY"))
            .is_some()
    } else {
        let doc: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        doc.pointer("/mcpServers/hutan/env/HUTAN_MCP_ENTRY")
            .is_some()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn install_status_and_uninstall_touch_only_hutans_entry() {
    let root = tempfile::tempdir().unwrap();
    for agent in agents(&root) {
        let name = agent.name;
        // The agent's own configuration, with another MCP server in it.
        let theirs = if agent.toml {
            "model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"other-server\"\n".to_owned()
        } else {
            serde_json::to_string_pretty(&json!({
                "theme": "dark",
                "mcpServers": {"other": {"command": "other-server"}},
            }))
            .unwrap()
        };
        fs::write(&agent.config, &theirs).unwrap();

        let status = integration("status", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(status["status"], "not_installed", "{name}");

        let installed = integration("install", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(installed["status"], "installed", "{name}: {installed}");
        assert_eq!(installed["changed"], true, "{name}");
        assert!(has_our_entry(&agent), "{name}");
        let after = fs::read_to_string(&agent.config).unwrap();
        assert!(
            after.contains("other-server"),
            "{name}: their server survives"
        );
        assert!(after.contains("mcp"), "{name}: runs `hutan mcp`");
        let prefix = agent
            .config
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let backup_dir = agent.config.parent().unwrap();
        assert_eq!(
            backups(backup_dir, &format!("{prefix}.hutan-")),
            1,
            "{name}"
        );
        let backup = fs::read_dir(backup_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{prefix}.hutan-"))
            })
            .unwrap();
        assert_eq!(
            fs::read_to_string(backup).unwrap(),
            theirs,
            "{name}: the backup is the original"
        );
        // The hooks were not part of this.
        assert!(!agent.dir.join("hooks.json").exists(), "{name}");

        // Again: nothing changes, no further backup.
        let again = integration("install", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(again["changed"], false, "{name}");
        assert_eq!(
            backups(backup_dir, &format!("{prefix}.hutan-")),
            1,
            "{name}"
        );
        assert_eq!(fs::read_to_string(&agent.config).unwrap(), after, "{name}");

        let status = integration("status", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(status["status"], "installed", "{name}");

        let removed = integration("uninstall", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(removed["status"], "not_installed", "{name}");
        assert!(!has_our_entry(&agent), "{name}");
        let left = fs::read_to_string(&agent.config).unwrap();
        assert!(
            left.contains("other-server"),
            "{name}: their server survives"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_that_is_not_hutans_is_never_overwritten_or_removed() {
    let root = tempfile::tempdir().unwrap();
    for agent in agents(&root) {
        let name = agent.name;
        let foreign = if agent.toml {
            "[mcp_servers.hutan]\ncommand = \"somebody-else\"\n".to_owned()
        } else {
            serde_json::to_string_pretty(&json!({
                "mcpServers": {"hutan": {"command": "somebody-else"}},
            }))
            .unwrap()
        };
        fs::write(&agent.config, &foreign).unwrap();

        let refused = integration("install", name, &agent.dir, &["--mcp"]).await;
        assert_ne!(refused.code, 0, "{name}: {refused:?}");
        assert!(
            refused.stderr.contains("not Hutan's"),
            "{name}: {refused:?}"
        );
        assert_eq!(
            fs::read_to_string(&agent.config).unwrap(),
            foreign,
            "{name}"
        );

        let removed = integration("uninstall", name, &agent.dir, &["--mcp"])
            .await
            .ok()
            .json();
        assert_eq!(removed["changed"], false, "{name}");
        assert_eq!(
            fs::read_to_string(&agent.config).unwrap(),
            foreign,
            "{name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_mcp_no_server_entry_is_written() {
    let root = tempfile::tempdir().unwrap();
    for agent in agents(&root) {
        integration("install", agent.name, &agent.dir, &[])
            .await
            .ok();
        assert!(!has_our_entry(&agent), "{}", agent.name);
        assert!(
            !agent.config.exists(),
            "{}: {}",
            agent.name,
            agent.config.display()
        );
        integration("uninstall", agent.name, &agent.dir, &[])
            .await
            .ok();
    }
}
