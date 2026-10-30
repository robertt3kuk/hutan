//! `agents.board` (protocol 0.63, capability `agents.board`): the board of every agent, from the
//! daemon's own state (`crate::agents_board`).

use std::collections::BTreeMap;

use hutan_protocol::board::{AgentsBoard, AgentsBoardRequest};
use hutan_protocol::control::ErrorBody;
use hutan_protocol::messages::SessionListFilter;
use hutan_protocol::task::TaskListRequest;

use super::Router;
use crate::agents_board::{BoardInputs, build_board};
use crate::registry::now_ms;

impl Router {
    /// The board; read-only. Reads the registry, the pending requests and the tasks once each,
    /// so the answer is one moment's view.
    pub(super) async fn agents_board(
        &self,
        req: AgentsBoardRequest,
    ) -> Result<AgentsBoard, ErrorBody> {
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let sessions = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions;
        let trees: BTreeMap<_, _> = self
            .registry
            .agent_tree(None)
            .await?
            .observations
            .into_iter()
            .map(|tree| (tree.session_id, tree))
            .collect();
        let approvals = self.registry.chats().approval_list(None).approvals;
        let questions = self.registry.chats().question_list(None).questions;
        let tasks = self.tasks.list(TaskListRequest::default()).tasks;
        build_board(
            &BoardInputs {
                now_ms: now_ms(),
                workspaces: &workspaces,
                sessions: &sessions,
                approvals: &approvals,
                questions: &questions,
                trees: &trees,
                tasks: &tasks,
            },
            req,
        )
    }
}
