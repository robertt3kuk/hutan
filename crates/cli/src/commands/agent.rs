//! `hutan agent report | release | hook`: the reporting side of the agent channel. These are
//! called by agent hooks and extensions, so they never fail, never print, and never wait: no
//! `HUTAN_SESSION_ID`, no daemon, a daemon error, or 200 ms of silence all end in exit 0.

use std::io::{IsTerminal as _, Read as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hutan_core::insight::InsightReport;
use hutan_core::{NodeKind, NodeOutcome, SessionId, Usage};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, Request,
};
use hutan_protocol::tree::NodeReport;
use serde_json::Value;
use tokio::time::timeout;

use super::Ctx;
use crate::args::{
    AgentCommand, AgentReportArgs, AgentStateArg, HookAgentArg, NodeKindArg, OutcomeArg,
};
use crate::hook::{self, HookEnv, Plan, PlanNode};

/// Everything — connecting included — must be over within this time.
const BUDGET: Duration = Duration::from_millis(200);
/// Hook payloads (prompts, tool inputs) are cut here; the fields that matter come first.
const MAX_HOOK_INPUT: u64 = 1024 * 1024;

pub(super) async fn run(ctx: Option<Ctx>, command: &AgentCommand) {
    // Taken first: the order in which hooks start is the order of their events.
    let default_seq = nanos_since_epoch();
    // Always consumed, so the agent writing it never meets a closed pipe.
    let stdin = match command {
        AgentCommand::Hook(_) | AgentCommand::Statusline => read_stdin(),
        AgentCommand::Report(args) if args.insight_json.as_deref() == Some("-") => read_stdin(),
        AgentCommand::Report(_) | AgentCommand::Release(_) | AgentCommand::Interrupt(_) => None,
    };
    let Some(ctx) = ctx else { return };
    let Some(session) = session_from_env() else {
        return;
    };
    let json = || serde_json::from_str::<Value>(stdin.as_deref()?).ok();
    let request = match command {
        AgentCommand::Report(args) => report_request(args, stdin.as_deref(), session, default_seq),
        AgentCommand::Release(args) => Some(Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: args.source.clone(),
            seq: args.seq.unwrap_or(default_seq),
            node: args.node.clone(),
            outcome: args.outcome.map(outcome),
        })),
        AgentCommand::Hook(args) => {
            json().and_then(|input| hook_request(args.agent, &input, session, default_seq))
        }
        AgentCommand::Statusline => {
            json().and_then(|input| statusline_request(&input, session, default_seq))
        }
        // A user command with output and exit codes: `commands.rs` runs it, not this path.
        AgentCommand::Interrupt(_) => None,
    };
    let Some(request) = request else { return };
    // Nothing to do with the outcome: a hook cannot act on it, and must not be noisy about it.
    let _ = timeout(BUDGET, async {
        let client = ctx.connect().await.ok()?;
        client.call::<AgentApplied>(request).await.ok()
    })
    .await;
}

/// `hutan agent report`; `None` when `--usage-json` is not a usage object or `--insight-json` is
/// not an insight report (the report is dropped rather than sent without the numbers it was
/// meant to carry). `stdin` is what standard input held when `--insight-json` is `-`.
fn report_request(
    args: &AgentReportArgs,
    stdin: Option<&str>,
    session: SessionId,
    default_seq: u64,
) -> Option<Request> {
    let usage = match &args.usage_json {
        Some(json) => Some(serde_json::from_str::<Usage>(json).ok()?),
        None => None,
    };
    let insight = match args.insight_json.as_deref() {
        Some("-") => Some(serde_json::from_str::<InsightReport>(stdin?).ok()?),
        Some(json) => Some(serde_json::from_str::<InsightReport>(json).ok()?),
        None => None,
    };
    let node = args.node.as_ref().map(|id| NodeReport {
        id: id.clone(),
        parent: args.parent.clone(),
        kind: match args.kind {
            NodeKindArg::Main => NodeKind::Main,
            NodeKindArg::Sub => NodeKind::Sub,
        },
        name: args.name.clone(),
        agent_type: args.agent_type.clone(),
        outcome: args.outcome.map(outcome),
        model: args.model.clone(),
        activity: args.activity.clone(),
        usage,
        transcript_path: None,
    });
    Some(Request::AgentReport(AgentReportRequest {
        pid: args.pid,
        session_id: session,
        source: args.source.clone(),
        agent: args.agent.clone(),
        state: args.state.map(state),
        message: args.message.clone(),
        seq: args.seq.unwrap_or(default_seq),
        agent_session_id: args.agent_session_id.clone(),
        resume_argv: (!args.resume_argv.is_empty()).then(|| args.resume_argv.clone()),
        node,
        insight,
    }))
}

/// One tick of Claude Code's status line (`hutan agent statusline`): an insight-only report.
fn statusline_request(input: &Value, session: SessionId, seq: u64) -> Option<Request> {
    let insight = hook::insight::from_statusline(input)?;
    Some(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session,
        source: hook::CLAUDE_STATUSLINE_SOURCE.to_owned(),
        agent: "claude".to_owned(),
        state: None,
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
        insight: Some(insight),
    }))
}

fn hook_request(
    agent: HookAgentArg,
    input: &Value,
    session: SessionId,
    seq: u64,
) -> Option<Request> {
    let codex_thread = std::env::var("CODEX_THREAD_ID").ok();
    let env = HookEnv {
        codex_thread_id: codex_thread.as_deref(),
    };
    Some(match hook::plan(agent, input, &env)? {
        Plan::Report {
            agent,
            source,
            state: report_state,
            message,
            agent_session_id,
            resume_argv,
            node,
            insight,
        } => Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session,
            source: source.to_owned(),
            agent: agent.to_owned(),
            state: report_state.map(state),
            message,
            seq,
            agent_session_id,
            resume_argv: (!resume_argv.is_empty()).then_some(resume_argv),
            node: node.map(node_report),
            insight,
        }),
        Plan::Release { source, node } => Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: source.to_owned(),
            seq,
            node,
            outcome: None,
        }),
    })
}

fn node_report(node: PlanNode) -> NodeReport {
    NodeReport {
        id: node.id,
        parent: None,
        kind: node.kind.unwrap_or(NodeKind::Main),
        name: node.name,
        agent_type: node.agent_type,
        outcome: node.outcome,
        model: node.model,
        activity: node.activity,
        usage: None,
        transcript_path: node.transcript_path,
    }
}

fn state(arg: AgentStateArg) -> AgentState {
    match arg {
        AgentStateArg::Idle => AgentState::Idle,
        AgentStateArg::Working => AgentState::Working,
        AgentStateArg::Blocked => AgentState::Blocked,
    }
}

fn outcome(arg: OutcomeArg) -> NodeOutcome {
    match arg {
        OutcomeArg::Completed => NodeOutcome::Completed,
        OutcomeArg::Failed => NodeOutcome::Failed,
        OutcomeArg::Interrupted => NodeOutcome::Interrupted,
        OutcomeArg::Disconnected => NodeOutcome::Disconnected,
    }
}

/// The Hutan session this process runs in, from the environment the daemon gave it.
pub(crate) fn session_from_env() -> Option<SessionId> {
    std::env::var("HUTAN_SESSION_ID").ok()?.parse().ok()
}

/// What the agent (or the extension) wrote to standard input; `None` when there is none.
fn read_stdin() -> Option<String> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut text = String::new();
    stdin
        .by_ref()
        .take(MAX_HOOK_INPUT)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn nanos_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}
