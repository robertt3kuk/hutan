//! The daemon's own menu bar item (M6-D) end to end: a real daemon on a temporary `HUTAN_HOME`,
//! real client connections to its socket, real PTY sessions, the fake status item and the fake
//! app launcher standing in for AppKit and `open`. The item shows while no desktop is
//! connected, hides while one is, follows who needs the user, and routes its menu: open
//! (session, Hutan, settings), stop with confirmation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, create, sh, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{ApprovalPolicy, SandboxMode, SessionId};
use hutan_daemon::menu_bar::fake::{Call, FakeLauncher, FakeStatusItem, menu_bar};
use hutan_daemon::menu_bar::{MenuChoice, MenuModel, Need, OPEN_SETTINGS_ACTION, StopAnswer};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{
    AgentReportRequest, AgentState, ClientKind, Empty, Event, Request, SessionInfo, SessionResult,
    StopBy,
};
use hutan_protocol::ui::{
    AppearanceSummary, DockSummary, FocusSummary, LayoutSummary, PaneItem, PaneSummary, UiAction,
    UiPublish,
};
use tokio::time::{sleep, timeout};

/// Short enough to keep the tests quick, long enough that a started desktop beats it.
const SHOW_DELAY: Duration = Duration::from_millis(80);
const PATIENCE: Duration = Duration::from_secs(10);

/// What a test changes about the default daemon.
#[derive(Default)]
struct Opts {
    show_delay: Option<Duration>,
    patience: Option<Duration>,
    /// The committed codex fixture the agent replays.
    codex: Option<&'static str>,
    /// `settings.toml`, written before the daemon starts.
    settings: Option<&'static str>,
}

struct Env {
    daemon: TestDaemon,
    item: Arc<FakeStatusItem>,
    launcher: Arc<FakeLauncher>,
    /// A CLI client, for creating sessions and reading what the daemon says.
    client: Client,
    /// Where the fake agent works.
    work: tempfile::TempDir,
}

impl Env {
    async fn start() -> Self {
        Self::start_with(Opts::default()).await
    }

    async fn start_with(opts: Opts) -> Self {
        let item = FakeStatusItem::new();
        let launcher = FakeLauncher::new();
        let bar = menu_bar(
            &item,
            &launcher,
            opts.show_delay.unwrap_or(SHOW_DELAY),
            opts.patience.unwrap_or(PATIENCE),
        );
        let work = tempfile::tempdir().unwrap();
        let config = opts
            .codex
            .map(|name| replay(name, &work.path().join("received.jsonl")));
        let settings = opts.settings;
        let daemon = TestDaemon::start_with("", move |c| {
            c.menu_bar = Some(bar);
            c.codex = config;
            if let Some(settings) = settings {
                std::fs::write(c.paths.base.join("settings.toml"), settings).unwrap();
            }
        })
        .await;
        let (client, _) = daemon.connect_subscribed().await;
        Self {
            daemon,
            item,
            launcher,
            client,
            work,
        }
    }

    /// A desktop: a `desktop` client that has subscribed, with the events it sees.
    async fn desktop(&self) -> Client {
        let client = self.daemon.connect_as(ClientKind::Desktop).await;
        client
            .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        client
    }

    async fn shown(&self, what: &str, done: impl Fn(&MenuModel) -> bool) -> MenuModel {
        self.item.wait_shown(what, done).await
    }

