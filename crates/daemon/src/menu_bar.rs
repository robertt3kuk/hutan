//! The daemon's own menu bar item (M6-D): while `hutand` runs from `Hutan.app` and no desktop is
//! connected, it shows the orangutan glyph in the menu bar, so a daemon that outlives the app
//! (keep running, start at login) is never invisible. While a desktop is there the desktop's
//! item is the one on the menu bar (it is richer) and the daemon shows none: one icon at a time.
//! "There" is a desktop *process*, not only a connection: a desktop that is reconnecting (the
//! daemon restarted, the connection dropped) still has its item on the menu bar, so the daemon
//! waits until neither a connection nor the desktop's single-instance socket
//! ([`DESKTOP_SOCKET_FILE`]) answers.
//!
//! Layers, like [`notify_native`](crate::notify_native):
//!
//! - [`MenuModel`] is what the item shows (live sessions, who needs the user, the start-at-login
//!   setting) and its menu rows ([`MenuRow`], [`MenuChoice`]). Pure, so it is tested without a
//!   system.
//! - [`StatusItemHost`] is the seam to the system: [`show`](StatusItemHost::show),
//!   [`hide`](StatusItemHost::hide), the stop confirmation and a channel of [`MenuChoice`]s.
//!   [`SystemStatusItem`](cocoa::SystemStatusItem) (macOS) is the real one;
//!   [`fake::FakeStatusItem`] stands in for it in tests. Calls never block: the real host hands
//!   work to the main thread's run loop that also hosts the native notifications.
//! - [`AppLauncher`] is the seam to "open Hutan.app" ([`cocoa::OpenApp`] starts the app's
//!   desktop executable).
//! - [`DesktopPresence`] counts the handshaken `desktop` clients; [`desktop_running`] asks whether
//!   a desktop process runs without being connected; [`start`] is the task that follows both.
//!
//! **Who needs the user.** Entries are one per session, strongest first: a permission request
//! waits for an answer (`approval.list`), an agent's question waits (`question.list`), an agent is blocked (the session's agent report), or
//! the session failed while no desktop was around (a pending `session_exited_with_error` intent
//! raised since the last desktop left; the desktop knows what you have seen, the daemon only
//! what happened while you were away, so it follows `[notify]`: `enabled = false` or a `kinds`
//! without it lists none). Acting on a failed session's entry acknowledges its intent.
//!
//! **Hand-off.** A choice that opens Hutan launches the app and then asks the desktop through
//! the paths a connected client already has: `ui.open` for a session, `ui.run` of
//! [`OPEN_SETTINGS_ACTION`] for the settings. The desktop is asked once one of its windows has
//! published (`ui.publish`: it is built and synchronised), at most [`OPEN_PATIENCE`] after the
//! click.
//!
//! The item exists only in a daemon that runs from `Hutan.app` ([`runs_from_hutan_app`]); a
//! `cargo run` daemon or a test shows none and touches no Cocoa. It is not optional: unlike the
//! desktop's (`app.menu_bar`) there is no setting to hide it.
//!
//! [`runs_from_hutan_app`]: crate::notify_native::runs_from_hutan_app

use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hutan_core::{SessionId, SessionState, WorkspaceId};
use hutan_protocol::messages::{AgentState, Event, SessionListFilter, StopBy};
use hutan_protocol::notify::IntentKind;
use hutan_protocol::ui::UiRun;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bus::EventBus;
use crate::notify::NotifyService;
use crate::registry::{RegistryHandle, now_ms};
use crate::uihub::UiHub;

#[cfg(target_os = "macos")]
pub mod cocoa;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

