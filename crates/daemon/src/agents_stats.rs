//! `agents.stats` (protocol 0.66, capability `agents.stats`, ADR 0025): the daemon's answer to
//! "where does the time go, what do agents wait on, what fails again and again".
//!
//! A pure function over what the daemon records — the turn table, the approval and question
//! audit tables, the usage table, the task store, the state log and the session rows — so it can
//! be tested without a daemon. Every number says how well it is known (spike § 2.7):
//!
//! - turns and tasks are exact for chats and tasks; a terminal agent has neither;
//! - waits of chats come from the audit tables (exact); waits of terminal agents only from the
//!   state log, and only as far back as it goes ([`StatsCoverage`]);
//! - retries are inferred (a turn of the same chat soon after a failed one);
//! - tokens are exact where reported, cost is the harness's estimate and absent where none gave
//!   one; token rows are kept 35 days;
//! - the clustering of failures is a heuristic, its counts are exact;
//! - a terminal agent that says it works for hours is flagged *unconfirmed*, never "stuck".
//!
//! A number nobody recorded is absent, not zero. The answer is bounded ([`STATS_MAX_BYTES`]) and
//! every text is redacted and cut.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use hutan_core::task::{Task, TaskState};
use hutan_core::{SessionId, SessionKind, WorkspaceId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::SessionInfo;
use hutan_protocol::stats::{
    AgentsStats, AgentsStatsRequest, FailureCluster, FailureSource, FlagKind, GroupCoverage,
    RETRY_WITHIN_MS, RetryStats, STATS_DEFAULT_GROUPS, STATS_DEFAULT_WINDOW_MS, STATS_MAX_BYTES,
    STATS_MAX_FAILURES, STATS_MAX_FLAGS, STATS_MAX_GROUPS, StatsBy, StatsCoverage, StatsFlag,
    StatsGroup, TaskStats, TokenStats, TurnStats, WaitKind, WaitSide, WaitSource, WaitStats,
};
use hutan_storage::{
    AgentStateRow, ApprovalRow, ChatTurnRow, ChatTurnStatus, QuestionRow, SessionRow, UsageTurnRow,
    WorkspaceRow,
};
use regex::Regex;

use crate::agents_board::text;

/// What this answer cannot say, in words. The same for every answer of this daemon.
pub const NOT_RECORDED: &[&str] = &[
    "active time per turn (only the wall time between a turn's start and end is recorded)",
    "turns, durations and tokens of terminal agents (only chats are journaled)",
    "which tool calls failed (see agent_insight of a chat)",
    "a retry is inferred from turns that follow a failed one, not recorded",
];

/// A turn running this long is flagged.
const LONG_TURN_MS: u64 = 30 * 60 * 1000;
/// A terminal agent that says it works and has said nothing for this long is flagged.
const UNCONFIRMED_AFTER_MS: u64 = 3 * 60 * 60 * 1000;

/// What the answer is built from: everything the daemon records, read for the window.
#[derive(Debug)]
pub struct StatsInputs<'a> {
    pub now_ms: u64,
    pub workspaces: &'a [WorkspaceRow],
    /// Every session row, closed ones included.
    pub sessions: &'a [SessionRow],
    /// The sessions the daemon lists now: where the harness and the live state come from.
    pub infos: &'a [SessionInfo],
    /// Turns that started or ended in the window.
    pub turns: &'a [ChatTurnRow],
    /// Requests asked or resolved in the window, and those still open.
    pub approvals: &'a [ApprovalRow],
    pub questions: &'a [QuestionRow],
    pub usage: &'a [UsageTurnRow],
    /// The state log: what each agent was doing when the window began, and what changed in it.
    pub state_before: &'a [AgentStateRow],
    pub state_window: &'a [AgentStateRow],
    pub state_log_enabled: bool,
    /// The oldest row of the state log.
    pub state_log_from_ms: Option<u64>,
    pub tasks: &'a [Task],
}

/// The window `req` asks for.
pub fn window(req: &AgentsStatsRequest, now_ms: u64) -> (u64, u64) {
    let until = req.until_ms.unwrap_or(now_ms);
    let since = req
        .since_ms
        .unwrap_or_else(|| until.saturating_sub(STATS_DEFAULT_WINDOW_MS));
    (since.min(until), until)
}

/// What one session is, for grouping.
#[derive(Debug, Clone)]
struct Meta {
    title: String,
    workspace: WorkspaceId,
    project: String,
    harness: String,
    chat: bool,
    live: bool,
}

#[derive(Debug, Default, Clone)]
struct WaitAcc {
    count: usize,
    total: u64,
    longest: u64,
    open: usize,
}

impl WaitAcc {
    fn add(&mut self, ms: u64, open: bool) {
        self.count += 1;
        self.total += ms;
        self.longest = self.longest.max(ms);
        self.open += usize::from(open);
    }
}

/// Everything a group collects.
#[derive(Debug, Default, Clone)]
struct Acc {
    chats: BTreeSet<SessionId>,
    terminals: BTreeSet<SessionId>,
    label: String,
    turns: usize,
    failed: usize,
    cancelled: usize,
    wall_total: u64,
    wall_longest: u64,
    retries: usize,
    done: usize,
    tasks_failed: usize,
    tasks_cancelled: usize,
    task_wall: u64,
    task_longest: u64,
    audit: WaitAcc,
    log: WaitAcc,
    by_kind: BTreeMap<String, (usize, u64)>,
    tokens: Option<TokenAcc>,
}

#[derive(Debug, Default, Clone)]
struct TokenAcc {
    turns: usize,
    input: u64,
    cached: u64,
    output: u64,
    total: u64,
    cost: Option<f64>,
}

impl Acc {
    fn merge(&mut self, other: &Acc) {
        self.chats.extend(&other.chats);
        self.terminals.extend(&other.terminals);
        self.turns += other.turns;
        self.failed += other.failed;
        self.cancelled += other.cancelled;
        self.wall_total += other.wall_total;
        self.wall_longest = self.wall_longest.max(other.wall_longest);
        self.retries += other.retries;
        self.done += other.done;
        self.tasks_failed += other.tasks_failed;
        self.tasks_cancelled += other.tasks_cancelled;
        self.task_wall += other.task_wall;
        self.task_longest = self.task_longest.max(other.task_longest);
        for (mine, theirs) in [(&mut self.audit, &other.audit), (&mut self.log, &other.log)] {
            mine.count += theirs.count;
            mine.total += theirs.total;
            mine.longest = mine.longest.max(theirs.longest);
            mine.open += theirs.open;
        }
        for (kind, (count, total)) in &other.by_kind {
            let entry = self.by_kind.entry(kind.clone()).or_default();
            entry.0 += count;
            entry.1 += total;
        }
        if let Some(tokens) = &other.tokens {
            let mine = self.tokens.get_or_insert_with(TokenAcc::default);
            mine.turns += tokens.turns;
            mine.input += tokens.input;
            mine.cached += tokens.cached;
            mine.output += tokens.output;
            mine.total += tokens.total;
            mine.cost = match (mine.cost, tokens.cost) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
        }
    }

