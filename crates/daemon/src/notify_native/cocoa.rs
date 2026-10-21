//! The Cocoa side of native notifications: `UNUserNotificationCenter` through objc2, driven from
//! the process's main thread.
//!
//! **Threads.** The notification centre delivers its callbacks (permission answer, clicks,
//! `willPresent`) through the main thread's run loop; the spike (`docs/spikes/2026-10-15-notifications.md`
//! fact 5) proved that, so `hutand` runs tokio on a thread of its own and gives the main
//! thread to [`MainThreadHost::run`], a `NSRunLoop` loop that ends when the runtime thread
//! does. Everything that touches an Objective-C object runs there: the poster's calls are
//! queued (bounded) and a single drain block is scheduled on the main dispatch queue, so the
//! tokio side never waits for Cocoa and Cocoa never waits for tokio.
//!
//! **The application object.** Once the daemon's menu bar item (`menu_bar::cocoa`) has
//! something to show it asks [`shared_application`] for an `NSApplication` (an accessory: no
//! Dock icon, no menu). From then on the loop dequeues and dispatches AppKit events itself
//! (`nextEvent…` / `sendEvent:`), because status item clicks and modal alerts are delivered
//! through the application's event queue, which a bare run loop never drains; queued blocks
//! still run, as the main queue is served while it waits. A stop is announced with an
//! application-defined event so that the wait ends.
//!
//! **Nothing here runs unless asked.** [`system_host`] asks the bundle (`NSBundle.mainBundle`)
//! who we are and builds the poster only inside `Hutan.app`; the notification centre itself is
//! first touched on the main thread when the first command arrives, which is the first post
//! (lazy authorization, `notify_native`).

#![allow(unsafe_code)] // Objective-C calls and class definition; each block says why it is sound.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSBundle, NSDate, NSDefaultRunLoopMode, NSError, NSObject,
    NSObjectProtocol, NSPoint, NSRunLoop, NSSet, NSString, NSTimer,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use super::{
    NativeNotification, NativePoster, NativeResponse, RESPONSE_QUEUE, app_of, response_action,
    runs_from_hutan_app,
};
use crate::menu_bar::MenuBar;
use crate::menu_bar::cocoa::{OpenApp, SystemStatusItem};

/// Commands queued for the main thread.
const COMMAND_QUEUE: usize = super::OPS_QUEUE;
/// Categories kept registered; the oldest goes first. A category is needed until its
/// notification is clicked, and a daemon only ever sees a handful of distinct option sets.
const MAX_CATEGORIES: usize = 64;
/// Period of the timer that keeps the main run loop from returning for want of a source.
const KEEPALIVE_SECS: f64 = 24.0 * 3600.0;

// ---- The main thread's run loop --------------------------------------------------------------

static STOP: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The source that keeps `runMode:beforeDate:` waiting (main thread only).
    static KEEPALIVE: RefCell<Option<Retained<NSTimer>>> = const { RefCell::new(None) };
    /// The application object, once the menu bar item asked for it (main thread only).
    static APPLICATION: RefCell<Option<Retained<NSApplication>>> = const { RefCell::new(None) };
}

/// The process's `NSApplication` as an accessory, created on first use. Main thread only.
pub fn shared_application(mtm: MainThreadMarker) -> Retained<NSApplication> {
    APPLICATION.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                let app = NSApplication::sharedApplication(mtm);
                if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
                    warn!("the system refused the accessory activation policy");
                }
                app.finishLaunching();
                app
            })
            .clone()
    })
}

