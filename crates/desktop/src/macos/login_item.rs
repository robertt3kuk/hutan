//! "Start at login" (`app.start_at_login`): registering the bundled launch agent
//! `dev.hutan.daemon.plist` with
//! `SMAppService` (macOS 13+), which makes it a Login Item the user can see and switch off in
//! System Settings → General → Login Items.
//!
//! Everything the desktop does with the system goes through [`LoginItem`]. The real
//! implementation ([`SystemLoginItem`]) exists only on macOS and only when the process runs
//! from `Hutan.app`; tests and every other run use [`FakeLoginItem`] or [`Unavailable`] and
//! never call the framework.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// File name of the agent plist inside `Contents/Library/LaunchAgents`.
pub const AGENT_PLIST: &str = "dev.hutan.daemon.plist";
/// The bundle identifier of Hutan.app (ADR 0007).
pub const BUNDLE_ID: &str = "dev.hutan.Hutan";

/// What the system says about the launch agent (`SMAppServiceStatus`), read with the bundle:
/// see [`not_found_status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemStatus {
    /// Registered and allowed: launchd runs the daemon at login.
    Enabled,
    /// Registered, but the user has not allowed it (or switched it off) in Login Items.
    RequiresApproval,
    /// Not registered.
    NotRegistered,
    /// The agent's plist is not in the app bundle: it cannot be registered.
    Missing,
}

impl LoginItemStatus {
    /// The words shown next to the toggle.
    pub fn label(self) -> &'static str {
        match self {
            LoginItemStatus::Enabled => "enabled",
            LoginItemStatus::RequiresApproval => "requires approval",
            LoginItemStatus::NotRegistered => "not registered",
            LoginItemStatus::Missing => "missing from Hutan.app",
        }
    }

    /// Registered with the system, approved or not.
    pub fn registered(self) -> bool {
        matches!(
            self,
            LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval
        )
    }
}

/// What `SMAppService`'s `notFound` means for the agent. macOS answers it for an agent that was
/// never registered ("the service hasn't registered with the Service Management framework") as
/// well as for one whose plist is not in the bundle: measured on macOS 27.0.1 with ad-hoc signed
/// copies of Hutan.app in a scratch folder and in an `Applications` folder, with and without
/// `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` — `notFound` in all four
/// (docs/packaging.md § Start at login). So the file decides: there, the agent is just not
/// registered yet; absent, it is missing.
pub fn not_found_status(plist_in_bundle: bool) -> LoginItemStatus {
    if plist_in_bundle {
        LoginItemStatus::NotRegistered
    } else {
        LoginItemStatus::Missing
    }
}

/// The login item of the daemon.
pub trait LoginItem {
    /// Whether registering makes sense here: the desktop runs from `Hutan.app`.
    fn available(&self) -> bool;
    /// The current status; cheap, safe to call whenever the window activates.
    fn status(&self) -> LoginItemStatus;
    /// `SMAppService.register()`. The error is the system's own message. A registration
    /// records the bundle it was made from ([`registered_bundle`](Self::registered_bundle)).
    fn register(&self) -> Result<(), String>;
    /// `SMAppService.unregister()`.
    fn unregister(&self) -> Result<(), String>;
    /// The app bundle the registered agent belongs to, as recorded when it was registered;
    /// `None` when unknown (nothing registered, or registered before the record existed).
    /// The system does not say where a registered agent's bundle is.
    fn registered_bundle(&self) -> Option<PathBuf>;
    /// The app bundle this process runs from.
    fn running_bundle(&self) -> Option<PathBuf>;
    /// `SMAppService.openSystemSettingsLoginItems()`.
    fn open_login_items_settings(&self);
}

/// What runs outside the bundle: nothing is available and nothing is called.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unavailable;

impl LoginItem for Unavailable {
    fn available(&self) -> bool {
        false
    }

    fn status(&self) -> LoginItemStatus {
        LoginItemStatus::NotRegistered
    }

    fn register(&self) -> Result<(), String> {
        Err("starting at login is available in Hutan.app".to_owned())
    }

    fn unregister(&self) -> Result<(), String> {
        Ok(())
    }

    fn registered_bundle(&self) -> Option<PathBuf> {
        None
    }

    fn running_bundle(&self) -> Option<PathBuf> {
        None
    }

    fn open_login_items_settings(&self) {}
}

/// A call the desktop made to a [`FakeLoginItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginItemCall {
    Register,
    Unregister,
    OpenSettings,
}

