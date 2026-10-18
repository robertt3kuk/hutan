#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The desktop as part of the agent control surface (protocol 0.21, ADR 0016) against an
//! in-process daemon: it publishes its action catalog, settings catalog and layout (after
//! connecting, after a reconnect, after a change), and executes the `ui.command`s the daemon
//! routes to it for a split, a preset, a panel, a session to focus and a setting, answering
//! ok or error.

mod common;

use std::time::Duration;

use common::control::Pending;
use common::{TestDaemon, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_client::ClientError;
use hutan_core::SessionId;
use hutan_desktop::workbench::Workbench;
use hutan_desktop::workbench::model::{ItemRef, PanelId};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::Request;
use hutan_protocol::ui::{
    LayoutSummary, PaneSummary, UiActionList, UiLayoutResult, UiRan, UiRun, UiSetSetting, UiTarget,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

/// A workbench on `connection` with the daemon's home as its settings directory.
fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

/// Lets the debounce timers of the window run.
fn tick(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

/// Pumps the window (and its timers) until `condition` holds.
fn settle(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        tick(cx);
        condition(cx)
    });
}

fn layout_of(daemon: &TestDaemon) -> Option<LayoutSummary> {
    daemon
        .try_call::<UiLayoutResult>(Request::UiLayout(UiTarget::default()))
        .ok()
        .map(|r| r.layout)
}

fn pane_count(node: &PaneSummary) -> usize {
    match node {
        PaneSummary::Pane { .. } => 1,
        PaneSummary::Split { children, .. } => children.iter().map(pane_count).sum(),
    }
}

/// Sends a `ui.run` and pumps the window until its answer arrives.
fn run(
    daemon: &TestDaemon,
    cx: &mut VisualTestContext,
    action: &str,
    args: Option<Value>,
) -> Result<UiRan, ClientError> {
    let pending: Pending<UiRan> = daemon.call_in_background(Request::UiRun(UiRun {
        window_id: None,
        action: action.to_owned(),
        args,
    }));
    answer(cx, pending, action)
}

fn set(
    daemon: &TestDaemon,
    cx: &mut VisualTestContext,
    namespace: &str,
    key: &str,
    value: Value,
    layer: &str,
) -> Result<UiRan, ClientError> {
    let pending: Pending<UiRan> = daemon.call_in_background(Request::UiSetSetting(UiSetSetting {
        window_id: None,
        namespace: namespace.to_owned(),
        key: key.to_owned(),
        value,
        layer: layer.to_owned(),
    }));
    answer(cx, pending, key)
}

fn answer<T>(
    cx: &mut VisualTestContext,
    pending: Pending<T>,
    what: &str,
) -> Result<T, ClientError> {
    let mut taken = None;
    wait_until(cx, &format!("the answer to {what}"), |_| {
        taken = pending.try_take();
        taken.is_some()
    });
    taken.unwrap()
}

fn code<T: std::fmt::Debug>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Remote(error)) => error.code,
        other => panic!("expected a daemon error, got {other:?}"),
    }
}

fn state(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
) -> hutan_desktop::workbench::model::WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

