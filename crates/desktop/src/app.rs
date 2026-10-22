//! Application bootstrap: keymap, menus, window options, quit routing, and the `run` entry point.

use crate::cli_link::CliLink;
use crate::harness_install::SystemRunner;
use crate::instance;
use crate::integrations::CliInstaller;
use crate::macos::status_item::{KEEP_RUNNING, start_at_login_title, start_at_login_usable};
use crate::macos::{LoginItem, LoginItemStatus, MacServices};
use crate::settings::{AppSettings, WorkspaceSort};
use crate::workbench::actions::{self, ApplyPreset, daemon};
use crate::workbench::model::LayoutPreset;
use crate::workbench::persistence;
use crate::workbench::{StartFailureKind, Workbench, WorkbenchOptions};
use gpui::{
    App, AppContext as _, Bounds, Context, Menu, MenuItem, SystemMenuType, TitlebarOptions, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, point, px, size,
};
use hutan_client::{ConnectOptions, Connection, DaemonIdentity};
use hutan_protocol::messages::{ClientInfo, ClientKind};
use hutan_settings::{SettingsPaths, SettingsStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the `Daemon` menu shows: whether the daemon is up (Restart and Stop apply) or not
/// (Start applies), whether an operation is under way (none applies), the two checkboxes, and
/// what a recovery item would recover from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonMenu {
    pub running: bool,
    /// The app is stopping, starting or restarting the daemon.
    pub busy: bool,
    /// `app.keep_running`.
    pub keep_running: bool,
    /// `app.start_at_login`, whether it can be used (the app runs from Hutan.app) and what the
    /// system says about the launch agent.
    pub start_at_login: bool,
    pub start_at_login_available: bool,
    pub start_at_login_status: LoginItemStatus,
    /// Why the daemon could not start, while it could not.
    pub start_failure: Option<StartFailureKind>,
    /// The daemon that answers is newer than this app (the line under the title bar offers
    /// *Copy Update Command*).
    pub newer_daemon: bool,
    /// The daemon that answers is another build or from elsewhere (the line offers *Use This
    /// App's Service*).
    pub other_daemon: bool,
}

impl Default for DaemonMenu {
    fn default() -> Self {
        Self {
            running: true,
            busy: false,
            keep_running: false,
            start_at_login: false,
            start_at_login_available: false,
            start_at_login_status: LoginItemStatus::NotRegistered,
            start_failure: None,
            newer_daemon: false,
            other_daemon: false,
        }
    }
}

impl DaemonMenu {
    /// Whether `action` applies now. A recovery action applies only while what it recovers
    /// from is current (and nothing is under way): the menu and the palette list it only
    /// then. Every other action always applies.
    pub fn offers(&self, action: &dyn gpui::Action) -> bool {
        let action = action.as_any();
        let failure = self.start_failure.filter(|_| !self.busy);
        if action.is::<daemon::StartWithEmptyData>() {
            failure == Some(StartFailureKind::NewerData)
        } else if action.is::<daemon::CopyUpdateCommand>() {
            failure == Some(StartFailureKind::NewerData) || self.newer_daemon
        } else if action.is::<daemon::ShowStuckDaemon>() || action.is::<daemon::StopStuckDaemon>() {
            failure == Some(StartFailureKind::Stuck)
        } else if action.is::<daemon::CopyStartError>() {
            failure.is_some()
        } else if action.is::<daemon::UseOwnDaemon>() {
            self.other_daemon && !self.busy
        } else {
            true
        }
    }
}

/// Runs `run` on the workbench window; quits the app when there is none. Deferred because
/// the caller may be inside that window's own update.
fn with_workbench(
    cx: &mut App,
    run: impl FnOnce(&mut Workbench, &mut Window, &mut Context<Workbench>) + 'static,
) {
    let handle = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<Workbench>());
    match handle {
        Some(handle) => cx.defer(move |cx| {
            if handle.update(cx, run).is_err() {
                cx.quit();
            }
        }),
        None => cx.quit(),
    }
}

/// Install the global actions and the menu bar. Key bindings come from the workbench (defaults
/// plus `keymap.toml`), which rebinds them live. Quit is decided by the workbench (live
/// sessions, `app.keep_running`, start at login); this handler only catches the action when no
/// window has focus.
pub fn init(cx: &mut App, presets: &[LayoutPreset]) {
    crate::workbench::ui::init_prose_font(cx);
    cx.on_action(|_: &actions::Quit, cx| {
        with_workbench(cx, |workbench, window, cx| {
            workbench.request_quit(window, cx)
        });
    });
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    cx.set_menus(menus(
        presets,
        LayoutPreset::DEFAULT,
        &DaemonMenu::default(),
    ));
}

