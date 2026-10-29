//! The transcript: [`AgentEvent`]s folded into the entries the chat view draws.
//!
//! Events are applied in any mix of journal replay and live delivery. Every event is applied at
//! most once (by its `seq`), streaming text only grows an item that is still open, and an item is
//! found by its id, so a reconnect that replays what was already seen, or a history reload that
//! overlaps live events, changes nothing twice. Deltas are not journaled: text that streamed
//! while nobody listened is complete again when the item's `*_end` event arrives, or sooner when
//! the journal's `message_partial` / `reasoning_partial` snapshot of the open item arrives: it
//! holds the text of every delta with a smaller `seq`, so the item takes it and the deltas that
//! came live before it are neither lost nor added twice.
//!
//! [`Transcript::prepend`] folds in an older page of the journal ("Load earlier") in front of
//! what is there, without disturbing the state of the turn in progress.

use std::collections::{HashMap, HashSet};

use hutan_core::agent::{
    DecidedBy, FileChange, NoticeLevel, PermissionKind, PermissionOption, PermissionOptionKind,
    PermissionOutcome, PlanStep, ReasoningPart, TokenCounts, ToolKind, ToolStatus,
};
use hutan_core::question::QuestionSpec;
use hutan_core::{
    AgentEvent, AgentEventKind, ApprovalId, ItemId, QuestionId, QuestionOutcome, TurnId,
};
use serde_json::Value;

/// Bytes of live command output kept per tool; the final output replaces it at the end.
pub const MAX_LIVE_OUTPUT: usize = 256 * 1024;
/// `tool_update` messages kept per tool.
const MAX_UPDATES: usize = 20;

#[derive(Debug, Clone)]
pub struct Message {
    pub item: Option<ItemId>,
    pub text: String,
    pub phase: Option<String>,
    /// The `message_end` event arrived (or the turn ended).
    pub done: bool,
    stream: Stream,
}

/// What an open item needs to reconcile a journal snapshot with the deltas that came live.
#[derive(Debug, Clone, Default)]
struct Stream {
    /// The highest `seq` whose delta (or snapshot) the text includes.
    through: u64,
    /// The item was first seen through a delta: the start of its text is missing until a
    /// snapshot (or the end event) brings it.
    prefix_missing: bool,
    /// The deltas seen while the prefix is missing, to put back after a snapshot.
    tail: Vec<(u64, Option<ReasoningPart>, String)>,
}

impl Stream {
    /// A streamed piece at `seq`; `true` when it extends the text (it does not when a snapshot
    /// already included it). `known`: the item had been seen before this piece.
    fn delta(&mut self, seq: u64, known: bool, part: Option<ReasoningPart>, text: &str) -> bool {
        if seq <= self.through {
            return false;
        }
        if !known {
            self.prefix_missing = true;
        }
        if self.prefix_missing {
            self.tail.push((seq, part, text.to_owned()));
        }
        self.through = seq;
        true
    }

    /// A snapshot journaled at `seq` holds every piece with a smaller `seq`. `parts` (one text
    /// per part, `[summary, content]`; a message only has the first) become what they should
    /// be and `true` is returned, or `false` when the text already includes more than the
    /// snapshot.
    fn snapshot_parts(&mut self, seq: u64, parts: &mut [String; 2]) -> bool {
        let through = seq.saturating_sub(1);
        if self.prefix_missing {
            // Pieces that came live while the start was missing; those the snapshot already has
            // are dropped, the later ones go after it.
            for (at, part, piece) in self.tail.drain(..) {
                if at > through {
                    let slot = usize::from(part == Some(ReasoningPart::Content));
                    parts[slot].push_str(&piece);
                }
            }
            self.prefix_missing = false;
            self.through = self.through.max(through);
            true
        } else if through > self.through {
            self.through = through;
            true
        } else {
            false
        }
    }

    fn snapshot(&mut self, seq: u64, text: &str) -> Option<String> {
        let mut parts = [text.to_owned(), String::new()];
        self.snapshot_parts(seq, &mut parts)
            .then(|| std::mem::take(&mut parts[0]))
    }
}

#[derive(Debug, Clone)]
pub struct Reasoning {
    pub item: Option<ItemId>,
    pub summary: String,
    pub content: String,
    pub done: bool,
    stream: Stream,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub item: Option<ItemId>,
    pub kind: ToolKind,
    pub name: String,
    /// One line: the command, the files, the query.
    pub title: String,
    pub input: Option<Value>,
    /// `None` while the tool runs.
    pub status: Option<ToolStatus>,
    /// The turn ended before the tool reported an end.
    pub interrupted: bool,
    /// Live output while it runs, the complete output after.
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub updates: Vec<String>,
    pub changes: Vec<FileChange>,
    /// Permission requests made for this tool, oldest first.
    pub approvals: Vec<ApprovalId>,
}

impl Tool {
    fn new(item: Option<ItemId>, kind: ToolKind) -> Self {
        Self {
            item,
            kind,
            name: String::new(),
            title: String::new(),
            input: None,
            status: None,
            interrupted: false,
            output: String::new(),
            truncated: false,
            exit_code: None,
            duration_ms: None,
            updates: Vec::new(),
            changes: Vec::new(),
            approvals: Vec::new(),
        }
    }

    pub fn running(&self) -> bool {
        self.status.is_none() && !self.interrupted
    }

    /// The working directory of a command, from its input.
    pub fn cwd(&self) -> Option<&str> {
        self.input.as_ref()?.get("cwd")?.as_str()
    }

    /// The command line of a command, from its input (the title otherwise).
    pub fn command(&self) -> &str {
        self.input
            .as_ref()
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str)
            .unwrap_or(&self.title)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Selected {
        option_id: String,
    },
    /// The turn ended, the agent withdrew the request, or it went away.
    Expired,
}

#[derive(Debug, Clone)]
pub struct Approval {
    pub id: ApprovalId,
    pub turn: Option<TurnId>,
    pub kind: PermissionKind,
    pub title: String,
    pub detail: Option<String>,
    pub options: Vec<PermissionOption>,
    pub state: ApprovalState,
    /// Who answered (protocol 0.15); absent while pending, after an expiry, and for answers
    /// journaled by an older daemon.
    pub decided_by: Option<DecidedBy>,
}

impl Approval {
    /// What was decided, in the past tense ("Allowed once"); the option's own label for a
    /// decision this build has no wording for, "Answered" when the option is not among those
    /// offered.
    pub fn decision_text(&self) -> String {
        match self.chosen() {
            Some(option) => match option.kind {
                PermissionOptionKind::AllowOnce => "Allowed once".to_owned(),
                PermissionOptionKind::AllowAlways => "Always allowed".to_owned(),
                PermissionOptionKind::RejectOnce => "Denied once".to_owned(),
                PermissionOptionKind::RejectAlways => "Always denied".to_owned(),
                PermissionOptionKind::Cancel => "Cancelled".to_owned(),
                PermissionOptionKind::Other => option.label.clone(),
            },
            None => "Answered".to_owned(),
        }
    }

    /// Where the answer came from, for a label ("Allowed once · from CLI"): `here` says this
    /// window sent it. A kind this build does not know, an answer journaled before protocol
    /// 0.15 and one the daemon could not attribute are "another client".
    pub fn answered_from(&self, here: bool) -> &'static str {
        if here {
            return "from this window";
        }
        match self.decided_by.as_ref().map(|by| by.client_kind.as_str()) {
            Some("cli") => "from CLI",
            Some("desktop") => "from another window",
            Some("notifier") => "from a notification",
            _ => "from another client",
        }
    }

    /// The option that was chosen, when one was.
    pub fn chosen(&self) -> Option<&PermissionOption> {
        match &self.state {
            ApprovalState::Selected { option_id } => {
                self.options.iter().find(|o| o.id == *option_id)
            }
            _ => None,
        }
    }
}

/// How a question of the agent stands.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionState {
    Pending,
    /// Answered: `values` are the answers except those of secret fields, which the journal
    /// never held (`withheld` names them).
    Answered {
        values: hutan_core::question::Values,
        withheld: Vec<String>,
    },
    Declined,
    Cancelled,
    /// The turn ended, the agent went away, or the daemon restarted before anyone answered.
    Expired,
}

impl QuestionState {
    fn of(outcome: &QuestionOutcome) -> Self {
        match outcome {
            QuestionOutcome::Answered { values, withheld } => Self::Answered {
                values: values.clone(),
                withheld: withheld.clone(),
            },
            QuestionOutcome::Declined => Self::Declined,
            QuestionOutcome::Cancelled => Self::Cancelled,
            QuestionOutcome::Expired => Self::Expired,
        }
    }
}

/// A question the agent put to the user (protocol 0.24).
#[derive(Debug, Clone)]
pub struct Question {
    pub id: QuestionId,
    pub turn: Option<TurnId>,
    pub spec: QuestionSpec,
    pub state: QuestionState,
    /// Who decided; absent while pending, after an expiry, and when the daemon could not tell.
    pub decided_by: Option<DecidedBy>,
}