    /// How much time the group holds, to put the heavy ones first.
    fn weight(&self) -> u64 {
        self.wall_total + self.task_wall + self.audit.total + self.log.total
    }

    fn group(&self, key: String) -> StatsGroup {
        let by_source: Vec<WaitSide> = [
            (WaitSource::Audit, &self.audit),
            (WaitSource::StateLog, &self.log),
        ]
        .into_iter()
        .filter(|(_, side)| side.count > 0)
        .map(|(source, side)| WaitSide {
            source,
            count: side.count,
            total_ms: side.total,
            longest_ms: side.longest,
            open: side.open,
        })
        .collect();
        let waits = (!by_source.is_empty()).then(|| WaitStats {
            count: self.audit.count + self.log.count,
            total_ms: self.audit.total + self.log.total,
            longest_ms: self.audit.longest.max(self.log.longest),
            by_source,
            by_kind: {
                let mut kinds: Vec<WaitKind> = self
                    .by_kind
                    .iter()
                    .map(|(kind, (count, total))| WaitKind {
                        kind: kind.clone(),
                        count: *count,
                        total_ms: *total,
                    })
                    .collect();
                kinds.sort_by(|a, b| b.total_ms.cmp(&a.total_ms).then(a.kind.cmp(&b.kind)));
                kinds.truncate(8);
                kinds
            },
        });
        StatsGroup {
            key,
            label: self.label.clone(),
            coverage: GroupCoverage {
                chats: self.chats.len(),
                terminal_agents: self.terminals.len(),
            },
            turns: (self.turns > 0).then(|| TurnStats {
                count: self.turns,
                failed: self.failed,
                cancelled: self.cancelled,
                wall_ms_total: self.wall_total,
                wall_ms_longest: self.wall_longest,
                wall_ms_mean: self.wall_total / self.turns as u64,
                active_ms_total: None,
            }),
            tasks: (self.done + self.tasks_failed + self.tasks_cancelled > 0).then_some(
                TaskStats {
                    done: self.done,
                    failed: self.tasks_failed,
                    cancelled: self.tasks_cancelled,
                    wall_ms_total: self.task_wall,
                    wall_ms_longest: self.task_longest,
                },
            ),
            waits,
            retries: (self.retries > 0).then_some(RetryStats {
                count: self.retries,
                inferred: true,
            }),
            tokens: self.tokens.as_ref().map(|t| TokenStats {
                turns_reported: t.turns,
                input: t.input,
                cached_input: t.cached,
                output: t.output,
                total: t.total,
                cost_usd_estimate: t.cost,
            }),
        }
    }
}