/// Wakes the loop out of its wait for an AppKit event (main thread only). Nothing without an
/// application: the loop then waits in the run loop, which a queued block wakes by itself.
fn wake_application() {
    let Some(app) = APPLICATION.with(|slot| slot.borrow().clone()) else {
        return;
    };
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

/// Runs the calling thread's run loop until a [`Stopper`] says stop. Must be the main thread:
/// only it serves the main dispatch queue the poster hands its work to. Not a poll: the loop
/// sleeps until the system or a queued block wakes it.
pub fn run_main_loop() {
    STOP.store(false, Ordering::Release);
    let keepalive = RcBlock::new(|_timer: NonNull<NSTimer>| {});
    // SAFETY: the block is `Fn(NonNull<NSTimer>)` as the API wants and does nothing, so it is
    // sound to call from the run loop; it is retained by the timer.
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_repeats_block(KEEPALIVE_SECS, true, &keepalive)
    };
    KEEPALIVE.with(|slot| *slot.borrow_mut() = Some(timer));
    let run_loop = NSRunLoop::currentRunLoop();
    while !STOP.load(Ordering::Acquire) {
        // SAFETY: reading a constant the framework exports.
        let mode = unsafe { NSDefaultRunLoopMode };
        let application = APPLICATION.with(|slot| slot.borrow().clone());
        if let Some(app) = application {
            // Sleeps until an event or a queued block arrives; a stop posts an event.
            let event = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantFuture()),
                mode,
                true,
            );
            if let Some(event) = event {
                app.sendEvent(&event);
            }
        } else if !run_loop.runMode_beforeDate(mode, &NSDate::distantFuture()) {
            // No source at all (the keepalive timer was invalidated): do not spin.
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    KEEPALIVE.with(|slot| {
        if let Some(timer) = slot.borrow_mut().take() {
            timer.invalidate();
        }
    });
}

/// Runs `work` on the main thread, inside its run loop. Never blocks; the work is lost if the
/// loop never runs.
pub fn on_main_thread(work: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(work);
}

/// Ends [`run_main_loop`] from any thread.
#[derive(Debug, Clone, Copy)]
pub struct Stopper;

impl Stopper {
    pub fn stop(&self) {
        // Through the main queue so that it also works before the loop has started (the
        // block then runs once it does) and always wakes a sleeping loop.
        on_main_thread(|| {
            STOP.store(true, Ordering::Release);
            KEEPALIVE.with(|slot| {
                if let Some(timer) = slot.borrow_mut().take() {
                    timer.invalidate();
                }
            });
            wake_application();
        });
    }
}

// ---- The host that `hutand` runs on its main thread ------------------------------------------

/// The real poster and the main thread's run loop that makes it work.
#[derive(Debug)]
pub struct MainThreadHost {
    poster: Arc<UserNotifications>,
    menu_bar: MenuBar,
}

impl MainThreadHost {
    pub fn poster(&self) -> Arc<dyn NativePoster> {
        self.poster.clone()
    }

    pub fn stopper(&self) -> Stopper {
        Stopper
    }

    /// The daemon's menu bar item and the way to open `Hutan.app`.
    pub fn menu_bar(&self) -> MenuBar {
        self.menu_bar.clone()
    }

    /// Runs the main thread's run loop until [`Stopper::stop`].
    pub fn run(self) {
        run_main_loop();
    }
}

/// The host when this process is `Hutan.app`'s daemon; otherwise `None`, after saying so once.
/// Asks the bundle for its identifier and nothing else: no notification API is touched.
pub fn system_host() -> Option<MainThreadHost> {
    let exe = std::env::current_exe().ok();
    let identifier = NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|identifier| identifier.to_string());
    let app = exe
        .as_deref()
        .filter(|exe| runs_from_hutan_app(exe, identifier.as_deref()))
        .and_then(app_of);
    match app {
        Some(app) => Some(MainThreadHost {
            poster: Arc::new(UserNotifications::new()),
            menu_bar: MenuBar::new(
                Arc::new(SystemStatusItem::new()),
                Arc::new(OpenApp::new(app)),
            ),
        }),
        None => {
            info!(
                executable = ?exe,
                bundle = ?identifier,
                "native notifications and the menu bar item are off: not running from Hutan.app ({}); intents stay available through notify.list",
                super::BUNDLE_IDENTIFIER
            );
            None
        }
    }
}