#[gpui::test]
fn the_window_publishes_its_catalog_and_layout_and_keeps_them_current(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, daemon.connection());
    settle(cx, "the first publish", |_| layout_of(&daemon).is_some());

    let actions: UiActionList = daemon.call(Request::UiActions(UiTarget::default()));
    let ids: Vec<&str> = actions.actions.iter().map(|a| a.id.as_str()).collect();
    for expected in [
        "workbench::SplitRight",
        "workbench::ToggleLeftDock",
        "workbench::ApplyPreset",
        "workbench::OpenPanel",
        "workbench::OpenSession",
        "workbench::OpenWorkspace",
    ] {
        assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
    }
    let preset = actions
        .actions
        .iter()
        .find(|a| a.id == "workbench::ApplyPreset")
        .unwrap();
    let schema = preset.args_schema.as_ref().unwrap();
    assert_eq!(schema["required"], json!(["name"]));
    assert!(
        schema["properties"]["name"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("focus"))
    );
    // The variants of one action are one entry that asks which one.
    let moves = actions
        .actions
        .iter()
        .find(|a| a.id == "workbench::MoveActivePanel")
        .expect("the panel-moving variants are collapsed into one action");
    assert!(
        moves.args_schema.as_ref().unwrap()["properties"]["choice"]["enum"]
            .as_array()
            .unwrap()
            .len()
            > 1
    );
    let font = actions
        .settings
        .iter()
        .find(|s| s.namespace == "appearance" && s.key == "density")
        .expect("settings of the schema are published");
    assert_eq!(font.kind, "enum");
    assert_eq!(font.layers, ["user"]);
    assert!(font.values.contains(&"comfortable".to_owned()));
    assert_eq!(font.current, Some(json!("compact")));

    let layout = layout_of(&daemon).unwrap();
    assert_eq!(layout.window_id, actions.window_id);
    assert_eq!(layout.preset, "default");
    assert_eq!(layout.docks.len(), 3);
    let left = layout.docks.iter().find(|d| d.position == "left").unwrap();
    assert!(left.visible && left.panels.contains(&"sessions".to_owned()));
    assert_eq!(pane_count(&layout.panes), 1);
    assert_eq!(layout.focused.kind, "pane");
    assert_eq!(layout.appearance.density, "compact");

    // A change shows up within the debounce.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            let pane = s.active_pane_id();
            s.add_item(pane, ItemRef::placeholder("Extra")).unwrap();
        });
    });
    settle(cx, "the changed layout", |_| {
        layout_of(&daemon).is_some_and(|l| match l.panes {
            PaneSummary::Pane { items, .. } => items.len() == 2,
            PaneSummary::Split { .. } => false,
        })
    });
}

#[gpui::test]
fn the_window_publishes_again_on_a_new_connection(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let connection = daemon.proxied_connection();
    let (_view, cx) = open(cx, &daemon, connection);
    settle(cx, "the first publish", |_| layout_of(&daemon).is_some());

    daemon.proxy().block(true);
    daemon.proxy().sever();
    // The daemon forgets a window with its connection.
    settle(cx, "the window to be forgotten", |_| {
        layout_of(&daemon).is_none()
    });
    daemon.proxy().block(false);
    settle(cx, "the publish on the new connection", |_| {
        layout_of(&daemon).is_some()
    });
}

