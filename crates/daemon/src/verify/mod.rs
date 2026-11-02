//! `agents.verify` (protocol 0.70, capability `agents.verify`, ADR 0025, design
//! `docs/spikes/2026-11-02-checks.md` §4): Hutan runs a session's tests itself and keeps the exit
//! code, so "the tests pass" is a record and not an agent's word (`docs/assistant.md` § Checks ›
//! Verification, `docs/control.md`).
//!
//! - [`VerifyService::plan`] resolves *what would run*: the command (the caller's, the
//!   project's `[checks.projects.<name or id>] test_command`, `[checks] test_command`, else
//!   [`detect`]), the folder (the session's worktree when it has one) and whether the run would be
//!   refused now. The control gate shows these words on the card before anything runs.
//! - [`VerifyService::run`] runs the plan in the user's login environment with a timeout, one
//!   run per session at a time, refused while the session's own turn is running unless the user
//!   forces it. The run belongs to the daemon: it ends by itself at the timeout, and it is
//!   killed when the caller waiting for it goes away, on `agents.verify.cancel` and when the
//!   daemon stops. It is stored (`verify_runs`) when it starts and when it ends, and both are
//!   events.

pub mod detect;
mod run;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hutan_core::SessionId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    AgentState, Event, SessionInfo, SessionListFilter, TerminalAgentState,
};
use hutan_protocol::verify::{
    AgentsVerifyPlanRequest, AgentsVerifyRequest, VERIFY_COMMAND_CHARS, VerifyPlan, VerifyRefusal,
    VerifyRequester, VerifyRun, VerifySource, VerifyStatus,
};
use hutan_storage::{Storage, VerifyRunEnd, VerifyRunRow};
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::bus::EventBus;
use crate::config::{ChecksSettings, TestCommandOrigin};
use crate::harness::Harnesses;
use crate::registry::{RegistryHandle, now_ms};
use crate::resources::{Actor, audit};

use run::{Ended, Spec};

/// A run in progress: what stops it.
#[derive(Debug)]
struct Active {
    cancel: CancellationToken,
}

/// Takes the session's place in the table when the run ends, however it ends.
struct Claim {
    service: Arc<VerifyService>,
    session: SessionId,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.service.running.lock().remove(&self.session);
    }
}

/// What [`VerifyService::resolve`] found.
struct Resolved {
    plan: VerifyPlan,
}

/// `agents.verify` over the daemon's state.
#[derive(Debug)]
pub struct VerifyService {
    registry: RegistryHandle,
    storage: Storage,
    bus: Arc<EventBus>,
    harnesses: Arc<Harnesses>,
    settings: ChecksSettings,
    shutdown: CancellationToken,
    running: Mutex<HashMap<SessionId, Active>>,
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

fn redacted(text: &str) -> String {
    hutan_index::redact::redact(text).into_owned()
}

impl VerifyService {
    /// A run that was in progress when the daemon last stopped is closed as an error: nothing
    /// survives a restart.
    pub fn new(
        registry: RegistryHandle,
        storage: Storage,
        bus: Arc<EventBus>,
        harnesses: Arc<Harnesses>,
        settings: ChecksSettings,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        if let Err(error) = storage.verify_runs().abandon_running(now_ms()) {
            warn!(%error, "could not close the verify runs a stopped daemon left");
        }
        Arc::new(Self {
            registry,
            storage,
            bus,
            harnesses,
            settings,
            shutdown,
            running: Mutex::new(HashMap::new()),
        })
    }

    /// `agents.verify.plan`: what [`run`](Self::run) would do for this request now.
    pub async fn plan(&self, req: AgentsVerifyPlanRequest) -> Result<VerifyPlan, ErrorBody> {
        Ok(self
            .resolve(req.session_id, req.command.as_deref())
            .await?
            .plan)
    }

