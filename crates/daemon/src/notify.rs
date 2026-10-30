//! Notification intents (protocol 0.13, capability `notify.intents`): the daemon decides *that*
//! something deserves the user's attention, clients decide how to show it.
//!
//! Intents are derived from the event stream, never from polling:
//!
//! | kind | raised when |
//! |---|---|
//! | `approval_required` | `approval.required`; answered by `notify.act` through `approval.respond` |
//! | `question_required` | `question.required` (protocol 0.24); "Open" only, the form is answered in the chat; withdrawn when the question is resolved |
//! | `agent_blocked` | a node of a session's agent tree becomes `blocked` |
//! | `agent_finished` | the main agent goes `working` → `idle` with outcome `completed` after `min_turn_secs` |
//! | `session_exited_with_error` | a session's process exits with a non-zero code |
//! | `chat_turn_finished` | a chat turn completes |
//! | `task_done` / `task_failed` | a task (`task.updated`) changes into `done` / `failed`, for the session that ran it; exempt from the rate limit |
//!
//! Every candidate passes the same gate: the `[notify]` settings (`enabled`, `kinds`, and
//! `finished` for finished work), the session's mute (`notify.mute`, protocol 0.34), the focus
//! (a session some client has focused raises nothing: its card is on screen), dedup by
//! `(kind, session, node)` against the pending intents, and, for the finished turns of one
//! session only, a burst limit of one intent per [`FINISHED_BURST_MS`]: requests and failures
//! always call. Only they play the system's sound. Pending intents are kept in memory, at
//! most [`MAX_PENDING`]; they are lost with the daemon, which is right for something that says
//! "look now". An intent leaves the list when it is acknowledged, acted on, expires, (approvals,
//! questions) is resolved, or its session is focused or muted.
//!
//! [`Core`] is the whole decision logic, synchronous and driven by explicit timestamps, so its
//! rules are unit-tested without a clock; [`NotifyService`] feeds it from the event bus and
//! answers the requests.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hutan_core::agent::{Delivery, MessageRole, PermissionOptionKind};
use hutan_core::{
    AgentEventKind, AgentNode, ApprovalId, Liveness, NodeKind, NodeOutcome, NodeState, QuestionId,
    SessionId, Task, TaskId, TaskState, WorkspaceId,
};
use hutan_protocol::chat::{ApprovalInfo, QuestionInfo};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{ClientKind, Event, SessionListFilter};
use hutan_protocol::notify::{
    ACTION_OPEN, ActRoute, BODY_MAX_CHARS, IntentAction, IntentKind, NotificationIntent, NotifyAct,
    NotifyActed, TITLE_MAX_CHARS,
};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use ulid::Ulid;

use crate::bus::EventBus;
use crate::config::{NotifySettings, NotifySound};
use crate::conn::ConnId;
use crate::notify_native::{
    self, NativeNotification, NativeOp, NativePoster, NativeResponse, ResponseAction,
};
use crate::registry::{RegistryHandle, now_ms};

/// A session's finished turns come in bursts: after one raised an intent (`agent_finished`,
/// `chat_turn_finished`), the others of the session within this long raise none.
pub const FINISHED_BURST_MS: u64 = 90_000;

/// How hard an intent calls for the user (UX-ATTN-3, the three groups of the desktop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Urgency {
    /// Something waits for an answer: a *Request*. Never held back, plays the sound.
    Request,
    /// Something failed. Never held back, plays the sound.
    Failure,
    /// Work is finished: *Ready for review*. Quiet (no sound), at most once per burst, and
    /// switched off by `notify.finished`.
    Finished,
}

impl Urgency {
    const fn of(kind: IntentKind) -> Self {
        match kind {
            IntentKind::ApprovalRequired
            | IntentKind::QuestionRequired
            | IntentKind::AgentBlocked => Self::Request,
            IntentKind::SessionExitedWithError | IntentKind::TaskFailed => Self::Failure,
            IntentKind::AgentFinished
            | IntentKind::ChatTurnFinished
            | IntentKind::TaskDone
            | IntentKind::Digest => Self::Finished,
        }
    }
}

/// Most pending intents; the oldest is dropped to make room.
pub const MAX_PENDING: usize = 100;
/// Expired intents are swept out at least this often (their native notifications go with them).
const SWEEP_MAX_SECS: u64 = 30;
/// Queue between the event bus and the service task.
pub const OBSERVER_QUEUE: usize = 1024;
/// Per-node memory of the agent trees is dropped when it grows past this many entries (a safety
/// net: trees are bounded and report their removals).
const MAX_MEMOS: usize = 4096;
/// A task seen for the first time that ended less than this long ago raises its intent (what a
/// daemon restart makes of a task that was running).
const TASK_FRESH_MS: u64 = 60_000;

/// What the session is called in an intent, where it lives, and whether the user muted it.
#[derive(Debug, Clone, Default)]
struct SessionMeta {
    title: Option<String>,
    workspace_id: Option<WorkspaceId>,
    muted: bool,
}

/// Something that may become an intent: everything but the session's title, which is looked up
/// asynchronously between planning and offering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    kind: IntentKind,
    session: SessionId,
    /// The third part of the dedup key: an approval, node or turn id.
    node: String,
    /// Said after the session's title: `"{title}: {headline}"`.
    headline: String,
    body: String,
    actions: Vec<IntentAction>,
    approval: Option<ApprovalId>,
    /// The question this intent is about (`question_required`): it lives as long as the
    /// question does.
    question: Option<QuestionId>,
    /// The ids of the actions that refuse (an approval's deny and cancel kinds); the native
    /// notification shows them in the destructive style.
    destructive: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    intent: NotificationIntent,
    /// The dedup key's node part.
    node: String,
    approval: Option<ApprovalId>,
    question: Option<QuestionId>,
    destructive: Vec<String>,
}

/// What is remembered of an agent tree node between two `agent.tree_updated` events.
#[derive(Debug, Clone, Copy)]
struct NodeMemo {
    state: NodeState,
    /// When the node was first seen working in the turn that is going on.
    working_since: Option<u64>,
}

/// The decision logic: settings, pending intents, per-session rate limits, node memory, focus.
#[derive(Debug)]
struct Core {
    settings: NotifySettings,
    pending: VecDeque<Entry>,
    /// When the last finished-turn intent of each session was raised (`FINISHED_BURST_MS`).
    last_finished: HashMap<SessionId, u64>,
    nodes: HashMap<(SessionId, String), NodeMemo>,
    /// The latest finished assistant message of each chat, already redacted and bounded.
    last_message: HashMap<SessionId, String>,
    /// The state last seen of each task (a task raises its intent on the change into
    /// `done` or `failed`).
    tasks: HashMap<TaskId, TaskState>,
    focus: HashMap<ConnId, SessionId>,
    /// Ids of the intents that left `pending` (acknowledged, acted on, expired, resolved,
    /// pushed out) since the service last took them: their native notifications go too.
    withdrawn: Vec<String>,
}

impl Core {
    fn new(settings: NotifySettings) -> Self {
        Self {
            settings,
            pending: VecDeque::new(),
            withdrawn: Vec::new(),
            last_finished: HashMap::new(),
            nodes: HashMap::new(),
            last_message: HashMap::new(),
            tasks: HashMap::new(),
            focus: HashMap::new(),
        }
    }

    /// Whether the system's notification for an intent of `kind` plays a sound: only a request
    /// or a failure does (and only while `notify.sound` allows it); finished work is a banner.
    fn plays_sound(&self, kind: IntentKind) -> bool {
        self.settings.sound != NotifySound::None && Urgency::of(kind) != Urgency::Finished
    }

    // Planning --------------------------------------------------------------------------------