impl Question {
    /// What was decided, in the past tense.
    pub fn decision_text(&self) -> &'static str {
        match self.state {
            QuestionState::Pending => "Waiting for your answer",
            QuestionState::Answered { .. } => "Answered",
            QuestionState::Declined => "Declined",
            QuestionState::Cancelled => "Cancelled",
            QuestionState::Expired => "Expired",
        }
    }

    /// Where the answer came from ("from CLI"); `here`: this window sent it.
    pub fn answered_from(&self, here: bool) -> &'static str {
        if here {
            return "from this window";
        }
        match self.decided_by.as_ref().map(|by| by.client_kind.as_str()) {
            Some("cli") => "from CLI",
            Some("desktop") => "from another window",
            Some("notifier") => "from a notification",
            _ => "from another client",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TurnDiff {
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Failed {
        message: String,
        details: Option<String>,
    },
    Cancelled,
}

/// What one turn cost; a field the provider did not report stays `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnUsage {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Tokens the whole conversation has used so far.
    pub conversation_tokens: Option<u64>,
    pub context_window: Option<u64>,
}

/// How much of the model's context the conversation fills, from the latest usage event that
/// reported a total (what the turn footers' `context X / Y` read too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUse {
    /// Tokens in the context now (see [`Transcript::context_use`]).
    pub used: u64,
    /// The model's context window, when the harness reported one.
    pub window: Option<u64>,
    /// The cumulative counts the harness reported next to the total; a field it did not report
    /// is `None`.
    pub counts: TokenCounts,
}

impl ContextUse {
    /// Percent of the window used, rounded down; `None` without a window.
    pub fn percent(&self) -> Option<u64> {
        let window = self.window?;
        Some(self.used.saturating_mul(100) / window)
    }
}

#[derive(Debug, Clone)]
pub struct Footer {
    pub outcome: TurnOutcome,
    pub duration_ms: Option<u64>,
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, Clone)]
pub enum Body {
    User(Message),
    Assistant(Message),
    Reasoning(Reasoning),
    Tool(Tool),
    /// An approval whose tool is not in the transcript.
    Approval(ApprovalId),
    /// A question of the agent (protocol 0.24).
    Question(QuestionId),
    Plan(Plan),
    TurnDiff(TurnDiff),
    Footer(Footer),
    Notice {
        level: NoticeLevel,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable for the life of the transcript (view state such as "expanded" hangs on it).
    pub id: u64,
    /// The transcript's revision when this entry last changed.
    pub rev: u64,
    pub ts_ms: u64,
    pub turn: Option<TurnId>,
    pub body: Body,
}

#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    by_item: HashMap<ItemId, usize>,
    approvals: HashMap<ApprovalId, Approval>,
    approval_entry: HashMap<ApprovalId, usize>,
    questions: HashMap<QuestionId, Question>,
    question_entry: HashMap<QuestionId, usize>,
    turn_plan: HashMap<TurnId, usize>,
    turn_diff: HashMap<TurnId, usize>,
    /// Turns whose user message is the measurement command (`/context`, sent by *Measure
    /// context*): not part of the conversation the reader sees.
    measure_turns: HashSet<TurnId>,
    seen: HashSet<u64>,
    max_seq: u64,
    rev: u64,
    epoch: u64,
    /// Entries ever put in front by [`prepend`](Self::prepend), since the last reset.
    prepended: u64,
    next_id: u64,
    active_turn: bool,
    /// Index of the first entry of the turn in progress.
    turn_start: usize,
    /// The latest cumulative counts, the model's window, and the tokens now in the context
    /// (`last` when the harness reports both, else the total: see [`Transcript::context_use`]).
    usage: Option<(TokenCounts, Option<u64>, Option<u64>)>,
    usage_at_turn_start: TokenCounts,
    /// The usage before the turn is not known (an older page of the journal is being read
    /// and the one before it is not): the footer then reports no per-turn figures.
    usage_baseline_unknown: bool,
    /// Answers that arrived before their request (the request is in an older page).
    early_answers: HashMap<ApprovalId, (ApprovalState, Option<DecidedBy>)>,
    /// The same for questions.
    early_decisions: HashMap<QuestionId, (QuestionState, Option<DecidedBy>)>,
    /// A page older than the transcript is being folded in.
    backfill: bool,
    /// ... and the page before that one is not known either.
    blind_start: bool,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Whether `entry` belongs to a measurement (`/context` and the harness's answer to it): a
    /// view leaves it out. A permission request or question stays visible whatever turn it is
    /// in, so nothing that waits for the user is ever hidden.
    pub fn is_measurement(&self, entry: &Entry) -> bool {
        !matches!(entry.body, Body::Approval(_) | Body::Question(_))
            && entry
                .turn
                .is_some_and(|turn| self.measure_turns.contains(&turn))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Changes with every change of an entry; an entry whose `rev` is newer than the one a view
    /// last saw has to be drawn again.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Changes when the entries were replaced wholesale ([`reset`](Self::reset)), which a view
    /// must treat as an entirely new list.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many entries [`prepend`](Self::prepend) has put in front since the transcript was
    /// last replaced wholesale: a view that keeps per-entry state by position shifts it by the
    /// difference it has not seen yet.
    pub fn prepended(&self) -> u64 {
        self.prepended
    }

    /// The highest `seq` applied.
    pub fn max_seq(&self) -> u64 {
        self.max_seq
    }

    /// The context use the latest usage event reported; `None` while no event reported a total
    /// (a harness that reports none never gets a meter, a zero would be a lie).
    ///
    /// What fills the context depends on what the harness counts. A harness that reports the
    /// conversation's cumulative `total` together with the latest request's `last` (Codex,
    /// direct chats) fills the context with `last`: the next request resends the history, so
    /// the cumulative figure grows past the window while the context does not. A harness that
    /// reports only a `total` (an ACP `usage_update`'s `used`) says how full the context is
    /// with it.
    pub fn context_use(&self) -> Option<ContextUse> {
        let (counts, window, context) = self.usage?;
        Some(ContextUse {
            used: context?,
            window: window.filter(|window| *window > 0),
            counts,
        })
    }

    /// A turn is in progress, as far as the events say.
    pub fn turn_active(&self) -> bool {
        self.active_turn
    }

    pub fn approval(&self, id: ApprovalId) -> Option<&Approval> {
        self.approvals.get(&id)
    }

    /// Permission requests still waiting for an answer, oldest first.
    pub fn pending_approvals(&self) -> Vec<&Approval> {
        let mut pending: Vec<(usize, &Approval)> = self
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending)
            .map(|a| {
                (
                    self.approval_entry
                        .get(&a.id)
                        .copied()
                        .unwrap_or(usize::MAX),
                    a,
                )
            })
            .collect();
        pending.sort_by_key(|(at, a)| (*at, a.id));
        pending.into_iter().map(|(_, a)| a).collect()
    }

    pub fn question(&self, id: QuestionId) -> Option<&Question> {
        self.questions.get(&id)
    }

    /// Questions still waiting for an answer, oldest first.
    pub fn pending_questions(&self) -> Vec<&Question> {
        let mut pending: Vec<(usize, &Question)> = self
            .questions
            .values()
            .filter(|q| q.state == QuestionState::Pending)
            .map(|q| {
                (
                    self.question_entry
                        .get(&q.id)
                        .copied()
                        .unwrap_or(usize::MAX),
                    q,
                )
            })
            .collect();
        pending.sort_by_key(|(at, q)| (*at, q.id));
        pending.into_iter().map(|(_, q)| q).collect()
    }

    /// Questions the journal shows as open that the daemon no longer holds are over.
    pub fn expire_missing_questions(&mut self, pending: &HashSet<QuestionId>) {
        let stale: Vec<QuestionId> = self
            .questions
            .values()
            .filter(|q| q.state == QuestionState::Pending && !pending.contains(&q.id))
            .map(|q| q.id)
            .collect();
        for id in stale {
            self.resolve_question(id, QuestionState::Expired, None);
        }
    }

    /// Requests the journal shows as open that the daemon no longer holds are over (a daemon
    /// that restarted expires them without a journal entry of their own).
    pub fn expire_missing(&mut self, pending: &HashSet<ApprovalId>) {
        let stale: Vec<ApprovalId> = self
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending && !pending.contains(&a.id))
            .map(|a| a.id)
            .collect();
        for id in stale {
            self.set_approval_state(id, ApprovalState::Expired);
        }
    }

    /// Forgets every entry (the daemon restarted: its sequence numbers start over).
    pub fn reset(&mut self) {
        let epoch = self.epoch + 1;
        let rev = self.rev + 1;
        *self = Self::default();
        self.epoch = epoch;
        self.rev = rev;
    }

    // ---- applying events -------------------------------------------------------------------

