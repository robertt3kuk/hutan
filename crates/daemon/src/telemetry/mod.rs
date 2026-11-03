//! The loopback OTLP receiver (protocol 0.74, capability `agent.telemetry`, ADR 0026;
//! `docs/integrations.md` § Report your agent's stats to Hutan): the daemon's first TCP
//! listener, off until the user enables an agent.
//!
//! - **Off by default.** No socket exists until `[telemetry.agents] <agent> = true` (a
//!   user-only setting: `daemon.settings.set`, `hutan telemetry enable <agent>`). The listener
//!   binds `127.0.0.1` only when the first agent is enabled and closes with the last. Its port
//!   is `[telemetry] port`; `0` means *picked once at random in the ephemeral range and written
//!   back to `daemon.toml`*, because Codex's endpoint lives in a config file. A busy port leaves
//!   the receiver [down and visible](hutan_protocol::telemetry::ListenerState::Down) — it never
//!   binds another port in its place.
//! - **A process reports only as its own session.** A terminal that starts while the receiver
//!   listens gets `HUTAN_OTLP_ENDPOINT` and `HUTAN_OTLP_TOKEN` ([`SessionEnv`]); the token
//!   selects the session, nothing in a payload does.
//! - **Always answers at once.** [`http`] checks, reads under limits, queues and answers; the
//!   decoding and mapping ([`worker`], over `hutan-telemetry`) run on one task behind a bounded
//!   queue.
//! - **No authority.** The only output is an insight report (`source: otlp`) given to the
//!   session registry through `report_agent`, exactly as a hook's report is: it cannot set a
//!   state, an approval, a task or a notification.

mod env;
mod http;
mod worker;

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use bytes::Bytes;
use hutan_core::SessionId;
use hutan_protocol::telemetry::{
    ListenerState, TelemetryAgent, TelemetryCounters, TelemetrySession, TelemetryStatus,
};
use hutan_telemetry::{Agent, Signal};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub use env::{ENDPOINT_ENV, SessionEnv, TOKEN_ENV};

use crate::config::{self, TelemetrySettings};
use crate::registry::{RegistryHandle, now_ms};

/// Requests waiting for the mapper; past this a request is answered `429`.
pub const QUEUE_CAPACITY: usize = 64;
/// How long a turn that Claude Code's or Codex's logs began may be quiet before it is closed.
pub const IDLE_CLOSE: Duration = Duration::from_secs(30);
/// Sessions the status lists.
const MAX_STATUS_SESSIONS: usize = 64;
/// The range a port is picked from (the IANA ephemeral range).
const PORT_RANGE: std::ops::RangeInclusive<u16> = 49152..=65535;
/// Ports tried before a pick gives up.
const PICK_ATTEMPTS: usize = 32;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No critical section leaves the state half updated across a panic point.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A token bucket's size and speed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub burst: f64,
    pub per_sec: f64,
}

/// What the tests change of the receiver (the daemon's defaults are ADR 0026 decision 5).
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Requests the mapper's queue holds.
    pub queue: usize,
    /// How long a turn the logs never end stays open.
    pub idle_close: Duration,
    /// Requests a session may make: ≈ 20 a second.
    pub requests: Rate,
    /// Bytes a session may send: ≈ 1 MiB a second.
    pub bytes: Rate,
    /// Holds the mapper still while it has no permit: a test that proves the request path never
    /// waits for it. `None`: the mapper never waits.
    pub mapper_gate: Option<Arc<Semaphore>>,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            queue: QUEUE_CAPACITY,
            idle_close: IDLE_CLOSE,
            requests: Rate {
                burst: 40.0,
                per_sec: 20.0,
            },
            bytes: Rate {
                burst: 2.0 * 1024.0 * 1024.0,
                per_sec: 1024.0 * 1024.0,
            },
            mapper_gate: None,
        }
    }
}

/// One accepted request on its way to the mapper.
#[derive(Debug)]
pub(crate) struct Job {
    pub session: SessionId,
    pub signal: Signal,
    pub encoding: hutan_telemetry::Encoding,
    pub gzip: bool,
    pub body: Bytes,
    pub at_ms: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
    pub dropped: AtomicU64,
    pub ignored: AtomicU64,
}

/// What the request path and the mapper share.
#[derive(Debug)]
pub(crate) struct Shared {
    pub env: Arc<SessionEnv>,
    pub tuning: Tuning,
    pub counters: Counters,
    pub queue: mpsc::Sender<Job>,
    enabled: RwLock<HashSet<Agent>>,
    pub limits: Mutex<HashMap<SessionId, http::SessionLimits>>,
    sessions: Mutex<HashMap<SessionId, TelemetrySession>>,
}

