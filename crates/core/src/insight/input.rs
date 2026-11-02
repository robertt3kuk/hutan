//! What a terminal agent's integration tells Hutan about its insight: the optional `insight` of
//! `agent.report` (protocol 0.71, capability `agent.insight.report`; INSIGHT-B,
//! `docs/integrations.md` § Insight).
//!
//! The integrations of Hutan (the OMP extension, the Claude Code and Codex hook script, the
//! Claude Code status-line tee) cannot send what a chat's journal holds, so they send what they
//! have as it happens, in small reports:
//!
//! - **deltas** for what accumulates: the tokens one model call used, the tools that finished and
//!   the files that changed since the last report. A report that is lost is a lost delta, so a
//!   sender keeps what it has not delivered and sends it with the next report;
//! - **latest values** for what is a level: the context now, the session's cost so far.
//!
//! A report carries counts, tool names and paths, never text of a message, tool output or a diff
//! (the same privacy rule as [`AgentInsight`](super::AgentInsight)), and is at most
//! [`MAX_REPORT_BYTES`] as JSON. [`InsightReducer::apply_report`](super::InsightReducer) folds it.

use serde::{Deserialize, Serialize};

use crate::agent::{FileChangeKind, TokenCounts};

use super::{InsightOutcome, InsightSourceKind, MAX_PATH_BYTES, reducer};

/// A report larger than this many bytes (as JSON) is refused.
pub const MAX_REPORT_BYTES: usize = 4 * 1024;
/// Tool rows of one report.
pub const MAX_REPORT_TOOLS: usize = 16;
/// Files of one report.
pub const MAX_REPORT_FILES: usize = 32;
/// A tool name longer than this many bytes is cut.
pub const MAX_TOOL_NAME_BYTES: usize = 64;

/// One report of an integration. Every part is optional; a report with none of them says
/// nothing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InsightReport {
    /// Who knows this (`extension`, `hooks` or `statusline`); `chat` is reserved for the
    /// daemon's own chats. Absent: `extension`.
    #[serde(default)]
    pub source: InsightSourceKind,
    /// A turn of the agent began or ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<ReportedTurn>,
    /// What one model call of the running turn used (a delta). A counter the agent does not
    /// know stays out, it is never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenCounts>,
    /// The context now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ReportedContext>,
    /// The cost of the whole conversation so far, in US dollars, as the agent's own price table
    /// gives it (cumulative, not a delta).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Milliseconds the agent spent waiting for its model so far (cumulative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_ms: Option<u64>,
    /// Tools that finished since the last report, one row per name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ReportedTool>,
    /// Files the agent changed since the last report (reads are not changes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ReportedFile>,
}

/// A turn of a terminal agent: from the user's prompt to the agent being idle again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedTurn {
    pub phase: TurnPhase,
    /// How an ending turn ended; absent: completed. `running` is not an outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<InsightOutcome>,
    /// How long the turn took, when the agent measured it; absent: from the reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPhase {
    Start,
    End,
}

/// The context of the agent now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedContext {
    /// Tokens in the context.
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// What the context holds before the first message (the system prompt, the tools, the
    /// project files): the part that is the same in every turn. With it the context splits in
    /// two, *fixed* and *messages* (`used` less `fixed`). The agent gives it when it started,
    /// switched conversation, compacted or changed the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<u64>,
    /// The agent's own estimate, not a count its provider gave.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// Calls of one tool that finished since the last report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedTool {
    pub name: String,
    /// Calls that finished (at least 1).
    #[serde(default = "one")]
    pub calls: u32,
    /// Of them, calls that failed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed: u32,
    /// Time the calls took together, when the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
}

fn one() -> u32 {
    1
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedFile {
    /// As the agent wrote it; the reducer makes it relative to the session's folder.
    pub path: String,
    pub kind: FileChangeKind,
}

/// Why a report is not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    #[error(
        "the source `chat` is the daemon's own; a terminal agent reports as `extension`, `hooks` or `statusline`"
    )]
    ChatSource,
    #[error("a turn ends with `completed`, `failed` or `cancelled`, not `running`")]
    RunningOutcome,
    #[error("the insight report is larger than {MAX_REPORT_BYTES} bytes")]
    TooLarge,
}

