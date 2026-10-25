//! User hooks (protocol 0.49, capability `hooks`): commands the user's `daemon.toml` asks the
//! daemon to run when something happens in Hutan (`[[hooks]]`, `docs/integrations.md` § User
//! hooks).
//!
//! Events come from the event bus, like notification intents do, and the same rules of silence
//! apply to what a hook is *given*: [`HookPayload`] carries ids, names, titles, states and
//! counts, redacted and bounded, and never transcript text, a path or a secret.
//!
//! - [`Core`] turns bus events into hook events. It is synchronous and keeps the little memory
//!   that needs (which agent node was working, which session needed the user, which files a
//!   turn changed), so its rules are unit-tested without a clock or a process.
//! - [`HooksService`] looks the session up, applies the hooks' filters and `when_focused`, and
//!   hands the job to the hook's own bounded queue; it never waits for a command. One worker
//!   per hook takes jobs one at a time and runs them under a daemon-wide limit
//!   ([`MAX_CONCURRENT`] at once); a full queue drops the event and counts it.
//! - A run is the command with the payload as JSON on its standard input and as `HUTAN_EVENT_*`
//!   variables, a timeout that kills the whole process tree, and bounded, redacted output kept in
//!   a ring of [`LOG_CAPACITY`] runs (`hooks.log`).
//!
//! Hooks exist only in the user's `daemon.toml`, which only the user's own hand (or the user-only
//! `daemon.settings.set`, which has no key for them) changes: a repository's files are never read
//! here, and `hooks.list|log|test` are refused to agents.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hutan_core::agent::Delivery;
use hutan_core::{
    AgentEventKind, AgentNode, ApprovalId, Liveness, NodeKind, NodeOutcome, NodeState, QuestionId,
    SessionId, SessionKind, TaskId, TaskState, WorkspaceId,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::hooks::{
    DETAIL_MAX_CHARS, HookEvent, HookInfo, HookOutcome, HookPayload, HookRun, HooksList, HooksLog,
    HooksLogRequest, HooksTest, HooksTested, LOG_CAPACITY, MAX_CONCURRENT, OUTPUT_MAX_BYTES,
    QUEUE_CAPACITY, TITLE_MAX_CHARS,
};
use hutan_protocol::messages::{Event, SessionListFilter, WorkspaceKind};
use hutan_protocol::worktree::{StepStatus, WorktreeFinished};
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bus::EventBus;
use crate::config::HookSettings;
use crate::harness::Harnesses;
use crate::notify::clean;
use crate::registry::{RegistryHandle, now_ms};
use crate::uihub::UiHub;

/// Queue between the event bus and the service task.
const OBSERVER_QUEUE: usize = 1024;
/// Per-node and per-task memory is dropped when it grows past this many entries (a safety net:
/// the sets are bounded by what exists and report their removals).
const MAX_MEMOS: usize = 4096;
/// Most distinct files remembered per turn (only their number is reported).
const MAX_TURN_FILES: usize = 10_000;
/// A task seen for the first time that ended less than this long ago counts (what a daemon
/// restart makes of a task that was running).
const TASK_FRESH_MS: u64 = 60_000;
/// Output read from a command before it is redacted and cut to [`OUTPUT_MAX_BYTES`]: redaction
/// has to see a secret whole.
const CAPTURE_MAX: usize = 64 * 1024;
/// How long output may still arrive after the command ended (a grandchild may keep the pipe).
const OUTPUT_GRACE: Duration = Duration::from_secs(2);
/// The default number of runs `hooks.log` answers.
const DEFAULT_LOG_LIMIT: u32 = 50;

// Planning ----------------------------------------------------------------------------------

/// What happened, before the session around it is looked up.
#[derive(Debug, Clone, PartialEq)]
struct Planned {
    payload: HookPayload,
    /// Seen in an agent tree (a terminal session's agent), not in a chat's journal: a chat
    /// session reports its turns through its journal, and the tree of a Codex chat must not
    /// make a second event of one turn.
    via_tree: bool,
    /// The agent program the tree names, for a session that reported no agent of its own.
    harness: Option<String>,
}

impl Planned {
    fn new(payload: HookPayload) -> Self {
        Self {
            payload,
            via_tree: false,
            harness: None,
        }
    }
}

/// What is remembered of an agent tree node between two `agent.tree_updated` events.
#[derive(Debug, Clone, Copy)]
struct NodeMemo {
    /// When the node was first seen working in the turn that is going on.
    working_since: Option<u64>,
}

/// What of a session needs the user: the base of `attention.changed`.
#[derive(Debug, Default)]
struct Attention {
    blocked: HashSet<String>,
    approvals: HashSet<ApprovalId>,
    questions: HashSet<QuestionId>,
    /// What the last `attention.changed` said.
    raised: bool,
}

impl Attention {
    fn needs(&self) -> bool {
        !(self.blocked.is_empty() && self.approvals.is_empty() && self.questions.is_empty())
    }
}

/// The decision logic: bus events in, hook events out.
#[derive(Debug, Default)]
struct Core {
    nodes: HashMap<(SessionId, String), NodeMemo>,
    attention: HashMap<SessionId, Attention>,
    /// Files a chat turn reported changed since it started.
    turn_files: HashMap<SessionId, HashSet<String>>,
    tasks: HashMap<TaskId, TaskState>,
}

impl Core {
    fn plan(&mut self, event: &Event, now_ms: u64) -> Vec<Planned> {
        match event {
            Event::ApprovalRequired { approval } => {
                let mut payload = base(HookEvent::ApprovalRequested, now_ms, approval.session_id);
                payload.state = Some("pending".to_owned());
                payload.detail = Some(clean(&approval.title, DETAIL_MAX_CHARS));
                payload.approval_id = Some(approval.id.to_string());
                let mut planned = vec![Planned::new(payload)];
                self.attention
                    .entry(approval.session_id)
                    .or_default()
                    .approvals
                    .insert(approval.id);
                planned.extend(self.flip(approval.session_id, now_ms));
                planned
            }
            Event::ApprovalResolved {
                approval_id,
                session_id,
                ..
            } => {
                if let Some(attention) = self.attention.get_mut(session_id) {
                    attention.approvals.remove(approval_id);
                }
                self.flip(*session_id, now_ms).into_iter().collect()
            }
            Event::QuestionRequired { question } => {
                self.attention
                    .entry(question.session_id)
                    .or_default()
                    .questions
                    .insert(question.id);
                self.flip(question.session_id, now_ms).into_iter().collect()
            }
            Event::QuestionResolved {
                question_id,
                session_id,
                ..
            } => {
                if let Some(attention) = self.attention.get_mut(session_id) {
                    attention.questions.remove(question_id);
                }
                self.flip(*session_id, now_ms).into_iter().collect()
            }
            Event::AgentTreeUpdated {
                session_id,
                nodes_changed,
                nodes_removed,
            } => self.plan_tree(*session_id, nodes_changed, nodes_removed, now_ms),
            Event::SessionExited {
                session_id,
                code,
                signal,
            } => {
                let mut planned: Vec<Planned> = self.forget(*session_id, now_ms);
                let mut payload = base(HookEvent::SessionExited, now_ms, *session_id);
                payload.state = Some(if code.is_some() { "exited" } else { "signaled" }.to_owned());
                payload.exit_code = *code;
                if code.is_none()
                    && let Some(signal) = signal
                {
                    payload.detail = Some(format!("signal {signal}"));
                }
                planned.push(Planned::new(payload));
                planned
            }
            Event::SessionClosed { session_id } => self.forget(*session_id, now_ms),
            Event::ChatEvent { session_id, event } if event.delivery == Delivery::Live => {
                match &event.kind {
                    AgentEventKind::TurnStarted => {
                        self.turn_files.remove(session_id);
                        Vec::new()
                    }
                    AgentEventKind::FileDiff { changes, .. } => {
                        let files = self.turn_files.entry(*session_id).or_default();
                        for change in changes {
                            if files.len() < MAX_TURN_FILES {
                                files.insert(change.path.clone());
                            }
                        }
                        Vec::new()
                    }
                    AgentEventKind::TurnCompleted { duration_ms } => {
                        let mut payload = base(HookEvent::TurnFinished, now_ms, *session_id);
                        payload.state = Some("completed".to_owned());
                        payload.duration_ms = *duration_ms;
                        payload.changed_files = self.take_files(*session_id);
                        vec![Planned::new(payload)]
                    }
                    AgentEventKind::TurnFailed { .. } => {
                        let mut payload = base(HookEvent::TurnFailed, now_ms, *session_id);
                        payload.state = Some("failed".to_owned());
                        payload.changed_files = self.take_files(*session_id);
                        vec![Planned::new(payload)]
                    }
                    _ => Vec::new(),
                }
            }
            Event::TaskUpdated { task } => self.plan_task(task, now_ms),
            _ => Vec::new(),
        }
    }

    /// The number of files the turn that just ended changed; absent when it reported none (a
    /// turn that changed nothing and a turn whose agent does not say are not told apart).
    fn take_files(&mut self, session: SessionId) -> Option<u32> {
        let files = self.turn_files.remove(&session)?;
        u32::try_from(files.len()).ok().filter(|n| *n > 0)
    }

    /// `attention.changed` when the session started or stopped needing the user.
    fn flip(&mut self, session: SessionId, now_ms: u64) -> Option<Planned> {
        let attention = self.attention.get_mut(&session)?;
        let needs = attention.needs();
        if needs == attention.raised {
            if !needs {
                self.attention.remove(&session);
            }
            return None;
        }
        attention.raised = needs;
        if !needs {
            self.attention.remove(&session);
        }
        let mut payload = base(HookEvent::AttentionChanged, now_ms, session);
        payload.needs_attention = Some(needs);
        payload.state = Some(if needs { "attention" } else { "clear" }.to_owned());
        Some(Planned::new(payload))
    }

    fn plan_tree(
        &mut self,
        session: SessionId,
        changed: &[AgentNode],
        removed: &[String],
        now_ms: u64,
    ) -> Vec<Planned> {
        let mut planned = Vec::new();
        for node in changed {
            let key = (session, node.node_id.clone());
            let before = self.nodes.get(&key).copied();
            let is_root = node.kind == NodeKind::Main && node.parent.is_none();
            // A turn runs from the first `working` to the next `idle`, blocked stretches
            // included.
            let working_since = match node.state {
                NodeState::Working => Some(before.and_then(|b| b.working_since).unwrap_or(now_ms)),
                NodeState::Blocked => before.and_then(|b| b.working_since),
                NodeState::Idle | NodeState::Unknown => None,
            };
            if is_root
                && node.state == NodeState::Idle
                && node.source != hutan_agents::codex::TREE_SOURCE
                && let Some(since) = before.and_then(|b| b.working_since)
            {
                let event = match node.outcome {
                    None | Some(NodeOutcome::Completed) => Some(HookEvent::TurnFinished),
                    Some(NodeOutcome::Failed) => Some(HookEvent::TurnFailed),
                    Some(NodeOutcome::Interrupted | NodeOutcome::Disconnected) => None,
                };
                if let Some(event) = event {
                    let mut payload = base(event, now_ms, session);
                    payload.state = Some(
                        if event == HookEvent::TurnFinished {
                            "completed"
                        } else {
                            "failed"
                        }
                        .to_owned(),
                    );
                    payload.duration_ms = Some(now_ms.saturating_sub(since));
                    planned.push(Planned {
                        payload,
                        via_tree: true,
                        harness: Some(node.agent.clone()),
                    });
                }
            }
            let attention = self.attention.entry(session).or_default();
            if node.state == NodeState::Blocked && node.liveness != Liveness::Gone {
                attention.blocked.insert(node.node_id.clone());
            } else {
                attention.blocked.remove(&node.node_id);
            }
            planned.extend(self.flip(session, now_ms));
            self.nodes.insert(key, NodeMemo { working_since });
        }
        for id in removed {
            self.nodes.remove(&(session, id.clone()));
            if let Some(attention) = self.attention.get_mut(&session) {
                attention.blocked.remove(id);
            }
            planned.extend(self.flip(session, now_ms));
        }
        if self.nodes.len() > MAX_MEMOS {
            self.nodes.clear();
        }
        planned
    }

    /// A task that has just become `done` (only the change into the state counts; a task seen
    /// for the first time counts when it ended just now).
    fn plan_task(&mut self, task: &hutan_core::Task, now_ms: u64) -> Vec<Planned> {
        let before = self.tasks.insert(task.id, task.state);
        if self.tasks.len() > MAX_MEMOS {
            self.tasks.clear();
        }
        if task.state != TaskState::Done {
            return Vec::new();
        }
        let fresh = match before {
            Some(state) => state != task.state,
            None => task
                .ended_at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) < TASK_FRESH_MS),
        };
        if !fresh {
            return Vec::new();
        }
        let mut payload = HookPayload::new(HookEvent::TaskDone, now_ms);
        payload.session_id = task.sessions.last().copied();
        payload.workspace_id = None;
        payload.state = Some("done".to_owned());
        payload.task_id = Some(task.id.to_string());
        payload.task_title = Some(clean(&task.title, TITLE_MAX_CHARS));
        payload.detail = task
            .outcome
            .as_deref()
            .map(|outcome| clean(outcome, DETAIL_MAX_CHARS))
            .filter(|outcome| !outcome.is_empty());
        payload.duration_ms = match (task.started_at_ms, task.ended_at_ms) {
            (Some(start), Some(end)) => Some(end.saturating_sub(start)),
            _ => None,
        };
        // The session decides project and workspace; a task's own workspace is the fallback.
        let mut planned = Planned::new(payload);
        planned.payload.workspace_id = Some(task.workspace_id);
        vec![planned]
    }

    /// A session ended or went away: what is remembered about it goes. A session that needed
    /// the user no longer does.
    fn forget(&mut self, session: SessionId, now_ms: u64) -> Vec<Planned> {
        self.nodes.retain(|(s, _), _| *s != session);
        self.turn_files.remove(&session);
        let mut planned = Vec::new();
        if let Some(attention) = self.attention.get_mut(&session) {
            attention.blocked.clear();
            attention.approvals.clear();
            attention.questions.clear();
            planned.extend(self.flip(session, now_ms));
        }
        self.attention.remove(&session);
        planned
    }
}

