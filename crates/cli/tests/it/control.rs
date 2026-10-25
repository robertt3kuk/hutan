//! The control surface through the CLI (ADR 0016): `hutan ui actions | layout | run | set`
//! against a fake desktop, and the approval flow of a CLI that runs inside an agent's session
//! (`HUTAN_SESSION_ID`), next to the same CLI in a plain shell session, which is the user.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{HUTAN, Out, TestDaemon, WAIT, command, finish};
use hutan_client::{Client, ConnectOptions};
use hutan_protocol::ErrorCode;
use hutan_protocol::control::ErrorBody;
use hutan_protocol::messages::{ClientInfo, ClientKind, Empty, Event, Request, Subscribed};
use hutan_protocol::ui::{
    AppearanceSummary, DockSummary, FocusSummary, LayoutSummary, PaneItem, PaneSummary, UiAction,
    UiCommandResult, UiPublish, UiSetting,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

// ---- a fake desktop -----------------------------------------------------------------------

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

pub(crate) struct FakeDesktop {
    commands: Seen,
    _client: Client,
}

fn catalog() -> UiPublish {
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
                    "properties": {"name": {"type": "string", "enum": ["default", "focus"]}},
                    "required": ["name"],
                })),
            },
            UiAction {
                id: "workbench::RunSetup".into(),
                title: "Hutan: Run Setup Again…".into(),
                args_schema: None,
            },
            UiAction {
                id: "workbench::OpenFile".into(),
                title: "File: Open".into(),
                args_schema: None,
            },
            UiAction {
                id: "workbench::Broken".into(),
                title: "Layout: Broken".into(),
                args_schema: None,
            },
        ],
        settings: vec![
            UiSetting {
                namespace: "terminal".into(),
                key: "font_size".into(),
                kind: "integer".into(),
                doc: "Font size in points".into(),
                layers: vec!["user".into()],
                values: Vec::new(),
                current: Some(json!(13)),
            },
            UiSetting {
                namespace: "appearance".into(),
                key: "density".into(),
                kind: "enum".into(),
                doc: "UI density".into(),
                layers: vec!["user".into()],
                values: vec!["compact".into(), "comfortable".into()],
                current: Some(json!("compact")),
            },
        ],
        layout: LayoutSummary {
            window_id: "w1".into(),
            workspace_id: None,
            preset: "default".into(),
            docks: vec![DockSummary {
                position: "left".into(),
                visible: true,
                panels: vec!["sessions".into(), "history".into()],
                active: Some("sessions".into()),
            }],
            panes: PaneSummary::Pane {
                pane_id: "pane-1".into(),
                items: vec![PaneItem {
                    kind: "terminal".into(),
                    session_id: None,
                    panel: None,
                    path: None,
                    title: "zsh".into(),
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
            window_active: true,
        },
    }
}

impl FakeDesktop {
    pub(crate) async fn start(socket: &Path) -> Self {
        let client = Client::connect(ConnectOptions {
            socket: Some(socket.to_owned()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Desktop,
                version: "test".into(),
                origin: None,
            },
        })
        .await
        .unwrap();
        let mut events = client.events();
        client
            .call::<Subscribed>(Request::EventsSubscribe(Empty {}))
            .await
            .unwrap();
        client
            .call::<Empty>(Request::UiPublish(catalog()))
            .await
            .unwrap();
        let commands: Seen = Arc::default();
        let answerer = client.clone();
        let seen = commands.clone();
        tokio::spawn(async move {
            while let Ok((_, event)) = events.recv().await {
                let Event::UiCommand {
                    request_id,
                    action,
                    args,
                    ..
                } = event
                else {
                    continue;
                };
                let args_path = args["path"].as_str().unwrap_or_default().to_owned();
                seen.lock().push((action.clone(), args));
                let result = if action == "workbench::Broken" {
                    UiCommandResult {
                        request_id,
                        ok: None,
                        error: Some(ErrorBody::new(ErrorCode::InvalidState, "nothing to break")),
                    }
                } else if action == "workbench::OpenFile" && args_path.ends_with("README.md") {
                    // A window whose focus reaches no handler for the action.
                    UiCommandResult {
                        request_id,
                        ok: None,
                        error: Some(ErrorBody::new(
                            ErrorCode::InvalidState,
                            "`workbench::OpenFile` has no handler where the window's focus is now",
                        )),
                    }
                } else {
                    UiCommandResult {
                        request_id,
                        ok: Some(json!({"ran": action})),
                        error: None,
                    }
                };
                let _ = answerer
                    .call::<Empty>(Request::UiCommandResult(result))
                    .await;
            }
        });
        Self {
            commands,
            _client: client,
        }
    }

    pub(crate) fn seen(&self) -> Vec<(String, Value)> {
        self.commands.lock().clone()
    }
}

// ---- helpers --------------------------------------------------------------------------------

/// `hutan` as typed in a Hutan session: `HUTAN_SESSION_ID` set.
fn in_session(socket: &Path, session: &str, args: &[&str]) -> tokio::process::Command {
    let mut command = command(Path::new(HUTAN), Some(socket), args);
    command.env("HUTAN_SESSION_ID", session);
    command
}

async fn start_with_policy(policy: &str) -> TestDaemon {
    let policy = policy.to_owned();
    TestDaemon::start_with(move |config| {
        std::fs::write(
            &config.paths.config_file,
            format!("[control]\nagents = \"{policy}\"\napproval_timeout_secs = 20\n"),
        )
        .unwrap();
    })
    .await
}

/// An agent reports itself in `session`, as its hooks do.
async fn report_agent(t: &TestDaemon, session: &str) {
    finish(in_session(
        &t.paths.socket,
        session,
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    ))
    .await
    .ok();
    timeout(WAIT, async {
        loop {
            if t.session(session).await.get("agent").is_some() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the report never showed up");
}

async fn first_pending(t: &TestDaemon) -> Value {
    timeout(WAIT, async {
        loop {
            let out = t.hutan(&["approvals", "--json"]).await.ok();
            if let Some(first) = out.json()["approvals"].as_array().and_then(|a| a.first()) {
                return first.clone();
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("no approval was raised")
}

fn spawn(command: tokio::process::Command) -> tokio::task::JoinHandle<Out> {
    tokio::spawn(finish(command))
}

// ---- hutan ui -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ui_actions_layout_run_and_set_talk_to_the_desktop() {
    let t = TestDaemon::start().await;

    // No window: a message that says so, and a failing exit code.
    let none = t.hutan(&["ui", "actions"]).await;
    assert_eq!(none.code, 1, "{none:?}");
    assert!(none.stderr.contains("window"), "{none:?}");

    let desktop = FakeDesktop::start(&t.paths.socket).await;

    let actions = t.hutan(&["ui", "actions", "--json"]).await.ok().json();
    assert_eq!(actions["window_id"], "w1");
    let ids: Vec<&str> = actions["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "workbench::SplitRight",
            "workbench::ApplyPreset",
            "workbench::RunSetup",
            "workbench::OpenFile",
            "workbench::Broken",
        ]
    );
    assert_eq!(actions["settings"][0]["key"], "font_size");
    let text = t.hutan(&["ui", "actions"]).await.ok();
    assert!(text.stdout.contains("workbench::ApplyPreset"), "{text:?}");
    assert!(text.stdout.contains("terminal.font_size"), "{text:?}");
    let filtered = t
        .hutan(&["ui", "actions", "--filter", "preset", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(filtered["actions"].as_array().unwrap().len(), 1);
    assert!(filtered["settings"].as_array().unwrap().is_empty());

    let layout = t.hutan(&["ui", "layout", "--json"]).await.ok().json();
    assert_eq!(layout["preset"], "default");
    assert_eq!(layout["docks"][0]["panels"][1], "history");
    let text = t.hutan(&["ui", "layout"]).await.ok();
    assert!(text.stdout.contains("default"), "{text:?}");
    assert!(text.stdout.contains("zsh"), "{text:?}");

    // run: arguments become the action's object.
    let ran = t
        .hutan(&[
            "ui",
            "run",
            "workbench::ApplyPreset",
            "--arg",
            "name=focus",
            "--json",
        ])
        .await
        .ok()
        .json();
    assert_eq!(ran["window_id"], "w1");
    assert_eq!(ran["result"]["ran"], "workbench::ApplyPreset");
    t.hutan(&["ui", "run", "workbench::SplitRight"]).await.ok();
    assert_eq!(
        desktop.seen(),
        [
            (
                "workbench::ApplyPreset".to_owned(),
                json!({"name": "focus"})
            ),
            ("workbench::SplitRight".to_owned(), json!({})),
        ]
    );

    // The window's refusal and the daemon's are the CLI's failures.
    let broken = t.hutan(&["ui", "run", "workbench::Broken"]).await;
    assert_eq!(broken.code, 1, "{broken:?}");
    assert!(broken.stderr.contains("nothing to break"), "{broken:?}");
    let unknown = t.hutan(&["ui", "run", "workbench::Nope"]).await;
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert!(unknown.stderr.contains("unknown action"), "{unknown:?}");

    // set: the value follows the declared type, the layer is named.
    t.hutan(&["ui", "set", "terminal.font_size", "15"])
        .await
        .ok();
    t.hutan(&["ui", "set", "appearance.density", "comfortable"])
        .await
        .ok();
    let seen = desktop.seen();
    assert_eq!(seen[3].0, "ui::SetSetting");
    assert_eq!(
        seen[3].1,
        json!({"namespace": "terminal", "key": "font_size", "value": 15, "layer": "user"})
    );
    assert_eq!(seen[4].1["value"], "comfortable");
    let layer = t
        .hutan(&[
            "ui",
            "set",
            "terminal.font_size",
            "15",
            "--layer",
            "workspace",
        ])
        .await;
    assert_eq!(layer.code, 1, "{layer:?}");
    assert!(layer.stderr.contains("layer"), "{layer:?}");
    let missing = t.hutan(&["ui", "set", "terminal.nope", "1"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");

    t.stop().await;
}

/// `hutan setup` opens the desktop's setup through `ui.run`; with no window it says where the
/// setup lives.
#[tokio::test(flavor = "multi_thread")]
async fn setup_opens_the_desktops_setup_and_says_so_without_a_window() {
    let t = TestDaemon::start().await;

    let none = t.hutan(&["setup", "--json"]).await.ok().json();
    assert_eq!(none["opened"], false);
    let text = t.hutan(&["setup"]).await.ok();
    assert!(
        text.stdout.contains("Run Setup Again…") && text.stdout.contains("none is open"),
        "{text:?}"
    );

    let desktop = FakeDesktop::start(&t.paths.socket).await;
    let opened = t.hutan(&["setup", "--json"]).await.ok().json();
    assert_eq!(opened["opened"], true);
    assert_eq!(
        t.hutan(&["setup"]).await.ok().stdout.trim(),
        "the setup is open in Hutan"
    );
    assert_eq!(
        desktop.seen(),
        [
            ("workbench::RunSetup".to_owned(), json!({})),
            ("workbench::RunSetup".to_owned(), json!({})),
        ]
    );
    t.stop().await;
}

/// A desktop whose Quit ends the window's connection before it can answer, as the real one's
/// does.
async fn start_quitting_desktop(socket: &Path) {
    let client = Client::connect(ConnectOptions {
        socket: Some(socket.to_owned()),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
            origin: None,
        },
    })
    .await
    .unwrap();
    let mut events = client.events();
    client
        .call::<Subscribed>(Request::EventsSubscribe(Empty {}))
        .await
        .unwrap();
    let mut publish = catalog();
    publish.actions = vec![UiAction {
        id: "workbench::Quit".into(),
        title: "App: Quit Hutan".into(),
        args_schema: None,
    }];
    client
        .call::<Empty>(Request::UiPublish(publish))
        .await
        .unwrap();
    tokio::spawn(async move {
        while let Ok((_, event)) = events.recv().await {
            if matches!(event, Event::UiCommand { .. }) {
                // The window is gone: the last handle of the connection goes with it.
                drop(client);
                return;
            }
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn running_an_action_that_closes_the_window_reports_success() {
    let t = TestDaemon::start().await;
    start_quitting_desktop(&t.paths.socket).await;

    let ran = t
        .hutan(&["ui", "run", "workbench::Quit", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(ran["window_id"], "w1");
    assert_eq!(
        ran["result"],
        json!({"ran": "workbench::Quit", "window_closed": true})
    );

    // The window is gone with its connection: the next command says so.
    let gone = t.hutan(&["ui", "run", "workbench::Quit"]).await;
    assert_eq!(gone.code, 1, "{gone:?}");
    assert!(gone.stderr.contains("window"), "{gone:?}");

    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_human_output_of_a_window_closing_action_says_so() {
    let t = TestDaemon::start().await;
    start_quitting_desktop(&t.paths.socket).await;
    let out = t.hutan(&["ui", "run", "workbench::Quit"]).await.ok();
    assert!(out.stdout.contains("the window closed"), "{out:?}");
    t.stop().await;
}

// ---- who is asking ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_plain_shell_sessions_cli_is_not_gated_while_an_agent_sessions_is() {
    let t = start_with_policy("ask").await;
    let shell = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;

    // `hutan rename` typed in a plain Hutan terminal is the user: no approval, whatever
    // `HUTAN_SESSION_ID` says.
    let by_hand = finish(in_session(
        &t.paths.socket,
        &shell,
        &["rename", &target, "by-hand"],
    ))
    .await;
    assert_eq!(by_hand.code, 0, "{by_hand:?}");
    assert_eq!(t.session(&target).await["title"], "by-hand");
    let pending = t.hutan(&["approvals", "--json"]).await.ok().json();
    assert!(pending["approvals"].as_array().unwrap().is_empty());

    // An agent reported itself in the other session: its CLI waits for the user.
    report_agent(&t, &agent).await;
    let waiting = spawn(in_session(
        &t.paths.socket,
        &agent,
        &["rename", &target, "by-agent", "--json"],
    ));
    let approval = first_pending(&t).await;
    assert_eq!(approval["kind"], "control");
    assert_eq!(approval["session_id"], agent.as_str());
    let title = approval["title"].as_str().unwrap();
    assert!(
        title.contains("by-agent") && title.contains("by-hand"),
        "{title}"
    );
    let options: Vec<&str> = approval["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["allow_once", "allow_session", "deny"]);
    assert_eq!(
        t.session(&target).await["title"],
        "by-hand",
        "nothing ran before the answer"
    );
    let id = approval["id"].as_str().unwrap();
    t.hutan(&["approve", id, "allow_once"]).await.ok();
    let done = waiting.await.unwrap();
    assert_eq!(done.code, 0, "{done:?}");
    assert_eq!(t.session(&target).await["title"], "by-agent");

    // Denied: exit 1, and the message tells the agent not to retry.
    let waiting = spawn(in_session(
        &t.paths.socket,
        &agent,
        &["rename", &target, "denied"],
    ));
    let id = first_pending(&t).await["id"].as_str().unwrap().to_owned();
    t.hutan(&["approve", &id, "deny"]).await.ok();
    let refused = waiting.await.unwrap();
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("denied"), "{refused:?}");
    assert_eq!(t.session(&target).await["title"], "by-agent");

    // Reads of the agent's CLI are never gated.
    let ls = finish(in_session(&t.paths.socket, &agent, &["ls", "--json"])).await;
    assert_eq!(ls.code, 0, "{ls:?}");

    // An agent cannot answer approvals: not its own, not anyone's.
    let own = finish(in_session(
        &t.paths.socket,
        &agent,
        &["approve", "0000", "allow_once"],
    ))
    .await;
    assert_eq!(own.code, 1, "{own:?}");

    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_ui_commands_ask_through_the_same_approval_flow() {
    let t = start_with_policy("ask").await;
    let desktop = FakeDesktop::start(&t.paths.socket).await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;

    let waiting = spawn(in_session(
        &t.paths.socket,
        &agent,
        &["ui", "run", "workbench::SplitRight"],
    ));
    let approval = first_pending(&t).await;
    assert!(
        approval["title"]
            .as_str()
            .unwrap()
            .contains("workbench::SplitRight"),
        "{approval}"
    );
    assert!(desktop.seen().is_empty(), "the window was not asked yet");
    t.hutan(&["approve", approval["id"].as_str().unwrap(), "allow_session"])
        .await
        .ok();
    assert_eq!(waiting.await.unwrap().code, 0);
    assert_eq!(desktop.seen().len(), 1);

    // `allow_session` covers this action for this session: no second prompt.
    let again = finish(in_session(
        &t.paths.socket,
        &agent,
        &["ui", "run", "workbench::SplitRight"],
    ))
    .await;
    assert_eq!(again.code, 0, "{again:?}");
    assert_eq!(desktop.seen().len(), 2);

    // The user's own `hutan ui run` is never asked.
    t.hutan(&["ui", "run", "workbench::SplitRight"]).await.ok();
    assert_eq!(desktop.seen().len(), 3);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_refuses_an_agents_mutation_with_a_clear_message() {
    let t = start_with_policy("read_only").await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;
    let refused = finish(in_session(
        &t.paths.socket,
        &agent,
        &["rename", &target, "nope"],
    ))
    .await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("read-only"), "{refused:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_guide_is_printed_by_help_agent() {
    let t = TestDaemon::start().await;
    let out = t.hutan(&["help", "agent"]).await.ok();
    assert!(out.stdout.contains("approval"), "{out:?}");
    assert!(out.stdout.contains("hutan ui"), "{out:?}");
    t.stop().await;
}
