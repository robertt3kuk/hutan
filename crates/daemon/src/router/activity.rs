//! `agents.activity` (protocol 0.65, capability `agents.activity`): what happened across agents
//! in a window, from what the daemon records (`crate::agents_activity`).

use hutan_protocol::activity::{AgentsActivity, AgentsActivityRequest};
use hutan_protocol::control::ErrorBody;

use super::Router;
use crate::agents_activity::read_activity;

impl Router {
    /// The catch-up; read-only (`crate::agents_activity::read_activity`).
    pub(super) async fn agents_activity(
        &self,
        req: AgentsActivityRequest,
    ) -> Result<AgentsActivity, ErrorBody> {
        read_activity(&self.storage, &self.tasks, &req).await
    }
}