impl InsightReport {
    /// Checks the report and cuts what is over the limits: at most [`MAX_REPORT_TOOLS`] tool
    /// rows and [`MAX_REPORT_FILES`] files (the first ones), names and paths cut to their
    /// bytes, rows without a name or a plain path dropped, a cost that is not a finite number
    /// of at least zero dropped. The size is checked on the report as it arrived.
    pub fn validated(mut self) -> Result<Self, ReportError> {
        if self.source == InsightSourceKind::Chat {
            return Err(ReportError::ChatSource);
        }
        if self.turn.is_some_and(|turn| {
            turn.phase == TurnPhase::End && turn.outcome == Some(InsightOutcome::Running)
        }) {
            return Err(ReportError::RunningOutcome);
        }
        if serde_json::to_vec(&self).is_ok_and(|json| json.len() > MAX_REPORT_BYTES) {
            return Err(ReportError::TooLarge);
        }
        self.tools.truncate(MAX_REPORT_TOOLS);
        for tool in &mut self.tools {
            tool.name = reducer::clip(tool.name.trim(), MAX_TOOL_NAME_BYTES);
            tool.calls = tool.calls.max(1);
            tool.failed = tool.failed.min(tool.calls);
        }
        self.tools.retain(|tool| !tool.name.is_empty());
        self.files.truncate(MAX_REPORT_FILES);
        for file in &mut self.files {
            file.path = reducer::clip(&file.path, MAX_PATH_BYTES);
        }
        self.files
            .retain(|file| !file.path.is_empty() && !file.path.chars().any(char::is_control));
        self.cost_usd = self
            .cost_usd
            .filter(|cost| cost.is_finite() && *cost >= 0.0);
        Ok(self)
    }

    /// Nothing in the report changes any block.
    pub fn is_empty(&self) -> bool {
        self.turn.is_none()
            && self.tokens.is_none()
            && self.context.is_none()
            && self.cost_usd.is_none()
            && self.api_ms.is_none()
            && self.tools.is_empty()
            && self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ReportedTool {
        ReportedTool {
            name: name.to_owned(),
            calls: 1,
            failed: 0,
            ms: None,
        }
    }

    #[test]
    fn a_minimal_report_reads_with_the_defaults() {
        let report: InsightReport = serde_json::from_str(
            r#"{"tools":[{"name":"read"}],"context":{"used":1200,"window":100000}}"#,
        )
        .expect("parses");
        assert_eq!(report.source, InsightSourceKind::Extension);
        assert_eq!(report.tools, vec![tool("read")]);
        assert_eq!(report.context.map(|c| c.used), Some(1200));
        assert!(!report.context.is_some_and(|c| c.estimated));
    }

    #[test]
    fn unknown_fields_of_a_newer_integration_are_ignored() {
        let report: InsightReport =
            serde_json::from_str(r#"{"source":"hooks","something_new":{"a":1}}"#).expect("parses");
        assert_eq!(report.source, InsightSourceKind::Hooks);
        assert!(report.is_empty());
    }

    #[test]
    fn the_daemons_own_source_and_a_running_end_are_refused() {
        let chat = InsightReport {
            source: InsightSourceKind::Chat,
            ..InsightReport::default()
        };
        assert_eq!(chat.validated(), Err(ReportError::ChatSource));
        let running = InsightReport {
            turn: Some(ReportedTurn {
                phase: TurnPhase::End,
                outcome: Some(InsightOutcome::Running),
                duration_ms: None,
            }),
            ..InsightReport::default()
        };
        assert_eq!(running.validated(), Err(ReportError::RunningOutcome));
    }

    #[test]
    fn limits_cut_rows_names_and_paths() {
        let report = InsightReport {
            tools: (0..30).map(|n| tool(&format!("t{n}"))).collect(),
            files: vec![
                ReportedFile {
                    path: "x".repeat(400),
                    kind: FileChangeKind::Update,
                },
                ReportedFile {
                    path: String::new(),
                    kind: FileChangeKind::Add,
                },
                ReportedFile {
                    path: "a\nb".to_owned(),
                    kind: FileChangeKind::Add,
                },
            ],
            cost_usd: Some(f64::NAN),
            ..InsightReport::default()
        };
        let report = report.validated().expect("fits");
        assert_eq!(report.tools.len(), MAX_REPORT_TOOLS);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path.len(), MAX_PATH_BYTES);
        assert_eq!(report.cost_usd, None);
    }

    #[test]
    fn a_report_over_four_kib_is_refused() {
        let report = InsightReport {
            files: (0..32)
                .map(|n| ReportedFile {
                    path: format!("{}{n}", "d/".repeat(120)),
                    kind: FileChangeKind::Update,
                })
                .collect(),
            ..InsightReport::default()
        };
        assert_eq!(report.validated(), Err(ReportError::TooLarge));
    }
}
