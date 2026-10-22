//! The menu bar status item (`app.menu_bar`): the orangutan glyph as a template image, the
//! number of sessions that need the user next to it, and a menu (daemon state, the sessions
//! that need attention, Open Hutan, New Terminal, Keep Daemon Running After Hutan Quits, Start Daemon at
//! Login, Quit).
//!
//! This is the item of a running desktop. While no desktop is connected to the daemon, `hutand`
//! shows an item of its own (`hutan_daemon::menu_bar`, M6-D) and takes it off the moment a
//! desktop connects: one Hutan icon at a time. The desktop keeps this one on the menu bar for as
//! long as it runs (`app.menu_bar` permitting), also while it reconnects, which the daemon's
//! show delay covers.
//!
//! [`StatusItemModel`] is pure data: what the item shows and which menu it offers, derived
//! from the attention model. [`StatusItemHost`] puts a model on the menu bar; the real host
//! ([`SystemStatusItem`]) renders it with `NSStatusBar`/`NSMenu`, the fake records it.

use std::cell::RefCell;
use std::rc::Rc;

use super::MenuCommand;
use super::login_item::LoginItemStatus;
use crate::sessions::attention::AttentionEntry;

/// Most sessions listed in the menu; the rest are summarised.
pub const MAX_LISTED: usize = 10;

/// One row of the status item's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuSpec {
    /// A line that is only text.
    Label(String),
    Separator,
    Item {
        title: String,
        command: MenuCommand,
        checked: bool,
        enabled: bool,
    },
}

/// What the status item shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItemModel {
    /// `hutand connected`, `hutand stopped`, `hutand reconnecting…`.
    pub daemon: String,
    /// The sessions with a request waiting or an unseen failure, in daemon order.
    pub attention: Vec<AttentionEntry>,
    /// The keep-running checkbox (`app.keep_running`): always usable.
    pub keep_running: bool,
    /// The start-at-login checkbox: on, whether it can be used, and what the system says.
    pub start_at_login: bool,
    pub start_at_login_available: bool,
    pub start_at_login_status: LoginItemStatus,
}

impl StatusItemModel {
    /// The count next to the glyph; empty at zero.
    pub fn title(&self) -> String {
        match self.attention.len() {
            0 => String::new(),
            n => n.to_string(),
        }
    }

    /// The menu, top to bottom.
    pub fn menu(&self) -> Vec<MenuSpec> {
        let mut menu = vec![MenuSpec::Label(self.daemon.clone()), MenuSpec::Separator];
        let needing = self.attention.len();
        if needing > 0 {
            menu.push(MenuSpec::Label(if needing == 1 {
                "1 needs attention".to_owned()
            } else {
                format!("{needing} need attention")
            }));
            menu.extend(
                self.attention
                    .iter()
                    .take(MAX_LISTED)
                    .map(|entry| MenuSpec::Item {
                        title: entry.text(),
                        command: MenuCommand::FocusSession(entry.session),
                        checked: false,
                        enabled: true,
                    }),
            );
            if needing > MAX_LISTED {
                menu.push(MenuSpec::Label(format!(
                    "…and {} more",
                    needing - MAX_LISTED
                )));
            }
            menu.push(MenuSpec::Separator);
        }
        menu.push(item("Open Hutan", MenuCommand::OpenHutan));
        menu.push(item("New Terminal", MenuCommand::NewTerminal));
        menu.push(MenuSpec::Separator);
        menu.push(MenuSpec::Item {
            title: KEEP_RUNNING.to_owned(),
            command: MenuCommand::ToggleKeepRunning,
            checked: self.keep_running,
            enabled: true,
        });
        menu.push(MenuSpec::Item {
            title: start_at_login_title(self.start_at_login_available, self.start_at_login_status),
            command: MenuCommand::ToggleStartAtLogin,
            checked: self.start_at_login,
            enabled: start_at_login_usable(
                self.start_at_login_available,
                self.start_at_login_status,
            ),
        });
        menu.push(MenuSpec::Separator);
        menu.push(item("Quit Hutan", MenuCommand::Quit));
        menu
    }
}

