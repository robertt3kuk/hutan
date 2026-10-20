//! Fakes for every macOS API the desktop reaches through `hutan_desktop::macos`, shared by the
//! GPUI tests: no test touches `SMAppService`, `NSStatusBar`, the Dock tile, the hotkey API or
//! the activation policy.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::macos::{
    FakeDock, FakeHotkey, FakeLoginItem, FakeNotifications, FakePresence, FakeStatusItem,
    MacServices,
};
use hutan_desktop::workbench::{Workbench, WorkbenchOptions};
use hutan_settings::{SettingsPaths, SettingsStore};

use super::{TestDaemon, open_options};

/// Every fake, kept by the test while the workbench holds clones.
pub struct Fakes {
    pub login: FakeLoginItem,
    pub notifications: FakeNotifications,
    pub item: FakeStatusItem,
    pub dock: FakeDock,
    pub hotkey: FakeHotkey,
    pub presence: FakePresence,
}

impl Fakes {
    pub fn new() -> Self {
        Self {
            login: FakeLoginItem::new(),
            notifications: FakeNotifications::new(),
            item: FakeStatusItem::new(),
            dock: FakeDock::new(),
            hotkey: FakeHotkey::new(),
            presence: FakePresence::new(),
        }
    }

    pub fn services(&self) -> MacServices {
        MacServices {
            login_item: Rc::new(self.login.clone()),
            notifications: Rc::new(self.notifications.clone()),
            status_item: Rc::new(self.item.clone()),
            dock: Rc::new(self.dock.clone()),
            hotkey: Rc::new(self.hotkey.clone()),
            presence: Rc::new(self.presence.clone()),
            ..MacServices::inert()
        }
    }
}

impl Default for Fakes {
    fn default() -> Self {
        Self::new()
    }
}

/// A workbench over `daemon` with these services.
pub fn open_with_services<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    mac: MacServices,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let options = WorkbenchOptions::builtin(settings, daemon.connection(), None).with_mac(mac);
    open_options(cx, options)
}

/// A workbench over `daemon` with the fakes.
pub fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    fakes: &Fakes,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with_services(cx, daemon, fakes.services())
}
