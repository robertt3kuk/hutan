//! `agents.board` (protocol 0.63, capability `agents.board`, ADR 0025): the daemon's answer to
//! "what are my agents doing and who waits for whom".
//!
//! It is the reducer of the Agents page (`hutan-agents-view`) run over the daemon's own state,
//! so the window, the CLI and a model read one rule set. What only a window knows (unread
//! marks, snoozes, answers not yet confirmed) is empty here; the daemon says instead what it
//! can know without a user present:
//!
//! - an agent the reducer finds failed (a non-zero exit, a signal, a spawn failure, an agent
//!   that reported its own failure) stays in *Failed* — what the window shows after the user
//!   looked at it ([`Unread::FailedSeen`]);
//! - a session whose task finished and waits for a reviewer is *Ready for review*;
//! - nothing is *Snoozed*, and an unseen completion is no reason.
//!
//! The answer is bounded ([`BOARD_MAX_AGENTS`], [`BOARD_MAX_BYTES`]), every text is redacted and
//! cut ([`BOARD_TEXT_CHARS`]), and every row says where its facts come from
//! ([`BoardSource`]). Nothing is invented: a missing model or time stays absent.

use std::collections::{BTreeMap, HashMap, HashSet};

use hutan_agents_view::attention::Unread;
use hutan_agents_view::models::ModelNames;
use hutan_agents_view::presentation::{
    AgentRecord, Counts, Facts, Group, Link, ReasonKind, Role, Scope, State, build,
};
use hutan_core::task::{DeliveryState, Task, TaskState};
use hutan_core::{AgentObservation, SessionId};
use hutan_protocol::board::{
    AgentsBoard, AgentsBoardRequest, BOARD_MAX_AGENTS, BOARD_MAX_BYTES, BOARD_TEXT_CHARS,
    BoardAgent, BoardCheckout, BoardCounts, BoardCoverage, BoardGroup, BoardKind, BoardRef,
    BoardRole, BoardSource, BoardState, BoardSubagents, BoardWaitKind, BoardWaiting,
};
use hutan_protocol::chat::{ApprovalInfo, QuestionInfo};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{SessionInfo, WorkspaceInfo};

use crate::context::{Keep, bound};
use crate::term_agent::OSC_SOURCE;
use hutan_index::redact::redact;

/// What the board is built from: the daemon's state at one moment.
#[derive(Debug)]
pub struct BoardInputs<'a> {
    pub now_ms: u64,
    pub workspaces: &'a [WorkspaceInfo],
    pub sessions: &'a [SessionInfo],
    pub approvals: &'a [ApprovalInfo],
    pub questions: &'a [QuestionInfo],
    pub trees: &'a BTreeMap<SessionId, AgentObservation>,
    pub tasks: &'a [Task],
}