    /// Folds in a page of the journal that is older than everything applied so far ("Load
    /// earlier"): its entries go in front of the others, and what is already there is not
    /// disturbed: an event applied before is skipped, an item that also has newer events is
    /// completed in place, and the state of the turn in progress (`turn_active`, the usage its
    /// footer will report) stays as it was. `reached_start`: no page precedes this one, which
    /// tells the first turn's footer that its usage started from zero.
    ///
    /// Returns how many entries were added in front (the others moved down by that many).
    pub fn prepend(&mut self, events: &[AgentEvent], reached_start: bool) -> usize {
        let before = self.entries.len();
        let saved = (
            self.active_turn,
            self.turn_start,
            self.usage.take(),
            std::mem::take(&mut self.usage_at_turn_start),
            self.usage_baseline_unknown,
        );
        self.backfill = true;
        self.blind_start = !reached_start;
        self.usage_baseline_unknown = false;
        // Entries of the older page are appended while it is read; those of the turn that ends
        // inside it are the ones that ending closes.
        self.turn_start = before;
        for event in events {
            self.apply(event);
        }
        self.backfill = false;
        self.blind_start = false;
        (
            self.active_turn,
            self.turn_start,
            self.usage,
            self.usage_at_turn_start,
            self.usage_baseline_unknown,
        ) = saved;

        let added = self.entries.len() - before;
        if added == 0 {
            return 0;
        }
        // The new entries were appended; they belong in front.
        self.entries.rotate_left(before);
        let remap = |at: usize| {
            if at >= before {
                at - before
            } else {
                at + added
            }
        };
        for at in self
            .by_item
            .values_mut()
            .chain(self.approval_entry.values_mut())
            .chain(self.question_entry.values_mut())
            .chain(self.turn_plan.values_mut())
            .chain(self.turn_diff.values_mut())
        {
            *at = remap(*at);
        }
        self.turn_start += added;
        self.prepended += added as u64;
        added
    }

