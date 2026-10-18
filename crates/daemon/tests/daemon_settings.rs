//! `daemon.settings.get` / `.set` and `assistant.reset` (protocol 0.28) against an in-process
//! daemon on a temporary `HUTAN_HOME`: the defaults of an empty home, live and restart settings,
//! reset, refused values, a hand-broken file, agent-originated connections, and the assistant's
//! stored choice. Nothing here starts a harness, reads a login or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{TestDaemon, create, remote_code, sh, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::{SessionId, SessionState};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{
    AssistantBackend, AssistantConfigure, AssistantOpen, AssistantScope,
};
use hutan_protocol::daemon_settings::{Applies, DaemonSetting, DaemonSettingKind, DaemonSettings};
use hutan_protocol::history::HistoryStatusInfo;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Origin, Request, SessionInfo,
    SessionList, SessionRename, SessionResult, WorkspaceCreate, WorkspaceInfo,
};
use serde_json::{Value, json};
use tokio::time::sleep;

fn entry<'a>(settings: &'a DaemonSettings, key: &str) -> &'a DaemonSetting {
    settings
        .entries
        .iter()
        .find(|e| e.key == key)
        .unwrap_or_else(|| panic!("no setting `{key}`"))
}

async fn set(client: &Client, key: &str, value: Value) -> DaemonSettings {
    client.daemon_setting_set(key, Some(value)).await.unwrap()
}

async fn reset(client: &Client, key: &str) -> DaemonSettings {
    client.daemon_setting_set(key, None).await.unwrap()
}

/// What the daemon says about a refused change.
async fn refused(client: &Client, key: &str, value: Value) -> (ErrorCode, String) {
    match client.daemon_setting_set(key, Some(value)).await {
        Err(ClientError::Remote(body)) => (body.code, body.message),
        other => panic!("`{key}` was not refused: {other:?}"),
    }
}

fn file(t: &TestDaemon) -> String {
    std::fs::read_to_string(&t.paths.config_file).unwrap_or_default()
}