#[gpui::test]
fn commands_split_apply_presets_open_panels_focus_sessions_and_write_settings(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let first = daemon.sh("sleep 300");
    let second = daemon.sh("sleep 300");
    let (view, cx) = open(cx, &daemon, daemon.connection());
    settle(cx, "the first publish", |_| layout_of(&daemon).is_some());

    // A split: any palette action by id.
    assert_eq!(state(&view, cx).panes().len(), 1);
    let ran = run(&daemon, cx, "workbench::SplitRight", None).unwrap();
    assert_eq!(ran.result["ran"], "workbench::SplitRight");
    // The split starts a session, then shows it.
    wait_until(cx, "the split", |cx| {
        view.read_with(cx, |wb, _| wb.state().panes().len() == 2)
    });
    settle(cx, "the split in the published layout", |_| {
        layout_of(&daemon).is_some_and(|l| pane_count(&l.panes) == 2)
    });
    run(&daemon, cx, "workbench::ToggleLeftDock", None).unwrap();
    assert!(!state(&view, cx).docks.left.visible);

    // A preset, and an unknown one.
    run(
        &daemon,
        cx,
        "workbench::ApplyPreset",
        Some(json!({"name": "focus"})),
    )
    .unwrap();
    assert_eq!(state(&view, cx).preset, "focus");
    assert_eq!(
        code(run(
            &daemon,
            cx,
            "workbench::ApplyPreset",
            Some(json!({"name": "nope"})),
        )),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(run(&daemon, cx, "workbench::ApplyPreset", None)),
        ErrorCode::BadRequest,
        "the name is required"
    );
    assert_eq!(
        state(&view, cx).preset,
        "focus",
        "refused commands change nothing"
    );

    // A panel.
    assert!(!state(&view, cx).docks.left.visible);
    run(
        &daemon,
        cx,
        "workbench::OpenPanel",
        Some(json!({"id": "sessions"})),
    )
    .unwrap();
    let s = state(&view, cx);
    assert!(s.docks.left.visible);
    assert_eq!(s.docks.left.active, Some(PanelId::new("sessions")));
    assert_eq!(
        code(run(
            &daemon,
            cx,
            "workbench::OpenPanel",
            Some(json!({"id": "no-such-panel"})),
        )),
        ErrorCode::NotFound
    );

    // Focusing sessions: each becomes the active tab.
    for session in [first.id, second.id, first.id] {
        run(
            &daemon,
            cx,
            "workbench::OpenSession",
            Some(json!({"id": session.to_string()})),
        )
        .unwrap();
        let s = state(&view, cx);
        assert_eq!(
            s.active_item(),
            Some(ItemRef::Terminal {
                session: session.to_string()
            })
        );
    }
    assert_eq!(
        code(run(
            &daemon,
            cx,
            "workbench::OpenSession",
            Some(json!({"id": SessionId::new().to_string()})),
        )),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(run(
            &daemon,
            cx,
            "workbench::OpenSession",
            Some(json!({"id": "not an id"})),
        )),
        ErrorCode::BadRequest
    );

    // Variants of one action are chosen by title.
    assert_eq!(
        code(run(&daemon, cx, "workbench::MoveActivePanel", None)),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(run(&daemon, cx, "workbench::NoSuchAction", None)),
        ErrorCode::NotFound,
        "the daemon refuses what the catalog does not list"
    );

    // A setting: validated by the schema, written to the user's settings.toml, applied by hot
    // reload.
    let ran = set(
        &daemon,
        cx,
        "appearance",
        "density",
        json!("comfortable"),
        "user",
    )
    .unwrap();
    assert_eq!(ran.result["value"], "comfortable");
    wait_until(cx, "hot reload", |cx| {
        view.read_with(cx, |wb, _| {
            wb.density() == hutan_desktop::theme::Density::Comfortable
        })
    });
    let file = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();
    assert!(file.contains("comfortable"), "{file}");
    assert_eq!(
        code(set(
            &daemon,
            cx,
            "appearance",
            "density",
            json!("huge"),
            "user"
        )),
        ErrorCode::BadRequest,
        "a value the schema does not allow"
    );
    assert_eq!(
        code(set(&daemon, cx, "appearance", "density", json!(3), "user")),
        ErrorCode::BadRequest
    );
    assert_eq!(
        code(set(&daemon, cx, "appearance", "nope", json!(1), "user")),
        ErrorCode::NotFound
    );
    // The workspace and project layers: only for namespaces a workspace may override, and
    // only while a workspace with a folder is shown.
    assert_eq!(
        code(set(
            &daemon,
            cx,
            "appearance",
            "density",
            json!("compact"),
            "workspace"
        )),
        ErrorCode::BadRequest,
        "`appearance` is not overridable per workspace"
    );
    assert_eq!(
        code(set(
            &daemon,
            cx,
            "terminal",
            "font_size",
            json!(15),
            "elsewhere"
        )),
        ErrorCode::BadRequest,
        "an unknown layer"
    );
    // The workspace layer is the first project folder of a user workspace (ADR 0014): a project
    // that belongs to no workspace has no workspace layer.
    let folder = daemon.home().join("project-folder");
    std::fs::create_dir_all(&folder).unwrap();
    let project = daemon.create_workspace("proj", &[folder.as_path()]);
    let workspace = daemon.create_workspace("team", &[]);
    let _: hutan_protocol::messages::WorkspaceInfo = daemon.call(Request::WorkspaceAssign(
        hutan_protocol::project::WorkspaceAssign {
            project_id: project.id,
            workspace_id: Some(workspace.id),
        },
    ));
    wait_until(cx, "the workspace and its project to arrive", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.workspace(workspace.id).is_some()
                && store
                    .workspace(project.id)
                    .is_some_and(|p| p.parent_id == Some(workspace.id))
        })
    });
    let file = folder.join(".hutan/settings.toml");
    assert!(!file.exists());
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx);
    });
    wait_until(cx, "the workspace folder to apply", |cx| {
        view.read_with(cx, |wb, _| wb.workspace_root().is_some())
    });
    let ran = set(&daemon, cx, "terminal", "font_size", json!(17), "workspace").unwrap();
    assert_eq!(ran.result["layer"], "workspace");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("font_size") && text.contains("17"), "{text}");
    assert!(
        !std::fs::read_to_string(daemon.home().join("settings.toml"))
            .unwrap()
            .contains("font_size"),
        "the user's file is untouched"
    );
    wait_until(cx, "the workspace value to apply", |cx| {
        view.read_with(cx, |wb, _| {
            (wb.settings_store()
                .get::<hutan_desktop::settings::TerminalSettings>()
                .font_size
                - 17.0)
                .abs()
                < f64::EPSILON
        })
    });
    // `project` is the folder of the project of the focused session, whatever workspace shows.
    let other = daemon.home().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    let created: hutan_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
        hutan_protocol::messages::SessionCreate {
            workspace_id: None,
            spec: hutan_protocol::messages::SessionSpec::Terminal(
                hutan_protocol::messages::TerminalSpec {
                    command: vec!["sleep".into(), "300".into()],
                    cwd: Some(other.canonicalize().unwrap()),
                    ..Default::default()
                },
            ),
        },
    ));
    let in_other = created.session;
    // The window opens sessions it knows: wait for the new one to reach it (under load the
    // event can trail the request by more than the 5 s a window has to answer).
    wait_until(cx, "the window to know the new session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(in_other.id).is_some()
        })
    });
    run(
        &daemon,
        cx,
        "workbench::OpenSession",
        Some(json!({"id": in_other.id.to_string()})),
    )
    .unwrap();
    wait_until(cx, "the session to be the focused one", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session() == Some(in_other.id))
    });
    set(&daemon, cx, "terminal", "font_size", json!(18), "project").unwrap();
    let project_file = other.canonicalize().unwrap().join(".hutan/settings.toml");
    let text = std::fs::read_to_string(&project_file).unwrap();
    assert!(text.contains("font_size") && text.contains("18"), "{text}");
    assert!(
        !std::fs::read_to_string(&file).unwrap().contains("18"),
        "the workspace's own file is untouched"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.density()),
        hutan_desktop::theme::Density::Comfortable
    );
}

