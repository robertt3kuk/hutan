//! `hutan agents`: the agents and subagents the daemon observes (`agent.tree`), as a tree per
//! session; with `--watch`, again whenever a tree changes.

use hutan_client::Client;
use hutan_core::{
    AgentNode, AgentObservation, Liveness, NodeOutcome, NodeState, SessionId, TurnId, Usage,
};
use hutan_protocol::activity::{
    ActivityDetail, ActivityEvent, AgentsActivity, AgentsActivityRequest, RequestKind, TurnOutcome,
};
use hutan_protocol::board::{
    AgentsBoard, AgentsBoardRequest, BoardAgent, BoardGroup, BoardKind, BoardSource, BoardState,
    BoardWaitKind,
};
use hutan_protocol::messages::{Empty, Event, Request, SessionInfo, Subscribed};
use hutan_protocol::review::AgentsReviewRequest;
use hutan_protocol::stats::{
    AgentsStats, AgentsStatsRequest, FailureSource, FlagKind, StatsBy, StatsGroup, WaitSource,
};
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{AgentsArgs, StatsByArg};
use crate::error::CliError;

impl Ctx {
    pub(super) async fn agents(&self, args: AgentsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if args.board {
            return self.agents_board(&client, &args).await;
        }
        if args.activity {
            return self.agents_activity(&client, &args).await;
        }
        if args.stats {
            return self.agents_stats(&client, &args).await;
        }
        if let Some(target) = &args.review {
            return self
                .agents_review(&client, target, args.turn.as_deref())
                .await;
        }
        if args.ws.is_some() || args.since.is_some() || args.limit.is_some() {
            return Err(CliError::Usage(
                "--ws, --since and --limit go with --board, --activity or --stats".to_owned(),
            ));
        }
        if !client.supports_agent_tree() {
            return Err(CliError::Failed(
                "this daemon does not observe agent trees (protocol 0.10); restart it with a newer hutand"
                    .to_owned(),
            ));
        }
        let sessions = self.session_list(&client, None).await?.sessions;
        let session = match &args.target {
            Some(target) => Some(super::sessions::resolve_target(&sessions, target)?.id),
            None => None,
        };
        // Subscribe first so a change between the print and the wait is not lost.
        let mut events = args.watch.then(|| client.events());
        if args.watch {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        self.print_trees(&client, session).await?;
        let Some(events) = events.as_mut() else {
            return Ok(());
        };
        loop {
            match events.recv().await {
                Ok((_, Event::AgentTreeUpdated { session_id, .. }))
                    if session.is_none_or(|id| id == session_id) =>
                {
                    self.print_trees(&client, session).await?;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }

    /// `--stats`: `agents.stats` (protocol 0.66), as text or `--json`.
    async fn agents_stats(&self, client: &Client, args: &AgentsArgs) -> Result<(), CliError> {
        if !client.supports_agents_stats() {
            return Err(CliError::Failed(
                "this daemon has no flow statistics (it predates protocol 0.67); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let now = now_ms();
        let since_ms = match &args.since {
            Some(ago) => Some(now.saturating_sub(parse_ago(ago)?)),
            None => None,
        };
        let workspace_id = match &args.ws {
            Some(target) => Some(self.resolve_workspace_id(client, target).await?),
            None => None,
        };
        let stats = client
            .agents_stats(AgentsStatsRequest {
                since_ms,
                until_ms: None,
                group_by: match args.by.unwrap_or(StatsByArg::Project) {
                    StatsByArg::Project => StatsBy::Project,
                    StatsByArg::Harness => StatsBy::Harness,
                    StatsByArg::Day => StatsBy::Day,
                    StatsByArg::Session => StatsBy::Session,
                    StatsByArg::Task => StatsBy::Task,
                },
                workspace_id,
                limit: args.limit,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&stats, || stats_text(&stats, now))
    }

    /// `--activity`: `agents.activity` (protocol 0.65), as text or `--json`.
    async fn agents_activity(&self, client: &Client, args: &AgentsArgs) -> Result<(), CliError> {
        if !client.supports_agents_activity() {
            return Err(CliError::Failed(
                "this daemon has no activity feed (it predates protocol 0.65); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let now = now_ms();
        let since_ms = match &args.since {
            Some(ago) => Some(now.saturating_sub(parse_ago(ago)?)),
            None => None,
        };
        let workspace_id = match &args.ws {
            Some(target) => Some(self.resolve_workspace_id(client, target).await?),
            None => None,
        };
        let activity = client
            .agents_activity(AgentsActivityRequest {
                since_ms,
                until_ms: None,
                workspace_id,
                session_id: None,
                limit: args.limit,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&activity, || activity_text(&activity, now))
    }

    /// `--board`: `agents.board` (protocol 0.63), as text or `--json`.
    async fn agents_board(&self, client: &Client, args: &AgentsArgs) -> Result<(), CliError> {
        if !client.supports_agents_board() {
            return Err(CliError::Failed(
                "this daemon has no agents board (it predates protocol 0.63); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let workspace_id = match &args.ws {
            Some(target) => Some(self.resolve_workspace_id(client, target).await?),
            None => None,
        };
        let board = client
            .agents_board(AgentsBoardRequest {
                workspace_id,
                include_finished: args.finished,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&board, || board_text(&board, now_ms()))
    }

    async fn print_trees(
        &self,
        client: &Client,
        session: Option<SessionId>,
    ) -> Result<(), CliError> {
        let observations = client.agent_tree(session).await.map_err(|e| self.fail(e))?;
        let sessions = self.session_list(client, None).await?.sessions;
        self.print(&observations, || {
            tree_report(&observations, &sessions, now_ms())
        })
    }
}

impl Ctx {
    /// `--review`: `agents.review` (protocol 0.67), as the packet's text or `--json`.
    async fn agents_review(
        &self,
        client: &Client,
        target: &str,
        turn: Option<&str>,
    ) -> Result<(), CliError> {
        if !client.supports_agents_review() {
            return Err(CliError::Failed(
                "this daemon cannot review a turn (it predates protocol 0.67); restart it with \
                 `hutan daemon stop` and run the command again"
                    .to_owned(),
            ));
        }
        let sessions = self.session_list(client, None).await?.sessions;
        let session = super::sessions::resolve_target(&sessions, target)?.id;
        let turn_id = match turn {
            Some(turn) => Some(turn.parse::<TurnId>().map_err(|_| {
                CliError::Failed(format!(
                    "`{turn}` is not a turn id (`hutan chat log` shows them)"
                ))
            })?),
            None => None,
        };
        let packet = client
            .agents_review(AgentsReviewRequest {
                session_id: session,
                turn_id,
            })
            .await
            .map_err(|e| self.fail(e))?;
        self.print(&packet, || packet.text())
    }
}

/// Every tree, headed by its session.
fn tree_report(observations: &[AgentObservation], sessions: &[SessionInfo], now_ms: u64) -> String {
    if observations.is_empty() {
        return "no agents".to_owned();
    }
    observations
        .iter()
        .map(|observation| {
            let title = sessions
                .iter()
                .find(|s| s.id == observation.session_id)
                .map_or("", |s| s.title.as_str());
            format!(
                "{} {title}\n{}",
                observation.session_id,
                render_tree(observation, now_ms)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One tree: indentation by depth, a state glyph, the label, the model, how long it has been
/// there, and tokens when the source reports them.
pub(super) fn render_tree(observation: &AgentObservation, now_ms: u64) -> String {
    let mut out = String::new();
    fn walk(
        observation: &AgentObservation,
        node: &AgentNode,
        depth: usize,
        now_ms: u64,
        out: &mut String,
    ) {
        out.push_str(&"  ".repeat(depth + 1));
        out.push_str(&node_line(node, now_ms));
        out.push('\n');
        for child in observation
            .nodes
            .values()
            .filter(|n| n.parent.as_deref() == Some(node.node_id.as_str()))
        {
            walk(observation, child, depth + 1, now_ms, out);
        }
    }
    if let Some(root) = observation.nodes.get(&observation.root) {
        walk(observation, root, 0, now_ms, &mut out);
    }
    out.trim_end().to_owned()
}

fn glyph(node: &AgentNode) -> &'static str {
    match (node.outcome, node.state) {
        (Some(NodeOutcome::Completed), _) => "✓",
        (Some(NodeOutcome::Failed), _) => "✗",
        (Some(NodeOutcome::Interrupted), _) => "■",
        (Some(NodeOutcome::Disconnected), _) => "⊘",
        (None, NodeState::Working) => "●",
        (None, NodeState::Blocked) => "▲",
        (None, NodeState::Idle) => "○",
        (None, NodeState::Unknown) => "?",
    }
}

fn node_line(node: &AgentNode, now_ms: u64) -> String {
    let mut parts = vec![format!("{} {}", glyph(node), node.label())];
    let status = match (node.outcome, node.state) {
        (Some(outcome), _) => format!("{outcome:?}").to_lowercase(),
        (None, state) => format!("{state:?}").to_lowercase(),
    };
    parts.push(status);
    if node.liveness == Liveness::Unknown && node.outcome.is_none() {
        parts.push("(liveness unknown)".to_owned());
    }
    if let Some(model) = &node.model {
        parts.push(model.clone());
    }
    if let Some(started) = node.started_at_ms {
        let end = node.ended_at_ms.unwrap_or(now_ms);
        parts.push(age_text(end.saturating_sub(started) / 1000));
    }
    if let Some(tokens) = node.usage.as_ref().and_then(tokens_text) {
        parts.push(tokens);
    }
    if let Some(activity) = &node.activity {
        parts.push(format!("— {activity}"));
    }
    parts.join("  ")
}

/// Tokens as the source counted them, labelled by what the counter means; nothing when the
/// source reported no token figure (never a made-up zero).
fn tokens_text(usage: &Usage) -> Option<String> {
    let total = match (usage.input, usage.output) {
        (None, None) => usage.context_tokens?,
        (input, output) => input.unwrap_or(0) + output.unwrap_or(0),
    };
    Some(format!("{} tok [{}]", compact(total), usage.kind))
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// The board as text: the groups of the Agents page in its order, one agent per two lines (who
/// and what, then what it waits for), and the coverage that says what the numbers are worth.
pub(super) fn board_text(board: &AgentsBoard, now_ms: u64) -> String {
    let counts = &board.counts;
    if counts.agents == 0 {
        return "no agents".to_owned();
    }
    let mut lines = vec![format!(
        "{} agents: {} need you · {} failed · {} ready for review · {} working · {} unconfirmed · {} idle · {} finished",
        counts.agents,
        counts.requests,
        counts.failed,
        counts.review,
        counts.working,
        counts.unconfirmed,
        counts.idle,
        counts.finished
    )];
    let coverage = &board.coverage;
    lines.push(format!(
        "seen through: {} chats (journal) · {} terminal agents with hooks (state only) · {} recognised only (state and title)",
        coverage.chats, coverage.terminal_reported, coverage.terminal_recognised
    ));
    let mut group: Option<BoardGroup> = None;
    for agent in &board.agents {
        if group != Some(agent.group) {
            group = Some(agent.group);
            lines.push(String::new());
            lines.push(group_title(agent.group).to_owned());
        }
        lines.extend(agent_lines(agent, now_ms));
    }
    if board.hidden_finished > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} finished not listed (--finished lists them)",
            board.hidden_finished
        ));
    }
    if board.omitted > 0 {
        lines.push(format!(
            "{} more not listed: the board lists at most 200 agents in 24 KiB; narrow it with --ws",
            board.omitted
        ));
    }
    lines.join("\n")
}

fn group_title(group: BoardGroup) -> &'static str {
    match group {
        BoardGroup::Requests => "Needs you",
        BoardGroup::Failed => "Failed",
        BoardGroup::Review => "Ready for review",
        BoardGroup::Working => "Working",
        BoardGroup::Unconfirmed => "State unavailable",
        BoardGroup::Idle => "Idle",
        BoardGroup::Snoozed => "Snoozed",
        BoardGroup::Finished => "Finished",
    }
}

fn state_word(state: BoardState) -> &'static str {
    match state {
        BoardState::Working => "working",
        BoardState::Blocked => "blocked",
        BoardState::Idle => "idle",
        BoardState::Unconfirmed => "state unavailable",
        BoardState::Completed => "completed",
        BoardState::Failed => "failed",
        BoardState::Stopped => "stopped",
        BoardState::Interrupted => "interrupted",
        BoardState::Disconnected => "disconnected",
    }
}

fn source_word(source: BoardSource) -> &'static str {
    match source {
        BoardSource::Journal => "exact",
        BoardSource::Hook => "state only",
        BoardSource::Recognised => "recognised",
    }
}

fn wait_word(kind: BoardWaitKind) -> &'static str {
    match kind {
        BoardWaitKind::Approval => "approval",
        BoardWaitKind::Question => "question",
        BoardWaitKind::Plan => "plan",
        BoardWaitKind::TerminalPrompt => "prompt",
    }
}

fn agent_lines(agent: &BoardAgent, now_ms: u64) -> Vec<String> {
    let place = match &agent.workspace {
        Some(workspace) if workspace.name != agent.project.name => {
            format!("{} / {}", agent.project.name, workspace.name)
        }
        _ => agent.project.name.clone(),
    };
    let mut head = vec![
        format!("  {}", agent.title),
        state_word(agent.state).to_owned(),
        agent.harness.clone(),
    ];
    if let Some(model) = &agent.model {
        head.push(model.clone());
    }
    head.push(place);
    if let Some(branch) = agent.checkout.as_ref().and_then(|c| c.branch.as_ref()) {
        head.push(branch.clone());
    }
    head.push(format!(
        "{} ({})",
        match agent.kind {
            BoardKind::Chat => "chat",
            BoardKind::Terminal => "terminal",
        },
        source_word(agent.source)
    ));
    let mut lines = vec![head.join(" · ")];
    let indent = "    ";
    for wait in &agent.waiting {
        let age = wait
            .since_ms
            .map(|since| {
                format!(
                    " · waiting {}",
                    age_text(now_ms.saturating_sub(since) / 1000)
                )
            })
            .unwrap_or_default();
        lines.push(format!(
            "{indent}{}: {}{age}",
            wait_word(wait.kind),
            wait.title
        ));
    }
    // The message of a blocked terminal agent is already its wait.
    if let Some(activity) = agent
        .activity
        .as_ref()
        .filter(|activity| !agent.waiting.iter().any(|wait| &wait.title == *activity))
    {
        lines.push(format!("{indent}{activity}"));
    }
    if !agent.detail.is_empty() {
        lines.push(format!("{indent}{}", agent.detail));
    }
    lines.push(format!("{indent}session {}", agent.session_id));
    lines
}

/// The statistics as text: the window and what it rests on, the total, one block per group, the
/// failures that came again and what looks stuck — every number with how well it is known.
pub(super) fn stats_text(stats: &AgentsStats, now_ms: u64) -> String {
    let when = hutan_client::time::local_time_text;
    let mut lines = vec![
        format!(
            "since {} ({}) until {}, by {}",
            when(stats.from_ms),
            hutan_client::time::relative_text(now_ms, stats.from_ms),
            when(stats.to_ms),
            match stats.group_by {
                StatsBy::Project => "project",
                StatsBy::Harness => "harness",
                StatsBy::Day => "day (UTC)",
                StatsBy::Session => "session",
                StatsBy::Task => "task",
            }
        ),
        format!(
            "seen through: {} chats (journal: exact) · {} terminal agents (waits only, from the state log: {})",
            stats.coverage.chats,
            stats.coverage.terminal_agents,
            match (stats.coverage.state_log, stats.coverage.state_log_from_ms) {
                (false, _) => "OFF".to_owned(),
                (true, Some(from)) => format!("on, since {}", when(from)),
                (true, None) => "on, nothing recorded yet".to_owned(),
            }
        ),
    ];
    lines.push(String::new());
    lines.extend(group_lines(&stats.total, "  "));
    for group in &stats.groups {
        lines.push(String::new());
        lines.extend(group_lines(group, "  "));
    }
    if stats.omitted_groups > 0 {
        lines.push(format!(
            "\n{} more groups not listed (--limit, or narrow with --ws)",
            stats.omitted_groups
        ));
    }
    if !stats.failures.is_empty() {
        lines.push(String::new());
        lines.push("failures that came again (grouped by how they read: a heuristic; the counts are exact)".to_owned());
        for failure in &stats.failures {
            lines.push(format!(
                "  {}× in {} sessions ({}): {}",
                failure.count,
                failure.sessions,
                match failure.source {
                    FailureSource::TurnError => "a chat's turn error",
                    FailureSource::TerminalError => "a terminal agent's error",
                },
                failure.pattern
            ));
        }
    }
    if !stats.flags.is_empty() {
        lines.push(String::new());
        lines.push("worth a look".to_owned());
        for flag in &stats.flags {
            lines.push(format!(
                "  {} ({}): {}",
                flag.title,
                match flag.kind {
                    FlagKind::LongTurn => "exact",
                    FlagKind::UnconfirmedWorking => "unconfirmed",
                },
                flag.detail
            ));
        }
    }
    lines.push(String::new());
    for what in &stats.not_recorded {
        lines.push(format!("not recorded: {what}"));
    }
    lines.join("\n")
}

fn group_lines(group: &StatsGroup, indent: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "{}{} · {} chats, {} terminal agents",
        indent, group.label, group.coverage.chats, group.coverage.terminal_agents
    )];
    let detail = format!("{indent}  ");
    if let Some(turns) = &group.turns {
        lines.push(format!(
            "{detail}turns: {} ({} failed, {} cancelled) · wall {} in all, {} longest, {} on average (active time is not recorded)",
            turns.count,
            turns.failed,
            turns.cancelled,
            took_text(turns.wall_ms_total),
            took_text(turns.wall_ms_longest),
            took_text(turns.wall_ms_mean)
        ));
    }
    if let Some(tasks) = &group.tasks {
        lines.push(format!(
            "{detail}tasks: {} done, {} failed, {} cancelled · {} in all, {} longest",
            tasks.done,
            tasks.failed,
            tasks.cancelled,
            took_text(tasks.wall_ms_total),
            took_text(tasks.wall_ms_longest)
        ));
    }
    if let Some(waits) = &group.waits {
        let sides: Vec<String> = waits
            .by_source
            .iter()
            .map(|side| {
                format!(
                    "{} {}{}",
                    side.count,
                    match side.source {
                        WaitSource::Audit => "of chats (exact)",
                        WaitSource::StateLog => "of terminal agents (state log)",
                    },
                    if side.open > 0 {
                        format!(", {} still going", side.open)
                    } else {
                        String::new()
                    }
                )
            })
            .collect();
        lines.push(format!(
            "{detail}waiting for you: {} · {} in all, {} longest ({})",
            waits.count,
            took_text(waits.total_ms),
            took_text(waits.longest_ms),
            sides.join(", ")
        ));
        let kinds: Vec<String> = waits
            .by_kind
            .iter()
            .map(|k| format!("{} {}× {}", k.kind, k.count, took_text(k.total_ms)))
            .collect();
        if !kinds.is_empty() {
            lines.push(format!("{detail}  by kind: {}", kinds.join(" · ")));
        }
    }
    if let Some(retries) = &group.retries {
        lines.push(format!(
            "{detail}retries: {} (inferred: a turn soon after a failed one)",
            retries.count
        ));
    }
    if let Some(tokens) = &group.tokens {
        lines.push(format!(
            "{detail}tokens: {} in · {} cached · {} out · {} total over {} turns{}",
            tokens.input,
            tokens.cached_input,
            tokens.output,
            tokens.total,
            tokens.turns_reported,
            match tokens.cost_usd_estimate {
                Some(cost) => format!(" · cost ≈ ${cost:.2} (the harnesses' estimate)"),
                None => " · no cost reported".to_owned(),
            }
        ));
    }
    if lines.len() == 1 {
        lines.push(format!("{detail}nothing recorded"));
    }
    lines
}

/// `30m`, `2h`, `1d`, `90s`: how far back, in ms.
fn parse_ago(text: &str) -> Result<u64, CliError> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let count: u64 = digits.parse().map_err(|_| {
        CliError::Usage(format!(
            "--since `{text}`: a number and a unit, like 30m, 2h or 1d"
        ))
    })?;
    let unit_ms: u64 = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => {
            return Err(CliError::Usage(format!(
                "--since `{text}`: the unit is s, m, h or d"
            )));
        }
    };
    Ok(count.saturating_mul(unit_ms))
}

/// The catch-up as text: the window, what is counted, what is not recorded, then the events
/// newest first.
pub(super) fn activity_text(activity: &AgentsActivity, now_ms: u64) -> String {
    let s = &activity.summary;
    let mut lines = vec![format!(
        "since {} ({}) until {}",
        hutan_client::time::local_time_text(activity.from_ms),
        hutan_client::time::relative_text(now_ms, activity.from_ms),
        hutan_client::time::local_time_text(activity.to_ms),
    )];
    let mut parts: Vec<String> = Vec::new();
    let mut count = |n: usize, what: &str| {
        if n > 0 {
            parts.push(format!("{n} {what}"));
        }
    };
    count(s.turns_finished, "turns finished");
    count(s.turns_failed, "turns failed");
    count(s.turns_cancelled, "turns cancelled");
    count(s.turns_running, "turns running");
    count(s.requests_asked, "requests asked");
    count(s.requests_resolved, "answered");
    count(s.requests_open, "still waiting");
    count(s.checkpoints, "checkpoints");
    count(s.tasks_done, "tasks done");
    count(s.tasks_failed, "tasks failed");
    count(s.tasks_review, "sent to review");
    count(s.sessions_started, "sessions started");
    count(s.sessions_ended, "sessions ended");
    if parts.is_empty() {
        lines.push("nothing recorded in this window".to_owned());
    } else {
        lines.push(parts.join(" · "));
    }
    if let Some(longest) = s.wait_longest_ms {
        lines.push(format!(
            "requests waited {} in all, the longest {}",
            super::sessions::age_text(s.wait_total_ms.unwrap_or(0) / 1000),
            super::sessions::age_text(longest / 1000)
        ));
    }
    if let (Some(files), Some(added), Some(removed)) =
        (s.files_changed, s.lines_added, s.lines_removed)
    {
        lines.push(format!("{files} files changed · +{added} −{removed} lines"));
    }
    if let Some(from) = activity
        .coverage_from_ms
        .filter(|from| *from > activity.from_ms)
    {
        lines.push(format!(
            "records begin {}: nothing earlier is recorded",
            hutan_client::time::local_time_text(from)
        ));
    }
    for what in &activity.not_recorded {
        lines.push(format!("not recorded: {what}"));
    }
    lines.push(String::new());
    for event in &activity.events {
        lines.push(format!(
            "{}  {}",
            hutan_client::time::local_time_text(event.at_ms),
            event_line(event)
        ));
    }
    if activity.omitted > 0 {
        lines.push(format!(
            "{} older events not listed (--limit, or a shorter --since)",
            activity.omitted
        ));
    }
    lines.join("\n")
}

/// `850 ms`, `3.9 s`, `3m 20s`, `2h 05m`.
fn took_text(ms: u64) -> String {
    let seconds = ms / 1_000;
    match seconds {
        0 => format!("{ms} ms"),
        1..=9 => format!("{:.1} s", ms as f64 / 1_000.0),
        10..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

fn event_line(event: &ActivityEvent) -> String {
    let whose = format!("{} · {}", event.title, event.project);
    let what = match &event.detail {
        ActivityDetail::TurnEnded {
            outcome,
            duration_ms,
            error,
        } => {
            let word = match outcome {
                TurnOutcome::Completed => "turn finished",
                TurnOutcome::Failed => "turn failed",
                TurnOutcome::Cancelled => "turn cancelled",
            };
            let took = duration_ms
                .map(|ms| format!(" after {}", took_text(ms)))
                .unwrap_or_default();
            let why = error.as_ref().map(|e| format!(": {e}")).unwrap_or_default();
            format!("{word}{took}{why}")
        }
        ActivityDetail::Request {
            request,
            text,
            resolved_ms,
            wait_ms,
            outcome,
            decided_by,
            ..
        } => {
            let noun = match request {
                RequestKind::Approval => "approval",
                RequestKind::Question => "question",
            };
            match resolved_ms {
                Some(_) => format!(
                    "{noun} {} by {} after {}: {text}",
                    outcome.as_deref().unwrap_or("resolved"),
                    decided_by.as_deref().unwrap_or("nobody"),
                    took_text(wait_ms.unwrap_or(0)),
                ),
                None => format!("{noun} asked, still waiting: {text}"),
            }
        }
        ActivityDetail::Task { state, run } => {
            if *run {
                format!("task {state}")
            } else {
                format!("task work {state}")
            }
        }
        ActivityDetail::SessionStarted { chat } => {
            if *chat {
                "chat started".to_owned()
            } else {
                "terminal started".to_owned()
            }
        }
        ActivityDetail::SessionEnded { state } => format!("session ended: {state}"),
        ActivityDetail::Checkpoint {
            files,
            added,
            removed,
        } => format!("{files} files changed · +{added} −{removed}"),
    };
    format!("{whose}: {what}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hutan_core::{NodeKind, NodeOutcome};

    use super::*;

    fn node(id: &str, parent: Option<&str>, state: NodeState) -> AgentNode {
        AgentNode {
            pid: None,
            node_id: id.into(),
            parent: parent.map(str::to_owned),
            kind: if parent.is_some() {
                NodeKind::Sub
            } else {
                NodeKind::Main
            },
            source: "s".into(),
            agent: "claude".into(),
            name: None,
            agent_type: None,
            state,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1_000),
            ended_at_ms: None,
            last_activity_ms: 1_000,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    #[test]
    fn the_tree_is_indented_with_state_glyphs_models_durations_and_tokens() {
        let mut root = node("root", None, NodeState::Working);
        root.model = Some("claude-opus-5".into());
        let mut explore = node("a1", Some("root"), NodeState::Blocked);
        explore.name = Some("Explore".into());
        explore.activity = Some("permission: Bash".into());
        let mut done = node("a2", Some("a1"), NodeState::Idle);
        done.name = Some("Grep".into());
        done.outcome = Some(NodeOutcome::Completed);
        done.ended_at_ms = Some(61_000);
        done.usage = Some(Usage {
            kind: "omp.tokens".into(),
            input: Some(1_200),
            output: Some(300),
            ..Usage::default()
        });
        let observation = AgentObservation {
            session_id: SessionId::new(),
            root: "root".into(),
            nodes: [root, explore, done]
                .into_iter()
                .map(|n| (n.node_id.clone(), n))
                .collect::<BTreeMap<_, _>>(),
        };
        let text = render_tree(&observation, 31_000);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(
            lines[0].starts_with("  ● root  working  claude-opus-5  30s"),
            "{text}"
        );
        assert!(lines[1].starts_with("    ▲ Explore  blocked"), "{text}");
        assert!(lines[1].ends_with("— permission: Bash"), "{text}");
        assert!(lines[2].starts_with("      ✓ Grep  completed"), "{text}");
        assert!(lines[2].contains("1.5k tok [omp.tokens]"), "{text}");
    }

    #[test]
    fn no_token_figure_prints_nothing() {
        let mut n = node("root", None, NodeState::Idle);
        n.usage = Some(Usage {
            kind: "claude.context".into(),
            context_window: Some(200_000),
            ..Usage::default()
        });
        assert!(!node_line(&n, 2_000).contains("tok"));
    }

    #[test]
    fn the_board_text_groups_like_the_page_and_says_what_each_row_is_worth() {
        use hutan_core::WorkspaceId;
        use hutan_protocol::board::{
            AgentsBoard, BoardAgent, BoardCounts, BoardCoverage, BoardRef, BoardRole,
            BoardSubagents, BoardWaiting,
        };

        let project = BoardRef {
            id: WorkspaceId::new(),
            name: "hutan".into(),
        };
        let agent = |title: &str, state, group, source, kind| BoardAgent {
            session_id: SessionId::new(),
            title: title.into(),
            role: BoardRole::Agent,
            kind,
            harness: "OMP".into(),
            model: None,
            project: project.clone(),
            workspace: None,
            checkout: None,
            state,
            group,
            detail: String::new(),
            waiting: Vec::new(),
            activity: None,
            turn_active: false,
            started_ms: None,
            last_event_ms: None,
            reported_ms: None,
            subagents: BoardSubagents::default(),
            source,
        };
        let mut asking = agent(
            "Release checklist",
            BoardState::Blocked,
            BoardGroup::Requests,
            BoardSource::Hook,
            BoardKind::Terminal,
        );
        asking.waiting.push(BoardWaiting {
            kind: BoardWaitKind::TerminalPrompt,
            title: "Run the full suite?".into(),
            since_ms: Some(1_000),
            ref_id: None,
        });
        let board = AgentsBoard {
            generated_at_ms: 100_000,
            counts: BoardCounts {
                agents: 3,
                requests: 1,
                unconfirmed: 1,
                finished: 1,
                ..BoardCounts::default()
            },
            coverage: BoardCoverage {
                chats: 0,
                terminal_reported: 1,
                terminal_recognised: 2,
            },
            agents: vec![
                asking,
                agent(
                    "Docs pass",
                    BoardState::Unconfirmed,
                    BoardGroup::Unconfirmed,
                    BoardSource::Recognised,
                    BoardKind::Terminal,
                ),
            ],
            hidden_finished: 1,
            omitted: 0,
        };
        let text = board_text(&board, 100_000);
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle:?}: {text}"))
        };
        assert!(at("Needs you") < at("Release checklist"));
        assert!(at("Release checklist") < at("State unavailable"));
        assert!(
            text.contains("prompt: Run the full suite? · waiting 1m"),
            "{text}"
        );
        assert!(text.contains("terminal (state only)"));
        assert!(text.contains("terminal (recognised)"));
        assert!(text.contains("1 finished not listed"));
        assert!(text.contains("2 recognised only"));
        let empty = AgentsBoard {
            counts: BoardCounts::default(),
            agents: Vec::new(),
            hidden_finished: 0,
            ..board
        };
        assert_eq!(board_text(&empty, 0), "no agents");
    }

    #[test]
    fn a_window_is_a_number_and_a_unit_and_the_text_says_what_is_not_recorded() {
        assert_eq!(parse_ago("90s").unwrap(), 90_000);
        assert_eq!(parse_ago("30m").unwrap(), 1_800_000);
        assert_eq!(parse_ago("2h").unwrap(), 7_200_000);
        assert_eq!(parse_ago("1d").unwrap(), 86_400_000);
        for bad in ["", "m", "soon", "5x", "-1h", "1.5h"] {
            assert!(parse_ago(bad).is_err(), "{bad:?}");
        }

        let activity = AgentsActivity {
            from_ms: 1_000_000,
            to_ms: 5_000_000,
            coverage_from_ms: Some(2_000_000),
            summary: Default::default(),
            events: Vec::new(),
            omitted: 0,
            not_recorded: vec!["turns of terminal agents".into()],
        };
        let text = activity_text(&activity, 5_000_000);
        assert!(text.contains("nothing recorded in this window"), "{text}");
        assert!(text.contains("nothing earlier is recorded"), "{text}");
        assert!(
            text.contains("not recorded: turns of terminal agents"),
            "{text}"
        );
    }

    #[test]
    fn the_stats_text_says_how_each_number_is_known_and_what_is_not_recorded() {
        use hutan_protocol::stats::{
            FailureCluster, GroupCoverage, RetryStats, StatsCoverage, TokenStats, TurnStats,
            WaitKind, WaitSide, WaitStats,
        };

        let group = StatsGroup {
            key: "p".into(),
            label: "hutan".into(),
            coverage: GroupCoverage {
                chats: 1,
                terminal_agents: 1,
            },
            turns: Some(TurnStats {
                count: 4,
                failed: 1,
                cancelled: 0,
                wall_ms_total: 120_000,
                wall_ms_longest: 60_000,
                wall_ms_mean: 30_000,
                active_ms_total: None,
            }),
            tasks: None,
            waits: Some(WaitStats {
                count: 2,
                total_ms: 90_000,
                longest_ms: 60_000,
                by_source: vec![WaitSide {
                    source: WaitSource::StateLog,
                    count: 2,
                    total_ms: 90_000,
                    longest_ms: 60_000,
                    open: 1,
                }],
                by_kind: vec![WaitKind {
                    kind: "permission".into(),
                    count: 2,
                    total_ms: 90_000,
                }],
            }),
            retries: Some(RetryStats {
                count: 1,
                inferred: true,
            }),
            tokens: Some(TokenStats {
                turns_reported: 3,
                input: 10,
                cached_input: 5,
                output: 2,
                total: 12,
                cost_usd_estimate: None,
            }),
        };
        let stats = AgentsStats {
            from_ms: 1_000_000,
            to_ms: 2_000_000,
            group_by: StatsBy::Harness,
            coverage: StatsCoverage {
                state_log: false,
                state_log_from_ms: None,
                tokens_from_ms: None,
                chats: 1,
                terminal_agents: 1,
            },
            total: StatsGroup {
                key: "all".into(),
                label: "Everything".into(),
                ..group.clone()
            },
            groups: vec![group],
            omitted_groups: 0,
            failures: vec![FailureCluster {
                pattern: "rate limited: retry in # s".into(),
                count: 3,
                sessions: 2,
                last_ms: 1_500_000,
                source: FailureSource::TurnError,
            }],
            flags: Vec::new(),
            not_recorded: vec!["active time per turn".into()],
        };
        let text = stats_text(&stats, 2_000_000);
        for needle in [
            "by harness",
            "state log: OFF",
            "turns: 4 (1 failed, 0 cancelled)",
            "active time is not recorded",
            "of terminal agents (state log), 1 still going",
            "retries: 1 (inferred",
            "no cost reported",
            "3× in 2 sessions (a chat's turn error): rate limited: retry in # s",
            "a heuristic; the counts are exact",
            "not recorded: active time per turn",
        ] {
            assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
        }
    }
}