// ---- the list ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_home_lists_every_setting_at_its_default() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    assert!(client.supports_daemon_settings());
    let settings = client.daemon_settings().await.unwrap();

    assert_eq!(settings.file, t.paths.config_file.to_string_lossy());
    assert!(!settings.restart_pending);
    assert!(settings.entries.len() > 40, "{}", settings.entries.len());
    for e in &settings.entries {
        assert!(!e.set_in_file, "{}", e.key);
        assert!(!e.pending_restart, "{}", e.key);
        assert_eq!(e.value, e.default, "{}", e.key);
    }
    assert!(
        settings.entries.iter().all(|e| !e.key.starts_with("mcp.")),
        "no MCP server is configured"
    );

    let notify = entry(&settings, "notify.enabled");
    assert_eq!(notify.kind, DaemonSettingKind::Bool);
    assert_eq!(notify.value, json!(true));
    assert_eq!(notify.applies, Applies::Live);
    let restore = entry(&settings, "daemon.restore_sessions");
    assert_eq!(restore.value, json!("respawn"));
    assert_eq!(restore.applies, Applies::Restart);
    assert_eq!(
        restore.kind,
        DaemonSettingKind::Enum {
            choices: vec!["respawn".into(), "stopped".into()]
        }
    );
    assert_eq!(
        entry(&settings, "assistant.global_context.files").value,
        json!(false)
    );
    assert_eq!(entry(&settings, "tasks.max_parallel").value, json!(3));
    assert_eq!(
        entry(&settings, "history.range_days").kind,
        DaemonSettingKind::Int {
            min: Some(0),
            max: Some(90)
        }
    );
    assert_eq!(
        entry(&settings, "harness.omp.env").kind,
        DaemonSettingKind::StringMap
    );
    assert_eq!(
        entry(&settings, "control.agents").value,
        json!("ask"),
        "the policy of a daemon nobody configured"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_file_is_listed_with_what_it_sets_and_mcp_servers_read_only() {
    let t = TestDaemon::start_with(
        "[tasks]\nmax_parallel = 2\n[mcp.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs\"]\n",
        |_| {},
    )
    .await;
    let client = t.connect().await;
    let settings = client.daemon_settings().await.unwrap();
    let tasks = entry(&settings, "tasks.max_parallel");
    assert_eq!(tasks.value, json!(2));
    assert_eq!(tasks.default, json!(3));
    assert!(tasks.set_in_file && !tasks.pending_restart);
    let docs = entry(&settings, "mcp.docs");
    assert_eq!(docs.kind, DaemonSettingKind::Table);
    assert_eq!(
        docs.value,
        json!({"command": "npx", "args": ["-y", "docs"]})
    );
    assert!(!docs.pending_restart);

    let (code, message) = refused(&client, "mcp.docs", json!({"command": "x"})).await;
    assert_eq!(code, ErrorCode::BadRequest);
    assert!(message.contains("daemon.toml"), "{message}");
    assert_eq!(
        remote_code(client.daemon_setting_set("mcp.docs", None).await),
        ErrorCode::BadRequest
    );
    t.stop().await;
}

// ---- live settings ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_live_setting_takes_effect_at_once_and_is_written_with_the_comments_kept() {
    let t = TestDaemon::start_with(
        "# my daemon\n[notify]\nenabled = true  # the master switch\nmin_turn_secs = 20\n",
        |_| {},
    )
    .await;
    let client = t.connect().await;
    let (_subscriber, mut events) = t.connect_subscribed().await;

    // With the default an exiting session raises an intent.
    create(&client, sh("exit 3")).await;
    let intent = wait_event(&mut events, "the first intent", |event| match event {
        Event::NotifyIntent { intent } => Some(intent.clone()),
        _ => None,
    })
    .await;
    assert_eq!(client.notify_list().await.unwrap().len(), 1);

    let settings = set(&client, "notify.enabled", json!(false)).await;
    let notify = entry(&settings, "notify.enabled");
    assert_eq!(notify.value, json!(false));
    assert!(notify.set_in_file);
    assert!(!notify.pending_restart, "a live setting is in use at once");
    assert!(!settings.restart_pending);
    assert_eq!(
        file(&t),
        "# my daemon\n[notify]\nenabled = false  # the master switch\nmin_turn_secs = 20\n"
    );

    // Now the same thing raises nothing.
    let second = create(&client, sh("exit 3")).await;
    wait_state(&client, second.id).await;
    sleep(Duration::from_millis(300)).await;
    let pending = client.notify_list().await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].id, intent.id);

    // Back on: removing the key restores the default and the next failure is raised again.
    let settings = reset(&client, "notify.enabled").await;
    assert_eq!(entry(&settings, "notify.enabled").value, json!(true));
    assert!(!entry(&settings, "notify.enabled").set_in_file);
    assert_eq!(
        file(&t),
        "# my daemon\n[notify]\nmin_turn_secs = 20\n",
        "the key goes, the comment above the table and the other key stay"
    );
    create(&client, sh("exit 3")).await;
    wait_event(&mut events, "an intent again", |event| match event {
        Event::NotifyIntent { intent: i } if i.id != intent.id => Some(()),
        _ => None,
    })
    .await;
    t.stop().await;
}

