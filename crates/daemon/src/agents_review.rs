//! `agents.review` (protocol 0.66, capability `agents.review`, ADR 0025): what an agent did in one
//! turn, from what the daemon recorded.
//!
//! [`build_review`] is a pure function of [`ReviewInputs`] — the turn's journal events, the
//! turn's checkpoint, the language servers' diagnostics, the requests that wait — so every rule
//! of the packet (the commands and their exit codes, the files, each signal, the bound, the
//! redaction, what is *not recorded*) is tested without a daemon. [`ReviewService`] collects the
//! inputs from the registry, the journal, the checkpoints and the language servers.
//!
//! Nothing here is invented (R10 of the supervisor spike): an exit code the harness did not
//! report stays absent, a checkpoint that is off says so instead of listing no files, a terminal
//! agent — which has no journal — gets its report and a list of what is not recorded. Signals
//! are heuristics with names; they point at evidence and never decide.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hutan_core::agent::{
    AgentEvent, AgentEventKind, FileChangeKind, MessageRole, ToolKind, ToolStatus,
};
use hutan_core::checkpoint::{CheckpointKind, CheckpointState};
use hutan_core::{SessionId, TurnId};
use hutan_index::redact::redact;
use hutan_protocol::board::BoardSource;
use hutan_protocol::chat::{ApprovalInfo, QuestionInfo};
use hutan_protocol::checkpoint::{
    CheckpointDiff, CheckpointDiffRequest, CheckpointFile, CheckpointFileStatus,
    CheckpointListRequest, LIST_MAX_LIMIT,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::lsp::{LspDiagnosticsRequest, LspDiagnosticsResult, LspSeverity};
use hutan_protocol::messages::{SessionInfo, SessionListFilter, TerminalAgentState};
use hutan_protocol::review::{
    AgentsReviewRequest, NotRecorded, REVIEW_COMMAND_CHARS, REVIEW_COMMANDS_MAX,
    REVIEW_DIAGNOSTIC_CHARS, REVIEW_DIAGNOSTIC_FILES_MAX, REVIEW_DIAGNOSTICS_PER_FILE,
    REVIEW_EVIDENCE_CHARS, REVIEW_EVIDENCE_MAX, REVIEW_FILES_MAX, REVIEW_LARGE_DIFF_LINES,
    REVIEW_MAX_BYTES, REVIEW_MESSAGE_CHARS, REVIEW_PROMPT_CHARS, REVIEW_TOOL_NAMES_MAX,
    ReviewCommand, ReviewCommands, ReviewDiagnostic, ReviewDiagnosticFile, ReviewDiagnostics,
    ReviewFile, ReviewFiles, ReviewFilesSource, ReviewReport, ReviewRequest, ReviewRequestKind,
    ReviewSignal, ReviewSignalId, ReviewText, ReviewToolCount, ReviewToolStatus, ReviewTools,
    ReviewTurn, ReviewTurnStatus, TurnReview,
};
use hutan_storage::{ChatTurnRow, ChatTurnStatus, Storage};

use crate::checkpoints::caps::{DEFAULT_EXCLUDE, excluded};
use crate::context::{Keep, bound};
use crate::lsp::LspHub;
use crate::registry::{RegistryHandle, now_ms};
use crate::term_agent::OSC_SOURCE;

/// The most events of one turn the journal is read for.
const TURN_EVENTS_MAX: u32 = 20_000;

/// Where the files of the turn come from.
#[derive(Debug, Clone)]
pub enum FilesInput {
    /// The turn's checkpoint recorded them.
    Checkpoint {
        id: hutan_core::CheckpointId,
        /// Its top level, to put absolute journal paths next to its relative ones.
        checkout: Option<PathBuf>,
        diff: CheckpointDiff,
        /// The checkpoint says its start may already hold part of the turn's edits.
        late: bool,
    },
    /// The checkpoint says the turn changed no file.
    Unchanged {
        id: hutan_core::CheckpointId,
        checkout: Option<PathBuf>,
    },
    /// No usable checkpoint; why.
    Missing(String),
}

/// What the language servers said about the changed files.
#[derive(Debug, Clone)]
pub enum DiagnosticsInput {
    Got(LspDiagnosticsResult),
    /// Not asked or not answered; why.
    Missing(String),
}

/// The events of the reviewed turn.
#[derive(Debug, Clone)]
pub struct TurnInput {
    pub row: ChatTurnRow,
    /// From 1, in the order of the journal's turns.
    pub number: u32,
    /// Oldest first.
    pub events: Vec<AgentEvent>,
    /// The turn has more events than were read.
    pub events_cut: bool,
    /// The user message that started the turn when its event is not one of the turn's.
    pub prompt_before: Option<String>,
}

/// Everything a packet is built from.
#[derive(Debug, Clone)]
pub struct ReviewInputs<'a> {
    pub now_ms: u64,
    pub session: &'a SessionInfo,
    pub source: BoardSource,
    /// `None` for a terminal agent.
    pub turn: Option<TurnInput>,
    pub files: FilesInput,
    pub diagnostics: DiagnosticsInput,
    pub requests: Vec<ReviewRequest>,
}

// Building ----------------------------------------------------------------------------------------

/// The packet of `inputs`, within [`REVIEW_MAX_BYTES`].
pub fn build_review(inputs: &ReviewInputs<'_>) -> TurnReview {
    let session = inputs.session;
    let mut review = TurnReview {
        generated_at_ms: inputs.now_ms,
        session_id: session.id,
        title: line(&session.title, REVIEW_COMMAND_CHARS),
        harness: session
            .chat
            .as_ref()
            .map(|chat| chat.agent.clone())
            .or_else(|| session.agent.as_ref().map(|report| report.agent.clone()))
            .or_else(|| {
                session
                    .terminal_agent
                    .as_ref()
                    .map(|agent| agent.app.clone())
            }),
        model: session
            .chat
            .as_ref()
            .map(|chat| chat.model.clone())
            .filter(|model| !model.is_empty()),
        source: inputs.source,
        turn: None,
        report: None,
        prompt: None,
        final_message: None,
        tools: None,
        commands: None,
        files: None,
        diagnostics: None,
        requests: inputs.requests.clone(),
        signals: Vec::new(),
        not_recorded: Vec::new(),
        cut: Vec::new(),
    };
    match &inputs.turn {
        Some(turn) => fill_from_turn(&mut review, inputs, turn),
        None => fill_terminal(&mut review, session, inputs.source),
    }
    fit(&mut review);
    review
}

/// A terminal agent has no journal: its report, and what that leaves out.
fn fill_terminal(review: &mut TurnReview, session: &SessionInfo, source: BoardSource) {
    review.report = match (&session.agent, &session.terminal_agent) {
        (Some(report), _) => Some(ReviewReport {
            state: format!("{:?}", report.state).to_lowercase(),
            message: report
                .message
                .as_deref()
                .map(|m| line(&redact(m), REVIEW_COMMAND_CHARS)),
            updated_ms: Some(report.updated_at_ms),
        }),
        (None, Some(agent)) => Some(ReviewReport {
            state: terminal_state(agent.state).to_owned(),
            message: agent
                .message
                .as_deref()
                .map(|m| line(&redact(m), REVIEW_COMMAND_CHARS)),
            updated_ms: Some(agent.since_ms),
        }),
        (None, None) => None,
    };
    let why = match source {
        BoardSource::Hook => {
            "a terminal agent has no journal: Hutan keeps only the current state its hook reported"
        }
        _ => {
            "a terminal agent has no journal: Hutan recognised the program and sees its state and title only"
        }
    };
    for what in [
        "turns and their outcome",
        "the prompt and the agent's final message",
        "tools and commands, with exit codes",
        "files changed and lines",
        "diagnostics of the changed files",
    ] {
        review.not_recorded.push(NotRecorded::new(what, why));
    }
}

