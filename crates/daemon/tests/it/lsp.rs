//! Language servers in the daemon (protocol 0.39, capabilities `doc.sync` and `lsp`): a real
//! daemon on a temporary `HUTAN_HOME`, real client connections, and `hutan-fake-lsp`, the
//! scripted server of `hutan-lsp-client`, standing in for rust-analyzer. Waits are on events or
//! on the process table with an upper bound that only turns a hang into a failure.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{TestDaemon, WAIT, create_in, sh, wait_event};
use hutan_client::Client;
use hutan_core::{Seq, SessionId};
use hutan_daemon::lsp::LspTuning;
use hutan_lsp_client::{RestartPolicy, Timeouts};
use hutan_protocol::ErrorCode;
use hutan_protocol::lsp::{
    DocChange, DocEdit, DocOpen, DocOpened, LspCompletionKind, LspCompletionRequest,
    LspCompletionResolve, LspDiagnosticsRequest, LspPosition, LspPositionRequest, LspServerInfo,
    LspServerState, LspSignatureHelpRequest, LspStatusRequest, LspTrust, LspTrustRequest,
};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Origin, Request, WorkspaceOpen,
    WorkspaceOpened,
};
use hutan_protocol::resources::{ResourceSample, ResourcesListRequest};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

fn fake_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-fake-lsp");
    assert!(
        bin.is_file(),
        "hutan-fake-lsp not built: run `cargo build -p hutan-lsp-client --bins` (or `just test`)"
    );
    bin
}

/// A daemon that tracks `root` (a folder with the root marker `fake.toml`) as a project and
/// runs `hutan-fake-lsp` with `script` for the language `fake`.
struct Env {
    t: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    /// Canonical.
    root: PathBuf,
    script: PathBuf,
    _work: tempfile::TempDir,
}

fn fast() -> LspTuning {
    LspTuning {
        restart: Some(RestartPolicy {
            max_failures: 3,
            window: Duration::from_secs(60),
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(200),
        }),
        timeouts: Some(Timeouts {
            initialize: Duration::from_secs(10),
            request: Duration::from_secs(10),
            interactive: Duration::from_secs(10),
            shutdown: Duration::from_secs(2),
            exit: Duration::from_secs(2),
        }),
        ..LspTuning::default()
    }
}

impl Env {
    async fn new(script: Value) -> Self {
        Self::with(script, "[lsp]\ntrust = \"always\"\n", fast()).await
    }

    /// `extra` is more `daemon.toml` (it may hold `[lsp]` and `[lsp.projects.*]`).
    async fn with(script: Value, extra: &str, tuning: LspTuning) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = work.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("fake.toml"), "").unwrap();
        let script_path = base.join("script.json");
        std::fs::write(&script_path, script.to_string()).unwrap();
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n{extra}\n\
             [lsp.servers.fake]\ncommand = \"{}\"\nargs = [\"--script\", \"{}\"]\n\
             languages = [\"fake\"]\nroot_markers = [\"fake.toml\"]\n",
            fake_bin().display(),
            script_path.display()
        );
        let t = TestDaemon::start_with(&toml, |config| config.lsp = tuning).await;
        let (client, events) = t.connect_subscribed().await;
        let _: WorkspaceOpened = client
            .call(Request::WorkspaceOpen(WorkspaceOpen {
                path: root.clone(),
                created_by: None,
            }))
            .await
            .unwrap();
        assert!(client.supports_doc_sync() && client.supports_lsp());
        Self {
            t,
            client,
            events,
            root,
            script: script_path,
            _work: work,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.root.join("src").join(name);
        if !path.exists() {
            std::fs::write(&path, "").unwrap();
        }
        path
    }

    async fn stop(self) {
        self.t.stop().await;
        wait_no_process(&self.script).await;
    }
}

async fn open(client: &Client, path: &Path, text: &str) -> DocOpened {
    client
        .doc_open(DocOpen {
            path: path.to_owned(),
            language: Some("fake".to_owned()),
            version: 1,
            text: text.to_owned(),
        })
        .await
        .unwrap()
}

fn position(line: u32, character: u32) -> LspPosition {
    LspPosition { line, character }
}

fn insert(at: LspPosition, text: &str) -> DocEdit {
    DocEdit {
        start: at,
        end: at,
        text: text.to_owned(),
    }
}