#[derive(Debug)]
struct FakeState {
    status: LoginItemStatus,
    /// The status a successful `register` leaves behind (a fresh registration of an
    /// unapproved item is `RequiresApproval`).
    after_register: LoginItemStatus,
    register_error: Option<String>,
    unregister_error: Option<String>,
    /// The bundle this "process" runs from.
    running: Option<PathBuf>,
    /// The bundle recorded by the last registration.
    registered: Option<PathBuf>,
    calls: Vec<LoginItemCall>,
}

/// A scripted login item for tests: records the calls, answers with the statuses it is told.
#[derive(Debug, Clone)]
pub struct FakeLoginItem {
    state: Rc<RefCell<FakeState>>,
}

impl FakeLoginItem {
    /// Available, not registered; registering makes it `Enabled`. It runs from
    /// [`FAKE_BUNDLE`].
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(FakeState {
                status: LoginItemStatus::NotRegistered,
                after_register: LoginItemStatus::Enabled,
                register_error: None,
                unregister_error: None,
                running: Some(PathBuf::from(FAKE_BUNDLE)),
                registered: None,
                calls: Vec::new(),
            })),
        }
    }

    /// An agent registered earlier from `bundle` (an app since moved or reinstalled
    /// elsewhere), with `status`.
    pub fn set_registered_from(&self, bundle: &Path, status: LoginItemStatus) {
        let mut state = self.state.borrow_mut();
        state.registered = Some(bundle.to_owned());
        state.status = status;
    }

    /// The system changes the status behind the app's back (the user answers in Login Items).
    pub fn set_status(&self, status: LoginItemStatus) {
        self.state.borrow_mut().status = status;
    }

    /// The status a successful `register` leaves behind.
    pub fn set_after_register(&self, status: LoginItemStatus) {
        self.state.borrow_mut().after_register = status;
    }

    /// Makes `register` fail with `message`.
    pub fn fail_register(&self, message: &str) {
        self.state.borrow_mut().register_error = Some(message.to_owned());
    }

    /// Makes `unregister` fail with `message`.
    pub fn fail_unregister(&self, message: &str) {
        self.state.borrow_mut().unregister_error = Some(message.to_owned());
    }

    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<LoginItemCall> {
        self.state.borrow().calls.clone()
    }
}

/// Where a [`FakeLoginItem`] says the app runs from.
pub const FAKE_BUNDLE: &str = "/Applications/Hutan.app";

impl Default for FakeLoginItem {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginItem for FakeLoginItem {
    fn available(&self) -> bool {
        true
    }

    fn status(&self) -> LoginItemStatus {
        self.state.borrow().status
    }

    fn register(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.calls.push(LoginItemCall::Register);
        if let Some(error) = state.register_error.clone() {
            return Err(error);
        }
        state.status = state.after_register;
        state.registered = state.running.clone();
        Ok(())
    }

    fn unregister(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        state.calls.push(LoginItemCall::Unregister);
        if let Some(error) = state.unregister_error.clone() {
            return Err(error);
        }
        state.status = LoginItemStatus::NotRegistered;
        state.registered = None;
        Ok(())
    }

    fn registered_bundle(&self) -> Option<PathBuf> {
        self.state.borrow().registered.clone()
    }

    fn running_bundle(&self) -> Option<PathBuf> {
        self.state.borrow().running.clone()
    }

    fn open_login_items_settings(&self) {
        self.state
            .borrow_mut()
            .calls
            .push(LoginItemCall::OpenSettings);
    }
}

/// Whether `exe` is `<Something>.app/Contents/MacOS/<exe>`.
pub fn inside_app_bundle(exe: &Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(app) = contents.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && app.extension().is_some_and(|e| e == "app")
}

#[cfg(target_os = "macos")]
pub use system::SystemLoginItem;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Objective-C calls; each block says why it is sound.
mod system {
    use std::path::PathBuf;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSBundle, NSString, NSUserDefaults};
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    use super::{
        AGENT_PLIST, BUNDLE_ID, LoginItem, LoginItemStatus, inside_app_bundle, not_found_status,
    };

    /// The app's user default (`defaults read dev.hutan.Hutan LoginItemBundle`) that records
    /// the bundle a registration was made from: `SMAppService` does not say it, and every
    /// copy of Hutan.app shares the defaults of its bundle identifier.
    const REGISTERED_BUNDLE_KEY: &str = "LoginItemBundle";

    /// `SMAppService.agent(plistName:)` of the bundled daemon agent.
    pub struct SystemLoginItem {
        service: Retained<SMAppService>,
        /// `Contents/Library/LaunchAgents/dev.hutan.daemon.plist` of the running bundle.
        plist: PathBuf,
    }