/// App, Session, Workspace, View, Daemon, and Window menus. Items dispatch the same named actions as the
/// keymap and palette. View → Layout Preset checks `current`, the preset the window shows (the
/// title bar no longer has a preset switcher, CL-CHROME).
pub fn menus(presets: &[LayoutPreset], current: &str, daemon_menu: &DaemonMenu) -> Vec<Menu> {
    let preset_items = presets.iter().map(|p| {
        MenuItem::action(
            crate::workbench::preset_label(&p.name),
            ApplyPreset(p.name.clone()),
        )
        .checked(p.name == current)
    });
    vec![
        Menu::new("Hutan").items([
            MenuItem::action("About Hutan", actions::About),
            MenuItem::separator(),
            MenuItem::action("Settings…", actions::OpenSettings),
            MenuItem::action("Run Setup Again…", actions::RunSetup),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Hutan", actions::Quit),
        ]),
        Menu::new("Session").items([
            MenuItem::action("New Terminal", actions::NewTerminal),
            MenuItem::action("New Agent", actions::NewAgent),
            MenuItem::action("New Chat", actions::NewChat),
            MenuItem::separator(),
            MenuItem::action("Rename…", actions::RenameSession),
            MenuItem::action("Restart", actions::RestartSession),
            MenuItem::action("Stop…", actions::StopSession),
            MenuItem::action("Copy Session ID", actions::CopySessionId),
            MenuItem::separator(),
            MenuItem::action("Jump to Next Attention", actions::JumpToNextAttention),
        ]),
        Menu::new("Workspace").items([
            MenuItem::action("Open Project…", actions::OpenProject),
            MenuItem::action("Open Folder…", actions::OpenFolder),
            MenuItem::action("Jump to Workspace or Session…", actions::OpenJump),
            MenuItem::action("Go to File…", actions::QuickOpen),
            MenuItem::action("Open File…", actions::OpenFileDialog),
            MenuItem::action("Go to Home", actions::GoHome),
            MenuItem::separator(),
            MenuItem::action("Pin / Unpin", actions::PinActiveWorkspace),
            MenuItem::action("Rename…", actions::RenameActiveWorkspace),
            MenuItem::action("Add Folder…", actions::AddFolderToWorkspace),
            MenuItem::action("Reveal in Finder", actions::RevealWorkspace),
            MenuItem::action("Close Workspace", actions::CloseActiveWorkspace),
            MenuItem::separator(),
            MenuItem::submenu(
                Menu::new("Sort").items(
                    WorkspaceSort::ALL.map(|mode| {
                        MenuItem::action(mode.label(), actions::SetWorkspaceSort(mode))
                    }),
                ),
            ),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Left Dock", actions::ToggleLeftDock),
            MenuItem::action("Toggle Right Dock", actions::ToggleRightDock),
            MenuItem::action("Toggle Bottom Dock", actions::ToggleBottomDock),
            MenuItem::action("Files", actions::ShowFiles),
            MenuItem::action("Search", actions::ShowSearch),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Layout Preset").items(preset_items)),
            MenuItem::action("Save Layout as Preset…", actions::SaveLayoutAsPreset),
            MenuItem::action("Use New Default Layout", actions::UseNewDefaultLayout),
            MenuItem::action("Overview", actions::ToggleOverview),
            MenuItem::action("Toggle Zoom Pane", actions::ToggleZoom),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Density").items([
                MenuItem::action("Compact", actions::SetDensityCompact),
                MenuItem::action("Comfortable", actions::SetDensityComfortable),
            ])),
            MenuItem::action("Reset Theme and Icons to Default", actions::ResetTheme),
            MenuItem::separator(),
            MenuItem::action("Command Palette…", actions::TogglePalette),
        ]),
        Menu::new("Daemon").items(daemon_items(daemon_menu)),
        Menu::new("Window").items([
            MenuItem::action("Minimize", actions::MinimizeWindow),
            MenuItem::action("Zoom", actions::ZoomWindow),
        ]),
    ]
}

