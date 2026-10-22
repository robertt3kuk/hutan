//! The daemon's lifetime as the desktop sees it (design.md § Daemon lifecycle): the quit flow
//! (the daemon follows the app unless `app.keep_running` or `app.start_at_login`), what closing
//! the last window does
//! (`app.on_last_window_close`), the daemon controls shared by the `Daemon` menu, the status-bar
//! popover and the palette, and the "daemon stopped" state that keeps a daemon the user
//! stopped from being brought back until they start it again.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, BackgroundExecutor, Context, SharedString, Task, Window};
use hutan_client::identity::Mismatch;
use hutan_client::{ClientError, Connection, ConnectionState, StoreState};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{DaemonShutdown, DaemonStatusInfo, Empty, Request};
use serde_json::Value;
use tokio::sync::watch;

use super::actions::daemon;
use super::modal::{Modal, ModalChoice};
use super::palette::Candidate;
use super::panels::{
    CHIP_HINT, ChipKind, ConnectionChip, StatusTone, chip_of_connection, connection_label,
};
use super::view::Workbench;
use crate::app::DaemonMenu;
use crate::settings::{AppSettings, LastWindowClose};

/// Longest wait for a stopping daemon to remove its socket.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// Longest wait for a started daemon to accept the connection.
const START_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(25);
/// The `connection` item's tooltip is read from a `daemon.status` at most this old.
const STATUS_REFRESH: Duration = Duration::from_secs(30);
/// The status item while the daemon is stopped on purpose: who stopped it and the way back.
pub const STOPPED_BY_YOU: &str = "Stopped by you · Start";

/// Shows a file in Finder.
type RevealHandler = Rc<dyn Fn(&Path, &mut App)>;

/// A daemon operation the app is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonBusy {
    Stopping,
    Starting,
    Restarting,
}

impl DaemonBusy {
    pub(crate) fn label(self) -> &'static str {
        match self {
            DaemonBusy::Stopping => "stopping hutand…",
            DaemonBusy::Starting => "starting hutand…",
            DaemonBusy::Restarting => "restarting hutand…",
        }
    }

    /// The `connection` item's word and tooltip while the operation runs.
    fn chip(self) -> (&'static str, &'static str) {
        match self {
            DaemonBusy::Stopping => ("Stopping…", "Stopping hutand"),
            DaemonBusy::Starting => ("Starting…", "Starting hutand"),
            DaemonBusy::Restarting => ("Restarting…", "Restarting hutand"),
        }
    }
}

/// The last `daemon.status` answer and when it arrived (the uptime counts on from there).
#[derive(Debug, Clone)]
pub(crate) struct FetchedStatus {
    pub info: DaemonStatusInfo,
    pub at: Instant,
}

/// State of the daemon controls and of the quit flow.
pub(crate) struct DaemonControl {
    /// The daemon is stopped on purpose — from the app, or announced by the daemon
    /// (`daemon.stopping`: the CLI, its menu bar item): autostart is off until the user starts
    /// it. Not kept across launches: a relaunched app starts the daemon it needs.
    pub stopped: bool,
    pub busy: Option<DaemonBusy>,
    pub popover: bool,
    pub status: Option<FetchedStatus>,
    /// When `daemon.status` was last asked for (`None` while disconnected).
    status_tried: Option<Instant>,
    fetching: bool,
    /// Quitting has started; nothing else may run.
    pub quitting: bool,
    /// What the menu bar was last built for: the daemon menu and the current layout preset.
    menu: Option<(DaemonMenu, String)>,
    pub(crate) task: Option<Task<()>>,
    fetch: Option<Task<()>>,
    /// `hutand.log`, as `Open Logs` reveals it and the popover shows it.
    pub log_path: Option<PathBuf>,
    /// Reveals a file in Finder (also for workspace folders).
    pub reveal: RevealHandler,
    /// Opens a file or folder with the system (the Data & logs page's "Open").
    pub open: RevealHandler,
    /// What *Show the Process* found about a stuck daemon (`start_failure`).
    pub stuck_info: Option<String>,
    /// Which build of the daemon this app runs and how the running one differs
    /// (`lifecycle.rs`).
    pub(crate) identity: super::lifecycle::IdentityCheck,
    /// Said once the next start connected (the update of an idle old daemon).
    pub(crate) start_notice: Option<String>,
}

