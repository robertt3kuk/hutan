//! Safety presets (protocol 0.51, capability `chat.safety`, ADR 0021 decision 1): one control
//! that picks an autonomy level and a boundary together, and the table that says what each
//! harness does for each preset.
//!
//! The table lives here, with no I/O, because the daemon (chat launch profiles) and the clients
//! (the flags of a terminal agent) both read it: there is one place to change when a harness
//! changes its vocabulary. Sources per harness: `docs/agents.md` § Safety presets.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::{ApprovalPolicy, SandboxMode};

/// The four things a person can ask for. What each one means depends on the harness, and a
/// harness that cannot honour one refuses it ([`plan`]); nothing is approximated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyPreset {
    /// The agent asks before it acts (Codex `on-request`, Claude Code `default`, OMP
    /// `always-ask`): the behaviour a chat has without a preset.
    Ask,
    /// The agent works without asking inside the harness's own sandbox: what the sandbox
    /// refuses fails instead of asking. Needs a harness with an OS sandbox (Codex, Claude Code).
    Contained,
    /// The agent reads and searches; it changes nothing.
    ReadOnly,
    /// The agent acts without asking and without a boundary (`danger-full-access`,
    /// `bypassPermissions`, `yolo`). Shown as a warning.
    Full,
}

impl SafetyPreset {
    /// Every preset, in the order the Start sheet shows them.
    pub const ALL: [SafetyPreset; 4] = [
        SafetyPreset::Ask,
        SafetyPreset::Contained,
        SafetyPreset::ReadOnly,
        SafetyPreset::Full,
    ];

    /// The wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Contained => "contained",
            Self::ReadOnly => "read_only",
            Self::Full => "full",
        }
    }

    /// What a person reads.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Contained => "Contained",
            Self::ReadOnly => "Read-only",
            Self::Full => "Full access",
        }
    }

    /// The preset lets the agent act without asking and without a boundary: clients show it
    /// in the warning colour.
    pub const fn is_full_access(self) -> bool {
        matches!(self, Self::Full)
    }
}

impl fmt::Display for SafetyPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SafetyPreset {
    type Err = String;

    /// `ask`, `contained`, `read_only` (or `read-only`, `readonly`), `full` (or `full-access`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "ask" => Ok(Self::Ask),
            "contained" => Ok(Self::Contained),
            "read_only" | "readonly" => Ok(Self::ReadOnly),
            "full" | "full_access" => Ok(Self::Full),
            other => Err(format!(
                "unknown safety preset `{other}` (ask, contained, read_only, full)"
            )),
        }
    }
}

/// Who enforces the boundary of a preset (`ChatInfo.safety.enforced_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyEnforcer {
    /// The harness: its own sandbox, permission mode or tool list.
    Harness,
    /// Hutan's own sandbox around the process (reserved: SANDBOX-2).
    Hutan,
    /// Nothing: *Full access*, or a preset the harness did not confirm (`note` says which).
    None,
}

/// What a harness is asked for, per preset. Built by [`plan`]; only the fields of the harness
/// it was built for are set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyPlan {
    pub preset: SafetyPreset,
    /// Who enforces it when the harness confirms the request.
    pub enforced_by: SafetyEnforcer,
    /// Codex: the approval policy and the sandbox of `thread/start` (`-a` and `-s`).
    pub codex: Option<(ApprovalPolicy, SandboxMode)>,
    /// Claude Code: the ACP session mode (`--permission-mode`).
    pub claude_mode: Option<&'static str>,
    /// Claude Code: the session turns its Bash sandbox on (`sandbox` of
    /// `_meta.claudeCode.options`, or `--settings`), with no unsandboxed retry.
    pub claude_sandbox: bool,
    /// OMP: `--approval-mode`.
    pub omp_approval_mode: Option<&'static str>,
    /// OMP: `--tools` (a list of the tools it keeps).
    pub omp_tools: Option<&'static str>,
}

