//! The mapper task: one task takes the accepted requests off the bounded queue, decodes them,
//! maps them with the session's [`Mapper`] and gives the reports to the session registry as
//! insight reports (`source: otlp`). Everything the request path must never wait for happens
//! here.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use hutan_core::SessionId;
use hutan_protocol::messages::AgentReportRequest;
use hutan_telemetry::{Agent, DecodeError, Mapped, Mapper, decode, inflate};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::{Job, Shared};
use crate::registry::{RegistryHandle, now_ms};

/// The `source` of the reports the receiver hands over: what `seq` is counted per.
const SOURCE: &str = "otlp";

struct SessionState {
    mapper: Mapper,
    /// The `seq` of the last report given to the registry; the next is larger.
    seq: u64,
}

pub(super) struct Worker {
    pub shared: std::sync::Arc<Shared>,
    pub registry: RegistryHandle,
    pub rx: mpsc::Receiver<Job>,
    pub idle_close: Duration,
}

impl Worker {
    pub(super) async fn run(mut self, shutdown: CancellationToken) {
        let mut sessions: HashMap<SessionId, SessionState> = HashMap::new();
        loop {
            // A turn the logs never end is closed when it has been quiet for `idle_close`.
            let deadline = sessions
                .values()
                .filter_map(|state| state.mapper.next_idle_at(self.idle_ms()))
                .min()
                .map(|at| Duration::from_millis(at.saturating_sub(now_ms())));
            let job = tokio::select! {
                () = shutdown.cancelled() => return,
                job = self.rx.recv() => match job {
                    Some(job) => Some(job),
                    None => return,
                },
                () = sleep_for(deadline) => None,
            };
            match job {
                Some(job) => self.map(&mut sessions, job).await,
                None => self.close_idle(&mut sessions).await,
            }
        }
    }

    fn idle_ms(&self) -> u64 {
        u64::try_from(self.idle_close.as_millis()).unwrap_or(u64::MAX)
    }

    async fn map(&self, sessions: &mut HashMap<SessionId, SessionState>, job: Job) {
        // The test hook that holds the mapper still: the request path must not notice.
        let _gate = match &self.shared.tuning.mapper_gate {
            Some(gate) => gate.clone().acquire_owned().await.ok(),
            None => None,
        };
        // A session that was closed since the request was queued has nothing to receive it.
        if !self.shared.env.has_token(job.session) {
            self.shared.counters.ignored.fetch_add(1, Ordering::Relaxed);
            sessions.remove(&job.session);
            return;
        }
        let decoded = match self.decode(&job) {
            Ok(decoded) => decoded,
            Err(error) => {
                debug!(%error, "an OTLP request did not decode");
                self.shared.counters.dropped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if decoded.dropped > 0 {
            self.shared.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        let enabled = self.shared.enabled();
        // The mappers of sessions that were closed do not pile up.
        if sessions.len() > 64 {
            sessions.retain(|id, _| self.shared.env.has_token(*id));
        }
        let state = sessions.entry(job.session).or_insert_with(|| SessionState {
            mapper: Mapper::new(),
            seq: 0,
        });
        let mapped = state.mapper.map(
            &decoded,
            &|agent: Agent| enabled.contains(&agent),
            job.at_ms,
        );
        if mapped.is_empty() {
            self.shared.counters.ignored.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.deliver(job.session, state, mapped).await;
    }

    fn decode(&self, job: &Job) -> Result<hutan_telemetry::Decoded, DecodeError> {
        let body = if job.gzip {
            inflate(&job.body)?.into()
        } else {
            job.body.clone()
        };
        decode(job.signal, job.encoding, body)
    }

    async fn close_idle(&self, sessions: &mut HashMap<SessionId, SessionState>) {
        // Sessions that were closed have nothing to close.
        sessions.retain(|session, _| self.shared.env.has_token(*session));
        let now = now_ms();
        let ids: Vec<SessionId> = sessions.keys().copied().collect();
        for session in ids {
            let Some(state) = sessions.get_mut(&session) else {
                continue;
            };
            let closed = state.mapper.close_idle(now, self.idle_ms());
            if !closed.is_empty() {
                self.deliver(session, state, closed).await;
            }
        }
    }

    /// Gives the reports to the session's registry entry, in order, and notes the session in the
    /// status.
    async fn deliver(&self, session: SessionId, state: &mut SessionState, mapped: Vec<Mapped>) {
        for group in mapped {
            let mut delivered = false;
            for report in group.reports {
                state.seq = state.seq.max(now_ms().saturating_sub(1)) + 1;
                let request = AgentReportRequest {
                    session_id: session,
                    source: SOURCE.to_owned(),
                    agent: group.name.clone(),
                    state: None,
                    message: None,
                    seq: state.seq,
                    agent_session_id: None,
                    resume_argv: None,
                    node: None,
                    pid: None,
                    insight: Some(report),
                };
                // The session may have ended meanwhile: nothing to tell then.
                match self.registry.report_agent(request).await {
                    Ok(applied) => delivered |= applied.applied,
                    Err(error) => {
                        debug!(message = %error.message, "an OTLP report was not taken");
                    }
                }
            }
            if delivered {
                self.shared
                    .note_report(session, group.name, group.service_version, group.model);
            }
        }
    }
}

/// Sleeps `deadline`, or forever when there is none.
async fn sleep_for(deadline: Option<Duration>) {
    match deadline {
        Some(after) => tokio::time::sleep(after).await,
        None => std::future::pending::<()>().await,
    }
}