/// Processes whose command line names `script`: the fake servers of one test.
fn fake_pids(script: &Path) -> Vec<u32> {
    let out = Command::new("pgrep")
        .arg("-f")
        .arg(script.display().to_string())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

async fn wait_no_process(script: &Path) {
    timeout(WAIT, async {
        while !fake_pids(script).is_empty() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a fake language server is still running");
}

async fn wait_state(
    events: &mut broadcast::Receiver<(Seq, Event)>,
    what: &str,
    mut want: impl FnMut(&LspServerInfo) -> bool,
) -> LspServerInfo {
    wait_event(events, what, |event| match event {
        Event::LspState { server } if want(server) => Some((**server).clone()),
        _ => None,
    })
    .await
}

async fn status(client: &Client) -> hutan_protocol::lsp::LspStatus {
    client
        .lsp_status(LspStatusRequest::default())
        .await
        .unwrap()
}

/// Waits until the server of `name` is in `state` (looking at `lsp.status`, so it does not
/// matter whether the event was already seen).
async fn wait_status(client: &Client, state: LspServerState) -> LspServerInfo {
    timeout(WAIT, async {
        loop {
            if let Some(server) = status(client)
                .await
                .servers
                .into_iter()
                .find(|s| s.state == state)
            {
                return server;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no server became {state:?}"))
}

fn error_code<T: std::fmt::Debug>(result: Result<T, hutan_client::ClientError>) -> ErrorCode {
    common::remote_code(result)
}

// ---- the acceptance flows ------------------------------------------------------------------

#[tokio::test]
async fn initialize_diagnostics_hover_and_definition_run_through_the_daemon() {
    let env = Env::new(json!({
        "capabilities": {
            "positionEncoding": "utf-8", "textDocumentSync": 2,
            "hoverProvider": true, "definitionProvider": true
        },
        "diagnostics": true,
        "hover": "**fake** hover",
    }))
    .await;
    let mut events = env.client.events();
    let path = env.file("a.fake");
    // "é" is two bytes in UTF-8, one UTF-16 unit: the server's byte columns become UTF-16.
    let opened = open(&env.client, &path, "é error\n").await;
    assert!(opened.owner);
    assert_eq!(opened.language.as_deref(), Some("fake"));
    assert_eq!(opened.servers.len(), 1);
    assert_eq!(opened.servers[0].name, "fake");
    assert!(matches!(
        opened.servers[0].state,
        LspServerState::Starting | LspServerState::Ready | LspServerState::Indexing
    ));

    // The diagnostics arrive as an event on the connection that has the document open.
    let (event_path, version, diagnostics) =
        wait_event(&mut events, "diagnostics", |event| match event {
            Event::LspDiagnosticsChanged {
                path,
                version,
                diagnostics,
                ..
            } if !diagnostics.is_empty() => Some((path.clone(), *version, diagnostics.clone())),
            _ => None,
        })
        .await;
    assert_eq!(event_path, path);
    assert_eq!(version, Some(1));
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "found error");
    assert_eq!(diagnostics[0].source.as_deref(), Some("fake"));
    assert_eq!(diagnostics[0].range.start, position(0, 2));
    assert_eq!(diagnostics[0].range.end, position(0, 7));

    // And the same through the query, which any connection may ask.
    let other = env.t.connect().await;
    let listed = other
        .lsp_diagnostics(LspDiagnosticsRequest {
            path: Some(path.clone()),
            ..LspDiagnosticsRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.files.len(), 1);
    assert_eq!(listed.files[0].diagnostics, diagnostics);
    assert_eq!(listed.servers.len(), 1);

    let ready = wait_status(&env.client, LspServerState::Ready).await;
    assert_eq!(ready.root, env.root);
    assert_eq!(ready.documents, 1);
    assert!(ready.capabilities.contains(&"hover".to_owned()));
    assert!(ready.capabilities.contains(&"definition".to_owned()));
    assert!(
        ready
            .pid
            .is_some_and(|pid| fake_pids(&env.script).contains(&pid))
    );

    let hover = env
        .client
        .lsp_hover(LspPositionRequest {
            path: path.clone(),
            position: position(0, 3),
        })
        .await
        .unwrap();
    assert_eq!(hover.contents.as_deref(), Some("**fake** hover"));
    assert_eq!(hover.range.unwrap().start, position(0, 3));

    let definition = env
        .client
        .lsp_definition(LspPositionRequest {
            path: path.clone(),
            position: position(0, 3),
        })
        .await
        .unwrap();
    assert_eq!(definition.locations.len(), 1);
    assert_eq!(definition.locations[0].path, path);

    env.stop().await;
}

#[tokio::test]
async fn a_crashing_server_restarts_with_backoff_and_then_fails() {
    let mut env = Env::new(json!({"crash": {"on": "initialize", "code": 3}})).await;
    let path = env.file("a.fake");
    open(&env.client, &path, "x\n").await;
    let failed = wait_state(&mut env.events, "failed", |s| {
        s.state == LspServerState::Failed
    })
    .await;
    // Three failures in the window ended it: two restarts happened first.
    assert_eq!(failed.restarts, 2);
    assert!(failed.reason.unwrap().contains("code 3"));
    assert_eq!(failed.pid, None);
    // The document is still open: asking for a hover says why there is no answer.
    let error = env
        .client
        .lsp_hover(LspPositionRequest {
            path: path.clone(),
            position: position(0, 0),
        })
        .await;
    assert_eq!(error_code(error), ErrorCode::InvalidState);
    // A restart by the user starts it again (and it crashes again).
    let restarted = env.client.lsp_restart(&failed.server_id).await.unwrap();
    assert_ne!(restarted.state, LspServerState::Stopped);
    wait_state(&mut env.events, "failed again", |s| {
        s.state == LspServerState::Failed
    })
    .await;
    env.stop().await;
}

#[tokio::test]
async fn an_idle_server_stops_and_the_next_open_starts_it_again() {
    let mut tuning = fast();
    tuning.idle_stop = Some(Duration::from_millis(150));
    let mut env = Env::with(json!({}), "[lsp]\ntrust = \"always\"\n", tuning).await;
    let a = env.file("a.fake");
    let b = env.file("b.fake");
    let opened = open(&env.client, &a, "x\n").await;
    let ready = wait_status(&env.client, LspServerState::Ready).await;
    let pid = ready.pid.unwrap();
    // A document keeps the server up beyond its idle limit: a second one opens and closes.
    let second = open(&env.client, &b, "y\n").await;
    env.client.doc_close(&second.doc_id).await.unwrap();
    assert_eq!(
        status(&env.client).await.servers[0].state,
        LspServerState::Ready
    );
    env.client.doc_close(&opened.doc_id).await.unwrap();
    let stopped = wait_state(&mut env.events, "idle stop", |s| {
        s.state == LspServerState::Stopped
    })
    .await;
    assert!(stopped.reason.unwrap().contains("idle"));
    assert_eq!(stopped.pid, None);
    timeout(WAIT, async {
        while fake_pids(&env.script).contains(&pid) {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the idle server's process is still there");
    // The next document starts it again.
    open(&env.client, &a, "x\n").await;
    let again = wait_status(&env.client, LspServerState::Ready).await;
    assert_ne!(again.pid, Some(pid));
    env.stop().await;
}

#[tokio::test]
async fn an_untrusted_folder_never_spawns_until_the_user_trusts_it() {
    // `[lsp] trust = "ask"` is the default. Agents may restart servers here, to show that the
    // trust still stands in their way.
    let env = Env::with(json!({}), "[control]\nagents = \"allow\"\n", fast()).await;
    let path = env.file("a.fake");
    let opened = open(&env.client, &path, "x\n").await;
    assert_eq!(opened.servers[0].state, LspServerState::Untrusted);
    assert!(opened.servers[0].trust_pending);
    // The request to trust is an attention item: an `lsp.state` event and a pending entry.
    let pending = status(&env.client).await;
    assert_eq!(pending.servers.len(), 1);
    assert_eq!(pending.servers[0].state, LspServerState::Untrusted);
    assert!(pending.servers[0].trust_pending);
    assert_eq!(pending.servers[0].pid, None);
    assert!(pending.trust.is_empty());
    // Not a process anywhere: the test would find the script on a command line.
    assert!(fake_pids(&env.script).is_empty());

    // A hover has no server to ask.
    let hover = env
        .client
        .lsp_hover(LspPositionRequest {
            path: path.clone(),
            position: position(0, 0),
        })
        .await;
    assert_eq!(error_code(hover), ErrorCode::InvalidState);

    // An agent cannot trust the folder, whatever `[control]` says.
    let session = agent_session(&env.t, &env.client).await;
    let agent = connect_from(&env.t, session).await;
    let forbidden = agent
        .lsp_trust(LspTrustRequest {
            folder: env.root.clone(),
            decision: LspTrust::Trusted,
        })
        .await;
    assert_eq!(error_code(forbidden), ErrorCode::Forbidden);
    assert!(fake_pids(&env.script).is_empty());
    // Nor can it restart an untrusted server into life.
    let restart = agent.lsp_restart(&pending.servers[0].server_id).await;
    assert_eq!(error_code(restart), ErrorCode::Forbidden);
    assert!(fake_pids(&env.script).is_empty());

    // The user trusts it: the server starts for the document that waited.
    let trusted = env
        .client
        .lsp_trust(LspTrustRequest {
            folder: env.root.clone(),
            decision: LspTrust::Trusted,
        })
        .await
        .unwrap();
    assert_eq!(trusted.decision, LspTrust::Trusted);
    assert_eq!(trusted.servers.len(), 1);
    let ready = wait_status(&env.client, LspServerState::Ready).await;
    assert!(!ready.trust_pending);
    assert!(!fake_pids(&env.script).is_empty());
    let after = status(&env.client).await;
    assert_eq!(after.trust.len(), 1);
    assert_eq!(after.trust[0].folder, env.root);
    assert_eq!(after.trust[0].decision, LspTrust::Trusted);
    assert_eq!(after.trust[0].decided_by, "cli");

    // The decision is persisted: it survives a daemon restart.
    let Env {
        t,
        client,
        root,
        script,
        _work,
        ..
    } = env;
    drop(client);
    let t = t.restart_with(|config| config.lsp = fast()).await;
    wait_no_process(&script).await;
    let client = t.connect().await;
    let restored = status(&client).await;
    assert_eq!(restored.trust.len(), 1);
    assert_eq!(restored.trust[0].folder, root);
    // And revoking it stops what runs and asks again.
    let (c2, mut events) = t.connect_subscribed().await;
    let _: WorkspaceOpened = c2
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: root.clone(),
            created_by: None,
        }))
        .await
        .unwrap();
    let doc = root.join("src/a.fake");
    let reopened = open(&c2, &doc, "x\n").await;
    assert_ne!(reopened.servers[0].state, LspServerState::Untrusted);
    wait_status(&c2, LspServerState::Ready).await;
    c2.lsp_trust(LspTrustRequest {
        folder: root.clone(),
        decision: LspTrust::Denied,
    })
    .await
    .unwrap();
    let denied = wait_state(&mut events, "untrusted", |s| {
        s.state == LspServerState::Untrusted
    })
    .await;
    assert!(!denied.trust_pending, "a denial is not asked again");
    wait_no_process(&script).await;
    // A denied folder never spawns, however often the document is opened.
    c2.doc_close(&reopened.doc_id).await.unwrap();
    let again = open(&c2, &doc, "x\n").await;
    assert_eq!(again.servers[0].state, LspServerState::Untrusted);
    assert!(fake_pids(&script).is_empty());
    t.stop().await;
    drop(_work);
}

#[tokio::test]
async fn a_change_burst_is_one_did_change() {
    let mut tuning = fast();
    // A window nobody waits out: only a request (or a flush) sends what was collected.
    tuning.coalesce = Some(Duration::from_secs(30));
    let env = Env::with(
        json!({"hover": "echo", "capabilities": {"textDocumentSync": 2, "hoverProvider": true}}),
        "[lsp]\ntrust = \"always\"\n",
        tuning,
    )
    .await;
    let path = env.file("a.fake");
    let opened = open(&env.client, &path, "start\n").await;
    let echo = |hover: hutan_protocol::lsp::LspHover| -> Value {
        serde_json::from_str(&hover.contents.unwrap()).unwrap()
    };
    let at = LspPositionRequest {
        path: path.clone(),
        position: position(0, 0),
    };
    // The first request sends the open; the server has the text and no change yet.
    let first = echo(env.client.lsp_hover(at.clone()).await.unwrap());
    assert_eq!(
        (first["opens"].clone(), first["changes"].clone()),
        (json!(1), json!(0))
    );
    // A hundred edits, one after the other, each at the end of the first line.
    let mut expected = String::from("start");
    for n in 0..100 {
        let piece = format!("{}", n % 10);
        env.client
            .doc_change(DocChange {
                doc_id: opened.doc_id.clone(),
                version: 2 + n,
                edits: vec![insert(position(0, 5 + n as u32), &piece)],
                text: None,
            })
            .await
            .unwrap();
        expected.push_str(&piece);
    }
    let after = echo(env.client.lsp_hover(at).await.unwrap());
    assert_eq!(after["changes"], json!(1), "{after}");
    assert_eq!(after["version"], json!(101));
    assert_eq!(after["text"], json!(format!("{expected}\n")));
    env.stop().await;
}

#[tokio::test]
async fn edits_apply_in_order_in_utf16_and_a_bad_edit_changes_nothing() {
    let env = Env::new(json!({"hover": "echo"})).await;
    let path = env.file("a.fake");
    let opened = open(&env.client, &path, "a😀b\nsecond\n").await;
    let at = LspPositionRequest {
        path: path.clone(),
        position: position(0, 0),
    };
    let text = |hover: hutan_protocol::lsp::LspHover| -> String {
        let value: Value = serde_json::from_str(&hover.contents.unwrap()).unwrap();
        value["text"].as_str().unwrap().to_owned()
    };
    // The emoji is two UTF-16 units: column 3 is right after it. The second edit is in the
    // coordinates the first left.
    env.client
        .doc_change(DocChange {
            doc_id: opened.doc_id.clone(),
            version: 2,
            edits: vec![
                insert(position(0, 3), "X"),
                DocEdit {
                    start: position(1, 0),
                    end: position(1, 6),
                    text: "2nd".to_owned(),
                },
            ],
            text: None,
        })
        .await
        .unwrap();
    assert_eq!(
        text(env.client.lsp_hover(at.clone()).await.unwrap()),
        "a😀Xb\n2nd\n"
    );
    // An edit inside the emoji (after the first edit made the line `!a😀…`, column 3 splits
    // it), or past the text, is refused and the first edit of the same request is not kept
    // either.
    for bad in [position(0, 3), position(9, 0)] {
        let error = env
            .client
            .doc_change(DocChange {
                doc_id: opened.doc_id.clone(),
                version: 3,
                edits: vec![insert(position(0, 0), "!"), insert(bad, "?")],
                text: None,
            })
            .await;
        // Inside a character the position is a clamp for the servers; the mirror is strict.
        assert_eq!(error_code(error), ErrorCode::BadRequest, "{bad:?}");
    }
    assert_eq!(
        text(env.client.lsp_hover(at.clone()).await.unwrap()),
        "a😀Xb\n2nd\n"
    );
    // Both `edits` and `text`, or neither, is a bad request.
    for (edits, text) in [
        (vec![], None),
        (vec![insert(position(0, 0), "x")], Some("y".to_owned())),
    ] {
        let error = env
            .client
            .doc_change(DocChange {
                doc_id: opened.doc_id.clone(),
                version: 4,
                edits,
                text,
            })
            .await;
        assert_eq!(error_code(error), ErrorCode::BadRequest);
    }
    // The whole text instead.
    env.client
        .doc_change(DocChange {
            doc_id: opened.doc_id.clone(),
            version: 5,
            edits: vec![],
            text: Some("fresh\n".to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(text(env.client.lsp_hover(at).await.unwrap()), "fresh\n");
    env.stop().await;
}

#[tokio::test]
async fn two_clients_share_one_server() {
    let env = Env::new(json!({"diagnostics": true})).await;
    let a = env.file("a.fake");
    let b = env.file("b.fake");
    // The desktop (here: a connection of kind desktop) opens one file …
    let desktop = env
        .t
        .connect_as(hutan_protocol::messages::ClientKind::Desktop)
        .await;
    let first = open(&desktop, &a, "error\n").await;
    // … and an agent's connection another, in the same root.
    let session = agent_session(&env.t, &env.client).await;
    let agent = connect_from(&env.t, session).await;
    let second = open(&agent, &b, "ok\n").await;
    assert!(first.owner && second.owner);
    assert_eq!(first.servers[0].server_id, second.servers[0].server_id);
    wait_status(&env.client, LspServerState::Ready).await;
    let all = status(&env.client).await;
    assert_eq!(
        all.servers.len(),
        1,
        "one server for the root, not one per client"
    );
    assert_eq!(all.servers[0].documents, 2);
    assert_eq!(fake_pids(&env.script).len(), 1);
    // Each sees the diagnostics of the file the desktop opened.
    let seen = agent
        .lsp_diagnostics(LspDiagnosticsRequest {
            path: Some(a.clone()),
            ..LspDiagnosticsRequest::default()
        })
        .await
        .unwrap();
    assert_eq!(seen.files.len(), 1);
    // Closing the desktop connection closes its documents; the agent's stays.
    drop(desktop);
    timeout(WAIT, async {
        while status(&env.client).await.servers[0].documents != 1 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the closed connection's document is still counted");
    // A second connection opening the same path follows it: it does not own it.
    let follower = env.t.connect().await;
    let followed = open(&follower, &b, "ok\n").await;
    assert!(!followed.owner);
    let refused = follower
        .doc_change(DocChange {
            doc_id: followed.doc_id,
            version: 2,
            edits: vec![insert(position(0, 0), "x")],
            text: None,
        })
        .await;
    assert_eq!(error_code(refused), ErrorCode::InvalidState);
    // The same connection cannot open a path twice.
    let again = follower
        .doc_open(DocOpen {
            path: b.clone(),
            language: Some("fake".into()),
            version: 1,
            text: String::new(),
        })
        .await;
    assert_eq!(error_code(again), ErrorCode::InvalidState);
    env.stop().await;
}

#[tokio::test]
async fn an_agent_opens_documents_only_inside_the_workspace_folders() {
    let env = Env::new(json!({})).await;
    let outside = env
        ._work
        .path()
        .canonicalize()
        .unwrap()
        .join("elsewhere.fake");
    std::fs::write(&outside, "").unwrap();
    let session = agent_session(&env.t, &env.client).await;
    let agent = connect_from(&env.t, session).await;
    let refused = agent
        .doc_open(DocOpen {
            path: outside.clone(),
            language: Some("fake".into()),
            version: 1,
            text: String::new(),
        })
        .await;
    assert_eq!(error_code(refused), ErrorCode::Forbidden);
    // The user may open anything; no folder is trusted for it, there is no server for it
    // either way ('always' here makes it start in the folder of the file).
    let relative = env
        .client
        .doc_open(DocOpen {
            path: "relative.fake".into(),
            language: None,
            version: 1,
            text: String::new(),
        })
        .await;
    assert_eq!(error_code(relative), ErrorCode::BadRequest);
    let dotdot = env
        .client
        .doc_open(DocOpen {
            path: env.root.join("src/../src/a.fake"),
            language: None,
            version: 1,
            text: String::new(),
        })
        .await;
    assert_eq!(error_code(dotdot), ErrorCode::BadRequest);
    env.stop().await;
}

#[tokio::test]
async fn a_missing_server_is_not_installed_with_the_command_and_the_setting() {
    let extra = "[lsp]\ntrust = \"always\"\n\n\
                 [lsp.servers.rust-analyzer]\ncommand = \"/nonexistent/rust-analyzer\"\n\n\
                 [lsp.servers.gone]\ncommand = \"/nonexistent/gone-lsp\"\nlanguages = [\"gone\"]\n\
                 install = \"brew install gone-lsp\"\n";
    let mut env = Env::with(json!({}), extra, fast()).await;
    let path = env.file("a.rs");
    let opened = env
        .client
        .doc_open(DocOpen {
            path: path.clone(),
            language: None,
            version: 1,
            text: "fn main() {}\n".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(opened.language.as_deref(), Some("rust"));
    let server = &opened.servers[0];
    assert_eq!(server.name, "rust-analyzer");
    assert_eq!(server.state, LspServerState::NotInstalled);
    assert_eq!(
        server.install.as_deref(),
        Some("rustup component add rust-analyzer")
    );
    assert_eq!(
        server.setting.as_deref(),
        Some("lsp.servers.rust-analyzer.command")
    );
    assert!(
        server
            .reason
            .as_deref()
            .unwrap()
            .contains("/nonexistent/rust-analyzer")
    );
    // A server of your own with an install hint of its own.
    let gone = env.file("a.gone");
    let opened = env
        .client
        .doc_open(DocOpen {
            path: gone,
            language: Some("gone".into()),
            version: 1,
            text: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        opened.servers[0].install.as_deref(),
        Some("brew install gone-lsp")
    );
    // The state is an event too, and `lsp.status` lists it with no process.
    let listed = status(&env.client).await;
    assert!(listed.servers.iter().all(|s| s.pid.is_none()));
    assert_eq!(
        listed
            .servers
            .iter()
            .filter(|s| s.state == LspServerState::NotInstalled)
            .count(),
        2
    );
    let event = wait_state(&mut env.events, "not installed", |s| {
        s.state == LspServerState::NotInstalled && s.name == "gone"
    })
    .await;
    assert_eq!(event.setting.as_deref(), Some("lsp.servers.gone.command"));
    // A document of a language nobody serves says so too, and starts nothing.
    let text = env.file("notes.txt");
    let opened = env
        .client
        .doc_open(DocOpen {
            path: text,
            language: None,
            version: 1,
            text: String::new(),
        })
        .await
        .unwrap();
    assert!(opened.servers.is_empty());
    assert!(opened.reason.is_some());
    env.stop().await;
}

/// The `rust-analyzer` on the user's login `PATH` is rustup's proxy and the toolchain lacks the
/// component (the second e2e pass, finding 2): the server is `not_installed` with rustup's
/// command, and the proxy never runs as a server (no crash loop).
#[tokio::test(flavor = "multi_thread")]
async fn rustups_proxy_without_the_component_is_not_installed_with_the_command() {
    use std::os::unix::fs::PermissionsExt as _;
    let work = tempfile::tempdir().unwrap();
    let base = work.path().canonicalize().unwrap();
    let bin = base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ran = base.join("proxy-ran");
    // rustup, and its proxy linked to it, as rustup installs them: run as `rust-analyzer` it
    // says what rustup says without the component, and so does `rustup which`.
    let rustup = bin.join("rustup");
    std::fs::write(
        &rustup,
        format!(
            "#!/bin/sh\n\
             name=$(basename \"$0\")\n\
             if [ \"$name\" != rustup ]; then\n\
             echo \"$name\" >> {ran:?}\n\
             echo \"error: Unknown binary '$name' in official toolchain 'stable-aarch64-apple-darwin'.\" >&2\n\
             exit 1\n\
             fi\n\
             if [ \"$1\" = which ]; then\n\
             echo \"error: Unknown binary '$2' in official toolchain 'stable-aarch64-apple-darwin'.\" >&2\n\
             exit 1\n\
             fi\n\
             exit 2\n",
            ran = ran.display().to_string()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&rustup, bin.join("rust-analyzer")).unwrap();
    // The user's login shell puts that folder first on `PATH`.
    let shell = base.join("login-shell");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nexport PATH=\"{}:$PATH\"\nexec /usr/bin/env -0\n",
            bin.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "").unwrap();

    let t = TestDaemon::start_with("[lsp]\ntrust = \"always\"\n", |config| {
        config.login_shell = Some(shell.clone());
        config.lsp = fast();
    })
    .await;
    // The daemon captures the login environment in the background; the language servers read
    // it once, so the test waits until the capture is in.
    let shell_name = shell.display().to_string();
    let mut login_path = None;
    for _ in 0..30 {
        let name = shell_name.clone();
        let env = tokio::task::spawn_blocking(move || {
            hutan_terminal::env::login_env(&name, &hutan_terminal::env::system_facts())
        })
        .await
        .unwrap();
        login_path = env.and_then(|e| e.get("PATH").cloned());
        if login_path.is_some() {
            break;
        }
    }
    let login_path = login_path.expect("the login environment was captured");
    assert!(
        login_path.starts_with(&bin.display().to_string()),
        "{login_path}"
    );

    let (client, _events) = t.connect_subscribed().await;
    let opened = client
        .doc_open(DocOpen {
            path: repo.join("main.rs"),
            language: None,
            version: 1,
            text: "fn main() {}\n".to_owned(),
        })
        .await
        .unwrap();
    let server = &opened.servers[0];
    assert_eq!(server.name, "rust-analyzer");
    assert_eq!(server.state, LspServerState::NotInstalled);
    assert_eq!(
        server.install.as_deref(),
        Some("rustup component add rust-analyzer")
    );
    let reason = server.reason.as_deref().unwrap();
    assert!(reason.contains("rustup's proxy"), "{reason}");
    assert!(reason.contains("Unknown binary 'rust-analyzer'"), "{reason}");
    assert!(
        reason.contains("rustup component add rust-analyzer"),
        "{reason}"
    );
    let listed = status(&client).await;
    assert_eq!(listed.servers.len(), 1);
    assert_eq!(listed.servers[0].state, LspServerState::NotInstalled);
    assert_eq!(listed.servers[0].pid, None);
    assert_eq!(listed.servers[0].restarts, 0);
    assert!(!ran.exists(), "the proxy never ran as a server");
    t.stop().await;
}

#[tokio::test]
async fn the_topmost_marker_is_the_root_and_a_project_can_override_it() {
    let extra = "[lsp]\ntrust = \"always\"\n";
    let env = Env::with(json!({}), extra, fast()).await;
    // A nested package with a marker of its own: the root is still the repo's.
    let nested = env.root.join("pkg/src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(env.root.join("pkg/fake.toml"), "").unwrap();
    let file = nested.join("deep.fake");
    std::fs::write(&file, "").unwrap();
    let opened = open(&env.client, &file, "x\n").await;
    let server = status(&env.client).await.servers.remove(0);
    assert_eq!(opened.servers[0].server_id, server.server_id);
    assert_eq!(server.root, env.root);
    env.stop().await;

    // The project named `repo` says otherwise for this server.
    let extra = "[lsp]\ntrust = \"always\"\n\n[lsp.projects.repo.roots]\nfake = \"pkg\"\n";
    let env = Env::with(json!({}), extra, fast()).await;
    std::fs::create_dir_all(env.root.join("pkg/src")).unwrap();
    std::fs::write(env.root.join("pkg/fake.toml"), "").unwrap();
    let file = env.root.join("pkg/src/deep.fake");
    std::fs::write(&file, "").unwrap();
    open(&env.client, &file, "x\n").await;
    let server = status(&env.client).await.servers.remove(0);
    assert_eq!(server.root, env.root.join("pkg"));
    // A file outside the override is not covered by it: it gets the detected root.
    let other = env.file("a.fake");
    open(&env.client, &other, "x\n").await;
    let roots: Vec<PathBuf> = status(&env.client)
        .await
        .servers
        .into_iter()
        .map(|s| s.root)
        .collect();
    assert!(roots.contains(&env.root.join("pkg")) && roots.contains(&env.root));
    env.stop().await;

    // A project can switch language servers off.
    let extra = "[lsp]\ntrust = \"always\"\n\n[lsp.projects.repo]\nenabled = false\n";
    let env = Env::with(json!({}), extra, fast()).await;
    let file = env.file("a.fake");
    let opened = open(&env.client, &file, "x\n").await;
    assert!(opened.servers.is_empty());
    assert!(opened.reason.unwrap().contains("switched off"));
    assert!(fake_pids(&env.script).is_empty());
    env.stop().await;
}

#[tokio::test]
async fn max_servers_makes_room_by_stopping_an_idle_server_and_refuses_otherwise() {
    let extra = "[lsp]\ntrust = \"always\"\nmax_servers = 1\n";
    let mut env = Env::with(json!({}), extra, fast()).await;
    // A second root next to the first, a project of the daemon too.
    let second_root = env.root.parent().unwrap().join("other");
    std::fs::create_dir_all(second_root.join("src")).unwrap();
    std::fs::write(second_root.join("fake.toml"), "").unwrap();
    let _: WorkspaceOpened = env
        .client
        .call(Request::WorkspaceOpen(WorkspaceOpen {
            path: second_root.clone(),
            created_by: None,
        }))
        .await
        .unwrap();
    let a = env.file("a.fake");
    let b = second_root.join("src/b.fake");
    std::fs::write(&b, "").unwrap();
    let first = open(&env.client, &a, "x\n").await;
    wait_status(&env.client, LspServerState::Ready).await;
    // Both have a document: no room, the second server is refused with the reason.
    let refused = open(&env.client, &b, "y\n").await;
    assert_eq!(refused.servers[0].state, LspServerState::Stopped);
    assert!(
        refused.servers[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("max_servers")
    );
    assert_eq!(fake_pids(&env.script).len(), 1);
    // The first one's document closes: it is idle, and a restart of the second makes room.
    env.client.doc_close(&first.doc_id).await.unwrap();
    let restarted = env
        .client
        .lsp_restart(&refused.servers[0].server_id)
        .await
        .unwrap();
    assert_ne!(restarted.state, LspServerState::Stopped);
    let evicted = wait_state(&mut env.events, "stopped for room", |s| {
        s.state == LspServerState::Stopped
            && s.reason.as_deref().is_some_and(|r| r.contains("make room"))
    })
    .await;
    assert_eq!(evicted.root, env.root);
    let running = wait_status(&env.client, LspServerState::Ready).await;
    assert_eq!(running.root, second_root);
    let servers = status(&env.client).await.servers;
    assert_eq!(
        servers
            .iter()
            .filter(|s| s.pid.is_some())
            .map(|s| &s.root)
            .collect::<Vec<_>>(),
        [&second_root]
    );
    timeout(WAIT, async {
        while fake_pids(&env.script).len() != 1 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("two servers run under max_servers = 1");
    env.stop().await;
}

#[tokio::test]
async fn servers_are_service_nodes_in_resources_and_die_with_the_daemon() {
    let env = Env::new(json!({"spawn_child": true})).await;
    let path = env.file("a.fake");
    open(&env.client, &path, "x\n").await;
    let ready = wait_status(&env.client, LspServerState::Ready).await;
    let listed = env
        .client
        .call::<ResourceSample>(Request::ResourcesList(ResourcesListRequest::default()))
        .await
        .unwrap();
    assert_eq!(listed.services.len(), 1);
    let node = &listed.services[0];
    assert_eq!(node.kind, "lsp");
    assert!(node.service_id.starts_with("lsp:fake:"));
    assert_eq!(node.processes[0].pid, ready.pid.unwrap());
    assert!(node.totals.rss_bytes > 0);
    // `lsp.status` carries the memory too.
    let with_rss = status(&env.client).await.servers.remove(0);
    assert!(with_rss.rss_bytes.is_some_and(|rss| rss > 0));
    // The server's own child (`spawn_child`) is part of the tree; the daemon's end takes both.
    let Env {
        t, script, _work, ..
    } = env;
    t.stop().await;
    wait_no_process(&script).await;
    drop(_work);
}

#[tokio::test]
async fn a_connection_that_goes_away_closes_its_documents() {
    let env = Env::new(json!({})).await;
    let path = env.file("a.fake");
    let gone = env.t.connect().await;
    open(&gone, &path, "x\n").await;
    assert_eq!(status(&env.client).await.servers[0].documents, 1);
    drop(gone);
    timeout(WAIT, async {
        while status(&env.client).await.servers[0].documents != 0 {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the document of a closed connection is still open");
    // Another connection opens it as the owner now.
    let again = open(&env.client, &path, "x\n").await;
    assert!(again.owner);
    env.stop().await;
}

// ---- helpers for agent-originated connections ----------------------------------------------

/// A live session an agent reported itself in: connections that name it are an agent's.
async fn agent_session(t: &TestDaemon, user: &Client) -> SessionId {
    let session = create_in(user, None, sh("sleep 60")).await;
    let reporter = t.connect().await;
    let applied: AgentApplied = reporter
        .call(Request::AgentReport(AgentReportRequest {
            session_id: session.id,
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
    session.id
}

async fn connect_from(t: &TestDaemon, session: SessionId) -> Client {
    let mut options = t.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    Client::connect(options).await.unwrap()
}

// ---- completion and signature help (protocol 0.41, E-12b) -----------------------------------

/// A server with completion (trigger `.`, resolve) and signature help (`(`, `,`) in UTF-8.
fn completion_script(journal: Option<&Path>) -> Value {
    json!({
        "capabilities": {
            "positionEncoding": "utf-8",
            "textDocumentSync": 2,
            "completionProvider": {"triggerCharacters": ["."], "resolveProvider": true},
            "signatureHelpProvider": {"triggerCharacters": ["("], "retriggerCharacters": [","]},
        },
        "completion": {"items": [
            {"label": "push", "kind": 2, "sortText": "1", "insertTextFormat": 2,
             "insertText": "push(${1:value})$0"},
            {"label": "HashMap", "kind": 22, "sortText": "2",
             "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0},
                 "end": {"line": 0, "character": 0}}, "newText": "use std::collections::HashMap;\n"}]},
            // A replacement after the `é` (two bytes, one UTF-16 unit).
            {"label": "émoji", "kind": 6, "textEdit": {"range": {"start": {"line": 0, "character": 3},
                 "end": {"line": 0, "character": 4}}, "newText": "émoji"}},
        ]},
        "resolve": {"push": {"documentation": {"kind": "markdown", "value": "Appends **value**."},
                             "detail": "fn push(&mut self, value: T)"}},
        "signature_help": {"label": "push(value: T, n: usize)", "parameters": ["value: T", "n: usize"],
                           "documentation": "Pushes."},
        "hang_once": if journal.is_some() { json!(["textDocument/completion"]) } else { json!([]) },
        "journal": journal,
    })
}

fn completion_at(path: &Path, at: LspPosition, trigger: Option<&str>) -> LspCompletionRequest {
    LspCompletionRequest {
        path: path.to_owned(),
        position: at,
        trigger: trigger.map(str::to_owned),
        incomplete: false,
    }
}

#[tokio::test]
async fn completion_resolve_and_signature_help_run_through_the_daemon() {
    let env = Env::new(completion_script(None)).await;
    assert!(env.client.supports_lsp_completion());
    let path = env.file("a.fake");
    open(&env.client, &path, "é.x\nv.push(a, \n").await;
    let ready = wait_status(&env.client, LspServerState::Ready).await;
    assert_eq!(ready.completion_triggers, ["."]);
    assert_eq!(ready.signature_triggers, ["(", ","]);
    assert!(ready.capabilities.contains(&"signature_help".to_owned()));

    let list = env
        .client
        .lsp_completion(completion_at(&path, position(0, 2), Some(".")))
        .await
        .unwrap();
    assert!(!list.incomplete && !list.truncated);
    let summary: Vec<_> = list
        .items
        .iter()
        .map(|i| (i.label.as_str(), i.kind, i.snippet, i.resolvable))
        .collect();
    assert_eq!(
        summary,
        [
            ("push", Some(LspCompletionKind::Method), true, true),
            ("HashMap", Some(LspCompletionKind::Struct), false, true),
            ("émoji", Some(LspCompletionKind::Variable), false, true),
        ]
    );
    assert_eq!(
        list.items[0].insert_text.as_deref(),
        Some("push(${1:value})$0")
    );
    assert_eq!(
        list.items[1].additional_edits[0].range.start,
        position(0, 0)
    );
    // The server's UTF-8 columns 3..4 are UTF-16 columns 2..3 on the wire.
    let edit = list.items[2].edit.clone().unwrap();
    assert_eq!(
        (edit.range.start, edit.range.end),
        (position(0, 2), position(0, 3))
    );

    let resolved = env
        .client
        .lsp_completion_resolve(LspCompletionResolve {
            completion_id: list.completion_id,
            index: 0,
        })
        .await
        .unwrap();
    assert_eq!(
        resolved.documentation.as_deref(),
        Some("Appends **value**.")
    );
    assert_eq!(
        resolved.detail.as_deref(),
        Some("fn push(&mut self, value: T)")
    );
    assert_eq!(
        error_code(
            env.client
                .lsp_completion_resolve(LspCompletionResolve {
                    completion_id: list.completion_id + 100,
                    index: 0,
                })
                .await
        ),
        ErrorCode::NotFound
    );

    // After `v.push(a, ` the second parameter is the one being typed.
    let help = env
        .client
        .lsp_signature_help(LspSignatureHelpRequest {
            path: path.clone(),
            position: position(1, 10),
            trigger: Some(",".into()),
            retrigger: true,
        })
        .await
        .unwrap();
    let signature = &help.signatures[help.active_signature as usize];
    assert_eq!(signature.active_parameter, Some(1));
    let n = &signature.parameters[1];
    assert_eq!(
        &signature.label[n.start as usize..n.end as usize],
        "n: usize"
    );
    assert_eq!(signature.documentation.as_deref(), Some("Pushes."));
    // Outside a call there is none.
    let none = env
        .client
        .lsp_signature_help(LspSignatureHelpRequest {
            path: path.clone(),
            position: position(0, 1),
            trigger: None,
            retrigger: false,
        })
        .await
        .unwrap();
    assert!(none.signatures.is_empty());

    // B16's share of the daemon: socket, hub, the fake server's answer and back. The fake
    // server answers at once, so this bounds protocol + daemon overhead from above.
    let mut times = Vec::new();
    for _ in 0..40 {
        let started = std::time::Instant::now();
        env.client
            .lsp_completion(completion_at(&path, position(0, 2), Some(".")))
            .await
            .unwrap();
        times.push(started.elapsed());
    }
    times.sort();
    let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
    eprintln!("B16 lsp.completion round trip with hutan-fake-lsp: median {median:?}, p95 {p95:?}");
    // The budget is 5 ms in a release build; a loaded debug build gets ten times that.
    assert!(median < Duration::from_millis(50), "median {median:?}");
    env.stop().await;
}

/// The journal's lines (see `hutan-fake-lsp`'s `journal`).
fn journal_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

async fn wait_journal(path: &Path, what: &str, found: impl Fn(&str) -> bool) -> String {
    timeout(WAIT, async {
        loop {
            if let Some(line) = journal_lines(path).into_iter().find(|l| found(l)) {
                return line;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the fake server's journal never showed {what}"))
}

#[tokio::test]
async fn a_newer_completion_supersedes_the_older_and_the_server_is_told() {
    let work = tempfile::tempdir().unwrap();
    let journal = work.path().join("journal");
    let env = Env::new(completion_script(Some(&journal))).await;
    let path = env.file("a.fake");
    open(&env.client, &path, "v.\n").await;
    wait_status(&env.client, LspServerState::Ready).await;

    // The first one is never answered by the server…
    let first = tokio::spawn({
        let client = env.client.clone();
        let request = completion_at(&path, position(0, 2), Some("."));
        async move { client.lsp_completion(request).await }
    });
    let asked = wait_journal(&journal, "the first completion", |l| {
        l.starts_with("textDocument/completion ")
    })
    .await;
    let first_id = asked
        .trim_start_matches("textDocument/completion ")
        .to_owned();
    // …and the next one of the connection replaces it while it waits.
    let second = env
        .client
        .lsp_completion(completion_at(&path, position(0, 2), None))
        .await
        .unwrap();
    assert_eq!(second.items.len(), 3);
    let superseded = first.await.unwrap();
    match superseded {
        Err(hutan_client::ClientError::Remote(error)) => {
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(error.data.unwrap()["reason"], "superseded");
        }
        other => panic!("expected the first one superseded, got {other:?}"),
    }
    wait_journal(&journal, "the cancellation", |l| {
        l == format!("cancelled {first_id}")
    })
    .await;
    env.stop().await;
}