// ---- The poster ------------------------------------------------------------------------------

enum Command {
    Authorize(oneshot::Sender<bool>),
    Post(NativeNotification),
    Remove(String),
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authorize(_) => f.write_str("Authorize"),
            Self::Post(n) => write!(f, "Post({})", n.identifier),
            Self::Remove(id) => write!(f, "Remove({id})"),
        }
    }
}

/// What the main-thread drain block and the poster share.
#[derive(Debug)]
struct Shared {
    commands: std_mpsc::SyncSender<Command>,
    inbox: Mutex<std_mpsc::Receiver<Command>>,
    /// A drain block is queued on the main queue (at most one is, so the queue stays bounded).
    scheduled: AtomicBool,
    responses: mpsc::Sender<NativeResponse>,
}

/// `UNUserNotificationCenter` as a [`NativePoster`].
#[derive(Debug)]
pub struct UserNotifications {
    shared: Arc<Shared>,
    responses: Mutex<Option<mpsc::Receiver<NativeResponse>>>,
}

impl UserNotifications {
    /// Builds the channels only; the notification centre is first touched on the main thread
    /// by the first command.
    fn new() -> Self {
        let (commands, inbox) = std_mpsc::sync_channel(COMMAND_QUEUE);
        let (responses_tx, responses) = mpsc::channel(RESPONSE_QUEUE);
        Self {
            shared: Arc::new(Shared {
                commands,
                inbox: Mutex::new(inbox),
                scheduled: AtomicBool::new(false),
                responses: responses_tx,
            }),
            responses: Mutex::new(Some(responses)),
        }
    }

    fn submit(&self, command: Command) {
        match self.shared.commands.try_send(command) {
            Ok(()) => {}
            Err(std_mpsc::TrySendError::Full(command)) => {
                warn!(?command, "the native notification queue is full; dropped");
                return;
            }
            Err(std_mpsc::TrySendError::Disconnected(_)) => return,
        }
        if !self.shared.scheduled.swap(true, Ordering::AcqRel) {
            let shared = self.shared.clone();
            on_main_thread(move || drain(&shared));
        }
    }
}

impl NativePoster for UserNotifications {
    fn authorize(&self) -> oneshot::Receiver<bool> {
        let (answer, receiver) = oneshot::channel();
        self.submit(Command::Authorize(answer));
        receiver
    }

    fn post(&self, notification: &NativeNotification) {
        self.submit(Command::Post(notification.clone()));
    }

    fn remove(&self, identifier: &str) {
        self.submit(Command::Remove(identifier.to_owned()));
    }

    fn responses(&self) -> Option<mpsc::Receiver<NativeResponse>> {
        self.responses.lock().take()
    }
}

// ---- Main-thread state -----------------------------------------------------------------------

struct MainState {
    center: Retained<UNUserNotificationCenter>,
    /// The centre keeps its delegate weakly.
    _delegate: Retained<Delegate>,
    categories: Vec<(String, Retained<UNNotificationCategory>)>,
}

thread_local! {
    /// Created by the first command, on the main thread; Objective-C objects never leave it.
    static STATE: RefCell<Option<MainState>> = const { RefCell::new(None) };
}

/// Runs on the main thread: carries out every queued command.
fn drain(shared: &Shared) {
    // Before reading: a command queued from now on schedules another drain.
    shared.scheduled.store(false, Ordering::Release);
    loop {
        let next = shared.inbox.lock().try_recv();
        let Ok(command) = next else { break };
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            let state = state.get_or_insert_with(|| MainState::new(shared));
            state.execute(command);
        });
    }
}

impl MainState {
    fn new(shared: &Shared) -> Self {
        let delegate = Delegate::new(shared.responses.clone());
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        info!("native notifications: notification centre ready");
        Self {
            center,
            _delegate: delegate,
            categories: Vec::new(),
        }
    }

