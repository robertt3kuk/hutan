//! One agent truth (`docs/spikes/2026-10-20-chrome-v3.md` § 2.1–2.2): the **agent presentation**,
//! one record per agent session, joined from everything the daemon tells the window about it —
//! the session, its report, its chat, its observation tree, its pending requests — and the one
//! reducer that decides how a session shows up. The Agents page, the Agents panel, Mawas's Today
//! rail and the nav badge all read these records, so they cannot disagree.
//!
//! State, attention and observation are separate axes: a record carries its activity [`State`],
//! its attention [`Reason`]s and its provenance independently. Reducer precedence:
//!
//! 1. Pending approvals and questions (the real objects), blocked agents, an unread failure, a
//!    task awaiting review the user has not seen (unless a turn runs: working beats review), an
//!    unread completion are *reasons*; the session is in **Needs you**. Review work the user
//!    has seen is no reason and no state: the session is in the group of its state (*Idle*),
//!    with [`AgentRecord::changes_to_review`] for the small *Changes to review* link.
//! 2. A session that ended, or a main agent whose node has an outcome, is **Finished**: an
//!    obsolete "working" report never revives it.
//! 3. A daemon-managed chat turn in progress is **Working**; otherwise the report or the root
//!    node says Working, Blocked or Idle (the newer one, when they disagree, with the conflict
//!    disclosed); a managed chat the daemon knows exactly (no turn) is **Idle**.
//! 4. Nothing known: **State unavailable** (the *Unconfirmed* group), never Idle or Done.
//! 5. Offline keeps the last good records and says when they were confirmed.
//!
//! GPUI-free and unit tested. Nothing here invents evidence: a missing model, start time or
//! counter stays absent.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use hutan_client::{ConnectionState, StoreState};
use hutan_core::agent::PermissionOptionKind;
use hutan_core::{
    AgentNode, AgentObservation, ApprovalId, Liveness, NodeOutcome, NodeState, QuestionId,
    SessionId, SessionState, WorkspaceId,
};
use hutan_protocol::chat::{ApprovalInfo, ChatInfo};
use hutan_protocol::messages::{AgentState, SessionInfo, WorkspaceInfo, WorkspaceKind};
use hutan_protocol::worktree::PlacementKind;

use super::agents::{self, Filters, activity_text, display_name, usage_label};
use super::attention::{Tone, Unread, present};
use super::models::ModelNames;

/// What an agent session is to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A coding agent: a chat, or a terminal an agent reports from.
    Agent,
    /// Mawas, the assistant.
    Assistant,
}

impl Role {
    pub const fn label(self) -> &'static str {
        match self {
            Role::Agent => "Agent",
            Role::Assistant => "Assistant",
        }
    }
}

/// The activity of an agent (one axis; attention is the other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    /// Waiting for a decision.
    Blocked,
    Idle,
    /// No evidence of what the agent is doing.
    Unconfirmed,
    Completed,
    Failed,
    Stopped,
    Interrupted,
    Disconnected,
}

impl State {
    /// The state word. Never a lone punctuation mark.
    pub const fn label(self) -> &'static str {
        match self {
            State::Working => "Working",
            State::Blocked => "Blocked",
            State::Idle => "Idle",
            State::Unconfirmed => "State unavailable",
            State::Completed => "Completed",
            State::Failed => "Failed",
            State::Stopped => "Stopped",
            State::Interrupted => "Interrupted",
            State::Disconnected => "Disconnected",
        }
    }

    /// The state's own shape: state is never color alone.
    pub const fn glyph(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            State::Working => Icon::StateWorking,
            State::Blocked => Icon::StateBlocked,
            State::Idle => Icon::StateIdle,
            State::Unconfirmed => Icon::StateUnknown,
            State::Completed => Icon::StateCompleted,
            State::Failed => Icon::StateFailed,
            State::Stopped => Icon::StateStopped,
            State::Interrupted => Icon::StateInterrupted,
            State::Disconnected => Icon::StateDisconnected,
        }
    }

    pub const fn tone(self) -> Tone {
        match self {
            State::Working => Tone::Accent,
            State::Blocked => Tone::Warning,
            State::Idle | State::Unconfirmed => Tone::Muted,
            State::Completed => Tone::Success,
            State::Failed => Tone::Error,
            State::Stopped | State::Interrupted | State::Disconnected => Tone::Muted,
        }
    }

    pub const fn is_finished(self) -> bool {
        matches!(
            self,
            State::Completed
                | State::Failed
                | State::Stopped
                | State::Interrupted
                | State::Disconnected
        )
    }

    fn of_node(state: NodeState) -> Self {
        match state {
            NodeState::Working => State::Working,
            NodeState::Blocked => State::Blocked,
            NodeState::Idle => State::Idle,
            NodeState::Unknown => State::Unconfirmed,
        }
    }

    fn of_outcome(outcome: NodeOutcome) -> Self {
        match outcome {
            NodeOutcome::Completed => State::Completed,
            NodeOutcome::Failed => State::Failed,
            NodeOutcome::Interrupted => State::Interrupted,
            NodeOutcome::Disconnected => State::Disconnected,
        }
    }
}

/// The groups of the page, in the order they are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    NeedsYou,
    Working,
    Unconfirmed,
    Idle,
    /// Needs the user, but snoozed: left out until its attention changes.
    Snoozed,
    Finished,
}

impl Group {
    pub const ALL: [Group; 6] = [
        Group::NeedsYou,
        Group::Working,
        Group::Unconfirmed,
        Group::Idle,
        Group::Snoozed,
        Group::Finished,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Group::NeedsYou => "Needs you",
            Group::Working => "Working",
            Group::Unconfirmed => "Unconfirmed",
            Group::Idle => "Idle",
            Group::Snoozed => "Snoozed",
            Group::Finished => "Finished",
        }
    }

    /// The name in a request to the page (`unconfirmed`, `needs-you`, …).
    pub const fn key(self) -> &'static str {
        match self {
            Group::NeedsYou => "needs-you",
            Group::Working => "working",
            Group::Unconfirmed => "unconfirmed",
            Group::Idle => "idle",
            Group::Snoozed => "snoozed",
            Group::Finished => "finished",
        }
    }

    pub fn from_key(key: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.key() == key)
    }
}

/// Why a session needs the user, highest priority first (the order of the derive is the order
/// of the page: approval, question, blocked, unread failure, review, completed-unread).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReasonKind {
    Approval,
    Question,
    Blocked,
    Failed,
    Review,
    CompletedUnread,
    /// The user marked the session unread.
    MarkedUnread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub kind: ReasonKind,
    /// `Permission: run cargo test`, `Question: choose a branch`, `reviewer blocked`.
    pub text: String,
    /// When the oldest unresolved reason of this kind appeared, when known.
    pub since_ms: Option<u64>,
}

/// One answer an approval offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    pub kind: PermissionOptionKind,
}

/// A permission request waiting for the user, as the daemon offered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: ApprovalId,
    pub title: String,
    pub detail: Option<String>,
    pub options: Vec<RequestOption>,
    pub requested_at_ms: u64,
}

/// A question waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub id: QuestionId,
    pub title: String,
    pub requested_at_ms: u64,
}

/// Where the session works: the checkout, separate from the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// `Worktree: chrome-v3` or `Main checkout: master`.
    pub label: String,
    pub root: PathBuf,
    pub branch: Option<String>,
    pub worktree: bool,
    /// Another live agent works in the same checkout.
    pub shared: bool,
}

