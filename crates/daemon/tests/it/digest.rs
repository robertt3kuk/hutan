//! The digest notification (MAWAS-DIGEST-A, protocol 0.68) end to end: a real daemon on a
//! temporary `HUTAN_HOME`, real connections that report whether their window is the active one
//! (`client.focus`), a provider-backed Global Mawas (opened, never asked: no model is called),
//! a request that waits (Mawas's own rename of a shell asks for approval) and the fake native
//! poster. The daemon's "hour" is shortened to [`HOUR`] and it looks every [`TICK`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, create, sh, wait_event};
use hutan_client::{Client, ConnectOptions};
use hutan_core::{Seq, SessionId};
use hutan_daemon::digest::DigestTuning;
use hutan_daemon::notify_native::NativePoster;
use hutan_daemon::notify_native::fake::{Authorization, FakePoster};
use hutan_daemon::{DaemonConfig, GatewayConfig};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
use hutan_protocol::chat::{ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond};
use hutan_protocol::messages::{
    ClientKind, Empty, Event, Origin, Request, SessionRename, SessionResult,
};
use hutan_protocol::notify::{ACTION_OPEN, ActRoute, IntentKind, NotificationIntent, UiView};
use hutan_protocol::provider::{ProviderAdd, ProviderResult, ProviderSetCredential};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

/// The length of the daemon's "hour" in these tests.
const HOUR: u64 = 150;
/// How often the daemon looks.
const TICK: Duration = Duration::from_millis(20);
/// Long enough for the daemon to have looked several times past a whole period.
const SEVERAL_PERIODS: Duration = Duration::from_millis(HOUR * 4);

struct Env {
    daemon: TestDaemon,
    poster: Arc<FakePoster>,
    /// A desktop window's connection.
    desktop: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    mawas: SessionId,
}