impl Shared {
    /// The agents the user enabled.
    pub fn enabled(&self) -> HashSet<Agent> {
        self.enabled
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A request of `session` mapped to insight.
    pub fn note_report(
        &self,
        session: SessionId,
        agent: String,
        service_version: Option<String>,
        model: Option<String>,
    ) {
        let mut sessions = lock(&self.sessions);
        let entry = sessions.entry(session).or_insert_with(|| TelemetrySession {
            session_id: session,
            agent: agent.clone(),
            last_report_ms: 0,
            reports: 0,
            service_version: None,
            model: None,
        });
        entry.agent = agent;
        entry.last_report_ms = now_ms();
        entry.reports += 1;
        if service_version.is_some() {
            entry.service_version = service_version;
        }
        if model.is_some() {
            entry.model = model;
        }
        if sessions.len() > MAX_STATUS_SESSIONS
            && let Some(oldest) = sessions
                .values()
                .min_by_key(|entry| entry.last_report_ms)
                .map(|entry| entry.session_id)
        {
            sessions.remove(&oldest);
        }
    }
}

/// What starts the receiver.
#[derive(Debug)]
pub struct TelemetryInputs {
    pub env: Arc<SessionEnv>,
    /// `daemon.toml`: where a picked port is written.
    pub config_file: PathBuf,
    pub settings: TelemetrySettings,
    pub tuning: Tuning,
    pub shutdown: CancellationToken,
}

#[derive(Debug)]
struct Listener {
    port: u16,
    cancel: CancellationToken,
}

#[derive(Debug)]
struct Control {
    settings: TelemetrySettings,
    listener: Option<Listener>,
    /// Why the receiver is down although an agent is enabled.
    down: Option<String>,
}

/// The receiver: its settings, its listener and its mapper.
#[derive(Debug)]
pub struct TelemetryService {
    shared: Arc<Shared>,
    control: tokio::sync::Mutex<Control>,
    config_file: PathBuf,
    shutdown: CancellationToken,
    /// The queue's receiving end, until [`attach`](TelemetryService::attach) gives it to the
    /// mapper task.
    pending: Mutex<Option<mpsc::Receiver<Job>>>,
    idle_close: Duration,
}

impl TelemetryService {
    /// Opens the receiver: when `daemon.toml` enables an agent, the listener binds now, so that
    /// the terminals the registry restores right away already get their endpoint. Requests that
    /// arrive before [`attach`](Self::attach) wait in the queue.
    pub async fn open(inputs: TelemetryInputs) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(inputs.tuning.queue.max(1));
        let shared = Arc::new(Shared {
            env: inputs.env,
            tuning: inputs.tuning.clone(),
            counters: Counters::default(),
            queue: tx,
            enabled: RwLock::new(HashSet::new()),
            limits: http::new_limits(),
            sessions: Mutex::new(HashMap::new()),
        });
        let service = Arc::new(Self {
            shared,
            control: tokio::sync::Mutex::new(Control {
                settings: TelemetrySettings::default(),
                listener: None,
                down: None,
            }),
            config_file: inputs.config_file,
            shutdown: inputs.shutdown,
            pending: Mutex::new(Some(rx)),
            idle_close: inputs.tuning.idle_close,
        });
        service.apply_settings(inputs.settings).await;
        service
    }

    /// Starts the mapper task, which gives the reports to `registry`. Once; a second call does
    /// nothing.
    pub fn attach(&self, registry: RegistryHandle) {
        let Some(rx) = lock(&self.pending).take() else {
            return;
        };
        let worker = worker::Worker {
            shared: self.shared.clone(),
            registry,
            rx,
            idle_close: self.idle_close,
        };
        tokio::spawn(worker.run(self.shutdown.clone()));
    }

    /// The settings the receiver runs on now: the user's, with the port that was picked.
    pub async fn settings(&self) -> TelemetrySettings {
        self.control.lock().await.settings.clone()
    }

    /// Takes new settings: the enabled agents at once, and the listener opened, closed or moved
    /// to what they say.
    pub async fn apply_settings(&self, settings: TelemetrySettings) {
        let mut control = self.control.lock().await;
        *self
            .shared
            .enabled
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings.enabled().into_iter().collect();
        control.settings = settings;
        self.reconcile(&mut control).await;
    }

