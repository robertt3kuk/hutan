//! Reviewing a turn (protocol 0.67, capability `agents.review`, ADR 0025): the DTOs of
//! `agents.review` and the readable text of its packet (`docs/protocol.md` § Reviewing a turn).
//!
//! One bounded, read-only, redacted packet says what an agent did in one turn, from what the
//! daemon recorded: the prompt and the final message, the tools and the commands (with exit
//! codes), the files and their lines, the diagnostics of those files, the requests still
//! waiting, and **signals** — named heuristics that point at evidence and never give a verdict.
//! What the source does not hold is listed in [`TurnReview::not_recorded`], never filled with a
//! zero or a guess: a terminal agent has no turns, and a checkpoint can be off.
//!
//! The same text ([`TurnReview::text`]) is what the `turn_review` attachment gives a model, what
//! `hutan agents --review` prints and what the desktop shows when the chip is opened.

use std::fmt::Write as _;

use hutan_core::{CheckpointId, SessionId, TurnId};
use serde::{Deserialize, Serialize};

use crate::board::BoardSource;
use crate::checkpoint::CheckpointFileStatus;

/// The most bytes of JSON one packet takes (R8 of the supervisor spike): sections are cut,
/// least important first, until it fits, and the packet says which.
pub const REVIEW_MAX_BYTES: usize = 24 * 1024;
/// The most characters of the prompt a packet carries.
pub const REVIEW_PROMPT_CHARS: usize = 400;
/// The most characters of the agent's final message a packet carries.
pub const REVIEW_MESSAGE_CHARS: usize = 1600;
/// The most commands a packet lists (the rest are counted in [`ReviewCommands::omitted`]).
pub const REVIEW_COMMANDS_MAX: usize = 30;
/// The most characters of one command.
pub const REVIEW_COMMAND_CHARS: usize = 200;
/// The most files a packet lists.
pub const REVIEW_FILES_MAX: usize = 60;
/// The most tool names a packet counts one by one.
pub const REVIEW_TOOL_NAMES_MAX: usize = 12;
/// The most files with diagnostics a packet lists, and the most diagnostics of each.
pub const REVIEW_DIAGNOSTIC_FILES_MAX: usize = 10;
pub const REVIEW_DIAGNOSTICS_PER_FILE: usize = 3;
/// The most characters of one diagnostic message.
pub const REVIEW_DIAGNOSTIC_CHARS: usize = 200;
/// The most pieces of evidence one signal lists.
pub const REVIEW_EVIDENCE_MAX: usize = 5;
/// The most characters of one piece of evidence.
pub const REVIEW_EVIDENCE_CHARS: usize = 200;
/// `large_diff` fires above this many lines added plus removed.
pub const REVIEW_LARGE_DIFF_LINES: u64 = 500;
/// What the packet says about its signals, wherever they are shown.
pub const REVIEW_SIGNALS_NOTE: &str = "Signals are heuristics that point at evidence; none is a \
verdict. \"Tests passed\" is only true of a command with exit code 0 in the commands list: never \
infer a result from the agent's own words.";

/// `agents.review`: the packet of one turn of an agent. Read-only.
///
/// `session_id` is a chat (a journal) or a terminal agent (a report: the packet then says that
/// there are no turns). `turn_id` absent: the last finished turn (`not_found` when none ended).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsReviewRequest {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
}

/// The packet. Absent sections were not recorded; each says why in `not_recorded`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnReview {
    pub generated_at_ms: u64,
    pub session_id: SessionId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// What the packet is worth: a chat's journal is exact; a terminal agent only reports.
    pub source: BoardSource,
    /// The reviewed turn; absent for a terminal agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<ReviewTurn>,
    /// The agent's own report (a terminal agent's current state and message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ReviewReport>,
    /// The first [`REVIEW_PROMPT_CHARS`] characters of the user's message that started the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<ReviewText>,
    /// The last message the agent wrote in the turn, bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<ReviewText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ReviewTools>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<ReviewCommands>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<ReviewFiles>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<ReviewDiagnostics>,
    /// Approvals and questions of the session that still wait.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<ReviewRequest>,
    /// Heuristics, by name; see [`REVIEW_SIGNALS_NOTE`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<ReviewSignal>,
    /// What this packet cannot say, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_recorded: Vec<NotRecorded>,
    /// Sections cut to keep the packet within [`REVIEW_MAX_BYTES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<String>,
}

