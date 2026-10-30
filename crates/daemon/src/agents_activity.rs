//! `agents.activity` (protocol 0.65, capability `agents.activity`, ADR 0025): the daemon's
//! answer to "what happened while I was away".
//!
//! A pure function over what the daemon already records — the turn table, the audit tables of
//! approvals and questions, the checkpoints, the task store and the session rows — so it can be
//! tested without a daemon. Nothing is invented: a terminal agent has no turns and no history
//! of states, so it contributes only the sessions that started and ended
//! ([`NOT_RECORDED`] says so in the answer), and a count nobody recorded is absent.
//!
//! The answer is bounded like the board ([`ACTIVITY_MAX_EVENTS`], [`ACTIVITY_MAX_BYTES`]), the
//! events are listed newest first, every text is one redacted line cut at 200 characters, and
//! the summary counts **everything** of the window, listed or not.

use std::collections::HashMap;

use hutan_core::task::{DeliveryState, Task, TaskState};
use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
use hutan_protocol::activity::{
    ACTIVITY_DEFAULT_EVENTS, ACTIVITY_DEFAULT_WINDOW_MS, ACTIVITY_MAX_BYTES, ACTIVITY_MAX_EVENTS,
    ActivityDetail, ActivityEvent, ActivitySummary, AgentsActivity, AgentsActivityRequest,
    RequestKind, TurnOutcome,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_storage::{
    ApprovalOutcome, ApprovalRow, ChatTurnRow, ChatTurnStatus, CheckpointRow, QuestionOutcome,
    QuestionRow, SessionRow, WorkspaceRow,
};

use crate::agents_board::text;

/// What this answer cannot say. The same words every time: they describe the daemon, not the
/// window.
pub const NOT_RECORDED: &[&str] = &[
    "turns, durations and token counts of terminal agents (only chats are journaled)",
    "the states of terminal agents over time (the daemon keeps only the current one)",
    "who answered a terminal agent's own prompt",
    "files and lines of terminal agents' work (checkpoints exist for chats, 30 days)",
];

/// What the answer is built from: everything the daemon records, read for the window.
#[derive(Debug)]
pub struct ActivityInputs<'a> {
    pub now_ms: u64,
    pub workspaces: &'a [WorkspaceRow],
    /// Every session row, closed ones included (a closed session keeps its rows).
    pub sessions: &'a [SessionRow],
    pub approvals: &'a [ApprovalRow],
    pub questions: &'a [QuestionRow],
    pub turns: &'a [ChatTurnRow],
    pub checkpoints: &'a [CheckpointRow],
    pub tasks: &'a [Task],
}

/// The window `req` asks for: `(since, until)`; `until` is never before `since`.
pub fn window(req: &AgentsActivityRequest, now_ms: u64) -> (u64, u64) {
    let until = req.until_ms.unwrap_or(now_ms);
    let since = req
        .since_ms
        .unwrap_or_else(|| until.saturating_sub(ACTIVITY_DEFAULT_WINDOW_MS));
    (since.min(until), until)
}

