//! The digest (MAWAS-DIGEST-A, ADR 0025): one quiet native notification when the user has been
//! away from every Hutan window for a while and something needs them. No model is asked: the
//! text is the one line of the activity summary (`ActivitySummary::line`) that Mawas Home's
//! *Catch up* chip shows, so the two cannot disagree.
//!
//! The daemon decides ([`decide`], a pure function over the settings, the presence of the
//! desktop windows and the time of the last digest) and a timer asks. A digest is sent only
//! when every one of these holds:
//!
//! - `notify.digest_hours` is not `0` (the default: off);
//! - no window is the active one (`client.focus` carries `window_active`, protocol 0.68; a
//!   desktop that does not say reports the session it shows, and none means away). No desktop
//!   connected at all is away too;
//! - the last window left at least that many hours ago, and so did the last digest: **at most
//!   one per period**, and never one per agent;
//! - the activity since the later of the two *needs the user* (`ActivitySummary::needs_you`:
//!   a request that waits, a failed turn or task, work sent to review). Finished work alone is
//!   not worth a ping.
//!
//! The time of the last digest survives a restart ([`STATE_FILE`]); the start of the daemon
//! counts as the last moment the user was here, so a restart does not make a digest due at
//! once. Its click opens Mawas Home on the Recent tab (`ui.open` with `view: mawas_recent`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hutan_core::SessionId;
use hutan_protocol::activity::AgentsActivityRequest;
use hutan_protocol::assistant::{AssistantListRequest, AssistantScope};
use hutan_storage::Storage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agents_activity::read_activity;
use crate::assistant::AssistantService;
use crate::conn::ConnId;
use crate::notify::NotifyService;
use crate::registry::now_ms;
use crate::tasks::TaskService;

/// The file of the digest's state in the data directory.
pub const STATE_FILE: &str = "digest-state.json";

/// The title of the notification.
pub const TITLE: &str = "While you were away";

/// Milliseconds in the hour of `notify.digest_hours`.
pub const HOUR_MS: u64 = 3_600_000;

/// How often the daemon looks (the decision is cheap; the records are read only when it says to).
pub const TICK: Duration = Duration::from_secs(300);

/// What a test changes of the digest's timing: the length of an "hour", how often the daemon
/// looks, and the model digest's limits.
#[derive(Debug, Clone)]
pub struct DigestTuning {
    pub tick: Duration,
    pub hour_ms: u64,
    /// The scheduled model digest's longest run (`crate::digest_model`); then it is stopped.
    pub run_limit: Duration,
    /// How often the model digest looks at the journal for the end of a turn.
    pub poll: Duration,
    /// The seconds local time is ahead of UTC, when a test fixes it (else the system's).
    pub utc_offset_secs: Option<i64>,
}

impl Default for DigestTuning {
    fn default() -> Self {
        Self {
            tick: TICK,
            hour_ms: HOUR_MS,
            run_limit: Duration::from_secs(5 * 60),
            poll: Duration::from_millis(250),
            utc_offset_secs: None,
        }
    }
}

/// What the desktop windows report: which of them is the active one and when the last one left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presence {
    windows: HashMap<ConnId, bool>,
    /// The last moment a window was the active one (the start of the daemon before any was).
    last_active_ms: u64,
}

impl Presence {
    pub fn new(now_ms: u64) -> Self {
        Self {
            windows: HashMap::new(),
            last_active_ms: now_ms,
        }
    }

    /// Whether any window is the active one now.
    pub fn any_active(&self) -> bool {
        self.windows.values().any(|active| *active)
    }

    /// The last moment a window was the active one; meaningful while none is.
    pub const fn last_active_ms(&self) -> u64 {
        self.last_active_ms
    }

    /// A window's report (`client.focus`): whether it is the active one now.
    pub fn report(&mut self, conn: ConnId, active: bool, now_ms: u64) {
        let was = self.any_active();
        self.windows.insert(conn, active);
        if was || active {
            // While one is active the user is here; the moment a window leaves is the last one.
            self.last_active_ms = now_ms;
        }
    }

    /// A connection closed: its window is gone (an active one leaving is the user leaving).
    pub fn closed(&mut self, conn: ConnId, now_ms: u64) {
        let was = self.any_active();
        if self.windows.remove(&conn).is_some() && was {
            self.last_active_ms = now_ms;
        }
    }
}

/// What the daemon concludes before it looks at what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// `notify.digest_hours` is `0`.
    Off,
    /// A window is the active one.
    Present,
    /// The user left less than the period ago.
    NotAwayLongEnough,
    /// A digest went out less than the period ago: at most one per period.
    AlreadySaid,
    /// A digest is due if something happened that needs the user since `since_ms`.
    Look { since_ms: u64 },
}