fn terminal_state(state: TerminalAgentState) -> &'static str {
    match state {
        TerminalAgentState::Unknown => "unknown",
        TerminalAgentState::Idle => "idle",
        TerminalAgentState::Working => "working",
        TerminalAgentState::Blocked => "blocked",
        _ => "error",
    }
}

/// One tool call: its start and, when the journal has it, its end.
#[derive(Debug, Clone)]
struct ToolRun {
    kind: ToolKind,
    name: String,
    title: String,
    status: Option<ToolStatus>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
}

/// A file as the turn's records give it, before the list is cut.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    status: CheckpointFileStatus,
    added: Option<u64>,
    removed: Option<u64>,
    binary: bool,
    /// Only the journal named it.
    journal_only: bool,
}

fn fill_from_turn(review: &mut TurnReview, inputs: &ReviewInputs<'_>, turn: &TurnInput) {
    let session = inputs.session;
    let row = &turn.row;
    let runs = tool_runs(&turn.events);
    let tools_ms = {
        let times: Vec<u64> = runs.iter().filter_map(|run| run.duration_ms).collect();
        (!times.is_empty()).then(|| times.iter().sum())
    };
    let ended = row.ended_at_ms;
    review.turn = Some(ReviewTurn {
        turn_id: row.turn_id.parse().unwrap_or_else(|_| TurnId::new()),
        number: turn.number,
        status: match row.status {
            ChatTurnStatus::Running => ReviewTurnStatus::Running,
            ChatTurnStatus::Completed => ReviewTurnStatus::Completed,
            ChatTurnStatus::Failed => ReviewTurnStatus::Failed,
            ChatTurnStatus::Cancelled => ReviewTurnStatus::Cancelled,
        },
        started_ms: row.started_at_ms,
        ended_ms: ended,
        wall_ms: ended.map(|end| end.saturating_sub(row.started_at_ms)),
        tools_ms,
        error: row
            .error
            .as_deref()
            .map(|e| line(&redact(e), REVIEW_COMMAND_CHARS)),
    });
    if row.status == ChatTurnStatus::Running {
        review.not_recorded.push(NotRecorded::new(
            "the end of the turn",
            "it is still running: everything below is what happened so far",
        ));
    }

    // The prompt and the final message are the agent's and the user's words, redacted and cut.
    let user_text = turn.events.iter().find_map(|event| match &event.kind {
        AgentEventKind::MessageEnd {
            role: MessageRole::User,
            text,
            ..
        } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    });
    review.prompt = user_text
        .or_else(|| turn.prompt_before.clone())
        .map(|text| text_of(prompt_part(&text), REVIEW_PROMPT_CHARS));
    let last_message = turn
        .events
        .iter()
        .rev()
        .find_map(|event| match &event.kind {
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                text,
                ..
            } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        });
    review.final_message = last_message
        .as_deref()
        .map(|text| text_of(text, REVIEW_MESSAGE_CHARS));
    if review.final_message.is_none() && row.status == ChatTurnStatus::Completed {
        review.not_recorded.push(NotRecorded::new(
            "the agent's final message",
            "the turn ended without a message of the agent in the journal",
        ));
    }

    // Tools and commands.
    review.tools = Some(tools_of(&runs));
    let commands: Vec<&ToolRun> = runs
        .iter()
        .filter(|run| run.kind == ToolKind::Command)
        .collect();
    let command_items: Vec<ReviewCommand> = commands
        .iter()
        .map(|run| ReviewCommand {
            text: line(&redact(&run.title), REVIEW_COMMAND_CHARS),
            status: match run.status {
                Some(ToolStatus::Completed) => ReviewToolStatus::Completed,
                Some(ToolStatus::Failed) => ReviewToolStatus::Failed,
                Some(ToolStatus::Declined) => ReviewToolStatus::Declined,
                None => ReviewToolStatus::Unfinished,
            },
            exit_code: run.exit_code,
            duration_ms: run.duration_ms,
        })
        .collect();
    let without_code = command_items
        .iter()
        .filter(|c| c.exit_code.is_none() && c.status == ReviewToolStatus::Completed)
        .count();
    if without_code > 0 {
        review.not_recorded.push(NotRecorded::new(
            format!("exit codes of {without_code} command(s) that completed"),
            "the harness reported none: a command without an exit code is not evidence that it passed",
        ));
    }
    let total = command_items.len();
    let mut listed = command_items.clone();
    listed.truncate(REVIEW_COMMANDS_MAX);
    review.commands = Some(ReviewCommands {
        total,
        omitted: total - listed.len(),
        items: listed,
    });
    if turn.events_cut {
        review.not_recorded.push(NotRecorded::new(
            "events past the first 20000 of the turn",
            "the journal holds more than a review reads",
        ));
    }

    // Files: the checkpoint's, plus what only the journal named.
    let roots = roots_of(session, &inputs.files);
    let journal = journal_files(&turn.events);
    let (seen, source, checkpoint_id, exact) = match &inputs.files {
        FilesInput::Checkpoint {
            id,
            diff,
            checkout,
            late,
        } => {
            if *late {
                review.not_recorded.push(NotRecorded::new(
                    "the start of the turn's files",
                    "the checkpoint was late: its start may already hold part of the turn's edits",
                ));
            }
            if diff.truncated {
                review.not_recorded.push(NotRecorded::new(
                    "every file of the diff",
                    "the checkpoint's diff was cut (a very large turn)",
                ));
            }
            (
                merge_files(
                    &diff.files,
                    &journal,
                    checkout.as_deref().or(roots.first().map(PathBuf::as_path)),
                ),
                ReviewFilesSource::Checkpoint,
                Some(*id),
                true,
            )
        }
        FilesInput::Unchanged { id, checkout } => (
            merge_files(&[], &journal, checkout.as_deref()),
            ReviewFilesSource::Checkpoint,
            Some(*id),
            true,
        ),
        FilesInput::Missing(why) => {
            if journal.is_empty() {
                review.not_recorded.push(NotRecorded::new(
                    "files changed in the turn",
                    format!("{why}; the journal names no file change either"),
                ));
            } else {
                review.not_recorded.push(NotRecorded::new(
                    "line counts and files changed outside the agent's file tools",
                    format!("{why}; only the files the agent's own file changes named are listed"),
                ));
            }
            (
                merge_files(&[], &journal, roots.first().map(PathBuf::as_path)),
                ReviewFilesSource::Journal,
                None,
                false,
            )
        }
    };
    if exact || !seen.is_empty() {
        let added: u64 = seen.iter().filter_map(|f| f.added).sum();
        let removed: u64 = seen.iter().filter_map(|f| f.removed).sum();
        let mut items: Vec<ReviewFile> = seen
            .iter()
            .map(|f| ReviewFile {
                path: line(&f.path, REVIEW_COMMAND_CHARS),
                status: f.status,
                added: f.added,
                removed: f.removed,
                binary: f.binary,
            })
            .collect();
        let total = items.len();
        items.truncate(REVIEW_FILES_MAX);
        review.files = Some(ReviewFiles {
            source,
            checkpoint_id,
            total,
            added: exact.then_some(added),
            removed: exact.then_some(removed),
            omitted: total - items.len(),
            items,
        });
    }

    // Diagnostics of the changed files, as the servers say now.
    if seen.is_empty() {
        // Nothing changed or nothing is known: nothing to ask.
    } else {
        match &inputs.diagnostics {
            DiagnosticsInput::Missing(why) => review.not_recorded.push(NotRecorded::new(
                "diagnostics of the changed files",
                why.clone(),
            )),
            DiagnosticsInput::Got(result) if result.servers.is_empty() => {
                review.not_recorded.push(NotRecorded::new(
                    "diagnostics of the changed files",
                    "no language server runs for this workspace",
                ));
            }
            DiagnosticsInput::Got(result) => {
                review.diagnostics = Some(diagnostics_of(result, &seen, &roots));
            }
        }
    }

    // What a language server or the harness cannot say per turn.
    review.not_recorded.push(NotRecorded::new(
        "the agent's active time in the turn",
        "no harness reports it per turn; `time` is wall time and the tools' own durations",
    ));

    review.signals = signals(
        &seen,
        &command_items,
        last_message.as_deref(),
        &roots,
        review.files.as_ref().and_then(|f| f.added.zip(f.removed)),
    );
}