/// The catch-up of `req` over `inputs`. The inputs hold the records of the window already
/// (`window`); this filters them to the scope and shapes them.
pub fn build_activity(
    inputs: &ActivityInputs<'_>,
    req: &AgentsActivityRequest,
) -> Result<AgentsActivity, ErrorBody> {
    let (since, until) = window(req, inputs.now_ms);
    if let Some(id) = req.workspace_id
        && !inputs.workspaces.iter().any(|w| w.id == id)
    {
        return Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no workspace {id}"),
        ));
    }
    if let Some(id) = req.session_id
        && !inputs.sessions.iter().any(|s| s.id == id)
    {
        return Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no session {id}"),
        ));
    }
    let scope = Scope::of(inputs, req);
    let in_window = |at: u64| (since..=until).contains(&at);
    let mut events: Vec<ActivityEvent> = Vec::new();
    let mut summary = ActivitySummary::default();

    // Sessions: started and ended.
    for row in inputs.sessions.iter().filter(|r| scope.has_session(r)) {
        let (title, project) = scope.names(row);
        if in_window(row.created_at_ms) {
            summary.sessions_started += 1;
            events.push(ActivityEvent {
                at_ms: row.created_at_ms,
                session_id: Some(row.id),
                title: title.clone(),
                project: project.clone(),
                detail: ActivityDetail::SessionStarted {
                    chat: row.kind != SessionKind::Terminal,
                },
            });
        }
        if let Some(ended) = row.ended_at_ms.filter(|ms| in_window(*ms))
            && !row.state.is_live()
        {
            summary.sessions_ended += 1;
            events.push(ActivityEvent {
                at_ms: ended,
                session_id: Some(row.id),
                title,
                project,
                detail: ActivityDetail::SessionEnded {
                    state: ended_text(&row.state),
                },
            });
        }
    }

    // Turns of chats.
    for turn in inputs.turns.iter().filter(|t| scope.has(t.session_id)) {
        let Some((title, project)) = scope.names_of(turn.session_id) else {
            continue;
        };
        match (turn.status, turn.ended_at_ms) {
            (ChatTurnStatus::Running, _) | (_, None) => {
                // Started in the window and not over: only while the session lives (a daemon
                // that crashed mid-turn leaves a row nobody will close).
                if in_window(turn.started_at_ms) && scope.is_live(turn.session_id) {
                    summary.turns_running += 1;
                }
            }
            (status, Some(ended)) if in_window(ended) => {
                let outcome = match status {
                    ChatTurnStatus::Completed => {
                        summary.turns_finished += 1;
                        TurnOutcome::Completed
                    }
                    ChatTurnStatus::Failed => {
                        summary.turns_failed += 1;
                        TurnOutcome::Failed
                    }
                    ChatTurnStatus::Cancelled | ChatTurnStatus::Running => {
                        summary.turns_cancelled += 1;
                        TurnOutcome::Cancelled
                    }
                };
                events.push(ActivityEvent {
                    at_ms: ended,
                    session_id: Some(turn.session_id),
                    title,
                    project,
                    detail: ActivityDetail::TurnEnded {
                        outcome,
                        duration_ms: Some(ended.saturating_sub(turn.started_at_ms)),
                        error: turn.error.as_deref().map(text).filter(|t| !t.is_empty()),
                    },
                });
            }
            _ => {}
        }
    }

    // Approvals and questions.
    let mut waits: Vec<u64> = Vec::new();
    let mut request = |kind: RequestKind,
                       session: SessionId,
                       asked: u64,
                       resolved: Option<u64>,
                       what: &str,
                       outcome: Option<String>,
                       decided_by: Option<&String>,
                       events: &mut Vec<ActivityEvent>,
                       summary: &mut ActivitySummary| {
        if !scope.has(session) {
            return;
        }
        let Some((title, project)) = scope.names_of(session) else {
            return;
        };
        // A resolution after the window is, as of its end, still a wait.
        let resolved = resolved.filter(|ms| *ms <= until);
        let asked_in = in_window(asked);
        let resolved_in = resolved.is_some_and(in_window);
        if !asked_in && !resolved_in {
            return;
        }
        if asked_in {
            summary.requests_asked += 1;
            if resolved.is_none() {
                summary.requests_open += 1;
            }
        }
        let wait = resolved.map(|ms| ms.saturating_sub(asked));
        if resolved_in {
            summary.requests_resolved += 1;
            waits.extend(wait);
        }
        events.push(ActivityEvent {
            at_ms: if resolved_in {
                resolved.unwrap_or(asked)
            } else {
                asked
            },
            session_id: Some(session),
            title,
            project,
            detail: ActivityDetail::Request {
                request: kind,
                text: text(what),
                asked_ms: asked,
                resolved_ms: resolved,
                wait_ms: wait,
                outcome: resolved.and(outcome),
                decided_by: resolved.and(decided_by.map(|d| text(d))),
            },
        });
    };
    for row in inputs.approvals {
        request(
            RequestKind::Approval,
            row.session_id,
            row.requested_at_ms,
            row.resolved_at_ms,
            &row.title,
            row.outcome.map(|o| {
                match o {
                    ApprovalOutcome::Selected => "selected",
                    ApprovalOutcome::Expired => "expired",
                }
                .to_owned()
            }),
            row.decided_by.as_ref(),
            &mut events,
            &mut summary,
        );
    }
    for row in inputs.questions {
        request(
            RequestKind::Question,
            row.session_id,
            row.requested_at_ms,
            row.resolved_at_ms,
            &row.title,
            row.outcome.map(|o| {
                match o {
                    QuestionOutcome::Answered => "answered",
                    QuestionOutcome::Declined => "declined",
                    QuestionOutcome::Cancelled => "cancelled",
                    QuestionOutcome::Expired => "expired",
                }
                .to_owned()
            }),
            row.decided_by.as_ref(),
            &mut events,
            &mut summary,
        );
    }
    if !waits.is_empty() {
        summary.wait_total_ms = Some(waits.iter().sum());
        summary.wait_longest_ms = waits.iter().copied().max();
    }

    // Checkpoints: what the turns of chats changed.
    for row in inputs.checkpoints.iter().filter(|c| {
        c.kind == hutan_core::CheckpointKind::Turn
            && matches!(
                c.state,
                hutan_core::CheckpointState::Ok | hutan_core::CheckpointState::Late
            )
            && c.files_changed > 0
            && in_window(c.created_ms)
            && scope.has(c.session_id)
    }) {
        let Some((title, project)) = scope.names_of(row.session_id) else {
            continue;
        };
        summary.checkpoints += 1;
        *summary.files_changed.get_or_insert(0) += row.files_changed;
        *summary.lines_added.get_or_insert(0) += row.lines_added;
        *summary.lines_removed.get_or_insert(0) += row.lines_removed;
        events.push(ActivityEvent {
            at_ms: row.created_ms,
            session_id: Some(row.session_id),
            title,
            project,
            detail: ActivityDetail::Checkpoint {
                files: row.files_changed,
                added: row.lines_added,
                removed: row.lines_removed,
            },
        });
    }

    // Tasks: how they ended and where their work went in review.
    for task in inputs.tasks.iter().filter(|t| scope.has_task(t)) {
        let project = scope.workspace_name(task.workspace_id);
        let session = task.sessions.last().copied();
        let mut push = |at_ms: u64, state: String, run: bool| {
            events.push(ActivityEvent {
                at_ms,
                session_id: session,
                title: text(&task.title),
                project: project.clone(),
                detail: ActivityDetail::Task { state, run },
            });
        };
        if let Some(ended) = task.ended_at_ms.filter(|ms| in_window(*ms)) {
            match task.state {
                TaskState::Done => summary.tasks_done += 1,
                TaskState::Failed => summary.tasks_failed += 1,
                TaskState::Cancelled => {}
                _ => continue,
            }
            push(ended, task.state.as_str().to_owned(), true);
        }
        for record in task.delivery_history.iter().filter(|r| in_window(r.at_ms)) {
            if record.state == DeliveryState::AwaitingReview {
                summary.tasks_review += 1;
            }
            push(record.at_ms, record.state.as_str().to_owned(), false);
        }
    }

    // Newest first; a stable order for equal times.
    events.sort_by(|a, b| {
        b.at_ms
            .cmp(&a.at_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    let total = events.len();
    let limit = req
        .limit
        .unwrap_or(ACTIVITY_DEFAULT_EVENTS)
        .clamp(1, ACTIVITY_MAX_EVENTS as u32) as usize;
    let mut answer = AgentsActivity {
        from_ms: since,
        to_ms: until,
        coverage_from_ms: inputs.sessions.iter().map(|s| s.created_at_ms).min(),
        summary,
        events: Vec::new(),
        omitted: 0,
        not_recorded: NOT_RECORDED.iter().map(|s| (*s).to_owned()).collect(),
    };
    let mut room =
        ACTIVITY_MAX_BYTES.saturating_sub(serde_json::to_vec(&answer).map_or(0, |v| v.len()) + 16);
    for event in events {
        let size = serde_json::to_vec(&event).map_or(usize::MAX, |v| v.len() + 1);
        if answer.events.len() >= limit || size > room {
            // The rest are older: none is listed out of order.
            room = 0;
            continue;
        }
        room -= size;
        answer.events.push(event);
    }
    answer.omitted = total - answer.events.len();
    Ok(answer)
}

/// How a session's process ended, in words.
fn ended_text(state: &SessionState) -> String {
    match state {
        SessionState::Exited {
            code: _,
            signal: Some(signal),
        } => format!("killed by signal {signal}"),
        SessionState::Exited {
            code: Some(code), ..
        } => format!("exited {code}"),
        SessionState::Exited { .. } => "exited".to_owned(),
        SessionState::Failed { reason } => text(&format!("failed: {reason}")),
        SessionState::Stopped => "stopped".to_owned(),
        // A live session has not ended: the caller never asks.
        _ => "ended".to_owned(),
    }
}

/// Which sessions and tasks the request is about, and what they are called.
struct Scope<'a> {
    workspaces: HashMap<WorkspaceId, &'a WorkspaceRow>,
    sessions: HashMap<SessionId, &'a SessionRow>,
    workspace: Option<WorkspaceId>,
    session: Option<SessionId>,
}

impl<'a> Scope<'a> {
    fn of(inputs: &ActivityInputs<'a>, req: &AgentsActivityRequest) -> Self {
        Self {
            workspaces: inputs.workspaces.iter().map(|w| (w.id, w)).collect(),
            sessions: inputs.sessions.iter().map(|s| (s.id, s)).collect(),
            workspace: req.workspace_id,
            session: req.session_id,
        }
    }

    /// Whether `workspace` is the asked one or a project in it.
    fn in_workspace(&self, workspace: WorkspaceId) -> bool {
        self.workspace.is_none_or(|asked| {
            workspace == asked
                || self
                    .workspaces
                    .get(&workspace)
                    .and_then(|w| w.parent_id)
                    .is_some_and(|parent| parent == asked)
        })
    }

    fn has_session(&self, row: &SessionRow) -> bool {
        self.session.is_none_or(|asked| asked == row.id) && self.in_workspace(row.workspace_id)
    }

    /// A session of the scope (one the rows still know).
    fn has(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|row| self.has_session(row))
    }

    fn has_task(&self, task: &Task) -> bool {
        self.in_workspace(task.workspace_id)
            && self
                .session
                .is_none_or(|asked| task.sessions.last() == Some(&asked))
    }

    fn is_live(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|row| row.state.is_live())
    }

    fn workspace_name(&self, id: WorkspaceId) -> String {
        self.workspaces
            .get(&id)
            .map_or_else(|| "Unknown project".to_owned(), |w| text(&w.name))
    }

    fn names(&self, row: &SessionRow) -> (String, String) {
        (text(&row.title), self.workspace_name(row.workspace_id))
    }

    fn names_of(&self, session: SessionId) -> Option<(String, String)> {
        self.sessions.get(&session).map(|row| self.names(row))
    }
}