/// The decision: pure. `last_digest_ms` is `0` when none was ever sent.
pub fn decide(
    now_ms: u64,
    hours: u64,
    hour_ms: u64,
    presence: &Presence,
    last_digest_ms: u64,
) -> Verdict {
    if hours == 0 {
        return Verdict::Off;
    }
    if presence.any_active() {
        return Verdict::Present;
    }
    let period = hours.saturating_mul(hour_ms);
    if now_ms.saturating_sub(presence.last_active_ms()) < period {
        return Verdict::NotAwayLongEnough;
    }
    if last_digest_ms != 0 && now_ms.saturating_sub(last_digest_ms) < period {
        return Verdict::AlreadySaid;
    }
    Verdict::Look {
        since_ms: presence.last_active_ms().max(last_digest_ms),
    }
}

/// What one look came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The decision said not to look.
    Skipped(Verdict),
    /// Nothing happened that needs the user: no notification.
    NothingToSay,
    /// There is nowhere to send the click: no Mawas Home conversation exists yet.
    NoMawas,
    /// Notifications are off (`notify.enabled`).
    Disabled,
    /// A digest went out with this text.
    Sent(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    #[serde(default)]
    last_digest_ms: u64,
}

/// What the digest is made of.
#[derive(Debug)]
pub struct DigestInputs {
    pub notify: Arc<NotifyService>,
    pub assistant: Arc<AssistantService>,
    pub storage: Storage,
    pub tasks: Arc<TaskService>,
    pub state_file: PathBuf,
    pub tuning: DigestTuning,
}

#[derive(Debug)]
struct Core {
    presence: Presence,
    last_digest_ms: u64,
}

pub struct DigestService {
    notify: Arc<NotifyService>,
    assistant: Arc<AssistantService>,
    storage: Storage,
    tasks: Arc<TaskService>,
    state_file: PathBuf,
    tuning: DigestTuning,
    core: Mutex<Core>,
}

impl std::fmt::Debug for DigestService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DigestService").finish_non_exhaustive()
    }
}

impl DigestService {
    pub fn new(inputs: DigestInputs) -> Arc<Self> {
        let persisted = load(&inputs.state_file);
        let now = now_ms();
        Arc::new(Self {
            notify: inputs.notify,
            assistant: inputs.assistant,
            storage: inputs.storage,
            tasks: inputs.tasks,
            state_file: inputs.state_file,
            tuning: inputs.tuning,
            core: Mutex::new(Core {
                presence: Presence::new(now),
                // A clock that went backwards must not postpone the next digest for longer
                // than a period.
                last_digest_ms: persisted.last_digest_ms.min(now),
            }),
        })
    }

    /// Looks every [`DigestTuning::tick`] until shutdown.
    pub fn start(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(this.tuning.tick);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    _ = ticker.tick() => {
                        match this.check().await {
                            Ok(Outcome::Sent(text)) => info!(%text, "sent the digest"),
                            Ok(outcome) => debug!(?outcome, "no digest"),
                            Err(error) => warn!(message = %error.message, "the digest could not look at what happened"),
                        }
                    }
                }
            }
        })
    }

    /// `client.focus` of a desktop connection: `window_active` says whether its window is the
    /// active one; an older client does not, and counts as active while it shows a session.
    pub fn report(&self, conn: ConnId, session: Option<SessionId>, window_active: Option<bool>) {
        let active = window_active.unwrap_or(session.is_some());
        self.core.lock().presence.report(conn, active, now_ms());
    }

    /// A connection closed.
    pub fn connection_closed(&self, conn: ConnId) {
        self.core.lock().presence.closed(conn, now_ms());
    }

    /// One look: decides, reads what happened since the user was last here or last heard from
    /// the digest, and sends the digest when something needs them.
    pub async fn check(&self) -> Result<Outcome, hutan_protocol::control::ErrorBody> {
        let hours = self.notify.settings().digest_hours;
        let (verdict, last_digest) = {
            let core = self.core.lock();
            (
                decide(
                    now_ms(),
                    hours,
                    self.tuning.hour_ms,
                    &core.presence,
                    core.last_digest_ms,
                ),
                core.last_digest_ms,
            )
        };
        let Verdict::Look { since_ms } = verdict else {
            return Ok(Outcome::Skipped(verdict));
        };
        let activity = read_activity(
            &self.storage,
            &self.tasks,
            &AgentsActivityRequest {
                since_ms: Some(since_ms),
                limit: Some(1),
                ..AgentsActivityRequest::default()
            },
        )
        .await?;
        let Some(line) = activity
            .summary
            .needs_you()
            .then(|| activity.summary.line())
            .flatten()
        else {
            return Ok(Outcome::NothingToSay);
        };
        let list = self
            .assistant
            .list(AssistantListRequest {
                scope: Some(AssistantScope::Global),
                workspace_id: None,
            })
            .await?;
        let Some(home) = list
            .assistants
            .iter()
            .find(|a| a.default)
            .or_else(|| list.assistants.first())
        else {
            return Ok(Outcome::NoMawas);
        };
        if !self.notify.raise_digest(
            home.session.id,
            Some(home.workspace_id),
            TITLE,
            &line,
            Some(hutan_protocol::notify::UiView::MawasRecent),
        ) {
            return Ok(Outcome::Disabled);
        }
        let now = now_ms();
        {
            let mut core = self.core.lock();
            // Nobody else sets it while a look runs, but a digest of a moment ago stays the later.
            core.last_digest_ms = now.max(last_digest);
        }
        if let Err(error) = save(
            &self.state_file,
            &Persisted {
                last_digest_ms: now,
            },
        ) {
            warn!(%error, "could not remember when the digest went out");
        }
        Ok(Outcome::Sent(line))
    }
}