/// The arguments an action asks for, from its published schema: the first value of an enum, or
/// the id of something that exists. `Err` says why the test cannot supply them.
fn arguments_for(
    action: &hutan_protocol::ui::UiAction,
    session: SessionId,
    workspace: hutan_core::WorkspaceId,
) -> Result<Option<Value>, String> {
    let Some(schema) = &action.args_schema else {
        return Ok(None);
    };
    let mut args = serde_json::Map::new();
    for key in schema["required"].as_array().into_iter().flatten() {
        let key = key.as_str().unwrap_or_default();
        let property = &schema["properties"][key];
        let value = if let Some(first) = property["enum"].as_array().and_then(|e| e.first()) {
            first.clone()
        } else if action.id == "workbench::OpenSession" && key == "id" {
            json!(session.to_string())
        } else if action.id == "workbench::OpenWorkspace" && key == "id" {
            json!(workspace.to_string())
        } else {
            return Err(format!("no way to supply `{key}`"));
        };
        args.insert(key.to_owned(), value);
    }
    Ok(Some(Value::Object(args)))
}

#[gpui::test]
fn every_action_the_window_lists_dispatches(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // A live session: stopping or restarting the daemon, or quitting, asks first instead of
    // doing it.
    let session = daemon.sh("sleep 300");
    let (view, cx) = open(cx, &daemon, daemon.connection());
    // Open Logs reveals the log in Finder; the test platform cannot.
    view.update(cx, |wb, _| wb.set_reveal_handler(|_, _| {}));
    settle(cx, "the first publish", |_| layout_of(&daemon).is_some());
    view.update_in(cx, |wb, window, cx| wb.open_session(session.id, window, cx));
    let workspace = session.workspace_id;
    let actions: UiActionList = daemon.call(Request::UiActions(UiTarget::default()));
    assert!(
        actions
            .actions
            .iter()
            .any(|a| a.id == "workbench::OpenAssistant"),
        "the assistant is on by default, so its action is listed"
    );

    let mut unknown = Vec::new();
    let mut skipped = Vec::new();
    let mut ran = 0;
    for action in &actions.actions {
        // GPUI's test window cannot be minimized or zoomed (`unimplemented!`).
        if matches!(
            action.id.as_str(),
            "workbench::MinimizeWindow" | "workbench::ZoomWindow"
        ) {
            skipped.push(format!(
                "{}: the test platform has no such window",
                action.id
            ));
            continue;
        }
        let args = match arguments_for(action, session.id, workspace) {
            Ok(args) => args,
            Err(why) => {
                skipped.push(format!("{}: {why}", action.id));
                continue;
            }
        };
        match run(&daemon, cx, &action.id, args) {
            Ok(answer) => {
                assert_eq!(answer.result["ran"], action.id.as_str());
                ran += 1;
            }
            Err(ClientError::Remote(error)) => {
                // A refusal is an answer; "unknown action" is a listed action nobody handles.
                if error.code == ErrorCode::NotFound || error.message.contains("unknown action") {
                    unknown.push(format!("{}: {}", action.id, error.message));
                }
                ran += 1;
            }
            Err(other) => panic!("{}: {other}", action.id),
        }
        // Whatever the action opened (a modal, the palette, the settings) closes again.
        cx.simulate_keystrokes("escape escape");
        view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    }
    assert!(unknown.is_empty(), "listed but unknown: {unknown:?}");
    assert!(ran > 90, "only {ran} actions ran; skipped: {skipped:?}");
    // Only what the test platform cannot do (a reason is recorded for each).
    assert_eq!(skipped.len(), 2, "unexpectedly skipped: {skipped:?}");
}