/// OMP's `--tools` in *Read-only*: its read and search tools, by OMP's own names. The same list
/// an Advisor assistant runs with.
pub const OMP_READ_TOOLS: &str = "read,grep,glob";

/// The Claude Code session modes the presets use (`--permission-mode`, ACP modes).
pub const CLAUDE_MODE_ASK: &str = "default";
pub const CLAUDE_MODE_CONTAINED: &str = "acceptEdits";
pub const CLAUDE_MODE_READ_ONLY: &str = "plan";
pub const CLAUDE_MODE_FULL: &str = "bypassPermissions";

/// The `sandbox` settings *Contained* gives a Claude Code session: the Bash tool runs inside
/// Claude Code's Seatbelt sandbox, sandboxed commands run without a prompt, and a command the
/// sandbox refused is not retried outside it.
pub fn claude_sandbox_settings() -> Value {
    json!({
        "enabled": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
    })
}

/// Why the preset of a harness cannot be honoured. Every refusal names what is missing.
fn no_boundary(agent: &str) -> String {
    match agent {
        "omp" => "OMP has no OS sandbox, so Contained would only be an approval prompt; it \
                  needs Contain (SANDBOX-2)"
            .to_owned(),
        other => format!("{other} has no sandbox Hutan can switch on"),
    }
}

/// What `agent` is started with for `preset`, or why it cannot honour it.
///
/// | | Ask | Contained | Read-only | Full access |
/// |---|---|---|---|---|
/// | Codex | `on-request`, `workspace-write` | `never`, `workspace-write` (network off) | `never`, `read-only` | `never`, `danger-full-access` |
/// | Claude Code | mode `default` | mode `acceptEdits` + Bash sandbox, no unsandboxed retry | mode `plan` | mode `bypassPermissions` |
/// | OMP | `--approval-mode always-ask` | refused: no OS sandbox | `--tools read,grep,glob` | `--approval-mode yolo` |
///
/// `direct` chats run no commands, so nothing is offered for them.
pub fn plan(agent: &str, preset: SafetyPreset) -> Result<SafetyPlan, String> {
    let base = SafetyPlan {
        preset,
        enforced_by: if preset.is_full_access() {
            SafetyEnforcer::None
        } else {
            SafetyEnforcer::Harness
        },
        codex: None,
        claude_mode: None,
        claude_sandbox: false,
        omp_approval_mode: None,
        omp_tools: None,
    };
    match agent {
        "codex" => {
            let (policy, sandbox) = match preset {
                SafetyPreset::Ask => (ApprovalPolicy::OnRequest, SandboxMode::WorkspaceWrite),
                SafetyPreset::Contained => (ApprovalPolicy::Never, SandboxMode::WorkspaceWrite),
                SafetyPreset::ReadOnly => (ApprovalPolicy::Never, SandboxMode::ReadOnly),
                SafetyPreset::Full => (ApprovalPolicy::Never, SandboxMode::DangerFullAccess),
            };
            Ok(SafetyPlan {
                codex: Some((policy, sandbox)),
                ..base
            })
        }
        "claude" => Ok(match preset {
            SafetyPreset::Ask => SafetyPlan {
                claude_mode: Some(CLAUDE_MODE_ASK),
                ..base
            },
            SafetyPreset::Contained => SafetyPlan {
                claude_mode: Some(CLAUDE_MODE_CONTAINED),
                claude_sandbox: true,
                ..base
            },
            SafetyPreset::ReadOnly => SafetyPlan {
                claude_mode: Some(CLAUDE_MODE_READ_ONLY),
                ..base
            },
            SafetyPreset::Full => SafetyPlan {
                claude_mode: Some(CLAUDE_MODE_FULL),
                ..base
            },
        }),
        "omp" => match preset {
            SafetyPreset::Ask => Ok(SafetyPlan {
                omp_approval_mode: Some("always-ask"),
                ..base
            }),
            SafetyPreset::Contained => Err(no_boundary("omp")),
            SafetyPreset::ReadOnly => Ok(SafetyPlan {
                omp_tools: Some(OMP_READ_TOOLS),
                ..base
            }),
            SafetyPreset::Full => Ok(SafetyPlan {
                omp_approval_mode: Some("yolo"),
                ..base
            }),
        },
        "direct" | "assistant" => Err(format!(
            "a {agent} chat runs no commands: there is nothing to contain"
        )),
        other => Err(format!("`{other}` has no safety presets")),
    }
}