fn prompt_part(text: &str) -> &str {
    // The attached context a message carries is not the user's prompt.
    text.split("\n--- attached context").next().unwrap_or(text)
}

fn text_of(text: &str, max: usize) -> ReviewText {
    let redacted = redact(text.trim());
    let cut = bound(&redacted, max, Keep::Start);
    ReviewText {
        text: cut.text,
        chars: cut.total_chars,
        truncated: cut.truncated,
    }
}

/// `text` on one line, redacted by the caller, at most `max` characters; a cut ends in `…`.
fn line(text: &str, max: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = bound(&joined, max, Keep::Start);
    if cut.truncated {
        format!("{}…", cut.text)
    } else {
        cut.text
    }
}

/// Pairs each `tool_start` with its `tool_end` (by item id) in the order of the starts.
fn tool_runs(events: &[AgentEvent]) -> Vec<ToolRun> {
    let mut runs: Vec<ToolRun> = Vec::new();
    let mut open: HashMap<String, usize> = HashMap::new();
    for event in events {
        match &event.kind {
            AgentEventKind::ToolStart {
                tool, name, title, ..
            } => {
                if let Some(item) = &event.item_id {
                    open.insert(item.to_string(), runs.len());
                }
                runs.push(ToolRun {
                    kind: *tool,
                    name: name.clone(),
                    title: title.clone(),
                    status: None,
                    exit_code: None,
                    duration_ms: None,
                });
            }
            AgentEventKind::ToolEnd {
                tool,
                status,
                exit_code,
                duration_ms,
                ..
            } => {
                let at = event
                    .item_id
                    .as_ref()
                    .and_then(|item| open.remove(&item.to_string()));
                let run = match at {
                    Some(at) => &mut runs[at],
                    None => {
                        runs.push(ToolRun {
                            kind: *tool,
                            name: format!("{tool:?}").to_lowercase(),
                            title: String::new(),
                            status: None,
                            exit_code: None,
                            duration_ms: None,
                        });
                        let last = runs.len() - 1;
                        &mut runs[last]
                    }
                };
                run.status = Some(*status);
                run.exit_code = *exit_code;
                run.duration_ms = *duration_ms;
            }
            _ => {}
        }
    }
    runs
}

fn tools_of(runs: &[ToolRun]) -> ReviewTools {
    let mut by_name: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let (mut failed, mut declined) = (0, 0);
    for run in runs {
        let entry = by_name.entry(run.name.as_str()).or_default();
        entry.0 += 1;
        match run.status {
            Some(ToolStatus::Failed) => {
                failed += 1;
                entry.1 += 1;
            }
            Some(ToolStatus::Declined) => declined += 1,
            _ => {}
        }
    }
    let mut names: Vec<ReviewToolCount> = by_name
        .into_iter()
        .map(|(name, (calls, failed))| ReviewToolCount {
            name: line(name, 60),
            calls,
            failed,
        })
        .collect();
    names.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));
    names.truncate(REVIEW_TOOL_NAMES_MAX);
    ReviewTools {
        calls: runs.len(),
        failed,
        declined,
        by_name: names,
    }
}

/// The file changes the journal's `file_diff` events name, in order, last kind wins.
fn journal_files(events: &[AgentEvent]) -> BTreeMap<String, CheckpointFileStatus> {
    let mut files = BTreeMap::new();
    for event in events {
        if let AgentEventKind::FileDiff { changes, .. } = &event.kind {
            for change in changes {
                let status = match change.kind {
                    FileChangeKind::Add => CheckpointFileStatus::Added,
                    FileChangeKind::Delete => CheckpointFileStatus::Deleted,
                    FileChangeKind::Update => CheckpointFileStatus::Modified,
                };
                files.insert(change.path.clone(), status);
                if let Some(to) = &change.move_path {
                    files.insert(to.clone(), CheckpointFileStatus::Added);
                }
            }
        }
    }
    files
}

/// The folders the session may change: its own and the extra ones. A checkout of the
/// checkpoint counts as the first when there is one.
fn roots_of(session: &SessionInfo, files: &FilesInput) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let checkout = match files {
        FilesInput::Checkpoint { checkout, .. } | FilesInput::Unchanged { checkout, .. } => {
            checkout.clone()
        }
        FilesInput::Missing(_) => None,
    };
    roots.extend(checkout);
    roots.extend(session.cwd.clone());
    roots.extend(session.folder.clone());
    roots.extend(session.extra_dirs.iter().map(|dir| dir.path.clone()));
    roots.dedup();
    roots
}

/// A journal path as the checkpoint writes it: relative to the checkout when it is below it.
fn relative_to(path: &str, root: Option<&Path>) -> String {
    match root {
        Some(root) => Path::new(path)
            .strip_prefix(root)
            .map_or_else(|_| path.to_owned(), |rel| rel.display().to_string()),
        None => path.to_owned(),
    }
}

/// The checkpoint's files with what only the journal named added (a secret-named file is
/// never in a snapshot; a file outside the checkout is not either), sorted by path.
fn merge_files(
    diff: &[CheckpointFile],
    journal: &BTreeMap<String, CheckpointFileStatus>,
    root: Option<&Path>,
) -> Vec<Seen> {
    let mut seen: BTreeMap<String, Seen> = diff
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                Seen {
                    path: file.path.clone(),
                    status: file.status,
                    added: (!file.binary).then_some(file.added),
                    removed: (!file.binary).then_some(file.removed),
                    binary: file.binary,
                    journal_only: false,
                },
            )
        })
        .collect();
    for (path, status) in journal {
        let rel = relative_to(path, root);
        seen.entry(rel.clone()).or_insert(Seen {
            path: if diff.is_empty() && !Path::new(path).is_absolute() || rel != *path {
                rel
            } else {
                path.clone()
            },
            status: *status,
            added: None,
            removed: None,
            binary: false,
            journal_only: true,
        });
    }
    seen.into_values().collect()
}

