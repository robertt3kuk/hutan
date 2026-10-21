//! Native notifications (roadmap M6.3, ADR 0007): the daemon itself posts its notification
//! intents to the system under the app's identifier `dev.hutan.Hutan`, and routes the user's
//! clicks back like `notify.act`.
//!
//! Layers:
//!
//! - [`NativeNotification`] is the mapping from an intent to what is shown: one category per
//!   intent kind, an approval's first [`MAX_OPTION_ACTIONS`] options as actions (destructive
//!   style for the deny and cancel kinds) plus *Open*, the intent id as the notification
//!   identifier and the session id as the thread. Pure, so it is tested without a system.
//! - [`NativePoster`] is the seam: [`authorize`](NativePoster::authorize),
//!   [`post`](NativePoster::post), [`remove`](NativePoster::remove) and a channel of
//!   [`NativeResponse`]s. [`UserNotifications`](cocoa::UserNotifications) (macOS) is the real
//!   one; [`fake::FakePoster`] stands in for it in tests. Calls never block: the real poster
//!   hands work to the main thread's run loop.
//! - [`Delivery`] is the state machine in front of a poster: authorization is requested lazily
//!   before the first post, once per daemon run; posts wait while the permission prompt is up;
//!   a refusal ends all posting.
//! - [`run`] is the task that [`NotifyService`](crate::notify::NotifyService) starts: it takes
//!   the posts and removals the service queues (bounded) and the clicks the poster reports.
//!
//! The real poster exists only in a daemon that runs from `Hutan.app` with bundle identifier
//! [`BUNDLE_IDENTIFIER`] ([`system_host`], decided by [`runs_from_hutan_app`]); any other
//! daemon (a `cargo run` one, a test) logs once and posts nothing, and its intents stay
//! available through `notify.list`. The system's notification centre needs a Cocoa run loop on
//! the process's main thread, so `hutand` runs tokio on its own thread and hands the main
//! thread to [`MainThreadHost::run`] (`docs/packaging.md`).

use std::collections::VecDeque;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;

use hutan_protocol::notify::{IntentKind, NotificationIntent};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::notify::NotifyService;

#[cfg(target_os = "macos")]
pub mod cocoa;
#[cfg(target_os = "macos")]
pub use cocoa::{MainThreadHost, Stopper, system_host};

#[cfg(any(test, feature = "test-support"))]
pub mod fake;

#[cfg(not(target_os = "macos"))]
mod host_stub {
    //! Only macOS has a notification centre: no host, nothing to run on the main thread.
    use std::sync::Arc;

    use super::NativePoster;
    use crate::menu_bar::MenuBar;

    #[derive(Debug)]
    pub enum MainThreadHost {}

    impl MainThreadHost {
        pub fn poster(&self) -> Arc<dyn NativePoster> {
            match *self {}
        }

        pub fn menu_bar(&self) -> MenuBar {
            match *self {}
        }

        pub fn stopper(&self) -> Stopper {
            match *self {}
        }

