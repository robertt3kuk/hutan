//! The words of the Agents surfaces (chrome v3 § 2.3, § 2.5, § 2.6): the time columns, the
//! status line, the reason with its `+N`, and what each action of a row may do. One place, so
//! the page, the panel and Mawas's Today say the same thing about the same record. GPUI-free
//! and unit tested.
//!
//! Nothing here invents a fact: a start time the harness did not send is `—` with the
//! explanation "Start time not reported", a model that is missing is "Model not reported" in
//! the details only, and a duration is never counted from the session's creation.

use hutan_client::time::local_time_seconds_text;

use super::agents::format_duration;
use super::presentation::{AgentRecord, Group, ReasonKind, State};

/// `12s ago`, `3m ago`, `2h ago`, `4d ago`.
pub fn age_text(now_ms: u64, then_ms: u64) -> String {
    let secs = now_ms.saturating_sub(then_ms) / 1000;
    match secs {
        0..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// The duration column: what is known about how long the agent runs, and the tooltip that
/// explains an absence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationCell {
    /// `Running 3m 05s`, `Ran 12m 40s`, or `—`.
    pub text: String,
    /// `Start time not reported` when the text is `—`; the absolute start otherwise.
    pub tooltip: String,
}

/// `Running 3m 05s` for a live agent with a known start, `Ran 12m` for a finished one with a
/// known start and end; `—` otherwise. Never the age of the session.
pub fn duration_cell(record: &AgentRecord, now_ms: u64) -> DurationCell {
    let absent = |tooltip: &str| DurationCell {
        text: "—".to_owned(),
        tooltip: tooltip.to_owned(),
    };
    let Some(start) = record.started_at_ms else {
        return absent("Start time not reported");
    };
    let started = format!("Started {}", local_time_seconds_text(start));
    if record.state.is_finished() {
        return match record.ended_at_ms {
            Some(end) => DurationCell {
                text: format!("Ran {}", format_duration(end.saturating_sub(start))),
                tooltip: started,
            },
            None => absent("End time not reported"),
        };
    }
    if !record.live {
        return absent("The session is not running");
    }
    DurationCell {
        text: format!("Running {}", format_duration(now_ms.saturating_sub(start))),
        tooltip: started,
    }
}

/// The activity-age column: `Updated 12s ago` (the last semantic activity), `Last reported 3h
/// ago` for an agent whose state is unavailable, `Finished 2m ago` for one that finished; an
/// empty text when nothing is known.
pub fn updated_text(record: &AgentRecord, now_ms: u64) -> String {
    if record.state == State::Unconfirmed {
        return match record.reported_at_ms {
            Some(at) => format!("Last reported {}", age_text(now_ms, at)),
            None => "Never reported".to_owned(),
        };
    }
    match (
        record.state.is_finished(),
        record.ended_at_ms.or(record.updated_at_ms),
    ) {
        (true, Some(at)) => format!("Finished {}", age_text(now_ms, at)),
        (false, Some(at)) => format!("Updated {}", age_text(now_ms, at)),
        _ => String::new(),
    }
}

/// The absolute time behind [`updated_text`], for a tooltip.
pub fn updated_tooltip(record: &AgentRecord) -> Option<String> {
    let at = if record.state == State::Unconfirmed {
        record.reported_at_ms
    } else {
        record.ended_at_ms.or(record.updated_at_ms)
    }?;
    Some(local_time_seconds_text(at))
}

/// The reason line of a *Needs you* row: the highest reason, `+2` for the others, and how many
/// requests wait when there is more than one.
pub fn reason_text(record: &AgentRecord) -> Option<String> {
    let headline = record.headline()?;
    let mut text = headline.text.clone();
    if record.reasons.len() > 1 {
        text.push_str(&format!(" +{}", record.reasons.len() - 1));
    }
    let requests = record.requests.len() + record.submitting;
    if requests > 1 {
        text.push_str(&format!(" · {requests} requests"));
    }
    if record.group == Group::Snoozed {
        text.insert_str(0, "Snoozed · ");
    }
    Some(text)
}

/// The reason's own words, without the kind the state word already says (`Permission: ` for an
/// approval, `Question: ` for a question): `edit: src/lib.rs +1 · 2 requests`. The line a
/// *Needs you* row shows under its state word `Needs you · approval`.
pub fn reason_detail(record: &AgentRecord) -> Option<String> {
    let headline = record.headline()?;
    let own = match headline.kind {
        ReasonKind::Approval => headline.text.strip_prefix("Permission: "),
        ReasonKind::Question => headline.text.strip_prefix("Question: "),
        _ => None,
    };
    let Some(own) = own else {
        return reason_text(record);
    };
    let mut text = own.to_owned();
    if record.reasons.len() > 1 {
        text.push_str(&format!(" +{}", record.reasons.len() - 1));
    }
    let requests = record.requests.len() + record.submitting;
    if requests > 1 {
        text.push_str(&format!(" · {requests} requests"));
    }
    if record.group == Group::Snoozed {
        text.insert_str(0, "Snoozed · ");
    }
    Some(text)
}

/// The state word of a row, one per record: attention wins over the working state the agent
/// is in (reducer precedence, chrome v3 § 2.2), so a session that waits for a decision says
/// `Needs you · approval` and never also `Working`; work waiting for review the user has seen
/// says `Ready for review`; every other record says its state.
pub fn state_word(record: &AgentRecord) -> String {
    if !record.needs_you() {
        if record.ready_for_review {
            return "Ready for review".to_owned();
        }
        return record.state.label().to_owned();
    }
    let kind = match record.headline().map(|reason| reason.kind) {
        Some(ReasonKind::Approval) => "approval",
        Some(ReasonKind::Question) => "question",
        Some(ReasonKind::Blocked) => "blocked",
        Some(ReasonKind::Failed) => "failed",
        Some(ReasonKind::Review) => "review",
        Some(ReasonKind::CompletedUnread) => "completed",
        Some(ReasonKind::MarkedUnread) => "unread",
        None => return "Needs you".to_owned(),
    };
    format!("Needs you · {kind}")
}

/// The leading glyph of a row and the tone of the state word: attention first (a triangle for a
/// decision, a cross for a failure), else the state's own shape. Never colour alone.
pub fn lead(record: &AgentRecord) -> (crate::icons::Icon, super::Tone) {
    use super::Tone;
    use crate::icons::Icon;
    match record.headline().map(|reason| reason.kind) {
        Some(ReasonKind::Approval | ReasonKind::Question | ReasonKind::Blocked) => {
            (Icon::StateBlocked, Tone::Warning)
        }
        Some(ReasonKind::Failed) => (Icon::StateFailed, Tone::Error),
        _ => (record.state.glyph(), record.state.tone()),
    }
}

/// What a row's reason says on hover, `UX-ATTENTION-CONTROLS`: every reason, then who said so
/// and when — `Permission: run cargo test · reported by claude-hooks · 2m ago`. `None` when the
/// session has no reason.
pub fn reason_tooltip(record: &AgentRecord, now_ms: u64) -> Option<String> {
    if record.reasons.is_empty() {
        return None;
    }
    let source = if let Some(source) = &record.report_source {
        Some(format!("reported by {source}"))
    } else if record.managed {
        Some("known to the daemon, which runs this chat".to_owned())
    } else if record.provenance.iter().any(|p| p.contains("observation")) {
        Some("seen in the observation tree".to_owned())
    } else {
        None
    };
    let lines: Vec<String> = record
        .reasons
        .iter()
        .map(|reason| {
            let mut parts = vec![reason.text.clone()];
            parts.extend(source.clone());
            if let Some(at) = reason.since_ms.or(record.reported_at_ms) {
                parts.push(age_text(now_ms, at));
            }
            parts.join(" · ")
        })
        .collect();
    let mut text = lines.join("\n");
    if record.group == Group::Snoozed {
        text.push_str("\nSnoozed until this changes");
    }
    Some(text)
}

/// The second line of a row: the reason when the session needs the user, else the current
/// activity, else a meaningful word about the state (never an empty subtitle).
pub fn status_line(record: &AgentRecord) -> String {
    if let Some(reason) = reason_text(record) {
        return reason;
    }
    if let Some(activity) = &record.activity {
        return activity.clone();
    }
    match record.state {
        State::Unconfirmed => format!("State unavailable · {}", record.detail),
        State::Working => "Working".to_owned(),
        State::Idle => "Idle · waiting for your next message".to_owned(),
        State::Blocked => "Blocked".to_owned(),
        _ if record.detail.is_empty() => record.state.label().to_owned(),
        _ => format!("{} · {}", record.state.label(), record.detail),
    }
}

/// `3 subagents · 1 blocked`, `2 finished subagents`; `None` without any.
pub fn children_text(record: &AgentRecord) -> Option<String> {
    if record.subagents_live == 0 && record.subagents_finished == 0 {
        return None;
    }
    if record.subagents_live == 0 {
        return Some(format!(
            "{} finished subagent{}",
            record.subagents_finished,
            if record.subagents_finished == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    let mut text = format!(
        "{} subagent{}",
        record.subagents_live,
        if record.subagents_live == 1 { "" } else { "s" }
    );
    if record.subagents_blocked > 0 {
        text.push_str(&format!(" · {} blocked", record.subagents_blocked));
    }
    Some(text)
}

/// The harness and model column: `OMP · gpt-6`, or the harness alone (the model is "not
/// reported" in the details, never guessed).
pub fn harness_text(record: &AgentRecord) -> String {
    match (&record.model, record.harness.is_empty()) {
        (Some(model), false) => format!("{} · {model}", record.harness),
        (Some(model), true) => model.clone(),
        (None, false) => record.harness.clone(),
        (None, true) => "Agent".to_owned(),
    }
}

/// The accessible name of a row: title, state, harness, workspace and the reason.
pub fn accessible_name(record: &AgentRecord) -> String {
    let mut parts = vec![
        record.title.clone(),
        record.state.label().to_owned(),
        record.harness.clone(),
        record.location(),
    ];
    if let Some(reason) = reason_text(record) {
        parts.push(reason);
    }
    if record.role == super::presentation::Role::Assistant {
        parts.insert(1, "Assistant".to_owned());
    }
    parts.retain(|p| !p.is_empty());
    parts.join(", ")
}

/// What a row's main button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primary {
    /// Expands the exact request with its option buttons (never approves by itself).
    ReviewRequest,
    /// Opens the session, where the question card is.
    AnswerQuestion,
    /// Opens the checkout's diff.
    ReviewChanges,
    OpenSession,
    OpenMawas,
}

impl Primary {
    pub const fn label(self) -> &'static str {
        match self {
            Primary::ReviewRequest => "Review request",
            Primary::AnswerQuestion => "Answer question",
            Primary::ReviewChanges => "Show changes",
            Primary::OpenSession => "Open session",
            Primary::OpenMawas => "Open Mawas",
        }
    }
}

/// The main button of `record`: the one decision a *Needs you* row waits for, else open.
pub fn primary(record: &AgentRecord) -> Primary {
    if !record.requests.is_empty() {
        Primary::ReviewRequest
    } else if !record.questions.is_empty() {
        Primary::AnswerQuestion
    } else if record
        .headline()
        .is_some_and(|r| r.kind == ReasonKind::Review)
        && record.checkout.is_some()
    {
        Primary::ReviewChanges
    } else if record.role == super::presentation::Role::Assistant {
        Primary::OpenMawas
    } else {
        Primary::OpenSession
    }
}

/// What the connection allows right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The daemon is connected: commands that need its authority work.
    pub online: bool,
    /// The daemon can interrupt a chat turn (`chat.codex`).
    pub chat: bool,
}

/// Whether an action is available, and when not, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Avail {
    Yes,
    No(&'static str),
}

impl Avail {
    pub fn is_yes(&self) -> bool {
        *self == Avail::Yes
    }
}

/// *Interrupt turn*: only a turn the daemon runs, and only while connected. An agent that runs
/// in a terminal has no interrupt Hutan could send; the reason says so.
pub fn interrupt(record: &AgentRecord, caps: Caps) -> Avail {
    if !caps.online {
        Avail::No("Offline: Hutan cannot reach the daemon")
    } else if !record.managed {
        Avail::No("This agent runs in a terminal: Hutan has no way to interrupt its turn")
    } else if !caps.chat {
        Avail::No("This daemon cannot interrupt a chat turn")
    } else if !record.turn_active {
        Avail::No("No turn is in progress")
    } else {
        Avail::Yes
    }
}

/// *Stop session…*: while the session lives and the daemon is there.
pub fn stop(record: &AgentRecord, caps: Caps) -> Avail {
    if !caps.online {
        Avail::No("Offline: Hutan cannot reach the daemon")
    } else if !record.live {
        Avail::No("The session already ended")
    } else {
        Avail::Yes
    }
}

/// Stopping one subagent has no real target: the daemon stops sessions, and a node's own
/// interrupt exists only for harnesses that report a process. Never the parent in its place.
pub const STOP_CHILD_DISABLED: &str =
    "Hutan stops whole sessions: this harness has no way to stop one subagent";

/// *Show changes* / *Review changes*: needs the checkout.
pub fn changes(record: &AgentRecord) -> Avail {
    if record.checkout.is_some() {
        Avail::Yes
    } else {
        Avail::No("This session is not in a git checkout")
    }
}

/// *Open transcript*: only when the agent reported one.
pub fn transcript(record: &AgentRecord) -> Avail {
    if record.transcript.is_some() {
        Avail::Yes
    } else {
        Avail::No("This agent did not report a transcript")
    }
}

/// The group heading's text: `Needs you · 1`, `Idle · 2`; a *Needs you* group with more requests
/// than sessions adds them (`Needs you · 1 · 3 requests`).
pub fn group_label(group: Group, count: usize, requests: usize) -> String {
    let mut text = format!("{} · {count}", group.label());
    if group == Group::NeedsYou && requests > count {
        text.push_str(&format!(" · {requests} requests"));
    }
    text
}

#[cfg(test)]
mod tests {
    use hutan_core::{SessionId, WorkspaceId};

    use super::*;
    use crate::sessions::presentation::{Checkout, Reason, Request, Role};

    fn record() -> AgentRecord {
        AgentRecord {
            session: SessionId::new(),
            title: "Fix dock collapse".into(),
            role: Role::Agent,
            state: State::Working,
            group: Group::Working,
            reasons: Vec::new(),
            requests: Vec::new(),
            submitting: 0,
            questions: Vec::new(),
            agent: "omp".into(),
            harness: "OMP".into(),
            model: None,
            workspace: WorkspaceId::new(),
            container: None,
            project: "hutan".into(),
            container_name: None,
            checkout: None,
            folder: None,
            activity: None,
            detail: String::new(),
            updated_at_ms: None,
            reported_at_ms: None,
            started_at_ms: None,
            ended_at_ms: None,
            usage: None,
            transcript: None,
            provenance: Vec::new(),
            children: Vec::new(),
            subagents_live: 0,
            subagents_finished: 0,
            subagents_blocked: 0,
            live: true,
            managed: false,
            turn_active: false,
            ready_for_review: false,
            report_source: None,
        }
    }

    #[test]
    fn an_unknown_start_is_a_dash_that_says_why_and_never_the_session_age() {
        let r = record();
        let cell = duration_cell(&r, 1_000_000);
        assert_eq!(
            (cell.text.as_str(), cell.tooltip.as_str()),
            ("—", "Start time not reported")
        );
    }

    #[test]
    fn a_known_start_gives_running_and_ran() {
        let mut r = record();
        r.started_at_ms = Some(1_000_000 - 185_000);
        assert_eq!(duration_cell(&r, 1_000_000).text, "Running 3m 05s");
        r.state = State::Completed;
        r.ended_at_ms = Some(1_000_000 - 5_000);
        assert_eq!(duration_cell(&r, 1_000_000).text, "Ran 3m 00s");
        r.ended_at_ms = None;
        assert_eq!(duration_cell(&r, 1_000_000).text, "—");
    }

    #[test]
    fn updated_text_says_what_kind_of_time_it_is() {
        let mut r = record();
        assert_eq!(updated_text(&r, 100_000), "");
        r.updated_at_ms = Some(88_000);
        assert_eq!(updated_text(&r, 100_000), "Updated 12s ago");
        r.state = State::Unconfirmed;
        assert_eq!(updated_text(&r, 100_000), "Never reported");
        r.reported_at_ms = Some(1_000_000);
        r.updated_at_ms = None;
        assert_eq!(
            updated_text(&r, 1_000_000 + 3 * 3_600_000 + 1000),
            "Last reported 3h ago"
        );
        let mut done = record();
        done.state = State::Completed;
        done.ended_at_ms = Some(40_000);
        assert_eq!(updated_text(&done, 160_000), "Finished 2m ago");
    }

    #[test]
    fn the_reason_line_carries_the_plus_and_the_request_count() {
        let mut r = record();
        assert_eq!(reason_text(&r), None);
        r.reasons = vec![
            Reason {
                kind: ReasonKind::Approval,
                text: "Permission: run cargo test".into(),
                since_ms: None,
            },
            Reason {
                kind: ReasonKind::Review,
                text: "Review changes".into(),
                since_ms: None,
            },
        ];
        assert_eq!(reason_text(&r).unwrap(), "Permission: run cargo test +1");
        r.submitting = 2;
        r.requests = vec![Request {
            id: hutan_core::ApprovalId::new(),
            title: "run cargo test".into(),
            detail: None,
            options: Vec::new(),
            requested_at_ms: 1,
        }];
        assert_eq!(
            reason_text(&r).unwrap(),
            "Permission: run cargo test +1 · 3 requests"
        );
    }

    #[test]
    fn a_group_heading_is_its_name_and_count() {
        assert_eq!(group_label(Group::Idle, 2, 0), "Idle · 2");
        assert_eq!(group_label(Group::NeedsYou, 1, 1), "Needs you · 1");
        assert_eq!(
            group_label(Group::NeedsYou, 1, 3),
            "Needs you · 1 · 3 requests"
        );
    }

    #[test]
    fn a_session_waiting_for_a_decision_says_needs_you_and_never_working() {
        use crate::sessions::presentation::{Reason, ReasonKind};
        let mut r = record();
        assert_eq!(state_word(&r), "Working");
        // The agent is still `Working` underneath; attention wins.
        r.group = Group::NeedsYou;
        r.reasons = vec![Reason {
            kind: ReasonKind::Approval,
            text: "Permission: edit: src/lib.rs".into(),
            since_ms: None,
        }];
        assert_eq!(r.state, State::Working);
        assert_eq!(state_word(&r), "Needs you · approval");
        assert_eq!(reason_detail(&r).unwrap(), "edit: src/lib.rs");
        assert_eq!(lead(&r).0, crate::icons::Icon::StateBlocked);
        assert_eq!(lead(&r).1, crate::sessions::Tone::Warning);
        r.reasons[0] = Reason {
            kind: ReasonKind::Question,
            text: "Question: which branch".into(),
            since_ms: None,
        };
        assert_eq!(state_word(&r), "Needs you · question");
        assert_eq!(reason_detail(&r).unwrap(), "which branch");
        r.reasons[0] = Reason {
            kind: ReasonKind::Failed,
            text: "Failed: exit 1".into(),
            since_ms: None,
        };
        assert_eq!(state_word(&r), "Needs you · failed");
        assert_eq!(reason_detail(&r), reason_text(&r));
        assert_eq!(lead(&r).1, crate::sessions::Tone::Error);
    }

    #[test]
    fn the_second_line_is_never_empty() {
        let mut r = record();
        r.state = State::Unconfirmed;
        r.detail = "This harness has not reported its current state.".into();
        assert_eq!(
            status_line(&r),
            "State unavailable · This harness has not reported its current state."
        );
        r.state = State::Idle;
        assert!(!status_line(&r).is_empty());
        r.activity = Some("Tool: Reading icons.rs".into());
        assert_eq!(status_line(&r), "Tool: Reading icons.rs");
    }

    #[test]
    fn the_main_button_is_the_decision_or_open() {
        let mut r = record();
        assert_eq!(primary(&r), Primary::OpenSession);
        r.role = Role::Assistant;
        assert_eq!(primary(&r), Primary::OpenMawas);
        r.questions = vec![crate::sessions::presentation::Ask {
            id: hutan_core::QuestionId::new(),
            title: "branch".into(),
            requested_at_ms: 1,
        }];
        assert_eq!(primary(&r), Primary::AnswerQuestion);
        r.requests = vec![Request {
            id: hutan_core::ApprovalId::new(),
            title: "x".into(),
            detail: None,
            options: Vec::new(),
            requested_at_ms: 1,
        }];
        assert_eq!(primary(&r), Primary::ReviewRequest);
    }

    #[test]
    fn availability_follows_the_record_and_the_connection() {
        let mut r = record();
        let online = Caps {
            online: true,
            chat: true,
        };
        assert!(
            !interrupt(&r, online).is_yes(),
            "a terminal agent has no interrupt"
        );
        r.managed = true;
        assert!(!interrupt(&r, online).is_yes(), "no turn");
        r.turn_active = true;
        assert!(interrupt(&r, online).is_yes());
        assert!(
            !interrupt(
                &r,
                Caps {
                    online: false,
                    chat: true
                }
            )
            .is_yes()
        );
        assert!(stop(&r, online).is_yes());
        r.live = false;
        assert!(!stop(&r, online).is_yes());
        assert!(!changes(&r).is_yes());
        r.checkout = Some(Checkout {
            label: "Main checkout: master".into(),
            root: "/code".into(),
            branch: Some("master".into()),
            worktree: false,
            shared: false,
        });
        assert!(changes(&r).is_yes());
        assert!(!transcript(&r).is_yes());
    }

    #[test]
    fn the_accessible_name_speaks_title_state_harness_workspace_and_reason() {
        let mut r = record();
        r.role = Role::Assistant;
        r.title = "Mawas".into();
        r.state = State::Unconfirmed;
        r.project = "Home".into();
        assert_eq!(
            accessible_name(&r),
            "Mawas, Assistant, State unavailable, OMP, Home / Mawas"
        );
    }

    #[test]
    fn children_are_counted_by_their_own_noun() {
        let mut r = record();
        assert_eq!(children_text(&r), None);
        r.subagents_live = 3;
        r.subagents_blocked = 1;
        assert_eq!(children_text(&r).unwrap(), "3 subagents · 1 blocked");
        r.subagents_live = 0;
        r.subagents_finished = 1;
        assert_eq!(children_text(&r).unwrap(), "1 finished subagent");
    }

    #[test]
    fn a_reason_says_who_reported_it_and_how_long_ago_on_hover() {
        use crate::sessions::presentation::{Reason, ReasonKind};
        let mut r = record();
        assert_eq!(reason_tooltip(&r, 1_000_000), None, "nothing to explain");
        r.group = Group::NeedsYou;
        r.reasons = vec![
            Reason {
                kind: ReasonKind::Approval,
                text: "Permission: run cargo test".into(),
                since_ms: Some(1_000_000 - 120_000),
            },
            Reason {
                kind: ReasonKind::CompletedUnread,
                text: "Completed · unread".into(),
                since_ms: None,
            },
        ];
        r.report_source = Some("claude-hooks".into());
        r.reported_at_ms = Some(1_000_000 - 30_000);
        assert_eq!(
            reason_tooltip(&r, 1_000_000).unwrap(),
            "Permission: run cargo test · reported by claude-hooks · 2m ago\n\
             Completed · unread · reported by claude-hooks · 30s ago"
        );
        // A chat the daemon runs says so; a snoozed session says that too.
        r.report_source = None;
        r.managed = true;
        r.group = Group::Snoozed;
        let text = reason_tooltip(&r, 1_000_000).unwrap();
        assert!(
            text.starts_with(
                "Permission: run cargo test · known to the daemon, which runs this chat · 2m ago"
            ),
            "{text}"
        );
        assert!(text.ends_with("Snoozed until this changes"), "{text}");
        assert_eq!(
            reason_text(&r).unwrap(),
            "Snoozed · Permission: run cargo test +1"
        );
    }
}