/// An observed subagent of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    pub node_id: String,
    /// 1 for a child of the main agent.
    pub depth: usize,
    pub label: String,
    pub agent_type: Option<String>,
    pub state: State,
    pub blocked: bool,
    pub finished: bool,
    pub activity: Option<String>,
    pub model: Option<String>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub usage: Option<String>,
    pub transcript: Option<String>,
    /// The names above it, from the first subagent down (empty for a child of the main agent).
    pub ancestors: Vec<String>,
}

/// One agent session: everything a surface shows about it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRecord {
    pub session: SessionId,
    pub title: String,
    pub role: Role,
    pub state: State,
    pub group: Group,
    /// Every reason, highest priority first; the first is the headline, the rest are `+N`.
    pub reasons: Vec<Reason>,
    /// The permission requests that can still be answered, oldest first.
    pub requests: Vec<Request>,
    /// Requests this window answered whose result the daemon has not confirmed yet.
    pub submitting: usize,
    pub questions: Vec<Ask>,
    /// The agent program (`omp`, `claude`, `codex`, `direct`, …).
    pub agent: String,
    /// What a person reads: `OMP`, `Claude Code`, `Codex`.
    pub harness: String,
    /// The model as a person reads it: its display name in the harness's catalog, else the id
    /// as reported; never inferred.
    pub model: Option<String>,
    /// The model's id as reported (the tooltip says it when the name differs).
    pub model_id: Option<String>,
    pub workspace: WorkspaceId,
    /// The workspace the project is in, when it is in one.
    pub container: Option<WorkspaceId>,
    pub project: String,
    pub container_name: Option<String>,
    pub checkout: Option<Checkout>,
    pub folder: Option<PathBuf>,
    /// The current activity on one short line.
    pub activity: Option<String>,
    /// Why the state is what it is, for the row's second line (exit detail, "has not reported").
    pub detail: String,
    /// The last semantic activity (a report, a node change); never the creation time.
    pub updated_at_ms: Option<u64>,
    /// When the harness last reported its state, for "Last reported 3h ago".
    pub reported_at_ms: Option<u64>,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub usage: Option<String>,
    pub transcript: Option<String>,
    /// Sources and conflicts, for the details.
    pub provenance: Vec<String>,
    pub children: Vec<Child>,
    pub subagents_live: usize,
    pub subagents_finished: usize,
    pub subagents_blocked: usize,
    /// The session's process is alive.
    pub live: bool,
    /// The daemon runs the agent (a chat), so it can interrupt a turn.
    pub managed: bool,
    pub turn_active: bool,
    /// Its work waits for review and the user has seen it, while no turn runs: not a state, the
    /// row keeps its state word and adds a small *Changes to review* link.
    pub changes_to_review: bool,
    /// Who reported the agent's state (`claude-hooks`), when something did.
    pub report_source: Option<String>,
}

impl AgentRecord {
    /// `hutan / Chrome cleanup`, `Home / Mawas`.
    pub fn location(&self) -> String {
        match (&self.container_name, self.role) {
            (Some(container), _) if *container != self.project => {
                format!("{} / {container}", self.project)
            }
            (None, Role::Assistant) if self.project == "Home" => "Home / Mawas".to_owned(),
            _ => self.project.clone(),
        }
    }

    /// The headline reason, if the session needs the user.
    pub fn headline(&self) -> Option<&Reason> {
        self.reasons.first()
    }

    /// Whether the record is in the *Needs you* group.
    pub fn needs_you(&self) -> bool {
        self.group == Group::NeedsYou
    }

    /// A text the search matches against: title, harness, model, location, branch, activity.
    pub fn haystack(&self) -> String {
        let mut text = format!(
            "{} {} {} {} {} {}",
            self.title,
            self.harness,
            self.agent,
            self.model.as_deref().unwrap_or(""),
            self.model_id.as_deref().unwrap_or(""),
            self.location()
        );
        if let Some(checkout) = &self.checkout {
            text.push(' ');
            text.push_str(&checkout.label);
            if let Some(branch) = &checkout.branch {
                text.push(' ');
                text.push_str(branch);
            }
        }
        if let Some(activity) = &self.activity {
            text.push(' ');
            text.push_str(activity);
        }
        if let Some(reason) = self.headline() {
            text.push(' ');
            text.push_str(&reason.text);
        }
        text.to_lowercase()
    }
}

/// Which records a surface lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scope {
    /// Every workspace of this host.
    #[default]
    All,
    /// A workspace with its projects, a project in no workspace, or Home.
    Workspace(WorkspaceId),
}

impl Scope {
    pub fn contains(self, record: &AgentRecord) -> bool {
        match self {
            Scope::All => true,
            Scope::Workspace(id) => record.workspace == id || record.container == Some(id),
        }
    }
}

/// What the collection knows about the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// The first snapshot has not arrived: no false zero.
    Loading,
    Online,
    /// The connection was lost; the records are the last confirmed ones.
    Offline {
        last_confirmed_ms: Option<u64>,
    },
}

/// Totals for one scope: sessions and subagents are different nouns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub sessions: usize,
    pub subagents: usize,
    pub subagents_blocked: usize,
    /// Unique sessions that need the user (not approval cards, not children).
    pub needs_you: usize,
    pub working: usize,
    pub unconfirmed: usize,
    pub idle: usize,
    /// Sessions that need the user but were snoozed until their attention changes.
    pub snoozed: usize,
    pub finished: usize,
    /// Permission requests waiting, over the sessions counted.
    pub requests: usize,
}

impl Counts {
    pub fn of<'a>(records: impl IntoIterator<Item = &'a AgentRecord>) -> Self {
        let mut counts = Counts::default();
        for record in records {
            counts.sessions += 1;
            counts.subagents += record.subagents_live;
            counts.subagents_blocked += record.subagents_blocked;
            counts.requests += record.requests.len() + record.submitting;
            match record.group {
                Group::NeedsYou => counts.needs_you += 1,
                Group::Working => counts.working += 1,
                Group::Unconfirmed => counts.unconfirmed += 1,
                Group::Idle => counts.idle += 1,
                Group::Snoozed => counts.snoozed += 1,
                Group::Finished => counts.finished += 1,
            }
        }
        counts
    }

    /// `5 agent sessions · 3 subagents`, `1 agent session`.
    pub fn summary(&self) -> String {
        let sessions = plural(self.sessions, "agent session", "agent sessions");
        if self.subagents == 0 {
            sessions
        } else {
            format!(
                "{sessions} · {}",
                plural(self.subagents, "subagent", "subagents")
            )
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The collection: every agent session, in the one canonical order.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPresentation {
    pub records: Vec<AgentRecord>,
    pub link: Link,
    /// The store revision the records were built from.
    pub revision: u64,
    by_id: HashMap<SessionId, usize>,
}

impl AgentPresentation {
    /// A collection of `records` (already in canonical order).
    pub fn new(records: Vec<AgentRecord>, link: Link, revision: u64) -> Self {
        let by_id = records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.session, index))
            .collect();
        Self {
            records,
            link,
            revision,
            by_id,
        }
    }

    pub fn empty(link: Link, revision: u64) -> Self {
        Self::new(Vec::new(), link, revision)
    }

    pub fn get(&self, session: SessionId) -> Option<&AgentRecord> {
        self.by_id.get(&session).map(|index| &self.records[*index])
    }

    /// The records of `scope`, in canonical order.
    pub fn in_scope(&self, scope: Scope) -> impl Iterator<Item = &AgentRecord> {
        self.records.iter().filter(move |r| scope.contains(r))
    }

    pub fn counts(&self, scope: Scope) -> Counts {
        Counts::of(self.in_scope(scope))
    }

    /// Sessions that need the user, whatever the scope.
    pub fn needs_you(&self) -> usize {
        self.records.iter().filter(|r| r.needs_you()).count()
    }
}