fn item(title: &str, command: MenuCommand) -> MenuSpec {
    MenuSpec::Item {
        title: title.to_owned(),
        command,
        checked: false,
        enabled: true,
    }
}

/// The title of the keep-running checkbox.
pub const KEEP_RUNNING: &str = "Keep Daemon Running After Hutan Quits";
/// The title of the start-at-login checkbox, before what is wrong with it when something is.
pub const START_AT_LOGIN: &str = "Start Daemon at Login";

/// The title of the start-at-login checkbox, with what is wrong with it when something is.
pub fn start_at_login_title(available: bool, status: LoginItemStatus) -> String {
    if !available {
        return format!("{START_AT_LOGIN} (available in Hutan.app)");
    }
    match status {
        LoginItemStatus::Enabled | LoginItemStatus::NotRegistered => START_AT_LOGIN.to_owned(),
        other => format!("{START_AT_LOGIN} ({})", other.label()),
    }
}

/// Whether the start-at-login checkbox can be used: the app runs from Hutan.app and the
/// launch agent is in it.
pub fn start_at_login_usable(available: bool, status: LoginItemStatus) -> bool {
    available && status != LoginItemStatus::Missing
}

/// The line under the start-at-login switch (Setup's *Background* step): what it does, or
/// why it cannot be used, or what it waits for.
pub fn start_at_login_note(available: bool, status: LoginItemStatus) -> &'static str {
    if !available {
        return "Available when Hutan runs from Hutan.app.";
    }
    match status {
        LoginItemStatus::RequiresApproval => {
            "Allow Hutan in System Settings → General → Login Items."
        }
        LoginItemStatus::Missing => {
            "The launch agent is missing from Hutan.app (Contents/Library/LaunchAgents): \
             install Hutan again."
        }
        LoginItemStatus::Enabled | LoginItemStatus::NotRegistered => {
            "Registers a Login Item; the daemon is restarted if it crashes."
        }
    }
}

/// The menu bar.
pub trait StatusItemHost {
    /// Shows the item (creating it if needed) with this content; called again whenever the
    /// content changes.
    fn show(&self, model: &StatusItemModel);
    /// Removes the item.
    fn hide(&self);
}

/// Does nothing (no menu bar integration).
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl StatusItemHost for Nothing {
    fn show(&self, _model: &StatusItemModel) {}

    fn hide(&self) {}
}

#[derive(Debug, Default)]
struct FakeState {
    shown: Option<StatusItemModel>,
    shows: usize,
    hides: usize,
}

/// A menu bar that remembers what it was given.
#[derive(Debug, Clone, Default)]
pub struct FakeStatusItem {
    state: Rc<RefCell<FakeState>>,
}

impl FakeStatusItem {
    pub fn new() -> Self {
        Self::default()
    }

    /// The content of the item now; `None` while it is not on the menu bar.
    pub fn shown(&self) -> Option<StatusItemModel> {
        self.state.borrow().shown.clone()
    }

    /// How many times content was put on the menu bar.
    pub fn shows(&self) -> usize {
        self.state.borrow().shows
    }

    /// How many times the item was removed.
    pub fn hides(&self) -> usize {
        self.state.borrow().hides
    }
}

impl StatusItemHost for FakeStatusItem {
    fn show(&self, model: &StatusItemModel) {
        let mut state = self.state.borrow_mut();
        state.shown = Some(model.clone());
        state.shows += 1;
    }

    fn hide(&self) {
        let mut state = self.state.borrow_mut();
        state.shown = None;
        state.hides += 1;
    }
}