impl DaemonControl {
    pub fn new(own: Option<hutan_client::DaemonIdentity>) -> Self {
        Self {
            stopped: false,
            busy: None,
            popover: false,
            status: None,
            status_tried: None,
            fetching: false,
            quitting: false,
            menu: None,
            task: None,
            fetch: None,
            log_path: log_file(),
            reveal: Rc::new(|path, cx| cx.reveal_path(path)),
            open: Rc::new(|path, _| open_with_system(path)),
            stuck_info: None,
            identity: super::lifecycle::IdentityCheck::new(own),
            start_notice: None,
        }
    }
}

/// The app has the daemon actions ([`register_app_actions`]).
struct AppActions;

impl gpui::Global for AppActions {}

/// Handles the daemon actions app-wide, once per app. AppKit enables an item of the `Daemon`
/// menu when GPUI says its action is available (`App::is_action_available`): on the focused
/// element's dispatch path in the active window, or app-wide. The path does not reach the
/// workbench's root in every focus case, and there is none while no window is active; an
/// app-wide handler is found in all of them. Whether an item can act now is the menu's own
/// business (`app::daemon_items`).
///
/// The handler runs the action on the workbench window (the active one, else the first),
/// deferred because the dispatch may come from inside that window's own update. An action
/// that shows something there (the popover, a confirmation) brings the window forward when it
/// was not active.
pub(crate) fn register_app_actions(cx: &mut App) {
    if cx.has_global::<AppActions>() {
        return;
    }
    cx.set_global(AppActions);
    on::<daemon::Status>(cx, true, |wb, window, cx| wb.show_daemon_status(window, cx));
    on::<daemon::Restart>(cx, true, |wb, window, cx| wb.restart_daemon(window, cx));
    on::<daemon::Stop>(cx, true, |wb, window, cx| wb.stop_daemon(window, cx));
    on::<daemon::Start>(cx, false, |wb, _, cx| wb.start_daemon(cx));
    on::<daemon::OpenLogs>(cx, false, |wb, _, cx| wb.open_daemon_logs(cx));
    on::<daemon::ToggleKeepRunning>(cx, false, |wb, _, cx| wb.toggle_keep_running(cx));
    on::<daemon::ToggleStartAtLogin>(cx, false, |wb, _, cx| wb.toggle_start_at_login(cx));
    on::<daemon::OpenLoginItems>(cx, false, |wb, _, cx| wb.open_login_items(cx));
    on::<daemon::StartWithEmptyData>(cx, true, |wb, window, cx| {
        wb.start_with_empty_data(window, cx)
    });
    on::<daemon::ShowStuckDaemon>(cx, true, |wb, _, cx| wb.show_stuck_daemon(cx));
    on::<daemon::StopStuckDaemon>(cx, true, |wb, window, cx| wb.stop_stuck_daemon(window, cx));
    on::<daemon::CopyStartError>(cx, false, |wb, _, cx| wb.copy_start_error(cx));
    on::<daemon::CopyUpdateCommand>(cx, false, |wb, _, cx| wb.copy_update_command(cx));
    on::<daemon::UseOwnDaemon>(cx, false, |wb, _, cx| wb.use_own_daemon(cx));
    on::<daemon::DismissMismatch>(cx, false, |wb, _, cx| wb.dismiss_daemon_mismatch(cx));
}

/// One app-wide daemon action: `run` on the workbench window; `forward` brings it forward
/// first when it is not the active window.
fn on<A: gpui::Action>(
    cx: &mut App,
    forward: bool,
    run: fn(&mut Workbench, &mut Window, &mut Context<Workbench>),
) {
    cx.on_action(move |_: &A, cx| {
        let active = cx
            .active_window()
            .and_then(|window| window.downcast::<Workbench>());
        let Some(handle) = active.or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<Workbench>())
        }) else {
            tracing::debug!("{} without a workbench window", A::name_for_type());
            return;
        };
        cx.defer(move |cx| {
            let ran = handle.update(cx, |workbench, window, cx| {
                if forward && !window.is_window_active() {
                    workbench.bring_forward(window, cx);
                }
                run(workbench, window, cx);
            });
            if let Err(error) = ran {
                tracing::debug!("the workbench window went away: {error}");
            }
        });
    });
}