/// The command-line arguments that give a **terminal agent** the preset (`codex -s … -a …`,
/// `claude --permission-mode … --settings '…'`, `omp --approval-mode …`), placed before the
/// prompt. A terminal agent reports nothing back: its own screen shows the effective state.
pub fn terminal_args(agent: &str, preset: SafetyPreset) -> Result<Vec<String>, String> {
    let plan = plan(agent, preset)?;
    let mut args = Vec::new();
    if let Some((policy, sandbox)) = plan.codex {
        args.extend([
            "-s".to_owned(),
            sandbox.as_str().to_owned(),
            "-a".to_owned(),
            policy.as_str().to_owned(),
        ]);
    }
    if let Some(mode) = plan.claude_mode {
        args.extend(["--permission-mode".to_owned(), mode.to_owned()]);
    }
    if plan.claude_sandbox {
        args.extend([
            "--settings".to_owned(),
            json!({ "sandbox": claude_sandbox_settings() }).to_string(),
        ]);
    }
    if let Some(mode) = plan.omp_approval_mode {
        args.extend(["--approval-mode".to_owned(), mode.to_owned()]);
    }
    if let Some(tools) = plan.omp_tools {
        args.extend(["--tools".to_owned(), tools.to_owned()]);
    }
    Ok(args)
}