/// The `Daemon` menu. While the daemon could not start, its reason comes first (disabled) and
/// the recovery items that fit it follow *Open Logs*; they are not there otherwise
/// ([`DaemonMenu::offers`]). Every other item is always there, enabled exactly when it can act.
fn daemon_items(daemon_menu: &DaemonMenu) -> Vec<MenuItem> {
    let idle = !daemon_menu.busy;
    let running = daemon_menu.running;
    let mut items = vec![MenuItem::action("Status…", daemon::Status)];
    if let Some(kind) = daemon_menu.start_failure {
        items.push(daemon_item(kind.menu_text(), daemon::Status, false));
    }
    items.extend([
        MenuItem::separator(),
        daemon_item("Restart Daemon", daemon::Restart, running && idle),
        daemon_item("Stop Daemon", daemon::Stop, running && idle),
        daemon_item("Start Daemon", daemon::Start, !running && idle),
        MenuItem::action("Open Logs", daemon::OpenLogs),
    ]);
    let recovery: Vec<MenuItem> = [
        recovery_item(
            daemon_menu,
            "Copy Update Command",
            daemon::CopyUpdateCommand,
        ),
        recovery_item(
            daemon_menu,
            "Start with Empty Data…",
            daemon::StartWithEmptyData,
        ),
        recovery_item(
            daemon_menu,
            "Stop the Stuck Daemon…",
            daemon::StopStuckDaemon,
        ),
        recovery_item(
            daemon_menu,
            "Copy Start Error Details",
            daemon::CopyStartError,
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !recovery.is_empty() {
        items.push(MenuItem::separator());
        items.extend(recovery);
    }
    items.extend([
        MenuItem::separator(),
        MenuItem::action(KEEP_RUNNING, daemon::ToggleKeepRunning).checked(daemon_menu.keep_running),
        daemon_item(
            start_at_login_title(
                daemon_menu.start_at_login_available,
                daemon_menu.start_at_login_status,
            ),
            daemon::ToggleStartAtLogin,
            start_at_login_usable(
                daemon_menu.start_at_login_available,
                daemon_menu.start_at_login_status,
            ),
        )
        .checked(daemon_menu.start_at_login),
        daemon_item(
            "Open Login Items",
            daemon::OpenLoginItems,
            daemon_menu.start_at_login_status == LoginItemStatus::RequiresApproval,
        ),
    ]);
    items
}

/// A `Daemon` menu item that acts only when `can_act`. AppKit enables a menu item by asking
/// GPUI whether its action is available (`App::is_action_available`), not by the item's
/// `disabled` flag, and the daemon actions are always available (handled app-wide,
/// `workbench::daemon::register_app_actions`). So an item that cannot act carries
/// [`daemon::Unavailable`], which nothing handles, as well as the flag.
fn daemon_item(
    name: impl Into<gpui::SharedString>,
    action: impl gpui::Action,
    can_act: bool,
) -> MenuItem {
    if can_act {
        MenuItem::action(name, action)
    } else {
        MenuItem::action(name, daemon::Unavailable).disabled(true)
    }
}

/// A recovery item of the `Daemon` menu, while it applies.
fn recovery_item(
    daemon_menu: &DaemonMenu,
    name: &'static str,
    action: impl gpui::Action,
) -> Option<MenuItem> {
    daemon_menu
        .offers(&action)
        .then(|| MenuItem::action(name, action))
}

/// Window options for the workbench: transparent titlebar with inset traffic lights and a
/// blurred background (the left dock paints translucent; everything else paints opaque).
pub fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Hutan".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(13.0), px(11.0))),
        }),
        window_background: WindowBackgroundAppearance::Blurred,
        window_min_size: Some(size(px(640.0), px(400.0))),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// Open a workbench window and focus the app.
pub fn open_workbench(
    cx: &mut App,
    options: WorkbenchOptions,
) -> gpui::Result<WindowHandle<Workbench>> {
    let window_options = window_options(cx);
    let handle = cx.open_window(window_options, move |window, cx| {
        cx.new(|cx| {
            let workbench = Workbench::new(options, window, cx);
            window.on_next_frame(|_, _| hutan_terminal_view::trace::startup("first_frame"));
            crate::bench::start(window, cx);
            workbench
        })
    })?;
    hutan_terminal_view::trace::startup("window_opened");
    cx.activate(true);
    Ok(handle)
}

/// `hutand` next to the running binary (`target/debug`, an app bundle), else the name for a
/// `PATH` lookup.
pub fn hutand_path() -> PathBuf {
    binary_beside(std::env::current_exe().ok().as_deref(), "hutand")
}

/// The `hutan` CLI next to the running binary, else the name for a `PATH` lookup: what
/// `hutan integration install` is run with.
pub fn hutan_path() -> PathBuf {
    binary_beside(std::env::current_exe().ok().as_deref(), "hutan")
}