/// `hutand.log` where the daemon writes it, next to `desktop.log` ([`crate::crash::log_dir`]).
pub fn log_file() -> Option<PathBuf> {
    crate::crash::log_dir().map(|dir| dir.join("hutand.log"))
}

/// Opens `path` the way a double click in Finder does (`open`); the result is not waited for.
fn open_with_system(path: &Path) {
    match std::process::Command::new("open").arg(path).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait().ok());
        }
        Err(error) => tracing::warn!("cannot open {}: {error}", path.display()),
    }
}

fn sessions_phrase(live: usize) -> String {
    format!("{live} live session{}", if live == 1 { "" } else { "s" })
}

/// Asks the daemon to shut down and waits until its socket is gone (it is removed just before
/// the daemon releases its lock, so a successor can start). A daemon that is already gone
/// counts as stopped. `restart`: a daemon is started right after (protocol 0.42: other clients
/// are told it is no stop to keep).
async fn shutdown_and_wait(
    connection: &Connection,
    stop_sessions: bool,
    restart: bool,
    background: &BackgroundExecutor,
) -> Result<(), ClientError> {
    match connection
        .call::<Value>(Request::DaemonShutdown(DaemonShutdown {
            stop_sessions,
            forget: false,
            restart,
        }))
        .await
    {
        Ok(_) | Err(ClientError::Disconnected) => {}
        Err(error) => return Err(error),
    }
    if let Some(socket) = connection.socket_path().map(Path::to_owned) {
        let gone = background
            .spawn(async move { wait_socket_gone(&socket, STOP_WAIT) })
            .await;
        if !gone {
            tracing::warn!("hutand accepted the shutdown but its socket is still there");
        }
    }
    Ok(())
}

