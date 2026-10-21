//! The workbench side of the macOS integration (roadmap M6.4, D-2, `crate::macos`): the daemon
//! outliving the app, starting it at login as a login item, the menu bar status item, the Dock
//! badge, the global Assistant shortcut and living on in the menu bar after the last window
//! closes.
//!
//! Each is driven by settings (`app.keep_running`, `app.start_at_login`, `app.menu_bar`,
//! `app.dock_badge`, `assistant.global_shortcut`) and follows the attention model. The
//! `sync_*` methods compare the wanted state with what was last handed to the system and only
//! call the trait when something changed, so they are cheap to run on every session update and
//! on every settings reload. Nothing here knows whether the traits are real.

use std::rc::Rc;

use gpui::{App, Context, Task, Window};
use hutan_client::ConnectionState;

use super::view::Workbench;
use crate::macos::{
    DockBadge, GlobalHotkey, LoginItem, LoginItemStatus, MacEvent, MacServices, MenuCommand,
    NotificationPermission, Presence, Shortcut, StatusItemHost, StatusItemModel,
};
use crate::settings::{AppSettings, AssistantSettings, LastWindowClose};
use hutan_settings::ChangeSet;

/// State of the macOS integration inside the workbench.
pub(crate) struct MacControl {
    login_item: Rc<dyn LoginItem>,
    notifications: Rc<dyn NotificationPermission>,
    status_item: Rc<dyn StatusItemHost>,
    dock: Rc<dyn DockBadge>,
    hotkey: Rc<dyn GlobalHotkey>,
    presence: Rc<dyn Presence>,
    /// The window is hidden and the app is an accessory: it lives in the menu bar.
    in_menu_bar: bool,
    /// The status of the login item the last time it was read.
    login_status: LoginItemStatus,
    /// The `(start_at_login, status)` the system refused to reconcile: the same pair is not
    /// tried again, so a write-back (ours, or another window's on the same file) cannot start a
    /// register / refuse / write-back loop. An explicit toggle forgets it.
    refused: Option<(bool, LoginItemStatus)>,
    /// What the Dock badge shows now (`None` until it was set once).
    badge: Option<usize>,
    /// What the menu bar item shows now (`None`: not on the menu bar).
    item: Option<StatusItemModel>,
    /// The shortcut setting the system was last asked about, whatever the outcome: a refused
    /// shortcut is reported once, not on every settings reload.
    shortcut_tried: Option<String>,
    shortcut_active: bool,
    pub(crate) launchd_daemon: bool,
    pub(crate) launchd_grace: std::time::Duration,
    pub(crate) grace_task: Option<Task<()>>,
    pub(crate) events_task: Option<Task<()>>,
}

impl MacControl {
    /// Takes the services apart; the receiver of their events is returned for the workbench
    /// to listen on.
    pub(crate) fn new(services: MacServices) -> (Self, Option<crate::macos::EventReceiver>) {
        let MacServices {
            login_item,
            notifications,
            status_item,
            dock,
            hotkey,
            presence,
            events,
            launchd_daemon,
            launchd_grace,
        } = services;
        let login_status = login_item.status();
        (
            Self {
                login_item,
                notifications,
                status_item,
                dock,
                hotkey,
                presence,
                in_menu_bar: false,
                login_status,
                refused: None,
                badge: None,
                item: None,
                shortcut_tried: None,
                shortcut_active: false,
                launchd_daemon,
                launchd_grace,
                grace_task: None,
                events_task: None,
            },
            events,
        )
    }

    pub(crate) fn login_status(&self) -> LoginItemStatus {
        self.login_status
    }

    pub(crate) fn start_at_login_available(&self) -> bool {
        self.login_item.available()
    }

    pub(crate) fn notifications(&self) -> Rc<dyn NotificationPermission> {
        Rc::clone(&self.notifications)
    }
}