    impl SystemLoginItem {
        /// The login item when this process runs from `Hutan.app` (its executable is in
        /// `Contents/MacOS` and the main bundle's identifier is `dev.hutan.Hutan`); `None`
        /// anywhere else, without touching the framework.
        pub fn new() -> Option<Self> {
            let exe = std::env::current_exe().ok()?;
            if !inside_app_bundle(&exe) {
                return None;
            }
            let bundle = NSBundle::mainBundle();
            let identifier = bundle.bundleIdentifier()?;
            if identifier.to_string() != BUNDLE_ID {
                return None;
            }
            let plist = PathBuf::from(bundle.bundlePath().to_string())
                .join("Contents/Library/LaunchAgents")
                .join(AGENT_PLIST);
            let name = NSString::from_str(AGENT_PLIST);
            // SAFETY: a class method taking a valid NSString; it only creates the proxy object.
            let service = unsafe { SMAppService::agentServiceWithPlistName(&name) };
            Some(Self { service, plist })
        }
    }

    impl std::fmt::Debug for SystemLoginItem {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SystemLoginItem").finish_non_exhaustive()
        }
    }

    impl LoginItem for SystemLoginItem {
        fn available(&self) -> bool {
            true
        }

        fn status(&self) -> LoginItemStatus {
            // SAFETY: `status` is a plain getter on a live service object.
            let status = unsafe { self.service.status() };
            if status == SMAppServiceStatus::Enabled {
                LoginItemStatus::Enabled
            } else if status == SMAppServiceStatus::RequiresApproval {
                LoginItemStatus::RequiresApproval
            } else if status == SMAppServiceStatus::NotFound {
                not_found_status(self.plist.is_file())
            } else {
                LoginItemStatus::NotRegistered
            }
        }

        fn register(&self) -> Result<(), String> {
            // SAFETY: the NSError out-parameter is handled by the generated binding.
            unsafe { self.service.registerAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())?;
            if let Some(bundle) = self.running_bundle() {
                let value = NSString::from_str(&bundle.to_string_lossy());
                let object: &AnyObject = &value;
                // SAFETY: an NSString is a property-list object, which user defaults store.
                unsafe {
                    NSUserDefaults::standardUserDefaults()
                        .setObject_forKey(Some(object), &NSString::from_str(REGISTERED_BUNDLE_KEY));
                }
            }
            Ok(())
        }

        fn unregister(&self) -> Result<(), String> {
            // SAFETY: as for `register`.
            unsafe { self.service.unregisterAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())?;
            NSUserDefaults::standardUserDefaults()
                .removeObjectForKey(&NSString::from_str(REGISTERED_BUNDLE_KEY));
            Ok(())
        }

        fn registered_bundle(&self) -> Option<PathBuf> {
            NSUserDefaults::standardUserDefaults()
                .stringForKey(&NSString::from_str(REGISTERED_BUNDLE_KEY))
                .map(|path| PathBuf::from(path.to_string()))
        }

        fn running_bundle(&self) -> Option<PathBuf> {
            let path = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
            Some(path.canonicalize().unwrap_or(path))
        }

        fn open_login_items_settings(&self) {
            // SAFETY: a class method without arguments.
            unsafe { SMAppService::openSystemSettingsLoginItems() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_executables_inside_an_app_bundle_count() {
        assert!(inside_app_bundle(Path::new(
            "/Users/me/Applications/Hutan.app/Contents/MacOS/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/work/hutan/target/debug/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/x/Hutan.app/Contents/Resources/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new(
            "/x/Hutan/Contents/MacOS/hutan-desktop"
        )));
        assert!(!inside_app_bundle(Path::new("hutan-desktop")));
    }

    #[test]
    fn the_fake_records_calls_and_follows_its_script() {
        let item = FakeLoginItem::new();
        assert_eq!(item.status(), LoginItemStatus::NotRegistered);
        assert_eq!(item.registered_bundle(), None);
        assert_eq!(item.running_bundle(), Some(PathBuf::from(FAKE_BUNDLE)));
        item.set_after_register(LoginItemStatus::RequiresApproval);
        item.register().unwrap();
        assert_eq!(item.status(), LoginItemStatus::RequiresApproval);
        assert!(item.status().registered());
        // A registration records the bundle it was made from; unregistering forgets it.
        assert_eq!(item.registered_bundle(), item.running_bundle());
        item.unregister().unwrap();
        assert_eq!(item.registered_bundle(), None);
        item.fail_register("boom");
        assert_eq!(item.register(), Err("boom".to_owned()));
        assert_eq!(item.status(), LoginItemStatus::NotRegistered);
        assert_eq!(item.registered_bundle(), None);
        assert_eq!(
            item.calls(),
            [
                LoginItemCall::Register,
                LoginItemCall::Unregister,
                LoginItemCall::Register
            ]
        );
    }
}