impl Env {
    /// A daemon with `toml` (plus the provider-backed assistant) whose digest counts `HOUR` ms
    /// to the hour; one desktop connection that says its window is the active one.
    async fn start(toml: &str) -> Self {
        let poster = FakePoster::new(Authorization::Grant);
        let store = Arc::new(MemoryStore::new());
        let for_poster = poster.clone();
        let for_config = store.clone();
        let toml = format!("[assistant]\nprovider = \"mock\"\nmodel = \"m-1\"\n\n{toml}");
        let daemon = TestDaemon::start_with(&toml, move |c: &mut DaemonConfig| {
            c.native = Some(for_poster as Arc<dyn NativePoster>);
            c.digest = DigestTuning {
                tick: TICK,
                hour_ms: HOUR,
                ..Default::default()
            };
            c.gateway = GatewayConfig {
                credentials: Some(for_config as Arc<dyn CredentialStore>),
                http: HttpConfig {
                    no_proxy: true,
                    connect_retries: 1,
                    retry_backoff: Duration::from_millis(5),
                    ..HttpConfig::default()
                },
                discovery: Some(Vec::new()),
            };
        })
        .await;
        let desktop = daemon.connect_as(ClientKind::Desktop).await;
        let events = desktop.events();
        desktop
            .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        desktop.client_focus_in_window(None, true).await.unwrap();
        desktop
            .call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
                kind: "openai-compatible".into(),
                name: "mock".into(),
                base_url: Some("http://127.0.0.1:9/v1".into()),
                default_model: Some("m-1".into()),
            }))
            .await
            .unwrap();
        desktop
            .call::<ProviderResult>(Request::ProviderSetCredential(ProviderSetCredential {
                provider_id: "mock".into(),
                secret: "mock-key".into(),
            }))
            .await
            .unwrap();
        let mawas = desktop
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }))
            .await
            .unwrap()
            .session
            .id;
        Self {
            daemon,
            poster,
            desktop,
            events,
            mawas,
        }
    }

    /// The user leaves: the window is no longer the active one.
    async fn leave(&self) {
        self.desktop
            .client_focus_in_window(None, false)
            .await
            .unwrap();
    }

    /// A request waits: Mawas asks to rename a shell, and the daemon asks the user. Returns the
    /// approval and the task of the blocked call.
    async fn a_request_waits(&self, via: &Client) -> (ApprovalInfo, tokio::task::JoinHandle<()>) {
        let target = create(via, sh("sleep 120")).await;
        let mut options: ConnectOptions = self.daemon.options();
        options.client.origin = Some(Origin {
            session_id: self.mawas,
        });
        let from_mawas = Client::connect(options).await.unwrap();
        let asked = tokio::spawn(async move {
            let _ = from_mawas
                .call::<SessionResult>(Request::SessionRename(SessionRename {
                    session_id: target.id,
                    title: "renamed".into(),
                }))
                .await;
        });
        let approval = timeout(WAIT, async {
            loop {
                let list: ApprovalList = via
                    .call(Request::ApprovalList(ApprovalListRequest::default()))
                    .await
                    .unwrap();
                if let Some(info) = list.approvals.into_iter().next() {
                    return info;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the request never reached the user");
        (approval, asked)
    }

    /// The user refuses `approval`: the request no longer waits.
    async fn refuse(
        &self,
        via: &Client,
        approval: &ApprovalInfo,
        asked: tokio::task::JoinHandle<()>,
    ) {
        let option = approval
            .options
            .iter()
            .find(|o| o.label.to_lowercase().contains("deny") || o.id.contains("deny"))
            .unwrap_or_else(|| approval.options.last().unwrap());
        via.call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: option.id.clone(),
        }))
        .await
        .unwrap();
        let _ = timeout(WAIT, asked).await;
    }

    async fn digests(&self) -> Vec<NotificationIntent> {
        self.desktop
            .notify_list()
            .await
            .unwrap()
            .into_iter()
            .filter(|i| i.kind == IntentKind::Digest)
            .collect()
    }

    fn native_digests(&self) -> usize {
        self.poster
            .posted()
            .iter()
            .filter(|n| n.category == "hutan.digest")
            .count()
    }

    async fn next_digest(&mut self) -> NotificationIntent {
        wait_event(&mut self.events, "the digest", |event| match event {
            Event::NotifyIntent { intent } if intent.kind == IntentKind::Digest => {
                Some(intent.clone())
            }
            _ => None,
        })
        .await
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_digest_goes_out_once_when_the_user_is_away_and_a_request_waits() {
    let mut env = Env::start("[notify]\ndigest_hours = 1\n").await;
    let desktop = env.desktop.clone();

    // The window is the active one: the user is here, whatever waits.
    let (here, asked_here) = env.a_request_waits(&desktop).await;
    sleep(SEVERAL_PERIODS).await;
    assert!(env.digests().await.is_empty());
    assert_eq!(env.native_digests(), 0);
    env.refuse(&desktop, &here, asked_here).await;

    // The user leaves and a request is made while they are away: after the period, one digest
    // with the summary line, for Mawas Home.
    env.leave().await;
    let (approval, asked) = env.a_request_waits(&desktop).await;
    let digest = env.next_digest().await;
    assert_eq!(digest.session_id, env.mawas);
    assert_eq!(digest.title, "While you were away");
    assert!(
        digest.body.contains("1 request waits for you"),
        "{}",
        digest.body
    );
    let offered: Vec<&str> = digest.actions.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(offered, [ACTION_OPEN]);
    assert_eq!(env.digests().await, vec![digest.clone()]);
    let native = env
        .poster
        .posted()
        .into_iter()
        .find(|n| n.category == "hutan.digest")
        .expect("a native notification");
    assert_eq!(native.body, digest.body);
    assert!(!native.sound, "the digest is a quiet banner");

    // At most one per period, and the same request is not news again.
    sleep(SEVERAL_PERIODS).await;
    assert_eq!(env.native_digests(), 1);
    assert_eq!(env.digests().await.len(), 1);

    // Its *Open* asks the window for Mawas Home on the Recent tab.
    let acted = env
        .desktop
        .notify_act(&digest.id, ACTION_OPEN)
        .await
        .unwrap();
    assert_eq!(acted.route, ActRoute::UiOpen);
    assert_eq!(acted.desktop_clients, 1);
    let (session, view) = wait_event(&mut env.events, "ui.open", |event| match event {
        Event::UiOpen {
            session_id, view, ..
        } => Some((*session_id, *view)),
        _ => None,
    })
    .await;
    assert_eq!(session, env.mawas);
    assert_eq!(view, Some(UiView::MawasRecent));

    env.refuse(&desktop, &approval, asked).await;
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_that_needs_the_user_is_no_notification_and_then_something_is() {
    let mut env = Env::start("[notify]\ndigest_hours = 1\n").await;
    let desktop = env.desktop.clone();
    env.leave().await;

    // Away for several periods and nothing waits: silence.
    sleep(SEVERAL_PERIODS).await;
    assert!(env.digests().await.is_empty());
    assert_eq!(env.native_digests(), 0);

    // A request arrives while away: the next look says so.
    let (approval, asked) = env.a_request_waits(&desktop).await;
    let digest = env.next_digest().await;
    assert!(
        digest.body.contains("1 request waits for you"),
        "{}",
        digest.body
    );
    assert_eq!(env.native_digests(), 1);
    env.refuse(&desktop, &approval, asked).await;
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_digest_is_off_by_default_and_a_live_setting_turns_it_on() {
    let mut env = Env::start("").await;
    let desktop = env.desktop.clone();
    env.leave().await;
    let (approval, asked) = env.a_request_waits(&desktop).await;

    // Off: the user is away, a request waits, and nothing is sent.
    sleep(SEVERAL_PERIODS).await;
    assert!(env.digests().await.is_empty());
    assert_eq!(env.native_digests(), 0);

    // `notify.digest_hours` is live: no restart.
    env.desktop
        .daemon_setting_set("notify.digest_hours", Some(serde_json::json!(1)))
        .await
        .unwrap();
    let digest = env.next_digest().await;
    assert!(digest.body.contains("1 request waits for you"));
    assert_eq!(env.native_digests(), 1);

    // And off again: a later period sends nothing more.
    env.desktop
        .daemon_setting_set("notify.digest_hours", Some(serde_json::json!(0)))
        .await
        .unwrap();
    sleep(SEVERAL_PERIODS).await;
    assert_eq!(env.native_digests(), 1);
    env.refuse(&desktop, &approval, asked).await;
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_closes_while_active_is_the_user_leaving() {
    let env = Env::start("[notify]\ndigest_hours = 1\n").await;
    let Env {
        desktop,
        daemon,
        poster,
        mawas,
        events: _,
    } = env;
    // The window is active and closes: nobody is connected as a desktop any more.
    drop(desktop);
    let other = daemon.connect_as(ClientKind::Desktop).await;
    let mut events = other.events();
    other
        .call::<hutan_protocol::messages::Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();
    let env = Env {
        desktop: other.clone(),
        daemon,
        poster,
        mawas,
        events: other.events(),
    };
    let (approval, asked) = env.a_request_waits(&other).await;
    let digest = wait_event(&mut events, "the digest", |event| match event {
        Event::NotifyIntent { intent } if intent.kind == IntentKind::Digest => Some(intent.clone()),
        _ => None,
    })
    .await;
    assert_eq!(digest.session_id, env.mawas);
    assert_eq!(env.native_digests(), 1);
    env.refuse(&other, &approval, asked).await;
    env.stop().await;
}