fn diagnostics_of(
    result: &LspDiagnosticsResult,
    seen: &[Seen],
    roots: &[PathBuf],
) -> ReviewDiagnostics {
    let changed = |path: &Path| {
        seen.iter().any(|file| {
            let file_path = Path::new(&file.path);
            if file_path.is_absolute() {
                path == file_path
            } else {
                roots.iter().any(|root| path == root.join(file_path)) || path.ends_with(file_path)
            }
        })
    };
    let mut files: Vec<ReviewDiagnosticFile> = Vec::new();
    let (mut errors, mut warnings) = (0, 0);
    for file in result.files.iter().filter(|f| changed(&f.path)) {
        let e = file
            .diagnostics
            .iter()
            .filter(|d| d.severity == Some(LspSeverity::Error))
            .count();
        let w = file
            .diagnostics
            .iter()
            .filter(|d| d.severity == Some(LspSeverity::Warning))
            .count();
        if e + w == 0 {
            continue;
        }
        errors += e;
        warnings += w;
        let mut worst: Vec<&hutan_protocol::lsp::LspDiagnostic> = file
            .diagnostics
            .iter()
            .filter(|d| matches!(d.severity, Some(LspSeverity::Error | LspSeverity::Warning)))
            .collect();
        worst.sort_by_key(|d| (d.severity != Some(LspSeverity::Error), d.range.start.line));
        let shown = roots
            .iter()
            .find_map(|root| file.path.strip_prefix(root).ok())
            .unwrap_or(&file.path)
            .display()
            .to_string();
        files.push(ReviewDiagnosticFile {
            path: line(&shown, REVIEW_COMMAND_CHARS),
            errors: e,
            warnings: w,
            first: worst
                .into_iter()
                .take(REVIEW_DIAGNOSTICS_PER_FILE)
                .map(|d| ReviewDiagnostic {
                    severity: d.severity.map_or("", LspSeverity::as_str).to_owned(),
                    line: Some(d.range.start.line + 1),
                    message: line(&redact(&d.message), REVIEW_DIAGNOSTIC_CHARS),
                })
                .collect(),
        });
    }
    files.sort_by(|a, b| b.errors.cmp(&a.errors).then_with(|| a.path.cmp(&b.path)));
    files.truncate(REVIEW_DIAGNOSTIC_FILES_MAX);
    ReviewDiagnostics {
        stale: result.stale,
        errors,
        warnings,
        files,
    }
}

// Signals -----------------------------------------------------------------------------------------

/// File names of lockfiles and package manifests.
const DEPENDENCY_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.mod",
    "go.sum",
    "Gemfile",
    "Gemfile.lock",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "Pipfile",
    "Pipfile.lock",
    "requirements.txt",
    "composer.json",
    "composer.lock",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "Package.swift",
    "Package.resolved",
    "Podfile",
    "Podfile.lock",
    "flake.lock",
    "mix.exs",
    "mix.lock",
];

/// Words that make a command look like a test run.
const TEST_WORDS: &[&str] = &[
    "test", "tests", "pytest", "jest", "vitest", "nextest", "mocha", "rspec", "phpunit", "tox",
    "unittest", "xctest", "ctest",
];

/// Words with which an agent says it is finished.
const DONE_WORDS: &[&str] = &[
    "done",
    "complete",
    "completed",
    "finished",
    "fixed",
    "implemented",
    "works",
    "working",
    "passes",
    "passing",
    "passed",
    "ready",
    "resolved",
    "all set",
];

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn looks_like_a_test(command: &str) -> bool {
    words(command)
        .iter()
        .any(|word| TEST_WORDS.contains(&word.as_str()))
}

fn sounds_done(message: &str) -> bool {
    let lower = message.to_lowercase();
    let tokens = words(message);
    DONE_WORDS.iter().any(|word| {
        if word.contains(' ') {
            lower.contains(word)
        } else {
            tokens.iter().any(|token| token == word)
        }
    })
}

fn is_failed(command: &ReviewCommand) -> bool {
    command.status == ReviewToolStatus::Failed || command.exit_code.is_some_and(|code| code != 0)
}

fn evidence(text: impl AsRef<str>) -> String {
    line(&redact(text.as_ref()), REVIEW_EVIDENCE_CHARS)
}

/// A path as the agent gave it that leaves the folders of the session.
fn outside(path: &str, roots: &[PathBuf]) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return !roots.is_empty() && !roots.iter().any(|root| path.starts_with(root));
    }
    matches!(path.components().next(), Some(Component::ParentDir))
}

fn signals(
    seen: &[Seen],
    commands: &[ReviewCommand],
    message: Option<&str>,
    roots: &[PathBuf],
    totals: Option<(u64, u64)>,
) -> Vec<ReviewSignal> {
    let mut out: Vec<ReviewSignal> = Vec::new();
    let mut push = |id: ReviewSignalId, label: &str, mut lines: Vec<String>| {
        lines.truncate(REVIEW_EVIDENCE_MAX);
        out.push(ReviewSignal {
            id,
            label: label.to_owned(),
            evidence: lines,
        });
    };

    let deps: Vec<String> = seen
        .iter()
        .filter(|f| {
            Path::new(&f.path)
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| DEPENDENCY_FILES.contains(&name))
        })
        .map(|f| evidence(&f.path))
        .collect();
    if !deps.is_empty() {
        push(
            ReviewSignalId::DependencyFileChanged,
            "a lockfile or a package manifest changed (by file name)",
            deps,
        );
    }
    let deleted: Vec<String> = seen
        .iter()
        .filter(|f| f.status == CheckpointFileStatus::Deleted)
        .map(|f| evidence(&f.path))
        .collect();
    if !deleted.is_empty() {
        push(ReviewSignalId::FileDeleted, "a file was deleted", deleted);
    }
    // Only the journal knows a path outside the checkout: the checkpoint's are relative to it.
    let away: Vec<String> = seen
        .iter()
        .filter(|f| f.journal_only && outside(&f.path, roots))
        .map(|f| evidence(&f.path))
        .collect();
    if !away.is_empty() {
        push(
            ReviewSignalId::PathOutsideFolder,
            "the agent changed a path outside the session's folders",
            away,
        );
    }
    let patterns: Vec<String> = DEFAULT_EXCLUDE.iter().map(|p| (*p).to_owned()).collect();
    let secret: Vec<String> = seen
        .iter()
        .filter(|f| excluded(f.path.as_bytes(), &patterns))
        .map(|f| evidence(&f.path))
        .collect();
    if !secret.is_empty() {
        push(
            ReviewSignalId::SecretLikePath,
            "a changed path has the name of a likely secret (.env, *.pem, *.key, id_rsa …)",
            secret,
        );
    }
    if let Some((added, removed)) = totals
        && added + removed > REVIEW_LARGE_DIFF_LINES
    {
        push(
            ReviewSignalId::LargeDiff,
            &format!("more than {REVIEW_LARGE_DIFF_LINES} lines added plus removed"),
            vec![format!("+{added} −{removed} over {} files", seen.len())],
        );
    }
    // The commands of a journaled turn are all recorded (none is a fact too).
    if !seen.is_empty() && !commands.iter().any(|c| looks_like_a_test(&c.text)) {
        push(
            ReviewSignalId::NoTestCommand,
            "files changed and no command that looks like a test run (a test, pytest, jest, nextest … in its words)",
            vec![format!(
                "{} files changed; {} command(s), none a test run",
                seen.len(),
                commands.len()
            )],
        );
    }
    let unrecovered: Vec<&ReviewCommand> = commands
        .iter()
        .enumerate()
        .filter(|(at, command)| {
            is_failed(command)
                && command.status != ReviewToolStatus::Declined
                && !commands[at + 1..].iter().any(|later| {
                    later.text == command.text
                        && !is_failed(later)
                        && later.status == ReviewToolStatus::Completed
                })
        })
        .map(|(_, command)| command)
        .collect();
    if !unrecovered.is_empty()
        && let Some(message) = message
        && sounds_done(message)
    {
        let mut lines: Vec<String> = unrecovered
            .iter()
            .map(|c| {
                let code = c
                    .exit_code
                    .map_or_else(|| "failed".to_owned(), |code| format!("exit {code}"));
                evidence(format!("{code}: {}", c.text))
            })
            .collect();
        lines.push(evidence(format!("final message: {message}")));
        push(
            ReviewSignalId::FailedCommandThenDone,
            "a command failed and was not run again with success, and the final message sounds done (a word like done, fixed, passes)",
            lines,
        );
    }
    out
}

// Fitting -----------------------------------------------------------------------------------------

fn size(review: &TurnReview) -> usize {
    serde_json::to_vec(review).map_or(usize::MAX, |bytes| bytes.len())
}