fn base(event: HookEvent, now_ms: u64, session: SessionId) -> HookPayload {
    let mut payload = HookPayload::new(event, now_ms);
    payload.session_id = Some(session);
    payload
}

/// The sample `hooks.test` runs `event` with: made up, marked `test`, and shaped like the real
/// one. A filter of the hook that names a harness, project or workspace is used for it, so the
/// sample looks like what the hook is for.
fn sample(hook: &HookSettings, now_ms: u64) -> HookPayload {
    let mut payload = HookPayload::new(hook.on, now_ms);
    payload.test = true;
    payload.session_id = Some(SessionId::new());
    payload.session_title = Some("Sample session".to_owned());
    payload.harness = Some(hook.harness.clone().unwrap_or_else(|| "omp".to_owned()));
    payload.project_id = Some(WorkspaceId::new());
    payload.project = Some(
        hook.project
            .clone()
            .unwrap_or_else(|| "sample-project".to_owned()),
    );
    if let Some(workspace) = &hook.workspace {
        payload.workspace_id = Some(WorkspaceId::new());
        payload.workspace = Some(workspace.clone());
    }
    match hook.on {
        HookEvent::TurnFinished => {
            payload.state = Some("completed".to_owned());
            payload.duration_ms = Some(94_000);
            payload.changed_files = Some(3);
        }
        HookEvent::TurnFailed => {
            payload.state = Some("failed".to_owned());
            payload.duration_ms = Some(12_000);
        }
        HookEvent::ApprovalRequested => {
            payload.state = Some("pending".to_owned());
            payload.detail = Some("Run `cargo test`".to_owned());
            payload.approval_id = Some(ApprovalId::new().to_string());
        }
        HookEvent::AttentionChanged => {
            payload.state = Some("attention".to_owned());
            payload.needs_attention = Some(true);
        }
        HookEvent::SessionExited => {
            payload.state = Some("exited".to_owned());
            payload.exit_code = Some(1);
        }
        HookEvent::TaskDone => {
            payload.state = Some("done".to_owned());
            payload.task_id = Some(TaskId::new().to_string());
            payload.task_title = Some("Sample task".to_owned());
            payload.detail = Some("turn completed".to_owned());
            payload.duration_ms = Some(240_000);
        }
        HookEvent::FinishDone => {
            payload.session_id = None;
            payload.session_title = None;
            payload.harness = None;
            payload.state = Some("ok".to_owned());
            payload.detail = Some("sample-checkout".to_owned());
            payload.branch = Some("feature/sample".to_owned());
            payload.target_branch = Some("main".to_owned());
            payload.commit_step = Some("ok".to_owned());
            payload.merge_step = Some("ok".to_owned());
            payload.push_step = Some("ok".to_owned());
            payload.changed_files = Some(3);
        }
    }
    payload
}