/// The desktop's action that opens its settings (`workbench::OpenSettings`); the desktop's tests
/// pin the name.
pub const OPEN_SETTINGS_ACTION: &str = "workbench::OpenSettings";
/// Sessions listed in the menu; the rest are summarised.
pub const MAX_LISTED: usize = 10;
/// Longest detail of an entry, in characters.
const DETAIL_MAX_CHARS: usize = 60;
/// How long the daemon waits, with no desktop attached, before it puts its item on the menu bar:
/// a desktop that is starting (it started the daemon) or reconnecting after a dropped
/// connection is back sooner, and the menu bar never shows two Hutan icons.
pub const SHOW_DELAY: Duration = Duration::from_millis(750);
/// How often the daemon looks again while a desktop process runs without a connection (see
/// [`desktop_running`]): the item appears once that desktop is gone.
pub const DESKTOP_POLL: Duration = Duration::from_secs(1);
/// The single-instance socket the desktop listens on in the data folder
/// (`hutan_desktop::instance::SOCKET_FILE`; the desktop's tests pin the name).
pub const DESKTOP_SOCKET_FILE: &str = "desktop.sock";
/// Longest a probe of the desktop's socket may take.
const DESKTOP_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
/// How long after a click the daemon keeps waiting for a desktop window to ask it to show what
/// was chosen.
pub const OPEN_PATIENCE: Duration = Duration::from_secs(30);
/// Changes within this long of each other are shown with one update of the item.
pub const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);
/// Choices queued between the host and the service.
pub const CHOICE_QUEUE: usize = 16;
/// After a launch, further requests to open Hutan (more clicks, Launch Services reopening the
/// app again) start nothing for this long unless a desktop connected meanwhile: one launch at a
/// time, never a loop of them.
pub const LAUNCH_GRACE: Duration = Duration::from_secs(10);

// ---- What the item shows -------------------------------------------------------------------------

/// Why a session is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// A permission request waits for an answer.
    Approval,
    /// An agent asks a question (protocol 0.24) that waits for an answer.
    Question,
    /// An agent in the session is blocked.
    Blocked,
    /// The session ended with an error while nobody had the desktop open.
    Failed,
}

impl Need {
    fn word(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}

/// One session that needs the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsYou {
    pub session: SessionId,
    pub workspace: Option<WorkspaceId>,
    pub title: String,
    pub need: Need,
    /// `exit 2`, `2 approvals`, the agent's message; empty when there is nothing to add.
    pub detail: String,
    /// The pending intent behind a failed session, acknowledged when the entry is acted on.
    pub intent: Option<String>,
}

impl NeedsYou {
    /// `claude — blocked: running tests`.
    pub fn text(&self) -> String {
        if self.detail.is_empty() {
            format!("{} — {}", self.title, self.need.word())
        } else {
            format!("{} — {}: {}", self.title, self.need.word(), self.detail)
        }
    }
}

/// What the item shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    /// Sessions with a live process.
    pub live_sessions: u32,
    pub needs_you: Vec<NeedsYou>,
    /// `app.start_at_login`, as set in `settings.toml`.
    pub start_at_login: bool,
}

/// What a menu row does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    /// Open Hutan showing this session.
    OpenSession(SessionId),
    OpenHutan,
    /// Stop the daemon (asks first while sessions run).
    StopDaemon,
    /// Open Hutan's settings (where start at login is changed).
    OpenSettings,
}

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuRow {
    /// A line that is only text.
    Label(String),
    Separator,
    Item {
        title: String,
        choice: MenuChoice,
        checked: bool,
    },
}

impl MenuModel {
    /// Something needs the user: the glyph gets a dot next to it.
    pub fn dot(&self) -> bool {
        !self.needs_you.is_empty()
    }

    /// `Hutan daemon — running · 3 sessions`.
    pub fn status(&self) -> String {
        let sessions = match self.live_sessions {
            1 => "1 session".to_owned(),
            n => format!("{n} sessions"),
        };
        format!("Hutan daemon — running · {sessions}")
    }

