//! `agents.activity` (protocol 0.65, capability `agents.activity`): what happened across agents
//! in a window, from what the daemon records (`crate::agents_activity`).

use hutan_protocol::activity::{AgentsActivity, AgentsActivityRequest};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::task::TaskListRequest;

use super::Router;
use crate::agents_activity::{ActivityInputs, build_activity, window};
use crate::registry::now_ms;

impl Router {
    /// The catch-up; read-only. The tables are read once each for the window, on a blocking
    /// thread (they are SQLite reads), and the answer is built from those rows alone.
    pub(super) async fn agents_activity(
        &self,
        req: AgentsActivityRequest,
    ) -> Result<AgentsActivity, ErrorBody> {
        let now = now_ms();
        let (since, until) = window(&req, now);
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
            Ok((
                storage.workspaces().list().map_err(failed)?,
                sessions,
                storage
                    .approvals()
                    .in_window(since, until)
                    .map_err(failed)?,
                storage
                    .questions()
                    .in_window(since, until)
                    .map_err(failed)?,
                storage
                    .chat()
                    .turns_in_window(since, until)
                    .map_err(failed)?,
                storage
                    .checkpoints()
                    .in_window(since, until)
                    .map_err(failed)?,
            ))
        })
        .await
        .map_err(|error| ErrorBody::new(ErrorCode::Internal, error.to_string()))??;
        let (workspaces, sessions, approvals, questions, turns, checkpoints) = rows;
        let tasks = self.tasks.list(TaskListRequest::default()).tasks;
        build_activity(
            &ActivityInputs {
                now_ms: now,
                workspaces: &workspaces,
                sessions: &sessions,
                approvals: &approvals,
                questions: &questions,
                turns: &turns,
                checkpoints: &checkpoints,
                tasks: &tasks,
            },
            &req,
        )
    }
}