fn step_word(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Ok => "ok",
        StepStatus::Skipped => "skipped",
        StepStatus::Failed => "failed",
    }
}

// Running -----------------------------------------------------------------------------------

/// What `run_command` found out.
#[derive(Debug)]
struct Finished {
    outcome: HookOutcome,
    exit_code: Option<i32>,
    signal: Option<i32>,
    stdout: String,
    stderr: String,
    output_truncated: bool,
    error: Option<String>,
}

/// The variables a hook gets: `HUTAN_EVENT` is the event's name and every other field of the
/// payload that is a string, number or bool `HUTAN_EVENT_<FIELD>`.
fn payload_env(payload: &HookPayload) -> Vec<(String, String)> {
    let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(payload) else {
        return Vec::new();
    };
    fields
        .into_iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(text) => text,
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            // A process environment holds no NUL, and a value is one line.
            let text: String = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let name = if key == "event" {
                "HUTAN_EVENT".to_owned()
            } else {
                format!("HUTAN_EVENT_{}", key.to_ascii_uppercase())
            };
            Some((name, text))
        })
        .collect()
}

/// Output as a log keeps it: redacted, control characters but line breaks and tabs gone, at
/// most [`OUTPUT_MAX_BYTES`]; whether anything was left out.
fn finish_output(bytes: &[u8], dropped: bool) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    let redacted = hutan_index::redact::redact(&text);
    let mut text: String = redacted
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let mut cut = dropped;
    if text.len() > OUTPUT_MAX_BYTES {
        let mut end = OUTPUT_MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        cut = true;
    }
    (text, cut)
}

/// What a reader of a pipe has so far.
#[derive(Debug, Default)]
struct Capture {
    bytes: Vec<u8>,
    /// More arrived than [`CAPTURE_MAX`]; it was read and thrown away.
    dropped: bool,
}

/// Reads `pipe` to its end, keeping at most [`CAPTURE_MAX`] bytes: a command that writes a lot
/// must not block on a full pipe, nor fill the daemon's memory.
async fn read_bounded<R: AsyncRead + Unpin>(mut pipe: R, into: Arc<Mutex<Capture>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut capture = into.lock();
                let room = CAPTURE_MAX.saturating_sub(capture.bytes.len());
                if room < n {
                    capture.dropped = true;
                }
                let keep = room.min(n);
                capture.bytes.extend_from_slice(&chunk[..keep]);
            }
        }
    }
}

