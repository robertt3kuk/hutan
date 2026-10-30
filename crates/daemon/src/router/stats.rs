//! `agents.stats` (protocol 0.67, capability `agents.stats`): flow statistics across agents,
//! from what the daemon records (`crate::agents_stats`).

use std::collections::BTreeSet;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::SessionListFilter;
use hutan_protocol::stats::{AgentsStats, AgentsStatsRequest};
use hutan_protocol::task::TaskListRequest;

use super::Router;
use crate::agents_stats::{StatsInputs, build_stats, window};
use crate::registry::now_ms;

impl Router {
    /// The statistics; read-only. The tables are read once each for the window, on a blocking
    /// thread, and the answer is built from those rows alone.
    pub(super) async fn agents_stats(
        &self,
        req: AgentsStatsRequest,
    ) -> Result<AgentsStats, ErrorBody> {
        let now = now_ms();
        let (since, until) = window(&req, now);
        let infos = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions;
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<_, ErrorBody> {
            let failed = |error: hutan_storage::StorageError| {
                ErrorBody::new(ErrorCode::Internal, error.to_string())
            };
            let mut sessions = storage.sessions().list().map_err(failed)?;
            sessions.extend(
                storage
                    .sessions()
                    .list_closed()
                    .map_err(failed)?
                    .into_iter()
                    .map(|closed| closed.session),
            );
            let mut approvals = storage
                .approvals()
                .in_window(since, until)
                .map_err(failed)?;
            let known: BTreeSet<String> = approvals.iter().map(|a| a.id.to_string()).collect();
            approvals.extend(
                storage
                    .approvals()
                    .unresolved()
                    .map_err(failed)?
                    .into_iter()
                    .filter(|a| !known.contains(&a.id.to_string())),
            );
            let mut questions = storage
                .questions()
                .in_window(since, until)
                .map_err(failed)?;
            let known: BTreeSet<String> = questions.iter().map(|q| q.id.to_string()).collect();
            questions.extend(
                storage
                    .questions()
                    .unresolved()
                    .map_err(failed)?
                    .into_iter()
                    .filter(|q| !known.contains(&q.id.to_string())),
            );
            let log = storage.agent_state_log();
            Ok((
                storage.workspaces().list().map_err(failed)?,
                sessions,
                storage
                    .chat()
                    .turns_in_window(since, until)
                    .map_err(failed)?,
                approvals,
                questions,
                storage
                    .usage()
                    .turns_in_window(since, until)
                    .map_err(failed)?,
                log.latest_before(since).map_err(failed)?,
                log.in_window(since, until).map_err(failed)?,
                log.oldest_ms().map_err(failed)?,
            ))
        })
        .await
        .map_err(|error| ErrorBody::new(ErrorCode::Internal, error.to_string()))??;
        let (
            workspaces,
            sessions,
            turns,
            approvals,
            questions,
            usage,
            state_before,
            state_window,
            state_log_from_ms,
        ) = rows;
        let tasks = self.tasks.list(TaskListRequest::default()).tasks;
        build_stats(
            &StatsInputs {
                now_ms: now,
                workspaces: &workspaces,
                sessions: &sessions,
                infos: &infos,
                turns: &turns,
                approvals: &approvals,
                questions: &questions,
                usage: &usage,
                state_before: &state_before,
                state_window: &state_window,
                state_log_enabled: self.state_log.enabled(),
                state_log_from_ms,
                tasks: &tasks,
            },
            &req,
        )
    }
}