    fn execute(&mut self, command: Command) {
        match command {
            Command::Authorize(answer) => self.authorize(answer),
            Command::Post(notification) => self.post(&notification),
            Command::Remove(identifier) => self.remove(&identifier),
        }
    }

    fn authorize(&self, answer: oneshot::Sender<bool>) {
        let answer = Mutex::new(Some(answer));
        let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            // SAFETY: the system passes either null or a valid `NSError` for the call.
            if let Some(error) = unsafe { error.as_ref() } {
                warn!(error = %error.localizedDescription(), "notification authorization failed");
            }
            if let Some(answer) = answer.lock().take() {
                let _ = answer.send(granted.as_bool());
            }
        });
        self.center
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &block,
            );
    }

    fn post(&mut self, notification: &NativeNotification) {
        self.register_category(notification);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        content.setCategoryIdentifier(&NSString::from_str(&notification.category));
        content.setThreadIdentifier(&NSString::from_str(&notification.thread));
        // `notify.sound = "none"`: no sound; the banner (and the presentation option) stay.
        if notification.sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&notification.identifier),
            &content,
            None,
        );
        let identifier = notification.identifier.clone();
        let done = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: null or a valid `NSError`, as above.
            if let Some(error) = unsafe { error.as_ref() } {
                warn!(intent = %identifier, error = %error.localizedDescription(), "posting a notification failed");
            } else {
                debug!(intent = %identifier, "notification posted");
            }
        });
        self.center
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }

    fn remove(&self, identifier: &str) {
        let identifiers = NSArray::from_retained_slice(&[NSString::from_str(identifier)]);
        self.center
            .removeDeliveredNotificationsWithIdentifiers(&identifiers);
        self.center
            .removePendingNotificationRequestsWithIdentifiers(&identifiers);
    }

    /// Makes sure the notification's category (its set of buttons) is registered.
    fn register_category(&mut self, notification: &NativeNotification) {
        if self
            .categories
            .iter()
            .any(|(id, _)| *id == notification.category)
        {
            return;
        }
        let actions: Vec<Retained<UNNotificationAction>> = notification
            .actions
            .iter()
            .map(|action| {
                let mut options = UNNotificationActionOptions::empty();
                if action.destructive {
                    options |= UNNotificationActionOptions::Destructive;
                }
                if action.foreground {
                    options |= UNNotificationActionOptions::Foreground;
                }
                UNNotificationAction::actionWithIdentifier_title_options(
                    &NSString::from_str(&action.id),
                    &NSString::from_str(&action.title),
                    options,
                )
            })
            .collect();
        let category =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(&notification.category),
                &NSArray::from_retained_slice(&actions),
                &NSArray::new(),
                // Without it a dismissal is not reported.
                UNNotificationCategoryOptions::CustomDismissAction,
            );
        self.categories
            .push((notification.category.clone(), category));
        if self.categories.len() > MAX_CATEGORIES {
            self.categories.remove(0);
        }
        let all: Vec<Retained<UNNotificationCategory>> = self
            .categories
            .iter()
            .map(|(_, category)| category.clone())
            .collect();
        self.center
            .setNotificationCategories(&NSSet::from_retained_slice(&all));
    }
}

// ---- The delegate ----------------------------------------------------------------------------

struct DelegateIvars {
    responses: mpsc::Sender<NativeResponse>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `Delegate` has no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "HutanNotificationDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    // SAFETY: `NSObjectProtocol` has no requirements.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method signatures are those of the protocol.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// The daemon is never the frontmost app; without this the system would not show
        /// anything while it is "active".
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let intent_id = response.notification().request().identifier().to_string();
            let action = response_action(&response.actionIdentifier().to_string());
            let response = NativeResponse { intent_id, action };
            debug!(?response, "notification response");
            if let Err(error) = self.ivars().responses.try_send(response) {
                warn!(%error, "a notification click could not be delivered");
            }
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new(responses: mpsc::Sender<NativeResponse>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { responses });
        // SAFETY: `init` of `NSObject`, the superclass, on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}
