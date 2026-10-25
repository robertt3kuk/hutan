//! Safety presets in the daemon (protocol 0.51, capability `chat.safety`, ADR 0021): what a
//! chat's preset is turned into when its agent starts, and what the chat says about it
//! afterwards.
//!
//! The table of presets per harness is `hutan_core::safety` (the clients read it too). This
//! module only adds what needs the daemon: the Claude Code session metadata and OMP arguments
//! it launches with, and [`effective`], which builds `ChatInfo.safety` from **what the harness
//! reported** — Codex's answer to `thread/start`, Claude Code's session mode — and says so when
//! the harness did not confirm the request (`docs/agents.md` § Safety presets).

use hutan_agents::ReportedSafety;
use hutan_core::safety::{self, SafetyPlan};
use hutan_core::{SafetyEnforcer, SafetyPreset};
use hutan_protocol::chat::{ChatMode, ChatSafety};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use serde_json::{Value, json};

/// What a chat creation does with a preset: the plan for `agent`, or a `bad_request` that says
/// why the harness cannot honour it (never an approximation).
pub fn require(agent: &str, preset: SafetyPreset) -> Result<SafetyPlan, ErrorBody> {
    safety::plan(agent, preset).map_err(|reason| {
        ErrorBody::new(
            ErrorCode::BadRequest,
            format!("the {} preset: {reason}", preset.label()),
        )
    })
}

/// The `_meta` of Claude Code's `session/new` for `preset`: the adapter reads
/// `_meta.claudeCode.options` as the SDK's options, whose `sandbox` is Claude Code's Bash
/// sandbox. `None` when the preset needs none.
pub fn claude_session_meta(agent: &str, preset: SafetyPreset) -> Option<Value> {
    let plan = safety::plan(agent, preset).ok()?;
    plan.claude_sandbox
        .then(|| json!({"claudeCode": {"options": {"sandbox": safety::claude_sandbox_settings()}}}))
}

/// The arguments OMP is launched with for `preset`, after its own (`omp acp …`).
pub fn omp_args(preset: SafetyPreset) -> Vec<String> {
    let mut args = Vec::new();
    if let Ok(plan) = safety::plan("omp", preset) {
        if let Some(mode) = plan.omp_approval_mode {
            args.extend(["--approval-mode".to_owned(), mode.to_owned()]);
        }
        if let Some(tools) = plan.omp_tools {
            args.extend(["--tools".to_owned(), tools.to_owned()]);
        }
    }
    args
}