/// Runs the hook's command with `payload` on its standard input, `env` and the payload's
/// variables, for at most `timeout`.
async fn run_command(
    hook: &HookSettings,
    payload: &HookPayload,
    env: &BTreeMap<String, String>,
    cwd: Option<&Path>,
) -> Finished {
    let failed = |outcome, error: String| Finished {
        outcome,
        exit_code: None,
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        output_truncated: false,
        error: Some(error),
    };
    let mut command = if hook.shell {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(&hook.run[0]);
        command
    } else {
        let mut command = Command::new(&hook.run[0]);
        command.args(&hook.run[1..]);
        command
    };
    command
        .envs(env)
        .envs(payload_env(payload))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    if let Some(cwd) = cwd.filter(|cwd| cwd.is_dir()) {
        command.current_dir(cwd);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed(
                HookOutcome::SpawnFailed,
                format!("could not start `{}`: {error}", hook.run[0]),
            );
        }
    };
    let pid = child.id();
    let mut input = serde_json::to_vec(payload).unwrap_or_default();
    input.push(b'\n');
    let writer = child.stdin.take().map(|mut stdin| {
        tokio::spawn(async move {
            // A command that never reads its input closes the pipe: that is not an error.
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        })
    });
    let stdout = Arc::new(Mutex::new(Capture::default()));
    let stderr = Arc::new(Mutex::new(Capture::default()));
    let readers: Vec<JoinHandle<()>> = [
        child
            .stdout
            .take()
            .map(|pipe| tokio::spawn(read_bounded(pipe, stdout.clone()))),
        child
            .stderr
            .take()
            .map(|pipe| tokio::spawn(read_bounded(pipe, stderr.clone()))),
    ]
    .into_iter()
    .flatten()
    .collect();

    let waited =
        tokio::time::timeout(Duration::from_secs(u64::from(hook.timeout)), child.wait()).await;
    let (outcome, exit_code, signal, error) = match waited {
        Ok(Ok(status)) => {
            use std::os::unix::process::ExitStatusExt as _;
            let outcome = if status.success() {
                HookOutcome::Ok
            } else {
                HookOutcome::Failed
            };
            (outcome, status.code(), status.signal(), None)
        }
        Ok(Err(error)) => (
            HookOutcome::Failed,
            None,
            None,
            Some(format!("could not wait for the command: {error}")),
        ),
        Err(_) => {
            // The command and everything it started go, then the zombie is reaped.
            if let Some(pid) = pid {
                let _ = tokio::task::spawn_blocking(move || crate::resources::kill_tree(pid)).await;
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            (
                HookOutcome::TimedOut,
                None,
                None,
                Some(format!("killed after {} s", hook.timeout)),
            )
        }
    };
    if let Some(writer) = writer {
        writer.abort();
    }
    // What was written before the end is read; a grandchild that keeps the pipe open does
    // not hold the run.
    for mut reader in readers {
        if tokio::time::timeout(OUTPUT_GRACE, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    let (stdout, out_cut) = {
        let capture = stdout.lock();
        finish_output(&capture.bytes, capture.dropped)
    };
    let (stderr, err_cut) = {
        let capture = stderr.lock();
        finish_output(&capture.bytes, capture.dropped)
    };
    Finished {
        outcome,
        exit_code,
        signal,
        stdout,
        stderr,
        output_truncated: out_cut || err_cut,
        error,
    }
}

// The service -------------------------------------------------------------------------------

#[derive(Debug)]
struct Job {
    payload: HookPayload,
}

#[derive(Debug, Default)]
struct Stats {
    runs: u64,
    failures: u64,
    dropped: u64,
    skipped: u64,
    last_run: Option<HookRun>,
}

#[derive(Debug)]
struct HookEntry {
    settings: HookSettings,
    queue: mpsc::Sender<Job>,
    stats: Mutex<Stats>,
}

#[derive(Debug, Default)]
struct Log {
    runs: VecDeque<HookRun>,
    next_id: u64,
}

/// What a session belongs to, for filters and payloads.
#[derive(Debug, Default, Clone)]
struct Context {
    kind: Option<SessionKind>,
    title: Option<String>,
    harness: Option<String>,
    project: Option<(WorkspaceId, String)>,
    workspace: Option<(WorkspaceId, String)>,
}

/// What the service needs of the daemon besides its settings.
#[derive(Debug)]
pub struct HooksInputs {
    pub settings: Vec<HookSettings>,
    pub bus: Arc<EventBus>,
    pub registry: RegistryHandle,
    pub ui: Arc<UiHub>,
    /// Where commands get their login environment from.
    pub harnesses: Arc<Harnesses>,
    /// Exported as `HUTAN_SOCKET` and `HUTAN_BIN`, so a hook can call `hutan`.
    pub socket: PathBuf,
    pub hutan_bin: PathBuf,
    /// The directory commands start in.
    pub home: Option<PathBuf>,
}

/// Runs the user's hooks and answers `hooks.*`.
#[derive(Debug)]
pub struct HooksService {
    hooks: Vec<HookEntry>,
    /// The receivers of the queues, taken by [`start`](Self::start).
    queues: Mutex<Vec<mpsc::Receiver<Job>>>,
    core: Mutex<Core>,
    log: Mutex<Log>,
    permits: Arc<Semaphore>,
    running: AtomicU32,
    bus: Arc<EventBus>,
    registry: RegistryHandle,
    ui: Arc<UiHub>,
    harnesses: Arc<Harnesses>,
    socket: PathBuf,
    hutan_bin: PathBuf,
    home: Option<PathBuf>,
}

/// Counts a run as running while it is alive.
struct RunningGuard<'a>(&'a AtomicU32);

impl<'a> RunningGuard<'a> {
    fn new(counter: &'a AtomicU32) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl HooksService {
    pub fn new(inputs: HooksInputs) -> Arc<Self> {
        let mut hooks = Vec::with_capacity(inputs.settings.len());
        let mut queues = Vec::with_capacity(inputs.settings.len());
        for settings in inputs.settings {
            let (queue, receiver) = mpsc::channel(QUEUE_CAPACITY as usize);
            hooks.push(HookEntry {
                settings,
                queue,
                stats: Mutex::new(Stats::default()),
            });
            queues.push(receiver);
        }
        Arc::new(Self {
            hooks,
            queues: Mutex::new(queues),
            core: Mutex::new(Core::default()),
            log: Mutex::new(Log::default()),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT as usize)),
            running: AtomicU32::new(0),
            bus: inputs.bus,
            registry: inputs.registry,
            ui: inputs.ui,
            harnesses: inputs.harnesses,
            socket: inputs.socket,
            hutan_bin: inputs.hutan_bin,
            home: inputs.home,
        })
    }

    /// Starts one worker per hook and watches the event bus until `shutdown`. Without hooks
    /// nothing is started and the bus has no observer to feed.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) {
        let receivers = std::mem::take(&mut *self.queues.lock());
        if receivers.is_empty() {
            return;
        }
        for (index, mut queue) in receivers.into_iter().enumerate() {
            let this = Arc::clone(self);
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        job = queue.recv() => match job {
                            Some(job) => {
                                let hook = &this.hooks[index];
                                let run = this.execute(index, &hook.settings, &job.payload).await;
                                this.record(index, run);
                            }
                            None => break,
                        },
                    }
                }
            });
        }
        let (tx, mut rx) = mpsc::channel(OBSERVER_QUEUE);
        self.bus.set_hooks_observer(Some(tx));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    event = rx.recv() => match event {
                        Some(event) => this.handle(event).await,
                        None => break,
                    },
                }
            }
            this.bus.set_hooks_observer(None);
        });
    }

    async fn handle(&self, event: Event) {
        let planned = self.core.lock().plan(&event, now_ms());
        for planned in planned {
            self.dispatch(planned).await;
        }
    }

    /// Looks up what the event's session belongs to and offers it to the hooks.
    async fn dispatch(&self, planned: Planned) {
        let Planned {
            mut payload,
            via_tree,
            harness,
        } = planned;
        if !self.hooks.iter().any(|h| h.settings.on == payload.event) {
            return;
        }
        let context = self.context(payload.session_id, payload.workspace_id).await;
        // A chat's turns are in its journal; its tree is a second report of the same turn.
        if via_tree
            && context
                .kind
                .is_some_and(|kind| kind != SessionKind::Terminal)
        {
            return;
        }
        payload.session_title = context.title;
        payload.harness = context.harness.or(harness);
        if let Some((id, name)) = context.project {
            payload.project_id = Some(id);
            payload.project = Some(name);
        }
        payload.workspace_id = context.workspace.as_ref().map(|(id, _)| *id);
        payload.workspace = context.workspace.map(|(_, name)| name);
        self.offer(payload);
    }

    /// What the session belongs to; for an event without a session, the workspace named (a
    /// task's own).
    async fn context(&self, session: Option<SessionId>, workspace: Option<WorkspaceId>) -> Context {
        let info = match session {
            Some(session) => match self
                .registry
                .list_sessions(SessionListFilter::default())
                .await
            {
                Ok(list) => list.sessions.into_iter().find(|s| s.id == session),
                Err(error) => {
                    debug!(message = %error.message, "session lookup for a hook failed");
                    None
                }
            },
            None => None,
        };
        let workspace = info.as_ref().map(|s| s.workspace_id).or(workspace);
        let workspaces = match workspace {
            Some(_) => self.registry.list_workspaces().await.ok(),
            None => None,
        };
        let mut context = Context::default();
        if let Some(info) = &info {
            context.kind = Some(info.kind);
            context.title = Some(clean(&info.title, TITLE_MAX_CHARS)).filter(|t| !t.is_empty());
            context.harness = info
                .chat
                .as_ref()
                .map(|chat| chat.agent.clone())
                .or_else(|| info.agent.as_ref().map(|agent| agent.agent.clone()));
        }
        if let (Some(workspace), Some(list)) = (workspace, workspaces) {
            let find = |id: WorkspaceId| list.workspaces.iter().find(|w| w.id == id);
            if let Some(own) = find(workspace) {
                match own.kind {
                    WorkspaceKind::Project => {
                        context.project = Some((own.id, clean(&own.name, TITLE_MAX_CHARS)));
                        context.workspace = own
                            .parent_id
                            .and_then(find)
                            .map(|w| (w.id, clean(&w.name, TITLE_MAX_CHARS)));
                    }
                    WorkspaceKind::Workspace | WorkspaceKind::Home => {
                        context.workspace = Some((own.id, clean(&own.name, TITLE_MAX_CHARS)));
                    }
                }
            }
        }
        context
    }

    /// Hands the payload to every hook it is for. Never waits: a hook whose queue is full
    /// loses the event, and counts it.
    fn offer(&self, payload: HookPayload) {
        for (index, hook) in self.hooks.iter().enumerate() {
            if !matches(&hook.settings, &payload) {
                continue;
            }
            if !hook.settings.when_focused && self.ui.any_window_active() {
                hook.stats.lock().skipped += 1;
                continue;
            }
            let job = Job {
                payload: payload.clone(),
            };
            if hook.queue.try_send(job).is_err() {
                hook.stats.lock().dropped += 1;
                warn!(
                    target: "hutan::hooks",
                    hook = index,
                    event = payload.event.as_str(),
                    "a hook's queue is full; dropped an event"
                );
            }
        }
    }

    /// Runs one command under the daemon-wide limit.
    async fn execute(&self, index: usize, hook: &HookSettings, payload: &HookPayload) -> HookRun {
        // The semaphore is never closed.
        let permit = self.permits.acquire().await;
        let _running = RunningGuard::new(&self.running);
        let started_at_ms = now_ms();
        let started = Instant::now();
        let mut env = self.base_env().await;
        env.insert(
            "HUTAN_SOCKET".to_owned(),
            self.socket.to_string_lossy().into_owned(),
        );
        env.insert(
            "HUTAN_BIN".to_owned(),
            self.hutan_bin.to_string_lossy().into_owned(),
        );
        let finished = run_command(hook, payload, &env, self.home.as_deref()).await;
        drop(permit);
        let id = {
            let mut log = self.log.lock();
            log.next_id += 1;
            log.next_id
        };
        HookRun {
            id,
            hook: u32::try_from(index).unwrap_or(u32::MAX),
            event: payload.event,
            test: payload.test,
            started_at_ms,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            outcome: finished.outcome,
            exit_code: finished.exit_code,
            signal: finished.signal,
            session_id: payload.session_id,
            stdout: finished.stdout,
            stderr: finished.stderr,
            output_truncated: finished.output_truncated,
            error: finished.error,
        }
    }

    /// The login environment commands start from (the daemon's own with
    /// `[terminal] env_inherit = "daemon"`).
    async fn base_env(&self) -> BTreeMap<String, String> {
        let harnesses = self.harnesses.clone();
        tokio::task::spawn_blocking(move || harnesses.environment())
            .await
            .unwrap_or_default()
    }

    /// Puts a finished run in the log and the hook's counters.
    fn record(&self, index: usize, run: HookRun) {
        info!(
            target: "hutan::hooks",
            hook = index,
            event = run.event.as_str(),
            test = run.test,
            outcome = ?run.outcome,
            exit_code = ?run.exit_code,
            duration_ms = run.duration_ms,
            "hook ran"
        );
        {
            let mut stats = self.hooks[index].stats.lock();
            stats.runs += 1;
            if run.outcome != HookOutcome::Ok {
                stats.failures += 1;
            }
            stats.last_run = Some(run.clone());
        }
        let mut log = self.log.lock();
        log.runs.push_back(run);
        while log.runs.len() > LOG_CAPACITY as usize {
            log.runs.pop_front();
        }
    }

    /// The Finish of a checkout ended: a `finish.done` event, offered in the background.
    pub fn finish_done(self: &Arc<Self>, finished: &WorktreeFinished, checkout: &Path) {
        if !self
            .hooks
            .iter()
            .any(|h| h.settings.on == HookEvent::FinishDone)
        {
            return;
        }
        let mut payload = HookPayload::new(HookEvent::FinishDone, now_ms());
        payload.state = Some(if finished.failed() { "failed" } else { "ok" }.to_owned());
        payload.detail = checkout
            .file_name()
            .map(|name| clean(&name.to_string_lossy(), DETAIL_MAX_CHARS))
            .filter(|name| !name.is_empty());
        payload.branch = Some(clean(&finished.commit.branch, DETAIL_MAX_CHARS));
        payload.commit_step = Some(step_word(finished.commit.status).to_owned());
        payload.changed_files = Some(finished.commit.files).filter(|n| *n > 0);
        if let Some(merge) = &finished.merge {
            payload.merge_step = Some(step_word(merge.status).to_owned());
            payload.target_branch = Some(clean(&merge.target, DETAIL_MAX_CHARS));
        }
        if let Some(push) = &finished.push {
            payload.push_step = Some(step_word(push.status).to_owned());
        }
        let this = Arc::clone(self);
        let checkout = checkout.to_owned();
        tokio::spawn(async move {
            // The project whose folder holds the checkout, if one does.
            if let Ok(list) = this.registry.list_workspaces().await {
                let owner = list.workspaces.iter().find(|w| {
                    w.kind == WorkspaceKind::Project
                        && (w.roots.iter().any(|root| checkout.starts_with(root))
                            || w.project
                                .as_ref()
                                .is_some_and(|p| checkout.starts_with(&p.root)))
                });
                if let Some(project) = owner {
                    payload.project_id = Some(project.id);
                    payload.project = Some(clean(&project.name, TITLE_MAX_CHARS));
                    if let Some(parent) = project
                        .parent_id
                        .and_then(|id| list.workspaces.iter().find(|w| w.id == id))
                    {
                        payload.workspace_id = Some(parent.id);
                        payload.workspace = Some(clean(&parent.name, TITLE_MAX_CHARS));
                    }
                }
            }
            this.offer(payload);
        });
    }

    // The requests -----------------------------------------------------------------------

    /// `hooks.list`.
    pub fn list(&self) -> HooksList {
        HooksList {
            hooks: self
                .hooks
                .iter()
                .enumerate()
                .map(|(index, hook)| {
                    let stats = hook.stats.lock();
                    HookInfo {
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                        name: hook.settings.name.clone(),
                        on: hook.settings.on,
                        harness: hook.settings.harness.clone(),
                        project: hook.settings.project.clone(),
                        workspace: hook.settings.workspace.clone(),
                        run: hook.settings.run.clone(),
                        shell: hook.settings.shell,
                        timeout_secs: hook.settings.timeout,
                        when_focused: hook.settings.when_focused,
                        runs: stats.runs,
                        failures: stats.failures,
                        dropped: stats.dropped,
                        skipped: stats.skipped,
                        last_run: stats.last_run.clone(),
                    }
                })
                .collect(),
            running: self.running.load(Ordering::Relaxed),
            max_concurrent: MAX_CONCURRENT,
            queue_capacity: QUEUE_CAPACITY,
        }
    }

    /// `hooks.log`: newest first.
    pub fn log(&self, req: HooksLogRequest) -> HooksLog {
        let limit = req.limit.unwrap_or(DEFAULT_LOG_LIMIT).min(LOG_CAPACITY) as usize;
        let log = self.log.lock();
        HooksLog {
            runs: log
                .runs
                .iter()
                .rev()
                .filter(|run| req.hook.is_none_or(|hook| run.hook == hook))
                .take(limit)
                .cloned()
                .collect(),
            capacity: LOG_CAPACITY,
        }
    }

    /// `hooks.test`: runs the hook once with a sample of its event and waits for it.
    pub async fn test(&self, req: HooksTest) -> Result<HooksTested, ErrorBody> {
        let index = req.hook as usize;
        let hook = self.hooks.get(index).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "there is no hook {}: `daemon.toml` has {} `[[hooks]]` (counted from 0)",
                    req.hook,
                    self.hooks.len()
                ),
            )
        })?;
        let payload = sample(&hook.settings, now_ms());
        let run = self.execute(index, &hook.settings, &payload).await;
        self.record(index, run.clone());
        Ok(HooksTested { run, payload })
    }
}

