//! macOS integration of the desktop (roadmap M6.4, D-2): starting the daemon at login as a login
//! item, the notification permission, the menu bar status item, the Dock badge and the
//! system-wide Assistant shortcut.
//!
//! Every system API sits behind a small trait ([`LoginItem`], [`NotificationPermission`],
//! [`StatusItemHost`], [`DockBadge`], [`GlobalHotkey`], [`Presence`]) with a fake that records
//! what the workbench asked for.
//! The workbench ([`crate::workbench`], `mac.rs`) only talks to the traits, and tests only use
//! the fakes: they never call `SMAppService`, `UNUserNotificationCenter`, `NSStatusBar`, the Dock
//! tile or the hotkey API.
//! [`MacServices::system`] builds the real ones, and only `app::run` calls it.
//!
//! Things the system tells us (a menu item was chosen, the shortcut was pressed) arrive as
//! [`MacEvent`]s on one bounded channel that the workbench drains on the main thread.

pub mod dock;
pub mod focus;
pub mod hotkey;
pub mod login_item;
pub mod notifications;
pub mod presence;
pub mod status_item;

use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use hutan_core::SessionId;
use tokio::sync::mpsc;

pub use dock::{DockBadge, FakeDock};
pub use hotkey::{FakeHotkey, GlobalHotkey, HotkeyError, Shortcut};
pub use login_item::{FakeLoginItem, LoginItem, LoginItemCall, LoginItemStatus};
pub use notifications::{FakeNotifications, NotificationAnswer, NotificationPermission};
pub use presence::{FakePresence, Presence, PresenceCall};
pub use status_item::{FakeStatusItem, MenuSpec, StatusItemHost, StatusItemModel};

/// How long a desktop that expects a launchd-started daemon waits for it before it starts one
/// itself (the daemon may have been stopped on purpose, or the login item not run yet).
pub const LAUNCHD_GRACE: Duration = Duration::from_secs(4);
/// Events the system can have queued before the workbench drains them; more are dropped.
const EVENT_QUEUE: usize = 32;

/// A choice in the status item's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    /// Bring the window forward and show this session.
    FocusSession(SessionId),
    OpenHutan,
    NewTerminal,
    /// Flip `app.keep_running`.
    ToggleKeepRunning,
    /// Flip `app.start_at_login`.
    ToggleStartAtLogin,
    Quit,
}

/// Something the system asked of the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacEvent {
    Menu(MenuCommand),
    /// The global shortcut was pressed.
    Hotkey,
}

/// The sending half of the event channel, handed to the real hosts. `try_send`s from the main
/// thread's callbacks; never blocks.
pub type EventSender = mpsc::Sender<MacEvent>;
pub type EventReceiver = mpsc::Receiver<MacEvent>;

/// The event channel.
pub fn event_channel() -> (EventSender, EventReceiver) {
    mpsc::channel(EVENT_QUEUE)
}

/// Everything the workbench needs from the system, as trait objects.
pub struct MacServices {
    pub login_item: Rc<dyn LoginItem>,
    pub notifications: Rc<dyn NotificationPermission>,
    pub status_item: Rc<dyn StatusItemHost>,
    pub dock: Rc<dyn DockBadge>,
    pub hotkey: Rc<dyn GlobalHotkey>,
    pub presence: Rc<dyn Presence>,
    /// What the system asked for; the workbench takes it when it is built.
    pub events: Option<EventReceiver>,
    /// The daemon is expected to be started by launchd (start at login is on and approved): the
    /// connection was made with [`hutan_client::Connection::start_held`], and the workbench
    /// lets it start `hutand` itself after [`MacServices::launchd_grace`].
    pub launchd_daemon: bool,
    pub launchd_grace: Duration,
}

impl fmt::Debug for MacServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacServices")
            .field("launchd_daemon", &self.launchd_daemon)
            .finish_non_exhaustive()
    }
}

impl MacServices {
    /// No integration: starting at login and notifications are unavailable, the other three do
    /// nothing. What every workbench gets unless it is given more
    /// (`WorkbenchOptions::builtin`).
    pub fn inert() -> Self {
        Self {
            login_item: Rc::new(login_item::Unavailable),
            notifications: Rc::new(notifications::Unavailable),
            status_item: Rc::new(status_item::Nothing),
            dock: Rc::new(dock::Nothing),
            hotkey: Rc::new(hotkey::Nothing),
            presence: Rc::new(presence::Nothing),
            events: None,
            launchd_daemon: false,
            launchd_grace: LAUNCHD_GRACE,
        }
    }

    /// The real thing on macOS: the login item exists only inside `Hutan.app`; the status
    /// item, Dock tile and hotkey are created lazily, when the settings first ask for them.
    /// Elsewhere this is [`inert`](Self::inert). Must be called on the main thread.
    pub fn system() -> Self {
        #[cfg(target_os = "macos")]
        {
            let (sender, receiver) = event_channel();
            let login_item: Rc<dyn LoginItem> = match login_item::SystemLoginItem::new() {
                Some(item) => Rc::new(item),
                None => Rc::new(login_item::Unavailable),
            };
            let notifications: Rc<dyn NotificationPermission> =
                match notifications::SystemNotifications::new() {
                    Some(permission) => Rc::new(permission),
                    None => Rc::new(notifications::Unavailable),
                };
            Self {
                login_item,
                notifications,
                status_item: Rc::new(status_item::SystemStatusItem::new(sender.clone())),
                dock: Rc::new(dock::SystemDock),
                hotkey: Rc::new(hotkey::SystemHotkey::new(sender)),
                presence: Rc::new(presence::SystemPresence),
                events: Some(receiver),
                launchd_daemon: false,
                launchd_grace: LAUNCHD_GRACE,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::inert()
        }
    }
}