    /// Folds one event in and returns what it may raise.
    fn plan(&mut self, event: &Event, now_ms: u64) -> Vec<Candidate> {
        if !self.settings.enabled {
            return Vec::new();
        }
        match event {
            Event::ApprovalRequired { approval } => vec![approval_candidate(approval)],
            Event::ApprovalResolved { approval_id, .. } => {
                self.withdraw_where(|e| e.approval == Some(*approval_id));
                Vec::new()
            }
            Event::QuestionRequired { question } => vec![question_candidate(question)],
            Event::QuestionResolved { question_id, .. } => {
                self.withdraw_where(|e| e.question == Some(*question_id));
                Vec::new()
            }
            Event::AgentTreeUpdated {
                session_id,
                nodes_changed,
                nodes_removed,
            } => self.plan_tree(*session_id, nodes_changed, nodes_removed, now_ms),
            Event::SessionExited {
                session_id, code, ..
            } => {
                self.forget_session(*session_id);
                match code {
                    Some(code) if *code != 0 => vec![Candidate {
                        kind: IntentKind::SessionExitedWithError,
                        session: *session_id,
                        node: "exit".to_owned(),
                        headline: "exited with an error".to_owned(),
                        body: format!("The process exited with code {code}."),
                        actions: vec![open_action()],
                        approval: None,
                        question: None,
                        destructive: Vec::new(),
                    }],
                    _ => Vec::new(),
                }
            }
            Event::SessionClosed { session_id } => {
                // Nothing is left to open: what was raised about the session goes with it.
                self.forget_session(*session_id);
                self.last_finished.remove(session_id);
                let session = *session_id;
                self.withdraw_where(|e| e.intent.session_id == session);
                Vec::new()
            }
            Event::ChatEvent { session_id, event } if event.delivery == Delivery::Live => {
                match &event.kind {
                    AgentEventKind::TurnStarted => {
                        self.last_message.remove(session_id);
                        Vec::new()
                    }
                    AgentEventKind::MessageEnd {
                        role: MessageRole::Assistant,
                        text,
                        ..
                    } => {
                        self.last_message
                            .insert(*session_id, clean(text, BODY_MAX_CHARS));
                        Vec::new()
                    }
                    AgentEventKind::TurnCompleted { .. } => {
                        let body = self
                            .last_message
                            .remove(session_id)
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| "The turn finished.".to_owned());
                        let node = event
                            .turn_id
                            .map_or_else(|| format!("seq-{}", event.seq), |turn| turn.to_string());
                        vec![Candidate {
                            kind: IntentKind::ChatTurnFinished,
                            session: *session_id,
                            node,
                            headline: "reply ready".to_owned(),
                            body,
                            actions: vec![open_action()],
                            approval: None,
                            question: None,
                            destructive: Vec::new(),
                        }]
                    }
                    _ => Vec::new(),
                }
            }
            Event::TaskUpdated { task } => self.plan_task(task, now_ms),
            _ => Vec::new(),
        }
    }

    fn plan_tree(
        &mut self,
        session: SessionId,
        changed: &[AgentNode],
        removed: &[String],
        now_ms: u64,
    ) -> Vec<Candidate> {
        let min_turn_ms = self.settings.min_turn_secs.saturating_mul(1000);
        let mut raised = Vec::new();
        for node in changed {
            let key = (session, node.node_id.clone());
            let before = self.nodes.get(&key).copied();
            let is_root = node.kind == NodeKind::Main && node.parent.is_none();
            let who = if is_root {
                node.agent.as_str()
            } else {
                node.label()
            };
            // A turn runs from the first `working` to the next `idle`, blocked stretches
            // included.
            let working_since = match node.state {
                NodeState::Working => Some(before.and_then(|b| b.working_since).unwrap_or(now_ms)),
                NodeState::Blocked => before.and_then(|b| b.working_since),
                NodeState::Idle | NodeState::Unknown => None,
            };
            if node.state == NodeState::Blocked
                && node.liveness != Liveness::Gone
                && before.map(|b| b.state) != Some(NodeState::Blocked)
            {
                raised.push(Candidate {
                    kind: IntentKind::AgentBlocked,
                    session,
                    node: node.node_id.clone(),
                    headline: format!("{who} is waiting for you"),
                    body: node
                        .activity
                        .as_deref()
                        .map(|activity| clean(activity, BODY_MAX_CHARS))
                        .filter(|text| !text.is_empty())
                        .unwrap_or_else(|| "It needs a decision from you.".to_owned()),
                    actions: vec![open_action()],
                    approval: None,
                    question: None,
                    destructive: Vec::new(),
                });
            }
            // A normal stop is a finish: a plain idle (the Stop hooks, OMP `agent_end`) or an
            // explicit `completed`. Failures, interruptions and lost contact are not. Hutan's own
            // Codex chats announce their turns as `chat_turn_finished` instead.
            if is_root
                && node.state == NodeState::Idle
                && matches!(node.outcome, None | Some(NodeOutcome::Completed))
                && node.source != hutan_agents::codex::TREE_SOURCE
                && let Some(since) = before.and_then(|b| b.working_since)
                && now_ms.saturating_sub(since) >= min_turn_ms
            {
                raised.push(Candidate {
                    kind: IntentKind::AgentFinished,
                    session,
                    node: node.node_id.clone(),
                    headline: format!("{who} finished"),
                    body: format!(
                        "Finished after {}.",
                        human_duration(now_ms.saturating_sub(since))
                    ),
                    actions: vec![open_action()],
                    approval: None,
                    question: None,
                    destructive: Vec::new(),
                });
            }
            self.nodes.insert(
                key,
                NodeMemo {
                    state: node.state,
                    working_since,
                },
            );
        }
        for id in removed {
            self.nodes.remove(&(session, id.clone()));
        }
        if self.nodes.len() > MAX_MEMOS {
            self.nodes.clear();
        }
        raised
    }

    /// A task that has just ended raises `task_done` / `task_failed` for the session that ran
    /// it. Only the change into the state counts: a title edited later does not; a task seen
    /// for the first time (the daemon restarted) counts when it ended just now.
    fn plan_task(&mut self, task: &Task, now_ms: u64) -> Vec<Candidate> {
        let before = self.tasks.insert(task.id, task.state);
        if self.tasks.len() > MAX_MEMOS {
            self.tasks.clear();
        }
        let (kind, headline, verb) = match task.state {
            TaskState::Done => (IntentKind::TaskDone, "task done", "Finished"),
            TaskState::Failed => (IntentKind::TaskFailed, "task failed", "Failed"),
            _ => return Vec::new(),
        };
        let fresh = match before {
            Some(state) => state != task.state,
            None => task
                .ended_at_ms
                .is_some_and(|at| now_ms.saturating_sub(at) < TASK_FRESH_MS),
        };
        let Some(session) = task.sessions.last().copied().filter(|_| fresh) else {
            return Vec::new();
        };
        // The task's intent says it all: the session's own "exited with an error" / "agent
        // finished" for the same run would be a second notification of one event.
        self.withdraw_where(|e| {
            e.intent.session_id == session
                && matches!(
                    e.intent.kind,
                    IntentKind::SessionExitedWithError | IntentKind::AgentFinished
                )
        });
        let outcome = task.outcome.as_deref().filter(|o| !o.is_empty());
        let body = match (task.started_at_ms, task.ended_at_ms) {
            (Some(start), Some(end)) => format!(
                "{verb} after {}{}.",
                human_duration(end.saturating_sub(start)),
                outcome.map_or_else(String::new, |o| format!(" ({o})"))
            ),
            _ => outcome.map_or_else(|| format!("{verb}."), |o| format!("{verb}: {o}.")),
        };
        vec![Candidate {
            kind,
            session,
            node: task.id.to_string(),
            headline: headline.to_owned(),
            body,
            actions: vec![open_action()],
            approval: None,
            question: None,
            destructive: Vec::new(),
        }]
    }

    /// A session ended: what is remembered about its agents and chat is of no use any more.
    fn forget_session(&mut self, session: SessionId) {
        self.nodes.retain(|(s, _), _| *s != session);
        self.last_message.remove(&session);
    }

    // Offering --------------------------------------------------------------------------------

    /// Applies the gate to a candidate; the intent when it passes (it is then pending).
    fn offer(&mut self, candidate: Candidate, meta: &SessionMeta, now_ms: u64) -> Option<Entry> {
        let settings = &self.settings;
        if !settings.enabled || !settings.kinds.contains(&candidate.kind) {
            return None;
        }
        self.expire(now_ms);
        let urgency = Urgency::of(candidate.kind);
        // Finished work is a quiet call: one setting turns it off for good.
        if urgency == Urgency::Finished && !self.settings.finished {
            return None;
        }
        // An approval and a question wait for the user: they live as long as what they are about
        // (the second of two in a row matters as much as the first).
        let approval = matches!(
            candidate.kind,
            IntentKind::ApprovalRequired | IntentKind::QuestionRequired
        );
        // Only a session's finished turns come in bursts: a request or a failure always calls,
        // and a task ends once, so a finished task is not lost because the session's own finish
        // raised an intent a moment before.
        let bursty = matches!(
            candidate.kind,
            IntentKind::AgentFinished | IntentKind::ChatTurnFinished
        );
        if meta.muted {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "the session is muted; no intent");
            return None;
        }
        // The user is looking at the session: its card (approval, question, reply) is on screen,
        // and a banner or a sound would only interrupt.
        if self.focus.values().any(|s| *s == candidate.session) {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "the session is focused; no intent");
            return None;
        }
        if self.pending.iter().any(|e| {
            e.intent.kind == candidate.kind
                && e.intent.session_id == candidate.session
                && e.node == candidate.node
        }) {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "duplicate intent dropped");
            return None;
        }
        if bursty
            && self
                .last_finished
                .get(&candidate.session)
                .is_some_and(|last| now_ms.saturating_sub(*last) < FINISHED_BURST_MS)
        {
            debug!(kind = candidate.kind.as_str(), session = %candidate.session, "finished intent held back: the session's burst");
            return None;
        }
        let title = clean(
            &format!(
                "{}: {}",
                meta.title.as_deref().unwrap_or("Session"),
                candidate.headline
            ),
            TITLE_MAX_CHARS,
        );
        let expires_at_ms = (!approval)
            .then(|| now_ms.saturating_add(self.settings.expire_secs.saturating_mul(1000)));
        let intent = NotificationIntent {
            id: Ulid::generate().to_string(),
            kind: candidate.kind,
            session_id: candidate.session,
            workspace_id: meta.workspace_id,
            title,
            body: clean(&candidate.body, BODY_MAX_CHARS),
            actions: candidate.actions,
            created_at_ms: now_ms,
            expires_at_ms,
        };
        if bursty {
            self.last_finished.insert(candidate.session, now_ms);
            if self.last_finished.len() > MAX_MEMOS {
                self.last_finished
                    .retain(|_, last| now_ms.saturating_sub(*last) < FINISHED_BURST_MS);
            }
        }
        let entry = Entry {
            intent,
            node: candidate.node,
            approval: candidate.approval,
            question: candidate.question,
            destructive: candidate.destructive,
        };
        self.pending.push_back(entry.clone());
        while self.pending.len() > MAX_PENDING {
            if let Some(dropped) = self.pending.pop_front() {
                self.withdrawn.push(dropped.intent.id);
            }
        }
        Some(entry)
    }

    // Pending intents -------------------------------------------------------------------------

    /// Takes every entry `gone` selects out of `pending`, noting it in `withdrawn`.
    fn withdraw_where(&mut self, mut gone: impl FnMut(&Entry) -> bool) {
        let withdrawn = &mut self.withdrawn;
        self.pending.retain(|e| {
            let go = gone(e);
            if go {
                withdrawn.push(e.intent.id.clone());
            }
            !go
        });
    }

    fn expire(&mut self, now_ms: u64) {
        self.withdraw_where(|e| e.intent.expires_at_ms.is_some_and(|at| at <= now_ms));
    }

    fn list(&mut self, now_ms: u64) -> Vec<NotificationIntent> {
        self.expire(now_ms);
        self.pending.iter().map(|e| e.intent.clone()).collect()
    }

    fn find(&mut self, id: &str, now_ms: u64) -> Option<Entry> {
        self.expire(now_ms);
        self.pending.iter().find(|e| e.intent.id == id).cloned()
    }

    /// Removes the intent; whether it was pending.
    fn remove(&mut self, id: &str) -> bool {
        let before = self.withdrawn.len();
        self.withdraw_where(|e| e.intent.id == id);
        self.withdrawn.len() != before
    }

    /// Withdraws every pending intent of `session` (it is looked at, or was muted).
    fn withdraw_session(&mut self, session: SessionId) {
        self.withdraw_where(|e| e.intent.session_id == session);
    }
}

