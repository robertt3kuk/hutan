//! The agent control surface (protocol 0.21, ADR 0016) against an in-process daemon: `ui.run`
//! and `ui.set_setting` routed to a fake desktop, who counts as an agent, the `[control]`
//! policy, the `control` approvals and their audit rows.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::time::Duration;

use common::*;
use hutan_client::{Client, ClientError};
use hutan_core::{ApprovalId, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond};
use hutan_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, ClientKind, Empty, Event,
    Origin, Request, SessionInfo, SessionRename, SessionResult,
};
use hutan_protocol::ui::{
    AppearanceSummary, DockSummary, FocusSummary, LayoutSummary, PaneItem, PaneSummary,
    SET_SETTING_ACTION, UiAction, UiActionList, UiCommandResult, UiLayoutResult, UiPublish, UiRan,
    UiRun, UiSetSetting, UiSetting, UiTarget,
};
use hutan_storage::{ApprovalRow, Storage};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

// ---- helpers ----------------------------------------------------------------------------------

/// A connection that says it runs inside `session` (`HUTAN_SESSION_ID`).
async fn connect_from(t: &TestDaemon, session: SessionId) -> Client {
    let mut options = t.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    Client::connect(options).await.unwrap()
}

/// An agent tells the daemon it runs in `session` (what its hooks do).
async fn report_agent(t: &TestDaemon, session: SessionId) {
    let client = t.connect().await;
    let applied: AgentApplied = client
        .call(Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            pid: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
}

async fn release_agent(t: &TestDaemon, session: SessionId) {
    let client = t.connect().await;
    let applied: AgentApplied = client
        .call(Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: "test-hooks".into(),
            seq: 2,
            node: None,
            outcome: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
}

fn daemon_toml(agents: &str) -> String {
    format!("[control]\nagents = \"{agents}\"\napproval_timeout_secs = 20\n")
}

async fn shell(t: &TestDaemon) -> SessionInfo {
    let client = t.connect().await;
    create(&client, sh("sleep 60")).await
}

async fn rename(
    client: &Client,
    session: SessionId,
    title: &str,
) -> Result<SessionInfo, ClientError> {
    client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: session,
            title: title.to_owned(),
        }))
        .await
        .map(|r| r.session)
}

async fn title_of(t: &TestDaemon, session: SessionId) -> String {
    let client = t.connect().await;
    let list: hutan_protocol::messages::SessionList = client
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    list.sessions
        .into_iter()
        .find(|s| s.id == session)
        .unwrap()
        .title
}

async fn pending(user: &Client) -> Vec<ApprovalInfo> {
    user.call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest::default()))
        .await
        .unwrap()
        .approvals
}

/// Waits until a control approval is pending and returns it.
async fn next_approval(user: &Client) -> ApprovalInfo {
    timeout(WAIT, async {
        loop {
            if let Some(info) = pending(user).await.into_iter().next() {
                return info;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a control approval never appeared")
}

async fn answer(user: &Client, approval: ApprovalId, option: &str) {
    user.call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
        approval_id: approval,
        option_id: option.to_owned(),
    }))
    .await
    .unwrap();
}

/// The audit rows of `kind = control`, once `count` of them exist.
async fn audit(t: &TestDaemon, count: usize) -> Vec<ApprovalRow> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let storage = Storage::open(&t.paths.database).unwrap();
        let rows: Vec<ApprovalRow> = storage
            .approvals()
            .list(None)
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == "control")
            .collect();
        storage.close();
        if rows.len() >= count && rows.iter().all(|r| r.outcome.is_some()) {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} resolved control rows, have {rows:?}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