/// The board of `req` over `inputs`.
pub fn build_board(
    inputs: &BoardInputs<'_>,
    req: AgentsBoardRequest,
) -> Result<AgentsBoard, ErrorBody> {
    if let Some(id) = req.workspace_id
        && !inputs.workspaces.iter().any(|w| w.id == id)
    {
        return Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no workspace {id}"),
        ));
    }
    let reviews = awaiting_review(inputs.tasks);
    let reduce = |unread: &BTreeMap<SessionId, Unread>| {
        build(&Facts {
            link: Link::Online,
            workspaces: inputs.workspaces,
            sessions: inputs.sessions,
            approvals: inputs.approvals,
            questions: inputs.questions,
            trees: inputs.trees,
            unread,
            reviews: &reviews,
            changes_to_review: &HashSet::new(),
            snoozed: &HashSet::new(),
            answered_approvals: &HashMap::new(),
            answered_questions: &HashSet::new(),
            model_names: &ModelNames::default(),
            revision: 0,
        })
    };
    // A window keeps a failure in *Failed* after the user has looked at it, until a new run
    // starts. With nobody to look, the daemon says the same of every agent the reducer finds
    // failed: a non-zero exit, a signal, a spawn failure, or an agent that reported its own
    // failure. A second reduction, with those marked, puts them there.
    let first = reduce(&BTreeMap::new());
    let failed: BTreeMap<SessionId, Unread> = first
        .records
        .iter()
        .filter(|record| record.state == State::Failed)
        .map(|record| (record.session, Unread::FailedSeen))
        .collect();
    let presentation = if failed.is_empty() {
        first
    } else {
        reduce(&failed)
    };
    let scope = req.workspace_id.map_or(Scope::All, Scope::Workspace);
    let in_scope: Vec<&AgentRecord> = presentation.in_scope(scope).collect();
    let by_id: HashMap<SessionId, &SessionInfo> =
        inputs.sessions.iter().map(|s| (s.id, s)).collect();

    let mut coverage = BoardCoverage::default();
    for record in &in_scope {
        match source_of(record, by_id.get(&record.session).copied(), inputs.trees) {
            BoardSource::Journal => coverage.chats += 1,
            BoardSource::Hook => coverage.terminal_reported += 1,
            BoardSource::Recognised => coverage.terminal_recognised += 1,
        }
    }
    let counts = counts_of(Counts::of(in_scope.iter().copied()));

    let mut board = AgentsBoard {
        generated_at_ms: inputs.now_ms,
        counts,
        coverage,
        agents: Vec::new(),
        hidden_finished: 0,
        omitted: 0,
    };
    // The room for the agents: what the answer takes with none, and the commas between them.
    let mut room =
        BOARD_MAX_BYTES.saturating_sub(serde_json::to_vec(&board).map_or(0, |v| v.len()));
    for record in in_scope {
        if record.group == Group::Finished && !req.include_finished {
            board.hidden_finished += 1;
            continue;
        }
        let info = by_id.get(&record.session).copied();
        let agent = agent_of(record, source_of(record, info, inputs.trees));
        let size = serde_json::to_vec(&agent).map_or(usize::MAX, |v| v.len() + 1);
        if board.agents.len() >= BOARD_MAX_AGENTS || size > room {
            // Once one does not fit, the rest are less urgent: none is listed out of order.
            board.omitted += 1;
            room = 0;
            continue;
        }
        room -= size;
        board.agents.push(agent);
    }
    Ok(board)
}

/// The sessions whose finished task waits for a reviewer.
fn awaiting_review(tasks: &[Task]) -> HashSet<SessionId> {
    tasks
        .iter()
        .filter(|task| {
            task.state == TaskState::Done && task.delivery == DeliveryState::AwaitingReview
        })
        .filter_map(|task| task.sessions.last().copied())
        .collect()
}

/// Where the facts of a row come from.
fn source_of(
    record: &AgentRecord,
    info: Option<&SessionInfo>,
    trees: &BTreeMap<SessionId, AgentObservation>,
) -> BoardSource {
    if record.managed {
        return BoardSource::Journal;
    }
    let hooked_report = info
        .and_then(|i| i.agent.as_ref())
        .is_some_and(|report| report.source != OSC_SOURCE);
    let hooked_tree = trees
        .get(&record.session)
        .and_then(|tree| tree.nodes.get(&tree.root))
        .is_some_and(|root| root.source != OSC_SOURCE);
    if hooked_report || hooked_tree {
        BoardSource::Hook
    } else {
        BoardSource::Recognised
    }
}

fn counts_of(counts: Counts) -> BoardCounts {
    BoardCounts {
        agents: counts.sessions,
        requests: counts.request_sessions,
        failed: counts.failed,
        review: counts.review,
        working: counts.working,
        unconfirmed: counts.unconfirmed,
        idle: counts.idle,
        finished: counts.finished,
    }
}

