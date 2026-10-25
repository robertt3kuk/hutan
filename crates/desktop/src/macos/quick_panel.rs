//! The system side of the quick terminal (`terminal.quick_terminal`): the screen the pointer is
//! on, whether another app is reading a password, and which app had the keyboard before the
//! panel took it, so that hiding the panel can give it back.
//!
//! The panel window itself is an ordinary GPUI pop-up window (`WindowKind::PopUp`: a
//! non-activating panel that joins every Space and full-screen Space, see
//! `workbench/quick_terminal.rs`); nothing here touches it. Everything sits behind
//! [`QuickPanelHost`] so the workbench's tests never ask the system anything.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Where the pointer is, in the coordinates GPUI gives displays (points, origin at the top left
/// of the primary display), and how tall the menu bar of the screen under it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerScreen {
    pub x: f32,
    pub y: f32,
    /// Points the menu bar takes off the top of that screen (0 when it is hidden or absent).
    pub menu_bar: f32,
}

/// What the quick terminal asks of the system.
pub trait QuickPanelHost {
    /// The pointer's position; `None` when the system cannot say.
    fn pointer(&self) -> Option<PointerScreen>;
    /// Another app has secure keyboard input on (a password field has the keyboard, or an app
    /// turned on Secure Keyboard Entry): showing a panel that takes the keyboard would steal it
    /// from that app.
    fn secure_input(&self) -> bool;
    /// Remembers the app that has the keyboard now (not Hutan), right before the panel shows.
    fn remember_front_app(&self);
    /// Gives the keyboard back to the app [`remember_front_app`](Self::remember_front_app)
    /// remembered, if it is still running. Forgets it either way.
    fn restore_front_app(&self);
}

/// Does nothing: no pointer, no secure input, no app to give the keyboard back to.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nothing;

impl QuickPanelHost for Nothing {
    fn pointer(&self) -> Option<PointerScreen> {
        None
    }

    fn secure_input(&self) -> bool {
        false
    }

    fn remember_front_app(&self) {}

    fn restore_front_app(&self) {}
}

/// A call the workbench made to a [`FakeQuickPanel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickPanelCall {
    Remember,
    Restore,
}

#[derive(Debug, Default)]
struct FakeState {
    pointer: Cell<Option<(f32, f32)>>,
    menu_bar: Cell<f32>,
    secure: Cell<bool>,
    calls: RefCell<Vec<QuickPanelCall>>,
}

/// A host that records what the workbench asked and answers what it is told to.
#[derive(Debug, Clone, Default)]
pub struct FakeQuickPanel {
    state: Rc<FakeState>,
}

impl FakeQuickPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts the pointer at `(x, y)` (display coordinates), under a menu bar `menu_bar` tall.
    pub fn point_at(&self, x: f32, y: f32, menu_bar: f32) {
        self.state.pointer.set(Some((x, y)));
        self.state.menu_bar.set(menu_bar);
    }

    /// Another app turns secure keyboard input on or off.
    pub fn set_secure_input(&self, on: bool) {
        self.state.secure.set(on);
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<QuickPanelCall> {
        self.state.calls.borrow().clone()
    }
}

impl QuickPanelHost for FakeQuickPanel {
    fn pointer(&self) -> Option<PointerScreen> {
        self.state.pointer.get().map(|(x, y)| PointerScreen {
            x,
            y,
            menu_bar: self.state.menu_bar.get(),
        })
    }

    fn secure_input(&self) -> bool {
        self.state.secure.get()
    }

    fn remember_front_app(&self) {
        self.state.calls.borrow_mut().push(QuickPanelCall::Remember);
    }

    fn restore_front_app(&self) {
        self.state.calls.borrow_mut().push(QuickPanelCall::Restore);
    }
}

#[cfg(target_os = "macos")]
pub use system::SystemQuickPanel;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // one Carbon call; its block says why it is sound.
mod system {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSApplicationActivationOptions, NSEvent, NSRunningApplication, NSScreen, NSWorkspace,
    };
    use objc2_foundation::MainThreadMarker;

    use super::{PointerScreen, QuickPanelHost};

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        /// `true` while any process has secure event input on.
        fn IsSecureEventInputEnabled() -> u8;
    }

    /// `NSWorkspace`, `NSScreen` and Carbon, on the main thread.
    #[derive(Debug, Default)]
    pub struct SystemQuickPanel {
        previous: RefCell<Option<Retained<NSRunningApplication>>>,
    }

    impl SystemQuickPanel {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl QuickPanelHost for SystemQuickPanel {
        fn pointer(&self) -> Option<PointerScreen> {
            let mtm = MainThreadMarker::new()?;
            let location = NSEvent::mouseLocation();
            let screens = NSScreen::screens(mtm);
            // The first screen is the one with the menu bar; its top left is GPUI's origin and
            // AppKit's y runs upward from its bottom.
            let primary_height = screens.firstObject()?.frame().size.height;
            let under = screens.iter().find(|screen| {
                let frame = screen.frame();
                location.x >= frame.origin.x
                    && location.x < frame.origin.x + frame.size.width
                    && location.y >= frame.origin.y
                    && location.y < frame.origin.y + frame.size.height
            });
            let menu_bar = under.map_or(0.0, |screen| {
                let frame = screen.frame();
                let visible = screen.visibleFrame();
                // Top edge of the screen minus top edge of what the menu bar and Dock leave.
                (frame.origin.y + frame.size.height) - (visible.origin.y + visible.size.height)
            });
            #[allow(clippy::cast_possible_truncation)] // screen points
            Some(PointerScreen {
                x: location.x as f32,
                y: (primary_height - location.y) as f32,
                menu_bar: menu_bar.max(0.0) as f32,
            })
        }

        fn secure_input(&self) -> bool {
            // SAFETY: a plain query with no arguments, callable from any thread.
            unsafe { IsSecureEventInputEnabled() != 0 }
        }

        fn remember_front_app(&self) {
            let front = NSWorkspace::sharedWorkspace().frontmostApplication();
            let ours = NSRunningApplication::currentApplication().processIdentifier();
            // Hutan itself in front: there is nothing to give the keyboard back to.
            *self.previous.borrow_mut() = front.filter(|app| app.processIdentifier() != ours);
        }

        fn restore_front_app(&self) {
            if let Some(app) = self.previous.borrow_mut().take()
                && !app.isTerminated()
            {
                app.activateWithOptions(NSApplicationActivationOptions::empty());
            }
        }
    }
}