        pub fn run(self) {
            match self {}
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Stopper;

    impl Stopper {
        pub fn stop(&self) {}
    }

    pub fn system_host() -> Option<MainThreadHost> {
        None
    }
}
#[cfg(not(target_os = "macos"))]
pub use host_stub::{MainThreadHost, Stopper, system_host};

/// The app's bundle identifier; also the code-signing identifier of `hutand` (`xtask bundle`),
/// which is what lets the daemon share the app's notification permission (ADR 0007).
pub const BUNDLE_IDENTIFIER: &str = "dev.hutan.Hutan";
/// The identifier of the *Open* action. Not [`ACTION_OPEN`](hutan_protocol::notify::ACTION_OPEN):
/// an approval's own option ids share the namespace of the other actions, and a provider's
/// `open` must not become a foreground action.
pub const OPEN_ACTION_ID: &str = "hutan.open";
/// How many of an approval's options become actions next to *Open* (the system shows few).
pub const MAX_OPTION_ACTIONS: usize = 3;
/// Posts and removals queued between the service and the delivery task.
pub const OPS_QUEUE: usize = 256;
/// Clicks queued between the poster and the delivery task.
pub const RESPONSE_QUEUE: usize = 64;
/// Posts held back while the permission prompt is up.
const MAX_WAITING: usize = crate::notify::MAX_PENDING;

/// The system's identifier of a click on the notification body.
pub const SYSTEM_DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";
/// The system's identifier of the user dismissing the notification.
pub const SYSTEM_DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";

/// One button of a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAction {
    /// What a click reports: an approval option's id, or [`OPEN_ACTION_ID`].
    pub id: String,
    pub title: String,
    /// Shown in the destructive style (deny, cancel).
    pub destructive: bool,
    /// Brings the app to the front (Open).
    pub foreground: bool,
}

/// What is shown for one intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeNotification {
    /// The intent's id: the system's notification identifier, which also removes it again.
    pub identifier: String,
    /// The session's id: notifications of one session stack.
    pub thread: String,
    /// Which set of buttons this notification carries; one per kind, and for approvals one per
    /// distinct set of options.
    pub category: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<NativeAction>,
    /// Plays the system's notification sound (`notify.sound = "default"`).
    pub sound: bool,
}

impl NativeNotification {
    /// The mapping. `destructive` lists the ids of the approval options that refuse (the
    /// intent itself only carries ids and labels). The title and body are the intent's:
    /// already redacted and bounded. It plays the sound; see [`silent`](Self::silent).
    pub fn from_intent(intent: &NotificationIntent, destructive: &[String]) -> Self {
        let mut actions: Vec<NativeAction> = if intent.kind == IntentKind::ApprovalRequired {
            intent
                .actions
                .iter()
                .take(MAX_OPTION_ACTIONS)
                .map(|action| NativeAction {
                    id: action.id.clone(),
                    title: if action.label.trim().is_empty() {
                        action.id.clone()
                    } else {
                        action.label.clone()
                    },
                    destructive: destructive.contains(&action.id),
                    foreground: false,
                })
                .collect()
        } else {
            Vec::new()
        };
        actions.push(NativeAction {
            id: OPEN_ACTION_ID.to_owned(),
            title: "Open".to_owned(),
            destructive: false,
            foreground: true,
        });
        Self {
            identifier: intent.id.clone(),
            thread: intent.session_id.to_string(),
            category: category_id(intent.kind, &actions),
            title: intent.title.clone(),
            body: intent.body.clone(),
            actions,
            sound: true,
        }
    }

    /// The same notification without its sound (`notify.sound = "none"`).
    pub fn silent(mut self, silent: bool) -> Self {
        self.sound &= !silent;
        self
    }
}

/// `hutan.<kind>`; approvals offer provider-defined options, so theirs carries a hash of the
/// buttons as well.
fn category_id(kind: IntentKind, actions: &[NativeAction]) -> String {
    if kind != IntentKind::ApprovalRequired {
        return format!("hutan.{}", kind.as_str());
    }
    // FNV-1a: stable and dependency free; the identifier only has to tell sets apart.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for action in actions {
        feed(action.id.as_bytes());
        feed(&[0x1f]);
        feed(action.title.as_bytes());
        feed(&[0x1f, u8::from(action.destructive), 0x1e]);
    }
    format!("hutan.{}.{hash:016x}", kind.as_str())
}

/// What the user did with a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseAction {
    /// An approval option's button (its option id).
    Action(String),
    /// The *Open* button or a click on the body.
    Open,
    /// The notification was dismissed.
    Dismiss,
}

/// Translates the system's action identifier of a click.
pub fn response_action(identifier: &str) -> ResponseAction {
    match identifier {
        SYSTEM_DEFAULT_ACTION | OPEN_ACTION_ID => ResponseAction::Open,
        SYSTEM_DISMISS_ACTION => ResponseAction::Dismiss,
        other => ResponseAction::Action(other.to_owned()),
    }
}