fn agent_of(record: &AgentRecord, source: BoardSource) -> BoardAgent {
    let project = BoardRef {
        id: record.workspace,
        name: cut(&record.project),
    };
    let workspace = record.container_name.as_ref().map(|name| BoardRef {
        id: record.container.unwrap_or(record.workspace),
        name: cut(name),
    });
    BoardAgent {
        session_id: record.session,
        title: text(&record.title),
        role: match record.role {
            Role::Agent => BoardRole::Agent,
            Role::Assistant => BoardRole::Assistant,
        },
        kind: if record.managed {
            BoardKind::Chat
        } else {
            BoardKind::Terminal
        },
        harness: cut(&record.harness),
        model: record.model.as_deref().map(cut),
        project,
        workspace,
        checkout: record.checkout.as_ref().map(|c| BoardCheckout {
            branch: c.branch.as_deref().map(cut),
            worktree: c.worktree,
            shared: c.shared,
        }),
        state: state_of(record.state),
        group: group_of(record.group),
        detail: text(&record.detail),
        waiting: waiting_of(record),
        activity: record
            .activity
            .as_deref()
            .map(text)
            .filter(|t| !t.is_empty()),
        turn_active: record.turn_active,
        started_ms: record.started_at_ms,
        last_event_ms: record.updated_at_ms,
        reported_ms: if record.managed {
            None
        } else {
            record.reported_at_ms
        },
        subagents: BoardSubagents {
            live: record.subagents_live,
            blocked: record.subagents_blocked,
            finished: record.subagents_finished,
        },
        source,
    }
}

/// Everything the agent waits for, the oldest first (a wait of unknown age last).
fn waiting_of(record: &AgentRecord) -> Vec<BoardWaiting> {
    let mut waiting: Vec<BoardWaiting> = Vec::new();
    for request in &record.requests {
        waiting.push(BoardWaiting {
            kind: BoardWaitKind::Approval,
            title: text(&request.title),
            since_ms: Some(request.requested_at_ms),
            ref_id: Some(request.id.to_string()),
        });
    }
    for ask in &record.questions {
        waiting.push(BoardWaiting {
            kind: if ask.is_plan() {
                BoardWaitKind::Plan
            } else {
                BoardWaitKind::Question
            },
            title: text(if ask.is_plan() {
                ask.plan_title()
            } else {
                &ask.title
            }),
            since_ms: Some(ask.requested_at_ms),
            ref_id: Some(ask.id.to_string()),
        });
    }
    // A terminal agent that says it waits for the user, without an object to answer.
    if let Some(reason) = record
        .reasons
        .iter()
        .find(|r| r.kind == ReasonKind::Blocked)
    {
        waiting.push(BoardWaiting {
            kind: BoardWaitKind::TerminalPrompt,
            title: text(&reason.text),
            since_ms: reason.since_ms,
            ref_id: None,
        });
    }
    waiting.sort_by_key(|w| w.since_ms.unwrap_or(u64::MAX));
    waiting
}

fn state_of(state: State) -> BoardState {
    match state {
        State::Working => BoardState::Working,
        State::Blocked => BoardState::Blocked,
        State::Idle => BoardState::Idle,
        State::Unconfirmed => BoardState::Unconfirmed,
        State::Completed => BoardState::Completed,
        State::Failed => BoardState::Failed,
        State::Stopped => BoardState::Stopped,
        State::Interrupted => BoardState::Interrupted,
        State::Disconnected => BoardState::Disconnected,
    }
}

fn group_of(group: Group) -> BoardGroup {
    match group {
        Group::Requests => BoardGroup::Requests,
        Group::Failed => BoardGroup::Failed,
        Group::Review => BoardGroup::Review,
        Group::Working => BoardGroup::Working,
        Group::Unconfirmed => BoardGroup::Unconfirmed,
        Group::Idle => BoardGroup::Idle,
        Group::Snoozed => BoardGroup::Snoozed,
        Group::Finished => BoardGroup::Finished,
    }
}