/// Blocking; runs on the background executor.
fn wait_socket_gone(socket: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while std::fs::symlink_metadata(socket).is_ok() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

/// Waits (without a timer: reconnect attempts publish a new state at least every 5 s) until
/// the connection is up again; `false` after `limit`, or as soon as a start fails anew (a
/// [`StoreState::daemon_start`] other than the one before).
async fn wait_connected(state: &mut watch::Receiver<Arc<StoreState>>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    let before = state.borrow().daemon_start.clone();
    loop {
        {
            let now = state.borrow_and_update();
            if now.connection == ConnectionState::Connected {
                return true;
            }
            if now.daemon_start.is_some() && now.daemon_start != before {
                return false;
            }
        }
        if Instant::now() >= deadline || state.changed().await.is_err() {
            return false;
        }
    }
}

impl Workbench {
    // ---- read access -------------------------------------------------------------------------

    /// Quitting has started (the connection is closed and the app is going away).
    pub fn quitting(&self) -> bool {
        self.daemon.quitting
    }

    /// The user stopped the daemon from the app and has not started it again.
    pub fn daemon_stopped(&self) -> bool {
        self.daemon.stopped
    }

    /// The daemon operation in progress, if any.
    pub fn daemon_busy(&self) -> Option<DaemonBusy> {
        self.daemon.busy
    }

    pub fn daemon_popover_open(&self) -> bool {
        self.daemon.popover
    }

    /// The last `daemon.status` the popover fetched (`None` while disconnected).
    pub fn daemon_status(&self) -> Option<&DaemonStatusInfo> {
        self.daemon.status.as_ref().map(|s| &s.info)
    }

    /// What closing the last window does now: `app.on_last_window_close`, except that
    /// `menu_bar` needs the menu bar item and quits without it.
    pub fn last_window_close(&self) -> LastWindowClose {
        self.settings
            .get::<AppSettings>()
            .effective_last_window_close()
    }

    /// Sessions the daemon runs right now.
    pub fn live_sessions(&self, cx: &App) -> usize {
        self.sessions
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| s.state.is_live())
            .count()
    }

    pub(crate) fn connection(&self, cx: &App) -> Connection {
        self.sessions.read(cx).connection().clone()
    }

    pub(crate) fn daemon_connected(&self, cx: &App) -> bool {
        self.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// What the connection status item and the popover headline say.
    pub(crate) fn daemon_line(&self, cx: &App) -> (String, StatusTone) {
        if let Some(busy) = self.daemon.busy {
            return (busy.label().to_owned(), StatusTone::Warn);
        }
        if self.daemon.stopped {
            return (STOPPED_BY_YOU.to_owned(), StatusTone::Error);
        }
        if let Some(line) = self.start_failure_line(cx) {
            return line;
        }
        connection_label(self.sessions.read(cx).connection_state())
    }

    /// The text of the connection status item ("hutand connected", "hutand stopped", …).
    pub fn daemon_status_line(&self, cx: &App) -> String {
        self.daemon_line(cx).0
    }

    /// The `connection` status item, always drawn: a `state.ok` dot and `hutand` while the
    /// daemon is up (the tooltip says its version, pid, uptime, sessions and clients), else the
    /// state in its colour: `Reconnecting…`, `Starting…`, `Stopped · Start`,
    /// `Can't start · Details`. A click opens the daemon popover.
    pub fn connection_chip(&self, cx: &App) -> ConnectionChip {
        if let Some(busy) = self.daemon.busy {
            let (word, tip) = busy.chip();
            return ConnectionChip {
                kind: ChipKind::Busy,
                word: word.to_owned(),
                tooltip: format!("{tip} · {CHIP_HINT}"),
            };
        }
        if self.daemon.stopped {
            return ConnectionChip {
                kind: ChipKind::Stopped,
                word: "Stopped · Start".to_owned(),
                tooltip: format!("You stopped hutand · {CHIP_HINT}"),
            };
        }
        if let Some(kind) = self.start_failure_kind(cx) {
            return ConnectionChip {
                kind: ChipKind::Failed,
                word: "Can't start · Details".to_owned(),
                tooltip: format!("{} · {CHIP_HINT}", kind.status_text()),
            };
        }
        let mut chip = chip_of_connection(self.sessions.read(cx).connection_state());
        if chip.kind == ChipKind::Up {
            chip.tooltip = self.up_tooltip(cx);
        }
        chip
    }

    /// Version, pid, uptime, sessions and clients of the daemon that is up, one per line.
    fn up_tooltip(&self, cx: &App) -> String {
        let (live, total) = {
            let sessions = self.sessions.read(cx).sessions();
            (
                sessions.iter().filter(|s| s.state.is_live()).count(),
                sessions.len(),
            )
        };
        let mut lines = Vec::new();
        match &self.daemon.status {
            Some(status) => {
                let info = &status.info;
                lines.push(format!(
                    "hutand {} · protocol {}.{}",
                    info.daemon_version, info.protocol.major, info.protocol.minor
                ));
                lines.push(format!("PID {}", info.pid));
                lines.push(format!(
                    "Uptime {}",
                    super::daemon_popover::uptime_text(
                        Duration::from_millis(info.uptime_ms) + status.at.elapsed()
                    )
                ));
                lines.push(format!("Sessions {live} live / {total} total"));
                lines.push(format!("Clients {}", info.clients));
            }
            None => {
                lines.push("hutand is running".to_owned());
                lines.push(format!("Sessions {live} live / {total} total"));
            }
        }
        lines.push(CHIP_HINT.to_owned());
        lines.join("\n")
    }

    // ---- menu bar ----------------------------------------------------------------------------

    pub(crate) fn menu_state(&self, cx: &App) -> DaemonMenu {
        let mismatch = self.daemon_mismatch().map(|m| &m.kind);
        DaemonMenu {
            running: self.daemon_connected(cx),
            busy: self.daemon.busy.is_some(),
            keep_running: self.keep_running_enabled(),
            start_at_login: self.start_at_login_enabled(),
            start_at_login_available: self.start_at_login_available(),
            start_at_login_status: self.start_at_login_status(),
            start_failure: self.start_failure_kind(cx),
            newer_daemon: matches!(mismatch, Some(Mismatch::Newer)),
            other_daemon: matches!(
                mismatch,
                Some(Mismatch::Different | Mismatch::Foreign { .. })
            ),
        }
    }

    /// Rebuilds the menu bar for the current layout presets and daemon state, and the open
    /// palette's list, which offers the same recovery commands.
    pub(crate) fn refresh_menus(&mut self, cx: &mut Context<Self>) {
        let state = self.menu_state(cx);
        cx.set_menus(crate::app::menus(&self.presets, &self.state.preset, &state));
        self.daemon.menu = Some((state, self.state.preset.clone()));
        if self.palette.is_some() {
            self.refilter_palette(cx);
            cx.notify();
        }
    }

    /// Rebuilds the menu bar when the daemon menu's checks, enabled or listed items, or the
    /// preset View → Layout Preset checks, changed.
    pub(crate) fn sync_menus(&mut self, cx: &mut Context<Self>) {
        let current = self
            .daemon
            .menu
            .as_ref()
            .map(|(menu, preset)| (*menu, preset.as_str()));
        if current != Some((self.menu_state(cx), self.state.preset.as_str())) {
            self.refresh_menus(cx);
        }
    }

    /// Matches the open palette's query again, leaving out the recovery commands that do not
    /// apply now ([`DaemonMenu::offers`]): the palette lists what the `Daemon` menu lists.
    pub(crate) fn refilter_palette(&mut self, cx: &App) {
        let offered = self.menu_state(cx);
        let commands = Rc::clone(&self.commands);
        if let Some(palette) = &mut self.palette {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            palette.refilter(commands.iter().map(|c| Candidate {
                title: c.title.as_ref(),
                // The commands people use have a key: they rank above the ones that have none.
                has_key: keymap.bindings_for_action(&*c.hint).next().is_some(),
            }));
            palette
                .matches
                .retain(|&i| commands.get(i).is_some_and(|c| offered.offers(&*c.action)));
        }
    }

    /// An operation on the daemon starts or ends: the menu follows (nothing in it can act
    /// meanwhile).
    fn set_busy(&mut self, busy: Option<DaemonBusy>, cx: &mut Context<Self>) {
        self.daemon.busy = busy;
        self.sync_menus(cx);
    }

    /// The connection changed: keep the menu, the popover data and the terminals current.
    pub(crate) fn on_daemon_state_changed(&mut self, cx: &mut Context<Self>) {
        self.follow_announced_stop(cx);
        self.follow_daemon_started(cx);
        self.check_daemon_identity(cx);
        self.sync_menus(cx);
        self.sync_mac_surfaces(cx);
        self.daemon_settings_follow_run(cx);
        if self.daemon.popover {
            self.refresh_daemon_status(cx);
        }
        if !self.daemon_connected(cx) {
            self.daemon.status = None;
            self.daemon.status_tried = None;
        } else {
            self.daemon.stuck_info = None;
        }
        self.maybe_auto_install(cx);
    }

    // ---- popover -----------------------------------------------------------------------------

    /// `daemon::Status`: opens the popover with the daemon's status and controls.
    pub fn show_daemon_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        self.context_menu = None;
        self.daemon.popover = true;
        window.focus(&self.daemon_focus, cx);
        self.refresh_daemon_status(cx);
        cx.notify();
    }

    pub fn close_daemon_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.daemon.popover) {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn refresh_daemon_status(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) {
            self.daemon.status = None;
            return;
        }
        if self.daemon.fetching {
            return;
        }
        self.daemon.fetching = true;
        self.daemon.status_tried = Some(Instant::now());
        let connection = self.connection(cx);
        self.daemon.fetch = Some(cx.spawn(async move |this, cx| {
            let info = connection
                .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
                .await;
            this.update(cx, |this, cx| {
                this.daemon.fetching = false;
                match info {
                    Ok(info) => {
                        this.daemon.status = Some(FetchedStatus {
                            info,
                            at: Instant::now(),
                        });
                    }
                    Err(error) => tracing::debug!("daemon.status failed: {error}"),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The status bar is drawn: the `connection` tooltip reads a `daemon.status` that is at
    /// most [`STATUS_REFRESH`] old. Asked once per interval, also when the answer is an error.
    pub(crate) fn keep_daemon_status_fresh(&mut self, cx: &mut Context<Self>) {
        let stale = self
            .daemon
            .status_tried
            .is_none_or(|at| at.elapsed() > STATUS_REFRESH);
        if stale && self.daemon_connected(cx) {
            self.refresh_daemon_status(cx);
        }
    }

    // ---- stop / start / restart --------------------------------------------------------------

    /// `daemon::Stop`: stops the sessions and the daemon; the app stays open and shows the
    /// daemon as stopped until it is started again. Confirms first while sessions are live.
    pub fn stop_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        let live = self.live_sessions(cx);
        if !self.daemon_connected(cx) || live == 0 {
            self.begin_stop(false, cx);
            return;
        }
        self.open_destructive(
            "Stop the daemon?",
            format!(
                "{} will be stopped with the daemon. Hutan stays open.",
                sessions_phrase(live)
            ),
            "Stop sessions and daemon",
            |this, _, cx| this.begin_stop(false, cx),
            window,
            cx,
        );
    }

    /// `daemon::Restart`: stops the daemon, then starts a new one (a new epoch: terminals end
    /// or re-attach as the daemon says). Confirms first while sessions are live.
    pub fn restart_daemon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        let live = self.live_sessions(cx);
        if !self.daemon_connected(cx) {
            self.begin_start(false, cx);
            return;
        }
        if live == 0 {
            self.begin_stop(true, cx);
            return;
        }
        let policy = self
            .daemon_settings()
            .value("daemon.restore_sessions")
            .and_then(Value::as_str);
        self.open_destructive(
            "Restart the daemon?",
            super::settings_labels::words::restart_message(&sessions_phrase(live), policy),
            "Stop sessions and restart",
            |this, _, cx| this.begin_stop(true, cx),
            window,
            cx,
        );
    }

    /// `daemon::Start`: starts `hutand` and reconnects.
    pub fn start_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon.busy.is_some() {
            return;
        }
        if self.daemon_connected(cx) {
            if self.daemon.stopped || self.sessions.read(cx).daemon_stopped() {
                // The daemon runs again (started elsewhere): reconnect for real, drop the
                // stopped state.
                self.resume_after_external_start(cx);
            } else {
                self.set_notice("The daemon is already running", cx);
            }
            return;
        }
        self.begin_start(false, cx);
    }

    /// The store saw a daemon of a new epoch come up while this window showed the stopped
    /// state: it was started by another path (CLI, autostart, another client).
    fn follow_daemon_started(&mut self, cx: &mut Context<Self>) {
        if self.daemon.stopped
            && self.daemon.busy.is_none()
            && !self.sessions.read(cx).daemon_stopped()
        {
            self.resume_after_external_start(cx);
        }
    }

    /// Leaves the stopped state without launching anything and lets the connection bring a
    /// daemon back again should this one go away.
    fn resume_after_external_start(&mut self, cx: &mut Context<Self>) {
        self.set_daemon_stopped(false, cx);
        self.connection(cx).set_autostart(true);
        cx.notify();
    }

    /// `daemon::OpenLogs`: reveals `hutand.log` in Finder. Nothing else happens.
    pub fn open_daemon_logs(&mut self, cx: &mut Context<Self>) {
        match self.daemon.log_path.clone() {
            Some(path) if path.is_file() => (self.daemon.reveal)(&path, cx),
            Some(path) => {
                self.set_notice(format!("No daemon log yet ({})", path.display()), cx);
            }
            None => self.set_notice("Cannot locate the daemon log", cx),
        }
    }

    /// Where `Open Logs` looks for `hutand.log` (default: the daemon's own log location).
    pub fn set_daemon_log_path(&mut self, path: PathBuf) {
        self.daemon.log_path = Some(path);
    }

    /// How `Open Logs` and "Reveal in Finder" reveal a path (default: Finder through the
    /// platform).
    pub fn set_reveal_handler(&mut self, reveal: impl Fn(&Path, &mut App) + 'static) {
        self.daemon.reveal = Rc::new(reveal);
    }

    /// How "Open" on the Data & logs page opens a file or folder (default: the system's `open`).
    pub fn set_open_handler(&mut self, open: impl Fn(&Path, &mut App) + 'static) {
        self.daemon.open = Rc::new(open);
    }

    /// Stops reconnecting, asks the daemon to shut down (stopping its sessions) and waits for
    /// it to go. `restart` then starts a new one (the `hutand` this app starts); otherwise the
    /// daemon stays stopped.
    pub(crate) fn begin_stop(&mut self, restart: bool, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        // Off first: the connection must not bring the daemon back while it shuts down.
        connection.set_autostart(false);
        self.set_busy(
            Some(if restart {
                DaemonBusy::Restarting
            } else {
                DaemonBusy::Stopping
            }),
            cx,
        );
        if !restart {
            self.set_daemon_stopped(true, cx);
        }
        cx.notify();
        let background = cx.background_executor().clone();
        self.daemon.task = Some(cx.spawn(async move |this, cx| {
            let result = shutdown_and_wait(&connection, true, restart, &background).await;
            this.update(cx, |this, cx| this.stop_finished(restart, result, cx))
                .ok();
        }));
    }

    fn stop_finished(
        &mut self,
        restart: bool,
        result: Result<(), ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Err(error) => {
                tracing::warn!("cannot stop the daemon: {error}");
                self.connection(cx).set_autostart(true);
                self.set_busy(None, cx);
                self.set_daemon_stopped(false, cx);
                self.set_notice(format!("Cannot stop the daemon: {error}"), cx);
            }
            Ok(()) if restart => self.begin_start(true, cx),
            Ok(()) => {
                self.set_busy(None, cx);
                // A daemon may have been started while the stop was finishing.
                self.follow_daemon_started(cx);
                self.sync_status_item(cx);
                cx.notify();
            }
        }
    }

    /// Allows autostart again and dials at once (which starts `hutand`), then waits for the
    /// connection.
    pub(crate) fn begin_start(&mut self, restarting: bool, cx: &mut Context<Self>) {
        let connection = self.connection(cx);
        self.set_busy(
            Some(if restarting {
                DaemonBusy::Restarting
            } else {
                DaemonBusy::Starting
            }),
            cx,
        );
        self.set_daemon_stopped(false, cx);
        let mut state = connection.state();
        connection.start_daemon_now();
        cx.notify();
        self.daemon.task = Some(cx.spawn(async move |this, cx| {
            let started = wait_connected(&mut state, START_WAIT).await;
            this.update(cx, |this, cx| {
                this.set_busy(None, cx);
                this.sync_status_item(cx);
                if started {
                    if let Some(notice) = this.daemon.start_notice.take() {
                        this.set_notice(notice, cx);
                    }
                } else {
                    this.daemon.start_notice = None;
                    let why = this
                        .daemon_start_failure(cx)
                        .map(|failure| failure.sentence())
                        .unwrap_or_else(|| "see the daemon log".to_owned());
                    this.set_notice(format!("The daemon did not start: {why}"), cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(crate) fn set_daemon_stopped(&mut self, stopped: bool, cx: &mut Context<Self>) {
        self.daemon.stopped = stopped;
        self.sessions
            .update(cx, |store, cx| store.set_daemon_stopped(stopped, cx));
        // The menu bar item says "stopped" and "stopping…" too.
        self.sync_status_item(cx);
    }

    // ---- quit --------------------------------------------------------------------------------

    /// ⌘Q, the app menu, the menu bar item's Quit and closing the last window when
    /// `app.on_last_window_close` is `quit` (design.md § Daemon lifecycle). The daemon follows
    /// the app: it stops with it, silently when no session is live and after a confirmation when
    /// some are, unless it was asked to outlive the app (`app.keep_running`) or launchd owns it
    /// (`app.start_at_login`, registered and approved): then the app just leaves.
    /// While disconnected there is no daemon to stop and the app quits at once.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.daemon.quitting {
            // The first Quit has settled everything and asked the platform to end the process;
            // it is still here. Ask again rather than leave the user with an item that does
            // nothing.
            cx.quit();
            return;
        }
        if !self.daemon_connected(cx) || self.daemon_outlives_app() {
            self.finish_quit(cx);
            return;
        }
        if self.live_sessions(cx) == 0 {
            self.shutdown_then_quit(false, window, cx);
            return;
        }
        self.open_quit_dialog(window, cx);
    }

    /// "Quit Hutan and stop N sessions?": Stop Sessions & Quit, Keep Running in Background
    /// (turns `app.keep_running` on, so the daemon stays; no login item), Cancel.
    fn open_quit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Quit from the menu bar item or the Dock while the window is hidden, minimised or behind
        // other apps: the question is in that window, so the window comes forward with it.
        self.bring_forward(window, cx);
        let live = self.live_sessions(cx);
        let modal = Modal {
            title: SharedString::from(format!("Quit Hutan and stop {}?", sessions_phrase(live))),
            message: SharedString::from(
                "The daemon stops with Hutan, and so do its sessions. To leave them running, \
                 keep the daemon running in the background: Hutan turns on \"Keep daemon \
                 running after Hutan quits\" and quits. That does not start it at login.",
            ),
            confirm_label: "Stop sessions and quit".into(),
            input: None,
            secure_input: false,
            destructive: true,
            choice: Some(ModalChoice {
                destructive: false,
                label: "Keep running in background".into(),
                run: Rc::new(|this, _, _, cx| this.keep_running_in_background(cx)),
            }),
            checkbox: None,
            on_confirm: Rc::new(|this, _, window, cx| this.shutdown_then_quit(true, window, cx)),
        };
        self.open_modal(modal, window, cx);
    }

    /// The quit dialog's second choice: turns `app.keep_running` on (nothing is registered with
    /// the system) and quits leaving the daemon. When the setting cannot be saved the app stays
    /// open (the notice says why), because the user asked for the daemon to stay and that is not
    /// settled.
    fn keep_running_in_background(&mut self, cx: &mut Context<Self>) {
        self.set_keep_running(true, cx);
        if !self.keep_running_enabled() {
            return;
        }
        self.finish_quit(cx);
    }

    /// The platform is terminating the app by a way that is not ours (the Dock's Quit, logout,
    /// a system shutdown): there is no window to ask in. An idle daemon stops with the app, as
    /// with ⌘Q; one with live sessions is left running with them, as after a signal. Our own
    /// quit paths have settled the daemon already (`quitting`). The request goes out before
    /// the process ends; the daemon answers it by itself.
    pub fn on_platform_quit(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl std::future::Future<Output = ()> + use<> {
        self.flush_save();
        let idle = !self.daemon.quitting
            && self.daemon_connected(cx)
            && !self.daemon_outlives_app()
            && self.live_sessions(cx) == 0;
        let connection = idle.then(|| self.connection(cx));
        async move {
            if let Some(connection) = connection {
                let _ = connection
                    .call::<Value>(Request::DaemonShutdown(DaemonShutdown {
                        stop_sessions: false,
                        forget: false,
                        restart: false,
                    }))
                    .await;
            }
        }
    }

    /// SIGTERM / SIGINT / SIGHUP: save, disconnect and quit; never asks and never stops the
    /// daemon.
    pub fn quit_leaving_daemon(&mut self, cx: &mut Context<Self>) {
        self.finish_quit(cx);
    }

    /// The window's close button. Closing the last window does what `app.on_last_window_close`
    /// says: `quit` runs the quit flow (the window stays until that is decided), `menu_bar`
    /// hides the app and keeps it in the menu bar. Closing any other window just closes it.
    /// Returns whether the window may close now.
    pub(crate) fn window_close_requested(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.flush_save();
        if self.daemon.quitting || cx.windows().len() > 1 {
            return true;
        }
        match self.last_window_close() {
            LastWindowClose::Quit => self.request_quit(window, cx),
            // The window is kept (hidden): it holds the connection, the sessions and the
            // layout, which the menu bar item and the shortcut keep serving.
            LastWindowClose::MenuBar => self.hide_to_menu_bar(cx),
        }
        false
    }

    /// `daemon.shutdown`, then quit. If sessions started since the decision (the daemon refuses
    /// a shutdown that does not stop them), the decision is made again.
    fn shutdown_then_quit(
        &mut self,
        stop_sessions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.connection(cx);
        let background = cx.background_executor().clone();
        self.set_notice("Stopping the daemon…", cx);
        self.daemon.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = shutdown_and_wait(&connection, stop_sessions, false, &background).await;
            this.update_in(cx, |this, window, cx| match result {
                Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                    this.notice = None;
                    this.open_quit_dialog(window, cx);
                }
                Err(error) => {
                    tracing::warn!("quitting without a clean daemon shutdown: {error}");
                    this.finish_quit(cx);
                }
                Ok(()) => this.finish_quit(cx),
            })
            .ok();
        }));
    }

    /// Saves the layout, closes the connection and quits (exit code 0). The daemon is left as
    /// it is; callers that want it stopped have asked it through `daemon.shutdown` already.
    fn finish_quit(&mut self, cx: &mut Context<Self>) {
        if self.daemon.quitting {
            return;
        }
        self.daemon.quitting = true;
        self.flush_save();
        self.connection(cx).shutdown();
        cx.quit();
    }
}