fn layout(window: &str, preset: &str, active: bool) -> LayoutSummary {
    LayoutSummary {
        window_id: window.to_owned(),
        workspace_id: None,
        preset: preset.to_owned(),
        docks: vec![DockSummary {
            position: "left".into(),
            visible: true,
            panels: vec!["sessions".into()],
            active: Some("sessions".into()),
        }],
        panes: PaneSummary::Pane {
            pane_id: "pane-1".into(),
            items: vec![PaneItem {
                kind: "placeholder".into(),
                session_id: None,
                panel: None,
                path: None,
                title: "Welcome".into(),
            }],
            active: Some(0),
        },
        focused: FocusSummary {
            kind: "pane".into(),
            target: "pane-1".into(),
        },
        zoomed: None,
        appearance: AppearanceSummary {
            chrome: "system".into(),
            density: "compact".into(),
        },
        window_active: active,
    }
}

fn publish_of(window: &str, preset: &str) -> UiPublish {
    UiPublish {
        actions: vec![
            UiAction {
                id: "workbench::SplitRight".into(),
                title: "Layout: Split Right".into(),
                args_schema: None,
            },
            UiAction {
                id: "workbench::ApplyPreset".into(),
                title: "Layout: Apply Preset".into(),
                args_schema: Some(json!({
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"],
                })),
            },
        ],
        settings: vec![UiSetting {
            namespace: "terminal".into(),
            key: "font_size".into(),
            kind: "integer".into(),
            doc: "Font size".into(),
            layers: vec!["user".into()],
            values: Vec::new(),
            current: Some(json!(13)),
        }],
        layout: layout(window, preset, true),
    }
}

/// A desktop that published its catalog and answers `ui.command` events by `reply`.
struct FakeDesktop {
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
}

impl FakeDesktop {
    async fn start(t: &TestDaemon, window: &str) -> Self {
        let (client, events) = t.connect_subscribed().await;
        client
            .call::<Empty>(Request::UiPublish(publish_of(window, "default")))
            .await
            .unwrap();
        Self { client, events }
    }

    /// The next `ui.command` event: (request id, action, args).
    async fn next_command(&mut self) -> (u64, String, Value) {
        wait_event(&mut self.events, "a ui.command", |event| match event {
            Event::UiCommand {
                request_id,
                action,
                args,
                ..
            } => Some((*request_id, action.clone(), args.clone())),
            _ => None,
        })
        .await
    }

    async fn reply_ok(&self, request_id: u64, ok: Value) {
        self.client
            .call::<Empty>(Request::UiCommandResult(UiCommandResult {
                request_id,
                ok: Some(ok),
                error: None,
            }))
            .await
            .unwrap();
    }

    async fn reply_err(&self, request_id: u64, code: ErrorCode, message: &str) {
        self.client
            .call::<Empty>(Request::UiCommandResult(UiCommandResult {
                request_id,
                ok: None,
                error: Some(hutan_protocol::control::ErrorBody::new(code, message)),
            }))
            .await
            .unwrap();
    }
}

fn run_request(action: &str, args: Option<Value>) -> Request {
    Request::UiRun(UiRun {
        window_id: None,
        action: action.to_owned(),
        args,
    })
}