fn load(path: &Path) -> Persisted {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, persisted: &Persisted) -> std::io::Result<()> {
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(persisted)?)?;
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const H: u64 = HOUR_MS;

    fn conn(n: u64) -> ConnId {
        ConnId(n)
    }

    /// A window that was active until `left_at` and is not now.
    fn away(left_at: u64) -> Presence {
        let mut presence = Presence::new(0);
        presence.report(conn(1), true, left_at.saturating_sub(1));
        presence.report(conn(1), false, left_at);
        presence
    }

    #[test]
    fn off_says_nothing_whatever_happened() {
        let presence = away(0);
        assert_eq!(decide(100 * H, 0, H, &presence, 0), Verdict::Off);
    }

    #[test]
    fn a_window_that_is_active_is_the_user_being_here() {
        let mut presence = Presence::new(0);
        presence.report(conn(1), true, 50 * H);
        assert_eq!(decide(100 * H, 4, H, &presence, 0), Verdict::Present);
        // Another window going inactive does not take the user away.
        presence.report(conn(2), false, 60 * H);
        assert_eq!(decide(100 * H, 4, H, &presence, 0), Verdict::Present);
    }

    #[test]
    fn the_period_runs_from_the_last_moment_a_window_was_active() {
        let presence = away(10 * H);
        assert_eq!(
            decide(10 * H + 4 * H - 1, 4, H, &presence, 0),
            Verdict::NotAwayLongEnough
        );
        assert_eq!(
            decide(14 * H, 4, H, &presence, 0),
            Verdict::Look { since_ms: 10 * H }
        );
    }

    #[test]
    fn no_desktop_connected_is_away_from_when_the_last_one_left_or_the_daemon_began() {
        // Never a window: the daemon's start is the last time the user was here.
        let presence = Presence::new(2 * H);
        assert_eq!(
            decide(5 * H, 4, H, &presence, 0),
            Verdict::NotAwayLongEnough
        );
        assert_eq!(
            decide(6 * H, 4, H, &presence, 0),
            Verdict::Look { since_ms: 2 * H }
        );
        // The active window's connection closing is the user leaving.
        let mut presence = Presence::new(0);
        presence.report(conn(1), true, 10 * H);
        presence.closed(conn(1), 20 * H);
        assert!(!presence.any_active());
        assert_eq!(presence.last_active_ms(), 20 * H);
        assert_eq!(
            decide(24 * H, 4, H, &presence, 0),
            Verdict::Look { since_ms: 20 * H }
        );
    }

    #[test]
    fn a_closed_window_that_was_not_active_does_not_move_the_last_moment() {
        let mut presence = Presence::new(0);
        presence.report(conn(1), false, 3 * H);
        presence.closed(conn(1), 9 * H);
        assert_eq!(presence.last_active_ms(), 0);
    }

    #[test]
    fn at_most_one_digest_per_period_and_the_next_looks_only_past_the_last() {
        let presence = away(10 * H);
        // A digest went out at 14 h: nothing until 18 h.
        assert_eq!(
            decide(15 * H, 4, H, &presence, 14 * H),
            Verdict::AlreadySaid
        );
        assert_eq!(
            decide(18 * H - 1, 4, H, &presence, 14 * H),
            Verdict::AlreadySaid
        );
        assert_eq!(
            decide(18 * H, 4, H, &presence, 14 * H),
            Verdict::Look { since_ms: 14 * H }
        );
    }

    #[test]
    fn coming_back_starts_the_period_over() {
        let mut presence = away(10 * H);
        presence.report(conn(1), true, 20 * H);
        presence.report(conn(1), false, 21 * H);
        assert_eq!(
            decide(24 * H, 4, H, &presence, 14 * H),
            Verdict::NotAwayLongEnough
        );
        assert_eq!(
            decide(25 * H, 4, H, &presence, 14 * H),
            Verdict::Look { since_ms: 21 * H }
        );
    }

    #[test]
    fn the_state_file_keeps_the_last_digest_and_a_broken_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(STATE_FILE);
        assert_eq!(load(&file).last_digest_ms, 0);
        save(
            &file,
            &Persisted {
                last_digest_ms: 1234,
            },
        )
        .unwrap();
        assert_eq!(load(&file).last_digest_ms, 1234);
        std::fs::write(&file, b"not json").unwrap();
        assert_eq!(load(&file).last_digest_ms, 0);
    }
}