async fn wait_state(client: &Client, id: SessionId) {
    for _ in 0..200 {
        let list: SessionList = client
            .call(Request::SessionList(Default::default()))
            .await
            .unwrap();
        if list
            .sessions
            .iter()
            .any(|s| s.id == id && !s.state.is_live())
        {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("session {id} never ended");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_other_live_settings_are_in_use_when_the_answer_arrives() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    for (key, value) in [
        ("tasks.max_parallel", json!(1)),
        ("gateway.max_context_chars", json!(5000)),
        ("control.agents", json!("read_only")),
        ("control.approval_timeout_secs", json!(30)),
        ("assistant.context.files", json!(false)),
        ("assistant.global_context.files", json!(true)),
        ("assistant.context_budget_chars", json!(1234)),
        ("assistant.provider", json!("somewhere")),
        ("notify.kinds", json!(["task_failed"])),
        ("notify.min_turn_secs", json!(0)),
        ("notify.expire_secs", json!(60)),
    ] {
        let settings = set(&client, key, value.clone()).await;
        let e = entry(&settings, key);
        assert_eq!(e.value, value, "{key}");
        assert_eq!(e.applies, Applies::Live, "{key}");
        assert!(e.set_in_file, "{key}");
        assert!(
            !e.pending_restart,
            "{key}: the service did not take the new value"
        );
        assert!(!settings.restart_pending, "{key}");
    }
    let text = file(&t);
    for line in [
        "[tasks]\nmax_parallel = 1",
        "max_context_chars = 5000",
        "agents = \"read_only\"",
        "kinds = [\"task_failed\"]",
    ] {
        assert!(text.contains(line), "{line} missing from\n{text}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_follows_its_settings_without_a_restart() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    for (source, dir) in ["claude", "codex", "omp"].into_iter().zip(&dirs) {
        let settings = set(
            &client,
            &format!("history.roots.{source}"),
            json!(dir.path().to_string_lossy()),
        )
        .await;
        assert!(!settings.restart_pending);
    }
    let status = |client: &Client| {
        let client = client.clone();
        async move {
            client
                .call::<HistoryStatusInfo>(Request::HistoryStatus(Empty {}))
                .await
                .unwrap()
        }
    };
    assert!(!status(&client).await.enabled);

    let settings = set(&client, "history.enabled", json!(true)).await;
    assert!(!entry(&settings, "history.enabled").pending_restart);
    assert!(status(&client).await.enabled);
    let settings = set(&client, "history.range_days", json!(30)).await;
    assert!(!settings.restart_pending);
    assert!(
        status(&client).await.enabled,
        "still indexing after a change"
    );

    let settings = set(&client, "history.enabled", json!(false)).await;
    assert!(!settings.restart_pending);
    assert!(!status(&client).await.enabled);
    assert!(file(&t).contains("enabled = false"));

    // `history.set_enabled` and the settings agree about what runs.
    client
        .call::<HistoryStatusInfo>(Request::HistorySetEnabled(
            hutan_protocol::history::HistorySetEnabled { enabled: true },
        ))
        .await
        .unwrap();
    let settings = client.daemon_settings().await.unwrap();
    assert_eq!(entry(&settings, "history.enabled").value, json!(true));
    assert!(!entry(&settings, "history.enabled").pending_restart);
    t.stop().await;
}

// ---- restart settings -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_setting_is_pending_until_the_daemon_restarts() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let live = create(&client, common::spec(&["/bin/cat"])).await;

    let settings = set(&client, "daemon.restore_sessions", json!("stopped")).await;
    let restore = entry(&settings, "daemon.restore_sessions");
    assert_eq!(restore.value, json!("stopped"));
    assert!(restore.set_in_file && restore.pending_restart);
    assert_eq!(restore.applies, Applies::Restart);
    assert!(settings.restart_pending);
    assert!(
        client.daemon_settings().await.unwrap().restart_pending,
        "asking again says the same"
    );
    drop(client);

    let t = t.restart().await;
    let client = t.connect().await;
    let settings = client.daemon_settings().await.unwrap();
    let restore = entry(&settings, "daemon.restore_sessions");
    assert_eq!(restore.value, json!("stopped"));
    assert!(!restore.pending_restart);
    assert!(!settings.restart_pending);
    // And it is in effect: the session that was alive is listed as stopped, not respawned.
    let list: SessionList = client
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    let restored = list.sessions.iter().find(|s| s.id == live.id).unwrap();
    assert_eq!(restored.state, SessionState::Stopped);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_edited_by_hand_shows_what_the_daemon_does_not_use_yet() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    std::fs::write(
        &t.paths.config_file,
        "[notify]\nenabled = false\n[terminal]\nenv_inherit = \"daemon\"\n",
    )
    .unwrap();
    let settings = client.daemon_settings().await.unwrap();
    assert!(settings.restart_pending);
    assert!(entry(&settings, "notify.enabled").pending_restart);
    assert!(entry(&settings, "terminal.env_inherit").pending_restart);
    assert!(!entry(&settings, "tasks.max_parallel").pending_restart);

    // A change of one notify setting hands the daemon the whole fresh `[notify]` table: the
    // hand edit of its neighbour is in use now too. The restart setting stays pending.
    let settings = set(&client, "notify.min_turn_secs", json!(5)).await;
    assert!(!entry(&settings, "notify.enabled").pending_restart);
    assert!(entry(&settings, "terminal.env_inherit").pending_restart);
    assert!(settings.restart_pending);
    t.stop().await;
}

// ---- refusals ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn values_that_do_not_fit_are_refused_and_nothing_is_written() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, "x").unwrap();

    for (key, value, wanted) in [
        ("nope", json!(true), "not a daemon setting"),
        ("notify", json!(true), "not a daemon setting"),
        ("notify.enabled", json!("yes"), "true or false"),
        ("tasks.max_parallel", json!(0), "at least 1"),
        ("tasks.max_parallel", json!("two"), "whole number"),
        ("history.range_days", json!(45), "0, 30, 90"),
        (
            "control.agents",
            json!("sometimes"),
            "ask, allow, read_only",
        ),
        ("notify.kinds", json!(["task_done", "nope"]), "`nope`"),
        ("codex.args", json!("app-server"), "list of text"),
        ("harness.omp.env", json!({"A": 1}), "text for `A`"),
        ("codex.bin", json!("codex"), "absolute path"),
        (
            "harness.omp.bin",
            json!(dir.path().join("missing").to_string_lossy()),
            "not usable",
        ),
        (
            "harness.claude.cli",
            json!(plain.to_string_lossy()),
            "not executable",
        ),
        (
            "history.roots.claude",
            json!(plain.to_string_lossy()),
            "not a folder",
        ),
        (
            "workspaces.home_folder",
            json!(dir.path().join("nope").to_string_lossy()),
            "not usable",
        ),
        (
            "assistant.workspaces.no-such-workspace.files",
            json!(false),
            "no workspace",
        ),
    ] {
        let (code, message) = refused(&client, key, value).await;
        assert_eq!(code, ErrorCode::BadRequest, "{key}: {message}");
        assert!(message.contains(wanted), "{key}: {message}");
        assert_eq!(file(&t), "", "{key} wrote something");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_broken_file_is_reported_with_its_line_and_never_overwritten() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let broken = "[notify]\nenabled = \n";
    std::fs::write(&t.paths.config_file, broken).unwrap();

    for result in [
        client.daemon_settings().await.map(|_| ()),
        client
            .daemon_setting_set("tasks.max_parallel", Some(json!(2)))
            .await
            .map(|_| ()),
    ] {
        let Err(ClientError::Remote(body)) = result else {
            panic!("a broken file was accepted");
        };
        assert_eq!(body.code, ErrorCode::InvalidState);
        assert!(body.message.contains("line 2"), "{}", body.message);
        assert!(
            body.message
                .contains(&*t.paths.config_file.to_string_lossy()),
            "{}",
            body.message
        );
    }
    assert_eq!(file(&t), broken, "the user's text is untouched");

    std::fs::write(&t.paths.config_file, "[notify]\nenabled = false\n").unwrap();
    assert!(client.daemon_settings().await.is_ok());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_in_the_way_of_a_key_is_reported_not_overwritten() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    // Valid TOML the daemon would not load either: `notify` is a number.
    std::fs::write(&t.paths.config_file, "notify = 3\n").unwrap();
    let Err(ClientError::Remote(body)) = client.daemon_settings().await else {
        panic!("accepted");
    };
    assert_eq!(body.code, ErrorCode::InvalidState);
    assert_eq!(file(&t), "notify = 3\n");
    t.stop().await;
}

// ---- who may change them ----------------------------------------------------------------------

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

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_never_changes_the_settings_whatever_the_policy_says() {
    // `allow`: every other mutation of an agent runs.
    let t = TestDaemon::start_with("[control]\nagents = \"allow\"\n", |_| {}).await;
    let user = t.connect().await;
    let session = create(&user, common::spec(&["/bin/cat"])).await;
    report_agent(&t, session.id).await;
    let agent = connect_from(&t, session.id).await;

    // The policy lets it rename its session…
    agent
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: session.id,
            title: "by the agent".into(),
        }))
        .await
        .unwrap();
    // …but not widen its own permissions, nor forget what runs the assistant.
    let before = file(&t);
    for result in [
        agent
            .daemon_setting_set("control.agents", Some(json!("allow")))
            .await
            .map(|_| ()),
        agent
            .daemon_setting_set("assistant.context.files", Some(json!(true)))
            .await
            .map(|_| ()),
        agent
            .daemon_setting_set("notify.enabled", None)
            .await
            .map(|_| ()),
        agent.assistant_reset(AssistantScope::Global, None).await,
    ] {
        let Err(ClientError::Remote(body)) = result else {
            panic!("an agent changed a setting");
        };
        assert_eq!(body.code, ErrorCode::Forbidden);
        assert!(
            body.message.contains("only available to the user"),
            "{}",
            body.message
        );
    }
    assert_eq!(file(&t), before, "nothing was written");

    // Reading is free for it.
    let settings = agent.daemon_settings().await.unwrap();
    assert_eq!(entry(&settings, "control.agents").value, json!("allow"));

    // The user can: and the policy they set is in force for the agent at once.
    set(&user, "control.agents", json!("read_only")).await;
    let denied = agent
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: session.id,
            title: "again".into(),
        }))
        .await;
    assert_eq!(remote_code(denied), ErrorCode::Forbidden);
    t.stop().await;
}