    /// Makes the listener what the settings say.
    async fn reconcile(&self, control: &mut Control) {
        if control.settings.enabled().is_empty() {
            self.close(control);
            control.down = None;
            return;
        }
        let wanted = control.settings.port;
        if let Some(listener) = &control.listener {
            if wanted == 0 || wanted == listener.port {
                // The port that was picked stays.
                control.settings.port = listener.port;
                return;
            }
            self.close(control);
        }
        let bound = if wanted == 0 {
            pick_and_bind().await
        } else {
            bind(wanted).await.map(|listener| (listener, wanted))
        };
        let (listener, port) = match bound {
            Ok(bound) => bound,
            Err(reason) => {
                warn!(%reason, "the OTLP receiver is down");
                control.down = Some(reason);
                self.shared.env.set_listening(None);
                return;
            }
        };
        if wanted == 0
            && let Err(error) = config::set_telemetry_port(&self.config_file, port)
        {
            // A port that cannot be remembered would change at the next start, and Codex's
            // endpoint lives in a config file.
            let reason = format!(
                "the picked port {port} could not be written to {}: {error}; set telemetry.port yourself",
                self.config_file.display()
            );
            warn!(%reason, "the OTLP receiver is down");
            control.down = Some(reason);
            self.shared.env.set_listening(None);
            return;
        }
        control.settings.port = port;
        let cancel = self.shutdown.child_token();
        tokio::spawn(http::serve(
            listener,
            port,
            self.shared.clone(),
            cancel.clone(),
        ));
        control.listener = Some(Listener { port, cancel });
        control.down = None;
        self.shared.env.set_listening(Some(port));
        info!(port, "the OTLP receiver listens on 127.0.0.1");
    }

    /// Closes the listener and every connection it accepted.
    fn close(&self, control: &mut Control) {
        if let Some(listener) = control.listener.take() {
            listener.cancel.cancel();
            info!(port = listener.port, "the OTLP receiver is closed");
        }
        self.shared.env.set_listening(None);
    }

    /// `telemetry.status`.
    pub async fn status(&self) -> TelemetryStatus {
        let control = self.control.lock().await;
        let enabled = control.settings.enabled();
        let listener = if enabled.is_empty() {
            ListenerState::Off
        } else if control.listener.is_some() {
            ListenerState::Listening
        } else {
            ListenerState::Down
        };
        let port = control
            .listener
            .as_ref()
            .map(|listener| listener.port)
            .or((control.settings.port != 0).then_some(control.settings.port));
        let mut sessions: Vec<TelemetrySession> = lock(&self.shared.sessions)
            .values()
            .filter(|entry| self.shared.env.has_token(entry.session_id))
            .cloned()
            .collect();
        sessions.sort_by_key(|entry| std::cmp::Reverse(entry.last_report_ms));
        let counters = &self.shared.counters;
        TelemetryStatus {
            listener,
            port,
            down_reason: control
                .down
                .clone()
                .filter(|_| listener == ListenerState::Down),
            agents: Agent::ALL
                .into_iter()
                .map(|agent| TelemetryAgent {
                    id: agent.id().to_owned(),
                    label: agent.label().to_owned(),
                    enabled: enabled.contains(&agent),
                })
                .collect(),
            sessions,
            counters: TelemetryCounters {
                accepted: counters.accepted.load(Ordering::Relaxed),
                rejected: counters.rejected.load(Ordering::Relaxed),
                dropped: counters.dropped.load(Ordering::Relaxed),
                ignored: counters.ignored.load(Ordering::Relaxed),
            },
        }
    }
}

/// Binds `127.0.0.1:port` and nothing wider.
async fn bind(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .map_err(|error| format!("127.0.0.1:{port} could not be bound: {error}"))
}

/// Picks a port in the ephemeral range at random and binds it; another when it is taken.
async fn pick_and_bind() -> Result<(TcpListener, u16), String> {
    let span = u32::from(*PORT_RANGE.end() - *PORT_RANGE.start()) + 1;
    let mut last = String::from("no port was tried");
    for _ in 0..PICK_ATTEMPTS {
        let mut bytes = [0_u8; 4];
        getrandom::fill(&mut bytes)
            .map_err(|error| format!("no random number for a port: {error}"))?;
        let offset = u32::from_le_bytes(bytes) % span;
        let port = *PORT_RANGE.start() + u16::try_from(offset).unwrap_or(0);
        match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).await {
            Ok(listener) => return Ok((listener, port)),
            Err(error) => last = format!("127.0.0.1:{port} could not be bound: {error}"),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_listener_is_bound_to_the_loopback_address_and_never_wider() {
        let listener = bind(0).await.unwrap();
        let address = listener.local_addr().unwrap();
        assert_eq!(address.ip(), std::net::IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(!address.ip().is_unspecified());
    }

    #[tokio::test]
    async fn a_picked_port_is_in_the_ephemeral_range() {
        let (_listener, port) = pick_and_bind().await.unwrap();
        assert!(PORT_RANGE.contains(&port), "{port}");
    }

    #[tokio::test]
    async fn a_taken_port_is_an_error_naming_it() {
        let held = bind(0).await.unwrap();
        let port = held.local_addr().unwrap().port();
        let error = bind(port).await.unwrap_err();
        assert!(error.contains(&format!("127.0.0.1:{port}")), "{error}");
    }
}