fn binary_beside(exe: Option<&Path>, name: &str) -> PathBuf {
    exe.and_then(Path::parent)
        .map(|dir| dir.join(name))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Connection options of the desktop: the standard socket (`HUTAN_SOCKET` / `HUTAN_HOME`
/// respected) with `hutand` autostarted when nothing listens.
pub fn connect_options() -> ConnectOptions {
    ConnectOptions {
        socket: None,
        autostart: Some(hutand_path()),
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            origin: None,
        },
    }
}

/// The daemon this app starts: `hutand` beside it ([`hutand_path`]) of this build. A daemon of
/// another build, or one from elsewhere, is told apart from it on every connect.
pub fn own_daemon() -> DaemonIdentity {
    DaemonIdentity::own(Some(&hutand_path()))
}

/// Starts the connection. With starting at login on and approved, launchd starts the daemon, so
/// the connection only dials ([`Connection::start_held`]) and the workbench lets it start
/// `hutand` itself if none has answered after a grace period; otherwise it autostarts as
/// always. The second value says which of the two it was.
pub fn start_connection(
    options: ConnectOptions,
    settings: &SettingsStore,
    login_item: &dyn LoginItem,
) -> (Connection, bool) {
    crate::settings::register_all(settings);
    let launchd = settings.get::<AppSettings>().start_at_login
        && login_item.available()
        && login_item.status() == LoginItemStatus::Enabled;
    if launchd {
        (Connection::start_held(options), true)
    } else {
        (Connection::start(options), false)
    }
}

/// The settings store over `settings.toml` / `keymap.toml` in the Hutan home. A `settings.toml`
/// from before `app.start_at_login` is migrated here, before anything reads it.
pub fn settings_store() -> SettingsStore {
    let paths = SettingsPaths::from_env().unwrap_or_else(|err| {
        tracing::warn!("{err}; settings are kept in the temp directory");
        SettingsPaths::in_dir(std::env::temp_dir().join("hutan"))
    });
    let user_file = paths.user.clone();
    let store = SettingsStore::new(paths);
    crate::settings::register_all(&store);
    crate::settings::migrate_start_at_login(&store, &user_file);
    store
}

/// Wakes the app from the listener thread of [`instance`]: a second desktop asked for it.
struct WakeApp(tokio::sync::mpsc::Sender<()>);

impl instance::ActivationSink for WakeApp {
    fn activate(&self) {
        // A full queue already holds a request that will bring the window forward.
        let _ = self.0.try_send(());
    }
}

/// What starting up found out about other desktops.
enum Startup {
    /// This is the desktop: the claim (when single-instance could be set up) lives as long as the
    /// app.
    Run(Option<instance::Primary>),
    /// Another desktop owns this data folder; it was asked to come forward. The exit code.
    Exit(i32),
}

/// Takes the single-instance lock of the data folder before anything else opens, or asks the
/// desktop that holds it to come forward (design.md § Single instance).
fn claim_instance(wake: tokio::sync::mpsc::Sender<()>) -> Startup {
    let dir = match instance::data_dir() {
        Ok(dir) => dir,
        Err(error) => {
            tracing::warn!("one desktop per data folder is not enforced: {error}");
            return Startup::Run(None);
        }
    };
    match instance::claim(&dir, WakeApp(wake)) {
        Ok(instance::Claim::Primary(primary)) => Startup::Run(Some(primary)),
        Ok(instance::Claim::Running) => {
            match instance::notify_running(&dir, instance::CONNECT_PATIENCE) {
                Ok(()) => {
                    tracing::info!("Hutan is already running; it was brought forward");
                    Startup::Exit(0)
                }
                Err(error) => {
                    eprintln!(
                        "hutan-desktop: Hutan is already running but did not answer: {error}"
                    );
                    Startup::Exit(1)
                }
            }
        }
        Err(error) => {
            tracing::warn!("one desktop per data folder is not enforced: {error}");
            Startup::Run(None)
        }
    }
}

/// Builds the settings, the macOS services and the connection, and opens the workbench window.
fn open_main(cx: &mut App) -> gpui::Result<WindowHandle<Workbench>> {
    let settings = settings_store();
    crate::theme::set_system_focus(crate::macos::focus::system_colors());
    let mut mac = MacServices::system();
    let (connection, launchd) =
        start_connection(connect_options(), &settings, mac.login_item.as_ref());
    mac.launchd_daemon = launchd;
    let options =
        WorkbenchOptions::builtin(settings, connection, persistence::default_state_path())
            .with_mac(mac)
            .with_first_run_setup()
            .with_cli_link(CliLink::system())
            .with_install(crate::install::Install::current())
            .with_installer(Arc::new(CliInstaller::new(hutan_path())))
            .with_package_runner(Arc::new(SystemRunner))
            .with_own_daemon(own_daemon());
    open_workbench(cx, options)
}

/// Brings the workbench window forward (from the menu bar, from behind other apps), or `reopen`s
/// it when there is none. `true` when there was a window.
pub fn activate_main_window(cx: &mut App, reopen: impl FnOnce(&mut App) + 'static) -> bool {
    let handle = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<Workbench>());
    match handle {
        Some(handle) => {
            cx.defer(move |cx| {
                if let Err(error) = handle.update(cx, |workbench, window, cx| {
                    workbench.bring_forward(window, cx);
                }) {
                    tracing::warn!("cannot bring the window forward: {error:#}");
                }
            });
            true
        }
        None => {
            cx.defer(reopen);
            false
        }
    }
}