    /// The menu, top to bottom.
    pub fn rows(&self) -> Vec<MenuRow> {
        let mut rows = vec![MenuRow::Label(self.status()), MenuRow::Separator];
        let needing = self.needs_you.len();
        if needing > 0 {
            rows.push(MenuRow::Label(if needing == 1 {
                "1 needs attention".to_owned()
            } else {
                format!("{needing} need attention")
            }));
            rows.extend(
                self.needs_you
                    .iter()
                    .take(MAX_LISTED)
                    .map(|entry| MenuRow::Item {
                        title: entry.text(),
                        choice: MenuChoice::OpenSession(entry.session),
                        checked: false,
                    }),
            );
            if needing > MAX_LISTED {
                rows.push(MenuRow::Label(format!(
                    "…and {} more",
                    needing - MAX_LISTED
                )));
            }
            rows.push(MenuRow::Separator);
        }
        rows.push(item("Open Hutan", MenuChoice::OpenHutan, false));
        rows.push(MenuRow::Separator);
        rows.push(item("Stop daemon…", MenuChoice::StopDaemon, false));
        rows.push(item(
            "Start at login",
            MenuChoice::OpenSettings,
            self.start_at_login,
        ));
        rows
    }
}

fn item(title: &str, choice: MenuChoice, checked: bool) -> MenuRow {
    MenuRow::Item {
        title: title.to_owned(),
        choice,
        checked,
    }
}

// ---- The seams ---------------------------------------------------------------------------------------

/// The user's answer to "stop the daemon and its sessions?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopAnswer {
    StopSessions,
    Cancel,
}

/// The menu bar. Implementations never block the caller (it is the tokio runtime) and only log
/// what they cannot do.
pub trait StatusItemHost: Debug + Send + Sync {
    /// Puts the item on the menu bar (creating it if needed) with this content; called again
    /// whenever the content changes.
    fn show(&self, model: &MenuModel);

    /// Takes the item off the menu bar.
    fn hide(&self);

    /// Asks whether to stop the daemon and the `live_sessions` sessions that run. A host that
    /// cannot ask answers by dropping the sender (counts as cancelling).
    fn confirm_stop(&self, live_sessions: u32) -> oneshot::Receiver<StopAnswer>;

    /// The menu choices; handed out once, to whoever consumes them.
    fn choices(&self) -> Option<mpsc::Receiver<MenuChoice>>;
}

/// Brings Hutan.app up (starting it when it is not running).
pub trait AppLauncher: Debug + Send + Sync {
    fn open_hutan(&self);
}

/// The daemon's menu bar item: the host and the launcher, which exist only inside `Hutan.app`
/// (`notify_native::system_host`), and the timings tests shorten.
#[derive(Debug, Clone)]
pub struct MenuBar {
    pub host: Arc<dyn StatusItemHost>,
    pub launcher: Arc<dyn AppLauncher>,
    /// [`SHOW_DELAY`].
    pub show_delay: Duration,
    /// [`OPEN_PATIENCE`].
    pub open_patience: Duration,
    /// [`DESKTOP_POLL`].
    pub desktop_poll: Duration,
}

impl MenuBar {
    pub fn new(host: Arc<dyn StatusItemHost>, launcher: Arc<dyn AppLauncher>) -> Self {
        Self {
            host,
            launcher,
            show_delay: SHOW_DELAY,
            open_patience: OPEN_PATIENCE,
            desktop_poll: DESKTOP_POLL,
        }
    }
}

// ---- Desktop clients -----------------------------------------------------------------------------------

/// How many handshaken `desktop` clients are attached, as it changes. The connection tasks count
/// themselves in ([`join`](Self::join)); the item follows [`watch`](Self::watch).
#[derive(Debug, Clone)]
pub struct DesktopPresence(Arc<watch::Sender<u32>>);

impl Default for DesktopPresence {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(0)))
    }
}

impl DesktopPresence {
    /// One more desktop until the guard drops.
    pub fn join(&self) -> DesktopGuard {
        self.0.send_modify(|count| *count += 1);
        DesktopGuard(self.0.clone())
    }

    pub fn watch(&self) -> watch::Receiver<u32> {
        self.0.subscribe()
    }
}