/// What a chat says of its preset: [`ChatInfo.safety`](hutan_protocol::chat::ChatInfo::safety).
///
/// `reported` is what the agent answered when the conversation opened (Codex), `mode` the
/// session mode the harness reports now (ACP). A request the harness did not confirm is
/// `enforced_by: none` with a note that starts with *Not applied*; a part the harness cannot
/// report (Claude Code's sandbox, OMP's approval mode) stays in `note` as *requested*.
pub fn effective(
    agent: &str,
    preset: SafetyPreset,
    reported: Option<&ReportedSafety>,
    mode: Option<&ChatMode>,
) -> ChatSafety {
    let not_applied = |note: String| ChatSafety {
        preset,
        enforced_by: SafetyEnforcer::None,
        network: None,
        note: Some(note),
    };
    let plan = match safety::plan(agent, preset) {
        Ok(plan) => plan,
        Err(reason) => return not_applied(format!("Not applied: {reason}")),
    };
    if let Some((policy, sandbox)) = plan.codex {
        let Some(reported) = reported else {
            return not_applied("Not applied: Codex did not report its sandbox".to_owned());
        };
        let mut differs = Vec::new();
        if reported.sandbox.as_deref() != Some(sandbox.as_str()) {
            differs.push(format!(
                "sandbox {}",
                reported.sandbox.as_deref().unwrap_or("unknown")
            ));
        }
        if reported.approval_policy.as_deref() != Some(policy.as_str()) {
            differs.push(format!(
                "approvals {}",
                reported.approval_policy.as_deref().unwrap_or("unknown")
            ));
        }
        if !differs.is_empty() {
            return not_applied(format!(
                "Not applied: Codex reports {} (asked for {sandbox}, {policy})",
                differs.join(", ")
            ));
        }
        // Full access has no sandbox to switch the network off.
        let network = reported
            .network
            .or_else(|| (sandbox == hutan_core::SandboxMode::DangerFullAccess).then_some(true));
        return ChatSafety {
            preset,
            enforced_by: plan.enforced_by,
            network,
            note: None,
        };
    }
    if let Some(wanted) = plan.claude_mode {
        match mode {
            None => {
                return not_applied("Not applied: Claude Code reported no session mode".to_owned());
            }
            Some(mode) if mode.id != wanted => {
                return not_applied(format!(
                    "Not applied: Claude Code is in mode {} (asked for {wanted})",
                    mode.id
                ));
            }
            Some(_) => {}
        }
        let note = plan.claude_sandbox.then(|| {
            "Mode confirmed. The Bash sandbox was requested through session metadata; Claude Code \
             does not report it back. File tools and MCP servers run outside it."
                .to_owned()
        });
        return ChatSafety {
            preset,
            enforced_by: plan.enforced_by,
            network: None,
            note,
        };
    }
    // OMP: the approval mode and tool list are command-line arguments it does not report.
    let note = if plan.omp_tools.is_some() {
        "The tool list was passed on OMP's command line; OMP does not report it back. It is a \
         tool list, not an OS boundary."
    } else {
        "The approval mode was passed on OMP's command line; OMP does not report it back."
    };
    let note = Some(note.to_owned());
    ChatSafety {
        preset,
        enforced_by: plan.enforced_by,
        network: None,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reported(policy: &str, sandbox: &str, network: Option<bool>) -> ReportedSafety {
        ReportedSafety {
            approval_policy: Some(policy.to_owned()),
            sandbox: Some(sandbox.to_owned()),
            network,
        }
    }

    #[test]
    fn codex_is_confirmed_by_what_the_server_reports() {
        let safety = effective(
            "codex",
            SafetyPreset::Contained,
            Some(&reported("never", "workspace-write", Some(false))),
            None,
        );
        assert_eq!(safety.enforced_by, SafetyEnforcer::Harness);
        assert_eq!(safety.network, Some(false));
        assert_eq!(safety.note, None);
    }

    #[test]
    fn codex_that_reports_something_else_is_not_applied_and_says_what_it_reports() {
        let safety = effective(
            "codex",
            SafetyPreset::Contained,
            Some(&reported("untrusted", "read-only", Some(false))),
            None,
        );
        assert_eq!(safety.enforced_by, SafetyEnforcer::None);
        let note = safety.note.unwrap();
        assert!(note.starts_with("Not applied"), "{note}");
        assert!(note.contains("sandbox read-only") && note.contains("approvals untrusted"));
    }

    #[test]
    fn codex_that_reports_nothing_is_not_applied() {
        let safety = effective("codex", SafetyPreset::ReadOnly, None, None);
        assert_eq!(safety.enforced_by, SafetyEnforcer::None);
        assert!(safety.note.unwrap().starts_with("Not applied"));
    }

    #[test]
    fn codex_full_access_has_no_enforcer_and_the_network_is_open() {
        let safety = effective(
            "codex",
            SafetyPreset::Full,
            Some(&reported("never", "danger-full-access", None)),
            None,
        );
        assert_eq!(safety.enforced_by, SafetyEnforcer::None);
        assert_eq!(safety.network, Some(true));
        assert_eq!(safety.note, None);
    }

    #[test]
    fn claude_is_confirmed_by_its_mode_and_never_claims_the_sandbox_it_cannot_read_back() {
        let mode = ChatMode::new("acceptEdits", None);
        let safety = effective("claude", SafetyPreset::Contained, None, Some(&mode));
        assert_eq!(safety.enforced_by, SafetyEnforcer::Harness);
        assert!(safety.note.unwrap().contains("does not report it back"));

        let plan = effective(
            "claude",
            SafetyPreset::ReadOnly,
            None,
            Some(&ChatMode::new("plan", None)),
        );
        assert_eq!(
            (plan.enforced_by, plan.note),
            (SafetyEnforcer::Harness, None)
        );
    }

    #[test]
    fn claude_in_another_mode_than_asked_is_not_applied() {
        let mode = ChatMode::new("default", None);
        let safety = effective("claude", SafetyPreset::Contained, None, Some(&mode));
        assert_eq!(safety.enforced_by, SafetyEnforcer::None);
        assert!(safety.note.unwrap().contains("mode default"));
        let none = effective("claude", SafetyPreset::Ask, None, None);
        assert_eq!(none.enforced_by, SafetyEnforcer::None);
    }

    #[test]
    fn omp_says_its_arguments_are_not_reported_back() {
        let ask = effective("omp", SafetyPreset::Ask, None, None);
        assert_eq!(ask.enforced_by, SafetyEnforcer::Harness);
        assert!(ask.note.unwrap().contains("does not report it back"));
        let read = effective("omp", SafetyPreset::ReadOnly, None, None);
        assert!(read.note.unwrap().contains("not an OS boundary"));
        let full = effective("omp", SafetyPreset::Full, None, None);
        assert_eq!(full.enforced_by, SafetyEnforcer::None);
    }

    #[test]
    fn a_preset_the_harness_cannot_honour_is_never_shown_as_applied() {
        let safety = effective("omp", SafetyPreset::Contained, None, None);
        assert_eq!(safety.enforced_by, SafetyEnforcer::None);
        assert!(safety.note.unwrap().contains("no OS sandbox"));
        let error = require("omp", SafetyPreset::Contained).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(error.message.contains("Contained") && error.message.contains("no OS sandbox"));
    }

    #[test]
    fn launch_extras_per_harness() {
        assert_eq!(
            omp_args(SafetyPreset::Ask),
            ["--approval-mode", "always-ask"]
        );
        assert_eq!(
            omp_args(SafetyPreset::ReadOnly),
            ["--tools", "read,grep,glob"]
        );
        assert_eq!(omp_args(SafetyPreset::Full), ["--approval-mode", "yolo"]);
        assert!(omp_args(SafetyPreset::Contained).is_empty());
        let meta = claude_session_meta("claude", SafetyPreset::Contained).unwrap();
        assert_eq!(
            meta["claudeCode"]["options"]["sandbox"],
            json!({
                "enabled": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
            })
        );
        assert!(claude_session_meta("claude", SafetyPreset::ReadOnly).is_none());
        assert!(claude_session_meta("codex", SafetyPreset::Contained).is_none());
    }
}