/// What the reducer reads, besides the daemon's own state.
#[derive(Debug)]
pub struct Facts<'a> {
    pub state: &'a StoreState,
    pub unread: &'a BTreeMap<SessionId, Unread>,
    /// Sessions whose finished task waits for a reviewer and that the user has not seen yet.
    pub reviews: &'a HashSet<SessionId>,
    /// Sessions whose finished task waits for a reviewer and that the user has seen: no reason,
    /// a *Changes to review* link beside their state.
    pub changes_to_review: &'a HashSet<SessionId>,
    /// Sessions the user snoozed whose attention is still the one they were snoozed at (the
    /// store decides: a snooze lapses when it changes).
    pub snoozed: &'a HashSet<SessionId>,
    /// Approvals this window answered; the daemon has not confirmed them yet.
    pub answered_approvals: &'a HashMap<ApprovalId, String>,
    pub answered_questions: &'a HashSet<QuestionId>,
    /// When this window last saw the daemon's state confirmed.
    pub last_confirmed_ms: Option<u64>,
    /// The display names of the harnesses' models.
    pub model_names: &'a ModelNames,
    pub revision: u64,
}

/// The agents of `facts`, joined and reduced.
pub fn build(facts: &Facts<'_>) -> AgentPresentation {
    let state = facts.state;
    let link = match state.connection {
        ConnectionState::Connected => Link::Online,
        ConnectionState::Connecting if state.epoch.is_none() => Link::Loading,
        ConnectionState::Connecting => Link::Online,
        ConnectionState::Reconnecting { .. } | ConnectionState::Stopped => Link::Offline {
            last_confirmed_ms: facts.last_confirmed_ms,
        },
    };
    if link == Link::Loading {
        return AgentPresentation::empty(link, facts.revision);
    }
    let mut approvals: HashMap<SessionId, Vec<&ApprovalInfo>> = HashMap::new();
    for approval in &state.approvals {
        approvals
            .entry(approval.session_id)
            .or_default()
            .push(approval);
    }
    let mut questions: HashMap<SessionId, Vec<&hutan_protocol::chat::QuestionInfo>> =
        HashMap::new();
    for question in &state.questions {
        questions
            .entry(question.session_id)
            .or_default()
            .push(question);
    }
    let workspaces: HashMap<WorkspaceId, &WorkspaceInfo> =
        state.workspaces.iter().map(|w| (w.id, w)).collect();
    let ctx = Ctx {
        facts,
        approvals,
        questions,
        workspaces,
    };
    let mut records: Vec<AgentRecord> = state
        .sessions
        .iter()
        .filter_map(|info| ctx.record(info))
        .collect();
    records.sort_by(compare);
    AgentPresentation::new(records, link, facts.revision)
}

/// The canonical order: by group; *Needs you* by the highest reason, then the oldest unresolved
/// one first; the other groups by the latest semantic activity, unknown last; the session id
/// breaks every tie.
fn compare(a: &AgentRecord, b: &AgentRecord) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    a.group.cmp(&b.group).then_with(|| {
        if a.group == Group::NeedsYou {
            let kind = |r: &AgentRecord| r.headline().map(|h| h.kind);
            let since = |r: &AgentRecord| r.headline().and_then(|h| h.since_ms);
            kind(a)
                .cmp(&kind(b))
                .then_with(|| match (since(a), since(b)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                })
        } else {
            match (a.updated_at_ms, b.updated_at_ms) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        .then_with(|| a.session.cmp(&b.session))
    })
}

struct Ctx<'a> {
    facts: &'a Facts<'a>,
    approvals: HashMap<SessionId, Vec<&'a ApprovalInfo>>,
    questions: HashMap<SessionId, Vec<&'a hutan_protocol::chat::QuestionInfo>>,
    workspaces: HashMap<WorkspaceId, &'a WorkspaceInfo>,
}

/// Whether the daemon knows a chat's state exactly: it runs the provider itself, or the
/// harness has handshaken (it has its own conversation id). A harness chat that never reported
/// either is not proof of anything.
fn chat_is_exact(chat: &ChatInfo) -> bool {
    chat.provider_id.is_some()
        || chat.agent == hutan_protocol::assistant::AGENT_ASSISTANT
        || chat.agent == "direct"
        || chat.agent_session_id.is_some()
}

/// The words of a harness.
pub fn harness_label(agent: &str) -> String {
    match agent {
        "omp" => "OMP".to_owned(),
        "claude" => "Claude Code".to_owned(),
        "codex" => "Codex".to_owned(),
        "direct" => "Direct chat".to_owned(),
        hutan_protocol::assistant::AGENT_ASSISTANT => "Provider".to_owned(),
        other => other.to_owned(),
    }
}

/// What the reducer settled on.
struct Reduced {
    state: State,
    detail: String,
    provenance: Vec<String>,
}