    /// Folds `event` in. `false` when it was applied before.
    pub fn apply(&mut self, event: &AgentEvent) -> bool {
        if !self.seen.insert(event.seq) {
            return false;
        }
        self.max_seq = self.max_seq.max(event.seq);
        let item = event.item_id;
        let turn = event.turn_id;
        match &event.kind {
            AgentEventKind::TurnStarted => {
                self.active_turn = true;
                self.turn_start = self.entries.len();
                self.usage_at_turn_start = self.usage.map(|(total, ..)| total).unwrap_or_default();
                self.usage_baseline_unknown = self.blind_start && self.usage.is_none();
            }
            AgentEventKind::MessageStart { role, phase } => {
                let at = self.message_entry(*role, item, turn, event.ts_ms);
                if let Some(message) = self.message_mut(at) {
                    message.phase = phase.clone();
                }
                self.touch(at);
            }
            AgentEventKind::MessageDelta { text } => {
                let known = item.is_some_and(|i| self.by_item.contains_key(&i));
                let at = self.message_entry(
                    hutan_core::agent::MessageRole::Assistant,
                    item,
                    turn,
                    event.ts_ms,
                );
                if let Some(message) = self.message_mut(at)
                    && !message.done
                    && message.stream.delta(event.seq, known, None, text)
                {
                    message.text.push_str(text);
                    self.touch(at);
                }
            }
            AgentEventKind::MessagePartial {
                message_id,
                phase,
                text,
            } => {
                let at = self.message_entry(
                    hutan_core::agent::MessageRole::Assistant,
                    Some(*message_id),
                    turn,
                    event.ts_ms,
                );
                if let Some(message) = self.message_mut(at)
                    && !message.done
                {
                    if message.phase.is_none() {
                        message.phase.clone_from(phase);
                    }
                    if let Some(text) = message.stream.snapshot(event.seq, text) {
                        message.text = text;
                    }
                }
                self.touch(at);
            }
            AgentEventKind::MessageEnd { role, phase, text } => {
                let at = self.message_entry(*role, item, turn, event.ts_ms);
                if let Some(message) = self.message_mut(at) {
                    message.text.clone_from(text);
                    message.phase = phase.clone();
                    message.done = true;
                    message.stream = Stream::default();
                }
                if matches!(role, hutan_core::agent::MessageRole::User)
                    && text.trim() == hutan_core::insight::MEASURE_COMMAND
                    && let Some(turn) = turn
                {
                    self.measure_turns.insert(turn);
                }
                self.touch(at);
            }
            AgentEventKind::ReasoningStart => {
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                self.touch(at);
            }
            AgentEventKind::ReasoningDelta { part, text } => {
                let known = item.is_some_and(|i| self.by_item.contains_key(&i));
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                if let Body::Reasoning(reasoning) = &mut self.entries[at].body
                    && !reasoning.done
                    && reasoning.stream.delta(event.seq, known, Some(*part), text)
                {
                    match part {
                        ReasoningPart::Summary => reasoning.summary.push_str(text),
                        ReasoningPart::Content => reasoning.content.push_str(text),
                    }
                    self.touch(at);
                }
            }
            AgentEventKind::ReasoningPartial {
                reasoning_id,
                summary,
                content,
            } => {
                let at = self.reasoning_entry(Some(*reasoning_id), turn, event.ts_ms);
                if let Body::Reasoning(reasoning) = &mut self.entries[at].body
                    && !reasoning.done
                {
                    // A snapshot of both parts at once: the summary and the content are
                    // reconciled together, as one text per part.
                    let mut parts = [summary.clone(), content.clone()];
                    if reasoning.stream.snapshot_parts(event.seq, &mut parts) {
                        let [summary, content] = parts;
                        reasoning.summary = summary;
                        reasoning.content = content;
                    }
                }
                self.touch(at);
            }
            AgentEventKind::ReasoningEnd { summary, content } => {
                let at = self.reasoning_entry(item, turn, event.ts_ms);
                if let Body::Reasoning(reasoning) = &mut self.entries[at].body {
                    reasoning.summary = summary.join("\n\n");
                    reasoning.content = content.join("\n\n");
                    reasoning.done = true;
                    reasoning.stream = Stream::default();
                }
                self.touch(at);
            }
            AgentEventKind::ToolStart {
                tool,
                name,
                title,
                input,
            } => {
                let at = self.tool_entry(item, turn, *tool, event.ts_ms);
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.kind = *tool;
                    card.name.clone_from(name);
                    card.title.clone_from(title);
                    card.input.clone_from(input);
                }
                self.touch(at);
            }
            AgentEventKind::ToolUpdate { message } => {
                if let Some(at) = self.known_tool(item)
                    && let Body::Tool(card) = &mut self.entries[at].body
                {
                    card.updates.push(message.clone());
                    if card.updates.len() > MAX_UPDATES {
                        card.updates.remove(0);
                    }
                    self.touch(at);
                }
            }
            AgentEventKind::CommandOutput { text } => {
                if let Some(at) = self.known_tool(item)
                    && let Body::Tool(card) = &mut self.entries[at].body
                    && card.running()
                {
                    card.output.push_str(text);
                    cut_front(&mut card.output, MAX_LIVE_OUTPUT);
                    self.touch(at);
                }
            }
            AgentEventKind::ToolEnd {
                tool,
                status,
                output,
                truncated,
                exit_code,
                duration_ms,
            } => {
                let at = self.tool_entry(item, turn, *tool, event.ts_ms);
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.status = Some(*status);
                    card.interrupted = false;
                    if let Some(output) = output {
                        card.output.clone_from(output);
                    }
                    card.truncated = *truncated;
                    card.exit_code = *exit_code;
                    card.duration_ms = *duration_ms;
                }
                self.touch(at);
            }
            AgentEventKind::FileDiff { changes, patch } => {
                self.file_diff(item, turn, changes, patch.as_deref(), event.ts_ms);
            }
            AgentEventKind::PermissionRequested {
                approval_id,
                kind,
                title,
                detail,
                options,
            } => {
                self.permission_requested(
                    item,
                    turn,
                    Approval {
                        id: *approval_id,
                        turn,
                        kind: *kind,
                        title: title.clone(),
                        detail: detail.clone(),
                        options: options.clone(),
                        state: ApprovalState::Pending,
                        decided_by: None,
                    },
                    event.ts_ms,
                );
            }
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
                decided_by,
            } => {
                let state = match outcome {
                    PermissionOutcome::Selected { option_id } => ApprovalState::Selected {
                        option_id: option_id.clone(),
                    },
                    PermissionOutcome::Expired => ApprovalState::Expired,
                };
                self.resolve_approval(*approval_id, state, decided_by.clone());
            }
            AgentEventKind::QuestionRequested { question_id, spec } => {
                self.question_requested(
                    turn,
                    Question {
                        id: *question_id,
                        turn,
                        spec: spec.clone(),
                        state: QuestionState::Pending,
                        decided_by: None,
                    },
                    event.ts_ms,
                );
            }
            AgentEventKind::QuestionResolved {
                question_id,
                outcome,
                decided_by,
            } => {
                self.resolve_question(*question_id, QuestionState::of(outcome), decided_by.clone());
            }
            AgentEventKind::Plan {
                explanation,
                steps,
                text,
            } => {
                let plan = Plan {
                    explanation: explanation.clone(),
                    steps: steps.clone(),
                    text: text.clone(),
                };
                let existing = match (item, turn) {
                    (Some(item), _) => self.by_item.get(&item).copied(),
                    (None, Some(turn)) => self.turn_plan.get(&turn).copied(),
                    (None, None) => None,
                };
                let at = match existing {
                    Some(at) => {
                        // An older page never overwrites what a newer event already said.
                        if !self.backfill {
                            self.entries[at].body = Body::Plan(plan);
                        }
                        at
                    }
                    None => {
                        let at = self.push(turn, event.ts_ms, Body::Plan(plan));
                        match (item, turn) {
                            (Some(item), _) => {
                                self.by_item.insert(item, at);
                            }
                            (None, Some(turn)) => {
                                self.turn_plan.insert(turn, at);
                            }
                            (None, None) => {}
                        }
                        at
                    }
                };
                self.touch(at);
            }
            AgentEventKind::Usage {
                total,
                last,
                context_window,
            } => match (total, &mut self.usage) {
                (Some(total), _) => {
                    let context = total
                        .total_tokens
                        .map(|cumulative| last.and_then(|l| l.total_tokens).unwrap_or(cumulative));
                    self.usage = Some((*total, *context_window, context));
                }
                (None, Some((_, window, _))) => *window = context_window.or(*window),
                (None, None) => {}
            },
            AgentEventKind::TurnCompleted { duration_ms } => {
                self.turn_ended(turn, TurnOutcome::Completed, *duration_ms, event.ts_ms);
            }
            AgentEventKind::TurnFailed { message, details } => {
                self.turn_ended(
                    turn,
                    TurnOutcome::Failed {
                        message: message.clone(),
                        details: details.clone(),
                    },
                    None,
                    event.ts_ms,
                );
            }
            AgentEventKind::TurnCancelled => {
                self.turn_ended(turn, TurnOutcome::Cancelled, None, event.ts_ms);
            }
            AgentEventKind::Notice { level, message } => {
                let at = self.push(
                    turn,
                    event.ts_ms,
                    Body::Notice {
                        level: *level,
                        message: message.clone(),
                    },
                );
                self.touch(at);
            }
            AgentEventKind::ResumeLocator { .. } | AgentEventKind::Opaque { .. } => {}
        }
        true
    }

    fn push(&mut self, turn: Option<TurnId>, ts_ms: u64, body: Body) -> usize {
        self.next_id += 1;
        self.entries.push(Entry {
            id: self.next_id,
            rev: 0,
            ts_ms,
            turn,
            body,
        });
        self.entries.len() - 1
    }

    fn touch(&mut self, at: usize) {
        self.rev += 1;
        if let Some(entry) = self.entries.get_mut(at) {
            entry.rev = self.rev;
        }
    }

    fn message_mut(&mut self, at: usize) -> Option<&mut Message> {
        match &mut self.entries[at].body {
            Body::User(message) | Body::Assistant(message) => Some(message),
            _ => None,
        }
    }

    fn message_entry(
        &mut self,
        role: hutan_core::agent::MessageRole,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        ts_ms: u64,
    ) -> usize {
        if let Some(at) = item.and_then(|i| self.by_item.get(&i).copied()) {
            return at;
        }
        let message = Message {
            item,
            text: String::new(),
            phase: None,
            done: false,
            stream: Stream::default(),
        };
        let body = match role {
            hutan_core::agent::MessageRole::User => Body::User(message),
            hutan_core::agent::MessageRole::Assistant => Body::Assistant(message),
        };
        let at = self.push(turn, ts_ms, body);
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn reasoning_entry(&mut self, item: Option<ItemId>, turn: Option<TurnId>, ts_ms: u64) -> usize {
        if let Some(at) = item.and_then(|i| self.by_item.get(&i).copied()) {
            return at;
        }
        let at = self.push(
            turn,
            ts_ms,
            Body::Reasoning(Reasoning {
                item,
                summary: String::new(),
                content: String::new(),
                done: false,
                stream: Stream::default(),
            }),
        );
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn tool_entry(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        kind: ToolKind,
        ts_ms: u64,
    ) -> usize {
        if let Some(at) = self.known_tool(item) {
            return at;
        }
        let at = self.push(turn, ts_ms, Body::Tool(Tool::new(item, kind)));
        if let Some(item) = item {
            self.by_item.insert(item, at);
        }
        at
    }

    fn known_tool(&self, item: Option<ItemId>) -> Option<usize> {
        let at = item.and_then(|i| self.by_item.get(&i).copied())?;
        matches!(self.entries[at].body, Body::Tool(_)).then_some(at)
    }

    fn file_diff(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        changes: &[FileChange],
        patch: Option<&str>,
        ts_ms: u64,
    ) {
        if !changes.is_empty() {
            let at = self.tool_entry(item, turn, ToolKind::FileChange, ts_ms);
            if let Body::Tool(card) = &mut self.entries[at].body {
                if card.name.is_empty() {
                    "apply_patch".clone_into(&mut card.name);
                    card.title = match changes {
                        [one] => format!("edit {}", one.path),
                        [first, rest @ ..] => {
                            format!("edit {} and {} more", first.path, rest.len())
                        }
                        [] => String::new(),
                    };
                }
                for change in changes {
                    match card.changes.iter_mut().find(|c| c.path == change.path) {
                        Some(existing) => *existing = change.clone(),
                        None => card.changes.push(change.clone()),
                    }
                }
            }
            self.touch(at);
        }
        if let (Some(patch), Some(turn)) = (patch, turn)
            && !patch.is_empty()
        {
            match self.turn_diff.get(&turn).copied() {
                Some(at) => {
                    if !self.backfill {
                        self.entries[at].body = Body::TurnDiff(TurnDiff {
                            patch: patch.to_owned(),
                        });
                    }
                    self.touch(at);
                }
                None => {
                    let at = self.push(
                        Some(turn),
                        ts_ms,
                        Body::TurnDiff(TurnDiff {
                            patch: patch.to_owned(),
                        }),
                    );
                    self.turn_diff.insert(turn, at);
                    self.touch(at);
                }
            }
        }
    }

    fn permission_requested(
        &mut self,
        item: Option<ItemId>,
        turn: Option<TurnId>,
        approval: Approval,
        ts_ms: u64,
    ) {
        let id = approval.id;
        if self.approvals.contains_key(&id) {
            return;
        }
        let mut approval = approval;
        // Its answer may have been read first (the request is in an older page).
        if let Some((state, decided_by)) = self.early_answers.remove(&id) {
            approval.state = state;
            approval.decided_by = decided_by;
        }
        self.approvals.insert(id, approval);
        let at = match self.known_tool(item) {
            Some(at) => {
                if let Body::Tool(card) = &mut self.entries[at].body {
                    card.approvals.push(id);
                }
                at
            }
            None => self.push(turn, ts_ms, Body::Approval(id)),
        };
        self.approval_entry.insert(id, at);
        self.touch(at);
    }

    fn question_requested(&mut self, turn: Option<TurnId>, question: Question, ts_ms: u64) {
        let id = question.id;
        if self.questions.contains_key(&id) {
            return;
        }
        let mut question = question;
        // Its answer may have been read first (the request is in an older page).
        if let Some((state, decided_by)) = self.early_decisions.remove(&id) {
            question.state = state;
            question.decided_by = decided_by;
        }
        self.questions.insert(id, question);
        let at = self.push(turn, ts_ms, Body::Question(id));
        self.question_entry.insert(id, at);
        self.touch(at);
    }

    fn resolve_question(
        &mut self,
        id: QuestionId,
        state: QuestionState,
        decided_by: Option<DecidedBy>,
    ) {
        match self.questions.get_mut(&id) {
            Some(question) => {
                question.state = state;
                if decided_by.is_some() {
                    question.decided_by = decided_by;
                }
            }
            None => {
                self.early_decisions.insert(id, (state, decided_by));
            }
        }
        if let Some(at) = self.question_entry.get(&id).copied() {
            self.touch(at);
        }
    }

    fn set_approval_state(&mut self, id: ApprovalId, state: ApprovalState) {
        self.resolve_approval(id, state, None);
    }

    fn resolve_approval(
        &mut self,
        id: ApprovalId,
        state: ApprovalState,
        decided_by: Option<DecidedBy>,
    ) {
        match self.approvals.get_mut(&id) {
            Some(approval) => {
                approval.state = state;
                if decided_by.is_some() {
                    approval.decided_by = decided_by;
                }
            }
            None => {
                self.early_answers.insert(id, (state, decided_by));
            }
        }
        if let Some(at) = self.approval_entry.get(&id).copied() {
            self.touch(at);
        }
    }

    fn turn_ended(
        &mut self,
        turn: Option<TurnId>,
        outcome: TurnOutcome,
        duration_ms: Option<u64>,
        ts_ms: u64,
    ) {
        // Whatever was still open when the turn ended is over.
        let from = self.turn_start.min(self.entries.len());
        for at in from..self.entries.len() {
            let changed = match &mut self.entries[at].body {
                Body::Assistant(m) | Body::User(m) if !m.done => {
                    m.done = true;
                    true
                }
                Body::Reasoning(r) if !r.done => {
                    r.done = true;
                    true
                }
                Body::Tool(t) if t.status.is_none() && !t.interrupted => {
                    t.interrupted = true;
                    true
                }
                _ => false,
            };
            if changed {
                self.touch(at);
            }
        }
        self.active_turn = false;
        let usage = self.turn_usage();
        let at = self.push(
            turn,
            ts_ms,
            Body::Footer(Footer {
                outcome,
                duration_ms,
                usage,
            }),
        );
        self.touch(at);
    }

    fn turn_usage(&self) -> Option<TurnUsage> {
        let (now, window, _) = self.usage?;
        if self.usage_baseline_unknown {
            return Some(TurnUsage {
                conversation_tokens: now.total_tokens,
                context_window: window,
                ..TurnUsage::default()
            });
        }
        let before = self.usage_at_turn_start;
        let delta = |now: Option<u64>, before: Option<u64>| {
            now.map(|n| n.saturating_sub(before.unwrap_or(0)))
        };
        Some(TurnUsage {
            input_tokens: delta(now.input_tokens, before.input_tokens),
            cached_input_tokens: delta(now.cached_input_tokens, before.cached_input_tokens),
            output_tokens: delta(now.output_tokens, before.output_tokens),
            reasoning_output_tokens: delta(
                now.reasoning_output_tokens,
                before.reasoning_output_tokens,
            ),
            total_tokens: delta(now.total_tokens, before.total_tokens),
            conversation_tokens: now.total_tokens,
            context_window: window,
        })
    }
}

/// Cuts `text` to its last `max` bytes (at a character boundary).
fn cut_front(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut at = text.len() - max;
    while !text.is_char_boundary(at) {
        at += 1;
    }
    text.drain(..at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_core::SessionId;
    use hutan_core::agent::{
        Delivery, FileChangeKind, MessageRole, PermissionOptionKind, ProviderIds, ReasoningPart,
    };

    struct Script {
        session: SessionId,
        turn: TurnId,
        seq: u64,
    }

    impl Script {
        fn new() -> Self {
            Self {
                session: SessionId::new(),
                turn: TurnId::new(),
                seq: 0,
            }
        }

        fn event(&mut self, item: Option<ItemId>, kind: AgentEventKind) -> AgentEvent {
            self.seq += 1;
            AgentEvent {
                session_id: self.session,
                seq: self.seq,
                ts_ms: self.seq * 10,
                delivery: Delivery::Live,
                turn_id: Some(self.turn),
                item_id: item,
                provider: ProviderIds::default(),
                kind,
                payload: None,
            }
        }
    }

    fn assistant(t: &Transcript, at: usize) -> &Message {
        match &t.entries()[at].body {
            Body::Assistant(m) => m,
            other => panic!("{other:?}"),
        }
    }

    fn tool(t: &Transcript, at: usize) -> &Tool {
        match &t.entries()[at].body {
            Body::Tool(tool) => tool,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_streamed_message_grows_and_ends_with_the_complete_text() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        assert!(t.turn_active());
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: Some("final_answer".into()),
            },
        ));
        for piece in ["po", "ng"] {
            t.apply(&s.event(
                Some(item),
                AgentEventKind::MessageDelta { text: piece.into() },
            ));
        }
        assert_eq!(assistant(&t, 0).text, "pong");
        assert!(!assistant(&t, 0).done);
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                phase: None,
                text: "pong!".into(),
            },
        ));
        assert_eq!(assistant(&t, 0).text, "pong!");
        assert!(assistant(&t, 0).done);
        // A late delta (replayed after the end) does not grow it.
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "x".into() },
        ));
        assert_eq!(assistant(&t, 0).text, "pong!");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn an_event_applies_once() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let start = s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::User,
                phase: None,
            },
        );
        let delta = s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "a".into() },
        );
        assert!(t.apply(&start));
        assert!(t.apply(&delta));
        assert!(!t.apply(&start));
        assert!(!t.apply(&delta));
        assert_eq!(t.len(), 1);
        assert_eq!(t.max_seq(), 2);
    }

    #[test]
    fn a_command_card_collects_live_output_and_ends_with_the_whole() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "ls".into(),
                input: Some(serde_json::json!({"command": "ls", "cwd": "/w"})),
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput { text: "a\n".into() },
        ));
        assert!(tool(&t, 0).running());
        assert_eq!(tool(&t, 0).output, "a\n");
        assert_eq!(tool(&t, 0).cwd(), Some("/w"));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status: ToolStatus::Completed,
                output: Some("a\nb\n".into()),
                truncated: false,
                exit_code: Some(0),
                duration_ms: Some(12),
            },
        ));
        let card = tool(&t, 0);
        assert!(!card.running());
        assert_eq!(card.output, "a\nb\n");
        assert_eq!(card.exit_code, Some(0));
        // Output after the end is ignored.
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput {
                text: "late".into(),
            },
        ));
        assert_eq!(tool(&t, 0).output, "a\nb\n");
    }

    #[test]
    fn a_tool_end_without_output_keeps_the_live_output() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "make".into(),
                input: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::CommandOutput {
                text: "built".into(),
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status: ToolStatus::Failed,
                output: None,
                truncated: false,
                exit_code: Some(2),
                duration_ms: None,
            },
        ));
        assert_eq!(tool(&t, 0).output, "built");
        assert_eq!(tool(&t, 0).status, Some(ToolStatus::Failed));
    }

    #[test]
    fn live_output_is_bounded_to_its_tail() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "yes".into(),
                input: None,
            },
        ));
        let chunk = "é".repeat(100_000);
        for _ in 0..4 {
            t.apply(&s.event(
                Some(item),
                AgentEventKind::CommandOutput {
                    text: chunk.clone(),
                },
            ));
        }
        let output = &tool(&t, 0).output;
        assert!(output.len() <= MAX_LIVE_OUTPUT);
        assert!(output.chars().all(|c| c == 'é'));
    }

    #[test]
    fn file_changes_merge_by_path_and_the_turn_diff_has_its_own_entry() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let change = |patch: &str| FileChange {
            path: "/w/a.txt".into(),
            kind: FileChangeKind::Add,
            move_path: None,
            patch: Some(patch.into()),
            before: None,
            after: None,
        };
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::FileChange,
                name: "apply_patch".into(),
                title: "edit /w/a.txt".into(),
                input: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::FileDiff {
                changes: vec![change("one\n")],
                patch: None,
            },
        ));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::FileDiff {
                changes: vec![change("one\ntwo\n")],
                patch: None,
            },
        ));
        assert_eq!(tool(&t, 0).changes.len(), 1);
        assert_eq!(tool(&t, 0).changes[0].patch.as_deref(), Some("one\ntwo\n"));
        t.apply(&s.event(
            None,
            AgentEventKind::FileDiff {
                changes: Vec::new(),
                patch: Some("diff --git a/a b/a\n".into()),
            },
        ));
        t.apply(&s.event(
            None,
            AgentEventKind::FileDiff {
                changes: Vec::new(),
                patch: Some("diff --git a/a b/a\n+x\n".into()),
            },
        ));
        assert_eq!(t.len(), 2, "one turn diff entry, updated in place");
        assert!(matches!(&t.entries()[1].body, Body::TurnDiff(d) if d.patch.ends_with("+x\n")));
    }

    fn request(s: &mut Script, item: Option<ItemId>, id: ApprovalId) -> AgentEvent {
        s.event(
            item,
            AgentEventKind::PermissionRequested {
                approval_id: id,
                kind: PermissionKind::Command,
                title: "touch hi.txt".into(),
                detail: None,
                options: vec![
                    PermissionOption {
                        id: "accept".into(),
                        label: "Allow once".into(),
                        kind: PermissionOptionKind::AllowOnce,
                        provider: None,
                    },
                    PermissionOption {
                        id: "cancel".into(),
                        label: "Cancel".into(),
                        kind: PermissionOptionKind::Cancel,
                        provider: None,
                    },
                ],
            },
        )
    }

    #[test]
    fn an_approval_belongs_to_its_tool_and_resolves_in_place() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let id = ApprovalId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "touch hi.txt".into(),
                input: None,
            },
        ));
        t.apply(&request(&mut s, Some(item), id));
        assert_eq!(t.len(), 1, "the card is part of the tool entry");
        assert_eq!(tool(&t, 0).approvals, [id]);
        assert_eq!(t.pending_approvals().len(), 1);
        let before = t.entries()[0].rev;
        t.apply(&s.event(
            None,
            AgentEventKind::PermissionResolved {
                approval_id: id,
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        ));
        assert!(t.pending_approvals().is_empty());
        let approval = t.approval(id).unwrap();
        assert_eq!(approval.chosen().unwrap().label, "Allow once");
        assert_eq!(approval.decision_text(), "Allowed once");
        assert_eq!(approval.decided_by.as_ref().unwrap().client_kind, "cli");
        assert_eq!(approval.answered_from(false), "from CLI");
        assert_eq!(approval.answered_from(true), "from this window");
        assert!(
            t.entries()[0].rev > before,
            "the tool's card is drawn again"
        );
    }

    #[test]
    fn an_approval_without_a_tool_is_its_own_entry() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = ApprovalId::new();
        t.apply(&request(&mut s, Some(ItemId::new()), id));
        assert!(matches!(t.entries()[0].body, Body::Approval(a) if a == id));
        // A repeated request changes nothing.
        let again = request(&mut s, Some(ItemId::new()), id);
        t.apply(&again);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn the_end_of_a_turn_expires_nothing_by_itself_but_closes_open_items() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let (msg, cmd) = (ItemId::new(), ItemId::new());
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(
            Some(msg),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        ));
        t.apply(&s.event(
            Some(cmd),
            AgentEventKind::ToolStart {
                tool: ToolKind::Command,
                name: "shell".into(),
                title: "sleep 9".into(),
                input: None,
            },
        ));
        t.apply(&s.event(None, AgentEventKind::TurnCancelled));
        assert!(!t.turn_active());
        assert!(assistant(&t, 0).done);
        assert!(tool(&t, 1).interrupted);
        assert!(!tool(&t, 1).running());
        assert!(matches!(
            &t.entries()[2].body,
            Body::Footer(f) if f.outcome == TurnOutcome::Cancelled
        ));
    }

    #[test]
    fn the_footer_says_what_the_turn_used() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let usage = |total: u64, input: u64| AgentEventKind::Usage {
            total: Some(TokenCounts {
                total_tokens: Some(total),
                input_tokens: Some(input),
                ..TokenCounts::default()
            }),
            last: None,
            context_window: Some(128_000),
        };
        for (total, input) in [(100, 90), (250, 220)] {
            t.apply(&s.event(None, AgentEventKind::TurnStarted));
            t.apply(&s.event(None, usage(total, input)));
            t.apply(&s.event(
                None,
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(1500),
                },
            ));
        }
        let footers: Vec<&Footer> = t
            .entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::Footer(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(footers.len(), 2);
        let second = footers[1].usage.unwrap();
        assert_eq!(second.total_tokens, Some(150));
        assert_eq!(second.input_tokens, Some(130));
        assert_eq!(second.output_tokens, None, "never zero-filled");
        assert_eq!(second.conversation_tokens, Some(250));
        assert_eq!(second.context_window, Some(128_000));
        assert_eq!(footers[1].duration_ms, Some(1500));
    }

    #[test]
    fn the_context_use_is_the_latest_reported_total_and_absent_without_one() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        assert_eq!(t.context_use(), None, "nothing reported, no meter");
        let usage = |total: Option<u64>, window: Option<u64>| AgentEventKind::Usage {
            total: total.map(|total| TokenCounts {
                total_tokens: Some(total),
                ..TokenCounts::default()
            }),
            last: None,
            context_window: window,
        };
        // A per-turn report alone is no context total.
        t.apply(&s.event(None, usage(None, Some(200_000))));
        assert_eq!(t.context_use(), None);
        t.apply(&s.event(None, usage(Some(122_000), Some(200_000))));
        let seen = t.context_use().unwrap();
        assert_eq!(
            (seen.used, seen.window, seen.percent()),
            (122_000, Some(200_000), Some(61))
        );
        // The newest total wins; a harness that reports no window leaves none (never zero).
        t.apply(&s.event(None, usage(Some(130_000), Some(0))));
        let seen = t.context_use().unwrap();
        assert_eq!(
            (seen.used, seen.window, seen.percent()),
            (130_000, None, None)
        );
    }

    #[test]
    fn the_context_is_the_latest_request_when_the_total_is_cumulative() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let usage = |total: u64, last: u64| AgentEventKind::Usage {
            total: Some(TokenCounts {
                total_tokens: Some(total),
                ..TokenCounts::default()
            }),
            last: Some(TokenCounts {
                total_tokens: Some(last),
                ..TokenCounts::default()
            }),
            context_window: Some(200_000),
        };
        t.apply(&s.event(None, usage(50_000, 50_000)));
        t.apply(&s.event(None, usage(110_000, 60_000)));
        let seen = t.context_use().unwrap();
        assert_eq!(
            (seen.used, seen.percent()),
            (60_000, Some(30)),
            "the history is resent: the cumulative 110k is not what fills the window"
        );
        assert_eq!(
            seen.counts.total_tokens,
            Some(110_000),
            "the tooltip keeps the cumulative counts"
        );
        // A `last` that carries no total (an ACP prompt's own counts) falls back to the total.
        let no_total_in_last = AgentEventKind::Usage {
            total: Some(TokenCounts {
                total_tokens: Some(70_000),
                ..TokenCounts::default()
            }),
            last: Some(TokenCounts::default()),
            context_window: Some(200_000),
        };
        t.apply(&s.event(None, no_total_in_last));
        assert_eq!(t.context_use().unwrap().used, 70_000);
    }

    /// The recorded Codex 0.162.1 turn of `approval-decline.jsonl`: its second request reports
    /// `total 41 290` (cumulative) and `last 20 672` on a 475 000 window. The meter says 4 %,
    /// as Codex's own status does, not the 8 % of the cumulative figure.
    #[test]
    fn the_recorded_codex_context_is_last_not_the_cumulative_total() {
        let fixture =
            include_str!("../../agents/tests/fixtures/codex-0.162/approval-decline.jsonl");
        let counts = |v: &serde_json::Value| TokenCounts {
            total_tokens: v["totalTokens"].as_u64(),
            input_tokens: v["inputTokens"].as_u64(),
            cached_input_tokens: v["cachedInputTokens"].as_u64(),
            cache_write_tokens: v["cacheWriteInputTokens"].as_u64(),
            output_tokens: v["outputTokens"].as_u64(),
            reasoning_output_tokens: v["reasoningOutputTokens"].as_u64(),
        };
        let mut s = Script::new();
        let mut t = Transcript::new();
        let mut seen = Vec::new();
        for line in fixture.lines() {
            let line: serde_json::Value = serde_json::from_str(line).unwrap();
            let msg = &line["msg"];
            if msg["method"] != "thread/tokenUsage/updated" {
                continue;
            }
            let usage = &msg["params"]["tokenUsage"];
            t.apply(&s.event(
                None,
                AgentEventKind::Usage {
                    total: Some(counts(&usage["total"])),
                    last: Some(counts(&usage["last"])),
                    context_window: usage["modelContextWindow"].as_u64(),
                },
            ));
            let context = t.context_use().unwrap();
            seen.push((context.used, context.window, context.percent()));
        }
        assert_eq!(
            seen,
            vec![
                (20_618, Some(475_000), Some(4)),
                (20_672, Some(475_000), Some(4)),
            ]
        );
    }

    #[test]
    fn a_turn_without_usage_has_no_usage_footer() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }));
        assert!(matches!(&t.entries()[0].body, Body::Footer(f) if f.usage.is_none()));
    }

    #[test]
    fn plans_update_in_place_per_turn() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let plan = |status| AgentEventKind::Plan {
            explanation: None,
            steps: vec![PlanStep {
                text: "do it".into(),
                status,
            }],
            text: None,
        };
        t.apply(&s.event(None, plan(hutan_core::agent::PlanStepStatus::Pending)));
        t.apply(&s.event(None, plan(hutan_core::agent::PlanStepStatus::Completed)));
        assert_eq!(t.len(), 1);
        assert!(matches!(
            &t.entries()[0].body,
            Body::Plan(p) if p.steps[0].status == hutan_core::agent::PlanStepStatus::Completed
        ));
    }

    fn ask(s: &mut Script, id: QuestionId) -> AgentEvent {
        s.event(
            None,
            AgentEventKind::QuestionRequested {
                question_id: id,
                spec: QuestionSpec {
                    title: "Which branch?".into(),
                    prompt: String::new(),
                    fields: Vec::new(),
                    raw_schema: None,
                    source: None,
                    expires: hutan_core::question::QuestionExpiry::Turn,
                },
            },
        )
    }

    fn settle(s: &mut Script, id: QuestionId, outcome: QuestionOutcome) -> AgentEvent {
        s.event(
            None,
            AgentEventKind::QuestionResolved {
                question_id: id,
                outcome,
                decided_by: Some(DecidedBy {
                    client_kind: "cli".into(),
                    client_name: None,
                }),
            },
        )
    }

    #[test]
    fn a_question_is_its_own_entry_and_resolves_in_place() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = QuestionId::new();
        t.apply(&ask(&mut s, id));
        assert!(matches!(t.entries()[0].body, Body::Question(q) if q == id));
        assert_eq!(t.pending_questions().len(), 1);
        let before = t.entries()[0].rev;
        t.apply(&settle(&mut s, id, QuestionOutcome::Declined));
        assert!(t.pending_questions().is_empty());
        let question = t.question(id).unwrap();
        assert_eq!(question.state, QuestionState::Declined);
        assert_eq!(question.decision_text(), "Declined");
        assert_eq!(question.answered_from(false), "from CLI");
        assert_eq!(question.answered_from(true), "from this window");
        assert!(t.entries()[0].rev > before, "the card is drawn again");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn an_answer_read_before_its_question_is_applied_to_it() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = QuestionId::new();
        let request = ask(&mut s, id);
        let answer = settle(
            &mut s,
            id,
            QuestionOutcome::Answered {
                values: hutan_core::question::Values::new(),
                withheld: vec!["token".into()],
            },
        );
        // The newer page first, then the older one with the request.
        t.apply(&answer);
        assert!(t.question(id).is_none());
        t.prepend(&[request], true);
        let question = t.question(id).unwrap();
        assert!(matches!(
            &question.state,
            QuestionState::Answered { withheld, .. } if withheld == &["token".to_owned()]
        ));
        assert!(t.pending_questions().is_empty());
    }

    #[test]
    fn expire_missing_questions_ends_what_the_daemon_no_longer_holds() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let (a, b) = (QuestionId::new(), QuestionId::new());
        t.apply(&ask(&mut s, a));
        t.apply(&ask(&mut s, b));
        t.expire_missing_questions(&HashSet::from([b]));
        assert_eq!(t.question(a).unwrap().state, QuestionState::Expired);
        assert_eq!(t.question(b).unwrap().state, QuestionState::Pending);
        assert_eq!(t.pending_questions().len(), 1);
    }

    #[test]
    fn reasoning_streams_then_takes_the_final_parts() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(Some(item), AgentEventKind::ReasoningStart));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Summary,
                text: "Check".into(),
            },
        ));
        assert!(
            matches!(&t.entries()[0].body, Body::Reasoning(r) if r.summary == "Check" && !r.done)
        );
        t.apply(&s.event(
            Some(item),
            AgentEventKind::ReasoningEnd {
                summary: vec!["Check the files".into()],
                content: Vec::new(),
            },
        ));
        assert!(matches!(
            &t.entries()[0].body,
            Body::Reasoning(r) if r.summary == "Check the files" && r.done
        ));
    }

    #[test]
    fn a_reload_replays_what_the_view_has_without_duplicating() {
        let mut s = Script::new();
        let item = ItemId::new();
        let events = vec![
            s.event(None, AgentEventKind::TurnStarted),
            s.event(
                Some(item),
                AgentEventKind::MessageStart {
                    role: MessageRole::Assistant,
                    phase: None,
                },
            ),
            s.event(
                Some(item),
                AgentEventKind::MessageDelta { text: "hel".into() },
            ),
            s.event(
                Some(item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: "hello".into(),
                },
            ),
            s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }),
        ];
        let mut t = Transcript::new();
        // Live: everything up to the delta.
        for e in &events[..3] {
            t.apply(e);
        }
        // The journal arrives (no deltas) and then the replayed live events.
        for e in events.iter().filter(|e| !e.kind.is_streaming()) {
            t.apply(e);
        }
        for e in &events {
            t.apply(e);
        }
        assert_eq!(t.len(), 2);
        assert_eq!(assistant(&t, 0).text, "hello");
        assert!(!t.turn_active());
    }

    #[test]
    fn expire_missing_ends_requests_the_daemon_no_longer_holds() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let (a, b) = (ApprovalId::new(), ApprovalId::new());
        t.apply(&request(&mut s, None, a));
        t.apply(&request(&mut s, None, b));
        t.expire_missing(&HashSet::from([b]));
        assert_eq!(t.approval(a).unwrap().state, ApprovalState::Expired);
        assert_eq!(t.approval(b).unwrap().state, ApprovalState::Pending);
    }

    #[test]
    fn reset_starts_a_new_list() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = ApprovalId::new();
        t.apply(&request(&mut s, None, id));
        let epoch = t.epoch();
        t.reset();
        assert!(t.is_empty());
        assert_ne!(t.epoch(), epoch);
        assert!(t.pending_approvals().is_empty());
        // Sequence numbers start over after a daemon restart: the same seq applies again.
        let fresh = s.event(None, AgentEventKind::TurnStarted);
        let mut again = fresh.clone();
        again.seq = 1;
        assert!(t.apply(&again));
    }

    #[test]
    fn every_change_bumps_the_entry_revision() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        ));
        let first = t.entries()[0].rev;
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "x".into() },
        ));
        assert!(t.entries()[0].rev > first);
        assert!(t.rev() >= t.entries()[0].rev);
    }

    // ---- snapshots, provenance, earlier pages ------------------------------------------------

    fn delta(s: &mut Script, item: ItemId, text: &str) -> AgentEvent {
        s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: text.into() },
        )
    }

    fn partial(s: &mut Script, item: ItemId, text: &str) -> AgentEvent {
        s.event(
            Some(item),
            AgentEventKind::MessagePartial {
                message_id: item,
                phase: Some("final_answer".into()),
                text: text.into(),
            },
        )
    }

    #[test]
    fn a_snapshot_then_live_deltas_end_with_exactly_the_final_text() {
        // A window opened mid-stream: the journal's snapshot, then what streams on.
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        s.seq = 9; // deltas 1..=9 went out before the window opened
        let snapshot = partial(&mut s, item, "Hello, ");
        assert_eq!(snapshot.seq, 10);
        t.apply(&snapshot);
        assert_eq!(assistant(&t, 0).text, "Hello, ");
        assert_eq!(assistant(&t, 0).phase.as_deref(), Some("final_answer"));
        assert!(!assistant(&t, 0).done);
        for piece in ["wor", "ld"] {
            let event = delta(&mut s, item, piece);
            t.apply(&event);
        }
        assert_eq!(assistant(&t, 0).text, "Hello, world");
        let end = s.event(
            Some(item),
            AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                phase: Some("final_answer".into()),
                text: "Hello, world".into(),
            },
        );
        t.apply(&end);
        assert_eq!(assistant(&t, 0).text, "Hello, world");
        assert!(assistant(&t, 0).done);
        assert_eq!(t.len(), 1, "one message, not one per snapshot");
    }

    #[test]
    fn live_deltas_that_came_before_the_snapshot_are_neither_lost_nor_doubled() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        // Deltas 1 and 2 ("Hel", "lo, ") went out before the window opened; the history reply
        // carries the snapshot taken after them (seq 3). Deltas 4 and 5 are live, and they
        // reach the window before the reply is applied (the message start was missed too).
        s.seq = 2;
        let snapshot = partial(&mut s, item, "Hello, ");
        let live_a = delta(&mut s, item, "wor");
        let live_b = delta(&mut s, item, "ld");
        assert_eq!((snapshot.seq, live_a.seq, live_b.seq), (3, 4, 5));
        t.apply(&live_a);
        t.apply(&live_b);
        assert_eq!(assistant(&t, 0).text, "world", "what there is so far");
        t.apply(&snapshot);
        assert_eq!(assistant(&t, 0).text, "Hello, world");
        // Deltas the snapshot has, delivered late, change nothing; later ones still append.
        let mut old = delta(&mut s, item, "lo, ");
        old.seq = 2;
        t.apply(&old);
        assert_eq!(assistant(&t, 0).text, "Hello, world");
        let more = delta(&mut s, item, "!");
        t.apply(&more);
        assert_eq!(assistant(&t, 0).text, "Hello, world!");
    }

    #[test]
    fn an_older_snapshot_never_shortens_the_text() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        let start = s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        );
        t.apply(&start);
        let first = delta(&mut s, item, "abc");
        let mut stale = partial(&mut s, item, "ab");
        stale.seq = 1; // covers nothing the transcript does not have
        let second = delta(&mut s, item, "def");
        t.apply(&first);
        t.apply(&second);
        t.apply(&stale);
        assert_eq!(assistant(&t, 0).text, "abcdef");
    }

    #[test]
    fn a_reasoning_snapshot_reconciles_summary_and_content() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let item = ItemId::new();
        s.seq = 3; // the start and two deltas went out before the window opened
        let snapshot = s.event(
            Some(item),
            AgentEventKind::ReasoningPartial {
                reasoning_id: item,
                summary: "Plan".into(),
                content: "details".into(),
            },
        );
        let live = s.event(
            Some(item),
            AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Summary,
                text: " more".into(),
            },
        );
        assert_eq!((snapshot.seq, live.seq), (4, 5));
        t.apply(&live);
        t.apply(&snapshot);
        let Body::Reasoning(reasoning) = &t.entries()[0].body else {
            panic!()
        };
        assert_eq!(
            (reasoning.summary.as_str(), reasoning.content.as_str()),
            ("Plan more", "details")
        );
        assert!(!reasoning.done);
        let end = s.event(
            Some(item),
            AgentEventKind::ReasoningEnd {
                summary: vec!["Plan more".into()],
                content: vec!["details".into()],
            },
        );
        t.apply(&end);
        let Body::Reasoning(reasoning) = &t.entries()[0].body else {
            panic!()
        };
        assert!(reasoning.done);
        assert_eq!(reasoning.summary, "Plan more");
    }

    #[test]
    fn who_answered_comes_with_the_resolution_even_when_it_was_read_first() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let id = ApprovalId::new();
        let ask = request(&mut s, None, id);
        let answer = s.event(
            None,
            AgentEventKind::PermissionResolved {
                approval_id: id,
                outcome: PermissionOutcome::Selected {
                    option_id: "accept".into(),
                },
                decided_by: Some(DecidedBy {
                    client_kind: "desktop".into(),
                    client_name: None,
                }),
            },
        );
        // The newer page, with the answer, is read before the page that has the request.
        t.apply(&answer);
        assert!(t.approval(id).is_none());
        t.prepend(&[ask], true);
        let approval = t.approval(id).unwrap();
        assert_eq!(
            approval.state,
            ApprovalState::Selected {
                option_id: "accept".into()
            }
        );
        assert_eq!(approval.decided_by.as_ref().unwrap().client_kind, "desktop");
        assert!(t.pending_approvals().is_empty());
    }

    /// A journal of two turns: a message, a command that ends in the second turn's page, and
    /// footers with usage.
    fn journal() -> Vec<AgentEvent> {
        let mut s = Script::new();
        let mut events = Vec::new();
        let usage = |s: &mut Script, total: u64| {
            s.event(
                None,
                AgentEventKind::Usage {
                    total: Some(TokenCounts {
                        total_tokens: Some(total),
                        ..TokenCounts::default()
                    }),
                    last: None,
                    context_window: Some(1000),
                },
            )
        };
        for (turn, total) in [(1_u64, 100_u64), (2, 250)] {
            s.turn = TurnId::new();
            events.push(s.event(None, AgentEventKind::TurnStarted));
            let user = ItemId::new();
            events.push(s.event(
                Some(user),
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    phase: None,
                    text: format!("question {turn}"),
                },
            ));
            let command = ItemId::new();
            events.push(s.event(
                Some(command),
                AgentEventKind::ToolStart {
                    tool: ToolKind::Command,
                    name: "shell".into(),
                    title: format!("run {turn}"),
                    input: None,
                },
            ));
            events.push(s.event(
                Some(command),
                AgentEventKind::ToolEnd {
                    tool: ToolKind::Command,
                    status: ToolStatus::Completed,
                    output: Some(format!("out {turn}")),
                    truncated: false,
                    exit_code: Some(0),
                    duration_ms: None,
                },
            ));
            let answer = ItemId::new();
            events.push(s.event(
                Some(answer),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: format!("answer {turn}"),
                },
            ));
            events.push(usage(&mut s, total));
            events.push(s.event(
                None,
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(turn),
                },
            ));
        }
        events
    }

    fn shape(t: &Transcript) -> Vec<String> {
        t.entries()
            .iter()
            .map(|e| match &e.body {
                Body::User(m) => format!("user {}", m.text),
                Body::Assistant(m) => format!("assistant {}", m.text),
                Body::Tool(tool) => format!("tool {} -> {}", tool.title, tool.output),
                Body::Footer(f) => format!("footer {:?} {:?}", f.duration_ms, f.usage),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn earlier_pages_go_in_front_without_duplicates_and_match_one_read_from_the_start() {
        let events = journal();
        let mut whole = Transcript::new();
        for event in &events {
            whole.apply(event);
        }

        // Newest page first; the page boundary cuts the second turn after its command started.
        let cut = events
            .iter()
            .position(|e| matches!(e.kind, AgentEventKind::ToolStart { .. }))
            .unwrap()
            + 8; // the second turn's tool_start
        let mut paged = Transcript::new();
        for event in &events[cut..] {
            paged.apply(event);
        }
        let newest_len = paged.len();
        let first_id = paged.entries()[0].id;
        // The older page overlaps the newer one by two events (a reload that was not exact).
        let added = paged.prepend(&events[..cut + 2], true);
        assert!(added > 0);
        assert_eq!(paged.len(), newest_len + added);
        assert_eq!(
            paged.entries().iter().filter(|e| e.id == first_id).count(),
            1,
            "entries are the same ones, moved"
        );
        assert_eq!(
            shape(&paged)
                .into_iter()
                .filter(|line| line.starts_with("user "))
                .collect::<Vec<_>>(),
            ["user question 1", "user question 2"]
        );
        // The command that spans the boundary is one card with its start and its end.
        let commands: Vec<String> = shape(&paged)
            .into_iter()
            .filter(|l| l.starts_with("tool "))
            .collect();
        assert_eq!(commands, ["tool run 1 -> out 1", "tool run 2 -> out 2"]);
        // Applying everything again changes nothing.
        for event in &events {
            assert!(!paged.apply(event));
        }
        assert!(
            shape(&paged)
                .iter()
                .filter(|l| l.starts_with("footer "))
                .count()
                == 2
        );
        assert!(!paged.turn_active());
        assert_eq!(paged.max_seq(), whole.max_seq());
        // The footers of both turns carry their own usage, as in the whole read.
        let footers = |t: &Transcript| -> Vec<Option<u64>> {
            t.entries()
                .iter()
                .filter_map(|e| match &e.body {
                    Body::Footer(f) => Some(f.usage.and_then(|u| u.total_tokens)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(footers(&whole), [Some(100), Some(150)]);
        assert_eq!(footers(&paged)[0], Some(100));
    }

    #[test]
    fn prepending_leaves_the_turn_in_progress_alone() {
        let mut s = Script::new();
        let older = vec![
            s.event(None, AgentEventKind::TurnStarted),
            s.event(None, AgentEventKind::TurnCancelled),
        ];
        let mut t = Transcript::new();
        s.turn = TurnId::new();
        let item = ItemId::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(
            Some(item),
            AgentEventKind::MessageStart {
                role: MessageRole::Assistant,
                phase: None,
            },
        ));
        assert!(t.turn_active());
        t.prepend(&older, true);
        assert!(t.turn_active(), "the old turn's end is not this turn's");
        // The open message was not closed by the old turn's end, and later deltas still grow it.
        let more = s.event(
            Some(item),
            AgentEventKind::MessageDelta { text: "x".into() },
        );
        t.apply(&more);
        let last = t.entries().last().unwrap();
        assert!(matches!(&last.body, Body::Assistant(m) if m.text == "x" && !m.done));
    }

    #[test]
    fn a_page_cut_in_the_middle_of_a_turn_reports_no_made_up_usage() {
        let events = journal();
        // Only the second turn's end and what follows the cut: the usage before it is unknown.
        let second = events
            .iter()
            .rposition(|e| matches!(e.kind, AgentEventKind::TurnStarted))
            .unwrap();
        let mut t = Transcript::new();
        for event in &events[second..] {
            t.apply(event);
        }
        // An older page that holds the first turn (not the start of the journal).
        t.prepend(&events[..second], false);
        let totals: Vec<(Option<u64>, Option<u64>)> = t
            .entries()
            .iter()
            .filter_map(|e| match &e.body {
                Body::Footer(f) => f.usage.map(|u| (u.total_tokens, u.conversation_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(
            totals,
            [(None, Some(100)), (Some(250), Some(250))],
            "the older turn names the conversation total but no per-turn figure"
        );
    }

    #[test]
    fn a_measurement_turn_is_marked_so_a_view_can_leave_it_out() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        let say = |s: &mut Script, t: &mut Transcript, role: MessageRole, text: &str| {
            let item = Some(ItemId::new());
            t.apply(&s.event(
                item,
                AgentEventKind::MessageEnd {
                    role,
                    phase: None,
                    text: text.to_owned(),
                },
            ));
        };
        // An ordinary turn, then the measurement, then one more ordinary turn.
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        say(&mut s, &mut t, MessageRole::User, "hello");
        say(&mut s, &mut t, MessageRole::Assistant, "hi");
        t.apply(&s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }));
        s.turn = TurnId::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        say(&mut s, &mut t, MessageRole::User, "/context");
        say(
            &mut s,
            &mut t,
            MessageRole::Assistant,
            "Context window: 200000 tokens",
        );
        t.apply(&s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }));
        s.turn = TurnId::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        say(&mut s, &mut t, MessageRole::User, "and now?");
        t.apply(&s.event(None, AgentEventKind::TurnCompleted { duration_ms: None }));

        let marks: Vec<bool> = t.entries().iter().map(|e| t.is_measurement(e)).collect();
        // user, assistant, footer | /context, table, footer | user, footer
        assert_eq!(marks, [false, false, false, true, true, true, false, false]);
    }

    #[test]
    fn a_user_who_only_mentions_slash_context_is_not_measuring() {
        let mut s = Script::new();
        let mut t = Transcript::new();
        t.apply(&s.event(None, AgentEventKind::TurnStarted));
        t.apply(&s.event(
            Some(ItemId::new()),
            AgentEventKind::MessageEnd {
                role: MessageRole::User,
                phase: None,
                text: "what does /context do?".to_owned(),
            },
        ));
        assert!(t.entries().iter().all(|e| !t.is_measurement(e)));
    }
}