/// The statistics of `req` over `inputs`.
pub fn build_stats(
    inputs: &StatsInputs<'_>,
    req: &AgentsStatsRequest,
) -> Result<AgentsStats, ErrorBody> {
    let (since, until) = window(req, inputs.now_ms);
    let in_window = |at: u64| (since..=until).contains(&at);
    let workspaces: HashMap<WorkspaceId, &WorkspaceRow> =
        inputs.workspaces.iter().map(|w| (w.id, w)).collect();
    if let Some(id) = req.workspace_id
        && !workspaces.contains_key(&id)
    {
        return Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no workspace {id}"),
        ));
    }
    let in_scope = |workspace: WorkspaceId| {
        req.workspace_id.is_none_or(|asked| {
            workspace == asked
                || workspaces
                    .get(&workspace)
                    .and_then(|w| w.parent_id)
                    .is_some_and(|parent| parent == asked)
        })
    };

    // What each session is.
    let infos: HashMap<SessionId, &SessionInfo> = inputs.infos.iter().map(|i| (i.id, i)).collect();
    let usage_harness: HashMap<SessionId, &str> = inputs
        .usage
        .iter()
        .map(|row| (row.session_id, row.harness.as_str()))
        .collect();
    let metas: HashMap<SessionId, Meta> = inputs
        .sessions
        .iter()
        .map(|row| {
            let info = infos.get(&row.id);
            let harness = info
                .and_then(|i| i.chat.as_ref().map(|c| c.agent.clone()))
                .or_else(|| usage_harness.get(&row.id).map(|h| (*h).to_owned()))
                .or_else(|| {
                    info.and_then(|i| {
                        i.terminal_agent
                            .as_ref()
                            .map(|a| a.app.clone())
                            .or_else(|| i.agent.as_ref().map(|a| a.agent.clone()))
                    })
                })
                .unwrap_or_else(|| "unknown".to_owned());
            (
                row.id,
                Meta {
                    title: text(&row.title),
                    workspace: row.workspace_id,
                    project: workspaces
                        .get(&row.workspace_id)
                        .map_or_else(|| "Unknown project".to_owned(), |w| text(&w.name)),
                    harness,
                    chat: row.kind != SessionKind::Terminal,
                    live: row.state.is_live(),
                },
            )
        })
        .collect();
    let task_of: HashMap<SessionId, &Task> = inputs
        .tasks
        .iter()
        .flat_map(|task| task.sessions.iter().map(move |s| (*s, task)))
        .collect();

    let mut groups: BTreeMap<String, Acc> = BTreeMap::new();
    let mut total = Acc {
        label: "Everything".to_owned(),
        ..Acc::default()
    };
    let mut failures: HashMap<(String, bool), FailureAcc> = HashMap::new();
    let mut flags: Vec<StatsFlag> = Vec::new();

    // Where a session's numbers at `at_ms` go.
    let place = |session: SessionId, at_ms: u64| -> Option<(String, String)> {
        let meta = metas.get(&session)?;
        Some(match req.group_by {
            StatsBy::Project => (meta.workspace.to_string(), meta.project.clone()),
            StatsBy::Harness => (meta.harness.clone(), meta.harness.clone()),
            StatsBy::Day => {
                let day = day_key(at_ms);
                (day.clone(), format!("{day} (UTC)"))
            }
            StatsBy::Session => (session.to_string(), meta.title.clone()),
            StatsBy::Task => match task_of.get(&session) {
                Some(task) => (task.id.to_string(), text(&task.title)),
                None => ("none".to_owned(), "No task".to_owned()),
            },
        })
    };
    let mut put = |session: SessionId, at_ms: u64, apply: &mut dyn FnMut(&mut Acc)| {
        let Some(meta) = metas.get(&session) else {
            return;
        };
        if !in_scope(meta.workspace) {
            return;
        }
        let Some((key, label)) = place(session, at_ms) else {
            return;
        };
        let entry = groups.entry(key).or_default();
        entry.label = label;
        let sessions = if meta.chat {
            &mut entry.chats
        } else {
            &mut entry.terminals
        };
        sessions.insert(session);
        apply(entry);
        // The total is the sum of the same changes: it never disagrees with its groups.
        let mut delta = Acc::default();
        apply(&mut delta);
        let kind_set = if meta.chat {
            &mut total.chats
        } else {
            &mut total.terminals
        };
        kind_set.insert(session);
        total.merge(&delta);
    };

    // Turns: wall time, failures, retries.
    let mut by_session: HashMap<SessionId, Vec<&ChatTurnRow>> = HashMap::new();
    for turn in inputs.turns {
        by_session.entry(turn.session_id).or_default().push(turn);
    }
    for (session, turns) in &mut by_session {
        turns.sort_by_key(|t| (t.started_at_ms, t.turn_id.clone()));
        for (index, turn) in turns.iter().enumerate() {
            let Some(ended) = turn.ended_at_ms else {
                // A turn that runs: a long one is worth a look (exact for chats).
                if turn.status == ChatTurnStatus::Running
                    && let Some(meta) = metas.get(session)
                    && meta.live
                    && in_scope(meta.workspace)
                    && until.saturating_sub(turn.started_at_ms) >= LONG_TURN_MS
                    && turn.started_at_ms <= until
                {
                    flags.push(StatsFlag {
                        session_id: *session,
                        title: meta.title.clone(),
                        kind: FlagKind::LongTurn,
                        since_ms: turn.started_at_ms,
                        detail: format!(
                            "a turn has run for {} minutes",
                            until.saturating_sub(turn.started_at_ms) / 60_000
                        ),
                    });
                }
                continue;
            };
            if !in_window(ended) || turn.status == ChatTurnStatus::Running {
                continue;
            }
            let wall = ended.saturating_sub(turn.started_at_ms);
            let retried = turn.status == ChatTurnStatus::Failed
                && turns[index + 1..]
                    .iter()
                    .any(|next| (ended..=ended + RETRY_WITHIN_MS).contains(&next.started_at_ms));
            put(*session, ended, &mut |acc| {
                acc.turns += 1;
                match turn.status {
                    ChatTurnStatus::Failed => acc.failed += 1,
                    ChatTurnStatus::Cancelled => acc.cancelled += 1,
                    _ => {}
                }
                acc.wall_total += wall;
                acc.wall_longest = acc.wall_longest.max(wall);
                acc.retries += usize::from(retried);
            });
            if turn.status == ChatTurnStatus::Failed
                && let Some(error) = turn.error.as_deref().filter(|e| !e.trim().is_empty())
                && metas.get(session).is_some_and(|m| in_scope(m.workspace))
            {
                failures
                    .entry((normalise(error), false))
                    .or_default()
                    .note(*session, ended);
            }
        }
    }

    // Waits of chats: the audit tables.
    let mut seen_requests: BTreeSet<String> = BTreeSet::new();
    let mut waits: Vec<(SessionId, u64, u64, bool, String)> = Vec::new();
    let mut request =
        |id: String, session: SessionId, asked: u64, resolved: Option<u64>, kind: String| {
            if !seen_requests.insert(id) {
                return;
            }
            let Some(meta) = metas.get(&session) else {
                return;
            };
            match resolved.filter(|ms| *ms <= until) {
                Some(done) if in_window(done) => {
                    waits.push((session, done, done.saturating_sub(asked), false, kind));
                }
                Some(_) => {}
                // Not answered by the window's end: a wait that goes on, for a session that lives.
                None if meta.live && asked <= until => {
                    waits.push((session, until, until.saturating_sub(asked), true, kind));
                }
                None => {}
            }
        };
    for row in inputs.approvals {
        request(
            row.id.to_string(),
            row.session_id,
            row.requested_at_ms,
            row.resolved_at_ms,
            if row.kind.is_empty() {
                "approval".to_owned()
            } else {
                text(&row.kind)
            },
        );
    }
    for row in inputs.questions {
        let kind = if hutan_agents_view::presentation::is_plan_title(&row.title) {
            "plan"
        } else {
            "question"
        };
        request(
            row.id.to_string(),
            row.session_id,
            row.requested_at_ms,
            row.resolved_at_ms,
            kind.to_owned(),
        );
    }
    for (session, at, ms, open, kind) in waits {
        put(session, at, &mut |acc| {
            acc.audit.add(ms, open);
            let entry = acc.by_kind.entry(kind.clone()).or_default();
            entry.0 += 1;
            entry.1 += ms;
        });
    }

    // Waits of terminal agents: spans of `blocked` in the state log.
    let mut rows_by_session: BTreeMap<SessionId, Vec<(u64, &AgentStateRow)>> = BTreeMap::new();
    for row in inputs.state_before {
        rows_by_session
            .entry(row.session_id)
            .or_default()
            .push((since, row));
    }
    for row in inputs.state_window {
        rows_by_session
            .entry(row.session_id)
            .or_default()
            .push((row.at_ms, row));
    }
    for (session, rows) in &mut rows_by_session {
        let Some(meta) = metas.get(session).filter(|m| !m.chat) else {
            continue;
        };
        // An agent that says the state it is already in (after a restart, or through another
        // source) has not changed state: one wait, one failure (the same words, in a row).
        rows.dedup_by(|next, prev| {
            next.1.state == prev.1.state
                && (next.1.state == "blocked" || next.1.message == prev.1.message)
        });
        for (index, (start, row)) in rows.iter().enumerate() {
            match row.state.as_str() {
                "blocked" => {
                    let next = rows.get(index + 1).map(|(at, _)| *at);
                    // An agent that has not left the state is waiting up to the window's end —
                    // if its session still lives.
                    let (end, open) = match next {
                        Some(at) => (at, false),
                        None if meta.live => (until, true),
                        None => continue,
                    };
                    let ms = end.saturating_sub(*start);
                    let kind = row
                        .kind
                        .as_deref()
                        .filter(|k| !k.is_empty())
                        .map_or_else(|| "terminal_prompt".to_owned(), text);
                    put(*session, *start, &mut |acc| {
                        acc.log.add(ms, open);
                        let entry = acc.by_kind.entry(kind.clone()).or_default();
                        entry.0 += 1;
                        entry.1 += ms;
                    });
                }
                "error" if in_window(row.at_ms) => {
                    put(*session, row.at_ms, &mut |_| {});
                    if let Some(message) = row.message.as_deref().filter(|m| !m.trim().is_empty()) {
                        failures
                            .entry((normalise(message), true))
                            .or_default()
                            .note(*session, row.at_ms);
                    }
                }
                _ => {}
            }
        }
    }

    // Tokens of the turns that reported them.
    for row in inputs.usage.iter().filter(|r| in_window(r.at_ms)) {
        put(row.session_id, row.at_ms, &mut |acc| {
            let tokens = acc.tokens.get_or_insert_with(TokenAcc::default);
            tokens.turns += 1;
            tokens.input += row.input_tokens.unwrap_or(0);
            tokens.cached += row.cached_input_tokens.unwrap_or(0);
            tokens.output += row.output_tokens.unwrap_or(0);
            tokens.total += row
                .total_tokens
                .or_else(|| Some(row.input_tokens? + row.output_tokens?))
                .unwrap_or(0);
            tokens.cost = match (tokens.cost, row.cost_usd) {
                (Some(a), Some(b)) => Some(a + b),
                (a, b) => a.or(b),
            };
        });
    }

    // Tasks that ended: exact, a task has a start and an end.
    for task in inputs.tasks {
        let Some(ended) = task.ended_at_ms.filter(|ms| in_window(*ms)) else {
            continue;
        };
        let Some(started) = task.started_at_ms else {
            continue;
        };
        let wall = ended.saturating_sub(started);
        // A task has a project of its own; its last session is where its harness is.
        let session = task.sessions.last().copied();
        let Some(workspace) = workspaces.get(&task.workspace_id) else {
            continue;
        };
        if !in_scope(workspace.id) {
            continue;
        }
        let (key, label) = match req.group_by {
            StatsBy::Project => (workspace.id.to_string(), text(&workspace.name)),
            StatsBy::Harness => {
                let harness = session
                    .and_then(|s| metas.get(&s))
                    .map_or_else(|| "unknown".to_owned(), |m| m.harness.clone());
                (harness.clone(), harness)
            }
            StatsBy::Day => {
                let day = day_key(ended);
                (day.clone(), format!("{day} (UTC)"))
            }
            StatsBy::Session => match session.and_then(|s| metas.get(&s).map(|m| (s, m))) {
                Some((s, m)) => (s.to_string(), m.title.clone()),
                None => continue,
            },
            StatsBy::Task => (task.id.to_string(), text(&task.title)),
        };
        let mut delta = Acc::default();
        match task.state {
            TaskState::Done => delta.done = 1,
            TaskState::Failed => delta.tasks_failed = 1,
            TaskState::Cancelled => delta.tasks_cancelled = 1,
            _ => continue,
        }
        delta.task_wall = wall;
        delta.task_longest = wall;
        let entry = groups.entry(key).or_default();
        if entry.label.is_empty() {
            entry.label = label;
        }
        entry.merge(&delta);
        total.merge(&delta);
    }

    // Terminal agents that say they work and say nothing else.
    for info in inputs.infos.iter().filter(|i| i.state.is_live()) {
        let Some(meta) = metas
            .get(&info.id)
            .filter(|m| !m.chat && in_scope(m.workspace))
        else {
            continue;
        };
        let working_since = info
            .agent
            .as_ref()
            .filter(|a| a.state == hutan_protocol::messages::AgentState::Working)
            .map(|a| a.updated_at_ms)
            .or_else(|| {
                info.terminal_agent
                    .as_ref()
                    .filter(|a| a.state == hutan_protocol::messages::TerminalAgentState::Working)
                    .map(|a| a.since_ms)
            });
        if let Some(since_ms) =
            working_since.filter(|at| until.saturating_sub(*at) >= UNCONFIRMED_AFTER_MS)
        {
            flags.push(StatsFlag {
                session_id: info.id,
                title: meta.title.clone(),
                kind: FlagKind::UnconfirmedWorking,
                since_ms,
                detail: format!(
                    "says it works since {} hours; nothing else is known, so its state is unconfirmed",
                    until.saturating_sub(since_ms) / 3_600_000
                ),
            });
        }
    }
    flags.sort_by_key(|f| f.since_ms);
    flags.truncate(STATS_MAX_FLAGS);

    // Failures that came again.
    let mut clusters: Vec<FailureCluster> = failures
        .into_iter()
        .filter(|(_, acc)| acc.count >= 2)
        .map(|((pattern, terminal), acc)| FailureCluster {
            pattern: text(&pattern),
            count: acc.count,
            sessions: acc.sessions.len(),
            last_ms: acc.last,
            source: if terminal {
                FailureSource::TerminalError
            } else {
                FailureSource::TurnError
            },
        })
        .collect();
    clusters.sort_by(|a, b| b.count.cmp(&a.count).then(a.pattern.cmp(&b.pattern)));
    clusters.truncate(STATS_MAX_FAILURES);

    // The groups: the heaviest first (a day: the newest first), as many as fit.
    let mut ordered: Vec<(String, Acc)> = groups.into_iter().collect();
    match req.group_by {
        StatsBy::Day => ordered.sort_by(|a, b| b.0.cmp(&a.0)),
        _ => ordered.sort_by(|a, b| b.1.weight().cmp(&a.1.weight()).then(a.0.cmp(&b.0))),
    }
    let limit = req
        .limit
        .unwrap_or(STATS_DEFAULT_GROUPS)
        .clamp(1, STATS_MAX_GROUPS as u32) as usize;
    let mut answer = AgentsStats {
        from_ms: since,
        to_ms: until,
        group_by: req.group_by,
        coverage: StatsCoverage {
            state_log: inputs.state_log_enabled,
            state_log_from_ms: inputs.state_log_from_ms,
            tokens_from_ms: Some(
                inputs
                    .now_ms
                    .saturating_sub(hutan_storage::USAGE_KEPT_DAYS * 86_400_000),
            ),
            chats: total.chats.len(),
            terminal_agents: total.terminals.len(),
        },
        total: total.group("all".to_owned()),
        groups: Vec::new(),
        omitted_groups: 0,
        failures: clusters,
        flags,
        not_recorded: NOT_RECORDED.iter().map(|s| (*s).to_owned()).collect(),
    };
    let mut room =
        STATS_MAX_BYTES.saturating_sub(serde_json::to_vec(&answer).map_or(0, |v| v.len()) + 16);
    let count = ordered.len();
    for (key, acc) in ordered {
        let group = acc.group(key);
        let size = serde_json::to_vec(&group).map_or(usize::MAX, |v| v.len() + 1);
        if answer.groups.len() >= limit || size > room {
            room = 0;
            continue;
        }
        room -= size;
        answer.groups.push(group);
    }
    answer.omitted_groups = count - answer.groups.len();
    Ok(answer)
}