/// Whether `payload` is what `hook` is for: its event, and every filter it sets (a project,
/// workspace or harness, by name or id, ignoring case). A payload without the thing a filter
/// names does not match.
fn matches(hook: &HookSettings, payload: &HookPayload) -> bool {
    if hook.on != payload.event {
        return false;
    }
    let same = |wanted: &Option<String>, name: Option<&str>, id: Option<String>| {
        wanted.as_ref().is_none_or(|wanted| {
            name.is_some_and(|name| name.eq_ignore_ascii_case(wanted))
                || id.is_some_and(|id| id.eq_ignore_ascii_case(wanted))
        })
    };
    same(&hook.harness, payload.harness.as_deref(), None)
        && same(
            &hook.project,
            payload.project.as_deref(),
            payload.project_id.map(|id| id.to_string()),
        )
        && same(
            &hook.workspace,
            payload.workspace.as_deref(),
            payload.workspace_id.map(|id| id.to_string()),
        )
}

#[cfg(test)]
mod tests {
    use hutan_core::TurnId;
    use hutan_core::agent::AgentEventDraft;
    use hutan_protocol::chat::ApprovalInfo;

    use super::*;

    fn node(id: &str, kind: NodeKind, state: NodeState) -> AgentNode {
        AgentNode {
            pid: None,
            node_id: id.to_owned(),
            parent: (kind == NodeKind::Sub).then(|| "root".to_owned()),
            kind,
            source: "claude-hooks".to_owned(),
            agent: "claude".to_owned(),
            name: None,
            agent_type: None,
            state,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: None,
            ended_at_ms: None,
            last_activity_ms: 0,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    fn tree(session: SessionId, nodes: Vec<AgentNode>) -> Event {
        Event::AgentTreeUpdated {
            session_id: session,
            nodes_changed: nodes,
            nodes_removed: Vec::new(),
        }
    }

    fn events(planned: &[Planned]) -> Vec<HookEvent> {
        planned.iter().map(|p| p.payload.event).collect()
    }

    fn chat_event(session: SessionId, kind: AgentEventKind) -> Event {
        let mut draft = AgentEventDraft::new(kind);
        draft.turn_id = Some(TurnId::new());
        Event::ChatEvent {
            session_id: session,
            event: Box::new(draft.into_event(session, 1, 1)),
        }
    }

    fn approval(session: SessionId) -> ApprovalInfo {
        ApprovalInfo {
            id: ApprovalId::new(),
            session_id: session,
            turn_id: None,
            kind: hutan_core::agent::PermissionKind::Command,
            title: "Run `curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789'`"
                .to_owned(),
            detail: Some("the whole command line".to_owned()),
            options: Vec::new(),
            requested_at_ms: 1,
        }
    }

    #[test]
    fn a_terminal_agents_turn_that_ends_is_a_finish_or_a_failure() {
        let mut core = Core::default();
        let session = SessionId::new();
        let root = |state, outcome| {
            let mut n = node("root", NodeKind::Main, state);
            n.outcome = outcome;
            n
        };
        // A plain idle with no turn before it is nothing.
        assert!(
            core.plan(&tree(session, vec![root(NodeState::Idle, None)]), 1_000)
                .is_empty()
        );
        core.plan(&tree(session, vec![root(NodeState::Working, None)]), 2_000);
        let finished = core.plan(&tree(session, vec![root(NodeState::Idle, None)]), 12_000);
        assert_eq!(events(&finished), [HookEvent::TurnFinished]);
        assert_eq!(finished[0].payload.duration_ms, Some(10_000));
        assert_eq!(finished[0].payload.state.as_deref(), Some("completed"));
        assert!(finished[0].via_tree);
        assert_eq!(finished[0].harness.as_deref(), Some("claude"));
        // A failed turn.
        core.plan(&tree(session, vec![root(NodeState::Working, None)]), 20_000);
        let failed = core.plan(
            &tree(
                session,
                vec![root(NodeState::Idle, Some(NodeOutcome::Failed))],
            ),
            21_000,
        );
        assert_eq!(events(&failed), [HookEvent::TurnFailed]);
        // An interrupted one is neither; neither is a subagent's end.
        core.plan(&tree(session, vec![root(NodeState::Working, None)]), 30_000);
        assert!(
            core.plan(
                &tree(
                    session,
                    vec![root(NodeState::Idle, Some(NodeOutcome::Interrupted))]
                ),
                31_000
            )
            .is_empty()
        );
        let mut sub = node("sub", NodeKind::Sub, NodeState::Working);
        core.plan(&tree(session, vec![sub.clone()]), 40_000);
        sub.state = NodeState::Idle;
        assert!(core.plan(&tree(session, vec![sub]), 41_000).is_empty());
    }

    #[test]
    fn codex_chats_tree_never_makes_a_second_turn_event() {
        let mut core = Core::default();
        let session = SessionId::new();
        let mut working = node("root", NodeKind::Main, NodeState::Working);
        working.source = hutan_agents::codex::TREE_SOURCE.to_owned();
        core.plan(&tree(session, vec![working.clone()]), 1);
        working.state = NodeState::Idle;
        assert!(core.plan(&tree(session, vec![working]), 2).is_empty());
    }

    #[test]
    fn a_chat_turn_reports_its_duration_and_the_number_of_files_it_changed() {
        let mut core = Core::default();
        let session = SessionId::new();
        core.plan(&chat_event(session, AgentEventKind::TurnStarted), 1);
        let change = |path: &str| hutan_core::agent::FileChange {
            path: path.to_owned(),
            kind: hutan_core::agent::FileChangeKind::Update,
            move_path: None,
            patch: None,
            before: None,
            after: None,
        };
        for paths in [vec!["a.rs", "b.rs"], vec!["a.rs", "c.rs"]] {
            core.plan(
                &chat_event(
                    session,
                    AgentEventKind::FileDiff {
                        changes: paths.into_iter().map(change).collect(),
                        patch: None,
                    },
                ),
                2,
            );
        }
        let done = core.plan(
            &chat_event(
                session,
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(5_000),
                },
            ),
            3,
        );
        assert_eq!(events(&done), [HookEvent::TurnFinished]);
        assert_eq!(done[0].payload.duration_ms, Some(5_000));
        assert_eq!(done[0].payload.changed_files, Some(3));
        assert!(!done[0].via_tree);
        // The next turn starts from nothing.
        let next = core.plan(
            &chat_event(session, AgentEventKind::TurnCompleted { duration_ms: None }),
            4,
        );
        assert_eq!(next[0].payload.changed_files, None);
        let failed = core.plan(
            &chat_event(
                session,
                AgentEventKind::TurnFailed {
                    message: "secret=abcdefghijklmnopqrstuvwxyz0123456789 broke".to_owned(),
                    details: None,
                },
            ),
            5,
        );
        assert_eq!(events(&failed), [HookEvent::TurnFailed]);
        // The failure's message is never in the payload.
        assert!(
            !serde_json::to_string(&failed[0].payload)
                .unwrap()
                .contains("broke")
        );
    }

    #[test]
    fn replayed_chat_events_and_cancelled_turns_make_nothing() {
        let mut core = Core::default();
        let session = SessionId::new();
        let mut replay = chat_event(session, AgentEventKind::TurnCompleted { duration_ms: None });
        if let Event::ChatEvent { event, .. } = &mut replay {
            event.delivery = Delivery::Replay;
        }
        assert!(core.plan(&replay, 1).is_empty());
        assert!(
            core.plan(&chat_event(session, AgentEventKind::TurnCancelled), 2)
                .is_empty()
        );
    }

    #[test]
    fn an_approval_is_requested_with_a_redacted_bounded_title_and_raises_attention() {
        let mut core = Core::default();
        let session = SessionId::new();
        let info = approval(session);
        let planned = core.plan(
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            5,
        );
        assert_eq!(
            events(&planned),
            [HookEvent::ApprovalRequested, HookEvent::AttentionChanged]
        );
        let payload = &planned[0].payload;
        assert_eq!(
            payload.approval_id.as_deref(),
            Some(info.id.to_string().as_str())
        );
        let json = serde_json::to_string(payload).unwrap();
        assert!(
            !json.contains("abcdefghijklmnopqrstuvwxyz0123456789"),
            "{json}"
        );
        assert!(!json.contains("the whole command line"));
        assert_eq!(planned[1].payload.needs_attention, Some(true));
        // A second approval of the session does not say it again.
        let second = core.plan(
            &Event::ApprovalRequired {
                approval: Box::new(approval(session)),
            },
            6,
        );
        assert_eq!(events(&second), [HookEvent::ApprovalRequested]);
        // Only the last answer clears it.
        let resolved = |id| Event::ApprovalResolved {
            approval_id: id,
            session_id: session,
            outcome: hutan_core::agent::PermissionOutcome::Expired,
            decided_by: None,
        };
        assert!(core.plan(&resolved(info.id), 7).is_empty());
        let ids: Vec<ApprovalId> = core.attention[&session].approvals.iter().copied().collect();
        let clear = core.plan(&resolved(ids[0]), 8);
        assert_eq!(events(&clear), [HookEvent::AttentionChanged]);
        assert_eq!(clear[0].payload.needs_attention, Some(false));
        assert_eq!(clear[0].payload.state.as_deref(), Some("clear"));
        assert!(core.attention.is_empty());
    }

    #[test]
    fn a_blocked_agent_node_raises_attention_until_it_is_unblocked_or_gone() {
        let mut core = Core::default();
        let session = SessionId::new();
        let blocked = |id: &str, kind| node(id, kind, NodeState::Blocked);
        let up = core.plan(&tree(session, vec![blocked("root", NodeKind::Main)]), 1);
        assert_eq!(events(&up), [HookEvent::AttentionChanged]);
        // A second blocked node changes nothing; unblocking one of two changes nothing.
        assert!(
            core.plan(&tree(session, vec![blocked("sub", NodeKind::Sub)]), 2)
                .is_empty()
        );
        assert!(
            core.plan(
                &tree(
                    session,
                    vec![node("root", NodeKind::Main, NodeState::Working)]
                ),
                3
            )
            .is_empty()
        );
        // The last one going away clears it.
        let down = core.plan(
            &Event::AgentTreeUpdated {
                session_id: session,
                nodes_changed: Vec::new(),
                nodes_removed: vec!["sub".to_owned()],
            },
            4,
        );
        assert_eq!(events(&down), [HookEvent::AttentionChanged]);
        assert_eq!(down[0].payload.needs_attention, Some(false));
        // A node that is already gone is not waiting for anybody.
        let mut gone = blocked("root", NodeKind::Main);
        gone.liveness = Liveness::Gone;
        assert!(core.plan(&tree(session, vec![gone]), 5).is_empty());
    }

    #[test]
    fn a_session_that_exits_clears_its_attention_and_says_how() {
        let mut core = Core::default();
        let session = SessionId::new();
        core.plan(
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Blocked)],
            ),
            1,
        );
        let planned = core.plan(
            &Event::SessionExited {
                session_id: session,
                code: Some(3),
                signal: None,
            },
            2,
        );
        assert_eq!(
            events(&planned),
            [HookEvent::AttentionChanged, HookEvent::SessionExited]
        );
        assert_eq!(planned[1].payload.exit_code, Some(3));
        assert_eq!(planned[1].payload.state.as_deref(), Some("exited"));
        let signaled = core.plan(
            &Event::SessionExited {
                session_id: session,
                code: None,
                signal: Some(9),
            },
            3,
        );
        assert_eq!(signaled[0].payload.state.as_deref(), Some("signaled"));
        assert_eq!(signaled[0].payload.detail.as_deref(), Some("signal 9"));
        assert!(core.nodes.is_empty() && core.attention.is_empty());
    }

    fn task(state: TaskState) -> hutan_core::Task {
        hutan_core::Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: "Ship it".to_owned(),
            prompt: Some("a long prompt that is never sent".to_owned()),
            state,
            depends_on: Vec::new(),
            placement: hutan_core::TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: vec![SessionId::new()],
            created_at_ms: 0,
            started_at_ms: Some(1_000),
            ended_at_ms: Some(61_000),
            outcome: Some("exited 0".to_owned()),
            delivery: Default::default(),
            after_policy: Default::default(),
            delivery_history: Vec::new(),
        }
    }

    #[test]
    fn a_task_becoming_done_is_one_event_with_its_title_and_outcome() {
        let mut core = Core::default();
        let mut running = task(TaskState::Running);
        assert!(
            core.plan(
                &Event::TaskUpdated {
                    task: running.clone()
                },
                62_000
            )
            .is_empty()
        );
        running.state = TaskState::Done;
        let planned = core.plan(
            &Event::TaskUpdated {
                task: running.clone(),
            },
            62_000,
        );
        assert_eq!(events(&planned), [HookEvent::TaskDone]);
        let payload = &planned[0].payload;
        assert_eq!(payload.task_title.as_deref(), Some("Ship it"));
        assert_eq!(payload.detail.as_deref(), Some("exited 0"));
        assert_eq!(payload.duration_ms, Some(60_000));
        assert_eq!(payload.session_id, running.sessions.last().copied());
        assert!(
            !serde_json::to_string(payload)
                .unwrap()
                .contains("never sent")
        );
        // The same state again (a title edit) is not a second finish; a failure is none.
        assert!(
            core.plan(
                &Event::TaskUpdated {
                    task: running.clone()
                },
                63_000
            )
            .is_empty()
        );
        let mut failed = task(TaskState::Failed);
        failed.id = running.id;
        assert!(
            core.plan(&Event::TaskUpdated { task: failed }, 64_000)
                .is_empty()
        );
        // A task first seen done counts only when it ended just now.
        let mut old = task(TaskState::Done);
        old.ended_at_ms = Some(1);
        assert!(
            core.plan(&Event::TaskUpdated { task: old }, 10_000_000)
                .is_empty()
        );
    }

    fn hook(on: HookEvent) -> HookSettings {
        HookSettings {
            name: None,
            on,
            harness: None,
            project: None,
            workspace: None,
            run: vec!["true".to_owned()],
            shell: false,
            timeout: 30,
            when_focused: true,
        }
    }

    #[test]
    fn filters_match_names_and_ids_ignoring_case_and_need_the_thing_they_name() {
        let project = WorkspaceId::new();
        let mut payload = HookPayload::new(HookEvent::TurnFinished, 1);
        payload.harness = Some("Claude".to_owned());
        payload.project = Some("Hutan".to_owned());
        payload.project_id = Some(project);
        assert!(matches(&hook(HookEvent::TurnFinished), &payload));
        assert!(!matches(&hook(HookEvent::TurnFailed), &payload));
        let mut by = hook(HookEvent::TurnFinished);
        by.harness = Some("claude".to_owned());
        by.project = Some("hutan".to_owned());
        assert!(matches(&by, &payload));
        by.project = Some(project.to_string());
        assert!(matches(&by, &payload));
        by.project = Some("other".to_owned());
        assert!(!matches(&by, &payload));
        // The payload names no workspace: a hook that wants one does not match.
        let mut ws = hook(HookEvent::TurnFinished);
        ws.workspace = Some("work".to_owned());
        assert!(!matches(&ws, &payload));
    }

    #[test]
    fn the_environment_names_the_event_and_every_scalar_field_on_one_line() {
        let mut payload = HookPayload::new(HookEvent::TurnFinished, 7);
        payload.session_title = Some("line\none\0".to_owned());
        payload.changed_files = Some(3);
        payload.needs_attention = Some(true);
        let env: BTreeMap<String, String> = payload_env(&payload).into_iter().collect();
        assert_eq!(env["HUTAN_EVENT"], "turn.finished");
        assert_eq!(env["HUTAN_EVENT_TIME_MS"], "7");
        assert_eq!(env["HUTAN_EVENT_SCHEMA"], "1");
        assert_eq!(env["HUTAN_EVENT_CHANGED_FILES"], "3");
        assert_eq!(env["HUTAN_EVENT_NEEDS_ATTENTION"], "true");
        assert_eq!(env["HUTAN_EVENT_SESSION_TITLE"], "line one ");
        assert!(!env.contains_key("HUTAN_EVENT_HARNESS"));
    }

    #[test]
    fn output_is_redacted_before_it_is_cut_and_bounded() {
        let secret = "abcdefghijklmnopqrstuvwxyz0123456789";
        let noisy = format!("{}token={secret} tail", "x".repeat(OUTPUT_MAX_BYTES - 10));
        let (text, cut) = finish_output(noisy.as_bytes(), false);
        assert!(text.len() <= OUTPUT_MAX_BYTES);
        assert!(cut);
        assert!(!text.contains(&secret[..12]), "{text}");
        let (short, cut) = finish_output(b"ok\x1b[31m\n", false);
        assert_eq!(short, "ok[31m\n");
        assert!(!cut);
        assert!(finish_output(b"", true).1);
    }
}