#[cfg(target_os = "macos")]
pub use system::SystemStatusItem;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Objective-C class definition and calls; each block says why it is sound.
mod system {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSBitmapImageRep, NSCellImagePosition, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem,
        NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    };
    use objc2_foundation::{
        MainThreadMarker, NSData, NSObject, NSObjectProtocol, NSSize, NSString,
    };

    use super::{MenuSpec, StatusItemHost, StatusItemModel};
    use crate::macos::{EventSender, MacEvent, MenuCommand};

    /// The glyph at 18 and 36 pixels (`just menubar-icon`).
    const GLYPH_1X: &[u8] = include_bytes!("../../../../assets/macos/MenuBarIcon.png");
    const GLYPH_2X: &[u8] = include_bytes!("../../../../assets/macos/MenuBarIcon@2x.png");
    const GLYPH_POINTS: f64 = 18.0;

    struct TargetIvars {
        sender: EventSender,
        /// What each menu item's tag stands for, for the menu on screen.
        commands: RefCell<Vec<MenuCommand>>,
    }

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements and `MenuTarget` has no `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "HutanStatusMenuTarget"]
        #[ivars = TargetIvars]
        struct MenuTarget;

        // SAFETY: `NSObjectProtocol` has no requirements.
        unsafe impl NSObjectProtocol for MenuTarget {}

        impl MenuTarget {
            // SAFETY: the selector takes the sending menu item, as `NSMenuItem` actions do.
            #[unsafe(method(chosen:))]
            fn chosen(&self, sender: &NSMenuItem) {
                let command = usize::try_from(sender.tag())
                    .ok()
                    .and_then(|tag| self.ivars().commands.borrow().get(tag).copied());
                if let Some(command) = command
                    && let Err(error) = self.ivars().sender.try_send(MacEvent::Menu(command))
                {
                    tracing::warn!(%error, "a menu bar choice could not be delivered");
                }
            }
        }
    );

    impl MenuTarget {
        fn new(sender: EventSender) -> Retained<Self> {
            let this = Self::alloc().set_ivars(TargetIvars {
                sender,
                commands: RefCell::new(Vec::new()),
            });
            // SAFETY: `init` of `NSObject`, the superclass, on a freshly allocated object.
            unsafe { msg_send![super(this), init] }
        }
    }

    struct Native {
        item: Retained<NSStatusItem>,
        target: Retained<MenuTarget>,
    }

    /// `NSStatusItem` in the system status bar, created on the first `show`.
    pub struct SystemStatusItem {
        sender: EventSender,
        native: RefCell<Option<Native>>,
    }

    impl SystemStatusItem {
        /// Touches no system API until `show`.
        pub fn new(sender: EventSender) -> Self {
            Self {
                sender,
                native: RefCell::new(None),
            }
        }

        fn create(&self, mtm: MainThreadMarker) -> Option<Native> {
            let item =
                NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
            let button = item.button(mtm)?;
            button.setImage(Some(&glyph()));
            button.setImagePosition(NSCellImagePosition::ImageLeft);
            Some(Native {
                item,
                target: MenuTarget::new(self.sender.clone()),
            })
        }
    }

    /// The template glyph: the 18-pixel PNG with the 36-pixel one as its @2x representation.
    fn glyph() -> Retained<NSImage> {
        let one = NSData::with_bytes(GLYPH_1X);
        let two = NSData::with_bytes(GLYPH_2X);
        let image = NSImage::initWithData(NSImage::alloc(), &one).unwrap_or_default();
        let size = NSSize::new(GLYPH_POINTS, GLYPH_POINTS);
        if let Some(rep) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &two) {
            // 36 pixels in 18 points: the representation for 2x displays.
            rep.setSize(size);
            image.addRepresentation(&rep);
        }
        image.setSize(size);
        image.setTemplate(true);
        image
    }

    impl std::fmt::Debug for SystemStatusItem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SystemStatusItem")
                .field("on_menu_bar", &self.native.borrow().is_some())
                .finish_non_exhaustive()
        }
    }

    impl StatusItemHost for SystemStatusItem {
        fn show(&self, model: &StatusItemModel) {
            let Some(mtm) = MainThreadMarker::new() else {
                tracing::warn!("the status item was updated off the main thread; ignored");
                return;
            };
            let mut slot = self.native.borrow_mut();
            if slot.is_none() {
                *slot = self.create(mtm);
            }
            let Some(native) = slot.as_ref() else {
                tracing::warn!("the status item has no button; the menu bar item is off");
                return;
            };
            if let Some(button) = native.item.button(mtm) {
                button.setTitle(&NSString::from_str(&model.title()));
            }
            let menu = NSMenu::new(mtm);
            menu.setAutoenablesItems(false);
            let mut commands = Vec::new();
            for spec in model.menu() {
                match spec {
                    MenuSpec::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                    MenuSpec::Label(text) => {
                        let item = menu_item(mtm, &text, None);
                        item.setEnabled(false);
                        menu.addItem(&item);
                    }
                    MenuSpec::Item {
                        title,
                        command,
                        checked,
                        enabled,
                    } => {
                        let item = menu_item(mtm, &title, Some(sel!(chosen:)));
                        item.setTag(isize::try_from(commands.len()).unwrap_or(isize::MAX));
                        // SAFETY: the target outlives the menu: both are owned by `Native`.
                        unsafe { item.setTarget(Some(&native.target)) };
                        item.setEnabled(enabled);
                        if checked {
                            item.setState(NSControlStateValueOn);
                        }
                        commands.push(command);
                        menu.addItem(&item);
                    }
                }
            }
            *native.target.ivars().commands.borrow_mut() = commands;
            native.item.setMenu(Some(&menu));
        }

        fn hide(&self) {
            if self.native.borrow_mut().take().is_some_and(|native| {
                NSStatusBar::systemStatusBar().removeStatusItem(&native.item);
                true
            }) {
                tracing::debug!("the menu bar item was removed");
            }
        }
    }

    fn menu_item(
        mtm: MainThreadMarker,
        title: &str,
        action: Option<objc2::runtime::Sel>,
    ) -> Retained<NSMenuItem> {
        // SAFETY: `initWithTitle:action:keyEquivalent:` on a freshly allocated item; the key
        // equivalent is empty.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc::<NSMenuItem>(),
                &NSString::from_str(title),
                action,
                &NSString::new(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::SessionId;

    use super::*;
    use crate::macos::login_item::not_found_status;
    use crate::sessions::attention::AttentionKind;

    #[test]
    fn the_daemons_menu_bar_asks_for_settings_by_the_name_of_this_action() {
        assert_eq!(
            hutan_daemon::menu_bar::OPEN_SETTINGS_ACTION,
            <crate::workbench::actions::OpenSettings as gpui::Action>::name_for_type()
        );
    }

    fn entry(title: &str, kind: AttentionKind) -> AttentionEntry {
        AttentionEntry {
            session: SessionId::new(),
            title: title.to_owned(),
            kind,
            detail: String::new(),
        }
    }

    fn model(attention: Vec<AttentionEntry>) -> StatusItemModel {
        StatusItemModel {
            daemon: "hutand connected".to_owned(),
            attention,
            keep_running: false,
            start_at_login: false,
            start_at_login_available: true,
            start_at_login_status: LoginItemStatus::NotRegistered,
        }
    }

    #[test]
    fn nobody_waiting_shows_no_count_and_no_list() {
        let model = model(vec![]);
        assert_eq!(model.title(), "");
        let menu = model.menu();
        assert_eq!(menu[0], MenuSpec::Label("hutand connected".to_owned()));
        assert!(
            !menu
                .iter()
                .any(|row| matches!(row, MenuSpec::Label(t) if t.contains("need"))),
            "{menu:?}"
        );
        let commands: Vec<MenuCommand> = menu
            .iter()
            .filter_map(|row| match row {
                MenuSpec::Item { command, .. } => Some(*command),
                _ => None,
            })
            .collect();
        assert_eq!(
            commands,
            [
                MenuCommand::OpenHutan,
                MenuCommand::NewTerminal,
                MenuCommand::ToggleKeepRunning,
                MenuCommand::ToggleStartAtLogin,
                MenuCommand::Quit
            ]
        );
    }

    #[test]
    fn waiting_sessions_are_counted_listed_and_focus_their_session() {
        let blocked = entry("claude", AttentionKind::Blocked);
        let approval = entry("codex", AttentionKind::Approval);
        let model = model(vec![blocked.clone(), approval.clone()]);
        assert_eq!(model.title(), "2");
        let menu = model.menu();
        assert!(menu.contains(&MenuSpec::Label("2 need attention".to_owned())));
        assert!(menu.iter().any(|row| matches!(
            row,
            MenuSpec::Item { command: MenuCommand::FocusSession(id), title, .. }
                if *id == blocked.session && title.starts_with("claude")
        )));
        assert!(menu.iter().any(|row| matches!(
            row,
            MenuSpec::Item { command: MenuCommand::FocusSession(id), .. } if *id == approval.session
        )));
        let single = self::model(vec![blocked]);
        assert!(
            single
                .menu()
                .contains(&MenuSpec::Label("1 needs attention".to_owned()))
        );
    }

    #[test]
    fn a_long_list_is_cut_and_summarised() {
        let many = (0..MAX_LISTED + 3)
            .map(|i| entry(&format!("s{i}"), AttentionKind::Failed))
            .collect();
        let model = model(many);
        assert_eq!(model.title(), "13");
        let listed = model
            .menu()
            .iter()
            .filter(|row| {
                matches!(
                    row,
                    MenuSpec::Item {
                        command: MenuCommand::FocusSession(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(listed, MAX_LISTED);
        assert!(
            model
                .menu()
                .contains(&MenuSpec::Label("…and 3 more".to_owned()))
        );
    }

    #[test]
    fn keep_running_is_always_usable() {
        let keep = |model: &StatusItemModel| {
            model
                .menu()
                .into_iter()
                .find_map(|row| match row {
                    MenuSpec::Item {
                        command: MenuCommand::ToggleKeepRunning,
                        title,
                        checked,
                        enabled,
                    } => Some((title, checked, enabled)),
                    _ => None,
                })
                .unwrap()
        };
        let mut model = model(vec![]);
        assert_eq!(keep(&model), (KEEP_RUNNING.to_owned(), false, true));
        model.keep_running = true;
        model.start_at_login_available = false;
        assert_eq!(
            keep(&model),
            (KEEP_RUNNING.to_owned(), true, true),
            "it needs no login item"
        );
    }

    #[test]
    fn start_at_login_shows_its_state_and_is_disabled_outside_the_bundle() {
        let login = |model: &StatusItemModel| {
            model
                .menu()
                .into_iter()
                .find_map(|row| match row {
                    MenuSpec::Item {
                        command: MenuCommand::ToggleStartAtLogin,
                        title,
                        checked,
                        enabled,
                    } => Some((title, checked, enabled)),
                    _ => None,
                })
                .unwrap()
        };
        let mut model = model(vec![]);
        assert_eq!(login(&model), (START_AT_LOGIN.to_owned(), false, true));
        model.start_at_login = true;
        model.start_at_login_status = LoginItemStatus::RequiresApproval;
        assert_eq!(
            login(&model),
            (format!("{START_AT_LOGIN} (requires approval)"), true, true)
        );
        model.start_at_login_available = false;
        assert_eq!(
            login(&model),
            (
                format!("{START_AT_LOGIN} (available in Hutan.app)"),
                true,
                false
            )
        );
    }

    #[test]
    fn an_agent_never_registered_is_not_called_missing() {
        // What SMAppService answers before the first registration, the plist in the bundle.
        let never = not_found_status(true);
        assert_eq!(never, LoginItemStatus::NotRegistered);
        assert_eq!(start_at_login_title(true, never), START_AT_LOGIN);
        assert!(start_at_login_usable(true, never));
        assert_eq!(
            start_at_login_note(true, never),
            "Registers a Login Item; the daemon is restarted if it crashes."
        );
        let mut model = model(vec![]);
        model.start_at_login_status = never;
        assert!(model.menu().iter().any(|row| matches!(
            row,
            MenuSpec::Item { command: MenuCommand::ToggleStartAtLogin, title, enabled: true, .. }
                if title == START_AT_LOGIN
        )));

        // The same answer without the plist: missing, and the toggle says so and is off.
        let missing = not_found_status(false);
        assert_eq!(missing, LoginItemStatus::Missing);
        assert_eq!(
            start_at_login_title(true, missing),
            format!("{START_AT_LOGIN} (missing from Hutan.app)")
        );
        assert!(!start_at_login_usable(true, missing));
        assert!(start_at_login_note(true, missing).starts_with("The launch agent is missing"));
        model.start_at_login_status = missing;
        assert!(model.menu().iter().any(|row| matches!(
            row,
            MenuSpec::Item {
                command: MenuCommand::ToggleStartAtLogin,
                enabled: false,
                ..
            }
        )));
    }
}