/// Where the connection stands, in the words the menu bar item uses.
fn connection_words(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connecting => "connecting to hutand…",
        ConnectionState::Connected => "hutand connected",
        ConnectionState::Reconnecting { .. } => "hutand reconnecting…",
        ConnectionState::Stopped => "hutand stopped",
    }
}

impl Workbench {
    // ---- the daemon outliving the app, and starting it at login ----------------------------

    /// `app.keep_running` as set: quitting leaves the daemon running.
    pub fn keep_running_enabled(&self) -> bool {
        self.settings.get::<AppSettings>().keep_running
    }

    /// `app.start_at_login` as set (the toggle's position, whatever the system says).
    pub fn start_at_login_enabled(&self) -> bool {
        self.settings.get::<AppSettings>().start_at_login
    }

    /// Whether the start-at-login toggle can be used: the desktop runs from Hutan.app.
    pub fn start_at_login_available(&self) -> bool {
        self.mac.start_at_login_available()
    }

    /// What the system said about the launch agent when it was last asked.
    pub fn start_at_login_status(&self) -> LoginItemStatus {
        self.mac.login_status()
    }

    /// The launch agent is registered *and* allowed: launchd owns the daemon and quitting never
    /// stops it. Reads the system fresh, because the user may have changed it in System
    /// Settings since the window last looked.
    pub fn start_at_login_active(&self) -> bool {
        self.start_at_login_available()
            && self.start_at_login_enabled()
            && self.mac.login_item.status() == LoginItemStatus::Enabled
    }

    /// Quitting leaves the daemon alone and does not ask: it was asked to outlive the app
    /// (`app.keep_running`), or launchd owns it (start at login, active).
    pub fn daemon_outlives_app(&self) -> bool {
        self.keep_running_enabled() || self.start_at_login_active()
    }

    /// Settings → General, the `Daemon` menu and the menu bar item: flips keep running.
    pub fn toggle_keep_running(&mut self, cx: &mut Context<Self>) {
        self.set_keep_running(!self.keep_running_enabled(), cx);
    }

    /// Writes `app.keep_running`. Nothing is registered with the system: the daemon is just
    /// left running when Hutan quits.
    pub fn set_keep_running(&mut self, on: bool, cx: &mut Context<Self>) {
        // `keep_running = true` on its own is what a settings file from before
        // `app.start_at_login` looks like (and is migrated as); naming start_at_login next to
        // it says this file knows both.
        let written = if on {
            self.settings
                .write_user(
                    "app.start_at_login",
                    toml::Value::Boolean(self.start_at_login_enabled()),
                )
                .and_then(|()| {
                    self.settings
                        .write_user("app.keep_running", toml::Value::Boolean(true))
                })
        } else {
            self.settings
                .write_user("app.keep_running", toml::Value::Boolean(false))
        };
        if let Err(error) = written {
            tracing::warn!("cannot save keep running: {error}");
            self.set_notice(format!("Cannot save keep running: {error}"), cx);
            return;
        }
        // The settings change would do it too; doing it here keeps the answer immediate.
        self.sync_menus(cx);
        self.sync_status_item(cx);
        cx.notify();
    }

    /// Settings → General, the `Daemon` menu and the menu bar item: flips starting at login.
    pub fn toggle_start_at_login(&mut self, cx: &mut Context<Self>) {
        self.set_start_at_login(!self.start_at_login_enabled(), cx);
    }

    /// Writes `app.start_at_login`, which registers or unregisters the launch agent.
    pub fn set_start_at_login(&mut self, on: bool, cx: &mut Context<Self>) {
        if !self.start_at_login_available() {
            self.set_notice("Starting at login is available in Hutan.app", cx);
            return;
        }
        if let Err(error) = self
            .settings
            .write_user("app.start_at_login", toml::Value::Boolean(on))
        {
            tracing::warn!("cannot save start at login: {error}");
            self.set_notice(format!("Cannot save start at login: {error}"), cx);
            return;
        }
        // The user asked: whatever was refused before may be tried again.
        self.mac.refused = None;
        // The settings change would do it too; doing it here keeps the answer immediate.
        self.sync_start_at_login(cx);
    }