impl Ctx<'_> {
    fn record(&self, info: &SessionInfo) -> Option<AgentRecord> {
        let tree = self.facts.state.trees.get(&info.id);
        // An agent session has an observation tree, a report or a chat; a plain shell is not
        // one, whatever its unread flag says (that stays in Projects and the global attention).
        if tree.is_none() && info.agent.is_none() && info.chat.is_none() {
            return None;
        }
        let root = tree.and_then(|t| t.nodes.get(&t.root));
        let chat = info.chat.as_ref();
        let role = if chat.is_some_and(ChatInfo::is_assistant) {
            Role::Assistant
        } else {
            Role::Agent
        };
        let live = info.state.is_live();
        let turn_active = chat.is_some_and(|c| c.turn_active);

        // ---- requests and questions: the real pending objects ----
        let mut requests = Vec::new();
        let mut submitting = 0;
        for approval in self.approvals.get(&info.id).into_iter().flatten() {
            if self.facts.answered_approvals.contains_key(&approval.id) {
                submitting += 1;
                continue;
            }
            requests.push(Request {
                id: approval.id,
                title: approval.title.clone(),
                detail: approval.detail.clone().filter(|d| !d.trim().is_empty()),
                options: approval
                    .options
                    .iter()
                    .map(|o| RequestOption {
                        id: o.id.clone(),
                        label: o.label.clone(),
                        kind: o.kind,
                    })
                    .collect(),
                requested_at_ms: approval.requested_at_ms,
            });
        }
        let asks: Vec<Ask> = self
            .questions
            .get(&info.id)
            .into_iter()
            .flatten()
            .filter(|q| !self.facts.answered_questions.contains(&q.id))
            .map(|q| Ask {
                id: q.id,
                title: q.spec.title.clone(),
                requested_at_ms: q.requested_at_ms,
            })
            .collect();

        // ---- the children and who is blocked ----
        let children = tree
            .map(|tree| children_of(tree, self.facts.model_names))
            .unwrap_or_default();
        let blocked_nodes = tree.map(agents::blocked_nodes).unwrap_or_default();
        let blocked_children: Vec<&agents::BlockedNode> =
            blocked_nodes.iter().filter(|n| !n.main).collect();
        let main_blocked = blocked_nodes.iter().any(|n| n.main);
        let subagents_live = children.iter().filter(|c| !c.finished).count();
        let subagents_finished = children.len() - subagents_live;
        let subagents_blocked = children.iter().filter(|c| c.blocked).count();

        let reduced = self.reduce(info, root, chat, live, turn_active);

        // ---- attention reasons ----
        let mut reasons = Vec::new();
        if let Some(first) = requests.first() {
            reasons.push(Reason {
                kind: ReasonKind::Approval,
                text: format!("Permission: {}", first.title),
                since_ms: requests.iter().map(|r| r.requested_at_ms).min(),
            });
        }
        if let Some(first) = asks.first() {
            reasons.push(Reason {
                kind: ReasonKind::Question,
                text: format!("Question: {}", first.title),
                since_ms: asks.iter().map(|q| q.requested_at_ms).min(),
            });
        }
        if live {
            let report = info.agent.as_ref();
            let main_waits = main_blocked
                || reduced.state == State::Blocked
                || info.state == SessionState::WaitingApproval
                || chat.is_some_and(|c| c.pending_approvals > 0 || c.pending_questions > 0)
                || report.is_some_and(|r| r.state == AgentState::Blocked);
            // The main agent's own wait is the approval or question when there is one; the
            // children's waits are separate evidence.
            let main_reason = main_waits && requests.is_empty() && asks.is_empty();
            let named: Vec<String> = if !blocked_children.is_empty() {
                blocked_children.iter().map(|n| n.label.clone()).collect()
            } else {
                report
                    .and_then(|r| r.blocked_sub.clone())
                    .into_iter()
                    .collect()
            };
            let since = root
                .map(|n| n.last_activity_ms)
                .or_else(|| report.map(|r| r.updated_at_ms));
            let text = match (named.as_slice(), main_reason) {
                ([], true) => Some(match report.and_then(|r| r.message.as_deref()) {
                    Some(message) if !message.trim().is_empty() => {
                        format!("Blocked: {}", activity_text(message))
                    }
                    _ if info.state == SessionState::WaitingApproval => {
                        "Waiting for approval".to_owned()
                    }
                    _ => "Blocked: waiting for you".to_owned(),
                }),
                ([only], false) => Some(format!("{only} blocked")),
                ([only], true) => Some(format!("{only} blocked, and the main agent waits")),
                ([], false) => None,
                (many, _) => Some(format!(
                    "{} agents blocked: {}",
                    many.len(),
                    many.join(", ")
                )),
            };
            if let Some(text) = text {
                reasons.push(Reason {
                    kind: ReasonKind::Blocked,
                    text,
                    since_ms: since,
                });
            }
        }
        let unread = self.facts.unread.get(&info.id).copied();
        let unread_failed = unread == Some(Unread::Failed) && role != Role::Assistant;
        if unread_failed {
            let presentation = present(info, unread, self.facts.model_names);
            reasons.push(Reason {
                kind: ReasonKind::Failed,
                text: if presentation.detail.is_empty() {
                    "Failed".to_owned()
                } else {
                    format!("Failed: {}", presentation.detail)
                },
                since_ms: None,
            });
        }
        // Working beats review: a turn that runs (or is stopping) is the work going on.
        let working = reduced.state == State::Working;
        if self.facts.reviews.contains(&info.id) && !working {
            reasons.push(Reason {
                kind: ReasonKind::Review,
                text: "Review changes".to_owned(),
                since_ms: None,
            });
        }
        let changes_to_review = self.facts.changes_to_review.contains(&info.id) && !working;
        if unread == Some(Unread::Done) {
            reasons.push(Reason {
                kind: ReasonKind::CompletedUnread,
                text: "Completed · unread".to_owned(),
                since_ms: None,
            });
        }
        if unread == Some(Unread::Marked) {
            reasons.push(Reason {
                kind: ReasonKind::MarkedUnread,
                text: "Marked unread".to_owned(),
                since_ms: None,
            });
        }
        reasons.sort_by_key(|r| r.kind);

        let snoozed = self.facts.snoozed.contains(&info.id);
        let group = if snoozed {
            Group::Snoozed
        } else if !reasons.is_empty() || reduced.state == State::Blocked {
            Group::NeedsYou
        } else {
            match reduced.state {
                State::Working => Group::Working,
                State::Unconfirmed => Group::Unconfirmed,
                State::Idle => Group::Idle,
                _ => Group::Finished,
            }
        };

        // ---- facts about the agent ----
        let agent = chat
            .map(|c| c.agent.clone())
            .or_else(|| info.agent.as_ref().map(|r| r.agent.clone()))
            .or_else(|| root.map(|n| n.agent.clone()))
            .unwrap_or_default();
        let model_id = root
            .and_then(|n| n.model.clone())
            .or_else(|| chat.map(|c| c.model.clone()))
            .filter(|m| !m.trim().is_empty());
        let model = model_id
            .as_deref()
            .map(|id| self.facts.model_names.label(&agent, id));
        let report_ms = info.agent.as_ref().map(|r| r.updated_at_ms);
        let updated_at_ms = [report_ms, tree.and_then(latest_activity)]
            .into_iter()
            .flatten()
            .filter(|ms| *ms > 0)
            .max();
        let reported_at_ms = [report_ms, root.map(|n| n.last_activity_ms)]
            .into_iter()
            .flatten()
            .filter(|ms| *ms > 0)
            .max();
        let activity = if live && !reduced.state.is_finished() {
            root.and_then(|n| n.activity.as_deref())
                .map(activity_text)
                .filter(|a| !a.is_empty())
                .or_else(|| {
                    info.agent
                        .as_ref()
                        .and_then(|r| r.message.as_deref())
                        .map(activity_text)
                        .filter(|a| !a.is_empty())
                })
        } else {
            None
        };
        let workspace_info = self.workspaces.get(&info.workspace_id).copied();
        let container = workspace_info
            .and_then(|w| w.parent_id)
            .filter(|parent| self.workspaces.contains_key(parent));
        let container_name = container
            .and_then(|id| self.workspaces.get(&id))
            .map(|w| w.name.clone());
        let project =
            workspace_info.map_or_else(|| "Unknown project".to_owned(), |w| w.name.clone());
        let container_name = container_name.or_else(|| {
            workspace_info
                .filter(|w| w.kind == WorkspaceKind::Workspace)
                .map(|w| w.name.clone())
        });
        let checkout = info.placement.as_ref().map(|p| {
            let dir = p.root.file_name().map_or_else(
                || p.root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            Checkout {
                label: match p.kind {
                    PlacementKind::Worktree => format!("Worktree: {dir}"),
                    PlacementKind::Main => format!(
                        "Main checkout: {}",
                        p.branch.as_deref().unwrap_or("(detached)")
                    ),
                },
                root: p.root.clone(),
                branch: p.branch.clone(),
                worktree: p.kind == PlacementKind::Worktree,
                shared: !p.shared_with.is_empty(),
            }
        });

        Some(AgentRecord {
            session: info.id,
            title: info.title.clone(),
            role,
            state: reduced.state,
            group,
            reasons,
            requests,
            submitting,
            questions: asks,
            harness: harness_label(&agent),
            agent,
            model,
            model_id,
            workspace: info.workspace_id,
            container,
            project,
            container_name,
            checkout,
            folder: info.folder.clone().or_else(|| info.cwd.clone()),
            activity,
            detail: reduced.detail,
            updated_at_ms,
            reported_at_ms,
            started_at_ms: root.and_then(|n| n.started_at_ms),
            ended_at_ms: root.and_then(|n| n.ended_at_ms),
            usage: root.and_then(|n| n.usage.as_ref()).and_then(usage_label),
            transcript: root.and_then(|n| n.transcript_path.clone()),
            provenance: reduced.provenance,
            children,
            subagents_live,
            subagents_finished,
            subagents_blocked,
            live,
            managed: chat.is_some(),
            turn_active,
            changes_to_review,
            report_source: info.agent.as_ref().map(|r| r.source.clone()),
        })
    }

    /// The effective activity of the main agent and why (precedence in the module docs).
    fn reduce(
        &self,
        info: &SessionInfo,
        root: Option<&AgentNode>,
        chat: Option<&ChatInfo>,
        live: bool,
        turn_active: bool,
    ) -> Reduced {
        let reduced = |state, detail: String, provenance: Vec<String>| Reduced {
            state,
            detail,
            provenance,
        };
        // 2. The session itself ended: nothing that says "working" is current any more.
        if !live {
            return match &info.state {
                SessionState::Exited {
                    code: Some(0),
                    signal: None,
                } => reduced(State::Completed, "Exited with code 0".into(), Vec::new()),
                SessionState::Exited { code, signal } => reduced(
                    State::Failed,
                    match (code, signal) {
                        (_, Some(sig)) => format!("Killed by signal {sig}"),
                        (Some(code), None) => format!("Exited with code {code}"),
                        (None, None) => "Exited".to_owned(),
                    },
                    Vec::new(),
                ),
                SessionState::Failed { reason } => {
                    reduced(State::Failed, reason.clone(), Vec::new())
                }
                _ => reduced(State::Stopped, "The session was stopped".into(), Vec::new()),
            };
        }
        // 3. A turn the daemon is running.
        if turn_active {
            return reduced(
                State::Working,
                "A turn is in progress".into(),
                vec!["Chat: a turn is in progress".to_owned()],
            );
        }
        // 2. The main agent's own outcome beats obsolete evidence (a managed chat's tree is the
        // harness's; the chat itself is exact).
        if chat.is_none()
            && let Some(outcome) = root.and_then(|n| n.outcome)
        {
            let state = State::of_outcome(outcome);
            return reduced(
                state,
                format!("The main agent {}", state.label().to_lowercase()),
                vec!["Observation: the main agent finished".to_owned()],
            );
        }
        let report = info.agent.as_ref().map(|r| {
            (
                match r.state {
                    AgentState::Working => State::Working,
                    AgentState::Blocked => State::Blocked,
                    AgentState::Idle => State::Idle,
                },
                r.updated_at_ms,
            )
        });
        let node = root
            .filter(|n| !n.is_finished() && n.state != NodeState::Unknown)
            .map(|n| (State::of_node(n.state), n.last_activity_ms));
        let exact_chat = chat.filter(|c| chat_is_exact(c));
        let mut provenance = Vec::new();
        let mut chosen = match (report, node) {
            (Some((a, at)), Some((b, bt))) if a != b => {
                // Disagreement: the newer report wins; a tie keeps the more urgent state.
                let urgent = |s: State| match s {
                    State::Blocked => 0,
                    State::Working => 1,
                    _ => 2,
                };
                let (state, source) = if at > bt || (at == bt && urgent(a) <= urgent(b)) {
                    (a, "report")
                } else {
                    (b, "observation")
                };
                provenance.push(format!(
                    "Sources disagree: the report says {}, the observation says {}; showing the {source}.",
                    a.label().to_lowercase(),
                    b.label().to_lowercase()
                ));
                Some(state)
            }
            (Some((state, _)), _) => {
                provenance.push("Source: the agent's report".to_owned());
                Some(state)
            }
            (None, Some((state, _))) => {
                provenance.push("Source: the observation tree".to_owned());
                Some(state)
            }
            (None, None) => None,
        };
        // A chat the daemon knows exactly, with no turn, is not working, whatever a stale
        // report says.
        if exact_chat.is_some() && chosen == Some(State::Working) {
            provenance.push(
                "A report says working, but the daemon runs this chat and no turn is active."
                    .to_owned(),
            );
            chosen = Some(State::Idle);
        }
        if let Some(state) = chosen {
            return reduced(state, String::new(), provenance);
        }
        // 3. The managed chat's own state is the fallback.
        if exact_chat.is_some() {
            return reduced(
                State::Idle,
                "No turn in progress".into(),
                vec!["Chat: the daemon runs this session and no turn is active".to_owned()],
            );
        }
        // 4. Nothing known.
        let mut provenance = Vec::new();
        if let Some(node) = root
            && node.liveness == Liveness::Unknown
        {
            provenance.push("Process liveness unconfirmed.".to_owned());
        }
        let detail = if info.state == SessionState::Starting {
            "Starting".to_owned()
        } else {
            "This harness has not reported its current state.".to_owned()
        };
        reduced(State::Unconfirmed, detail, provenance)
    }
}