fn open_action() -> IntentAction {
    IntentAction {
        id: ACTION_OPEN.to_owned(),
        label: "Open".to_owned(),
    }
}

fn approval_candidate(approval: &ApprovalInfo) -> Candidate {
    let body = match approval.detail.as_deref().filter(|d| !d.trim().is_empty()) {
        Some(detail) => format!("{}: {detail}", approval.title),
        None => approval.title.clone(),
    };
    Candidate {
        kind: IntentKind::ApprovalRequired,
        session: approval.session_id,
        node: approval.id.to_string(),
        headline: "approval needed".to_owned(),
        body,
        actions: approval
            .options
            .iter()
            .map(|option| IntentAction {
                id: option.id.clone(),
                label: option.label.clone(),
            })
            .collect(),
        approval: Some(approval.id),
        question: None,
        destructive: approval
            .options
            .iter()
            .filter(|option| {
                matches!(
                    option.kind,
                    PermissionOptionKind::RejectOnce
                        | PermissionOptionKind::RejectAlways
                        | PermissionOptionKind::Cancel
                )
            })
            .map(|option| option.id.clone())
            .collect(),
    }
}

/// A question of an agent: the intent says that there is one and opens the chat. Its fields are
/// answered there, never from a notification (a form does not fit a button).
fn question_candidate(question: &QuestionInfo) -> Candidate {
    let spec = &question.spec;
    let body = if spec.prompt.trim().is_empty() {
        // Markdown is no use in a banner: the first line of the prompt, or what the fields ask.
        spec.fields
            .iter()
            .map(|f| f.label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        spec.prompt
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .to_owned()
    };
    Candidate {
        kind: IntentKind::QuestionRequired,
        session: question.session_id,
        node: question.id.to_string(),
        headline: "question".to_owned(),
        body: if body.trim().is_empty() {
            spec.title.clone()
        } else {
            format!("{}: {body}", spec.title)
        },
        actions: vec![open_action()],
        approval: None,
        question: Some(question.id),
        destructive: Vec::new(),
    }
}

/// `text` as one redacted line of at most `max_chars` characters; a cut ends in `…`. Redaction
/// comes first and sees the whole text: cutting first could clip a quoted secret before its
/// closing quote, or a URL password before its `@`, and the prefix would then pass the masks.
pub(crate) fn clean(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let redacted = hutan_index::redact::redact(text);
    let mut words = redacted.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut chars = first
        .chars()
        .chain(words.flat_map(|word| std::iter::once(' ').chain(word.chars())));
    // One bounded UTF-8 buffer, including room for an ellipsis; do not normalize the whole reply.
    let capacity = max_chars
        .saturating_mul(4)
        .min(redacted.len().saturating_add(2));
    let mut line = String::with_capacity(capacity);
    line.extend(chars.by_ref().take(max_chars));
    if chars.next().is_some() {
        line.pop();
        line.push('…');
    }
    line
}

fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

/// The delivery task's inputs, held until [`NotifyService::start`] starts it.
#[derive(Debug)]
struct NativeParts {
    poster: Arc<dyn NativePoster>,
    ops: mpsc::Receiver<NativeOp>,
}

/// Feeds [`Core`] from the event bus and answers the `notify.*` requests; with a native poster
/// it also shows every raised intent as a system notification, withdraws it when the intent
/// goes, and answers its clicks like `notify.act` (`notify_native`).
#[derive(Debug)]
pub struct NotifyService {
    core: Mutex<Core>,
    bus: Arc<EventBus>,
    registry: RegistryHandle,
    /// Posts and removals for the delivery task; `None` posts nothing.
    native: Option<mpsc::Sender<NativeOp>>,
    native_parts: Mutex<Option<NativeParts>>,
    /// How often expired intents are swept out so that their notifications go too.
    sweep: Duration,
    /// Told when intents leave the pending list (acknowledged, acted on, expired, resolved,
    /// pushed out), which no event announces: the menu bar item lists what is pending.
    changed: OnceLock<mpsc::Sender<()>>,
}

impl NotifyService {
    /// `poster` is the system's notification centre in a daemon that runs from `Hutan.app`;
    /// it is ignored under `[notify] native = false`.
    pub fn new(
        settings: NotifySettings,
        bus: Arc<EventBus>,
        registry: RegistryHandle,
        poster: Option<Arc<dyn NativePoster>>,
    ) -> Arc<Self> {
        let poster = match poster {
            Some(_) if !settings.native => {
                info!("native notifications are disabled by [notify] native = false");
                None
            }
            poster => poster,
        };
        let sweep = Duration::from_secs(settings.expire_secs.clamp(1, SWEEP_MAX_SECS));
        let (native, native_parts) = match poster {
            Some(poster) => {
                let (tx, ops) = mpsc::channel(notify_native::OPS_QUEUE);
                (Some(tx), Some(NativeParts { poster, ops }))
            }
            None => (None, None),
        };
        Arc::new(Self {
            core: Mutex::new(Core::new(settings)),
            bus,
            registry,
            native,
            native_parts: Mutex::new(native_parts),
            sweep,
            changed: OnceLock::new(),
        })
    }

    /// Pokes `changed` whenever intents leave the pending list, and sweeps for expired ones
    /// even without native notifications. Call before [`start`](Self::start); once.
    pub fn set_changed(&self, changed: mpsc::Sender<()>) {
        let _ = self.changed.set(changed);
    }

    /// The settings intents are raised by now.
    pub fn settings(&self) -> NotifySettings {
        self.core.lock().settings.clone()
    }

    /// Raises intents by `settings` from now on (`daemon.settings.set` changed `[notify]`);
    /// the intents that are pending stay. `native` and the sweep interval were fixed at start.
    pub fn apply_settings(&self, settings: NotifySettings) {
        self.core.lock().settings = settings;
    }

    /// Starts watching the event bus until `shutdown`.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        if let Some(NativeParts { poster, ops }) = self.native_parts.lock().take() {
            match poster.responses() {
                Some(responses) => {
                    info!("native notifications on");
                    tokio::spawn(notify_native::run(
                        Arc::clone(self),
                        poster,
                        ops,
                        responses,
                        shutdown.clone(),
                    ));
                }
                None => warn!("the native notification poster has no responses; none are posted"),
            }
        }
        let mut sweep = (self.native.is_some() || self.changed.get().is_some()).then(|| {
            let mut interval = tokio::time::interval(self.sweep);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            interval
        });
        let (tx, mut rx) = mpsc::channel(OBSERVER_QUEUE);
        self.bus.set_observer(Some(tx));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    event = rx.recv() => match event {
                        Some(event) => this.handle(event).await,
                        None => break,
                    },
                    () = async {
                        match sweep.as_mut() {
                            Some(interval) => { interval.tick().await; }
                            None => std::future::pending().await,
                        }
                    } => this.with_core(|core| core.expire(now_ms())),
                }
            }
            this.bus.set_observer(None);
        })
    }

    /// Runs `f` on the core. What it withdrew (acknowledged, expired, resolved, pushed out) is
    /// taken off the screen afterwards.
    fn with_core<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        let (result, withdrawn) = {
            let mut core = self.core.lock();
            let result = f(&mut core);
            (result, std::mem::take(&mut core.withdrawn))
        };
        if !withdrawn.is_empty()
            && let Some(changed) = self.changed.get()
        {
            // Full: a poke is waiting already.
            let _ = changed.try_send(());
        }
        for id in withdrawn {
            self.native_send(NativeOp::Remove(id));
        }
        result
    }

    fn native_send(&self, op: NativeOp) {
        if let Some(native) = &self.native
            && let Err(error) = native.try_send(op)
        {
            warn!(%error, "the native notification queue is full or closed; an operation was dropped");
        }
    }

    async fn handle(&self, event: Event) {
        let candidates = self.with_core(|core| core.plan(&event, now_ms()));
        for candidate in candidates {
            let meta = self.meta(candidate.session).await;
            // The post is queued under the core's lock: a withdrawal of this very intent can
            // only be queued after it, never before.
            let raised = self.with_core(|core| {
                let entry = core.offer(candidate, &meta, now_ms())?;
                let silent = !core.plays_sound(entry.intent.kind);
                self.native_send(NativeOp::Post(
                    NativeNotification::from_intent(&entry.intent, &entry.destructive)
                        .silent(silent),
                ));
                Some(entry)
            });
            if let Some(entry) = raised
                && let Err(error) = self.bus.publish(&Event::NotifyIntent {
                    intent: entry.intent,
                })
            {
                warn!(%error, "could not publish a notification intent");
            }
        }
    }

    async fn meta(&self, session: SessionId) -> SessionMeta {
        match self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        {
            Ok(list) => list
                .sessions
                .into_iter()
                .find(|s| s.id == session)
                .map(|s| SessionMeta {
                    title: Some(s.title),
                    workspace_id: Some(s.workspace_id),
                    muted: s.muted,
                })
                .unwrap_or_default(),
            Err(error) => {
                debug!(message = %error.message, "session lookup for an intent failed");
                SessionMeta::default()
            }
        }
    }

    /// `notify.list`.
    pub fn list(&self) -> Vec<NotificationIntent> {
        self.with_core(|core| core.list(now_ms()))
    }

    /// `notify.ack`.
    pub fn ack(&self, id: &str) -> Result<(), ErrorBody> {
        self.with_core(|core| {
            core.expire(now_ms());
            core.remove(id)
        })
        .then_some(())
        .ok_or_else(|| not_found(id))
    }

    /// `notify.act`.
    pub async fn act(
        &self,
        client: Option<ClientKind>,
        req: NotifyAct,
    ) -> Result<NotifyActed, ErrorBody> {
        let entry = self
            .with_core(|core| core.find(&req.id, now_ms()))
            .ok_or_else(|| not_found(&req.id))?;
        if !entry.intent.actions.iter().any(|a| a.id == req.action_id) {
            let offered: Vec<&str> = entry.intent.actions.iter().map(|a| a.id.as_str()).collect();
            let mut error = ErrorBody::new(
                ErrorCode::OptionNotOffered,
                format!(
                    "action `{}` is not offered; offered: {}",
                    req.action_id,
                    offered.join(", ")
                ),
            );
            error.data = Some(serde_json::json!({ "offered": offered }));
            return Err(error);
        }
        match entry.approval {
            Some(approval) => {
                let answered = self
                    .registry
                    .chats()
                    .respond(approval, req.action_id, client)
                    .await;
                match answered {
                    Ok(()) => {}
                    // The approval is gone (answered elsewhere, its turn ended): the intent is
                    // stale, whatever the caller does about the error.
                    Err(error) if error.code == ErrorCode::NotFound => {
                        self.with_core(|core| core.remove(&req.id));
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                }
                self.with_core(|core| core.remove(&req.id));
                Ok(NotifyActed {
                    route: ActRoute::Approval,
                    desktop_clients: 0,
                })
            }
            None => {
                self.with_core(|core| core.remove(&req.id));
                Ok(NotifyActed {
                    route: ActRoute::UiOpen,
                    desktop_clients: self.publish_open(&entry)?,
                })
            }
        }
    }

    /// Publishes `ui.open` for the intent's session; how many desktop clients it reached.
    fn publish_open(&self, entry: &Entry) -> Result<u32, ErrorBody> {
        let reached = self.bus.subscriber_count_of(ClientKind::Desktop);
        self.bus
            .publish(&Event::UiOpen {
                session_id: entry.intent.session_id,
                workspace_id: entry.intent.workspace_id,
                at_ms: now_ms(),
                // A digest's *Open* shows what happened, not just the chat.
                view: (entry.intent.kind == IntentKind::Digest)
                    .then_some(hutan_protocol::notify::UiView::MawasRecent),
            })
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))?;
        Ok(u32::try_from(reached).unwrap_or(u32::MAX))
    }

    /// A click on a native notification, routed like `notify.act` from a `notifier` client:
    /// an approval's button answers the approval, *Open* (the button or the body) shows the
    /// session, and a dismissal acknowledges the intent.
    pub(crate) async fn native_response(&self, response: NativeResponse) {
        let NativeResponse { intent_id, action } = response;
        let outcome = match action {
            ResponseAction::Action(action_id) => self
                .act(
                    Some(ClientKind::Notifier),
                    NotifyAct {
                        id: intent_id.clone(),
                        action_id,
                    },
                )
                .await
                .map(|_| ()),
            ResponseAction::Open => self.open(&intent_id),
            ResponseAction::Dismiss => self.ack(&intent_id),
        };
        match outcome {
            Ok(()) => {}
            // Clicked after the intent was answered or expired: nothing to do.
            Err(error) if error.code == ErrorCode::NotFound => {
                debug!(intent = %intent_id, "a native notification outlived its intent");
            }
            Err(error) => {
                warn!(intent = %intent_id, code = ?error.code, message = %error.message, "a native notification click was not applied");
            }
        }
    }

    /// *Open* on an intent. Unlike `notify.act` this is also allowed on an approval's intent
    /// (its notification has the button next to the options): the session is shown and the
    /// approval stays pending, to be answered there.
    fn open(&self, id: &str) -> Result<(), ErrorBody> {
        let entry = self
            .with_core(|core| core.find(id, now_ms()))
            .ok_or_else(|| not_found(id))?;
        if entry.approval.is_none() {
            self.with_core(|core| core.remove(id));
        }
        self.publish_open(&entry).map(|_| ())
    }

    /// `client.focus`. A session that is reported is looked at: what it has pending is
    /// withdrawn (its banners leave the notification centre).
    pub fn set_focus(&self, conn: ConnId, session: Option<SessionId>) {
        self.with_core(|core| match session {
            Some(session) => {
                core.focus.insert(conn, session);
                core.withdraw_session(session);
            }
            None => {
                core.focus.remove(&conn);
            }
        });
    }

    /// `notify.mute` silenced `session`: what it has pending is withdrawn.
    pub fn session_muted(&self, session: SessionId) {
        self.with_core(|core| core.withdraw_session(session));
    }

    /// A connection closed: its focus goes with it.
    pub fn connection_closed(&self, conn: ConnId) {
        self.core.lock().focus.remove(&conn);
    }
}