/// A desktop client is attached while this lives.
#[derive(Debug)]
pub struct DesktopGuard(Arc<watch::Sender<u32>>);

impl Drop for DesktopGuard {
    fn drop(&mut self) {
        self.0.send_modify(|count| *count = count.saturating_sub(1));
    }
}

// ---- The service ---------------------------------------------------------------------------------------

/// What [`start`] needs.
pub(crate) struct Inputs {
    pub menu_bar: MenuBar,
    pub desktops: watch::Receiver<u32>,
    /// Pokes: something that may change the model happened (`EventBus::set_attention`,
    /// `NotifyService::set_changed`).
    pub changes: mpsc::Receiver<()>,
    pub notify: Arc<NotifyService>,
    pub registry: RegistryHandle,
    pub bus: Arc<EventBus>,
    pub ui: Arc<UiHub>,
    /// The user's `settings.toml`, for `app.start_at_login`.
    pub settings_file: PathBuf,
    /// The desktop's single-instance socket ([`DESKTOP_SOCKET_FILE`] in the data folder).
    pub desktop_socket: PathBuf,
    pub shutdown: CancellationToken,
}

/// Starts the task that keeps the item in step with the desktop clients and the sessions until
/// `shutdown`, when it takes the item off the menu bar.
pub(crate) fn start(inputs: Inputs) -> JoinHandle<()> {
    tokio::spawn(Service::new(inputs).run())
}

/// What a click on Hutan asks the desktop for once one of its windows is up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Handoff {
    Session {
        session: SessionId,
        workspace: Option<WorkspaceId>,
    },
    Settings,
}

struct Service {
    host: Arc<dyn StatusItemHost>,
    launcher: Arc<dyn AppLauncher>,
    show_delay: Duration,
    open_patience: Duration,
    desktop_poll: Duration,
    desktops: watch::Receiver<u32>,
    changes: mpsc::Receiver<()>,
    notify: Arc<NotifyService>,
    registry: RegistryHandle,
    bus: Arc<EventBus>,
    ui: Arc<UiHub>,
    settings_file: PathBuf,
    desktop_socket: PathBuf,
    shutdown: CancellationToken,
    /// On the menu bar now, with this content.
    shown: Option<MenuModel>,
    /// When the last desktop went away (Unix ms): what failed since is what the user missed.
    away_since_ms: u64,
    pending: Option<(Handoff, Instant)>,
    /// When the last launch started; cleared when a desktop connects ([`LAUNCH_GRACE`]).
    launched_at: Option<Instant>,
}

impl Service {
    fn new(inputs: Inputs) -> Self {
        let Inputs {
            menu_bar,
            desktops,
            changes,
            notify,
            registry,
            bus,
            ui,
            settings_file,
            desktop_socket,
            shutdown,
        } = inputs;
        Self {
            host: menu_bar.host,
            launcher: menu_bar.launcher,
            show_delay: menu_bar.show_delay,
            open_patience: menu_bar.open_patience,
            desktop_poll: menu_bar.desktop_poll,
            desktops,
            changes,
            notify,
            registry,
            bus,
            ui,
            settings_file,
            desktop_socket,
            shutdown,
            shown: None,
            away_since_ms: 0,
            pending: None,
            launched_at: None,
        }
    }

    fn desktop_present(&mut self) -> bool {
        *self.desktops.borrow_and_update() > 0
    }