/// Cuts sections, least important first, until the packet fits [`REVIEW_MAX_BYTES`]; says what
/// it cut.
fn fit(review: &mut TurnReview) {
    type Step = (&'static str, fn(&mut TurnReview));
    let steps: [Step; 7] = [
        ("the final message cut to 400 characters", |r| {
            if let Some(m) = &mut r.final_message {
                *m = text_of(&m.text, 400);
                m.chars = m.chars.max(m.text.chars().count());
                m.truncated = true;
            }
        }),
        ("commands cut to the first 15", |r| {
            if let Some(c) = &mut r.commands {
                c.items.truncate(15);
                c.omitted = c.total - c.items.len();
            }
        }),
        (
            "files cut to the first 30, diagnostics to 5 files of 2",
            |r| {
                if let Some(f) = &mut r.files {
                    f.items.truncate(30);
                    f.omitted = f.total - f.items.len();
                }
                if let Some(d) = &mut r.diagnostics {
                    d.files.truncate(5);
                    for file in &mut d.files {
                        file.first.truncate(2);
                    }
                }
            },
        ),
        ("commands cut to the first 8, files to 15", |r| {
            if let Some(c) = &mut r.commands {
                c.items.truncate(8);
                c.omitted = c.total - c.items.len();
            }
            if let Some(f) = &mut r.files {
                f.items.truncate(15);
                f.omitted = f.total - f.items.len();
            }
        }),
        ("evidence cut to 2 lines per signal, tool names to 5", |r| {
            for signal in &mut r.signals {
                signal.evidence.truncate(2);
            }
            if let Some(t) = &mut r.tools {
                t.by_name.truncate(5);
            }
        }),
        ("diagnostics left out", |r| {
            r.diagnostics = None;
        }),
        ("the prompt and the final message left out", |r| {
            r.prompt = None;
            r.final_message = None;
        }),
    ];
    for (what, step) in steps {
        if size(review) <= REVIEW_MAX_BYTES {
            return;
        }
        step(review);
        review.cut.push(what.to_owned());
    }
}

// Collecting --------------------------------------------------------------------------------------

/// `agents.review` over the daemon's state.
#[derive(Debug)]
pub struct ReviewService {
    registry: RegistryHandle,
    storage: Storage,
    lsp: Arc<LspHub>,
}

impl ReviewService {
    pub fn new(registry: RegistryHandle, storage: Storage, lsp: Arc<LspHub>) -> Arc<Self> {
        Arc::new(Self {
            registry,
            storage,
            lsp,
        })
    }

    /// The packet of `req`. A chat gets its turn (the last finished one unless `turn_id`
    /// says); a terminal agent gets its report and the list of what is not recorded; any other
    /// session is `bad_request`.
    pub async fn review(&self, req: AgentsReviewRequest) -> Result<TurnReview, ErrorBody> {
        let session = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions
            .into_iter()
            .find(|s| s.id == req.session_id)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("session {} not found", req.session_id),
                )
            })?;
        let (approvals, questions) = self.requests_of(session.id);
        let requests = requests_of(&approvals, &questions);
        if session.chat.is_none() {
            let reported = session.agent.is_some() || session.terminal_agent.is_some();
            if !reported {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    format!(
                        "session {} is not an agent: it is a chat or a terminal agent that reports, and this is neither",
                        session.id
                    ),
                ));
            }
            let source = if session
                .agent
                .as_ref()
                .is_some_and(|report| report.source != OSC_SOURCE)
            {
                BoardSource::Hook
            } else {
                BoardSource::Recognised
            };
            return Ok(build_review(&ReviewInputs {
                now_ms: now_ms(),
                session: &session,
                source,
                turn: None,
                files: FilesInput::Missing(String::new()),
                diagnostics: DiagnosticsInput::Missing(String::new()),
                requests,
            }));
        }
        let turn = self.turn_of(&session, req.turn_id).await?;
        let turn_id: Option<TurnId> = turn.row.turn_id.parse().ok();
        let files = self.files_of(session.id, turn_id).await;
        let changed = match &files {
            FilesInput::Checkpoint { diff, .. } => !diff.files.is_empty(),
            _ => turn
                .events
                .iter()
                .any(|e| matches!(e.kind, AgentEventKind::FileDiff { .. })),
        };
        let diagnostics = if changed {
            match self
                .lsp
                .diagnostics(LspDiagnosticsRequest {
                    path: None,
                    workspace_id: Some(session.workspace_id),
                    min_severity: None,
                })
                .await
            {
                Ok(result) => DiagnosticsInput::Got(result),
                Err(error) => DiagnosticsInput::Missing(format!(
                    "the language servers did not answer: {}",
                    error.message
                )),
            }
        } else {
            DiagnosticsInput::Missing(String::new())
        };
        Ok(build_review(&ReviewInputs {
            now_ms: now_ms(),
            session: &session,
            source: BoardSource::Journal,
            turn: Some(turn),
            files,
            diagnostics,
            requests,
        }))
    }

    fn requests_of(&self, session: SessionId) -> (Vec<ApprovalInfo>, Vec<QuestionInfo>) {
        let chats = self.registry.chats();
        (
            chats.approval_list(Some(session)).approvals,
            chats.question_list(Some(session)).questions,
        )
    }

    /// The turn to review and its events.
    async fn turn_of(
        &self,
        session: &SessionInfo,
        asked: Option<TurnId>,
    ) -> Result<TurnInput, ErrorBody> {
        let storage = self.storage.clone();
        let id = session.id;
        let rows = tokio::task::spawn_blocking(move || storage.chat().turns(id))
            .await
            .map_err(internal)?
            .map_err(|e| internal(format!("reading the turns: {e}")))?;
        let at = match asked {
            Some(turn) => rows
                .iter()
                .position(|row| row.turn_id == turn.to_string())
                .ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("turn {turn} is not in the journal of session {id}"),
                    )
                })?,
            None => rows
                .iter()
                .rposition(|row| row.status != ChatTurnStatus::Running)
                .ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("session {id} has no finished turn to review yet"),
                    )
                })?,
        };
        let row = rows[at].clone();
        let storage = self.storage.clone();
        let turn_key = row.turn_id.clone();
        let (items, before) = tokio::task::spawn_blocking(move || {
            let items = storage
                .chat()
                .items_of_turn(id, &turn_key, TURN_EVENTS_MAX + 1)?;
            let first = items.first().map(|i| i.seq);
            let before = match first {
                Some(seq) => storage.chat().items_of_kind_through(
                    id,
                    "message_end",
                    seq.saturating_sub(1),
                    6,
                )?,
                None => Vec::new(),
            };
            Ok::<_, hutan_storage::StorageError>((items, before))
        })
        .await
        .map_err(internal)?
        .map_err(|e| internal(format!("reading the turn: {e}")))?;
        let events_cut = items.len() > usize::try_from(TURN_EVENTS_MAX).unwrap_or(usize::MAX);
        let events: Vec<AgentEvent> = items
            .iter()
            .take(usize::try_from(TURN_EVENTS_MAX).unwrap_or(usize::MAX))
            .filter_map(|item| serde_json::from_str(&item.event_json).ok())
            .collect();
        let prompt_before = before.iter().rev().find_map(|item| {
            match serde_json::from_str::<AgentEvent>(&item.event_json)
                .ok()?
                .kind
            {
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    text,
                    ..
                } => Some(text),
                _ => None,
            }
        });
        Ok(TurnInput {
            row,
            number: u32::try_from(at + 1).unwrap_or(u32::MAX),
            events,
            events_cut,
            prompt_before,
        })
    }

    /// The files of the turn from its checkpoint, or why there are none.
    async fn files_of(&self, session: SessionId, turn: Option<TurnId>) -> FilesInput {
        let checkpoints = self.registry.chats().checkpoints();
        let listed = checkpoints
            .list(CheckpointListRequest {
                session_id: Some(session),
                workspace_id: None,
                limit: Some(LIST_MAX_LIMIT),
                before: None,
            })
            .await;
        let info = match listed {
            Ok(list) => list.checkpoints.into_iter().find(|c| {
                c.kind == CheckpointKind::Turn && c.turn_id.is_some() && c.turn_id == turn
            }),
            Err(error) => {
                return FilesInput::Missing(format!(
                    "the checkpoints could not be read: {}",
                    error.message
                ));
            }
        };
        let Some(info) = info else {
            return FilesInput::Missing(
                "no checkpoint of this turn is kept (they are kept 30 days, and a chat may have them off)"
                    .to_owned(),
            );
        };
        match info.state {
            CheckpointState::Unchanged => FilesInput::Unchanged {
                id: info.id,
                checkout: info.checkout,
            },
            CheckpointState::Ok | CheckpointState::Late => {
                match checkpoints
                    .diff(CheckpointDiffRequest {
                        checkpoint_id: info.id,
                        against: hutan_protocol::checkpoint::DiffAgainst::End,
                        path: None,
                        stat: true,
                    })
                    .await
                {
                    Ok(diff) => FilesInput::Checkpoint {
                        id: info.id,
                        checkout: info.checkout,
                        diff,
                        late: info.state == CheckpointState::Late,
                    },
                    Err(error) => FilesInput::Missing(format!(
                        "the turn's checkpoint could not be diffed: {}",
                        error.message
                    )),
                }
            }
            other => FilesInput::Missing(format!(
                "the turn's checkpoint is {}{}",
                other.as_str(),
                info.reason
                    .as_deref()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            )),
        }
    }
}

