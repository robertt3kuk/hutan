//! `agents.stats` (protocol 0.66, capability `agents.stats`): flow statistics across agents,
//! from what the daemon records (`crate::agents_stats`).

use hutan_protocol::control::ErrorBody;
use hutan_protocol::stats::{AgentsStats, AgentsStatsRequest};

use super::Router;
use crate::agents_stats::read_stats;

impl Router {
    /// The statistics; read-only (`crate::agents_stats::read_stats`).
    pub(super) async fn agents_stats(
        &self,
        req: AgentsStatsRequest,
    ) -> Result<AgentsStats, ErrorBody> {
        read_stats(
            &self.storage,
            &self.tasks,
            &self.registry,
            self.state_log.enabled(),
            &req,
        )
        .await
    }
}