    async fn run(mut self) {
        let mut choices = self.host.choices();
        if choices.is_none() {
            warn!("the menu bar host has no choices; the item's menu does nothing");
        }
        let mut windows = self.ui.windows();
        let mut show_at = (!self.desktop_present()).then(|| Instant::now() + self.show_delay);
        let mut refresh_at: Option<Instant> = None;
        info!("the daemon's menu bar item follows the desktop clients");
        loop {
            let deadline = [show_at, refresh_at].into_iter().flatten().min();
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                changed = self.desktops.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if self.desktop_present() {
                        show_at = None;
                        refresh_at = None;
                        self.launched_at = None;
                        self.hide();
                    } else {
                        self.away_since_ms = now_ms();
                        if self.shown.is_none() && show_at.is_none() {
                            show_at = Some(Instant::now() + self.show_delay);
                        }
                    }
                }
                Some(()) = self.changes.recv() => {
                    if self.shown.is_some() && refresh_at.is_none() {
                        refresh_at = Some(Instant::now() + REFRESH_DEBOUNCE);
                    }
                }
                Some(choice) = next_choice(&mut choices) => self.choose(choice).await,
                _ = windows.changed() => self.deliver().await,
                () = sleep_until(deadline), if deadline.is_some() => {
                    let now = Instant::now();
                    if show_at.is_some_and(|at| at <= now) {
                        show_at = None;
                        if !self.desktop_present() {
                            if desktop_running(&self.desktop_socket).await {
                                // A desktop with no connection (reconnecting, the daemon just
                                // restarted) has its own item on the menu bar: look again later.
                                show_at = Some(Instant::now() + self.desktop_poll);
                            } else {
                                self.refresh().await;
                            }
                        }
                    }
                    if refresh_at.is_some_and(|at| at <= now) {
                        refresh_at = None;
                        if self.shown.is_some() {
                            self.refresh().await;
                        }
                    }
                }
            }
        }
        self.hide();
    }

    fn hide(&mut self) {
        if self.shown.take().is_some() {
            self.host.hide();
            debug!("the daemon's menu bar item is off");
        }
    }

    /// Puts the current model on the menu bar when it differs from what is there.
    async fn refresh(&mut self) {
        let model = self.model().await;
        // A desktop may have attached while the model was being gathered.
        if *self.desktops.borrow() > 0 {
            return;
        }
        if self.shown.as_ref() != Some(&model) {
            self.host.show(&model);
            debug!(
                live_sessions = model.live_sessions,
                needs_you = model.needs_you.len(),
                "the daemon's menu bar item is on"
            );
            self.shown = Some(model);
        }
    }

    // What needs the user ----------------------------------------------------------------------------

    async fn model(&self) -> MenuModel {
        let live_sessions = self
            .registry
            .session_counts()
            .await
            .map_or(0, |counts| counts.live);
        MenuModel {
            live_sessions,
            needs_you: self.needs_you().await,
            start_at_login: read_start_at_login(&self.settings_file).await,
        }
    }

    async fn needs_you(&self) -> Vec<NeedsYou> {
        let sessions = match self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        {
            Ok(list) => list.sessions,
            Err(error) => {
                debug!(message = %error.message, "the menu bar item could not list the sessions");
                return Vec::new();
            }
        };
        let approvals = self.registry.chats().approval_list(None).approvals;
        let questions = self.registry.chats().question_list(None).questions;
        let intents = self.notify.list();
        let away_since = self.away_since_ms;

        let mut entries: Vec<NeedsYou> = Vec::new();
        let mut taken: HashSet<SessionId> = HashSet::new();
        let title_of = |id: SessionId| {
            sessions
                .iter()
                .find(|s| s.id == id)
                .map_or_else(|| "Session".to_owned(), |s| s.title.clone())
        };
        let workspace_of =
            |id: SessionId| sessions.iter().find(|s| s.id == id).map(|s| s.workspace_id);

        // Approvals, oldest first; one entry per session.
        for approval in &approvals {
            let session = approval.session_id;
            if !taken.insert(session) {
                continue;
            }
            let count = approvals.iter().filter(|a| a.session_id == session).count();
            entries.push(NeedsYou {
                session,
                workspace: workspace_of(session),
                title: title_of(session),
                need: Need::Approval,
                detail: if count > 1 {
                    format!("{count} approvals")
                } else {
                    shorten(&approval.title)
                },
                intent: None,
            });
        }
        // Then questions of agents, the same way.
        for question in &questions {
            let session = question.session_id;
            if !taken.insert(session) {
                continue;
            }
            let count = questions.iter().filter(|q| q.session_id == session).count();
            entries.push(NeedsYou {
                session,
                workspace: workspace_of(session),
                title: title_of(session),
                need: Need::Question,
                detail: if count > 1 {
                    format!("{count} questions")
                } else {
                    shorten(&question.spec.title)
                },
                intent: None,
            });
        }
        for session in &sessions {
            let Some(agent) = session
                .agent
                .as_ref()
                .filter(|a| a.state == AgentState::Blocked && session.state.is_live())
            else {
                continue;
            };
            if !taken.insert(session.id) {
                continue;
            }
            let detail = agent
                .message
                .as_deref()
                .filter(|m| !m.trim().is_empty())
                .map(shorten)
                .or_else(|| agent.blocked_sub.as_deref().map(shorten))
                .unwrap_or_default();
            entries.push(NeedsYou {
                session: session.id,
                workspace: Some(session.workspace_id),
                title: session.title.clone(),
                need: Need::Blocked,
                detail,
                intent: None,
            });
        }
        for intent in intents.iter().filter(|i| {
            i.kind == IntentKind::SessionExitedWithError && i.created_at_ms >= away_since
        }) {
            if !taken.insert(intent.session_id) {
                continue;
            }
            let detail = sessions
                .iter()
                .find(|s| s.id == intent.session_id)
                .map(|s| match &s.state {
                    SessionState::Exited {
                        signal: Some(signal),
                        ..
                    } => format!("signal {signal}"),
                    SessionState::Exited {
                        code: Some(code), ..
                    } => format!("exit {code}"),
                    _ => String::new(),
                })
                .unwrap_or_default();
            entries.push(NeedsYou {
                session: intent.session_id,
                workspace: intent
                    .workspace_id
                    .or_else(|| workspace_of(intent.session_id)),
                title: title_of(intent.session_id),
                need: Need::Failed,
                detail,
                intent: Some(intent.id.clone()),
            });
        }
        entries
    }

    // Choices ----------------------------------------------------------------------------------------------

    async fn choose(&mut self, choice: MenuChoice) {
        debug!(?choice, "menu bar choice");
        match choice {
            MenuChoice::OpenHutan => self.launch(),
            MenuChoice::OpenSettings => self.open(Handoff::Settings).await,
            MenuChoice::OpenSession(session) => {
                let entry = self
                    .shown
                    .as_ref()
                    .and_then(|m| m.needs_you.iter().find(|e| e.session == session))
                    .cloned();
                if let Some(intent) = entry.as_ref().and_then(|e| e.intent.as_deref()) {
                    // Looked at now; the entry goes with the intent.
                    let _ = self.notify.ack(intent);
                }
                self.open(Handoff::Session {
                    session,
                    workspace: entry.and_then(|e| e.workspace),
                })
                .await;
            }
            MenuChoice::StopDaemon => self.stop().await,
        }
    }

    /// Launches Hutan and asks the desktop for `handoff` as soon as a window is there.
    async fn open(&mut self, handoff: Handoff) {
        self.launch();
        self.pending = Some((handoff, Instant::now() + self.open_patience));
        self.deliver().await;
    }

    /// Starts the desktop, unless a launch started less than [`LAUNCH_GRACE`] ago and no
    /// desktop has connected since.
    fn launch(&mut self) {
        if self
            .launched_at
            .is_some_and(|at| at.elapsed() < LAUNCH_GRACE)
        {
            debug!("a launch of Hutan is under way; not starting another");
            return;
        }
        self.launched_at = Some(Instant::now());
        self.launcher.open_hutan();
    }

    /// Hands the pending request to a window, when there is one and it is still wanted.
    async fn deliver(&mut self) {
        let Some((handoff, deadline)) = self.pending.take() else {
            return;
        };
        if Instant::now() > deadline {
            debug!(?handoff, "nobody opened Hutan in time; the request lapsed");
            return;
        }
        if *self.ui.windows().borrow() == 0 {
            self.pending = Some((handoff, deadline));
            return;
        }
        match handoff {
            Handoff::Session { session, workspace } => {
                if let Err(error) = self.bus.publish(&Event::UiOpen {
                    session_id: session,
                    workspace_id: workspace,
                    at_ms: now_ms(),
                }) {
                    warn!(%error, "could not ask the desktop to open a session");
                }
            }
            Handoff::Settings => {
                // The window answers when it has run the action; do not wait for it here.
                let ui = self.ui.clone();
                tokio::spawn(async move {
                    let run = UiRun {
                        window_id: None,
                        action: OPEN_SETTINGS_ACTION.to_owned(),
                        args: None,
                    };
                    if let Err(error) = ui.run(run).await {
                        warn!(message = %error.message, "the desktop did not open its settings");
                    }
                });
            }
        }
    }

    /// "Stop daemon…": straight away with nothing running, otherwise after the user agreed to
    /// stop the sessions too.
    async fn stop(&mut self) {
        let live = self
            .registry
            .session_counts()
            .await
            .map_or(0, |counts| counts.live);
        if live > 0 {
            let answer = self.host.confirm_stop(live);
            let answer = tokio::select! {
                () = self.shutdown.cancelled() => return,
                answer = answer => answer,
            };
            if answer != Ok(StopAnswer::StopSessions) {
                debug!("stopping the daemon was cancelled");
                return;
            }
        }
        info!(
            live_sessions = live,
            "the daemon is stopped from its menu bar item"
        );
        // Protocol 0.42: a stop the user wants kept; connected apps do not start it again.
        if let Err(error) = self.bus.publish(&Event::DaemonStopping {
            by: StopBy::MenuBar,
            restart: false,
        }) {
            warn!(%error, "could not announce the stop");
        }
        self.shutdown.cancel();
    }
}