/// The catch-up of `req`, read from the daemon's records: the tables are read once each for the
/// window, on a blocking thread (they are SQLite reads), and the answer is built from those rows
/// alone. What `agents.activity` answers, and what the digest looks at (`crate::digest`).
pub async fn read_activity(
    storage: &hutan_storage::Storage,
    tasks: &crate::tasks::TaskService,
    req: &AgentsActivityRequest,
) -> Result<AgentsActivity, ErrorBody> {
    let now = crate::registry::now_ms();
    let (since, until) = window(req, now);
    let storage = storage.clone();
    let rows = tokio::task::spawn_blocking(move || -> Result<_, ErrorBody> {
        let failed = |error: hutan_storage::StorageError| {
            ErrorBody::new(ErrorCode::Internal, error.to_string())
        };
        let mut sessions = storage.sessions().list().map_err(failed)?;
        sessions.extend(
            storage
                .sessions()
                .list_closed()
                .map_err(failed)?
                .into_iter()
                .map(|closed| closed.session),
        );
        Ok((
            storage.workspaces().list().map_err(failed)?,
            sessions,
            storage
                .approvals()
                .in_window(since, until)
                .map_err(failed)?,
            storage
                .questions()
                .in_window(since, until)
                .map_err(failed)?,
            storage
                .chat()
                .turns_in_window(since, until)
                .map_err(failed)?,
            storage
                .checkpoints()
                .in_window(since, until)
                .map_err(failed)?,
        ))
    })
    .await
    .map_err(|error| ErrorBody::new(ErrorCode::Internal, error.to_string()))??;
    let (workspaces, sessions, approvals, questions, turns, checkpoints) = rows;
    let tasks = tasks
        .list(hutan_protocol::task::TaskListRequest::default())
        .tasks;
    build_activity(
        &ActivityInputs {
            now_ms: now,
            workspaces: &workspaces,
            sessions: &sessions,
            approvals: &approvals,
            questions: &questions,
            turns: &turns,
            checkpoints: &checkpoints,
            tasks: &tasks,
        },
        req,
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use hutan_core::task::DeliveryActor;
    use hutan_core::{
        ApprovalId, CheckpointId, CheckpointKind, CheckpointState, QuestionId, TaskId,
    };
    use hutan_storage::{CreatedBy, WorkspaceKind};

    use super::*;

    const T0: u64 = 1_000_000_000;

    fn workspace(name: &str, parent: Option<WorkspaceId>) -> WorkspaceRow {
        WorkspaceRow {
            id: WorkspaceId::new(),
            name: name.into(),
            roots: Vec::new(),
            kind: WorkspaceKind::Project,
            pinned: false,
            created_by: CreatedBy::User,
            created_at_ms: 1,
            last_active_ms: 1,
            position: 0,
            common_dir: None,
            parent_id: parent,
        }
    }

    fn session(
        workspace: &WorkspaceRow,
        title: &str,
        kind: SessionKind,
        created: u64,
    ) -> SessionRow {
        SessionRow {
            id: SessionId::new(),
            workspace_id: workspace.id,
            kind,
            title: title.into(),
            title_locked: false,
            spec_json: "{}".into(),
            cwd: None,
            state: SessionState::Idle,
            created_at_ms: created,
            ended_at_ms: None,
            resume_ref_json: None,
        }
    }

    fn approval(
        session: &SessionRow,
        title: &str,
        asked: u64,
        resolved: Option<u64>,
    ) -> ApprovalRow {
        ApprovalRow {
            id: ApprovalId::new(),
            session_id: session.id,
            turn_id: None,
            kind: "command".into(),
            title: title.into(),
            options_json: "[]".into(),
            requested_at_ms: asked,
            resolved_at_ms: resolved,
            outcome: resolved.map(|_| ApprovalOutcome::Selected),
            option_id: resolved.map(|_| "accept".into()),
            decided_by: resolved.map(|_| "desktop".into()),
        }
    }

    fn turn(
        session: &SessionRow,
        n: u64,
        status: ChatTurnStatus,
        started: u64,
        ended: Option<u64>,
    ) -> ChatTurnRow {
        ChatTurnRow {
            session_id: session.id,
            turn_id: format!("t{n}"),
            provider_turn_id: None,
            status,
            started_at_ms: started,
            ended_at_ms: ended,
            error: (status == ChatTurnStatus::Failed).then(|| "Rate limited".to_owned()),
        }
    }

    fn inputs<'a>(
        workspaces: &'a [WorkspaceRow],
        sessions: &'a [SessionRow],
        approvals: &'a [ApprovalRow],
        questions: &'a [QuestionRow],
        turns: &'a [ChatTurnRow],
        checkpoints: &'a [CheckpointRow],
        tasks: &'a [Task],
    ) -> ActivityInputs<'a> {
        ActivityInputs {
            now_ms: T0 + 10_000,
            workspaces,
            sessions,
            approvals,
            questions,
            turns,
            checkpoints,
            tasks,
        }
    }

    fn since(ms: u64) -> AgentsActivityRequest {
        AgentsActivityRequest {
            since_ms: Some(ms),
            ..Default::default()
        }
    }

    #[test]
    fn turns_requests_and_sessions_are_counted_and_listed_newest_first() {
        let alpha = workspace("alpha", None);
        let chat = session(&alpha, "Touch a file", SessionKind::Harness, T0 + 100);
        let mut shell = session(&alpha, "build", SessionKind::Terminal, T0 + 200);
        shell.state = SessionState::Exited {
            code: Some(2),
            signal: None,
        };
        shell.ended_at_ms = Some(T0 + 900);
        let turns = [
            turn(
                &chat,
                1,
                ChatTurnStatus::Completed,
                T0 + 300,
                Some(T0 + 350),
            ),
            turn(&chat, 2, ChatTurnStatus::Failed, T0 + 400, Some(T0 + 460)),
            turn(&chat, 3, ChatTurnStatus::Running, T0 + 800, None),
        ];
        let approvals = [
            approval(&chat, "run cargo test", T0 + 310, Some(T0 + 340)),
            approval(&chat, "touch hi.txt", T0 + 700, None),
        ];
        let sessions = [chat.clone(), shell.clone()];
        let workspaces = [alpha];
        let answer = build_activity(
            &inputs(&workspaces, &sessions, &approvals, &[], &turns, &[], &[]),
            &since(T0),
        )
        .unwrap();
        let s = answer.summary;
        assert_eq!(
            (s.turns_finished, s.turns_failed, s.turns_running),
            (1, 1, 1)
        );
        assert_eq!(
            (s.requests_asked, s.requests_resolved, s.requests_open),
            (2, 1, 1)
        );
        assert_eq!((s.wait_total_ms, s.wait_longest_ms), (Some(30), Some(30)));
        assert_eq!((s.sessions_started, s.sessions_ended), (2, 1));
        let times: Vec<u64> = answer.events.iter().map(|e| e.at_ms).collect();
        assert!(
            times.windows(2).all(|w| w[0] >= w[1]),
            "newest first: {times:?}"
        );
        // The failed turn says why; the ended shell says how.
        assert!(answer.events.iter().any(|e| matches!(
            &e.detail,
            ActivityDetail::TurnEnded { outcome: TurnOutcome::Failed, error: Some(error), duration_ms: Some(60) }
                if error == "Rate limited"
        )));
        assert!(answer.events.iter().any(|e| matches!(
            &e.detail,
            ActivityDetail::SessionEnded { state } if state == "exited 2"
        )));
        // The pending request is listed at the time it was asked, with no resolution.
        assert!(answer.events.iter().any(|e| matches!(
            &e.detail,
            ActivityDetail::Request { text, resolved_ms: None, wait_ms: None, .. } if text == "touch hi.txt"
        )));
        assert_eq!(answer.coverage_from_ms, Some(T0 + 100));
        assert!(!answer.not_recorded.is_empty());
    }

    #[test]
    fn the_window_decides_what_counts_and_a_wait_over_its_end_is_still_a_wait() {
        let alpha = workspace("alpha", None);
        let chat = session(&alpha, "chat", SessionKind::Harness, T0);
        let approvals = [
            // Asked before the window, answered inside: a resolution only.
            approval(&chat, "early", T0 + 10, Some(T0 + 500)),
            // Asked inside, answered after the window ended: still open at its end.
            approval(&chat, "late", T0 + 600, Some(T0 + 5_000)),
        ];
        let workspaces = [alpha];
        let sessions = [chat];
        let answer = build_activity(
            &inputs(&workspaces, &sessions, &approvals, &[], &[], &[], &[]),
            &AgentsActivityRequest {
                since_ms: Some(T0 + 400),
                until_ms: Some(T0 + 1_000),
                ..Default::default()
            },
        )
        .unwrap();
        let s = answer.summary;
        assert_eq!(
            (s.requests_asked, s.requests_resolved, s.requests_open),
            (1, 1, 1)
        );
        assert_eq!(s.wait_longest_ms, Some(490));
        assert_eq!((answer.from_ms, answer.to_ms), (T0 + 400, T0 + 1_000));
        let late = answer
            .events
            .iter()
            .find(|e| matches!(&e.detail, ActivityDetail::Request { text, .. } if text == "late"))
            .unwrap();
        assert!(matches!(
            late.detail,
            ActivityDetail::Request {
                resolved_ms: None,
                ..
            }
        ));
    }

    #[test]
    fn a_workspace_narrows_the_answer_with_its_projects_and_unknown_ids_are_not_found() {
        let team = workspace("team", None);
        let app = workspace("app", Some(team.id));
        let other = workspace("other", None);
        let in_app = session(&app, "in app", SessionKind::Harness, T0 + 1);
        let in_other = session(&other, "in other", SessionKind::Harness, T0 + 2);
        let workspaces = [team.clone(), app, other];
        let sessions = [in_app.clone(), in_other.clone()];
        let all = build_activity(
            &inputs(&workspaces, &sessions, &[], &[], &[], &[], &[]),
            &since(T0),
        )
        .unwrap();
        assert_eq!(all.summary.sessions_started, 2);
        let only_team = build_activity(
            &inputs(&workspaces, &sessions, &[], &[], &[], &[], &[]),
            &AgentsActivityRequest {
                since_ms: Some(T0),
                workspace_id: Some(team.id),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            only_team
                .events
                .iter()
                .map(|e| e.session_id)
                .collect::<Vec<_>>(),
            [Some(in_app.id)]
        );
        let one = build_activity(
            &inputs(&workspaces, &sessions, &[], &[], &[], &[], &[]),
            &AgentsActivityRequest {
                since_ms: Some(T0),
                session_id: Some(in_other.id),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(one.summary.sessions_started, 1);
        for req in [
            AgentsActivityRequest {
                workspace_id: Some(WorkspaceId::new()),
                ..Default::default()
            },
            AgentsActivityRequest {
                session_id: Some(SessionId::new()),
                ..Default::default()
            },
        ] {
            let error = build_activity(
                &inputs(&workspaces, &sessions, &[], &[], &[], &[], &[]),
                &req,
            )
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
        }
    }

    #[test]
    fn questions_checkpoints_and_tasks_have_their_own_events_and_counts() {
        let alpha = workspace("alpha", None);
        let chat = session(&alpha, "chat", SessionKind::Harness, T0);
        let question = QuestionRow {
            id: QuestionId::new(),
            session_id: chat.id,
            turn_id: None,
            title: "Which branch?".into(),
            requested_at_ms: T0 + 100,
            resolved_at_ms: Some(T0 + 160),
            outcome: Some(QuestionOutcome::Answered),
            decided_by: Some("cli".into()),
        };
        let mut checkpoint = CheckpointRow::new(
            CheckpointId::new(),
            chat.id,
            CheckpointKind::Turn,
            CheckpointState::Ok,
            T0 + 200,
        );
        checkpoint.files_changed = 3;
        checkpoint.lines_added = 40;
        checkpoint.lines_removed = 5;
        let mut unchanged = checkpoint.clone();
        unchanged.id = CheckpointId::new();
        unchanged.state = CheckpointState::Unchanged;
        unchanged.files_changed = 0;
        let mut task = Task {
            id: TaskId::new(),
            workspace_id: alpha.id,
            title: "Add the card".into(),
            prompt: None,
            state: TaskState::Done,
            depends_on: Vec::new(),
            placement: Default::default(),
            run: None,
            auto_start: false,
            sessions: vec![chat.id],
            created_at_ms: T0,
            started_at_ms: Some(T0 + 10),
            ended_at_ms: Some(T0 + 300),
            outcome: None,
            delivery: DeliveryState::AwaitingReview,
            after_policy: Default::default(),
            delivery_history: Vec::new(),
        };
        task.record_delivery(hutan_core::task::DeliveryRecord::new(
            DeliveryState::AwaitingReview,
            DeliveryActor::Daemon,
            T0 + 310,
        ));
        let workspaces = [alpha];
        let sessions = [chat];
        let answer = build_activity(
            &inputs(
                &workspaces,
                &sessions,
                &[],
                std::slice::from_ref(&question),
                &[],
                &[checkpoint, unchanged],
                std::slice::from_ref(&task),
            ),
            &since(T0 + 50),
        )
        .unwrap();
        let s = answer.summary;
        assert_eq!((s.requests_asked, s.requests_resolved), (1, 1));
        assert_eq!(s.wait_longest_ms, Some(60));
        assert_eq!(
            (
                s.checkpoints,
                s.files_changed,
                s.lines_added,
                s.lines_removed
            ),
            (1, Some(3), Some(40), Some(5)),
            "a turn that changed nothing is no checkpoint"
        );
        assert_eq!((s.tasks_done, s.tasks_failed, s.tasks_review), (1, 0, 1));
        let kinds: Vec<&str> = answer
            .events
            .iter()
            .map(|e| match &e.detail {
                ActivityDetail::Request { .. } => "request",
                ActivityDetail::Checkpoint { .. } => "checkpoint",
                ActivityDetail::Task { run: true, .. } => "task-run",
                ActivityDetail::Task { run: false, .. } => "task-review",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["task-review", "task-run", "checkpoint", "request"]);
    }

    #[test]
    fn nothing_recorded_is_an_empty_answer_and_counts_stay_absent() {
        let answer = build_activity(
            &inputs(&[], &[], &[], &[], &[], &[], &[]),
            &AgentsActivityRequest::default(),
        )
        .unwrap();
        assert!(answer.events.is_empty());
        assert_eq!(answer.summary, ActivitySummary::default());
        assert_eq!(answer.coverage_from_ms, None);
        assert_eq!(
            answer.summary.files_changed, None,
            "not recorded is not zero"
        );
        assert_eq!(answer.summary.wait_longest_ms, None);
        // The default window is the last day.
        assert_eq!(answer.to_ms - answer.from_ms, ACTIVITY_DEFAULT_WINDOW_MS);
    }

    #[test]
    fn the_list_is_bounded_redacted_and_cut_and_the_summary_still_counts_everything() {
        let alpha = workspace("alpha", None);
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let long = "y".repeat(900);
        let chat = session(&alpha, "chat", SessionKind::Harness, T0);
        let mut approvals = vec![
            approval(&chat, &format!("deploy with {secret}"), T0 + 5_000, None),
            approval(&chat, &long, T0 + 5_001, None),
        ];
        for n in 0..300 {
            approvals.push(approval(&chat, &format!("ask {n}"), T0 + 10 + n, None));
        }
        let workspaces = [alpha];
        let sessions = [chat];
        let everything = build_activity(
            &inputs(&workspaces, &sessions, &approvals, &[], &[], &[], &[]),
            &AgentsActivityRequest {
                since_ms: Some(T0),
                limit: Some(1_000),
                ..Default::default()
            },
        )
        .unwrap();
        let json = serde_json::to_string(&everything).unwrap();
        assert!(json.len() <= ACTIVITY_MAX_BYTES, "{} bytes", json.len());
        assert!(everything.events.len() <= ACTIVITY_MAX_EVENTS);
        assert_eq!(everything.summary.requests_asked, 302);
        // 302 requests and the session that began.
        assert_eq!(everything.events.len() + everything.omitted, 303);
        assert!(!json.contains(secret));
        assert!(json.contains("[REDACTED]"));
        let cut = everything
            .events
            .iter()
            .find_map(|e| match &e.detail {
                ActivityDetail::Request { text, .. } if text.starts_with("yyy") => Some(text),
                _ => None,
            })
            .unwrap();
        assert_eq!(cut.chars().count(), 200);
        assert!(cut.ends_with('…'));
        // A limit asks for fewer.
        let few = build_activity(
            &inputs(&workspaces, &sessions, &approvals, &[], &[], &[], &[]),
            &AgentsActivityRequest {
                since_ms: Some(T0),
                limit: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(few.events.len(), 5);
        assert_eq!(few.omitted, 303 - 5);
    }
}