/// How the reviewed turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTurnStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ReviewTurnStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTurn {
    pub turn_id: TurnId,
    /// Its place among the turns the journal holds, from 1.
    pub number: u32,
    pub status: ReviewTurnStatus,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// Wall time, `ended - started`; absent while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// The sum of the durations tools reported (not the agent's thinking time, which no
    /// harness reports per turn); absent when no tool reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_ms: Option<u64>,
    /// The error of a failed turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReport {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_ms: Option<u64>,
}

/// A text, redacted and cut: `chars` is its length before the cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewText {
    pub text: String,
    pub chars: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewTools {
    pub calls: usize,
    /// Calls whose end said `failed`.
    pub failed: usize,
    /// Calls the user or a policy refused.
    pub declined: usize,
    /// Most called first; at most [`REVIEW_TOOL_NAMES_MAX`].
    pub by_name: Vec<ReviewToolCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewToolCount {
    pub name: String,
    pub calls: usize,
    pub failed: usize,
}

/// The commands the agent ran, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommands {
    /// Every command of the turn, listed or not.
    pub total: usize,
    /// Commands past [`REVIEW_COMMANDS_MAX`] (the first ones are listed).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    pub items: Vec<ReviewCommand>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewToolStatus {
    Completed,
    Failed,
    Declined,
    /// The journal has no end for it (the turn was cut, or it still runs).
    Unfinished,
}

impl ReviewToolStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Declined => "declined",
            Self::Unfinished => "unfinished",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCommand {
    /// One line, redacted, at most [`REVIEW_COMMAND_CHARS`] characters.
    pub text: String,
    pub status: ReviewToolStatus,
    /// Absent when the harness reported none: never read as 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Where a packet's files come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFilesSource {
    /// The turn's checkpoint: exact files and lines.
    Checkpoint,
    /// The journal's file changes: paths and kinds, no line counts.
    Journal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFiles {
    pub source: ReviewFilesSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    /// Every changed file, listed or not.
    pub total: usize,
    /// Lines added and removed over every file (absent without a checkpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
    /// Files past [`REVIEW_FILES_MAX`].
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted: usize,
    /// Sorted by path.
    pub items: Vec<ReviewFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFile {
    /// Relative to the checkout when the checkpoint has it, else as the agent wrote it.
    pub path: String,
    pub status: CheckpointFileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
}

/// What the language servers say about the changed files now (the call behind the
/// `diagnostics` MCP tool). Not what they said when the turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiagnostics {
    /// Some server was not ready: the answer may be old or incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
    pub errors: usize,
    pub warnings: usize,
    /// Changed files that have any, worst first; at most [`REVIEW_DIAGNOSTIC_FILES_MAX`].
    pub files: Vec<ReviewDiagnosticFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiagnosticFile {
    pub path: String,
    pub errors: usize,
    pub warnings: usize,
    /// The first few, errors first.
    pub first: Vec<ReviewDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewDiagnostic {
    pub severity: String,
    /// 1-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRequestKind {
    Approval,
    Question,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub kind: ReviewRequestKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
}

/// The heuristics a packet may carry, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSignalId {
    /// A lockfile or a package manifest changed.
    DependencyFileChanged,
    /// A file was deleted.
    FileDeleted,
    /// The agent changed a path outside the session's folder.
    PathOutsideFolder,
    /// A changed path looks like a secret (the names the checkpoints leave out).
    SecretLikePath,
    /// More than [`REVIEW_LARGE_DIFF_LINES`] lines added plus removed.
    LargeDiff,
    /// Files changed and no command that looks like a test ran.
    NoTestCommand,
    /// A command failed, was not run again with success, and the final message sounds done.
    FailedCommandThenDone,
}

impl ReviewSignalId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DependencyFileChanged => "dependency_file_changed",
            Self::FileDeleted => "file_deleted",
            Self::PathOutsideFolder => "path_outside_folder",
            Self::SecretLikePath => "secret_like_path",
            Self::LargeDiff => "large_diff",
            Self::NoTestCommand => "no_test_command",
            Self::FailedCommandThenDone => "failed_command_then_done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSignal {
    pub id: ReviewSignalId,
    /// What the heuristic looks for, in a sentence: the rule, not a conclusion.
    pub label: String,
    /// What it found; at most [`REVIEW_EVIDENCE_MAX`] lines.
    pub evidence: Vec<String>,
}

/// Something the packet cannot say, and why: shown instead of a zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotRecorded {
    pub what: String,
    pub why: String,
}