    /// A session whose process sleeps, so that it is live.
    async fn live(&self) -> SessionInfo {
        create(&self.client, sh("exec sleep 600")).await
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

/// The fake app server, next to the test binaries; `just test` builds the workspace binaries
/// first.
fn fixture_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn replay(name: &str, received: &Path) -> CodexConfig {
    CodexConfig {
        program: fixture_bin(),
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            fixtures()
                .join(format!("{name}.jsonl"))
                .display()
                .to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

fn blocked_report(session: SessionId, state: AgentState, seq: u64) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        session_id: session,
        source: "test-agent".into(),
        agent: "fake".into(),
        state: Some(state),
        message: Some("waiting for\nyou".into()),
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }
}

fn window(window_id: &str) -> UiPublish {
    UiPublish {
        actions: vec![UiAction {
            id: OPEN_SETTINGS_ACTION.into(),
            title: "App: Settings…".into(),
            args_schema: None,
        }],
        settings: Vec::new(),
        layout: LayoutSummary {
            window_id: window_id.to_owned(),
            workspace_id: None,
            preset: "default".into(),
            docks: vec![DockSummary {
                position: "left".into(),
                visible: true,
                panels: vec!["sessions".into()],
                active: Some("sessions".into()),
            }],
            panes: PaneSummary::Pane {
                pane_id: "pane-1".into(),
                items: vec![PaneItem {
                    kind: "placeholder".into(),
                    session_id: None,
                    panel: None,
                    path: None,
                    title: "Welcome".into(),
                }],
                active: Some(0),
            },
            focused: FocusSummary {
                kind: "pane".into(),
                target: "pane-1".into(),
            },
            zoomed: None,
            appearance: AppearanceSummary {
                chrome: "system".into(),
                density: "compact".into(),
            },
            window_active: true,
        },
    }
}

async fn publish_window(desktop: &Client, id: &str) {
    desktop
        .call::<Empty>(Request::UiPublish(window(id)))
        .await
        .unwrap();
}

// ---- who is on the menu bar -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_item_is_shown_with_no_desktop_hidden_while_one_is_connected_and_back_after() {
    let t = Env::start().await;
    let first = t.shown("the item with no desktop", |_| true).await;
    assert_eq!(first.live_sessions, 0);
    assert!(!first.dot());

    // A CLI client is no desktop.
    let cli = t.daemon.connect().await;
    sleep(Duration::from_millis(300)).await;
    assert!(t.item.shown().is_some(), "only a desktop hides the item");
    drop(cli);

    // A desktop hides it at once (it was listed as connected the moment it said hello).
    let desktop = t.desktop().await;
    t.item.wait_hidden("the item to hide for the desktop").await;
    assert_eq!(
        t.item.calls().iter().filter(|c| **c == Call::Hide).count(),
        1,
        "one hide"
    );

    // Another desktop (a second window's process) changes nothing; the item stays away until
    // the last one is gone.
    let second = t.desktop().await;
    drop(desktop);
    sleep(Duration::from_millis(300)).await;
    assert!(t.item.shown().is_none(), "a desktop is still connected");

    drop(second);
    let back = t.shown("the item after the desktops left", |_| true).await;
    assert_eq!(back.live_sessions, 0);
    let item = t.item.clone();
    t.stop().await;
    assert_eq!(
        item.calls().last(),
        Some(&Call::Hide),
        "the item leaves with the daemon"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_that_reconnects_within_the_delay_never_lets_the_item_show() {
    // The desktop of a daemon it started (or one that dropped and dials again) is back before
    // the item would be on the menu bar: the menu bar never shows two Hutan icons.
    let t = Env::start_with(Opts {
        show_delay: Some(Duration::from_millis(1500)),
        ..Opts::default()
    })
    .await;
    let first = t.desktop().await;
    sleep(Duration::from_millis(200)).await;
    drop(first);
    sleep(Duration::from_millis(300)).await;
    let _second = t.desktop().await;
    sleep(Duration::from_millis(2000)).await;
    assert_eq!(
        t.item.calls(),
        Vec::<Call>::new(),
        "the item was never on the menu bar"
    );
    t.stop().await;
}

/// A desktop process, as far as the daemon can tell: it listens on `desktop.sock` in the data
/// folder (`hutan_desktop::instance`), whether or not it is connected to the daemon.
fn desktop_process(t: &Env) -> std::os::unix::net::UnixListener {
    std::os::unix::net::UnixListener::bind(
        t.daemon
            .paths
            .base
            .join(hutan_daemon::menu_bar::DESKTOP_SOCKET_FILE),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_that_is_running_but_not_connected_keeps_the_item_off_the_menu_bar() {
    // The desktop reconnecting after the daemon restarted (or after its connection dropped) is
    // not connected for longer than the show delay, and its own item is on the menu bar: the
    // daemon must not put a second one next to it.
    let t = Env::start().await;
    let connected = t.desktop().await;
    t.item.wait_hidden("the item to hide for the desktop").await;
    let process = desktop_process(&t);
    drop(connected);
    // Several show delays and polls later: still no second Hutan icon.
    sleep(SHOW_DELAY * 10).await;
    assert_eq!(
        t.item.shown(),
        None,
        "a desktop process runs; its item is the one on the menu bar"
    );

    // The desktop quits (its socket goes with it): now the daemon's item is the only one.
    drop(process);
    std::fs::remove_file(
        t.daemon
            .paths
            .base
            .join(hutan_daemon::menu_bar::DESKTOP_SOCKET_FILE),
    )
    .unwrap();
    t.shown("the item after the desktop process quit", |_| true)
        .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_file_left_by_a_crashed_desktop_does_not_keep_the_item_off() {
    let t = Env::start().await;
    // A socket file nobody listens on refuses connections: no desktop runs.
    let socket = t
        .daemon
        .paths
        .base
        .join(hutan_daemon::menu_bar::DESKTOP_SOCKET_FILE);
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());
    t.shown("the item with only a stale socket file", |_| true)
        .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_starts_while_a_desktop_runs_shows_no_item_until_it_is_gone() {
    // The daemon restarted under a running desktop: no connection yet, the desktop's item is on
    // the menu bar.
    let process = std::cell::RefCell::new(None);
    let item = FakeStatusItem::new();
    let launcher = FakeLauncher::new();
    let bar = menu_bar(&item, &launcher, SHOW_DELAY, PATIENCE);
    let daemon = common::TestDaemon::start_with("", |c| {
        c.menu_bar = Some(bar);
        *process.borrow_mut() = Some(
            std::os::unix::net::UnixListener::bind(
                c.paths
                    .base
                    .join(hutan_daemon::menu_bar::DESKTOP_SOCKET_FILE),
            )
            .unwrap(),
        );
    })
    .await;
    sleep(SHOW_DELAY * 10).await;
    assert_eq!(item.shown(), None, "a desktop runs; the daemon shows none");
    assert_eq!(item.calls(), Vec::<Call>::new());
    drop(process.take());
    item.wait_shown("the item after the desktop went", |_| true)
        .await;
    daemon.stop().await;
}

// ---- what it shows ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn live_sessions_and_the_dot_follow_the_daemon() {
    let t = Env::start().await;
    t.shown("the idle item", |m| m.live_sessions == 0).await;

    // The live count follows sessions starting and ending.
    let session = t.live().await;
    t.shown("one live session", |m| m.live_sessions == 1).await;
    common::stop(&t.client, session.id).await.unwrap();
    t.shown("no live session", |m| m.live_sessions == 0).await;

    // A blocked agent is a need, with its message on one line; unblocking clears it.
    let agent = t.live().await;
    let applied = t
        .client
        .agent_report(blocked_report(agent.id, AgentState::Blocked, 1))
        .await
        .unwrap();
    assert!(applied, "report ignored");
    let model = t
        .shown("the blocked agent", |m| {
            m.needs_you.iter().any(|e| e.need == Need::Blocked)
        })
        .await;
    assert!(model.dot());
    let entry = &model.needs_you[0];
    assert_eq!(entry.session, agent.id);
    assert_eq!(entry.detail, "waiting for you");
    assert_eq!(entry.workspace, Some(agent.workspace_id));

    let applied = t
        .client
        .agent_report(blocked_report(agent.id, AgentState::Working, 2))
        .await
        .unwrap();
    assert!(applied, "report ignored");
    let model = t.shown("the agent unblocked", |m| !m.dot()).await;
    assert_eq!(model.live_sessions, 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_that_failed_while_nobody_looked_needs_you_until_the_desktop_has_been_and_gone() {
    let t = Env::start().await;
    t.shown("the idle item", |_| true).await;
    let failed = create(&t.client, sh("exit 3")).await;
    let model = t
        .shown("the failed session", |m| {
            m.needs_you.iter().any(|e| e.need == Need::Failed)
        })
        .await;
    let entry = &model.needs_you[0];
    assert_eq!(entry.session, failed.id);
    assert_eq!(entry.detail, "exit 3");
    assert!(entry.intent.is_some());

    // The desktop was open at some point since: it showed the failure to the user, so the
    // daemon does not list it again when the desktop leaves.
    let desktop = t.desktop().await;
    t.item.wait_hidden("the item to hide").await;
    drop(desktop);
    let model = t.shown("the item after the desktop left", |_| true).await;
    assert!(
        model.needs_you.is_empty(),
        "what failed before the desktop came is the desktop's to show: {model:?}"
    );

    // What fails after it left is listed.
    let later = create(&t.client, sh("exit 4")).await;
    let model = t
        .shown("the later failure", |m| {
            m.needs_you.iter().any(|e| e.session == later.id)
        })
        .await;
    assert_eq!(model.needs_you.len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_approval_needs_you_until_it_is_answered() {
    let t = Env::start_with(Opts {
        codex: Some("approval-touch"),
        ..Opts::default()
    })
    .await;
    t.shown("the idle item", |_| true).await;
    let created = t
        .client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: t.work.path().to_owned(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: Some(ApprovalPolicy::Untrusted),
            sandbox: Some(SandboxMode::ReadOnly),
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap()
        .session;
    t.client
        .chat_send(
            created.id,
            "Run `touch hi.txt` in a shell, then reply with the word done.",
        )
        .await
        .unwrap();

    let model = t
        .shown("the approval", |m| {
            m.needs_you.iter().any(|e| e.need == Need::Approval)
        })
        .await;
    let entry = &model.needs_you[0];
    assert_eq!(entry.session, created.id);
    assert!(entry.detail.contains("touch hi.txt"), "{}", entry.detail);
    assert!(model.dot());

    let intents = t.client.notify_list().await.unwrap();
    let approval = intents
        .iter()
        .find(|i| i.kind == hutan_protocol::notify::IntentKind::ApprovalRequired)
        .expect("the approval's intent");
    t.client.notify_act(&approval.id, "accept").await.unwrap();
    t.shown("the approval answered", |m| {
        !m.needs_you.iter().any(|e| e.need == Need::Approval)
    })
    .await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_at_login_shows_the_setting_of_settings_toml() {
    let t = Env::start_with(Opts {
        settings: Some("[app]\nstart_at_login = true\n"),
        ..Opts::default()
    })
    .await;
    t.shown("start at login on", |m| m.start_at_login).await;
    t.stop().await;
}

// ---- the menu --------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn open_hutan_launches_the_app() {
    let t = Env::start().await;
    t.shown("the item", |_| true).await;
    t.item.click(MenuChoice::OpenHutan);
    timeout(WAIT, async {
        while t.launcher.opened() == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Hutan was not opened");
    assert_eq!(t.launcher.opened(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_requests_to_open_hutan_launch_once_until_a_desktop_comes() {
    let t = Env::start().await;
    t.shown("the item", |_| true).await;
    // Launch Services reopening the app while it starts, impatient clicks: one launch.
    for _ in 0..5 {
        t.item.click(MenuChoice::OpenHutan);
    }
    t.item.click(MenuChoice::OpenSettings);
    timeout(WAIT, async {
        while t.launcher.opened() == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Hutan was not opened");
    sleep(Duration::from_millis(300)).await;
    assert_eq!(t.launcher.opened(), 1, "a launch under way is not repeated");

    // The desktop came and went: the next request launches again.
    let desktop = t.desktop().await;
    t.item.wait_hidden("the item to hide for the desktop").await;
    drop(desktop);
    t.shown("the item back", |_| true).await;
    t.item.click(MenuChoice::OpenHutan);
    timeout(WAIT, async {
        while t.launcher.opened() < 2 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Hutan was not opened again");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_entry_launches_the_app_and_asks_the_desktop_once_its_window_is_up() {
    let t = Env::start().await;
    t.shown("the item", |_| true).await;
    let failed = create(&t.client, sh("exit 3")).await;
    t.shown("the failure", |m| !m.needs_you.is_empty()).await;
    assert_eq!(t.client.notify_list().await.unwrap().len(), 1);

    t.item.click(MenuChoice::OpenSession(failed.id));
    timeout(WAIT, async {
        while t.launcher.opened() == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Hutan was not opened");
    // Looking at it acknowledges the intent, so the entry goes.
    t.shown("the entry gone", |m| m.needs_you.is_empty()).await;
    assert!(t.client.notify_list().await.unwrap().is_empty());

    // The desktop that the launch brings up connects and subscribes; nothing is sent before a
    // window has published (it is not synchronised yet).
    let desktop = t.desktop().await;
    let mut events = desktop.events();
    sleep(Duration::from_millis(300)).await;
    assert!(
        !matches!(events.try_recv(), Ok((_, Event::UiOpen { .. }))),
        "no window yet"
    );

    publish_window(&desktop, "w1").await;
    let (session, workspace, at_ms) = wait_event(&mut events, "ui.open", |event| match event {
        Event::UiOpen {
            session_id,
            workspace_id,
            at_ms,
            ..
        } => Some((*session_id, *workspace_id, *at_ms)),
        _ => None,
    })
    .await;
    assert_eq!(session, failed.id);
    assert_eq!(workspace, Some(failed.workspace_id));
    assert!(at_ms > 0);
    assert_eq!(t.launcher.opened(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_settings_entry_launches_the_app_and_runs_its_settings_action() {
    let t = Env::start().await;
    t.shown("the item", |_| true).await;
    t.item.click(MenuChoice::OpenSettings);
    timeout(WAIT, async {
        while t.launcher.opened() == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Hutan was not opened");

    let desktop = t.desktop().await;
    let mut events = desktop.events();
    publish_window(&desktop, "w1").await;
    let action = wait_event(&mut events, "ui.command", |event| match event {
        Event::UiCommand { action, .. } => Some(action.clone()),
        _ => None,
    })
    .await;
    assert_eq!(action, OPEN_SETTINGS_ACTION);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_nobody_picked_up_in_time_lapses() {
    let t = Env::start_with(Opts {
        patience: Some(Duration::from_millis(150)),
        ..Opts::default()
    })
    .await;
    t.shown("the item", |_| true).await;
    t.item.click(MenuChoice::OpenSettings);
    sleep(Duration::from_millis(600)).await;

    // A desktop that comes much later is not suddenly asked to open its settings.
    let desktop = t.desktop().await;
    let mut events = desktop.events();
    publish_window(&desktop, "w1").await;
    sleep(Duration::from_millis(500)).await;
    while let Ok((_, event)) = events.try_recv() {
        assert!(
            !matches!(event, Event::UiCommand { .. }),
            "a lapsed request was delivered: {event:?}"
        );
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_daemon_stops_a_daemon_with_nothing_running_without_asking() {
    let t = Env::start().await;
    t.shown("the item", |_| true).await;
    let token = t.daemon.daemon.shutdown_token();
    // A CLI that listens (a desktop would hide the item).
    let (_listener, mut events) = t.daemon.connect_subscribed().await;
    t.item.click(MenuChoice::StopDaemon);
    timeout(WAIT, token.cancelled())
        .await
        .expect("the daemon did not stop");
    // Protocol 0.42: every client hears it was a stop the user asked for.
    let announced = timeout(WAIT, async {
        loop {
            match events.recv().await {
                Ok((_, Event::DaemonStopping { by, restart })) => return (by, restart),
                Ok(_) => {}
                Err(error) => panic!("no daemon.stopping: {error}"),
            }
        }
    })
    .await
    .expect("the stop was announced");
    assert_eq!(announced, (StopBy::MenuBar, false));
    assert!(
        !t.item
            .calls()
            .iter()
            .any(|c| matches!(c, Call::ConfirmStop(_))),
        "nothing to confirm"
    );
    t.item
        .wait_for("the item to leave", |calls| {
            calls.last() == Some(&Call::Hide)
        })
        .await;
    timeout(WAIT, t.daemon.daemon.wait())
        .await
        .expect("the daemon did not finish")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_daemon_asks_while_sessions_run_and_cancel_leaves_everything_as_it_was() {
    let t = Env::start().await;
    let session = t.live().await;
    t.shown("one live session", |m| m.live_sessions == 1).await;
    let token = t.daemon.daemon.shutdown_token();

    // Cancel (the fake's default answer).
    t.item.click(MenuChoice::StopDaemon);
    t.item
        .wait_for("the confirmation", |calls| {
            calls.contains(&Call::ConfirmStop(1))
        })
        .await;
    sleep(Duration::from_millis(300)).await;
    assert!(!token.is_cancelled(), "cancelled: the daemon runs on");
    let list: hutan_protocol::messages::SessionList = t
        .client
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    assert!(
        list.sessions
            .iter()
            .any(|s| s.id == session.id && s.state.is_live())
    );

    // The dialog stays up until answered: the daemon keeps serving, then stops on the answer.
    t.item.answer_with(None);
    t.item.click(MenuChoice::StopDaemon);
    t.item
        .wait_for("the second confirmation", |calls| {
            calls.iter().filter(|c| **c == Call::ConfirmStop(1)).count() == 2
        })
        .await;
    assert!(!token.is_cancelled(), "waiting for the answer");
    t.item.answer_held(StopAnswer::StopSessions);
    timeout(WAIT, token.cancelled())
        .await
        .expect("the daemon did not stop");
    timeout(WAIT, t.daemon.daemon.wait())
        .await
        .expect("the daemon did not finish")
        .unwrap();
    assert_eq!(t.item.calls().last(), Some(&Call::Hide));
}
