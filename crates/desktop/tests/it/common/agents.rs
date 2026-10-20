//! Agent reports in GPUI tests: a [`Reporter`] that plays one reporting integration (`omp`,
//! `claude-hooks`, …) against the in-process daemon, building the node reports the integrations
//! send (`hutan agent report --node …`) and numbering them the way the daemon wants.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::collections::HashMap;

use hutan_core::{NodeKind, NodeOutcome, SessionId, Usage};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, Request,
};
use hutan_protocol::tree::NodeReport;

use super::TestDaemon;

/// One node report under construction.
#[derive(Clone)]
pub struct Node {
    report: NodeReport,
    state: Option<AgentState>,
    resume_argv: Option<Vec<String>>,
}

impl Node {
    /// The main agent of the session.
    pub fn main(id: &str) -> Self {
        Self::new(id, NodeKind::Main, None)
    }

    /// A subagent of `parent`.
    pub fn sub(id: &str, parent: &str) -> Self {
        Self::new(id, NodeKind::Sub, Some(parent))
    }

    fn new(id: &str, kind: NodeKind, parent: Option<&str>) -> Self {
        Self {
            report: NodeReport {
                id: id.to_owned(),
                parent: parent.map(str::to_owned),
                kind,
                name: None,
                agent_type: None,
                outcome: None,
                model: None,
                activity: None,
                usage: None,
                transcript_path: None,
            },
            state: None,
            resume_argv: None,
        }
    }

    pub fn state(mut self, state: AgentState) -> Self {
        self.state = Some(state);
        self
    }

    pub fn working(self) -> Self {
        self.state(AgentState::Working)
    }

    pub fn blocked(self) -> Self {
        self.state(AgentState::Blocked)
    }

    pub fn idle(self) -> Self {
        self.state(AgentState::Idle)
    }

    pub fn name(mut self, name: &str) -> Self {
        self.report.name = Some(name.to_owned());
        self
    }

    pub fn agent_type(mut self, agent_type: &str) -> Self {
        self.report.agent_type = Some(agent_type.to_owned());
        self
    }

    pub fn model(mut self, model: &str) -> Self {
        self.report.model = Some(model.to_owned());
        self
    }

    pub fn activity(mut self, activity: &str) -> Self {
        self.report.activity = Some(activity.to_owned());
        self
    }

    pub fn usage(mut self, usage: Usage) -> Self {
        self.report.usage = Some(usage);
        self
    }

    pub fn transcript(mut self, path: &str) -> Self {
        self.report.transcript_path = Some(path.to_owned());
        self
    }

    /// Finishes the node (its state becomes idle).
    pub fn finished(mut self, outcome: NodeOutcome) -> Self {
        self.report.outcome = Some(outcome);
        self.state = Some(AgentState::Idle);
        self
    }

    /// How the session starts again after a daemon restart (main agent only).
    pub fn resume(mut self, argv: &[&str]) -> Self {
        self.resume_argv = Some(argv.iter().map(|a| (*a).to_owned()).collect());
        self
    }
}

/// One reporting integration of one session.
pub struct Reporter {
    session: SessionId,
    source: String,
    agent: String,
    seqs: RefCell<HashMap<String, u64>>,
}

impl Reporter {
    pub fn new(session: SessionId, source: &str, agent: &str) -> Self {
        Self {
            session,
            source: source.to_owned(),
            agent: agent.to_owned(),
            seqs: RefCell::default(),
        }
    }

    #[allow(dead_code)]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Numbers start afresh after a daemon restart (a new epoch).
    pub fn restart_numbering(&self) {
        self.seqs.borrow_mut().clear();
    }

    fn next(&self, node: &str) -> u64 {
        let mut seqs = self.seqs.borrow_mut();
        let seq = seqs.entry(node.to_owned()).or_insert(0);
        *seq += 1;
        *seq
    }

    /// Sends the report; panics when the daemon ignores it.
    pub fn report(&self, daemon: &TestDaemon, node: Node) {
        let seq = self.next(&node.report.id);
        let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: self.session,
            source: self.source.clone(),
            agent: self.agent.clone(),
            state: node.state,
            message: None,
            seq,
            agent_session_id: None,
            resume_argv: node.resume_argv,
            node: Some(node.report),
        }));
        assert!(applied.applied, "the daemon ignored a report");
    }

    /// `agent.release` of `node` (the main agent when `None`).
    pub fn release(&self, daemon: &TestDaemon, node: Option<&str>, outcome: Option<NodeOutcome>) {
        let seq = self.next(node.unwrap_or("<main>"));
        let applied: AgentApplied = daemon.call(Request::AgentRelease(AgentReleaseRequest {
            session_id: self.session,
            source: self.source.clone(),
            seq,
            node: node.map(str::to_owned),
            outcome,
        }));
        assert!(applied.applied, "the daemon ignored a release");
    }
}

pub fn usage_context(tokens: u64) -> Usage {
    Usage {
        kind: "claude.context".to_owned(),
        context_tokens: Some(tokens),
        ..Usage::default()
    }
}