    /// `agents.verify.cancel`: kills the run of `session`.
    pub fn cancel(&self, session: SessionId) -> Result<(), ErrorBody> {
        match self.running.lock().get(&session) {
            Some(active) => {
                active.cancel.cancel();
                Ok(())
            }
            None => Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("no verification run of session {session} is in progress"),
            )),
        }
    }

    /// `agents.verify`: runs the plan and answers when the run is over. `requested_by` is who
    /// asked, as the gate decided (the user, Mawas after her card, another agent); `gone` is the
    /// caller's connection: a caller that goes away takes its run with it.
    pub async fn run(
        self: &Arc<Self>,
        actor: Actor,
        requested_by: VerifyRequester,
        req: AgentsVerifyRequest,
        gone: &CancellationToken,
    ) -> Result<VerifyRun, ErrorBody> {
        let session = req.session_id;
        let outcome = self.run_inner(requested_by, req, gone).await;
        audit(actor, "agents.verify", &session.to_string(), &outcome);
        outcome
    }

    async fn run_inner(
        self: &Arc<Self>,
        requested_by: VerifyRequester,
        req: AgentsVerifyRequest,
        gone: &CancellationToken,
    ) -> Result<VerifyRun, ErrorBody> {
        if req.force && requested_by != VerifyRequester::User {
            return Err(ErrorBody::new(
                ErrorCode::Forbidden,
                "`force` is the user's alone: an agent cannot run the tests while the session's \
                 own turn is running",
            ));
        }
        let Resolved { plan } = self.resolve(req.session_id, req.command.as_deref()).await?;
        if let Some(error) = refusal_error(&plan, req.force) {
            return Err(error);
        }
        let cancel = CancellationToken::new();
        // The place in the table is taken before the row is written: two requests cannot both
        // pass the check above.
        {
            let mut running = self.running.lock();
            if running.contains_key(&req.session_id) {
                return Err(invalid_state(format!(
                    "a verification run of session {} is already in progress",
                    req.session_id
                )));
            }
            running.insert(
                req.session_id,
                Active {
                    cancel: cancel.clone(),
                },
            );
        }
        let claim = Claim {
            service: self.clone(),
            session: req.session_id,
        };
        let started_ms = now_ms();
        let mut run = VerifyRun {
            id: 0,
            session_id: req.session_id,
            command: redacted(&plan.command),
            cwd: plan.cwd.clone(),
            source: plan.source,
            why: plan.why.clone(),
            status: VerifyStatus::Running,
            exit_code: None,
            signal: None,
            started_ms,
            ended_ms: None,
            duration_ms: None,
            output_tail: String::new(),
            output_truncated: false,
            requested_by,
            forced: req.force && plan.refused == Some(VerifyRefusal::TurnRunning),
            error: None,
        };
        run.id = self
            .storage
            .verify_runs()
            .start(row_of(&run))
            .map_err(|error| {
                ErrorBody::new(
                    ErrorCode::Internal,
                    format!("could not record the verification run: {error}"),
                )
            })?;
        self.publish(Event::AgentsVerifyStarted {
            run: Box::new(run.clone()),
        });

        let service = self.clone();
        let gone = gone.clone();
        let (done, answer) = tokio::sync::oneshot::channel();
        let command = plan.command.clone();
        let cwd = plan.cwd.clone();
        let timeout = Duration::from_secs(plan.timeout_secs);
        tokio::spawn(async move {
            let _claim = claim;
            let harnesses = service.harnesses.clone();
            let env = tokio::task::spawn_blocking(move || harnesses.environment())
                .await
                .unwrap_or_default();
            let stop = [cancel, gone, service.shutdown.clone()];
            let outcome = run::run(
                Spec {
                    command,
                    cwd,
                    env,
                    timeout,
                },
                &stop,
            )
            .await;
            let ended = service.finish(run, outcome);
            // The caller may be gone; the run is stored and announced all the same.
            let _ = done.send(ended);
        });
        answer
            .await
            .map_err(|_| ErrorBody::new(ErrorCode::Internal, "the verification run was lost"))
    }

    /// Stores and announces the end of `run`.
    fn finish(&self, mut run: VerifyRun, outcome: run::Outcome) -> VerifyRun {
        let ended_ms = now_ms();
        run.ended_ms = Some(ended_ms);
        run.duration_ms = Some(ended_ms.saturating_sub(run.started_ms));
        run.output_tail = outcome.tail;
        run.output_truncated = outcome.truncated;
        match outcome.ended {
            Ended::Exited { code, signal } => {
                run.exit_code = code;
                run.signal = signal;
                run.status = if code == Some(0) {
                    VerifyStatus::Passed
                } else {
                    VerifyStatus::Failed
                };
            }
            Ended::TimedOut => run.status = VerifyStatus::TimedOut,
            Ended::Cancelled => run.status = VerifyStatus::Cancelled,
            Ended::StartFailed(why) => {
                run.status = VerifyStatus::Error;
                run.error = Some(redacted(&why));
            }
        }
        let end = VerifyRunEnd {
            status: run.status.as_str().to_owned(),
            exit_code: run.exit_code,
            signal: run.signal,
            ended_ms,
            duration_ms: run.duration_ms.unwrap_or(0),
            output_tail: run.output_tail.clone(),
            output_truncated: run.output_truncated,
            error: run.error.clone(),
        };
        if let Err(error) = self.storage.verify_runs().finish(run.id, end) {
            warn!(%error, run = run.id, "could not record the end of a verification run");
        }
        self.publish(Event::AgentsVerifyEnded {
            run: Box::new(run.clone()),
        });
        run
    }

    fn publish(&self, event: Event) {
        if let Err(error) = self.bus.publish(&event) {
            warn!(%error, "could not publish a verification event");
        }
    }

    /// What would run for `session` with `command`, and whether it would be refused.
    async fn resolve(
        &self,
        session_id: SessionId,
        command: Option<&str>,
    ) -> Result<Resolved, ErrorBody> {
        let session = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions
            .into_iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| {
                ErrorBody::new(ErrorCode::NotFound, format!("no session {session_id}"))
            })?;
        let (dirs, base) = folders(&session).ok_or_else(|| {
            invalid_state(format!(
                "session {session_id} has no folder to run the tests in"
            ))
        })?;
        if !base.is_dir() {
            return Err(invalid_state(format!(
                "the folder of session {session_id}, {}, does not exist (any more)",
                base.display()
            )));
        }
        let workspace_name = self
            .registry
            .list_workspaces()
            .await?
            .workspaces
            .into_iter()
            .find(|workspace| workspace.id == session.workspace_id)
            .map(|workspace| workspace.name)
            .unwrap_or_default();
        let workspace_id = session.workspace_id.to_string();
        let timeout_secs = self
            .settings
            .timeout_secs_for(&workspace_id, &workspace_name);

        let given = match command {
            Some(command) => {
                let command = command.trim();
                if command.is_empty() || command.chars().count() > VERIFY_COMMAND_CHARS {
                    return Err(ErrorBody::new(
                        ErrorCode::BadRequest,
                        format!("the command must be 1 to {VERIFY_COMMAND_CHARS} characters"),
                    ));
                }
                Some((
                    command.to_owned(),
                    VerifySource::Caller,
                    "given with the request".to_owned(),
                ))
            }
            None => None,
        };
        let (command, cwd, source, why) = if let Some((command, source, why)) = given {
            (command, base.clone(), source, why)
        } else if let Some((command, origin)) = self
            .settings
            .test_command_for(&workspace_id, &workspace_name)
        {
            let (source, why) = match origin {
                TestCommandOrigin::Project(key) => (
                    VerifySource::Project,
                    format!("`[checks.projects.{key}] test_command` of daemon.toml"),
                ),
                TestCommandOrigin::Global => (
                    VerifySource::Setting,
                    "`[checks] test_command` of daemon.toml".to_owned(),
                ),
            };
            (command, base.clone(), source, why)
        } else {
            // Reading a few small files: off the async threads.
            let found = {
                let dirs = dirs.clone();
                tokio::task::spawn_blocking(move || detect::detect(&dirs))
                    .await
                    .unwrap_or_default()
            };
            let found = found.ok_or_else(|| {
                invalid_state(format!(
                    "no test command found for session {session_id}: looked in {} for {}; set \
                     `test_command` under `[checks]` (or `[checks.projects.<name>]`) in \
                     daemon.toml, or give a command",
                    base.display(),
                    detect::LOOKED_FOR
                ))
            })?;
            (found.command, found.dir, VerifySource::Detected, found.why)
        };

        let refused = if self.running.lock().contains_key(&session_id) {
            Some(VerifyRefusal::Running)
        } else if turn_running(&session) {
            Some(VerifyRefusal::TurnRunning)
        } else {
            None
        };
        Ok(Resolved {
            plan: VerifyPlan {
                session_id,
                command,
                cwd,
                source,
                why,
                refused,
                timeout_secs,
            },
        })
    }
}