    /// "Open Login Items": System Settings → General → Login Items.
    pub fn open_login_items(&mut self, _cx: &mut Context<Self>) {
        self.mac.login_item.open_login_items_settings();
    }

    /// Makes the launch agent match `app.start_at_login`: registers it when the setting is on
    /// and nothing is registered, unregisters it when the setting is off and something is.
    /// A failure is shown and the setting goes back to what is true.
    pub(crate) fn sync_start_at_login(&mut self, cx: &mut Context<Self>) {
        if self.start_at_login_available() {
            let wanted = self.start_at_login_enabled();
            let status = self.mac.login_item.status();
            let differs = wanted != status.registered();
            if differs && self.mac.refused != Some((wanted, status)) {
                let refused = if wanted {
                    self.register_launch_agent(cx)
                } else {
                    self.unregister_launch_agent(cx)
                };
                self.mac.refused = refused.then_some((wanted, status));
            }
        }
        self.refresh_login_status(cx);
    }

    /// Registers the launch agent; `true` when that failed (the setting was written back).
    fn register_launch_agent(&mut self, cx: &mut Context<Self>) -> bool {
        let failure = match self.mac.login_item.register() {
            Err(error) => Some(format!("Cannot start Hutan's daemon at login: {error}")),
            Ok(()) => match self.mac.login_item.status() {
                LoginItemStatus::NotFound => Some(
                    "Cannot start Hutan's daemon at login: the launch agent is missing from \
                     Hutan.app"
                        .to_owned(),
                ),
                LoginItemStatus::RequiresApproval => {
                    self.set_notice(
                        "Allow Hutan in System Settings → General → Login Items to start its \
                         daemon at login",
                        cx,
                    );
                    None
                }
                LoginItemStatus::Enabled | LoginItemStatus::NotRegistered => None,
            },
        };
        let Some(message) = failure else {
            return false;
        };
        tracing::warn!("{message}");
        self.write_start_at_login_back(false, cx);
        self.set_notice(message, cx);
        true
    }

    /// Unregisters the launch agent; `true` when that failed (the setting was written back).
    fn unregister_launch_agent(&mut self, cx: &mut Context<Self>) -> bool {
        let Err(error) = self.mac.login_item.unregister() else {
            return false;
        };
        let message = format!("Cannot stop starting Hutan's daemon at login: {error}");
        tracing::warn!("{message}");
        self.write_start_at_login_back(true, cx);
        self.set_notice(message, cx);
        true
    }

    /// Puts `app.start_at_login` back to what the system says after a refused change.
    fn write_start_at_login_back(&mut self, value: bool, cx: &mut Context<Self>) {
        if let Err(error) = self
            .settings
            .write_user("app.start_at_login", toml::Value::Boolean(value))
        {
            tracing::warn!("cannot restore start at login: {error}");
            self.set_notice(format!("Cannot save start at login: {error}"), cx);
        }
    }

    /// Reads the login item's status; when it changed (the user answered in System Settings,
    /// or a registration just happened) the menus and the status item follow.
    pub fn refresh_login_status(&mut self, cx: &mut Context<Self>) {
        let status = self.mac.login_item.status();
        if status != self.mac.login_status {
            self.mac.login_status = status;
            self.sync_menus(cx);
            self.sync_status_item(cx);
            cx.notify();
        }
    }

    /// The window waited `launchd_grace` for a launchd-started daemon: when none answered, the
    /// connection may start `hutand` itself (it was stopped on purpose, or the login item has
    /// not run). While the user has stopped the daemon from the app, it stays down.
    pub(crate) fn launchd_grace_over(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_connected(cx) && !self.daemon.stopped {
            tracing::info!("no launchd-started daemon answered; starting hutand");
            self.connection(cx).set_autostart(true);
        }
    }