/// A click, reported by the poster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeResponse {
    pub intent_id: String,
    pub action: ResponseAction,
}

/// Shows notifications and reports clicks. Implementations never block the caller (it is the
/// tokio runtime) and only log what they cannot do.
pub trait NativePoster: Debug + Send + Sync {
    /// Asks the system for permission to notify; answers `true` when granted. The answer may
    /// take as long as the user does. A poster that dies answers by dropping the sender.
    fn authorize(&self) -> oneshot::Receiver<bool>;

    fn post(&self, notification: &NativeNotification);

    /// Takes the notification with this identifier off the screen (and out of the notification
    /// centre) if it is still there.
    fn remove(&self, identifier: &str);

    /// The clicks; handed out once, to whoever consumes them.
    fn responses(&self) -> Option<mpsc::Receiver<NativeResponse>>;
}

/// Whether a daemon whose executable is `exe` and whose main bundle is named
/// `bundle_identifier` may post: it runs from `<something>.app/Contents/MacOS/` and the
/// bundle is Hutan's. Pure; nothing here asks the system anything.
pub fn runs_from_hutan_app(exe: &Path, bundle_identifier: Option<&str>) -> bool {
    bundle_identifier == Some(BUNDLE_IDENTIFIER) && app_of(exe).is_some()
}