/// Every subagent of `tree`, parents first, siblings in the order they started.
fn children_of(tree: &AgentObservation, names: &ModelNames) -> Vec<Child> {
    let expanded: HashSet<String> = tree.nodes.keys().cloned().collect();
    let rows = agents::visible_rows(
        tree,
        Filters {
            show_finished: true,
            only_blocked: false,
        },
        &expanded,
    );
    let mut out = Vec::new();
    for row in rows {
        let node = row.node;
        if node.node_id == tree.root || (row.depth == 0 && node.parent.is_none()) {
            continue;
        }
        let mut ancestors = Vec::new();
        let mut at = node.parent.as_deref();
        while let Some(parent) = at.and_then(|id| tree.nodes.get(id)) {
            if parent.node_id == tree.root {
                break;
            }
            ancestors.push(display_name(parent).to_owned());
            at = parent.parent.as_deref();
            if ancestors.len() > tree.nodes.len() {
                break;
            }
        }
        ancestors.reverse();
        let finished = node.is_finished();
        out.push(Child {
            node_id: node.node_id.clone(),
            depth: row.depth.max(1),
            label: display_name(node).to_owned(),
            agent_type: node
                .agent_type
                .clone()
                .filter(|t| node.name.is_some() && t != display_name(node)),
            state: match node.outcome {
                Some(outcome) => State::of_outcome(outcome),
                None => State::of_node(node.state),
            },
            blocked: node.state == NodeState::Blocked && !finished,
            finished,
            activity: node
                .activity
                .as_deref()
                .map(activity_text)
                .filter(|a| !a.is_empty() && !finished),
            model: node.model.as_deref().map(|id| names.label(&node.agent, id)),
            started_at_ms: node.started_at_ms,
            ended_at_ms: node.ended_at_ms,
            usage: node.usage.as_ref().and_then(usage_label),
            transcript: node.transcript_path.clone(),
            ancestors,
        });
    }
    out
}