#[gpui::test]
fn the_assistant_action_follows_the_assistant_setting_in_both_directions(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon, daemon.connection());
    settle(cx, "the first publish", |_| layout_of(&daemon).is_some());
    let listed = |daemon: &TestDaemon| {
        let actions: UiActionList = daemon.call(Request::UiActions(UiTarget::default()));
        actions
            .actions
            .iter()
            .any(|a| a.id == "workbench::OpenAssistant")
    };
    assert!(listed(&daemon));
    run(&daemon, cx, "workbench::OpenAssistant", None).unwrap();
    assert!(view.read_with(cx, |wb, _| wb.panel_open("assistant")));

    set(&daemon, cx, "assistant", "enabled", json!(false), "user").unwrap();
    assert!(
        !listed(&daemon),
        "the catalog is current when the setting is answered"
    );
    assert_eq!(
        code(run(&daemon, cx, "workbench::OpenAssistant", None)),
        ErrorCode::NotFound
    );

    set(&daemon, cx, "assistant", "enabled", json!(true), "user").unwrap();
    // No waiting for the debounce: the answer to the setting came after the publish.
    assert!(
        listed(&daemon),
        "the catalog is current when the setting is answered"
    );
    run(&daemon, cx, "workbench::OpenAssistant", None).unwrap();
    assert!(view.read_with(cx, |wb, _| wb.panel_open("assistant")));
}
