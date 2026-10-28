//! `agent.insight` and `agent.measure_context` (protocol 0.59, capability `agent.insight`).

use hutan_core::SessionId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::insight::{
    AgentContextMeasured, AgentInsightRequest, AgentInsightResult, AgentMeasureContext,
};

use super::Router;
use crate::insight::measure_unsupported;
use hutan_core::insight::harness_insight;

impl Router {
    /// The insight of a chat session; `node_id` must be the session's main agent.
    pub(super) async fn agent_insight(
        &self,
        req: AgentInsightRequest,
    ) -> Result<AgentInsightResult, ErrorBody> {
        let target = self.registry.insight_target(req.session_id).await?;
        let main = self
            .main_node(req.session_id, req.node_id.as_deref())
            .await?;
        let mut insight = self.insight.snapshot(req.session_id, &target).await?;
        insight.node_id = main;
        Ok(AgentInsightResult { insight })
    }

    /// Asks the harness for its context by category and answers with the insight after it.
    pub(super) async fn measure_context(
        &self,
        req: AgentMeasureContext,
    ) -> Result<AgentContextMeasured, ErrorBody> {
        let target = self.registry.insight_target(req.session_id).await?;
        if !harness_insight(&target.harness).can_measure || target.assistant {
            return Err(measure_unsupported(&target.harness));
        }
        let before = self.insight.breakdown_stamp(req.session_id, &target).await;
        let turn_id = self
            .registry
            .chats()
            .measure_context(req.session_id)
            .await?;
        let mut insight = self.insight.snapshot(req.session_id, &target).await?;
        let after = insight
            .context
            .as_ref()
            .and_then(|context| context.breakdown_as_of_ms);
        insight.node_id = self.main_node(req.session_id, None).await.ok().flatten();
        Ok(AgentContextMeasured {
            turn_id,
            measured: after.is_some() && after != before,
            insight,
        })
    }

    /// The id of the session's main agent. `asked`, when given, must be it: a chat reports
    /// usage for its main agent only.
    async fn main_node(
        &self,
        session: SessionId,
        asked: Option<&str>,
    ) -> Result<Option<String>, ErrorBody> {
        let tree = self.registry.agent_tree(Some(session)).await?;
        let observation = tree.observations.into_iter().next();
        let root = observation.as_ref().map(|o| o.root.clone());
        match (asked, observation) {
            (None, _) => Ok(root),
            (Some(node), _) if root.as_deref() == Some(node) => Ok(root),
            (Some(node), Some(observation)) if observation.nodes.contains_key(node) => {
                Err(ErrorBody::new(
                    ErrorCode::Unsupported,
                    format!("{node} is a subagent: a chat reports usage for its main agent only"),
                ))
            }
            (Some(node), _) => Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("session {session} has no agent {node}"),
            )),
        }
    }
}