/// `X.app` for `X.app/Contents/MacOS/<exe>`.
pub(crate) fn app_of(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    if macos.file_name()? != "MacOS" {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != "Contents" {
        return None;
    }
    let app = contents.parent()?;
    (app.extension()? == "app").then_some(app)
}

/// What the delivery task is told by the service.
#[derive(Debug)]
pub(crate) enum NativeOp {
    Post(NativeNotification),
    Remove(String),
}

#[derive(Debug)]
enum Authorization {
    /// Not asked yet: the first post asks.
    Unrequested,
    /// The permission prompt may be up.
    Requesting(oneshot::Receiver<bool>),
    Granted,
    /// Refused (or the request failed): nothing is posted for the rest of this run.
    Denied,
}

/// The poster plus the authorization state in front of it.
#[derive(Debug)]
pub(crate) struct Delivery {
    poster: Arc<dyn NativePoster>,
    authorization: Authorization,
    /// Posts that wait for the answer to the permission prompt.
    waiting: VecDeque<NativeNotification>,
}

impl Delivery {
    pub(crate) fn new(poster: Arc<dyn NativePoster>) -> Self {
        Self {
            poster,
            authorization: Authorization::Unrequested,
            waiting: VecDeque::new(),
        }
    }

    pub(crate) fn post(&mut self, notification: NativeNotification) {
        match self.authorization {
            Authorization::Granted => self.poster.post(&notification),
            Authorization::Denied => {
                debug!(intent = %notification.identifier, "native notifications are not permitted; not posted");
            }
            Authorization::Unrequested => {
                self.authorization = Authorization::Requesting(self.poster.authorize());
                self.wait(notification);
            }
            Authorization::Requesting(_) => self.wait(notification),
        }
    }

    fn wait(&mut self, notification: NativeNotification) {
        self.waiting.push_back(notification);
        while self.waiting.len() > MAX_WAITING {
            self.waiting.pop_front();
        }
    }

    pub(crate) fn remove(&mut self, identifier: &str) {
        self.waiting.retain(|n| n.identifier != identifier);
        if matches!(self.authorization, Authorization::Granted) {
            self.poster.remove(identifier);
        }
    }

    /// Completes when the answer to the permission prompt arrives, and applies it; never
    /// completes while no request is out.
    pub(crate) async fn authorization_settled(&mut self) {
        let Authorization::Requesting(answer) = &mut self.authorization else {
            return std::future::pending().await;
        };
        match answer.await {
            Ok(true) => {
                info!("notification permission granted");
                self.authorization = Authorization::Granted;
                for notification in std::mem::take(&mut self.waiting) {
                    self.poster.post(&notification);
                }
            }
            Ok(false) => self.deny("the user did not allow notifications for Hutan"),
            Err(_) => self.deny("the notification permission request failed"),
        }
    }

    fn deny(&mut self, why: &str) {
        warn!("{why}: no native notifications this run; intents stay in notify.list");
        self.authorization = Authorization::Denied;
        self.waiting.clear();
    }
}

/// The delivery task: posts and removals from `ops`, clicks from `responses`, until `shutdown`.
pub(crate) async fn run(
    service: Arc<NotifyService>,
    poster: Arc<dyn NativePoster>,
    mut ops: mpsc::Receiver<NativeOp>,
    mut responses: mpsc::Receiver<NativeResponse>,
    shutdown: CancellationToken,
) {
    let mut delivery = Delivery::new(poster);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            op = ops.recv() => match op {
                Some(NativeOp::Post(notification)) => delivery.post(notification),
                Some(NativeOp::Remove(id)) => delivery.remove(&id),
                None => break,
            },
            response = responses.recv() => match response {
                Some(response) => service.native_response(response).await,
                None => {
                    warn!("the native notification poster went away; no more notifications");
                    break;
                }
            },
            () = delivery.authorization_settled() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::SessionId;
    use hutan_protocol::notify::IntentAction;

    use super::fake::{Authorization as Mode, Call, FakePoster};
    use super::*;

    fn intent(kind: IntentKind, actions: &[(&str, &str)]) -> NotificationIntent {
        NotificationIntent {
            id: format!("intent-{}", kind.as_str()),
            kind,
            session_id: SessionId::new(),
            workspace_id: None,
            title: "api: approval needed".to_owned(),
            body: "Run `rm -rf build`?".to_owned(),
            actions: actions
                .iter()
                .map(|(id, label)| IntentAction {
                    id: (*id).to_owned(),
                    label: (*label).to_owned(),
                })
                .collect(),
            created_at_ms: 1,
            expires_at_ms: None,
        }
    }

    fn open_only(kind: IntentKind) -> NotificationIntent {
        intent(kind, &[("open", "Open")])
    }

    fn ids(n: &NativeNotification) -> Vec<&str> {
        n.actions.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn every_kind_but_approvals_gets_its_own_category_with_just_open() {
        let mut categories = Vec::new();
        for kind in IntentKind::ALL {
            if kind == IntentKind::ApprovalRequired {
                continue;
            }
            let n = NativeNotification::from_intent(&open_only(kind), &[]);
            assert_eq!(n.category, format!("hutan.{}", kind.as_str()));
            assert_eq!(ids(&n), [OPEN_ACTION_ID]);
            assert!(n.actions[0].foreground && !n.actions[0].destructive);
            assert_eq!(n.actions[0].title, "Open");
            categories.push(n.category);
        }
        categories.sort();
        categories.dedup();
        assert_eq!(categories.len(), IntentKind::ALL.len() - 1);
    }

    #[test]
    fn task_intents_are_posted_with_their_own_category_and_just_open() {
        for kind in [IntentKind::TaskDone, IntentKind::TaskFailed] {
            let n = NativeNotification::from_intent(&open_only(kind), &[]);
            assert_eq!(n.category, format!("hutan.{}", kind.as_str()));
            assert_eq!(ids(&n), [OPEN_ACTION_ID]);
        }
    }

    #[test]
    fn an_approval_offers_its_options_then_open_with_refusals_destructive() {
        let intent = intent(
            IntentKind::ApprovalRequired,
            &[
                ("accept", "Allow once"),
                ("acceptForSession", "Allow for this session"),
                ("decline", "Deny"),
            ],
        );
        let n = NativeNotification::from_intent(&intent, &["decline".to_owned()]);
        assert_eq!(
            ids(&n),
            ["accept", "acceptForSession", "decline", OPEN_ACTION_ID]
        );
        let destructive: Vec<bool> = n.actions.iter().map(|a| a.destructive).collect();
        assert_eq!(destructive, [false, false, true, false]);
        let foreground: Vec<bool> = n.actions.iter().map(|a| a.foreground).collect();
        assert_eq!(foreground, [false, false, false, true]);
        assert_eq!(n.actions[0].title, "Allow once");
        assert!(
            n.category.starts_with("hutan.approval_required."),
            "{}",
            n.category
        );
        assert_eq!(n.identifier, intent.id, "the identifier is the intent id");
        assert_eq!(n.thread, intent.session_id.to_string());
        assert_eq!(n.title, intent.title);
        assert_eq!(n.body, intent.body);
    }

    #[test]
    fn only_the_first_three_options_become_actions() {
        let intent = intent(
            IntentKind::ApprovalRequired,
            &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D"), ("e", "E")],
        );
        let n = NativeNotification::from_intent(&intent, &[]);
        assert_eq!(ids(&n), ["a", "b", "c", OPEN_ACTION_ID]);
    }

    #[test]
    fn an_option_without_a_label_is_shown_by_its_id_and_a_provider_open_stays_an_option() {
        let intent = intent(
            IntentKind::ApprovalRequired,
            &[("accept", "  "), ("open", "Open the file")],
        );
        let n = NativeNotification::from_intent(&intent, &[]);
        assert_eq!(n.actions[0].title, "accept");
        assert_eq!(ids(&n), ["accept", "open", OPEN_ACTION_ID]);
        assert!(!n.actions[1].foreground, "only the real Open is foreground");
        assert_eq!(
            response_action("open"),
            ResponseAction::Action("open".to_owned()),
            "a provider's `open` is an option, not the Open button"
        );
    }

    #[test]
    fn approvals_share_a_category_exactly_when_their_buttons_match() {
        let make = |actions: &[(&str, &str)], destructive: &[&str]| {
            let destructive: Vec<String> = destructive.iter().map(|s| (*s).to_owned()).collect();
            NativeNotification::from_intent(
                &intent(IntentKind::ApprovalRequired, actions),
                &destructive,
            )
            .category
        };
        let base = make(&[("accept", "Allow"), ("decline", "Deny")], &["decline"]);
        assert_eq!(
            base,
            make(&[("accept", "Allow"), ("decline", "Deny")], &["decline"])
        );
        assert_ne!(base, make(&[("accept", "Allow")], &[]));
        assert_ne!(
            base,
            make(&[("accept", "Allow"), ("decline", "Deny")], &[]),
            "the style is part of the buttons"
        );
        assert_ne!(
            base,
            make(&[("accept", "Yes"), ("decline", "Deny")], &["decline"]),
            "so is the label"
        );
    }

    #[test]
    fn the_systems_action_identifiers_map_to_default_and_dismiss() {
        assert_eq!(response_action(SYSTEM_DEFAULT_ACTION), ResponseAction::Open);
        assert_eq!(response_action(OPEN_ACTION_ID), ResponseAction::Open);
        assert_eq!(
            response_action(SYSTEM_DISMISS_ACTION),
            ResponseAction::Dismiss
        );
        assert_eq!(
            response_action("accept"),
            ResponseAction::Action("accept".to_owned())
        );
    }

    #[test]
    fn the_real_poster_is_only_for_hutan_app_and_its_bundle_identifier() {
        let app = Path::new("/Users/me/Applications/Hutan.app/Contents/MacOS/hutand");
        assert!(runs_from_hutan_app(app, Some(BUNDLE_IDENTIFIER)));
        // The right bundle id but not a bundle path, and a bundle path with another identity.
        assert!(!runs_from_hutan_app(
            Path::new("/Users/me/proj/hutan/target/debug/hutand"),
            Some(BUNDLE_IDENTIFIER)
        ));
        assert!(!runs_from_hutan_app(app, Some("dev.hutan.daemon")));
        assert!(!runs_from_hutan_app(app, Some("dev.hutan.hutan")));
        assert!(!runs_from_hutan_app(app, None));
        // Shapes that only look like a bundle.
        for path in [
            "/Users/me/Hutan.app/hutand",
            "/Users/me/Hutan.app/Contents/hutand",
            "/Users/me/Hutan.app/Contents/Resources/hutand",
            "/Users/me/Hutan/Contents/MacOS/hutand",
            "/hutand",
            "hutand",
        ] {
            assert!(
                !runs_from_hutan_app(Path::new(path), Some(BUNDLE_IDENTIFIER)),
                "{path}"
            );
        }
    }

    #[test]
    fn this_test_binary_is_not_a_bundle_so_no_host_is_built() {
        let exe = std::env::current_exe().unwrap();
        assert!(!runs_from_hutan_app(&exe, Some(BUNDLE_IDENTIFIER)));
        assert!(system_host().is_none());
    }

    fn notification(id: &str) -> NativeNotification {
        let mut n = NativeNotification::from_intent(&open_only(IntentKind::AgentBlocked), &[]);
        n.identifier = id.to_owned();
        // The session id of the intent is random; the tests compare notifications.
        n.thread = "session".to_owned();
        n
    }

    #[tokio::test]
    async fn the_first_post_requests_authorization_once_and_later_posts_go_straight_out() {
        let poster = FakePoster::new(Mode::Grant);
        let mut delivery = Delivery::new(poster.clone());
        assert!(poster.calls().is_empty(), "nothing is asked before a post");

        delivery.post(notification("a"));
        assert_eq!(
            poster.calls(),
            [Call::Authorize],
            "posts wait for the answer"
        );
        delivery.authorization_settled().await;
        delivery.post(notification("b"));
        delivery.remove("a");

        assert_eq!(
            poster.calls(),
            [
                Call::Authorize,
                Call::Post(notification("a")),
                Call::Post(notification("b")),
                Call::Remove("a".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_refusal_ends_posting_for_the_run_and_asks_only_once() {
        let poster = FakePoster::new(Mode::Deny);
        let mut delivery = Delivery::new(poster.clone());
        delivery.post(notification("a"));
        delivery.post(notification("b"));
        delivery.authorization_settled().await;
        delivery.post(notification("c"));
        delivery.remove("a");
        assert_eq!(
            poster.calls(),
            [Call::Authorize],
            "nothing posted, nothing removed"
        );
    }

    #[tokio::test]
    async fn a_poster_that_dies_while_asking_counts_as_a_refusal() {
        let poster = FakePoster::new(Mode::Hold);
        let mut delivery = Delivery::new(poster.clone());
        delivery.post(notification("a"));
        poster.drop_authorization();
        delivery.authorization_settled().await;
        delivery.post(notification("b"));
        assert_eq!(poster.calls(), [Call::Authorize]);
    }

    #[tokio::test]
    async fn posts_wait_for_the_prompt_and_those_withdrawn_meanwhile_are_never_shown() {
        let poster = FakePoster::new(Mode::Hold);
        let mut delivery = Delivery::new(poster.clone());
        delivery.post(notification("a"));
        delivery.post(notification("b"));
        delivery.post(notification("c"));
        delivery.remove("b");
        assert_eq!(poster.calls(), [Call::Authorize], "the prompt is still up");

        // While the prompt is up, no settlement completes.
        let early = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            delivery.authorization_settled(),
        )
        .await;
        assert!(early.is_err());

        poster.answer(true);
        delivery.authorization_settled().await;
        assert_eq!(
            poster.calls(),
            [
                Call::Authorize,
                Call::Post(notification("a")),
                Call::Post(notification("c")),
            ],
            "in order, without the withdrawn one"
        );
    }

    #[tokio::test]
    async fn the_waiting_posts_are_bounded_and_the_oldest_is_dropped() {
        let poster = FakePoster::new(Mode::Hold);
        let mut delivery = Delivery::new(poster.clone());
        for n in 0..MAX_WAITING + 5 {
            delivery.post(notification(&format!("n{n}")));
        }
        poster.answer(true);
        delivery.authorization_settled().await;
        let posted = poster.posted();
        assert_eq!(posted.len(), MAX_WAITING);
        assert_eq!(posted[0].identifier, "n5");
    }
}