/// The error a plan earns now, if it would be refused: a run of the session is in progress, or
/// its own turn is running and the caller does not force.
pub fn refusal_error(plan: &VerifyPlan, force: bool) -> Option<ErrorBody> {
    match plan.refused? {
        VerifyRefusal::Running => Some(invalid_state(format!(
            "a verification run of session {} is already in progress",
            plan.session_id
        ))),
        VerifyRefusal::TurnRunning if force => None,
        VerifyRefusal::TurnRunning => {
            let mut error = invalid_state(
                "the session's own turn is running in this checkout, so a test run would race \
                 the agent; wait for the turn to end (the user can force it)",
            );
            error.data = Some(json!({"verify": {"refused": "turn_running"}}));
            Some(error)
        }
    }
}

fn row_of(run: &VerifyRun) -> VerifyRunRow {
    VerifyRunRow {
        id: run.id,
        session_id: run.session_id,
        command: run.command.clone(),
        cwd: run.cwd.to_string_lossy().into_owned(),
        source: run.source.as_str().to_owned(),
        why: run.why.clone(),
        status: run.status.as_str().to_owned(),
        exit_code: run.exit_code,
        signal: run.signal,
        started_ms: run.started_ms,
        ended_ms: run.ended_ms,
        duration_ms: run.duration_ms,
        output_tail: run.output_tail.clone(),
        output_truncated: run.output_truncated,
        requested_by: run.requested_by.as_str().to_owned(),
        forced: run.forced,
        error: run.error.clone(),
    }
}