/// How often one failure came, and where.
#[derive(Debug, Default)]
struct FailureAcc {
    count: usize,
    sessions: BTreeSet<SessionId>,
    last: u64,
}

impl FailureAcc {
    fn note(&mut self, session: SessionId, at_ms: u64) {
        self.count += 1;
        self.sessions.insert(session);
        self.last = self.last.max(at_ms);
    }
}

static URL: LazyLock<Regex> = LazyLock::new(|| compile(r"https?://\S+"));
static PATH: LazyLock<Regex> = LazyLock::new(|| compile(r#"(?:~|\.{1,2})?/[^\s:;,)"']+"#));
static ULID: LazyLock<Regex> = LazyLock::new(|| compile(r"\b[0-9a-hjkmnp-tv-z]{26}\b"));
static HEX: LazyLock<Regex> = LazyLock::new(|| compile(r"\b[0-9a-f]{8,}\b"));
static NUMBER: LazyLock<Regex> = LazyLock::new(|| compile(r"\d+"));

#[allow(clippy::expect_used)] // The patterns are constants covered by the unit tests.
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant pattern")
}

/// A failure as it reads once what differs between two of the same kind is stripped: urls,
/// paths, ids and numbers. A heuristic: it joins what reads alike, nothing more.
pub fn normalise(error: &str) -> String {
    let lower =
        hutan_index::redact::redact(&error.split_whitespace().collect::<Vec<_>>().join(" "))
            .to_lowercase();
    let stripped = URL.replace_all(&lower, "<url>");
    let stripped = PATH.replace_all(&stripped, "<path>");
    let stripped = ULID.replace_all(&stripped, "<id>");
    let stripped = HEX.replace_all(&stripped, "<id>");
    let stripped = NUMBER.replace_all(&stripped, "#");
    stripped.chars().take(120).collect()
}

/// The UTC calendar day of `ms`, `2026-10-30`.
pub fn day_key(ms: u64) -> String {
    let days = i64::try_from(ms / 86_400_000).unwrap_or(0);
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // a world is filled in as a test needs it
mod tests {
    use super::*;

    use hutan_core::task::{DeliveryState, TaskPlacement};
    use hutan_core::{ApprovalId, QuestionId, SessionState, TaskId};
    use hutan_protocol::messages::{AgentReport, AgentState};
    use hutan_storage::{ApprovalOutcome, CreatedBy, QuestionOutcome, WorkspaceKind};

    const T0: u64 = 1_800_000_000_000;
    const MIN: u64 = 60_000;

    fn workspace(name: &str) -> WorkspaceRow {
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
            parent_id: None,
        }
    }

    fn session(workspace: &WorkspaceRow, title: &str, kind: SessionKind) -> SessionRow {
        SessionRow {
            id: SessionId::new(),
            workspace_id: workspace.id,
            kind,
            title: title.into(),
            title_locked: false,
            spec_json: "{}".into(),
            cwd: None,
            state: SessionState::Idle,
            created_at_ms: T0 - 1_000_000,
            ended_at_ms: None,
            resume_ref_json: None,
        }
    }

    fn turn(
        session: &SessionRow,
        n: u64,
        status: ChatTurnStatus,
        started: u64,
        ended: Option<u64>,
        error: Option<&str>,
    ) -> ChatTurnRow {
        ChatTurnRow {
            session_id: session.id,
            turn_id: format!("t{n}"),
            provider_turn_id: None,
            status,
            started_at_ms: started,
            ended_at_ms: ended,
            error: error.map(str::to_owned),
        }
    }

    fn approval(
        session: &SessionRow,
        kind: &str,
        asked: u64,
        resolved: Option<u64>,
    ) -> ApprovalRow {
        ApprovalRow {
            id: ApprovalId::new(),
            session_id: session.id,
            turn_id: None,
            kind: kind.into(),
            title: "run it".into(),
            options_json: "[]".into(),
            requested_at_ms: asked,
            resolved_at_ms: resolved,
            outcome: resolved.map(|_| ApprovalOutcome::Selected),
            option_id: None,
            decided_by: None,
        }
    }

    fn usage(
        session: &SessionRow,
        at: u64,
        harness: &str,
        tokens: u64,
        cost: Option<f64>,
    ) -> UsageTurnRow {
        UsageTurnRow {
            session_id: session.id,
            turn_id: format!("u{at}"),
            at_ms: at,
            harness: harness.into(),
            provider: "anthropic".into(),
            model: None,
            input_tokens: Some(tokens),
            cached_input_tokens: Some(tokens / 2),
            output_tokens: Some(10),
            reasoning_output_tokens: None,
            total_tokens: Some(tokens + 10),
            cost_usd: cost,
        }
    }

    fn state(
        session: &SessionRow,
        at: u64,
        state: &str,
        kind: Option<&str>,
        message: Option<&str>,
    ) -> AgentStateRow {
        AgentStateRow {
            session_id: session.id,
            at_ms: at,
            source: "osc7501".into(),
            state: state.into(),
            kind: kind.map(str::to_owned),
            message: message.map(str::to_owned),
        }
    }

    fn task(
        workspace: &WorkspaceRow,
        title: &str,
        session: &SessionRow,
        started: u64,
        ended: u64,
        state: TaskState,
    ) -> Task {
        Task {
            id: TaskId::new(),
            workspace_id: workspace.id,
            title: title.into(),
            prompt: None,
            state,
            depends_on: Vec::new(),
            placement: TaskPlacement::default(),
            run: None,
            auto_start: false,
            sessions: vec![session.id],
            created_at_ms: started,
            started_at_ms: Some(started),
            ended_at_ms: Some(ended),
            outcome: None,
            delivery: DeliveryState::None,
            after_policy: Default::default(),
            delivery_history: Vec::new(),
        }
    }

    /// Everything empty; tests fill in what they need.
    #[derive(Default)]
    struct World {
        workspaces: Vec<WorkspaceRow>,
        sessions: Vec<SessionRow>,
        infos: Vec<SessionInfo>,
        turns: Vec<ChatTurnRow>,
        approvals: Vec<ApprovalRow>,
        questions: Vec<QuestionRow>,
        usage: Vec<UsageTurnRow>,
        before: Vec<AgentStateRow>,
        log: Vec<AgentStateRow>,
        tasks: Vec<Task>,
        log_on: bool,
    }

    impl World {
        fn stats(&self, req: AgentsStatsRequest) -> Result<AgentsStats, ErrorBody> {
            build_stats(
                &StatsInputs {
                    now_ms: T0 + 60 * MIN,
                    workspaces: &self.workspaces,
                    sessions: &self.sessions,
                    infos: &self.infos,
                    turns: &self.turns,
                    approvals: &self.approvals,
                    questions: &self.questions,
                    usage: &self.usage,
                    state_before: &self.before,
                    state_window: &self.log,
                    state_log_enabled: self.log_on,
                    state_log_from_ms: self.log.first().map(|r| r.at_ms),
                    tasks: &self.tasks,
                },
                &req,
            )
        }
    }

    fn week() -> AgentsStatsRequest {
        AgentsStatsRequest {
            since_ms: Some(T0 - 24 * 60 * MIN),
            ..Default::default()
        }
    }

    fn group<'a>(stats: &'a AgentsStats, label: &str) -> &'a StatsGroup {
        stats
            .groups
            .iter()
            .find(|g| g.label == label)
            .unwrap_or_else(|| panic!("no group {label}: {:?}", stats.groups))
    }

    #[test]
    fn turns_waits_tokens_and_tasks_land_in_their_project_with_their_coverage() {
        let (hutan, shop) = (workspace("hutan"), workspace("shop"));
        let chat = session(&hutan, "chat", SessionKind::Harness);
        let terminal = session(&hutan, "terminal", SessionKind::Terminal);
        let other = session(&shop, "other", SessionKind::Harness);
        let mut w = World {
            log_on: true,
            ..World::default()
        };
        w.turns = vec![
            turn(
                &chat,
                1,
                ChatTurnStatus::Completed,
                T0,
                Some(T0 + MIN),
                None,
            ),
            turn(
                &chat,
                2,
                ChatTurnStatus::Failed,
                T0 + 5 * MIN,
                Some(T0 + 6 * MIN),
                Some("Rate limited"),
            ),
            // A turn of the same chat soon after the failed one: taken for its retry.
            turn(
                &chat,
                3,
                ChatTurnStatus::Completed,
                T0 + 8 * MIN,
                Some(T0 + 11 * MIN),
                None,
            ),
        ];
        w.approvals = vec![approval(&chat, "command", T0 + MIN, Some(T0 + 3 * MIN))];
        w.questions = vec![QuestionRow {
            id: QuestionId::new(),
            session_id: chat.id,
            turn_id: None,
            title: "Which branch?".into(),
            requested_at_ms: T0 + 12 * MIN,
            resolved_at_ms: Some(T0 + 13 * MIN),
            outcome: Some(QuestionOutcome::Answered),
            decided_by: Some("desktop".into()),
        }];
        w.usage = vec![
            usage(&chat, T0 + MIN, "omp", 1_000, Some(0.05)),
            usage(&chat, T0 + 6 * MIN, "omp", 2_000, None),
            usage(&other, T0 + MIN, "codex", 500, None),
        ];
        // The terminal agent blocked for 5 minutes, worked, then blocked again and still waits.
        w.log = vec![
            state(
                &terminal,
                T0 + 20 * MIN,
                "blocked",
                Some("permission"),
                Some("Run the suite?"),
            ),
            state(&terminal, T0 + 25 * MIN, "working", None, None),
            state(&terminal, T0 + 40 * MIN, "blocked", None, None),
        ];
        let mut live = terminal.clone();
        live.state = SessionState::Running;
        w.sessions = vec![chat.clone(), live, other.clone()];
        w.workspaces = vec![hutan.clone(), shop.clone()];
        w.tasks = vec![task(
            &hutan,
            "Add the card",
            &chat,
            T0,
            T0 + 10 * MIN,
            TaskState::Done,
        )];

        let stats = w.stats(week()).unwrap();
        let mine = group(&stats, "hutan");
        assert_eq!(
            mine.coverage,
            GroupCoverage {
                chats: 1,
                terminal_agents: 1
            }
        );
        let turns = mine.turns.unwrap();
        assert_eq!((turns.count, turns.failed), (3, 1));
        assert_eq!(turns.wall_ms_total, 5 * MIN);
        assert_eq!(turns.wall_ms_longest, 3 * MIN);
        assert_eq!(turns.active_ms_total, None, "active time is not recorded");
        assert_eq!(
            mine.retries,
            Some(RetryStats {
                count: 1,
                inferred: true
            })
        );
        assert_eq!(mine.tasks.unwrap().done, 1);
        assert_eq!(mine.tasks.unwrap().wall_ms_total, 10 * MIN);

        let waits = mine.waits.as_ref().unwrap();
        // Two answered waits of the chat (exact) and two of the terminal agent (state log, one
        // open at the window's end).
        assert_eq!(waits.count, 4);
        let audit = waits
            .by_source
            .iter()
            .find(|s| s.source == WaitSource::Audit)
            .unwrap();
        assert_eq!(
            (audit.count, audit.total_ms, audit.longest_ms, audit.open),
            (2, 3 * MIN, 2 * MIN, 0)
        );
        let log = waits
            .by_source
            .iter()
            .find(|s| s.source == WaitSource::StateLog)
            .unwrap();
        assert_eq!((log.count, log.open), (2, 1));
        assert_eq!(log.longest_ms, 20 * MIN, "open until the end of the window");
        let kinds: Vec<&str> = waits.by_kind.iter().map(|k| k.kind.as_str()).collect();
        assert!(kinds.contains(&"command") && kinds.contains(&"question"));
        assert!(kinds.contains(&"permission") && kinds.contains(&"terminal_prompt"));

        // Tokens are exact; the cost is the estimate of the turns that gave one.
        let tokens = mine.tokens.unwrap();
        assert_eq!(
            (tokens.turns_reported, tokens.input, tokens.output),
            (2, 3_000, 20)
        );
        assert_eq!(tokens.cost_usd_estimate, Some(0.05));
        // The other project reported tokens and no cost: absent, not zero.
        let theirs = group(&stats, "shop");
        assert_eq!(theirs.tokens.unwrap().cost_usd_estimate, None);
        assert!(theirs.turns.is_none() && theirs.waits.is_none() && theirs.retries.is_none());

        // The total is the sum of the groups.
        assert_eq!(stats.total.turns.unwrap().count, 3);
        assert_eq!(stats.total.tokens.unwrap().turns_reported, 3);
        assert_eq!(
            stats.total.coverage,
            GroupCoverage {
                chats: 2,
                terminal_agents: 1
            }
        );
        assert_eq!(
            (stats.coverage.chats, stats.coverage.terminal_agents),
            (2, 1)
        );
        assert!(
            stats.coverage.state_log && stats.coverage.state_log_from_ms == Some(T0 + 20 * MIN)
        );
        assert!(!stats.not_recorded.is_empty());
    }

    #[test]
    fn the_same_state_said_again_is_one_wait_and_gone_ends_it() {
        let hutan = workspace("hutan");
        let terminal = session(&hutan, "terminal", SessionKind::Terminal);
        let mut w = World {
            log_on: true,
            ..World::default()
        };
        w.log = vec![
            state(&terminal, T0 + 10 * MIN, "blocked", Some("question"), None),
            // Said again (another source, or after a restart): the same wait.
            state(&terminal, T0 + 20 * MIN, "blocked", Some("question"), None),
            state(&terminal, T0 + 30 * MIN, "gone", None, None),
        ];
        w.sessions = vec![terminal];
        w.workspaces = vec![hutan];
        let waits = w.stats(week()).unwrap().total.waits.unwrap();
        assert_eq!((waits.count, waits.total_ms), (1, 20 * MIN), "{waits:?}");
        assert_eq!(waits.by_source[0].open, 0);
    }

    #[test]
    fn without_the_state_log_a_terminal_agents_waits_are_not_recorded_not_zero() {
        let hutan = workspace("hutan");
        let terminal = session(&hutan, "terminal", SessionKind::Terminal);
        let mut w = World::default();
        w.sessions = vec![terminal];
        w.workspaces = vec![hutan];
        let stats = w.stats(week()).unwrap();
        assert!(!stats.coverage.state_log);
        assert!(stats.groups.is_empty(), "nothing recorded makes no group");
        assert!(stats.total.waits.is_none());
        assert_eq!(
            stats.total.coverage,
            GroupCoverage {
                chats: 0,
                terminal_agents: 0
            }
        );
    }

    #[test]
    fn a_wait_that_began_before_the_window_counts_from_what_the_log_said_then() {
        let hutan = workspace("hutan");
        let terminal = session(&hutan, "terminal", SessionKind::Terminal);
        let mut w = World {
            log_on: true,
            ..World::default()
        };
        // Blocked since before the window; answered 10 minutes into it.
        w.before = vec![state(
            &terminal,
            T0 - 90 * MIN,
            "blocked",
            Some("question"),
            None,
        )];
        w.log = vec![state(&terminal, T0 + 10 * MIN, "working", None, None)];
        w.sessions = vec![terminal];
        w.workspaces = vec![hutan];
        let stats = w
            .stats(AgentsStatsRequest {
                since_ms: Some(T0),
                ..Default::default()
            })
            .unwrap();
        let waits = stats.total.waits.unwrap();
        assert_eq!(
            (waits.count, waits.total_ms),
            (1, 10 * MIN),
            "only the part inside the window"
        );
        assert_eq!(waits.by_kind[0].kind, "question");
    }

    #[test]
    fn groups_are_made_by_harness_day_session_and_task() {
        let hutan = workspace("hutan");
        let chat = session(&hutan, "chat", SessionKind::Harness);
        let mut w = World::default();
        w.turns = vec![turn(
            &chat,
            1,
            ChatTurnStatus::Completed,
            T0,
            Some(T0 + MIN),
            None,
        )];
        w.usage = vec![usage(&chat, T0 + MIN, "omp", 100, None)];
        w.tasks = vec![task(
            &hutan,
            "Add the card",
            &chat,
            T0,
            T0 + 2 * MIN,
            TaskState::Failed,
        )];
        w.sessions = vec![chat.clone()];
        w.workspaces = vec![hutan];
        let by = |group_by| w.stats(AgentsStatsRequest { group_by, ..week() }).unwrap();
        let harness = by(StatsBy::Harness);
        assert_eq!(harness.groups.len(), 1);
        assert_eq!(
            harness.groups[0].key, "omp",
            "from the turns that reported usage"
        );
        let day = by(StatsBy::Day);
        assert_eq!(
            day.groups[0].key,
            day_key(T0 + 2 * MIN)
                .as_str()
                .to_owned()
                .as_str()
                .to_owned()
        );
        assert!(day.groups[0].label.ends_with("(UTC)"));
        let session = by(StatsBy::Session);
        assert_eq!(session.groups[0].key, chat.id.to_string());
        assert_eq!(session.groups[0].label, "chat");
        let task = by(StatsBy::Task);
        assert_eq!(task.groups[0].label, "Add the card");
        assert_eq!(task.groups[0].tasks.unwrap().failed, 1);
        assert_eq!(
            task.groups[0].turns.unwrap().count,
            1,
            "the turns of the task's session"
        );
    }

    #[test]
    fn a_failure_that_came_again_is_a_cluster_and_one_that_came_once_is_not() {
        let hutan = workspace("hutan");
        let (a, b) = (
            session(&hutan, "a", SessionKind::Harness),
            session(&hutan, "b", SessionKind::Harness),
        );
        let terminal = session(&hutan, "t", SessionKind::Terminal);
        let mut w = World {
            log_on: true,
            ..World::default()
        };
        w.turns = vec![
            turn(
                &a,
                1,
                ChatTurnStatus::Failed,
                T0,
                Some(T0 + MIN),
                Some("Rate limited: retry in 40 s"),
            ),
            turn(
                &a,
                2,
                ChatTurnStatus::Failed,
                T0 + 30 * MIN,
                Some(T0 + 31 * MIN),
                Some("Rate limited: retry in 12 s"),
            ),
            turn(
                &b,
                1,
                ChatTurnStatus::Failed,
                T0 + 2 * MIN,
                Some(T0 + 3 * MIN),
                Some("rate limited: retry in 7 s"),
            ),
            turn(
                &b,
                2,
                ChatTurnStatus::Failed,
                T0 + 40 * MIN,
                Some(T0 + 41 * MIN),
                Some("context window exceeded"),
            ),
        ];
        w.log = vec![
            state(
                &terminal,
                T0 + 5 * MIN,
                "error",
                None,
                Some("npm ERR! code 1"),
            ),
            state(
                &terminal,
                T0 + 50 * MIN,
                "error",
                None,
                Some("npm ERR! code 127"),
            ),
            state(&terminal, T0 + 55 * MIN, "error", None, Some("killed")),
        ];
        w.sessions = vec![a, b, terminal];
        w.workspaces = vec![hutan];
        let stats = w.stats(week()).unwrap();
        assert_eq!(stats.failures.len(), 2, "{:?}", stats.failures);
        let rate = &stats.failures[0];
        assert_eq!(rate.pattern, "rate limited: retry in # s");
        assert_eq!((rate.count, rate.sessions), (3, 2));
        assert_eq!(rate.source, FailureSource::TurnError);
        assert_eq!(rate.last_ms, T0 + 31 * MIN);
        // The terminal agent's error came once as another text and once alike: its own cluster.
        let terminal_error = &stats.failures[1];
        assert_eq!(terminal_error.source, FailureSource::TerminalError);
        assert_eq!(terminal_error.pattern, "npm err! code #");
        assert_eq!((terminal_error.count, terminal_error.sessions), (2, 1));
        assert!(
            stats.failures.iter().all(|f| f.pattern != "killed"),
            "one that came once is no cluster"
        );
    }

    #[test]
    fn long_turns_and_terminal_agents_that_only_say_they_work_are_flagged_with_their_certainty() {
        let hutan = workspace("hutan");
        let mut chat = session(&hutan, "chat", SessionKind::Harness);
        chat.state = SessionState::Running;
        let mut terminal = session(&hutan, "terminal", SessionKind::Terminal);
        terminal.state = SessionState::Running;
        let mut info = hutan_protocol::messages::SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: terminal.id,
            workspace_id: hutan.id,
            kind: SessionKind::Terminal,
            title: "terminal".into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 1,
            agent: Some(AgentReport {
                source: "omp-extension".into(),
                agent: "omp".into(),
                state: AgentState::Working,
                message: None,
                agent_session_id: None,
                updated_at_ms: T0 - 4 * 60 * MIN,
                blocked_sub: None,
            }),
            restored: false,
            chat: None,
            placement: None,
            folder: None,
            recovery: None,
            muted: false,
        };
        info.agent.as_mut().unwrap().updated_at_ms = T0 - 4 * 60 * MIN;
        let mut w = World::default();
        w.turns = vec![turn(
            &chat,
            1,
            ChatTurnStatus::Running,
            T0 - 50 * MIN,
            None,
            None,
        )];
        w.infos = vec![info];
        w.sessions = vec![chat.clone(), terminal.clone()];
        w.workspaces = vec![hutan];
        let stats = w
            .stats(AgentsStatsRequest {
                since_ms: Some(T0 - 24 * 60 * MIN),
                until_ms: Some(T0),
                ..Default::default()
            })
            .unwrap();
        let kinds: Vec<(FlagKind, SessionId)> =
            stats.flags.iter().map(|f| (f.kind, f.session_id)).collect();
        assert!(kinds.contains(&(FlagKind::LongTurn, chat.id)), "{kinds:?}");
        assert!(
            kinds.contains(&(FlagKind::UnconfirmedWorking, terminal.id)),
            "{kinds:?}"
        );
        let unconfirmed = stats
            .flags
            .iter()
            .find(|f| f.kind == FlagKind::UnconfirmedWorking)
            .unwrap();
        assert!(
            unconfirmed.detail.contains("unconfirmed"),
            "{}",
            unconfirmed.detail
        );
        assert!(!unconfirmed.detail.contains("stuck"));
    }

    #[test]
    fn a_workspace_narrows_it_and_the_list_is_bounded() {
        let team = workspace("team");
        let mut app = workspace("app");
        app.parent_id = Some(team.id);
        let other = workspace("other");
        let mut w = World::default();
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        for n in 0..70u64 {
            let target = if n % 2 == 0 { &app } else { &other };
            let title = if n == 0 {
                format!("deploy {secret}")
            } else {
                format!("chat {n}")
            };
            let chat = session(target, &title, SessionKind::Harness);
            w.turns.push(turn(
                &chat,
                1,
                ChatTurnStatus::Completed,
                T0 + n,
                Some(T0 + n + 1_000 * (n + 1)),
                None,
            ));
            w.sessions.push(chat);
        }
        w.workspaces = vec![team.clone(), app, other];
        let all = w
            .stats(AgentsStatsRequest {
                group_by: StatsBy::Session,
                limit: Some(1_000),
                ..week()
            })
            .unwrap();
        let json = serde_json::to_string(&all).unwrap();
        assert!(json.len() <= STATS_MAX_BYTES, "{} bytes", json.len());
        assert!(all.groups.len() <= STATS_MAX_GROUPS);
        assert_eq!(all.groups.len() + all.omitted_groups, 70);
        assert_eq!(
            all.total.turns.unwrap().count,
            70,
            "the total counts what is not listed"
        );
        assert!(!json.contains(secret));
        // The heaviest first.
        let walls: Vec<u64> = all
            .groups
            .iter()
            .map(|g| g.turns.unwrap().wall_ms_total)
            .collect();
        assert!(walls.windows(2).all(|p| p[0] >= p[1]));
        let few = w
            .stats(AgentsStatsRequest {
                group_by: StatsBy::Session,
                limit: Some(3),
                ..week()
            })
            .unwrap();
        assert_eq!((few.groups.len(), few.omitted_groups), (3, 67));
        let narrowed = w
            .stats(AgentsStatsRequest {
                workspace_id: Some(team.id),
                group_by: StatsBy::Project,
                ..week()
            })
            .unwrap();
        assert_eq!(
            narrowed.total.turns.unwrap().count,
            35,
            "the projects of the workspace"
        );
        let missing = w
            .stats(AgentsStatsRequest {
                workspace_id: Some(WorkspaceId::new()),
                ..week()
            })
            .unwrap_err();
        assert_eq!(missing.code, ErrorCode::NotFound);
    }

    #[test]
    fn an_empty_daemon_has_empty_statistics_and_says_what_it_cannot_say() {
        let stats = World::default()
            .stats(AgentsStatsRequest::default())
            .unwrap();
        assert!(stats.groups.is_empty() && stats.failures.is_empty() && stats.flags.is_empty());
        assert_eq!(stats.to_ms - stats.from_ms, STATS_DEFAULT_WINDOW_MS);
        assert!(stats.total.turns.is_none() && stats.total.tokens.is_none());
        assert!(!stats.not_recorded.is_empty());
    }

    #[test]
    fn a_failure_is_read_without_what_differs_between_two_of_a_kind() {
        let a = normalise("Rate limited: retry in 40 s (request 01K7F3B5N2Q9W8E4R6T1Y0V3KM)");
        let b = normalise("rate limited: retry in 12 s (request 01M66J3RH7X4AA2V2W47JZJA07)");
        assert_eq!(a, b);
        assert_eq!(a, "rate limited: retry in # s (request <id>)");
        assert_eq!(
            normalise("cannot read /Users/me/proj/a.rs: permission denied"),
            normalise("cannot read /tmp/x/b.rs: permission denied")
        );
        assert_eq!(
            normalise("GET https://api.example.com/v1/x?a=1 failed with 503"),
            "get <url> failed with #"
        );
        assert!(normalise(&"x".repeat(500)).chars().count() <= 120);
        assert!(!normalise("key sk-abcdefghijklmnopqrstuvwxyz0123456789").contains("sk-"));
    }

    #[test]
    fn a_day_is_its_utc_calendar_date() {
        assert_eq!(day_key(0), "1970-01-01");
        assert_eq!(day_key(86_399_999), "1970-01-01");
        assert_eq!(day_key(86_400_000), "1970-01-02");
        // 2026-10-30 12:00 UTC.
        assert_eq!(day_key(1_793_361_600_000), "2026-10-30");
        assert_eq!(day_key(951_782_400_000), "2000-02-29");
    }
}