fn not_found(id: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::NotFound,
        format!("notification {id} is not pending (acknowledged, expired, or never raised)"),
    )
}

#[cfg(test)]
mod tests {
    use hutan_core::agent::{PermissionKind, PermissionOption, PermissionOptionKind};
    use hutan_core::{AgentEvent, AgentEventDraft, TurnId};

    use super::*;

    fn core() -> Core {
        Core::new(NotifySettings {
            min_turn_secs: 20,
            ..NotifySettings::default()
        })
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            title: Some("api".to_owned()),
            workspace_id: Some(WorkspaceId::new()),
            muted: false,
        }
    }

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
            started_at_ms: Some(1),
            ended_at_ms: None,
            last_activity_ms: 1,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    /// A node that finished its turn as the daemon reports it: idle, outcome `completed`.
    fn completed(id: &str, kind: NodeKind) -> AgentNode {
        AgentNode {
            pid: None,
            outcome: Some(NodeOutcome::Completed),
            ..node(id, kind, NodeState::Idle)
        }
    }

    fn tree(session: SessionId, nodes: Vec<AgentNode>) -> Event {
        Event::AgentTreeUpdated {
            session_id: session,
            nodes_changed: nodes,
            nodes_removed: Vec::new(),
        }
    }

    fn approval(session: SessionId) -> ApprovalInfo {
        ApprovalInfo {
            id: ApprovalId::new(),
            session_id: session,
            turn_id: None,
            kind: PermissionKind::Command,
            title: "Run `rm -rf build`?".to_owned(),
            detail: Some("token=abcdef123456789 in the env".to_owned()),
            options: vec![
                PermissionOption {
                    id: "accept".to_owned(),
                    label: "Allow once".to_owned(),
                    kind: PermissionOptionKind::AllowOnce,
                    provider: None,
                },
                PermissionOption {
                    id: "cancel".to_owned(),
                    label: "Cancel".to_owned(),
                    kind: PermissionOptionKind::Cancel,
                    provider: None,
                },
            ],
            requested_at_ms: 1,
            target: None,
            suggestion: None,
        }
    }

    fn chat_event(session: SessionId, kind: AgentEventKind, seq: u64) -> Event {
        let mut draft = AgentEventDraft::new(kind);
        draft.turn_id = Some(TurnId::new());
        Event::ChatEvent {
            session_id: session,
            event: Box::new(draft.into_event(session, seq, 1)),
        }
    }

    fn turn_completed(session: SessionId, seq: u64) -> Event {
        chat_event(
            session,
            AgentEventKind::TurnCompleted { duration_ms: None },
            seq,
        )
    }

    /// Plans `event` and offers every candidate; the intents raised.
    fn raise(core: &mut Core, event: &Event, now: u64) -> Vec<NotificationIntent> {
        let meta = meta();
        core.plan(event, now)
            .into_iter()
            .filter_map(|c| core.offer(c, &meta, now).map(|entry| entry.intent))
            .collect()
    }

    #[test]
    fn a_muted_session_raises_nothing_of_any_kind() {
        let mut core = core();
        let session = SessionId::new();
        let muted = SessionMeta {
            muted: true,
            ..meta()
        };
        let events = [
            Event::ApprovalRequired {
                approval: Box::new(approval(session)),
            },
            tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Blocked)],
            ),
            turn_completed(session, 3),
        ];
        for event in &events {
            for candidate in core.plan(event, 1_000) {
                assert!(core.offer(candidate, &muted, 1_000).is_none());
            }
        }
        assert!(core.list(1_000).is_empty());
        // The same approval in a session that is not muted is raised.
        assert_eq!(raise(&mut core, &events[0], 1_000).len(), 1);
    }

    #[test]
    fn a_focused_session_raises_nothing_and_focusing_withdraws_what_it_had() {
        let mut core = core();
        let (looked_at, elsewhere) = (SessionId::new(), SessionId::new());
        let conn = ConnId(3);
        core.focus.insert(conn, looked_at);
        let asks = |session| Event::ApprovalRequired {
            approval: Box::new(approval(session)),
        };
        assert!(raise(&mut core, &asks(looked_at), 1_000).is_empty());
        let blocked = tree(
            looked_at,
            vec![node("root", NodeKind::Main, NodeState::Blocked)],
        );
        assert!(raise(&mut core, &blocked, 1_000).is_empty());
        assert_eq!(raise(&mut core, &asks(elsewhere), 1_000).len(), 1);

        // Looking at the other session takes its intents away, and only its.
        core.focus.remove(&conn);
        let kept = raise(&mut core, &asks(looked_at), 2_000);
        assert_eq!(kept.len(), 1);
        core.withdrawn.clear();
        core.withdraw_session(elsewhere);
        assert_eq!(core.withdrawn.len(), 1);
        let pending: Vec<SessionId> = core.list(2_000).iter().map(|i| i.session_id).collect();
        assert_eq!(pending, [looked_at]);
    }

    #[test]
    fn an_approval_becomes_an_intent_with_its_options_as_actions() {
        let mut core = core();
        let session = SessionId::new();
        let info = approval(session);
        let raised = raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            1_000,
        );
        let [intent] = raised.as_slice() else {
            panic!("one intent expected, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::ApprovalRequired);
        assert_eq!(intent.session_id, session);
        assert_eq!(intent.title, "api: approval needed");
        let ids: Vec<_> = intent.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["accept", "cancel"]);
        assert_eq!(intent.expires_at_ms, None, "lives as long as the approval");
        assert!(
            !intent.body.contains("abcdef123456789"),
            "secrets are redacted: {}",
            intent.body
        );
        assert!(intent.body.starts_with("Run `rm -rf build`?"));
    }

    #[test]
    fn resolving_an_approval_withdraws_its_intent() {
        let mut core = core();
        let info = approval(SessionId::new());
        let session = info.session_id;
        raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            1_000,
        );
        assert_eq!(core.list(1_001).len(), 1);
        raise(
            &mut core,
            &Event::ApprovalResolved {
                approval_id: info.id,
                session_id: session,
                outcome: hutan_core::agent::PermissionOutcome::Expired,
                decided_by: None,
            },
            1_002,
        );
        assert!(core.list(1_003).is_empty());
    }

    #[test]
    fn a_node_becoming_blocked_raises_once_until_it_is_unblocked() {
        let mut core = core();
        let session = SessionId::new();
        let mut blocked = node("agent-1", NodeKind::Sub, NodeState::Blocked);
        blocked.name = Some("Explore".to_owned());
        blocked.activity = Some("wants to run npm install".to_owned());

        let first = raise(&mut core, &tree(session, vec![blocked.clone()]), 1_000);
        let [intent] = first.as_slice() else {
            panic!("expected one intent, got {first:?}");
        };
        assert_eq!(intent.kind, IntentKind::AgentBlocked);
        assert_eq!(intent.title, "api: Explore is waiting for you");
        assert_eq!(intent.body, "wants to run npm install");
        assert_eq!(intent.actions[0].id, ACTION_OPEN);

        // Still blocked (only the activity changed): not a new transition.
        blocked.activity = Some("still waiting".to_owned());
        assert!(raise(&mut core, &tree(session, vec![blocked.clone()]), 30_000).is_empty());

        // Unblocked, then blocked again after the rate limit and after the first was seen.
        let working = node("agent-1", NodeKind::Sub, NodeState::Working);
        assert!(raise(&mut core, &tree(session, vec![working]), 31_000).is_empty());
        core.remove(&intent.id);
        assert_eq!(
            raise(&mut core, &tree(session, vec![blocked]), 60_000).len(),
            1
        );
    }

    #[test]
    fn a_gone_node_that_is_blocked_raises_nothing() {
        let mut core = core();
        let mut gone = node("a", NodeKind::Sub, NodeState::Blocked);
        gone.liveness = Liveness::Gone;
        assert!(raise(&mut core, &tree(SessionId::new(), vec![gone]), 1_000).is_empty());
    }

    #[test]
    fn the_main_agent_finishing_a_long_enough_turn_raises_agent_finished() {
        let mut core = core();
        let session = SessionId::new();
        let working = node("root", NodeKind::Main, NodeState::Working);
        assert!(raise(&mut core, &tree(session, vec![working.clone()]), 1_000).is_empty());
        // Still working later: nothing, and the start of the turn is kept.
        assert!(raise(&mut core, &tree(session, vec![working]), 10_000).is_empty());
        let idle = completed("root", NodeKind::Main);
        let raised = raise(&mut core, &tree(session, vec![idle]), 61_000);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::AgentFinished);
        assert_eq!(intent.title, "api: claude finished");
        assert_eq!(intent.body, "Finished after 1m 0s.");
    }

    #[test]
    fn a_plain_idle_after_a_long_turn_is_a_finish_but_not_in_a_codex_chat() {
        // The Stop hooks and OMP report a normal stop as a plain idle: that is a finish.
        let mut core = core();
        let session = SessionId::new();
        let working = node("root", NodeKind::Main, NodeState::Working);
        raise(&mut core, &tree(session, vec![working.clone()]), 1_000);
        let idle = node("root", NodeKind::Main, NodeState::Idle);
        assert_eq!(idle.outcome, None);
        let raised = raise(&mut core, &tree(session, vec![idle.clone()]), 600_000);
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].kind, IntentKind::AgentFinished);

        // A Hutan Codex chat raises `chat_turn_finished` for the same turn, not this.
        let mut core = self::core();
        let chat = SessionId::new();
        let mut working = working;
        working.source = hutan_agents::codex::TREE_SOURCE.to_owned();
        let mut idle = idle;
        idle.source = working.source.clone();
        raise(&mut core, &tree(chat, vec![working]), 1_000);
        assert!(raise(&mut core, &tree(chat, vec![idle]), 600_000).is_empty());
    }

    #[test]
    fn a_short_turn_a_subagent_or_a_failure_is_not_a_finish() {
        let session = SessionId::new();
        // Under min_turn_secs.
        let mut core = core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("root", NodeKind::Main);
        assert!(raise(&mut core, &tree(session, vec![idle]), 20_999).is_empty());

        // Exactly min_turn_secs counts.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("root", NodeKind::Main);
        assert_eq!(
            raise(&mut core, &tree(session, vec![idle]), 21_000).len(),
            1
        );

        // A subagent finishing, however long it took.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("sub", NodeKind::Sub, NodeState::Working)],
            ),
            1_000,
        );
        let idle = completed("sub", NodeKind::Sub);
        assert!(raise(&mut core, &tree(session, vec![idle]), 600_000).is_empty());

        // The main agent failing, being interrupted, or losing contact.
        for outcome in [
            NodeOutcome::Failed,
            NodeOutcome::Interrupted,
            NodeOutcome::Disconnected,
        ] {
            let mut core = self::core();
            raise(
                &mut core,
                &tree(
                    session,
                    vec![node("root", NodeKind::Main, NodeState::Working)],
                ),
                1_000,
            );
            let mut idle = node("root", NodeKind::Main, NodeState::Idle);
            idle.outcome = Some(outcome);
            assert!(
                raise(&mut core, &tree(session, vec![idle]), 600_000).is_empty(),
                "{outcome:?}"
            );
        }

        // An explicit `completed` outcome is a finish.
        let mut core = self::core();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        let mut done = node("root", NodeKind::Main, NodeState::Idle);
        done.outcome = Some(NodeOutcome::Completed);
        assert_eq!(
            raise(&mut core, &tree(session, vec![done]), 600_000).len(),
            1
        );
    }

    #[test]
    fn a_blocked_stretch_does_not_restart_the_turn() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        // Blocked raises its own intent; the rate limit and the pending intent are out of the way
        // by the time the turn ends.
        let blocked = node("root", NodeKind::Main, NodeState::Blocked);
        assert_eq!(
            raise(&mut core, &tree(session, vec![blocked]), 5_000).len(),
            1
        );
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            6_000,
        );
        let idle = completed("root", NodeKind::Main);
        let raised = raise(&mut core, &tree(session, vec![idle]), 300_000);
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].kind, IntentKind::AgentFinished);
        assert_eq!(raised[0].body, "Finished after 4m 59s.");
    }

    #[test]
    fn a_non_zero_exit_raises_and_a_clean_exit_or_signal_does_not() {
        let mut core = core();
        let session = SessionId::new();
        let exited = |code, signal| Event::SessionExited {
            session_id: session,
            code,
            signal,
        };
        assert!(raise(&mut core, &exited(Some(0), None), 1_000).is_empty());
        assert!(raise(&mut core, &exited(None, Some(1)), 1_000).is_empty());
        let raised = raise(&mut core, &exited(Some(2), None), 1_000);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::SessionExitedWithError);
        assert_eq!(intent.body, "The process exited with code 2.");
    }

    #[test]
    fn closing_a_session_withdraws_what_was_raised_about_it() {
        let mut core = core();
        let session = SessionId::new();
        let other = SessionId::new();
        for id in [session, other] {
            let raised = raise(
                &mut core,
                &Event::SessionExited {
                    session_id: id,
                    code: Some(2),
                    signal: None,
                },
                1_000,
            );
            assert_eq!(raised.len(), 1);
        }
        assert_eq!(core.list(1_001).len(), 2);
        let raised = raise(
            &mut core,
            &Event::SessionClosed {
                session_id: session,
            },
            1_002,
        );
        assert!(raised.is_empty());
        let left = core.list(1_003);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].session_id, other);
        assert_eq!(core.withdrawn.len(), 1, "its native notification goes too");
    }

    #[test]
    fn a_finished_chat_turn_raises_with_the_reply_unless_the_session_is_focused() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &chat_event(session, AgentEventKind::TurnStarted, 1),
            1_000,
        );
        raise(
            &mut core,
            &chat_event(
                session,
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: "Use  a\nBTreeMap, password=hunter2hunter2".to_owned(),
                },
                2,
            ),
            1_100,
        );
        let raised = raise(&mut core, &turn_completed(session, 3), 1_200);
        let [intent] = raised.as_slice() else {
            panic!("expected one intent, got {raised:?}");
        };
        assert_eq!(intent.kind, IntentKind::ChatTurnFinished);
        assert!(intent.body.starts_with("Use a BTreeMap"), "{}", intent.body);
        assert!(!intent.body.contains("hunter2"), "{}", intent.body);

        // Focus suppresses it, and releasing the focus lets the next one through.
        let conn = ConnId(7);
        core.focus.insert(conn, session);
        assert!(raise(&mut core, &turn_completed(session, 4), 100_000).is_empty());
        core.focus.remove(&conn);
        core.remove(&intent.id);
        assert_eq!(
            raise(&mut core, &turn_completed(session, 5), 100_001).len(),
            1
        );

        // Another session's focus does not suppress.
        core.focus.insert(conn, SessionId::new());
        let other = SessionId::new();
        assert_eq!(
            raise(&mut core, &turn_completed(other, 1), 100_002).len(),
            1
        );
    }

    #[test]
    fn replayed_and_streaming_chat_events_raise_nothing() {
        let mut core = core();
        let session = SessionId::new();
        let replay = match turn_completed(session, 1) {
            Event::ChatEvent { session_id, event } => Event::ChatEvent {
                session_id,
                event: Box::new(AgentEvent::replayed(*event)),
            },
            other => other,
        };
        assert!(raise(&mut core, &replay, 1_000).is_empty());
        let delta = chat_event(
            session,
            AgentEventKind::MessageDelta {
                text: "hi".to_owned(),
            },
            2,
        );
        assert!(raise(&mut core, &delta, 1_000).is_empty());
    }

    #[test]
    fn identical_intents_are_deduplicated_while_pending_but_not_after_an_ack() {
        let mut core = core();
        let session = SessionId::new();
        let info = approval(session);
        let event = Event::ApprovalRequired {
            approval: Box::new(info),
        };
        let first = raise(&mut core, &event, 1_000);
        assert_eq!(first.len(), 1);
        assert!(raise(&mut core, &event, 1_001).is_empty(), "same approval");
        assert_eq!(core.list(1_002).len(), 1);
        // A different approval is a different intent.
        assert_eq!(
            raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(session)),
                },
                1_003,
            )
            .len(),
            1
        );
        // Acknowledged intents may be raised again for the same key.
        core.remove(&first[0].id);
        assert_eq!(raise(&mut core, &event, 1_004).len(), 1);
    }

    #[test]
    fn a_sessions_finished_turns_come_once_per_burst_but_requests_and_failures_always_call() {
        let mut core = core();
        let session = SessionId::new();
        assert_eq!(
            raise(&mut core, &turn_completed(session, 1), 1_000).len(),
            1
        );
        // A second finished turn inside the 90 s burst raises nothing.
        assert!(
            raise(
                &mut core,
                &turn_completed(session, 2),
                1_000 + FINISHED_BURST_MS - 1
            )
            .is_empty()
        );
        // Requests and failures are not held back by the burst, and do not start one.
        for _ in 0..3 {
            assert_eq!(
                raise(
                    &mut core,
                    &Event::ApprovalRequired {
                        approval: Box::new(approval(session)),
                    },
                    1_001,
                )
                .len(),
                1
            );
        }
        let exit = Event::SessionExited {
            session_id: session,
            code: Some(1),
            signal: None,
        };
        assert_eq!(raise(&mut core, &exit, 1_002).len(), 1);
        // Another session has its own burst.
        let other = SessionId::new();
        assert_eq!(raise(&mut core, &turn_completed(other, 1), 1_001).len(), 1);
        // The burst ends after exactly FINISHED_BURST_MS.
        assert_eq!(
            raise(
                &mut core,
                &turn_completed(session, 3),
                1_000 + FINISHED_BURST_MS
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_request_or_failure_does_not_wait_for_the_burst_of_a_finish_before_it() {
        let mut core = core();
        let session = SessionId::new();
        let exit = Event::SessionExited {
            session_id: session,
            code: Some(1),
            signal: None,
        };
        // A failure, then a blocked agent a moment later: both call.
        assert_eq!(raise(&mut core, &exit, 1_000).len(), 1);
        let blocked = tree(
            session,
            vec![node("root", NodeKind::Main, NodeState::Blocked)],
        );
        assert_eq!(raise(&mut core, &blocked, 1_001).len(), 1);
    }

    #[test]
    fn finished_work_is_off_with_notify_finished_and_requests_and_failures_stay() {
        let mut core = Core::new(NotifySettings {
            finished: false,
            ..NotifySettings::default()
        });
        let session = SessionId::new();
        assert!(raise(&mut core, &turn_completed(session, 1), 1_000).is_empty());
        assert_eq!(
            raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(session)),
                },
                1_001,
            )
            .len(),
            1
        );
        let exit = Event::SessionExited {
            session_id: session,
            code: Some(2),
            signal: None,
        };
        assert_eq!(raise(&mut core, &exit, 1_002).len(), 1);
    }

    #[test]
    fn only_requests_and_failures_play_a_sound() {
        let loud = Core::new(NotifySettings::default());
        for kind in IntentKind::ALL {
            let calls = matches!(
                kind,
                IntentKind::ApprovalRequired
                    | IntentKind::QuestionRequired
                    | IntentKind::AgentBlocked
                    | IntentKind::SessionExitedWithError
                    | IntentKind::TaskFailed
            );
            assert_eq!(loud.plays_sound(kind), calls, "{kind:?}");
        }
        // `notify.sound = "none"` silences them all.
        let silent = Core::new(NotifySettings {
            sound: NotifySound::None,
            ..NotifySettings::default()
        });
        assert!(IntentKind::ALL.iter().all(|k| !silent.plays_sound(*k)));
    }

    #[test]
    fn intents_other_than_approvals_expire_and_approvals_do_not() {
        let mut core = Core::new(NotifySettings {
            expire_secs: 60,
            ..NotifySettings::default()
        });
        let session = SessionId::new();
        raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(1),
                signal: None,
            },
            1_000,
        );
        raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(approval(session)),
            },
            1_000,
        );
        let listed = core.list(60_999);
        assert_eq!(listed.len(), 2);
        let exit = listed
            .iter()
            .find(|i| i.kind == IntentKind::SessionExitedWithError)
            .unwrap();
        assert_eq!(exit.expires_at_ms, Some(61_000));
        let listed = core.list(61_000);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].kind, IntentKind::ApprovalRequired);
        assert!(core.find(&exit.id, 61_000).is_none());
        assert!(!core.remove(&exit.id));
    }

    #[test]
    fn disabled_kinds_and_a_disabled_service_raise_nothing() {
        let session = SessionId::new();
        let exited = Event::SessionExited {
            session_id: session,
            code: Some(1),
            signal: None,
        };
        let mut only_approvals = Core::new(NotifySettings {
            kinds: vec![IntentKind::ApprovalRequired],
            ..NotifySettings::default()
        });
        assert!(raise(&mut only_approvals, &exited, 1_000).is_empty());
        assert_eq!(
            raise(
                &mut only_approvals,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(session)),
                },
                1_000,
            )
            .len(),
            1
        );
        let mut off = Core::new(NotifySettings {
            enabled: false,
            ..NotifySettings::default()
        });
        assert!(raise(&mut off, &exited, 1_000).is_empty());
        assert!(off.list(1_001).is_empty());
    }

    #[test]
    fn the_pending_list_is_bounded_and_drops_the_oldest() {
        let mut core = core();
        let mut ids = Vec::new();
        for n in 0..=MAX_PENDING {
            let raised = raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(SessionId::new())),
                },
                1_000 + n as u64,
            );
            ids.push(raised[0].id.clone());
        }
        let listed = core.list(2_000);
        assert_eq!(listed.len(), MAX_PENDING);
        assert_eq!(listed[0].id, ids[1], "the oldest was dropped");
    }

    #[test]
    fn titles_and_bodies_are_bounded_redacted_single_lines() {
        let long = format!(
            "{}\n{}",
            "word ".repeat(200),
            "sk-abcdefghijklmnopqrstuvwxyz0123"
        );
        let body = clean(&long, BODY_MAX_CHARS);
        assert_eq!(body.chars().count(), BODY_MAX_CHARS);
        assert!(body.ends_with('…'));
        assert!(!body.contains('\n'));
        let secret = clean("key sk-abcdefghijklmnopqrstuvwxyz0123 end", BODY_MAX_CHARS);
        assert_eq!(secret, "key [REDACTED] end");
        let multibyte = clean(&"é".repeat(500), 10);
        assert_eq!(multibyte.chars().count(), 10);
    }

    #[test]
    fn a_reply_that_exactly_fits_the_body_budget_keeps_its_last_character() {
        let fits = format!("{}x", "é".repeat(BODY_MAX_CHARS - 1));
        assert_eq!(clean(&fits, BODY_MAX_CHARS), fits);
        assert_eq!(
            clean(&format!("{fits}z"), BODY_MAX_CHARS),
            format!("{}…", "é".repeat(BODY_MAX_CHARS - 1))
        );
    }

    #[test]
    fn a_secret_that_ends_beyond_any_prefix_is_still_masked() {
        // A quoted assignment whose closing quote lies far beyond the notification body.
        let secret = "abcdefghij".repeat(1000);
        let quoted = format!("private_key=\"{secret}\" and then some text");
        let body = clean(&quoted, BODY_MAX_CHARS);
        assert!(!body.contains("abcdefghij"), "{body}");
        assert!(body.starts_with("private_key=[REDACTED] and"), "{body}");
        assert!(body.chars().count() <= BODY_MAX_CHARS);

        // A URL password whose `@` lies beyond the prefix.
        let password = "p4ssw0rdp4ss".repeat(800);
        let url = format!("fetch https://deploy:{password}@example.com/repo.git now");
        let body = clean(&url, BODY_MAX_CHARS);
        assert!(!body.contains("p4ssw0rdp4ss"), "{body}");
        assert!(
            body.contains("https://deploy:[REDACTED]@example.com"),
            "{body}"
        );

        // The same holds for the title, which carries the session's name.
        let title = clean(&quoted, TITLE_MAX_CHARS);
        assert!(!title.contains("abcdefghij"), "{title}");
    }

    #[test]
    fn ending_a_session_forgets_its_nodes_and_message() {
        let mut core = core();
        let session = SessionId::new();
        raise(
            &mut core,
            &tree(
                session,
                vec![node("root", NodeKind::Main, NodeState::Working)],
            ),
            1_000,
        );
        assert_eq!(core.nodes.len(), 1);
        raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(0),
                signal: None,
            },
            2_000,
        );
        assert!(core.nodes.is_empty());
    }

    #[test]
    fn every_way_out_of_the_pending_list_is_reported_for_the_native_notification() {
        let mut core = core();
        let session = SessionId::new();
        let info = approval(session);
        let approval_intent = raise(
            &mut core,
            &Event::ApprovalRequired {
                approval: Box::new(info.clone()),
            },
            1_000,
        )
        .remove(0);
        let exit = |code| Event::SessionExited {
            session_id: session,
            code: Some(code),
            signal: None,
        };
        let first = raise(&mut core, &exit(1), 1_000).remove(0);
        assert!(core.withdrawn.is_empty(), "nothing has left yet");

        // Acknowledged or acted on (`remove`), and a second removal reports nothing.
        assert!(core.remove(&first.id));
        assert!(!core.remove(&first.id));
        assert_eq!(core.withdrawn, std::slice::from_ref(&first.id));
        core.withdrawn.clear();

        // Expired: an intent past its time, found by any later look.
        let second = raise(&mut core, &exit(2), 20_000).remove(0);
        let at = second.expires_at_ms.unwrap();
        core.withdrawn.clear();
        core.expire(at - 1);
        assert!(core.withdrawn.is_empty());
        core.expire(at);
        assert_eq!(
            core.withdrawn,
            std::slice::from_ref(&second.id),
            "the approval does not expire"
        );
        core.withdrawn.clear();

        // Resolved: the approval was answered somewhere else.
        raise(
            &mut core,
            &Event::ApprovalResolved {
                session_id: session,
                approval_id: info.id,
                outcome: hutan_core::agent::PermissionOutcome::Expired,
                decided_by: None,
            },
            at + 1,
        );
        assert_eq!(core.withdrawn, [approval_intent.id]);
    }

    #[test]
    fn an_intent_pushed_out_of_the_bounded_list_is_reported_too() {
        let mut core = core();
        let mut first = None;
        for n in 0..=MAX_PENDING {
            let raised = raise(
                &mut core,
                &Event::ApprovalRequired {
                    approval: Box::new(approval(SessionId::new())),
                },
                1_000 + n as u64,
            );
            first.get_or_insert_with(|| raised[0].id.clone());
        }
        assert_eq!(core.withdrawn, [first.unwrap()]);
    }

    #[test]
    fn the_refusing_options_of_an_approval_are_marked_destructive() {
        let mut core = core();
        let session = SessionId::new();
        let mut info = approval(session);
        info.options.push(PermissionOption {
            id: "decline".to_owned(),
            label: "Decline".to_owned(),
            kind: PermissionOptionKind::RejectOnce,
            provider: None,
        });
        info.options.push(PermissionOption {
            id: "always".to_owned(),
            label: "Always".to_owned(),
            kind: PermissionOptionKind::AllowAlways,
            provider: None,
        });
        let candidate = approval_candidate(&info);
        assert_eq!(candidate.destructive, ["cancel", "decline"]);
        let entry = core.offer(candidate, &meta(), 1_000).unwrap();
        assert_eq!(entry.destructive, ["cancel", "decline"]);
        let native = NativeNotification::from_intent(&entry.intent, &entry.destructive);
        let marked: Vec<(&str, bool)> = native
            .actions
            .iter()
            .map(|a| (a.id.as_str(), a.destructive))
            .collect();
        assert_eq!(
            marked,
            [
                ("accept", false),
                ("cancel", true),
                ("decline", true),
                (notify_native::OPEN_ACTION_ID, false)
            ],
            "the first three options, then Open"
        );
    }

    fn ended_task(state: TaskState, outcome: &str, sessions: Vec<SessionId>) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: WorkspaceId::new(),
            title: "Build".to_owned(),
            prompt: None,
            state,
            depends_on: Vec::new(),
            placement: hutan_core::TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions,
            created_at_ms: 1_000,
            started_at_ms: Some(10_000),
            ended_at_ms: Some(75_000),
            outcome: Some(outcome.to_owned()),
            delivery: Default::default(),
            after_policy: Default::default(),
            delivery_history: Vec::new(),
        }
    }

    #[test]
    fn a_task_that_ends_raises_an_intent_for_the_session_that_ran_it_once() {
        let mut core = core();
        let session = SessionId::new();
        let mut task = ended_task(TaskState::Running, "", vec![SessionId::new(), session]);
        task.outcome = None;
        task.ended_at_ms = None;
        let updated = |task: &Task| Event::TaskUpdated { task: task.clone() };
        assert!(raise(&mut core, &updated(&task), 20_000).is_empty());

        task.state = TaskState::Done;
        task.outcome = Some("exited 0".to_owned());
        task.ended_at_ms = Some(75_000);
        let raised = raise(&mut core, &updated(&task), 75_000);
        let [intent] = raised.as_slice() else {
            panic!("{raised:?}")
        };
        assert_eq!(intent.kind, IntentKind::TaskDone);
        assert_eq!(intent.session_id, session, "the last session");
        assert_eq!(intent.title, "api: task done");
        assert_eq!(intent.body, "Finished after 1m 5s (exited 0).");
        assert_eq!(intent.actions[0].id, ACTION_OPEN);

        // The same state again (a title edit) raises nothing, even after the intent is gone.
        core.remove(&intent.id);
        task.title = "Build it".to_owned();
        assert!(raise(&mut core, &updated(&task), 80_000).is_empty());

        // A failure carries its outcome; a task never started has no duration.
        let mut failed = ended_task(TaskState::Running, "", vec![session]);
        assert!(raise(&mut core, &updated(&failed), 80_000).is_empty());
        failed.state = TaskState::Failed;
        failed.outcome = Some("exited 3".to_owned());
        let raised = raise(&mut core, &updated(&failed), 80_000);
        assert_eq!(raised[0].kind, IntentKind::TaskFailed);
        assert_eq!(raised[0].body, "Failed after 1m 5s (exited 3).");
        let mut by_hand = ended_task(TaskState::Todo, "", vec![session]);
        assert!(raise(&mut core, &updated(&by_hand), 80_000).is_empty());
        by_hand.state = TaskState::Done;
        by_hand.started_at_ms = None;
        by_hand.outcome = Some("marked done".to_owned());
        let raised = raise(&mut core, &updated(&by_hand), 80_000);
        assert_eq!(raised[0].body, "Finished: marked done.");
    }

    #[test]
    fn a_task_seen_for_the_first_time_counts_only_when_it_ended_just_now() {
        let mut core = core();
        let session = SessionId::new();
        // What a daemon restart announces: the task failed a moment ago.
        let fresh = ended_task(TaskState::Failed, "interrupted", vec![session]);
        let raised = raise(
            &mut core,
            &Event::TaskUpdated { task: fresh },
            75_000 + 1_000,
        );
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].body, "Failed after 1m 5s (interrupted).");
        // An old task that is merely listed again is not news.
        let old = ended_task(TaskState::Done, "exited 0", vec![session]);
        assert!(
            raise(
                &mut core,
                &Event::TaskUpdated { task: old },
                75_000 + TASK_FRESH_MS + 1
            )
            .is_empty()
        );
        // And without a session there is nobody to open.
        let sessionless = ended_task(TaskState::Done, "marked done", Vec::new());
        assert!(raise(&mut core, &Event::TaskUpdated { task: sessionless }, 76_000).is_empty());
    }

    #[test]
    fn task_intents_pass_the_settings_and_dedup() {
        let session = SessionId::new();
        let mut core = core();
        // The session's own failed exit raised an intent a moment ago.
        let exit = raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(1),
                signal: None,
            },
            75_000,
        );
        assert_eq!(exit.len(), 1);
        let mut task = ended_task(TaskState::Running, "", vec![session]);
        let first = Event::TaskUpdated { task: task.clone() };
        raise(&mut core, &first, 75_000);
        task.state = TaskState::Failed;
        task.outcome = Some("exited 1".to_owned());
        let event = Event::TaskUpdated { task: task.clone() };
        let raised = raise(&mut core, &event, 75_100);
        assert_eq!(raised.len(), 1, "{raised:?}");
        // One event, one notification: the task's intent replaces the session's exit intent.
        assert_eq!(core.withdrawn, [exit[0].id.clone()]);
        assert_eq!(
            core.list(75_100).iter().map(|i| i.kind).collect::<Vec<_>>(),
            [IntentKind::TaskFailed]
        );
        // A failure is never held back: the exit raises its own intent again.
        let again = raise(
            &mut core,
            &Event::SessionExited {
                session_id: session,
                code: Some(2),
                signal: None,
            },
            75_200,
        );
        assert_eq!(again.len(), 1, "failures always call: {again:?}");

        // The settings gate them like any kind.
        let mut quiet = Core::new(NotifySettings {
            kinds: vec![IntentKind::TaskFailed],
            ..NotifySettings::default()
        });
        let mut done = ended_task(TaskState::Running, "", vec![session]);
        raise(
            &mut quiet,
            &Event::TaskUpdated { task: done.clone() },
            1_000,
        );
        done.state = TaskState::Done;
        assert!(raise(&mut quiet, &Event::TaskUpdated { task: done }, 2_000).is_empty());
    }
}
