//! The insight a hook event or a status-line tick carries (protocol 0.71, INSIGHT-B,
//! `docs/integrations.md` § Insight): which tool ran and how long it took, which file it changed,
//! when a turn began and ended, and (status line) the context, the cost and the API time.
//!
//! Pure, like the rest of the translation, so it is tested on recorded and documented payloads.
//! Counts, tool names and paths only: nothing of a prompt, a tool's output or a diff is read.
//!
//! What the agents' hooks do not give: Claude Code's and Codex's hooks carry **no token counts**
//! (only a foreground subagent's last request), no context size and no cost; Codex's hooks have
//! no tool duration either. Those are the status line's job (Claude Code) or the transcript's
//! (`transcript_path`, a later slice).

use hutan_core::agent::FileChangeKind;
use hutan_core::insight::{
    InsightOutcome, InsightReport, InsightSourceKind, ReportedContext, ReportedFile, ReportedTool,
    ReportedTurn, TurnPhase,
};
use serde_json::Value;

use crate::args::HookAgentArg;

/// Claude Code's file tools and the argument that names the file.
const CLAUDE_FILE_TOOLS: &[(&str, &str)] = &[
    ("Edit", "file_path"),
    ("Write", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

/// The insight of one hook event, or `None` when the event says nothing about it.
pub fn from_hook(agent: HookAgentArg, event: &str, input: &Value) -> Option<InsightReport> {
    let mut report = InsightReport {
        source: InsightSourceKind::Hooks,
        ..InsightReport::default()
    };
    match event {
        "UserPromptSubmit" => report.turn = Some(turn(TurnPhase::Start, None)),
        // A subagent's stop is not the end of the conversation's turn.
        "Stop" if input.get("agent_id").is_none() => {
            report.turn = Some(turn(TurnPhase::End, None));
        }
        "StopFailure" if input.get("agent_id").is_none() => {
            report.turn = Some(turn(TurnPhase::End, Some(InsightOutcome::Failed)));
        }
        "PostToolUse" | "PostToolUseFailure" => {
            let failed = event == "PostToolUseFailure";
            let name = input.get("tool_name")?.as_str().filter(|n| !n.is_empty())?;
            report.tools.push(ReportedTool {
                name: name.to_owned(),
                calls: 1,
                failed: u32::from(failed),
                ms: input.get("duration_ms").and_then(Value::as_u64),
            });
            if !failed {
                report.files = files(agent, name, input);
            }
        }
        _ => return None,
    }
    Some(report)
}

fn turn(phase: TurnPhase, outcome: Option<InsightOutcome>) -> ReportedTurn {
    ReportedTurn {
        phase,
        outcome,
        duration_ms: None,
    }
}

/// The files a finished tool changed.
fn files(agent: HookAgentArg, tool: &str, input: &Value) -> Vec<ReportedFile> {
    let tool_input = input.get("tool_input");
    match agent {
        HookAgentArg::Claude => {
            let Some((_, key)) = CLAUDE_FILE_TOOLS.iter().find(|(name, _)| *name == tool) else {
                return Vec::new();
            };
            let Some(path) = tool_input
                .and_then(|value| value.get(*key))
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
            else {
                return Vec::new();
            };
            // The Write tool says whether it created the file.
            let created = tool == "Write"
                && input
                    .get("tool_response")
                    .and_then(|response| response.get("type"))
                    .and_then(Value::as_str)
                    == Some("create");
            vec![ReportedFile {
                path: path.to_owned(),
                kind: if created {
                    FileChangeKind::Add
                } else {
                    FileChangeKind::Update
                },
            }]
        }
        HookAgentArg::Codex => {
            if tool != "apply_patch" {
                return Vec::new();
            }
            let patch = match tool_input {
                Some(Value::String(text)) => Some(text.as_str()),
                Some(object) => ["command", "input", "patch"]
                    .iter()
                    .find_map(|key| object.get(*key)?.as_str()),
                None => None,
            };
            patch.map(patch_files).unwrap_or_default()
        }
    }
}

/// The files of an `apply_patch` envelope: `*** Add File: p`, `*** Update File: p`,
/// `*** Delete File: p`. Nothing else of the patch is read.
fn patch_files(patch: &str) -> Vec<ReportedFile> {
    const MARKS: [(&str, FileChangeKind); 3] = [
        ("*** Add File: ", FileChangeKind::Add),
        ("*** Update File: ", FileChangeKind::Update),
        ("*** Delete File: ", FileChangeKind::Delete),
    ];
    patch
        .lines()
        .filter_map(|line| {
            let line = line.trim_end();
            MARKS.iter().find_map(|(mark, kind)| {
                let path = line.strip_prefix(mark).filter(|path| !path.is_empty())?;
                Some(ReportedFile {
                    path: path.to_owned(),
                    kind: *kind,
                })
            })
        })
        .collect()
}

/// The insight of one tick of Claude Code's status line: the context window, the session's cost
/// and the time its API calls took. Fields the tick does not carry stay out (it is never zero);
/// a tick that carries none gives `None`.
///
/// The shape is the documented one (`code.claude.com/docs/en/statusline`): `context_window
/// {context_window_size, used_percentage, current_usage {input_tokens, cache_creation_input_tokens,
/// cache_read_input_tokens, output_tokens}}` and `cost {total_cost_usd, total_api_duration_ms}`.
/// The context is what the last request read, as Claude Code's own percentage counts it: input,
/// cache creation and cache read tokens; before the first request (`current_usage` is null) the
/// percentage times the window, marked as an estimate.
pub fn from_statusline(input: &Value) -> Option<InsightReport> {
    let mut report = InsightReport {
        source: InsightSourceKind::Statusline,
        ..InsightReport::default()
    };
    if let Some(window) = input.get("context_window") {
        let size = window
            .get("context_window_size")
            .and_then(Value::as_u64)
            .filter(|size| *size > 0);
        let usage = window
            .get("current_usage")
            .filter(|usage| usage.is_object());
        let sum = |key: &str| {
            usage
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_u64)
        };
        let counted = [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
        ]
        .iter()
        .filter_map(|key| sum(key))
        .reduce(u64::saturating_add);
        let percentage = window.get("used_percentage").and_then(Value::as_f64);
        report.context = match (counted, size, percentage) {
            (Some(used), _, _) => Some(ReportedContext {
                used,
                window: size,
                fixed: None,
                estimated: false,
            }),
            (None, Some(size), Some(percentage)) if percentage.is_finite() && percentage >= 0.0 => {
                Some(ReportedContext {
                    used: (size as f64 * percentage / 100.0).round() as u64,
                    window: Some(size),
                    fixed: None,
                    estimated: true,
                })
            }
            _ => None,
        };
        // What the last request read is one call's size, not tokens the turn spent: no token delta.
    }
    if let Some(cost) = input.get("cost") {
        report.cost_usd = cost
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        report.api_ms = cost.get("total_api_duration_ms").and_then(Value::as_u64);
    }
    (!report.is_empty()).then_some(report)
}

#[cfg(test)]
mod tests;