fn requests_of(approvals: &[ApprovalInfo], questions: &[QuestionInfo]) -> Vec<ReviewRequest> {
    let mut out: Vec<ReviewRequest> = approvals
        .iter()
        .map(|a| ReviewRequest {
            kind: ReviewRequestKind::Approval,
            title: line(&redact(&a.title), REVIEW_COMMAND_CHARS),
            since_ms: Some(a.requested_at_ms),
        })
        .chain(questions.iter().map(|q| ReviewRequest {
            kind: ReviewRequestKind::Question,
            title: line(&redact(&q.spec.title), REVIEW_COMMAND_CHARS),
            since_ms: Some(q.requested_at_ms),
        }))
        .collect();
    out.sort_by_key(|r| r.since_ms);
    out.truncate(10);
    out
}

fn internal(error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use hutan_core::agent::ProviderIds;
    use hutan_core::{ItemId, SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::lsp::{
        LspDiagnostic, LspFileDiagnostics, LspPosition, LspRange, LspServerBrief, LspServerState,
    };
    use hutan_protocol::messages::{AgentReport, AgentState};

    use super::*;

    const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

    fn session() -> SessionInfo {
        SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Harness,
            title: "api: add health endpoint".into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: Some("/work/api".into()),
            terminal_id: None,
            created_at_ms: 1,
            agent: Some(AgentReport {
                source: "journal".into(),
                agent: "codex".into(),
                state: AgentState::Idle,
                message: None,
                agent_session_id: None,
                updated_at_ms: 5,
                blocked_sub: None,
            }),
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    struct Journal {
        session: SessionId,
        turn: TurnId,
        seq: u64,
        events: Vec<AgentEvent>,
    }

    impl Journal {
        fn new(session: &SessionInfo) -> Self {
            Self {
                session: session.id,
                turn: TurnId::new(),
                seq: 0,
                events: Vec::new(),
            }
        }

        fn push(&mut self, item: Option<ItemId>, kind: AgentEventKind) {
            self.seq += 1;
            self.events.push(AgentEvent {
                session_id: self.session,
                seq: self.seq,
                ts_ms: 1_000 + self.seq,
                delivery: Default::default(),
                turn_id: Some(self.turn),
                item_id: item,
                provider: ProviderIds::default(),
                kind,
                payload: None,
            });
        }

        fn say(&mut self, role: MessageRole, text: &str) {
            self.push(
                None,
                AgentEventKind::MessageEnd {
                    role,
                    phase: None,
                    text: text.into(),
                },
            );
        }

        /// A command and how it ended (`None`: the journal has no end for it).
        fn command(&mut self, text: &str, end: Option<(ToolStatus, Option<i32>)>) {
            let item = ItemId::new();
            self.push(
                Some(item),
                AgentEventKind::ToolStart {
                    tool: ToolKind::Command,
                    name: "command".into(),
                    title: text.into(),
                    input: None,
                },
            );
            if let Some((status, exit_code)) = end {
                self.push(
                    Some(item),
                    AgentEventKind::ToolEnd {
                        tool: ToolKind::Command,
                        status,
                        output: None,
                        truncated: false,
                        exit_code,
                        duration_ms: Some(100),
                    },
                );
            }
        }

        fn edit(&mut self, path: &str, kind: FileChangeKind) {
            self.push(
                None,
                AgentEventKind::FileDiff {
                    changes: vec![hutan_core::agent::FileChange {
                        path: path.into(),
                        kind,
                        move_path: None,
                        patch: None,
                        before: None,
                        after: None,
                    }],
                    patch: None,
                },
            );
        }

        fn input(&self, session: &SessionInfo) -> TurnInput {
            let _ = session;
            TurnInput {
                row: ChatTurnRow {
                    session_id: self.session,
                    turn_id: self.turn.to_string(),
                    provider_turn_id: None,
                    status: ChatTurnStatus::Completed,
                    started_at_ms: 1_000,
                    ended_at_ms: Some(186_000),
                    error: None,
                },
                number: 3,
                events: self.events.clone(),
                events_cut: false,
                prompt_before: None,
            }
        }
    }

    fn file(path: &str, status: CheckpointFileStatus, added: u64, removed: u64) -> CheckpointFile {
        CheckpointFile {
            path: path.into(),
            status,
            added,
            removed,
            binary: false,
            truncated: false,
        }
    }

    fn checkpoint(files: Vec<CheckpointFile>) -> FilesInput {
        FilesInput::Checkpoint {
            id: hutan_core::CheckpointId::new(),
            checkout: Some("/work/api".into()),
            diff: CheckpointDiff {
                files,
                patch: None,
                truncated: false,
            },
            late: false,
        }
    }

    fn review_of(
        session: &SessionInfo,
        journal: &Journal,
        files: FilesInput,
        diagnostics: DiagnosticsInput,
    ) -> TurnReview {
        build_review(&ReviewInputs {
            now_ms: 500_000,
            session,
            source: BoardSource::Journal,
            turn: Some(journal.input(session)),
            files,
            diagnostics,
            requests: Vec::new(),
        })
    }

    fn ids(review: &TurnReview) -> Vec<&'static str> {
        review.signals.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn commands_keep_their_exit_codes_and_a_missing_one_stays_missing() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(MessageRole::User, "Add a health endpoint and test it.");
        journal.command(
            "cargo nextest run -p api",
            Some((ToolStatus::Failed, Some(101))),
        );
        journal.command("cargo fmt", Some((ToolStatus::Completed, Some(0))));
        journal.command("git status --short", Some((ToolStatus::Completed, None)));
        journal.command("sleep 600", None);
        journal.say(MessageRole::Assistant, "Working on it.");
        let review = review_of(
            &session,
            &journal,
            checkpoint(Vec::new()),
            DiagnosticsInput::Missing(String::new()),
        );
        let commands = review.commands.as_ref().unwrap();
        assert_eq!(commands.total, 4);
        let shown: Vec<(&str, ReviewToolStatus, Option<i32>)> = commands
            .items
            .iter()
            .map(|c| (c.text.as_str(), c.status, c.exit_code))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "cargo nextest run -p api",
                    ReviewToolStatus::Failed,
                    Some(101)
                ),
                ("cargo fmt", ReviewToolStatus::Completed, Some(0)),
                ("git status --short", ReviewToolStatus::Completed, None),
                ("sleep 600", ReviewToolStatus::Unfinished, None),
            ]
        );
        let tools = review.tools.as_ref().unwrap();
        assert_eq!((tools.calls, tools.failed, tools.declined), (4, 1, 0));
        assert_eq!(tools.by_name[0].name, "command");
        assert!(
            review
                .not_recorded
                .iter()
                .any(|n| n.what.contains("exit codes of 1 command")),
            "{:?}",
            review.not_recorded
        );
        let turn = review.turn.as_ref().unwrap();
        assert_eq!(
            (turn.number, turn.wall_ms, turn.tools_ms),
            (3, Some(185_000), Some(300))
        );
        assert_eq!(
            review.prompt.as_ref().unwrap().text,
            "Add a health endpoint and test it."
        );
        assert_eq!(
            review.final_message.as_ref().unwrap().text,
            "Working on it."
        );
    }

    #[test]
    fn the_prompt_leaves_out_attached_context_and_falls_back_to_the_message_before() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(
            MessageRole::User,
            "Fix it\n--- attached context (read-only) ---\n[file: a.rs]\nfn a() {}\n",
        );
        let review = review_of(
            &session,
            &journal,
            checkpoint(Vec::new()),
            DiagnosticsInput::Missing(String::new()),
        );
        assert_eq!(review.prompt.unwrap().text, "Fix it");

        let empty = Journal::new(&session);
        let mut turn = empty.input(&session);
        turn.prompt_before = Some("Earlier ask".into());
        let review = build_review(&ReviewInputs {
            now_ms: 1,
            session: &session,
            source: BoardSource::Journal,
            turn: Some(turn),
            files: checkpoint(Vec::new()),
            diagnostics: DiagnosticsInput::Missing(String::new()),
            requests: Vec::new(),
        });
        assert_eq!(review.prompt.unwrap().text, "Earlier ask");
    }

    #[test]
    fn every_signal_fires_on_its_evidence_and_only_there() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(MessageRole::User, "Clean up");
        journal.command("cargo build", Some((ToolStatus::Failed, Some(101))));
        journal.edit("/etc/hosts", FileChangeKind::Update);
        journal.edit("/work/api/.env.production", FileChangeKind::Update);
        journal.say(MessageRole::Assistant, "All done, it builds and works.");
        let files = checkpoint(vec![
            file("Cargo.lock", CheckpointFileStatus::Modified, 300, 280),
            file("src/old.rs", CheckpointFileStatus::Deleted, 0, 40),
            file("src/new.rs", CheckpointFileStatus::Added, 30, 0),
        ]);
        let review = review_of(
            &session,
            &journal,
            files,
            DiagnosticsInput::Missing(String::new()),
        );
        assert_eq!(
            ids(&review),
            [
                "dependency_file_changed",
                "file_deleted",
                "path_outside_folder",
                "secret_like_path",
                "large_diff",
                "no_test_command",
                "failed_command_then_done",
            ]
        );
        let signal = |id: &str| review.signals.iter().find(|s| s.id.as_str() == id).unwrap();
        assert_eq!(signal("dependency_file_changed").evidence, ["Cargo.lock"]);
        assert_eq!(signal("file_deleted").evidence, ["src/old.rs"]);
        assert_eq!(signal("path_outside_folder").evidence, ["/etc/hosts"]);
        assert_eq!(signal("secret_like_path").evidence, [".env.production"]);
        assert_eq!(signal("large_diff").evidence, ["+330 −320 over 5 files"]);
        let failed = &signal("failed_command_then_done").evidence;
        assert_eq!(failed[0], "exit 101: cargo build");
        assert!(
            failed[1].starts_with("final message: All done"),
            "{failed:?}"
        );
        // Every signal says in words what the rule is.
        assert!(review.signals.iter().all(|s| !s.label.is_empty()));
    }

    #[test]
    fn the_signals_stay_quiet_without_their_evidence() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(MessageRole::User, "Fix");
        journal.command("cargo nextest run", Some((ToolStatus::Failed, Some(101))));
        journal.command("cargo nextest run", Some((ToolStatus::Completed, Some(0))));
        journal.say(MessageRole::Assistant, "Done: the tests pass.");
        let files = checkpoint(vec![file(
            "src/lib.rs",
            CheckpointFileStatus::Modified,
            3,
            1,
        )]);
        let review = review_of(
            &session,
            &journal,
            files,
            DiagnosticsInput::Missing(String::new()),
        );
        // A test ran, the failure was run again with success, nothing else looks odd.
        assert_eq!(ids(&review), Vec::<&str>::new());

        // A failure and a message that does not claim to be finished.
        let mut journal = Journal::new(&session);
        journal.command("cargo build", Some((ToolStatus::Failed, Some(1))));
        journal.say(
            MessageRole::Assistant,
            "The build fails; I need your help with the linker.",
        );
        let review = review_of(
            &session,
            &journal,
            checkpoint(Vec::new()),
            DiagnosticsInput::Missing(String::new()),
        );
        assert_eq!(ids(&review), Vec::<&str>::new());

        // A declined command is not a failure.
        let mut journal = Journal::new(&session);
        journal.command("rm -rf build", Some((ToolStatus::Declined, None)));
        journal.say(MessageRole::Assistant, "Done.");
        let review = review_of(
            &session,
            &journal,
            checkpoint(Vec::new()),
            DiagnosticsInput::Missing(String::new()),
        );
        assert_eq!(ids(&review), Vec::<&str>::new());
        assert_eq!(review.tools.unwrap().declined, 1);
    }

    #[test]
    fn files_come_from_the_checkpoint_and_say_what_is_not_recorded_without_one() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.edit("src/a.rs", FileChangeKind::Add);
        // Unchanged: an exact "no file".
        let review = review_of(
            &session,
            &journal,
            FilesInput::Unchanged {
                id: hutan_core::CheckpointId::new(),
                checkout: Some("/work/api".into()),
            },
            DiagnosticsInput::Missing(String::new()),
        );
        let files = review.files.as_ref().unwrap();
        assert_eq!(files.source, ReviewFilesSource::Checkpoint);
        // The journal named a file the checkpoint did not: listed, without line counts.
        assert_eq!(files.total, 1);
        assert_eq!((files.items[0].added, files.items[0].removed), (None, None));

        // No checkpoint: the journal's paths only, and the packet says so.
        let review = review_of(
            &session,
            &journal,
            FilesInput::Missing("the turn's checkpoint is off (disk low)".into()),
            DiagnosticsInput::Missing(String::new()),
        );
        let files = review.files.as_ref().unwrap();
        assert_eq!(files.source, ReviewFilesSource::Journal);
        assert_eq!((files.added, files.removed), (None, None));
        assert!(
            review
                .not_recorded
                .iter()
                .any(|n| n.why.contains("checkpoint is off")),
            "{:?}",
            review.not_recorded
        );

        // No checkpoint and nothing in the journal: no files section at all.
        let nothing = Journal::new(&session);
        let review = review_of(
            &session,
            &nothing,
            FilesInput::Missing("no checkpoint of this turn is kept".into()),
            DiagnosticsInput::Missing(String::new()),
        );
        assert!(review.files.is_none());
        assert!(
            review
                .not_recorded
                .iter()
                .any(|n| n.what == "files changed in the turn"),
        );
    }

    #[test]
    fn diagnostics_cover_the_changed_files_only_and_carry_the_stale_flag() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.command("cargo check", Some((ToolStatus::Completed, Some(0))));
        let diagnostic = |severity, line: u32, message: &str| LspDiagnostic {
            range: LspRange {
                start: LspPosition { line, character: 0 },
                end: LspPosition { line, character: 1 },
            },
            severity: Some(severity),
            code: None,
            source: None,
            message: message.into(),
            related: Vec::new(),
        };
        let result = LspDiagnosticsResult {
            files: vec![
                LspFileDiagnostics {
                    path: "/work/api/src/health.rs".into(),
                    version: None,
                    server_id: "rust-analyzer".into(),
                    diagnostics: vec![
                        diagnostic(LspSeverity::Warning, 3, "unused import"),
                        diagnostic(LspSeverity::Error, 11, "cannot find value `status`"),
                        diagnostic(LspSeverity::Hint, 12, "consider borrowing"),
                    ],
                    truncated: false,
                },
                LspFileDiagnostics {
                    path: "/work/api/src/untouched.rs".into(),
                    version: None,
                    server_id: "rust-analyzer".into(),
                    diagnostics: vec![diagnostic(LspSeverity::Error, 1, "old problem")],
                    truncated: false,
                },
            ],
            truncated: false,
            stale: true,
            servers: vec![LspServerBrief {
                server_id: "rust-analyzer".into(),
                name: "rust-analyzer".into(),
                state: LspServerState::Starting,
            }],
        };
        let files = checkpoint(vec![file(
            "src/health.rs",
            CheckpointFileStatus::Added,
            40,
            0,
        )]);
        let review = review_of(
            &session,
            &journal,
            files,
            DiagnosticsInput::Got(result.clone()),
        );
        let diagnostics = review.diagnostics.as_ref().unwrap();
        assert!(diagnostics.stale);
        assert_eq!((diagnostics.errors, diagnostics.warnings), (1, 1));
        assert_eq!(diagnostics.files.len(), 1);
        assert_eq!(diagnostics.files[0].path, "src/health.rs");
        assert_eq!(diagnostics.files[0].first[0].severity, "error");
        assert_eq!(diagnostics.files[0].first[0].line, Some(12));

        // No server runs: not recorded, never "no diagnostics".
        let none = LspDiagnosticsResult {
            servers: Vec::new(),
            files: Vec::new(),
            ..result
        };
        let files = checkpoint(vec![file(
            "src/health.rs",
            CheckpointFileStatus::Added,
            40,
            0,
        )]);
        let review = review_of(&session, &journal, files, DiagnosticsInput::Got(none));
        assert!(review.diagnostics.is_none());
        assert!(
            review
                .not_recorded
                .iter()
                .any(|n| n.what.contains("diagnostics") && n.why.contains("no language server"))
        );
    }

    #[test]
    fn secrets_are_redacted_everywhere_a_text_goes() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(
            MessageRole::User,
            &format!("Use the key {SECRET} to deploy"),
        );
        journal.command(
            &format!("curl -H 'Authorization: Bearer {SECRET}' https://x"),
            Some((ToolStatus::Completed, Some(0))),
        );
        journal.say(MessageRole::Assistant, &format!("Deployed with {SECRET}."));
        let review = review_of(
            &session,
            &journal,
            checkpoint(Vec::new()),
            DiagnosticsInput::Missing(String::new()),
        );
        let json = serde_json::to_string(&review).unwrap();
        assert!(!json.contains("sk-abc"), "{json}");
        assert!(!json.contains("ABCD"), "{json}");
        assert!(!review.text().contains("sk-abc"));
    }

    #[test]
    fn a_big_turn_is_cut_to_the_bound_and_says_what_was_cut() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(MessageRole::User, &"p".repeat(2000));
        for n in 0..120 {
            journal.command(
                &format!("cargo run --example number{n} -- {}", "x".repeat(400)),
                Some((ToolStatus::Completed, Some(0))),
            );
        }
        journal.say(MessageRole::Assistant, &"final words ".repeat(300));
        let files: Vec<CheckpointFile> = (0..400)
            .map(|n| {
                file(
                    &format!("src/generated/file_{n:04}_{}.rs", "y".repeat(190)),
                    CheckpointFileStatus::Added,
                    10,
                    0,
                )
            })
            .collect();
        let review = review_of(
            &session,
            &journal,
            checkpoint(files),
            DiagnosticsInput::Missing(String::new()),
        );
        assert!(size(&review) <= REVIEW_MAX_BYTES, "{} bytes", size(&review));
        let commands = review.commands.as_ref().unwrap();
        assert_eq!(commands.total, 120, "the count is over every command");
        assert!(commands.items.len() <= REVIEW_COMMANDS_MAX);
        assert_eq!(commands.items.len() + commands.omitted, 120);
        let files = review.files.as_ref().unwrap();
        assert_eq!(files.total, 400, "the count is over every file");
        assert_eq!(files.items.len() + files.omitted, 400);
        assert!(!review.cut.is_empty(), "the packet says what it cut");
        let prompt = review.prompt.as_ref().unwrap();
        assert_eq!(
            (prompt.text.chars().count(), prompt.chars, prompt.truncated),
            (400, 2000, true)
        );
        // The text says the same, with the note about the signals.
        assert!(review.text().contains("Cut to fit"));
    }

    #[test]
    fn a_terminal_agent_has_a_report_and_a_list_of_what_is_not_recorded() {
        let mut session = session();
        session.chat = None;
        session.agent = Some(AgentReport {
            source: "claude-hooks".into(),
            agent: "claude".into(),
            state: AgentState::Blocked,
            message: Some("Waiting for permission".into()),
            agent_session_id: None,
            updated_at_ms: 99,
            blocked_sub: None,
        });
        let review = build_review(&ReviewInputs {
            now_ms: 1,
            session: &session,
            source: BoardSource::Hook,
            turn: None,
            files: FilesInput::Missing(String::new()),
            diagnostics: DiagnosticsInput::Missing(String::new()),
            requests: vec![ReviewRequest {
                kind: ReviewRequestKind::Approval,
                title: "Run rm".into(),
                since_ms: Some(50),
            }],
        });
        assert!(review.turn.is_none() && review.commands.is_none() && review.files.is_none());
        let report = review.report.as_ref().unwrap();
        assert_eq!(report.state, "blocked");
        assert_eq!(report.message.as_deref(), Some("Waiting for permission"));
        let what: Vec<&str> = review
            .not_recorded
            .iter()
            .map(|n| n.what.as_str())
            .collect();
        assert!(what.contains(&"turns and their outcome"));
        assert!(what.contains(&"tools and commands, with exit codes"));
        assert!(review.not_recorded[0].why.contains("hook"));
        assert_eq!(review.requests.len(), 1);
        assert!(review.text().contains("Not recorded:"));
    }

    #[test]
    fn the_text_names_each_section_and_ends_with_the_rule_about_tests() {
        let session = session();
        let mut journal = Journal::new(&session);
        journal.say(MessageRole::User, "Do it");
        journal.command("cargo test", Some((ToolStatus::Failed, Some(101))));
        journal.say(MessageRole::Assistant, "Done.");
        let review = review_of(
            &session,
            &journal,
            checkpoint(vec![file("src/a.rs", CheckpointFileStatus::Modified, 2, 1)]),
            DiagnosticsInput::Missing(String::new()),
        );
        let text = review.text();
        for part in [
            "Review of api: add health endpoint",
            "Turn 3 completed in 3 m 5 s",
            "Prompt:",
            "Agent's final message",
            "Tools: 1 calls, 1 failed",
            "[exit 101 · 100 ms] cargo test",
            "Files changed: 1 (checkpoint) · +2 −1",
            "modified src/a.rs +2 −1",
            "failed_command_then_done",
            "none is a verdict",
            "Not recorded:",
        ] {
            assert!(text.contains(part), "missing {part:?} in\n{text}");
        }
    }
}