/// A second desktop asks this one to come forward, over [`instance`].
fn watch_instance(cx: &mut App, mut requests: tokio::sync::mpsc::Receiver<()>) {
    cx.spawn(async move |cx| {
        while requests.recv().await.is_some() {
            cx.update(|cx| {
                activate_main_window(cx, |cx| {
                    if let Err(error) = open_main(cx) {
                        tracing::error!("cannot reopen the workbench window: {error:#}");
                    }
                });
            });
        }
    })
    .detach();
}

/// Binary entry point: `HUTAN_HOME=/tmp/x hutan-desktop`.
pub fn run() {
    hutan_terminal_view::trace::startup("main");
    if let Some(path) = crate::crash::default_log_path() {
        crate::crash::install(path);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let (wake, requests) = tokio::sync::mpsc::channel(4);
    let _instance = match claim_instance(wake) {
        Startup::Run(claim) => claim,
        Startup::Exit(0) => return,
        Startup::Exit(code) => std::process::exit(code),
    };
    let application = gpui_platform::application().with_assets(crate::icons::Assets);
    // The Dock icon was clicked while no window is on screen.
    application.on_reopen(|cx| {
        activate_main_window(cx, |cx| {
            if let Err(error) = open_main(cx) {
                tracing::error!("cannot reopen the workbench window: {error:#}");
            }
        });
    });
    application.run(move |cx: &mut App| {
        init(cx, &LayoutPreset::builtin());
        watch_signals(cx);
        watch_instance(cx, requests);
        if let Err(err) = open_main(cx) {
            tracing::error!("cannot open the workbench window: {err:#}");
            cx.quit();
        }
    });
}

/// SIGTERM / SIGINT / SIGHUP: layout saved, connection closed, exit code 0, the daemon
/// untouched; never a question.
fn watch_signals(cx: &mut App) {
    match crate::signals::watch() {
        Ok(mut signals) => cx
            .spawn(async move |cx| {
                while let Some(signal) = signals.recv().await {
                    tracing::info!(
                        ?signal,
                        "terminated by a signal; leaving the daemon running"
                    );
                    cx.update(|cx| {
                        with_workbench(cx, |workbench, _, cx| workbench.quit_leaving_daemon(cx));
                    });
                }
            })
            .detach(),
        Err(err) => tracing::warn!("cannot watch for termination signals: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hutan_menu_offers_run_setup_again() {
        let menus = menus(
            &LayoutPreset::builtin(),
            LayoutPreset::DEFAULT,
            &DaemonMenu::default(),
        );
        let hutan = menus.first().expect("the Hutan menu comes first");
        assert!(
            hutan.items.iter().any(|item| matches!(
                item,
                MenuItem::Action { name, .. } if name.as_ref() == "Run Setup Again…"
            )),
            "Hutan ▸ Run Setup Again…"
        );
    }

    #[test]
    fn hutand_is_found_beside_the_binary_else_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("hutan-desktop");
        assert_eq!(binary_beside(Some(&exe), "hutand"), PathBuf::from("hutand"));
        std::fs::write(dir.path().join("hutand"), "").unwrap();
        assert_eq!(
            binary_beside(Some(&exe), "hutand"),
            dir.path().join("hutand")
        );
        assert_eq!(binary_beside(None, "hutand"), PathBuf::from("hutand"));
    }

    #[test]
    fn the_desktop_of_an_app_bundle_starts_the_bundled_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let macos = dir.path().join("Hutan.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        for name in ["hutan-desktop", "hutand", "hutan"] {
            std::fs::write(macos.join(name), "").unwrap();
        }
        assert_eq!(
            binary_beside(Some(&macos.join("hutan-desktop")), "hutand"),
            macos.join("hutand")
        );
    }
}