    // ---- menu bar item and Dock badge ------------------------------------------------------

    fn status_item_model(&self, cx: &App) -> StatusItemModel {
        let daemon = if let Some(busy) = self.daemon.busy {
            busy.label().to_owned()
        } else if self.daemon.stopped {
            connection_words(ConnectionState::Stopped).to_owned()
        } else {
            connection_words(self.sessions.read(cx).connection_state()).to_owned()
        };
        StatusItemModel {
            daemon,
            needs_you: self.sessions.read(cx).attention_entries(),
            keep_running: self.keep_running_enabled(),
            start_at_login: self.start_at_login_enabled(),
            start_at_login_available: self.start_at_login_available(),
            start_at_login_status: self.mac.login_status,
        }
    }

    /// Puts the menu bar item on the menu bar, updates it, or takes it off, per
    /// `app.menu_bar` and the attention model.
    pub(crate) fn sync_status_item(&mut self, cx: &mut Context<Self>) {
        if self.settings.get::<AppSettings>().menu_bar {
            let model = self.status_item_model(cx);
            if self.mac.item.as_ref() != Some(&model) {
                self.mac.status_item.show(&model);
                self.mac.item = Some(model);
            }
        } else if self.mac.item.take().is_some() {
            self.mac.status_item.hide();
        }
    }

    /// Sets the Dock badge to the number of sessions that need the user (`app.dock_badge`).
    pub(crate) fn sync_dock(&mut self, cx: &mut Context<Self>) {
        let count = if self.settings.get::<AppSettings>().dock_badge {
            self.sessions.read(cx).attention_entries().len()
        } else {
            0
        };
        if self.mac.badge != Some(count) {
            self.mac.dock.set_count(count);
            self.mac.badge = Some(count);
        }
    }

    // ---- global shortcut -------------------------------------------------------------------

    /// Registers `assistant.global_shortcut` when it changed since the system was last asked.
    /// A shortcut that cannot be registered (taken by another app, or not a shortcut) is
    /// reported once in the window; the setting is left alone.
    pub(crate) fn sync_hotkey(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.get::<AssistantSettings>();
        let wanted = if settings.enabled {
            settings.global_shortcut.trim().to_owned()
        } else {
            String::new()
        };
        if self.mac.shortcut_tried.as_deref() == Some(wanted.as_str()) {
            return;
        }
        self.mac.shortcut_tried = Some(wanted.clone());
        if self.mac.shortcut_active {
            self.mac.hotkey.unregister();
            self.mac.shortcut_active = false;
        }
        if wanted.is_empty() {
            return;
        }
        let notice = match Shortcut::parse(&wanted) {
            Err(error) => Some(format!(
                "The global shortcut \"{wanted}\" is not valid: {error} \
                 (assistant.global_shortcut)"
            )),
            Ok(shortcut) => match self.mac.hotkey.register(&shortcut) {
                Ok(()) => {
                    self.mac.shortcut_active = true;
                    None
                }
                Err(error) => Some(format!(
                    "The global shortcut {} is not available: {error}. The setting \
                     assistant.global_shortcut is unchanged; the shortcut still works inside Hutan.",
                    shortcut.display
                )),
            },
        };
        if let Some(text) = notice {
            tracing::warn!("{text}");
            self.set_notice(text, cx);
        }
    }

    // ---- wiring ----------------------------------------------------------------------------

    /// Brings every surface up to date (the window just opened).
    pub(crate) fn sync_mac_all(&mut self, cx: &mut Context<Self>) {
        self.sync_start_at_login(cx);
        self.sync_status_item(cx);
        self.sync_dock(cx);
        self.sync_hotkey(cx);
        self.sync_last_window_close_note();
    }

    /// The sessions, approvals or connection changed.
    pub(crate) fn sync_mac_surfaces(&mut self, cx: &mut Context<Self>) {
        self.sync_status_item(cx);
        self.sync_dock(cx);
    }