/// A name or a label: cut, not redacted (ids and names are not free text of a transcript).
fn cut(value: &str) -> String {
    let value = value.trim();
    if value.chars().count() <= BOARD_TEXT_CHARS {
        return value.to_owned();
    }
    format!("{}…", bound(value, BOARD_TEXT_CHARS - 1, Keep::Start).text)
}

/// Free text an agent or a user wrote: one line, redacted, then cut.
fn text(value: &str) -> String {
    let line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    cut(&redact(&line))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use hutan_core::{
        AgentNode, Liveness, NodeKind, NodeOutcome, NodeState, SessionKind, SessionState,
        WorkspaceId,
    };
    use hutan_protocol::messages::{AgentReport, AgentState, WorkspaceKind};

    use super::*;

    fn workspace(name: &str, kind: WorkspaceKind) -> WorkspaceInfo {
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
            parent_id: None,
            project: None,
        }
    }

    fn session(workspace: &WorkspaceInfo, title: &str, source: &str, at: u64) -> SessionInfo {
        SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id: workspace.id,
            kind: SessionKind::Terminal,
            title: title.into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 1,
            agent: Some(AgentReport {
                source: source.into(),
                agent: "omp".into(),
                state: AgentState::Working,
                message: None,
                agent_session_id: None,
                updated_at_ms: at,
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

    fn board(
        workspaces: &[WorkspaceInfo],
        sessions: &[SessionInfo],
        tasks: &[Task],
        req: AgentsBoardRequest,
    ) -> Result<AgentsBoard, ErrorBody> {
        build_board(
            &BoardInputs {
                now_ms: 9,
                workspaces,
                sessions,
                approvals: &[],
                questions: &[],
                trees: &BTreeMap::new(),
                tasks,
            },
            req,
        )
    }

    #[test]
    fn coverage_says_where_each_row_comes_from() {
        let project = workspace("hutan", WorkspaceKind::Project);
        let hooked = session(&project, "hooked", "omp-extension", 10);
        let recognised = session(&project, "recognised", OSC_SOURCE, 20);
        let board = board(
            std::slice::from_ref(&project),
            &[hooked.clone(), recognised.clone()],
            &[],
            AgentsBoardRequest::default(),
        )
        .unwrap();
        let source = |id| {
            board
                .agents
                .iter()
                .find(|a| a.session_id == id)
                .unwrap()
                .source
        };
        assert_eq!(source(hooked.id), BoardSource::Hook);
        assert_eq!(source(recognised.id), BoardSource::Recognised);
        assert_eq!(
            board.coverage,
            BoardCoverage {
                chats: 0,
                terminal_reported: 1,
                terminal_recognised: 1
            }
        );
        assert_eq!(board.counts.agents, 2);
        assert_eq!(board.counts.working, 2);
        // A terminal agent has no journal: nothing is made up for the missing fields.
        let row = &board.agents[0];
        assert_eq!(row.kind, BoardKind::Terminal);
        assert_eq!(row.model, None);
        assert!(!row.turn_active);
    }

    #[test]
    fn a_workspace_narrows_the_board_and_an_unknown_one_is_not_found() {
        let a = workspace("alpha", WorkspaceKind::Project);
        let b = workspace("beta", WorkspaceKind::Project);
        let in_a = session(&a, "in a", "omp-extension", 10);
        let in_b = session(&b, "in b", "omp-extension", 11);
        let both = [a.clone(), b.clone()];
        let sessions = [in_a.clone(), in_b.clone()];
        let all = board(&both, &sessions, &[], AgentsBoardRequest::default()).unwrap();
        assert_eq!(all.agents.len(), 2);
        let only_a = board(
            &both,
            &sessions,
            &[],
            AgentsBoardRequest {
                workspace_id: Some(a.id),
                include_finished: false,
            },
        )
        .unwrap();
        assert_eq!(
            only_a
                .agents
                .iter()
                .map(|x| x.session_id)
                .collect::<Vec<_>>(),
            [in_a.id]
        );
        assert_eq!(only_a.counts.agents, 1, "the counts follow the scope");
        let missing = board(
            &both,
            &sessions,
            &[],
            AgentsBoardRequest {
                workspace_id: Some(WorkspaceId::new()),
                include_finished: false,
            },
        )
        .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }

    #[test]
    fn finished_agents_are_counted_and_listed_only_when_asked() {
        let project = workspace("hutan", WorkspaceKind::Project);
        let live = session(&project, "live", "omp-extension", 10);
        let mut done = session(&project, "done", "omp-extension", 5);
        done.state = SessionState::Exited {
            code: Some(0),
            signal: None,
        };
        let sessions = [live, done.clone()];
        let hidden = board(
            std::slice::from_ref(&project),
            &sessions,
            &[],
            AgentsBoardRequest::default(),
        )
        .unwrap();
        assert_eq!(hidden.agents.len(), 1);
        assert_eq!((hidden.counts.finished, hidden.hidden_finished), (1, 1));
        let shown = board(
            std::slice::from_ref(&project),
            &sessions,
            &[],
            AgentsBoardRequest {
                workspace_id: None,
                include_finished: true,
            },
        )
        .unwrap();
        assert_eq!(shown.agents.len(), 2);
        assert_eq!(shown.hidden_finished, 0);
        let row = shown
            .agents
            .iter()
            .find(|a| a.session_id == done.id)
            .unwrap();
        assert_eq!(
            (row.state, row.group),
            (BoardState::Completed, BoardGroup::Finished)
        );
        assert_eq!(row.detail, "Exited with code 0");
    }

    #[test]
    fn a_failed_session_stays_in_failed() {
        let project = workspace("hutan", WorkspaceKind::Project);
        let mut failed = session(&project, "failed", "omp-extension", 5);
        failed.state = SessionState::Exited {
            code: Some(2),
            signal: None,
        };
        let board = board(
            std::slice::from_ref(&project),
            std::slice::from_ref(&failed),
            &[],
            AgentsBoardRequest::default(),
        )
        .unwrap();
        assert_eq!(board.counts.failed, 1);
        assert_eq!(board.agents[0].group, BoardGroup::Failed);
        assert_eq!(board.agents[0].state, BoardState::Failed);
        assert_eq!(board.agents[0].detail, "Exited with code 2");
    }

    #[test]
    fn an_agent_that_reported_its_own_failure_is_in_failed_while_its_session_runs() {
        let project = workspace("hutan", WorkspaceKind::Project);
        let hooked = session(&project, "flaky", "omp-extension", 5);
        let root = AgentNode {
            pid: None,
            node_id: "root".into(),
            parent: None,
            kind: NodeKind::Main,
            source: "omp-extension".into(),
            agent: "omp".into(),
            name: None,
            agent_type: None,
            state: NodeState::Idle,
            outcome: Some(NodeOutcome::Failed),
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1),
            ended_at_ms: Some(4),
            last_activity_ms: 4,
            activity: None,
            usage: None,
            transcript_path: None,
        };
        let trees = BTreeMap::from([(
            hooked.id,
            AgentObservation {
                session_id: hooked.id,
                root: "root".into(),
                nodes: BTreeMap::from([("root".to_owned(), root)]),
            },
        )]);
        let board = build_board(
            &BoardInputs {
                now_ms: 9,
                workspaces: std::slice::from_ref(&project),
                sessions: std::slice::from_ref(&hooked),
                approvals: &[],
                questions: &[],
                trees: &trees,
                tasks: &[],
            },
            AgentsBoardRequest::default(),
        )
        .unwrap();
        assert_eq!(board.counts.failed, 1);
        assert_eq!(board.agents[0].state, BoardState::Failed);
        assert_eq!(board.agents[0].group, BoardGroup::Failed);
        assert_eq!(board.agents[0].source, BoardSource::Hook);
    }

    #[test]
    fn a_session_whose_finished_task_waits_for_review_is_ready_for_review() {
        use hutan_core::TaskId;

        let project = workspace("hutan", WorkspaceKind::Project);
        let mut waiting = session(&project, "waiting", "omp-extension", 5);
        waiting.state = SessionState::Stopped;
        let task = |delivery, sessions: Vec<SessionId>| Task {
            id: TaskId::new(),
            workspace_id: project.id,
            title: "t".into(),
            prompt: None,
            state: TaskState::Done,
            depends_on: Vec::new(),
            placement: Default::default(),
            run: None,
            auto_start: false,
            sessions,
            created_at_ms: 1,
            started_at_ms: None,
            ended_at_ms: None,
            outcome: None,
            delivery,
            after_policy: Default::default(),
            delivery_history: Vec::new(),
        };
        let sessions = [waiting.clone()];
        let reviewed = board(
            std::slice::from_ref(&project),
            &sessions,
            &[task(DeliveryState::AwaitingReview, vec![waiting.id])],
            AgentsBoardRequest::default(),
        )
        .unwrap();
        assert_eq!(reviewed.counts.review, 1);
        assert_eq!(reviewed.agents[0].group, BoardGroup::Review);
        // A task that is accepted (or one of another session) asks nothing of the reviewer.
        for tasks in [
            vec![task(DeliveryState::Accepted, vec![waiting.id])],
            vec![task(DeliveryState::AwaitingReview, vec![SessionId::new()])],
        ] {
            let other = board(
                std::slice::from_ref(&project),
                &sessions,
                &tasks,
                AgentsBoardRequest::default(),
            )
            .unwrap();
            assert_eq!(other.counts.review, 0);
        }
    }

    #[test]
    fn strings_are_redacted_and_cut_and_the_list_is_bounded() {
        let project = workspace("hutan", WorkspaceKind::Project);
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let long = "x".repeat(BOARD_TEXT_CHARS * 3);
        let mut sessions = Vec::new();
        for n in 0..(BOARD_MAX_AGENTS + 50) {
            let title = match n {
                0 => format!("deploy with {secret} now"),
                1 => long.clone(),
                _ => format!("agent {n}"),
            };
            // The first two are the latest active, so that the cap leaves them listed.
            let at = if n < 2 {
                1_000_000 + n as u64
            } else {
                n as u64 + 1
            };
            sessions.push(session(&project, &title, "omp-extension", at));
        }
        let board = board(
            std::slice::from_ref(&project),
            &sessions,
            &[],
            AgentsBoardRequest::default(),
        )
        .unwrap();
        let json = serde_json::to_string(&board).unwrap();
        assert!(json.len() <= BOARD_MAX_BYTES, "{} bytes", json.len());
        assert!(board.agents.len() <= BOARD_MAX_AGENTS);
        // Counts cover everything in scope, the list only what fits; the rest is said, not hidden.
        assert_eq!(board.counts.agents, BOARD_MAX_AGENTS + 50);
        assert_eq!(board.agents.len() + board.omitted, BOARD_MAX_AGENTS + 50);
        assert!(board.omitted >= 50);
        assert!(!json.contains(secret), "a secret never reaches the board");
        assert!(json.contains("[REDACTED]"));
        let cut = board
            .agents
            .iter()
            .find(|a| a.title.starts_with("xxx"))
            .expect("the long title is listed");
        assert_eq!(cut.title.chars().count(), BOARD_TEXT_CHARS);
        assert!(cut.title.ends_with('…'));
        // What is listed is the most recently active first (the order of the Agents page).
        assert!(board.agents[0].last_event_ms >= board.agents[1].last_event_ms);
    }

    #[test]
    fn an_empty_daemon_has_an_empty_board_with_zero_counts() {
        let board = board(&[], &[], &[], AgentsBoardRequest::default()).unwrap();
        assert!(board.agents.is_empty());
        assert_eq!(board.counts, BoardCounts::default());
        assert_eq!(board.coverage, BoardCoverage::default());
        assert_eq!(board.generated_at_ms, 9);
    }
}