/// Whether a desktop process runs on this data folder: its single-instance socket accepts a
/// connection (a socket file left by a crashed desktop refuses). Connecting and hanging up
/// asks it nothing; the desktop reads that as a request it does not know and goes on.
pub async fn desktop_running(socket: &std::path::Path) -> bool {
    matches!(
        tokio::time::timeout(
            DESKTOP_PROBE_TIMEOUT,
            tokio::net::UnixStream::connect(socket)
        )
        .await,
        Ok(Ok(_))
    )
}

async fn next_choice(choices: &mut Option<mpsc::Receiver<MenuChoice>>) -> Option<MenuChoice> {
    match choices {
        Some(choices) => choices.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// One line, at most [`DETAIL_MAX_CHARS`] characters.
fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= DETAIL_MAX_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(DETAIL_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `app.start_at_login` of the user's `settings.toml`; off when the file or the key is missing
/// or unreadable.
async fn read_start_at_login(file: &std::path::Path) -> bool {
    let Ok(text) = tokio::fs::read_to_string(file).await else {
        return false;
    };
    text.parse::<toml::Table>()
        .ok()
        .and_then(|table| table.get("app")?.get("start_at_login")?.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, need: Need, detail: &str) -> NeedsYou {
        NeedsYou {
            session: SessionId::new(),
            workspace: None,
            title: title.to_owned(),
            need,
            detail: detail.to_owned(),
            intent: None,
        }
    }

    fn model(needs_you: Vec<NeedsYou>) -> MenuModel {
        MenuModel {
            live_sessions: 3,
            needs_you,
            start_at_login: false,
        }
    }

    fn titles(rows: &[MenuRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                MenuRow::Label(text) => format!("label: {text}"),
                MenuRow::Separator => "---".to_owned(),
                MenuRow::Item { title, checked, .. } => {
                    format!("{}{title}", if *checked { "✓ " } else { "" })
                }
            })
            .collect()
    }

    #[test]
    fn a_quiet_daemon_shows_its_state_and_three_choices() {
        let m = model(Vec::new());
        assert!(!m.dot());
        assert_eq!(
            titles(&m.rows()),
            [
                "label: Hutan daemon — running · 3 sessions",
                "---",
                "Open Hutan",
                "---",
                "Stop daemon…",
                "Start at login",
            ]
        );
    }

    #[test]
    fn one_session_is_singular() {
        let mut m = model(Vec::new());
        m.live_sessions = 1;
        assert_eq!(m.status(), "Hutan daemon — running · 1 session");
        m.live_sessions = 0;
        assert_eq!(m.status(), "Hutan daemon — running · 0 sessions");
    }

    #[test]
    fn who_needs_you_is_listed_with_a_dot() {
        let blocked = entry("claude", Need::Blocked, "running tests");
        let failed = entry("make", Need::Failed, "exit 2");
        let approval = entry("codex", Need::Approval, "");
        let m = model(vec![blocked.clone(), failed, approval]);
        assert!(m.dot());
        let rows = m.rows();
        assert_eq!(
            titles(&rows)[2..8],
            [
                "label: 3 need attention",
                "claude — blocked: running tests",
                "make — failed: exit 2",
                "codex — approval",
                "---",
                "Open Hutan",
            ]
        );
        assert_eq!(
            rows[3],
            MenuRow::Item {
                title: blocked.text(),
                choice: MenuChoice::OpenSession(blocked.session),
                checked: false,
            },
            "an entry opens its session"
        );
        let one = model(vec![entry("a", Need::Blocked, "")]);
        assert_eq!(titles(&one.rows())[2], "label: 1 needs attention");
    }

    #[test]
    fn the_list_is_capped() {
        let many = (0..MAX_LISTED + 3)
            .map(|n| entry(&format!("s{n}"), Need::Blocked, ""))
            .collect();
        let rows = titles(&model(many).rows());
        assert_eq!(
            rows.iter().filter(|r| r.ends_with("— blocked")).count(),
            MAX_LISTED
        );
        assert!(rows.contains(&"label: …and 3 more".to_owned()));
    }

    #[test]
    fn start_at_login_shows_the_setting_and_opens_the_settings() {
        let mut m = model(Vec::new());
        m.start_at_login = true;
        let rows = m.rows();
        assert_eq!(
            rows.last(),
            Some(&MenuRow::Item {
                title: "Start at login".to_owned(),
                choice: MenuChoice::OpenSettings,
                checked: true,
            })
        );
        assert!(
            rows.contains(&MenuRow::Item {
                title: "Stop daemon…".to_owned(),
                choice: MenuChoice::StopDaemon,
                checked: false,
            }),
            "{rows:?}"
        );
    }

    #[test]
    fn details_are_one_short_line() {
        assert_eq!(shorten("  a\n b\t c "), "a b c");
        let long = "x".repeat(200);
        let cut = shorten(&long);
        assert_eq!(cut.chars().count(), DETAIL_MAX_CHARS);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn desktops_are_counted_in_and_out() {
        let presence = DesktopPresence::default();
        let watch = presence.watch();
        assert_eq!(*watch.borrow(), 0);
        let first = presence.join();
        let second = presence.join();
        assert_eq!(*watch.borrow(), 2);
        drop(first);
        assert_eq!(*watch.borrow(), 1);
        drop(second);
        assert_eq!(*watch.borrow(), 0);
    }

    #[tokio::test]
    async fn start_at_login_is_read_from_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.toml");
        assert!(!read_start_at_login(&file).await, "no file");
        std::fs::write(&file, "[app]\nstart_at_login = true\n").unwrap();
        assert!(read_start_at_login(&file).await);
        std::fs::write(&file, "[app]\nkeep_running = true\n").unwrap();
        assert!(!read_start_at_login(&file).await, "no key");
        std::fs::write(&file, "[app\nstart_at_login = true").unwrap();
        assert!(!read_start_at_login(&file).await, "unreadable");
    }
}