    /// Says in the settings diagnostics when `app.on_last_window_close` cannot take effect.
    pub(crate) fn sync_last_window_close_note(&mut self) {
        self.settings
            .set_notes("app", self.settings.get::<AppSettings>().diagnostics());
    }

    /// A settings reload: the namespaces this module reads.
    pub(crate) fn on_mac_settings_changed(
        &mut self,
        changes: &ChangeSet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if changes.contains("app") {
            self.sync_start_at_login(cx);
            self.sync_status_item(cx);
            self.sync_dock(cx);
            self.sync_last_window_close_note();
            // Hidden in the menu bar with nothing left to bring it back from (the item was
            // turned off, or closing no longer keeps the app there): show the window.
            if self.mac.in_menu_bar && self.last_window_close() != LastWindowClose::MenuBar {
                self.show_if_hidden(window, cx);
            }
        }
        if changes.contains("assistant") {
            self.sync_hotkey(cx);
        }
    }

    // ---- the menu bar as a home -------------------------------------------------------------

    /// The app is hidden in the menu bar (the last window was closed with
    /// `app.on_last_window_close = "menu_bar"`).
    pub fn in_menu_bar(&self) -> bool {
        self.mac.in_menu_bar
    }

    /// Closing the last window with `menu_bar`: no Dock icon, no windows on screen, the
    /// window and everything behind it kept alive for the menu bar item to serve.
    pub(crate) fn hide_to_menu_bar(&mut self, cx: &mut Context<Self>) {
        if self.mac.in_menu_bar {
            return;
        }
        self.mac.in_menu_bar = true;
        self.mac.presence.set_accessory(true);
        self.mac.presence.hide_app();
        cx.notify();
    }

    /// Brings Hutan back from the menu bar when it is there: a Dock icon again, the window
    /// shown. Does nothing otherwise.
    pub(crate) fn show_if_hidden(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.mac.in_menu_bar) {
            return;
        }
        self.mac.presence.set_accessory(false);
        self.mac.presence.show_app();
        cx.activate(true);
        window.activate_window();
        cx.notify();
    }

    /// Brings the app and its window to the front, from wherever it is: hidden in the menu
    /// bar, behind other apps, or minimised.
    pub(crate) fn bring_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_if_hidden(window, cx);
        cx.activate(true);
        window.activate_window();
    }

    /// The system asked for something: a menu bar choice or the global shortcut.
    pub(crate) fn on_mac_event(
        &mut self,
        event: MacEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.daemon.quitting {
            // Everything else is moot while the app is going away; a Quit is asked again (see
            // `request_quit`).
            if matches!(event, MacEvent::Menu(MenuCommand::Quit)) {
                self.request_quit(window, cx);
            }
            return;
        }
        match event {
            MacEvent::Hotkey => {
                self.bring_forward(window, cx);
                self.open_assistant(window, cx);
            }
            MacEvent::Menu(MenuCommand::OpenHutan) => self.bring_forward(window, cx),
            MacEvent::Menu(MenuCommand::NewTerminal) => {
                self.bring_forward(window, cx);
                self.new_terminal(window, cx);
            }
            MacEvent::Menu(MenuCommand::FocusSession(session)) => {
                self.bring_forward(window, cx);
                if self.sessions.read(cx).session(session).is_none() {
                    self.set_notice("That session no longer exists", cx);
                } else if !self.show_assistant_session(session, window, cx) {
                    self.open_session(session, window, cx);
                }
            }
            MacEvent::Menu(MenuCommand::ToggleKeepRunning) => self.toggle_keep_running(cx),
            MacEvent::Menu(MenuCommand::ToggleStartAtLogin) => self.toggle_start_at_login(cx),
            // Hidden in the menu bar, the quit question needs the window (`open_quit_dialog`).
            MacEvent::Menu(MenuCommand::Quit) => self.request_quit(window, cx),
        }
    }
}