// ---- ui.run / ui.set_setting routing --------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ui_requests_reach_the_desktop_and_its_answer_returns() {
    let t = TestDaemon::start().await;
    let mut desktop = FakeDesktop::start(&t, "w1").await;
    let caller = t.connect().await;

    // The catalog and layout the desktop published are served from the cache.
    let actions: UiActionList = caller
        .call(Request::UiActions(UiTarget::default()))
        .await
        .unwrap();
    assert_eq!(actions.window_id, "w1");
    assert_eq!(actions.actions.len(), 2);
    assert_eq!(actions.settings[0].key, "font_size");
    let shown: UiLayoutResult = caller
        .call(Request::UiLayout(UiTarget {
            window_id: Some("w1".into()),
        }))
        .await
        .unwrap();
    assert_eq!(shown.layout.preset, "default");

    // A later publish replaces the cached layout.
    desktop
        .client
        .call::<Empty>(Request::UiPublish(publish_of("w1", "focus")))
        .await
        .unwrap();
    let shown: UiLayoutResult = caller
        .call(Request::UiLayout(UiTarget::default()))
        .await
        .unwrap();
    assert_eq!(shown.layout.preset, "focus");

    // `ui.run`: the desktop receives the command and its answer is the caller's result.
    let run = tokio::spawn({
        let caller = caller.clone();
        async move {
            caller
                .call::<UiRan>(run_request(
                    "workbench::ApplyPreset",
                    Some(json!({"name": "focus"})),
                ))
                .await
        }
    });
    let (id, action, args) = desktop.next_command().await;
    assert_eq!(action, "workbench::ApplyPreset");
    assert_eq!(args, json!({"name": "focus"}));
    desktop.reply_ok(id, json!({"ran": action})).await;
    let ran = run.await.unwrap().unwrap();
    assert_eq!(ran.window_id, "w1");
    assert_eq!(ran.result, json!({"ran": "workbench::ApplyPreset"}));

    // The desktop's error comes back as the caller's error.
    let run = tokio::spawn({
        let caller = caller.clone();
        async move {
            caller
                .call::<UiRan>(run_request("workbench::SplitRight", None))
                .await
        }
    });
    let (id, _, args) = desktop.next_command().await;
    assert_eq!(args, json!({}), "no arguments are an empty object");
    desktop
        .reply_err(id, ErrorCode::InvalidState, "nothing to split")
        .await;
    match run.await.unwrap() {
        Err(ClientError::Remote(error)) => {
            assert_eq!(error.code, ErrorCode::InvalidState);
            assert!(error.message.contains("nothing to split"), "{error:?}");
        }
        other => panic!("expected the desktop's error, got {other:?}"),
    }

    // `ui.set_setting` travels as the reserved action with the whole request as arguments.
    let set = tokio::spawn({
        let caller = caller.clone();
        async move {
            caller
                .call::<UiRan>(Request::UiSetSetting(UiSetSetting {
                    window_id: None,
                    namespace: "terminal".into(),
                    key: "font_size".into(),
                    value: json!(15),
                    layer: "user".into(),
                }))
                .await
        }
    });
    let (id, action, args) = desktop.next_command().await;
    assert_eq!(action, SET_SETTING_ACTION);
    assert_eq!(
        args,
        json!({"namespace": "terminal", "key": "font_size", "value": 15, "layer": "user"})
    );
    desktop.reply_ok(id, json!({"value": 15})).await;
    assert_eq!(set.await.unwrap().unwrap().result, json!({"value": 15}));

    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_requests_name_what_is_wrong() {
    let t = TestDaemon::start().await;
    let caller = t.connect().await;

    // No window.
    for request in [
        Request::UiActions(UiTarget::default()),
        Request::UiLayout(UiTarget::default()),
        run_request("workbench::SplitRight", None),
    ] {
        match caller.call::<Value>(request).await {
            Err(ClientError::Remote(error)) => {
                assert_eq!(error.code, ErrorCode::InvalidState);
                assert!(error.message.contains("window"), "{error:?}");
            }
            other => panic!("expected invalid_state, got {other:?}"),
        }
    }

    let _desktop = FakeDesktop::start(&t, "w1").await;
    let unknown = caller
        .call::<UiRan>(run_request("workbench::Nope", None))
        .await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    let unknown_window = caller
        .call::<UiLayoutResult>(Request::UiLayout(UiTarget {
            window_id: Some("w9".into()),
        }))
        .await;
    assert_eq!(remote_code(unknown_window), ErrorCode::NotFound);
    let bad_args = caller
        .call::<UiRan>(run_request(
            "workbench::ApplyPreset",
            Some(json!(["focus"])),
        ))
        .await;
    assert_eq!(remote_code(bad_args), ErrorCode::BadRequest);

    let set = |namespace: &str, key: &str, layer: &str| {
        Request::UiSetSetting(UiSetSetting {
            window_id: None,
            namespace: namespace.into(),
            key: key.into(),
            value: json!(15),
            layer: layer.into(),
        })
    };
    assert_eq!(
        remote_code(caller.call::<UiRan>(set("terminal", "nope", "user")).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            caller
                .call::<UiRan>(set("terminal", "font_size", "workspace"))
                .await
        ),
        ErrorCode::BadRequest,
        "a layer the key does not take"
    );

    // A client that is not a window cannot answer commands it was never sent.
    let stray = caller
        .call::<Empty>(Request::UiCommandResult(UiCommandResult {
            request_id: 4242,
            ok: Some(json!({})),
            error: None,
        }))
        .await;
    assert_eq!(remote_code(stray), ErrorCode::NotFound);

    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_does_not_answer_times_out() {
    let t = TestDaemon::start_with("", |config| {
        config.ui_command_timeout = Duration::from_millis(300);
    })
    .await;
    let mut desktop = FakeDesktop::start(&t, "w1").await;
    let caller = t.connect().await;
    let run = tokio::spawn(async move {
        caller
            .call::<UiRan>(run_request("workbench::SplitRight", None))
            .await
    });
    let (late_id, _, _) = desktop.next_command().await;
    match run.await.unwrap() {
        Err(ClientError::Remote(error)) => {
            assert_eq!(error.code, ErrorCode::Internal);
            assert!(
                error.message.contains("did not answer"),
                "{}",
                error.message
            );
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    // The answer that comes too late has nobody to go to.
    let late = desktop
        .client
        .call::<Empty>(Request::UiCommandResult(UiCommandResult {
            request_id: late_id,
            ok: Some(json!({})),
            error: None,
        }))
        .await;
    assert_eq!(remote_code(late), ErrorCode::NotFound);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_most_recently_active_window_is_the_default_target() {
    let t = TestDaemon::start().await;
    let first = FakeDesktop::start(&t, "w1").await;
    let _second = FakeDesktop::start(&t, "w2").await;
    let caller = t.connect().await;
    let shown: UiLayoutResult = caller
        .call(Request::UiLayout(UiTarget::default()))
        .await
        .unwrap();
    assert_eq!(shown.layout.window_id, "w2", "published last");
    let w1: UiLayoutResult = caller
        .call(Request::UiLayout(UiTarget {
            window_id: Some("w1".into()),
        }))
        .await
        .unwrap();
    assert_eq!(w1.layout.window_id, "w1");

    // A window that goes away takes its publish with it.
    drop(first);
    drop(_second);
    timeout(WAIT, async {
        loop {
            if caller
                .call::<UiLayoutResult>(Request::UiLayout(UiTarget::default()))
                .await
                .is_err()
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the windows' publishes should be dropped with their connections");
    t.stop().await;
}

// ---- who is an agent --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_shell_is_the_user_and_an_agent_reported_session_is_gated() {
    let t = TestDaemon::start_with(&daemon_toml("read_only"), |_| {}).await;
    let session = shell(&t).await;
    let other = shell(&t).await;
    let user = t.connect().await;

    // `hutan rename` typed in a plain Hutan terminal: the user, so nothing is gated, even under
    // `read_only`.
    let from_shell = connect_from(&t, session.id).await;
    let renamed = rename(&from_shell, other.id, "by hand").await.unwrap();
    assert_eq!(renamed.title, "by hand");

    // An agent reports itself in that terminal: its next requests are an agent's.
    report_agent(&t, session.id).await;
    let from_agent = connect_from(&t, session.id).await;
    let refused = rename(&from_agent, other.id, "by agent").await;
    assert_eq!(remote_code(refused), ErrorCode::Forbidden);
    assert_eq!(title_of(&t, other.id).await, "by hand");

    // The check is made per request: the connection that was the user's before the report is an
    // agent's now, and the other way round once the agent releases itself.
    let refused = rename(&from_shell, other.id, "by agent again").await;
    assert_eq!(remote_code(refused), ErrorCode::Forbidden);
    release_agent(&t, session.id).await;
    assert_eq!(
        rename(&from_agent, other.id, "by hand again")
            .await
            .unwrap()
            .title,
        "by hand again"
    );

    // A claim of a session that does not exist is ignored (the user).
    let bogus = connect_from(&t, SessionId::new()).await;
    assert_eq!(
        rename(&bogus, other.id, "bogus").await.unwrap().title,
        "bogus"
    );

    // Reads are never gated, under any policy.
    report_agent_again(&t, session.id).await;
    let list: hutan_protocol::messages::SessionList = from_agent
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    assert!(list.sessions.iter().any(|s| s.id == session.id));
    let _ = user;
    t.stop().await;
}

/// The same agent reports again with a newer `seq`.
async fn report_agent_again(t: &TestDaemon, session: SessionId) {
    let client = t.connect().await;
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Idle),
            message: None,
            seq: 3,
            agent_session_id: None,
            resume_argv: None,
            pid: None,
            node: None,
        }))
        .await
        .unwrap();
}

// ---- the policy -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ask_raises_a_control_approval_the_user_answers() {
    let t = TestDaemon::start_with(&daemon_toml("ask"), |_| {}).await;
    let agent_session = shell(&t).await;
    let target = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let user = t.connect_as(ClientKind::Desktop).await;
    let agent = connect_from(&t, agent_session.id).await;

    // allow_once runs this request only.
    let first = tokio::spawn({
        let agent = agent.clone();
        let target = target.id;
        async move { rename(&agent, target, "once").await }
    });
    let approval = next_approval(&user).await;
    assert_eq!(
        approval.session_id, agent_session.id,
        "asked on behalf of the agent's session"
    );
    assert!(
        approval.title.contains("once"),
        "the title says what is asked: {}",
        approval.title
    );
    assert!(approval.title.contains(&target.title), "{}", approval.title);
    let options: Vec<&str> = approval.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(options, ["allow_once", "allow_session", "deny"]);
    assert_eq!(
        title_of(&t, target.id).await,
        target.title,
        "nothing ran before the answer"
    );
    answer(&user, approval.id, "allow_once").await;
    assert_eq!(first.await.unwrap().unwrap().title, "once");

    // Denied: the request is refused and the session keeps its title.
    let second = tokio::spawn({
        let agent = agent.clone();
        let target = target.id;
        async move { rename(&agent, target, "denied").await }
    });
    let approval = next_approval(&user).await;
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(second.await.unwrap()), ErrorCode::Forbidden);
    assert_eq!(title_of(&t, target.id).await, "once");

    // allow_session: the next request of the same kind does not ask again.
    let third = tokio::spawn({
        let agent = agent.clone();
        let target = target.id;
        async move { rename(&agent, target, "session").await }
    });
    let approval = next_approval(&user).await;
    answer(&user, approval.id, "allow_session").await;
    assert_eq!(third.await.unwrap().unwrap().title, "session");
    assert_eq!(
        rename(&agent, target.id, "again").await.unwrap().title,
        "again"
    );
    assert!(
        pending(&user).await.is_empty(),
        "no second approval was raised"
    );

    // The grant belongs to the session: another agent session still asks.
    let other_agent = shell(&t).await;
    report_agent(&t, other_agent.id).await;
    let other = connect_from(&t, other_agent.id).await;
    let fourth = tokio::spawn({
        let target = target.id;
        async move { rename(&other, target, "other").await }
    });
    let approval = next_approval(&user).await;
    assert_eq!(approval.session_id, other_agent.id);
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(fourth.await.unwrap()), ErrorCode::Forbidden);

    // Audit: every decision is a row of kind `control`.
    let rows = audit(&t, 5).await;
    let options: Vec<&str> = rows.iter().filter_map(|r| r.option_id.as_deref()).collect();
    assert_eq!(
        options,
        ["allow_once", "deny", "allow_session", "allow_grant", "deny"],
        "{rows:?}"
    );
    assert!(rows.iter().all(|r| r.kind == "control"));
    assert!(
        rows.iter()
            .filter(|r| r.decided_by.as_deref() == Some("desktop"))
            .count()
            >= 4,
        "the answers name the client that gave them: {rows:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unanswered_approval_expires_and_the_request_is_refused() {
    let t = TestDaemon::start_with(
        "[control]\nagents = \"ask\"\napproval_timeout_secs = 1\n",
        |_| {},
    )
    .await;
    let agent_session = shell(&t).await;
    let target = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let agent = connect_from(&t, agent_session.id).await;
    let result = rename(&agent, target.id, "never").await;
    assert_eq!(remote_code(result), ErrorCode::Forbidden);
    assert_eq!(title_of(&t, target.id).await, target.title);
    let rows = audit(&t, 1).await;
    assert_eq!(
        rows[0].outcome,
        Some(hutan_storage::ApprovalOutcome::Expired)
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_refuses_mutations_and_allow_runs_them_and_both_are_audited() {
    // read_only
    let t = TestDaemon::start_with(&daemon_toml("read_only"), |_| {}).await;
    let agent_session = shell(&t).await;
    let target = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let agent = connect_from(&t, agent_session.id).await;
    let refused = rename(&agent, target.id, "no").await;
    match refused {
        Err(ClientError::Remote(error)) => {
            assert_eq!(error.code, ErrorCode::Forbidden);
            assert!(error.message.contains("read-only"), "{}", error.message);
        }
        other => panic!("expected forbidden, got {other:?}"),
    }
    assert_eq!(title_of(&t, target.id).await, target.title);
    let rows = audit(&t, 1).await;
    assert_eq!(rows[0].option_id.as_deref(), Some("deny_policy"));
    assert_eq!(rows[0].session_id, agent_session.id);
    t.stop().await;

    // allow
    let t = TestDaemon::start_with(&daemon_toml("allow"), |_| {}).await;
    let agent_session = shell(&t).await;
    let target = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let agent = connect_from(&t, agent_session.id).await;
    assert_eq!(rename(&agent, target.id, "yes").await.unwrap().title, "yes");
    let rows = audit(&t, 1).await;
    assert_eq!(rows[0].option_id.as_deref(), Some("allow_policy"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_only_the_user_may_make_are_refused_to_agents_under_every_policy() {
    for policy in ["ask", "allow", "read_only"] {
        let t = TestDaemon::start_with(&daemon_toml(policy), |_| {}).await;
        let agent_session = shell(&t).await;
        report_agent(&t, agent_session.id).await;
        let agent = connect_from(&t, agent_session.id).await;
        // An agent does not answer approvals, and does not impersonate a window.
        let respond = agent
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: ApprovalId::new(),
                option_id: "allow_once".into(),
            }))
            .await;
        assert_eq!(remote_code(respond), ErrorCode::Forbidden, "{policy}");
        let publish = agent
            .call::<Empty>(Request::UiPublish(publish_of("evil", "default")))
            .await;
        assert_eq!(remote_code(publish), ErrorCode::Forbidden, "{policy}");
        t.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_run_asks_per_action_and_grants_cover_one_action_only() {
    let t = TestDaemon::start_with(&daemon_toml("ask"), |_| {}).await;
    let agent_session = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let mut desktop = FakeDesktop::start(&t, "w1").await;
    let user = t.connect_as(ClientKind::Notifier).await;
    let agent = connect_from(&t, agent_session.id).await;

    let run = |action: &'static str| {
        let agent = agent.clone();
        tokio::spawn(async move { agent.call::<UiRan>(run_request(action, None)).await })
    };

    let first = run("workbench::SplitRight");
    let approval = next_approval(&user).await;
    assert!(
        approval.title.contains("workbench::SplitRight"),
        "{}",
        approval.title
    );
    answer(&user, approval.id, "allow_session").await;
    let (id, _, _) = desktop.next_command().await;
    desktop.reply_ok(id, json!({})).await;
    let ran = first.await.unwrap().unwrap();
    assert_eq!(ran.window_id, "w1");

    // The same action runs without asking; a different action asks again.
    let again = run("workbench::SplitRight");
    let (id, _, _) = desktop.next_command().await;
    desktop.reply_ok(id, json!({})).await;
    again.await.unwrap().unwrap();
    assert!(pending(&user).await.is_empty());

    let other = run("workbench::ApplyPreset");
    let approval = next_approval(&user).await;
    assert!(approval.title.contains("workbench::ApplyPreset"));
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(other.await.unwrap()), ErrorCode::Forbidden);
    t.stop().await;
}

// ---- worktree review and merge (protocol 0.22) -------------------------------------------------

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_a_worktree_diff_freely_and_a_merge_waits_for_the_user() {
    use hutan_protocol::messages::{WorkspaceOpen, WorkspaceOpened};
    use hutan_protocol::worktree::{
        MergeStrategy, WorktreeCreate, WorktreeDiffRequest, WorktreeMerge,
    };

    let t = TestDaemon::start_with(&daemon_toml("ask"), |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", k, v]);
    }
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);

    let agent_session = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let user = t.connect_as(ClientKind::Desktop).await;
    let agent = connect_from(&t, agent_session.id).await;
    let workspace = user
        .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
            path: root.clone(),
            created_by: None,
        }))
        .await
        .unwrap()
        .workspace
        .id;
    let wt = user
        .worktree_create(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/agent".into(),
            base: None,
            path: None,
            no_setup: false,
        })
        .await
        .unwrap();
    std::fs::write(wt.path.join("b.txt"), "b\n").unwrap();
    git(&wt.path, &["add", "-A"]);
    git(&wt.path, &["commit", "-q", "-m", "work"]);

    // Reading what the worktree changed needs no approval.
    let diff = agent
        .worktree_diff(WorktreeDiffRequest {
            path: wt.path.clone(),
            base: None,
            include_uncommitted: true,
        })
        .await
        .unwrap();
    assert_eq!(diff.files.len(), 1);
    assert!(pending(&user).await.is_empty());

    // Merging asks, names what it does, and runs once the user allowed it.
    let merging = tokio::spawn({
        let agent = agent.clone();
        let path = wt.path.clone();
        async move {
            agent
                .worktree_merge(WorktreeMerge {
                    path,
                    into: None,
                    strategy: MergeStrategy::Squash,
                    message: Some("Add b".into()),
                })
                .await
        }
    });
    let approval = next_approval(&user).await;
    assert_eq!(approval.session_id, agent_session.id);
    assert!(
        approval.title.contains("squash-merge") && approval.title.contains("feature-agent"),
        "{}",
        approval.title
    );
    assert!(
        !root.join("b.txt").exists(),
        "nothing ran before the answer"
    );
    answer(&user, approval.id, "allow_once").await;
    let merged = merging.await.unwrap().unwrap();
    assert_eq!(merged.target, "main");
    assert!(root.join("b.txt").is_file());

    // A denied merge changes nothing.
    std::fs::write(wt.path.join("c.txt"), "c\n").unwrap();
    git(&wt.path, &["add", "-A"]);
    git(&wt.path, &["commit", "-q", "-m", "more"]);
    let denied = tokio::spawn({
        let agent = agent.clone();
        let path = wt.path.clone();
        async move {
            agent
                .worktree_merge(WorktreeMerge {
                    path,
                    into: None,
                    strategy: MergeStrategy::Merge,
                    message: None,
                })
                .await
        }
    });
    let approval = next_approval(&user).await;
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(denied.await.unwrap()), ErrorCode::Forbidden);
    assert!(!root.join("c.txt").exists());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_review_of_a_task_waits_for_the_user_and_is_recorded_as_the_agents() {
    use hutan_core::{DeliveryActor, DeliveryState, ReviewDecision, TaskState};
    use hutan_protocol::messages::{WorkspaceOpen, WorkspaceOpened};
    use hutan_protocol::task::{TaskCreate, TaskUpdate};
    use hutan_protocol::worktree::WorktreeCommit;

    let t = TestDaemon::start_with(&daemon_toml("ask"), |_| {}).await;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let agent_session = shell(&t).await;
    report_agent(&t, agent_session.id).await;
    let user = t.connect_as(ClientKind::Desktop).await;
    let agent = connect_from(&t, agent_session.id).await;
    let workspace = user
        .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
            path: root,
            created_by: None,
        }))
        .await
        .unwrap()
        .workspace
        .id;
    // A task that is done because its owner says so: nothing awaits review yet.
    let task = user
        .task_create(TaskCreate {
            workspace_id: workspace,
            title: "Write the docs".into(),
            prompt: None,
            depends_on: Vec::new(),
            placement: None,
            run: None,
            auto_start: false,
            after_policy: Default::default(),
        })
        .await
        .unwrap();
    user.task_start(task.id).await.unwrap();
    user.task_update(TaskUpdate {
        id: task.id,
        title: None,
        prompt: None,
        depends_on: None,
        placement: None,
        run: None,
        auto_start: None,
        after_policy: None,
        state: Some(TaskState::Done),
    })
    .await
    .unwrap();

    // Reading tasks is free; accepting asks, says what it does, and runs once allowed.
    assert_eq!(agent.task_list(None, None).await.unwrap().len(), 1);
    assert!(pending(&user).await.is_empty());
    let reviewing = tokio::spawn({
        let agent = agent.clone();
        async move {
            agent
                .task_review(task.id, ReviewDecision::Accept, Some("looks fine".into()))
                .await
        }
    });
    let approval = next_approval(&user).await;
    assert_eq!(approval.session_id, agent_session.id);
    assert!(
        approval.title.contains("accept the work") && approval.title.contains("looks fine"),
        "{}",
        approval.title
    );
    let unchanged = &user.task_list(None, None).await.unwrap()[0];
    assert_eq!(
        unchanged.delivery,
        DeliveryState::None,
        "nothing ran before the answer"
    );
    answer(&user, approval.id, "allow_once").await;
    let reviewed = reviewing.await.unwrap().unwrap();
    assert_eq!(reviewed.task.delivery, DeliveryState::Accepted);
    let record = reviewed.task.delivery_history.last().unwrap();
    assert_eq!(record.by, DeliveryActor::Agent, "recorded as the agent's");
    assert_eq!(record.note.as_deref(), Some("looks fine"));

    // A denied change request changes nothing.
    let denied = tokio::spawn({
        let agent = agent.clone();
        async move {
            agent
                .task_review(task.id, ReviewDecision::RequestChanges, Some("no".into()))
                .await
        }
    });
    let approval = next_approval(&user).await;
    assert!(
        approval.title.contains("request changes"),
        "{}",
        approval.title
    );
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(denied.await.unwrap()), ErrorCode::Forbidden);
    assert_eq!(
        user.task_list(None, None).await.unwrap()[0].delivery,
        DeliveryState::Accepted
    );

    // The user's own review needs no approval and is the desktop's.
    let by_user = user
        .task_review(task.id, ReviewDecision::RequestChanges, None)
        .await
        .unwrap();
    assert_eq!(
        by_user.task.delivery_history.last().unwrap().by,
        DeliveryActor::Desktop
    );

    // Committing in a worktree asks too: it decides what a merge takes.
    let committing = tokio::spawn({
        let agent = agent.clone();
        async move {
            agent
                .worktree_commit(WorktreeCommit {
                    path: "/nowhere".into(),
                    message: "x".into(),
                })
                .await
        }
    });
    let approval = next_approval(&user).await;
    assert!(
        approval.title.contains("commit everything"),
        "{}",
        approval.title
    );
    answer(&user, approval.id, "deny").await;
    assert_eq!(remote_code(committing.await.unwrap()), ErrorCode::Forbidden);
    t.stop().await;
}