// ---- assistant.reset --------------------------------------------------------------------------

async fn provider(client: &Client, name: &str) {
    client
        .provider_add(hutan_protocol::provider::ProviderAdd {
            kind: "ollama".into(),
            name: name.into(),
            base_url: Some("http://127.0.0.1:9".into()),
            default_model: Some("m".into()),
        })
        .await
        .unwrap();
}

async fn configure(
    client: &Client,
    scope: AssistantScope,
    workspace: Option<&WorkspaceInfo>,
    provider: &str,
) {
    client
        .assistant_configure(AssistantConfigure {
            scope,
            workspace_id: workspace.map(|w| w.id),
            backend: AssistantBackend::Provider,
            model: Some("m".into()),
            provider: Some(provider.into()),
        })
        .await
        .unwrap();
}

async fn open(client: &Client, workspace: &WorkspaceInfo) -> SessionInfo {
    client
        .assistant_open(AssistantOpen {
            scope: AssistantScope::Workspace,
            workspace_id: Some(workspace.id),
            new: false,
        })
        .await
        .unwrap()
}

fn provider_of(session: &SessionInfo) -> &str {
    session
        .chat
        .as_ref()
        .and_then(|chat| chat.provider_id.as_deref())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn resetting_a_workspace_makes_it_inherit_the_global_choice() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let project = tempfile::tempdir().unwrap();
    provider(&client, "alpha").await;
    provider(&client, "beta").await;
    let workspace = client
        .workspace_create(WorkspaceCreate {
            name: "proj".into(),
            paths: vec![project.path().canonicalize().unwrap()],
            project_ids: Vec::new(),
            created_by: None,
        })
        .await
        .unwrap();

    configure(&client, AssistantScope::Global, None, "alpha").await;
    configure(&client, AssistantScope::Workspace, Some(&workspace), "beta").await;
    let own = open(&client, &workspace).await;
    assert_eq!(provider_of(&own), "beta", "the workspace's own choice wins");

    client
        .assistant_reset(AssistantScope::Workspace, Some(workspace.id))
        .await
        .unwrap();
    let inherited = open(&client, &workspace).await;
    assert_eq!(provider_of(&inherited), "alpha", "now the global choice");
    assert_ne!(
        inherited.id, own.id,
        "the session that ran on beta is not the default"
    );
    let sessions: SessionList = client
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    assert!(
        sessions
            .sessions
            .iter()
            .any(|s| s.id == own.id && s.state.is_live()),
        "existing sessions are untouched"
    );

    // Resetting what is not stored is not an error, and the global choice goes the same way.
    client
        .assistant_reset(AssistantScope::Workspace, Some(workspace.id))
        .await
        .unwrap();
    client
        .assistant_reset(AssistantScope::Global, None)
        .await
        .unwrap();
    let stored: i64 = rusqlite::Connection::open(&t.paths.database)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM app_meta WHERE key LIKE 'assistant.config.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, 0);

    // Arguments are checked like `assistant.configure`'s.
    for (scope, workspace_id, code) in [
        (AssistantScope::Workspace, None, ErrorCode::BadRequest),
        (
            AssistantScope::Workspace,
            Some(hutan_core::WorkspaceId::new()),
            ErrorCode::NotFound,
        ),
    ] {
        assert_eq!(
            remote_code(client.assistant_reset(scope, workspace_id).await),
            code
        );
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_override_is_created_by_a_set_and_removed_by_a_reset() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let project = tempfile::tempdir().unwrap();
    let workspace = client
        .workspace_create(WorkspaceCreate {
            name: "proj".into(),
            paths: vec![project.path().canonicalize().unwrap()],
            project_ids: Vec::new(),
            created_by: None,
        })
        .await
        .unwrap();
    let key = format!("assistant.workspaces.{}.files", workspace.id);
    assert!(
        client
            .daemon_settings()
            .await
            .unwrap()
            .entries
            .iter()
            .all(|e| !e.key.starts_with("assistant.workspaces.")),
        "a workspace without an override has no entries"
    );

    let settings = set(&client, &key, json!(false)).await;
    let files = entry(&settings, &key);
    assert_eq!(files.value, json!(false));
    assert_eq!(
        files.default,
        json!(true),
        "inherited from [assistant.context]"
    );
    assert!(files.set_in_file && !files.pending_restart);
    assert_eq!(files.applies, Applies::Live);
    // Its siblings are listed so a client can edit them.
    let shell = entry(
        &settings,
        &format!("assistant.workspaces.{}.shell", workspace.id),
    );
    assert!(!shell.set_in_file);
    assert_eq!(
        file(&t),
        format!("[assistant.workspaces.{}]\nfiles = false\n", workspace.id)
    );

    let settings = reset(&client, &key).await;
    assert!(
        settings
            .entries
            .iter()
            .all(|e| !e.key.starts_with("assistant.workspaces."))
    );
    assert_eq!(file(&t), "", "the table goes with its last key");
    t.stop().await;
}
