//! `hutan agents`: the agents and subagents the daemon observes (`agent.tree`), as a tree per
//! session; with `--watch`, again whenever a tree changes.

use hutan_client::Client;
use hutan_core::{AgentNode, AgentObservation, Liveness, NodeOutcome, NodeState, SessionId, Usage};
use hutan_protocol::board::{
    AgentsBoard, AgentsBoardRequest, BoardAgent, BoardGroup, BoardKind, BoardSource, BoardState,
    BoardWaitKind,
};
use hutan_protocol::messages::{Empty, Event, Request, SessionInfo, Subscribed};
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::AgentsArgs;
use crate::error::CliError;

impl Ctx {
    pub(super) async fn agents(&self, args: AgentsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if args.board {
            return self.agents_board(&client, &args).await;
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
}