/// What the harness's choice of a preset means in one honest line, for the Start sheet: who
/// enforces it and what it leaves open (ADR 0021 decision 6). `None` when `agent` refuses it.
pub fn describe(agent: &str, preset: SafetyPreset) -> Option<&'static str> {
    plan(agent, preset).ok()?;
    Some(match (agent, preset) {
        ("codex", SafetyPreset::Ask) => {
            "Codex sandbox: workspace-write, network off. Asks when the agent decides to."
        }
        ("codex", SafetyPreset::Contained) => {
            "Codex sandbox: workspace-write, network off. Never asks: what the sandbox refuses fails."
        }
        ("codex", SafetyPreset::ReadOnly) => "Codex sandbox: read-only. Nothing is changed.",
        ("claude", SafetyPreset::Ask) => "Claude Code asks before it edits or runs a command.",
        ("claude", SafetyPreset::Contained) => {
            "Claude Code's Bash sandbox: writes stay in this folder, no unsandboxed retry. File \
             edits are accepted; file tools and MCP servers run outside the sandbox."
        }
        ("claude", SafetyPreset::ReadOnly) => {
            "Claude Code plan mode: it reads and plans, and changes nothing."
        }
        ("omp", SafetyPreset::Ask) => "OMP asks before every change (always-ask).",
        ("omp", SafetyPreset::ReadOnly) => {
            "OMP keeps only its read, grep and glob tools. This is a tool list, not an OS boundary."
        }
        (_, SafetyPreset::Full) => "No boundary and no questions: the agent can do what you can.",
        _ => "",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_roundtrip_their_wire_names() {
        for preset in SafetyPreset::ALL {
            let json = serde_json::to_string(&preset).unwrap();
            assert_eq!(json, format!("\"{}\"", preset.as_str()));
            assert_eq!(preset.as_str().parse::<SafetyPreset>().unwrap(), preset);
        }
        assert_eq!(
            "read-only".parse::<SafetyPreset>().unwrap(),
            SafetyPreset::ReadOnly
        );
        assert!("yolo".parse::<SafetyPreset>().is_err());
    }

    #[test]
    fn codex_gets_the_sandbox_and_policy_pair() {
        let pair = |preset| plan("codex", preset).unwrap().codex.unwrap();
        assert_eq!(
            pair(SafetyPreset::Ask),
            (ApprovalPolicy::OnRequest, SandboxMode::WorkspaceWrite)
        );
        assert_eq!(
            pair(SafetyPreset::Contained),
            (ApprovalPolicy::Never, SandboxMode::WorkspaceWrite)
        );
        assert_eq!(
            pair(SafetyPreset::ReadOnly),
            (ApprovalPolicy::Never, SandboxMode::ReadOnly)
        );
        assert_eq!(
            pair(SafetyPreset::Full),
            (ApprovalPolicy::Never, SandboxMode::DangerFullAccess)
        );
    }

    #[test]
    fn only_full_access_has_no_enforcer() {
        for agent in ["codex", "claude", "omp"] {
            for preset in SafetyPreset::ALL {
                if let Ok(plan) = plan(agent, preset) {
                    assert_eq!(
                        plan.enforced_by == SafetyEnforcer::None,
                        preset == SafetyPreset::Full,
                        "{agent} {preset}"
                    );
                }
            }
        }
    }

    #[test]
    fn omp_refuses_contained_and_says_why() {
        let reason = plan("omp", SafetyPreset::Contained).unwrap_err();
        assert!(reason.contains("no OS sandbox") && reason.contains("SANDBOX-2"));
        assert!(describe("omp", SafetyPreset::Contained).is_none());
        for preset in [
            SafetyPreset::Ask,
            SafetyPreset::ReadOnly,
            SafetyPreset::Full,
        ] {
            assert!(plan("omp", preset).is_ok(), "{preset}");
        }
    }

    #[test]
    fn direct_chats_and_unknown_agents_get_no_presets() {
        for agent in ["direct", "assistant", "gemini"] {
            for preset in SafetyPreset::ALL {
                assert!(plan(agent, preset).is_err(), "{agent} {preset}");
            }
        }
    }

    #[test]
    fn terminal_agents_get_flags_in_each_harnesss_own_vocabulary() {
        let args = |agent, preset| terminal_args(agent, preset).unwrap();
        assert_eq!(
            args("codex", SafetyPreset::Contained),
            ["-s", "workspace-write", "-a", "never"]
        );
        assert_eq!(
            args("claude", SafetyPreset::ReadOnly),
            ["--permission-mode", "plan"]
        );
        let contained = args("claude", SafetyPreset::Contained);
        assert_eq!(contained[..2], ["--permission-mode", "acceptEdits"]);
        assert_eq!(contained[2], "--settings");
        let settings: Value = serde_json::from_str(&contained[3]).unwrap();
        assert_eq!(
            settings,
            json!({"sandbox": {
                "enabled": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
            }})
        );
        assert_eq!(
            args("omp", SafetyPreset::Ask),
            ["--approval-mode", "always-ask"]
        );
        assert_eq!(
            args("omp", SafetyPreset::ReadOnly),
            ["--tools", "read,grep,glob"]
        );
        assert_eq!(args("omp", SafetyPreset::Full), ["--approval-mode", "yolo"]);
        assert!(terminal_args("omp", SafetyPreset::Contained).is_err());
    }

    #[test]
    fn every_offered_preset_has_an_honest_line() {
        for agent in ["codex", "claude", "omp"] {
            for preset in SafetyPreset::ALL {
                if plan(agent, preset).is_ok() {
                    let line = describe(agent, preset).unwrap();
                    assert!(!line.is_empty(), "{agent} {preset}");
                    assert!(!line.to_lowercase().contains("secure"), "{agent} {preset}");
                }
            }
        }
    }
}