/// The folders to look for a test command in (the session's own, then its parents up to the
/// checkout), and the checkout (a worktree's root, else the session's folder) a configured or
/// given command runs in.
fn folders(session: &SessionInfo) -> Option<(Vec<PathBuf>, PathBuf)> {
    let folder = session.folder.clone().or_else(|| session.cwd.clone())?;
    let root = session
        .placement
        .as_ref()
        .map(|placement| placement.root.clone())
        .filter(|root| folder.starts_with(root));
    let base = root.unwrap_or_else(|| folder.clone());
    let mut dirs = vec![folder.clone()];
    let mut dir: &Path = &folder;
    while dir != base {
        match dir.parent() {
            Some(parent) if parent.starts_with(&base) => {
                dirs.push(parent.to_path_buf());
                dir = parent;
            }
            _ => break,
        }
    }
    Some((dirs, base))
}

/// The session's own agent is working now (a chat's turn, a terminal agent that says so).
fn turn_running(session: &SessionInfo) -> bool {
    session.state.is_live()
        && (session.chat.as_ref().is_some_and(|chat| chat.turn_active)
            || session
                .agent
                .as_ref()
                .is_some_and(|report| report.state == AgentState::Working)
            || session
                .terminal_agent
                .as_ref()
                .is_some_and(|agent| agent.state == TerminalAgentState::Working))
}

#[cfg(test)]
mod tests {
    use hutan_core::{SessionKind, SessionState, WorkspaceId};
    use hutan_protocol::worktree::{Placement, PlacementKind};

    use super::*;

    fn session(folder: &str, root: Option<&str>) -> SessionInfo {
        SessionInfo {
            forked_from: None,
            extra_dirs: Vec::new(),
            containment: None,
            terminal_agent: None,
            id: SessionId::new(),
            workspace_id: WorkspaceId::new(),
            kind: SessionKind::Terminal,
            title: "t".into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: None,
            created_at_ms: 1,
            agent: None,
            restored: false,
            chat: None,
            placement: root.map(|root| Placement {
                kind: PlacementKind::Worktree,
                root: root.into(),
                branch: None,
                shared_with: Vec::new(),
            }),
            folder: Some(folder.into()),
            recovery: None,
            muted: false,
        }
    }

    #[test]
    fn the_folders_are_the_session_folder_and_its_parents_up_to_the_checkout() {
        let (dirs, base) = folders(&session("/w/repo/crates/x", Some("/w/repo"))).unwrap();
        assert_eq!(base, PathBuf::from("/w/repo"));
        assert_eq!(
            dirs,
            [
                PathBuf::from("/w/repo/crates/x"),
                PathBuf::from("/w/repo/crates"),
                PathBuf::from("/w/repo")
            ]
        );
        // No placement, or a folder outside its root: just the folder.
        let (dirs, base) = folders(&session("/w/other", None)).unwrap();
        assert_eq!((dirs, base), (vec!["/w/other".into()], "/w/other".into()));
        let (dirs, base) = folders(&session("/w/other", Some("/w/repo"))).unwrap();
        assert_eq!((dirs, base), (vec!["/w/other".into()], "/w/other".into()));
    }
}