/// The latest `last_activity_ms` of the unfinished nodes (the main agent's included).
fn latest_activity(tree: &AgentObservation) -> Option<u64> {
    tree.nodes
        .values()
        .filter(|n| !n.is_finished())
        .map(|n| n.last_activity_ms)
        .max()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hutan_core::agent::PermissionOption;
    use hutan_core::{NodeKind, SessionKind, Usage};
    use hutan_protocol::messages::{AgentReport, WorkspaceInfo};
    use hutan_protocol::worktree::Placement;

    use super::*;

    const NOW: u64 = 1_000_000;

    fn workspace(name: &str, kind: WorkspaceKind, parent: Option<WorkspaceId>) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.into(),
            roots: Vec::new(),
            kind,
            pinned: false,
            position: 0,
            created_by: Default::default(),
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
            parent_id: parent,
            project: None,
        }
    }

    fn session(workspace: WorkspaceId, title: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id: workspace,
            kind: SessionKind::Terminal,
            title: title.into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 1,
            agent: None,
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        }
    }

    fn chat(agent: &str, assistant: bool, exact: bool) -> ChatInfo {
        ChatInfo {
            agent: agent.into(),
            model: String::new(),
            approval_policy: Default::default(),
            sandbox: Default::default(),
            turn_active: false,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: exact.then(|| "conv-1".to_owned()),
            assistant,
            images: None,
            mode: None,
            checkpoints: None,
        }
    }

    fn node(id: &str, parent: Option<&str>, state: NodeState) -> AgentNode {
        AgentNode {
            node_id: id.into(),
            parent: parent.map(str::to_owned),
            kind: if parent.is_some() {
                NodeKind::Sub
            } else {
                NodeKind::Main
            },
            source: "omp-extension".into(),
            agent: "omp".into(),
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
            pid: None,
        }
    }

    fn tree(session: SessionId, nodes: Vec<AgentNode>) -> AgentObservation {
        AgentObservation {
            session_id: session,
            root: "root".into(),
            nodes: nodes.into_iter().map(|n| (n.node_id.clone(), n)).collect(),
        }
    }

    fn report(state: AgentState, at: u64) -> AgentReport {
        AgentReport {
            source: "omp-extension".into(),
            agent: "omp".into(),
            state,
            message: None,
            agent_session_id: None,
            updated_at_ms: at,
            blocked_sub: None,
        }
    }

    fn approval(session: SessionId, title: &str, at: u64) -> ApprovalInfo {
        ApprovalInfo {
            id: ApprovalId::new(),
            session_id: session,
            turn_id: None,
            kind: hutan_core::agent::PermissionKind::Command,
            title: title.into(),
            detail: None,
            options: vec![PermissionOption {
                id: "accept".into(),
                label: "Allow once".into(),
                kind: PermissionOptionKind::AllowOnce,
                provider: None,
            }],
            requested_at_ms: at,
        }
    }

    struct World {
        state: StoreState,
        unread: BTreeMap<SessionId, Unread>,
        reviews: HashSet<SessionId>,
        seen: HashSet<SessionId>,
        snoozed: HashSet<SessionId>,
        answered: HashMap<ApprovalId, String>,
        model_names: ModelNames,
    }

    impl World {
        fn new(workspaces: Vec<WorkspaceInfo>) -> Self {
            Self {
                state: StoreState {
                    connection: ConnectionState::Connected,
                    epoch: Some(hutan_core::Epoch::new()),
                    workspaces,
                    sessions: Vec::new(),
                    approvals: Vec::new(),
                    questions: Vec::new(),
                    trees: BTreeMap::new(),
                },
                unread: BTreeMap::new(),
                reviews: HashSet::new(),
                seen: HashSet::new(),
                snoozed: HashSet::new(),
                answered: HashMap::new(),
                model_names: ModelNames::default(),
            }
        }

        fn add(&mut self, info: SessionInfo) -> SessionId {
            let id = info.id;
            self.state.sessions.push(info);
            id
        }

        fn build(&self) -> AgentPresentation {
            build(&Facts {
                state: &self.state,
                unread: &self.unread,
                reviews: &self.reviews,
                changes_to_review: &self.seen,
                snoozed: &self.snoozed,
                answered_approvals: &self.answered,
                answered_questions: &HashSet::new(),
                last_confirmed_ms: Some(NOW),
                model_names: &self.model_names,
                revision: 7,
            })
        }
    }

    fn home() -> WorkspaceInfo {
        workspace("Home", WorkspaceKind::Home, None)
    }

    #[test]
    fn mawas_with_an_unknown_root_is_one_unconfirmed_session_never_idle_or_working() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut mawas = session(home.id, "Mawas");
        mawas.kind = SessionKind::Harness;
        // A harness assistant that never handshook: no conversation id.
        mawas.chat = Some(chat("omp", true, false));
        let id = world.add(mawas);
        world
            .state
            .trees
            .insert(id, tree(id, vec![node("root", None, NodeState::Unknown)]));
        let presentation = world.build();
        assert_eq!(presentation.records.len(), 1, "Mawas is an agent session");
        let record = &presentation.records[0];
        assert_eq!(record.role, Role::Assistant);
        assert_eq!(record.state, State::Unconfirmed);
        assert_eq!(record.group, Group::Unconfirmed);
        assert_eq!(record.state.label(), "State unavailable");
        assert_eq!(record.location(), "Home / Mawas");
        assert_eq!(
            record.detail,
            "This harness has not reported its current state."
        );
        assert_eq!(presentation.counts(Scope::All).unconfirmed, 1);
        assert_eq!(presentation.counts(Scope::All).working, 0);
    }

    #[test]
    fn a_managed_chat_the_daemon_knows_exactly_proves_idle() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut mawas = session(home.id, "Mawas");
        mawas.kind = SessionKind::Harness;
        mawas.chat = Some(chat("omp", true, true));
        let id = world.add(mawas);
        world
            .state
            .trees
            .insert(id, tree(id, vec![node("root", None, NodeState::Unknown)]));
        let presentation = world.build();
        assert_eq!(presentation.records[0].state, State::Idle);
        assert_eq!(presentation.records[0].group, Group::Idle);
    }

    #[test]
    fn an_active_turn_is_working_and_beats_an_idle_report() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "chat");
        info.kind = SessionKind::Harness;
        let mut c = chat("codex", false, true);
        c.turn_active = true;
        info.chat = Some(c);
        info.agent = Some(report(AgentState::Idle, 5));
        world.add(info);
        let presentation = world.build();
        assert_eq!(presentation.records[0].state, State::Working);
    }

    #[test]
    fn a_model_reads_by_its_catalog_name_and_keeps_its_id() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "chat");
        info.kind = SessionKind::Harness;
        let mut c = chat("omp", false, true);
        c.model = "anthropic/claude-haiku-5-5".into();
        info.chat = Some(c);
        world.add(info);
        let record = world.build().records[0].clone();
        // Nothing known: the id as reported.
        assert_eq!(record.model.as_deref(), Some("anthropic/claude-haiku-5-5"));
        world.model_names.learn(
            "omp",
            &[hutan_core::agent::ModelInfo {
                id: "anthropic/claude-haiku-5-5".into(),
                display_name: "Claude Haiku 5.5".into(),
                description: String::new(),
                is_default: false,
                hidden: false,
                default_reasoning_effort: None,
                reasoning_efforts: Vec::new(),
            }],
        );
        let record = world.build().records[0].clone();
        assert_eq!(record.model.as_deref(), Some("Claude Haiku 5.5"));
        assert_eq!(
            record.model_id.as_deref(),
            Some("anthropic/claude-haiku-5-5")
        );
        // The search (lower case) finds it by either.
        assert!(record.haystack().contains("claude haiku 5.5"));
        assert!(record.haystack().contains("anthropic/claude-haiku-5-5"));
    }

    #[test]
    fn report_chat_and_root_join_into_one_record_and_the_root_is_not_a_subagent() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "work");
        info.kind = SessionKind::Harness;
        info.chat = Some(chat("omp", false, true));
        info.agent = Some(report(AgentState::Working, 90));
        let id = world.add(info);
        let mut root = node("root", None, NodeState::Working);
        root.model = Some("gpt-6".into());
        root.last_activity_ms = 100;
        world.state.trees.insert(
            id,
            tree(id, vec![root, node("a", Some("root"), NodeState::Working)]),
        );
        let presentation = world.build();
        assert_eq!(presentation.records.len(), 1);
        let record = &presentation.records[0];
        assert_eq!(record.model.as_deref(), Some("gpt-6"));
        assert_eq!(record.children.len(), 1, "only the subagent is a child");
        assert_eq!(record.subagents_live, 1);
        assert_eq!(
            presentation.counts(Scope::All).summary(),
            "1 agent session · 1 subagent"
        );
        // The newer of the two timestamps is the semantic activity.
        assert_eq!(record.updated_at_ms, Some(100));
    }

    #[test]
    fn a_blocked_child_rolls_up_once_with_its_ancestor_path() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let id = world.add(session(home.id, "work"));
        let mut worker = node("w", Some("root"), NodeState::Working);
        worker.name = Some("worker".into());
        let mut reviewer = node("r", Some("w"), NodeState::Blocked);
        reviewer.name = Some("reviewer".into());
        world.state.trees.insert(
            id,
            tree(
                id,
                vec![node("root", None, NodeState::Working), worker, reviewer],
            ),
        );
        let presentation = world.build();
        assert_eq!(presentation.records.len(), 1);
        let record = &presentation.records[0];
        assert_eq!(record.group, Group::NeedsYou);
        assert_eq!(record.reasons.len(), 1, "one reason, once");
        assert_eq!(record.reasons[0].kind, ReasonKind::Blocked);
        assert_eq!(record.reasons[0].text, "reviewer blocked");
        assert_eq!(record.subagents_blocked, 1);
        let blocked = record.children.iter().find(|c| c.blocked).unwrap();
        assert_eq!(blocked.ancestors, ["worker"]);
        assert_eq!(presentation.counts(Scope::All).needs_you, 1);
    }

    #[test]
    fn three_approvals_are_one_session_with_three_requests() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "chat");
        info.kind = SessionKind::Harness;
        info.state = SessionState::WaitingApproval;
        let mut c = chat("codex", false, true);
        c.pending_approvals = 3;
        info.chat = Some(c);
        let id = world.add(info);
        for (i, title) in ["run cargo test", "write a.rs", "write b.rs"]
            .into_iter()
            .enumerate()
        {
            world
                .state
                .approvals
                .push(approval(id, title, 10 + i as u64));
        }
        let presentation = world.build();
        let counts = presentation.counts(Scope::All);
        assert_eq!(
            (counts.sessions, counts.needs_you, counts.requests),
            (1, 1, 3)
        );
        let record = &presentation.records[0];
        assert_eq!(record.requests.len(), 3);
        assert_eq!(record.reasons.len(), 1, "the wait is the requests, once");
        assert_eq!(record.reasons[0].text, "Permission: run cargo test");
        assert_eq!(record.reasons[0].since_ms, Some(10), "the oldest first");
    }

    #[test]
    fn an_answer_given_here_is_submitting_not_actionable() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "chat");
        info.kind = SessionKind::Harness;
        info.state = SessionState::WaitingApproval;
        info.chat = Some(chat("codex", false, true));
        let id = world.add(info);
        let pending = approval(id, "run it", 1);
        world.answered.insert(pending.id, "accept".into());
        world.state.approvals.push(pending);
        let record = &world.build().records[0];
        assert!(record.requests.is_empty());
        assert_eq!(record.submitting, 1);
    }

    #[test]
    fn a_plain_shell_is_not_an_agent_even_with_unread_attention() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let shell = world.add(session(home.id, "sh"));
        world.unread.insert(shell, Unread::Failed);
        let presentation = world.build();
        assert!(presentation.records.is_empty());
        // The global attention path still sees it.
        let info = &world.state.sessions[0];
        assert!(present(info, world.unread.get(&shell).copied(), &world.model_names).needs_you);
    }

    #[test]
    fn a_session_that_ended_is_finished_whatever_the_stale_report_says() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "done");
        info.state = SessionState::Exited {
            code: Some(2),
            signal: None,
        };
        info.agent = Some(report(AgentState::Working, 50));
        world.add(info);
        let record = &world.build().records[0];
        assert_eq!(record.state, State::Failed);
        assert_eq!(record.group, Group::Finished);
        assert_eq!(record.detail, "Exited with code 2");
        assert_eq!(record.activity, None);
    }

    #[test]
    fn an_unread_failure_and_completion_and_review_are_reasons_in_priority_order() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut failed = session(home.id, "failed");
        failed.state = SessionState::Exited {
            code: Some(2),
            signal: None,
        };
        failed.agent = Some(report(AgentState::Idle, 1));
        let failed = world.add(failed);
        world.unread.insert(failed, Unread::Failed);
        let mut done = session(home.id, "done");
        done.state = SessionState::Exited {
            code: Some(0),
            signal: None,
        };
        done.agent = Some(report(AgentState::Idle, 1));
        let done = world.add(done);
        world.unread.insert(done, Unread::Done);
        world.reviews.insert(done);
        let presentation = world.build();
        let by = |id| presentation.get(id).unwrap();
        assert_eq!(by(failed).headline().unwrap().text, "Failed: exit 2");
        assert_eq!(
            by(done).reasons.iter().map(|r| r.kind).collect::<Vec<_>>(),
            [ReasonKind::Review, ReasonKind::CompletedUnread],
            "review ranks above completed-unread"
        );
        assert_eq!(by(failed).group, Group::NeedsYou);
        // Failure outranks review in the list.
        assert_eq!(presentation.records[0].session, failed);
    }

    #[test]
    fn a_running_turn_is_working_not_review_and_seen_work_is_idle_with_changes_to_review() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "turn 2");
        info.kind = SessionKind::Harness;
        let mut busy = chat("omp", false, true);
        busy.turn_active = true;
        info.chat = Some(busy);
        let running = world.add(info.clone());
        world.reviews.insert(running);
        let mut idle = info;
        idle.id = SessionId::new();
        idle.title = "looked at".into();
        idle.chat = Some(chat("omp", false, true));
        let seen = world.add(idle);
        world.seen.insert(seen);
        let presentation = world.build();
        let by = |id| presentation.get(id).unwrap();
        assert_eq!(by(running).state, State::Working);
        assert_eq!(by(running).group, Group::Working, "working beats review");
        assert!(by(running).reasons.is_empty());
        assert!(!by(running).changes_to_review);
        assert_eq!(by(seen).group, Group::Idle, "seen work needs nobody");
        assert_eq!(
            by(seen).state,
            State::Idle,
            "seen review is no state of its own"
        );
        assert!(by(seen).reasons.is_empty());
        assert!(by(seen).changes_to_review);
        assert_eq!(presentation.counts(Scope::All).needs_you, 0);
    }

    #[test]
    fn mawas_failure_is_not_attention() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "Mawas");
        info.kind = SessionKind::Harness;
        info.state = SessionState::Failed {
            reason: "omp not found".into(),
        };
        info.chat = Some(chat("omp", true, false));
        let id = world.add(info);
        world.unread.insert(id, Unread::Failed);
        let record = &world.build().records[0];
        assert_eq!(record.group, Group::Finished);
        assert_eq!(record.detail, "omp not found");
    }

    #[test]
    fn missing_model_cost_and_start_are_not_zero_filled() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let id = world.add(session(home.id, "work"));
        let mut root = node("root", None, NodeState::Working);
        root.usage = Some(Usage {
            kind: "omp.tokens".into(),
            ..Usage::default()
        });
        world.state.trees.insert(id, tree(id, vec![root]));
        let record = &world.build().records[0];
        assert_eq!(record.model, None);
        assert_eq!(record.usage, None, "no counter was sent");
        assert_eq!(record.started_at_ms, None);
        assert_eq!(record.ended_at_ms, None);
    }

    #[test]
    fn conflicting_sources_show_the_newer_one_and_say_so() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "work");
        info.agent = Some(report(AgentState::Working, 10));
        let id = world.add(info);
        let mut root = node("root", None, NodeState::Idle);
        root.last_activity_ms = 20;
        world.state.trees.insert(id, tree(id, vec![root]));
        let record = &world.build().records[0];
        assert_eq!(record.state, State::Idle);
        assert!(
            record
                .provenance
                .iter()
                .any(|p| p.starts_with("Sources disagree")),
            "{:?}",
            record.provenance
        );
    }

    #[test]
    fn a_finished_root_wins_over_a_working_report() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "work");
        info.agent = Some(report(AgentState::Working, 10));
        let id = world.add(info);
        let mut root = node("root", None, NodeState::Idle);
        root.outcome = Some(NodeOutcome::Completed);
        world.state.trees.insert(id, tree(id, vec![root]));
        let record = &world.build().records[0];
        assert_eq!(record.state, State::Completed);
        assert_eq!(record.group, Group::Finished);
    }

    #[test]
    fn offline_keeps_the_last_records_and_loading_has_none() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "work");
        info.agent = Some(report(AgentState::Idle, 1));
        world.add(info);
        world.state.connection = ConnectionState::Reconnecting {
            attempt: 1,
            retry_in: std::time::Duration::from_secs(1),
        };
        let presentation = world.build();
        assert_eq!(presentation.records.len(), 1);
        assert_eq!(
            presentation.link,
            Link::Offline {
                last_confirmed_ms: Some(NOW)
            }
        );
        world.state.connection = ConnectionState::Connecting;
        world.state.epoch = None;
        let presentation = world.build();
        assert_eq!(presentation.link, Link::Loading);
        assert!(presentation.records.is_empty());
    }

    #[test]
    fn order_is_by_group_then_activity_with_unknown_last_and_the_id_as_tie_breaker() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        // A report time of 0 is "not known": it sorts after every known time.
        for (title, at) in [("old", 10), ("new", 30), ("unknown", 0)] {
            let mut info = session(home.id, title);
            info.agent = Some(report(AgentState::Working, at));
            world.add(info);
        }
        let titles: Vec<String> = world
            .build()
            .records
            .iter()
            .map(|r| r.title.clone())
            .collect();
        assert_eq!(titles, ["new", "old", "unknown"]);
    }

    #[test]
    fn needs_you_orders_by_reason_then_oldest_first() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut chats = Vec::new();
        for (title, at) in [("late", 50), ("early", 5)] {
            let mut info = session(home.id, title);
            info.kind = SessionKind::Harness;
            info.state = SessionState::WaitingApproval;
            info.chat = Some(chat("codex", false, true));
            let id = world.add(info);
            world.state.approvals.push(approval(id, "x", at));
            chats.push(id);
        }
        let mut blocked = session(home.id, "blocked");
        blocked.agent = Some(report(AgentState::Blocked, 1));
        world.add(blocked);
        let titles: Vec<String> = world
            .build()
            .records
            .iter()
            .map(|r| r.title.clone())
            .collect();
        assert_eq!(titles, ["early", "late", "blocked"]);
    }

    #[test]
    fn scope_matches_a_workspace_and_its_projects() {
        let home = home();
        let team = workspace("Team", WorkspaceKind::Workspace, None);
        let project = workspace("hutan", WorkspaceKind::Project, Some(team.id));
        let other = workspace("elsewhere", WorkspaceKind::Project, None);
        let mut world = World::new(vec![
            home.clone(),
            team.clone(),
            project.clone(),
            other.clone(),
        ]);
        for (ws, title) in [(project.id, "in-team"), (other.id, "alone")] {
            let mut info = session(ws, title);
            info.agent = Some(report(AgentState::Idle, 1));
            world.add(info);
        }
        let presentation = world.build();
        let titles = |scope| -> Vec<String> {
            presentation
                .in_scope(scope)
                .map(|r| r.title.clone())
                .collect()
        };
        assert_eq!(titles(Scope::All).len(), 2);
        assert_eq!(titles(Scope::Workspace(team.id)), ["in-team"]);
        assert_eq!(titles(Scope::Workspace(project.id)), ["in-team"]);
        assert_eq!(titles(Scope::Workspace(other.id)), ["alone"]);
        let record = presentation
            .records
            .iter()
            .find(|r| r.title == "in-team")
            .unwrap();
        assert_eq!(record.location(), "hutan / Team");
    }

    #[test]
    fn the_checkout_is_separate_from_the_project() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        let mut info = session(home.id, "wt");
        info.agent = Some(report(AgentState::Idle, 1));
        info.placement = Some(Placement {
            kind: PlacementKind::Worktree,
            root: PathBuf::from("/code/hutan-wt/chrome-v3"),
            branch: Some("slice/AG-1".into()),
            shared_with: Vec::new(),
        });
        world.add(info);
        let mut main = session(home.id, "main");
        main.agent = Some(report(AgentState::Idle, 1));
        main.placement = Some(Placement {
            kind: PlacementKind::Main,
            root: PathBuf::from("/code/hutan"),
            branch: Some("master".into()),
            shared_with: Vec::new(),
        });
        world.add(main);
        let presentation = world.build();
        let label = |title: &str| {
            presentation
                .records
                .iter()
                .find(|r| r.title == title)
                .unwrap()
                .checkout
                .clone()
                .unwrap()
                .label
        };
        assert_eq!(label("wt"), "Worktree: chrome-v3");
        assert_eq!(label("main"), "Main checkout: master");
    }

    #[test]
    fn nothing_counts_a_group_twice() {
        let home = home();
        let mut world = World::new(vec![home.clone()]);
        for state in [AgentState::Working, AgentState::Idle, AgentState::Blocked] {
            let mut info = session(home.id, "s");
            info.agent = Some(report(state, 1));
            world.add(info);
        }
        let counts = world.build().counts(Scope::All);
        assert_eq!(
            counts.needs_you + counts.working + counts.unconfirmed + counts.idle + counts.finished,
            counts.sessions
        );
    }

    #[test]
    fn zero_one_fifty_and_a_thousand_sessions_each_make_one_record_in_a_stable_order() {
        for n in [0usize, 1, 50, 1000] {
            let home = home();
            let mut world = World::new(vec![home.clone()]);
            let mut ids = Vec::new();
            for i in 0..n {
                let id = world.add(session(home.id, &format!("agent-{i}")));
                let state = if i % 3 == 0 {
                    NodeState::Working
                } else {
                    NodeState::Idle
                };
                world
                    .state
                    .trees
                    .insert(id, tree(id, vec![node("root", None, state)]));
                ids.push(id);
            }
            // Plain shells are no agents, however many.
            for i in 0..n {
                world.add(session(home.id, &format!("shell-{i}")));
            }
            let built = world.build();
            assert_eq!(built.records.len(), n, "{n} agents");
            assert_eq!(built.counts(Scope::All).sessions, n);
            assert_eq!(
                built.counts(Scope::All).working + built.counts(Scope::All).idle,
                n
            );
            // The same facts give the same order (the id is the last tie-breaker).
            let again = world.build();
            let order =
                |p: &AgentPresentation| p.records.iter().map(|r| r.session).collect::<Vec<_>>();
            assert_eq!(order(&built), order(&again));
        }
    }
}