impl NotRecorded {
    pub fn new(what: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            why: why.into(),
        }
    }
}

impl TurnReview {
    /// The packet as text: what the `turn_review` attachment gives a model, what
    /// `hutan agents --review` prints and what the desktop's view of the chip shows. Every
    /// section says what it is worth; absent sections are listed under *Not recorded*.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let source = match self.source {
            BoardSource::Journal => "the chat's journal",
            BoardSource::Hook => "a terminal agent's hook report",
            BoardSource::Recognised => "a recognised terminal agent (state and title only)",
        };
        let harness = self.harness.as_deref().unwrap_or("agent");
        let _ = writeln!(out, "Review of {} ({harness}) — from {source}", self.title);
        if let Some(model) = &self.model {
            let _ = writeln!(out, "Model: {model}");
        }
        if let Some(turn) = &self.turn {
            let mut line = format!("Turn {} {}", turn.number, turn.status.as_str());
            if let Some(wall) = turn.wall_ms {
                let _ = write!(line, " in {}", duration(wall));
            }
            if let Some(tools) = turn.tools_ms {
                let _ = write!(line, " ({} in tools)", duration(tools));
            }
            let _ = writeln!(out, "{line}");
            if let Some(error) = &turn.error {
                let _ = writeln!(out, "Error: {error}");
            }
        }
        if let Some(report) = &self.report {
            let _ = write!(out, "Reported state: {}", report.state);
            if let Some(message) = &report.message {
                let _ = write!(out, " — {message}");
            }
            let _ = writeln!(out);
        }
        if let Some(prompt) = &self.prompt {
            let _ = writeln!(out, "\nPrompt{}:\n{}", cut_note(prompt), prompt.text);
        }
        if let Some(message) = &self.final_message {
            let _ = writeln!(
                out,
                "\nAgent's final message{} (its own words, not evidence):\n{}",
                cut_note(message),
                message.text
            );
        }
        if let Some(tools) = &self.tools {
            let _ = write!(
                out,
                "\nTools: {} calls, {} failed",
                tools.calls, tools.failed
            );
            if tools.declined > 0 {
                let _ = write!(out, ", {} declined", tools.declined);
            }
            let names: Vec<String> = tools
                .by_name
                .iter()
                .map(|t| {
                    if t.failed > 0 {
                        format!("{} ×{} ({} failed)", t.name, t.calls, t.failed)
                    } else {
                        format!("{} ×{}", t.name, t.calls)
                    }
                })
                .collect();
            if !names.is_empty() {
                let _ = write!(out, " — {}", names.join(", "));
            }
            let _ = writeln!(out);
        }
        if let Some(commands) = &self.commands {
            let _ = writeln!(out, "\nCommands ({}):", commands.total);
            for command in &commands.items {
                let status = match (command.exit_code, command.status) {
                    (Some(code), _) => format!("exit {code}"),
                    (None, status) => format!("{}, no exit code", status.as_str()),
                };
                let time = command
                    .duration_ms
                    .map(|ms| format!(" · {}", duration(ms)))
                    .unwrap_or_default();
                let _ = writeln!(out, "  [{status}{time}] {}", command.text);
            }
            if commands.omitted > 0 {
                let _ = writeln!(out, "  … {} more not listed", commands.omitted);
            }
        }
        if let Some(files) = &self.files {
            let source = match files.source {
                ReviewFilesSource::Checkpoint => "checkpoint",
                ReviewFilesSource::Journal => "journal: paths only, no line counts",
            };
            let mut line = format!("\nFiles changed: {} ({source})", files.total);
            if let (Some(added), Some(removed)) = (files.added, files.removed) {
                let _ = write!(line, " · +{added} −{removed}");
            }
            let _ = writeln!(out, "{line}");
            for file in &files.items {
                let status = match file.status {
                    CheckpointFileStatus::Added => "added",
                    CheckpointFileStatus::Modified => "modified",
                    CheckpointFileStatus::Deleted => "deleted",
                };
                let lines = match (file.added, file.removed) {
                    (Some(a), Some(r)) if !file.binary => format!(" +{a} −{r}"),
                    _ if file.binary => " binary".to_owned(),
                    _ => String::new(),
                };
                let _ = writeln!(out, "  {status} {}{lines}", file.path);
            }
            if files.omitted > 0 {
                let _ = writeln!(out, "  … {} more not listed", files.omitted);
            }
        }
        if let Some(diagnostics) = &self.diagnostics {
            let _ = write!(
                out,
                "\nDiagnostics of the changed files now: {} errors, {} warnings",
                diagnostics.errors, diagnostics.warnings
            );
            if diagnostics.stale {
                let _ = write!(
                    out,
                    " (stale: a language server was not ready, so this may be old or incomplete)"
                );
            }
            let _ = writeln!(out);
            for file in &diagnostics.files {
                let _ = writeln!(out, "  {}", file.path);
                for d in &file.first {
                    let at = d.line.map(|l| format!(":{l}")).unwrap_or_default();
                    let _ = writeln!(out, "    {}{at} {}", d.severity, d.message);
                }
            }
        }
        if !self.requests.is_empty() {
            let _ = writeln!(out, "\nWaiting for the user:");
            for request in &self.requests {
                let kind = match request.kind {
                    ReviewRequestKind::Approval => "approval",
                    ReviewRequestKind::Question => "question",
                };
                let _ = writeln!(out, "  {kind}: {}", request.title);
            }
        }
        if !self.signals.is_empty() {
            let _ = writeln!(out, "\nSignals (heuristics, not verdicts):");
            for signal in &self.signals {
                let _ = writeln!(out, "  {} — {}", signal.id.as_str(), signal.label);
                for evidence in &signal.evidence {
                    let _ = writeln!(out, "    · {evidence}");
                }
            }
        }
        let _ = writeln!(out, "\n{REVIEW_SIGNALS_NOTE}");
        if !self.not_recorded.is_empty() {
            let _ = writeln!(out, "\nNot recorded:");
            for item in &self.not_recorded {
                let _ = writeln!(out, "  {} — {}", item.what, item.why);
            }
        }
        if !self.cut.is_empty() {
            let _ = writeln!(out, "\nCut to fit: {}", self.cut.join("; "));
        }
        out
    }
}

fn cut_note(text: &ReviewText) -> String {
    if text.truncated {
        format!(
            " (first {} of {} characters)",
            text.text.chars().count(),
            text.chars
        )
    } else {
        String::new()
    }
}

/// `850 ms`, `4.2 s`, `3 m 5 s`.
fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} m {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(4_200), "4.2 s");
        assert_eq!(duration(185_000), "3 m 5 s");
    }
}
