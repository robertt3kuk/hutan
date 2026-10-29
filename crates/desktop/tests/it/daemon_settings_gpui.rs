#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The daemon's settings in the settings sheet (ST-3): the pages drawn from
//! `daemon.settings.get`, what a change does (live, or after a restart with the banner), the
//! backend and model of the assistants with Inherit, the permissions, the errors of the
//! daemon, a daemon without the capability — and the keyboard and the empty search of the
//! sheet. Daemons are in-process on a temporary home, or a real `hutand` where one is
//! restarted.

use crate::common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use common::assistant::{
    AssistantEnv, Options, PATH, eventually, last_prompt, open_assistant, reply, shown, wait_answer,
};
use common::{ProcessDaemon, TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_client::{Connection, ConnectionState};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::chat::ContextChip;
use hutan_desktop::workbench::panels::{AssistantPanel, ScopeKey};
use hutan_desktop::workbench::settings_labels::daemon_wording_keys;
use hutan_desktop::workbench::settings_schema::FieldKind;
use hutan_desktop::workbench::{DaemonBusy, Region, SheetFocus, Workbench};
use hutan_protocol::assistant::{
    AssistantBackend, AssistantList, AssistantListRequest, AssistantOpen, AssistantScope,
};
use hutan_protocol::daemon_settings::Applies;
use hutan_protocol::messages::{Request, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

fn store(dir: &Path) -> SettingsStore {
    common::with_assistant(SettingsStore::new(SettingsPaths::in_dir(dir)))
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, store(daemon.home()), daemon.connection(), None);
    wait_synced(&view, cx);
    (view, cx)
}

fn open_page(view: &Entity<Workbench>, cx: &mut VisualTestContext, page: &str) {
    cx.dispatch_action(actions::OpenSettingsFor(page.into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some(page.to_owned())
    );
}

/// Waits until the daemon's list is in the window.
fn wait_listed(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the daemon settings", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_some())
    });
}

fn value(view: &Entity<Workbench>, cx: &VisualTestContext, key: &str) -> Value {
    view.read_with(cx, |wb, _| {
        wb.daemon_settings()
            .value(key)
            .cloned()
            .unwrap_or(Value::Null)
    })
}

fn wait_value(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, want: Value) {
    wait_until(cx, &format!("{key} to be {want}"), |cx| {
        value(view, cx, key) == want
    });
}

fn daemon_toml(path: &Path) -> String {
    std::fs::read_to_string(path.join("daemon.toml")).unwrap_or_default()
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn edit_text(view: &Entity<Workbench>, cx: &mut VisualTestContext, key: &str, text: &str) {
    view.update(cx, |wb, cx| wb.daemon_begin_edit(key, cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text(text, cx));
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
}

fn error_of(view: &Entity<Workbench>, cx: &VisualTestContext, key: &str) -> Option<String> {
    view.read_with(cx, |wb, _| {
        wb.daemon_settings().error(key).map(str::to_owned)
    })
}

// ---- the pages ----------------------------------------------------------------------------------

#[gpui::test]
fn the_notifications_page_lists_the_daemons_values_and_a_live_change_needs_no_restart(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "notifications");
    wait_listed(&view, cx);

    // The daemon's list is what the page shows: value, default, when a change counts.
    view.read_with(cx, |wb, _| {
        let state = wb.daemon_settings();
        let enabled = state.entry("notify.enabled").unwrap();
        assert_eq!(enabled.value, json!(true));
        assert!(!enabled.set_in_file);
        assert_eq!(enabled.applies, Applies::Live);
        assert_eq!(
            state.entry("notify.native").unwrap().applies,
            Applies::Restart
        );
        assert_eq!(
            state
                .value("notify.kinds")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert_eq!(state.value("notify.min_turn_secs"), Some(&json!(20)));
        assert!(!state.restart_pending());
    });
    // Every field of the page, in the order its sections are drawn, is a row of the keyboard's
    // list: General's own, the Notifications section's, then Sessions & restore's.
    let rows = view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx));
    assert_eq!(
        rows,
        [
            "f:keep_running",
            "f:start_at_login",
            "f:setup_done",
            "f:ask_notifications",
            "f:ask_hooks",
            "f:on_last_window_close",
            "f:menu_bar",
            "f:dock_badge",
            "f:usage_warnings",
            "d:notify.enabled",
            "d:notify.native",
            "d:notify.sound",
            "d:notify.finished",
            "d:notify.kinds",
            "d:notify.min_turn_secs",
            "d:notify.expire_secs",
            "d:daemon.restore_sessions",
            "d:workspaces.home_folder",
            "d:terminal.env_inherit",
            "d:projects.auto",
            "d:worktrees.cleanup",
            "d:worktrees.cleanup_idle_days",
            "d:worktrees.wait_for_setup"
        ]
    );

    // Turning it off writes daemon.toml and applies at once: no banner.
    view.update(cx, |wb, cx| wb.daemon_toggle("notify.enabled", cx));
    wait_value(&view, cx, "notify.enabled", json!(false));
    assert!(
        daemon_toml(daemon.home()).contains("enabled = false"),
        "{}",
        daemon_toml(daemon.home())
    );
    view.read_with(cx, |wb, _| {
        let entry = wb
            .daemon_settings()
            .entry("notify.enabled")
            .unwrap()
            .clone();
        assert!(entry.set_in_file && !entry.pending_restart);
        assert!(!wb.daemon_settings().restart_pending());
    });
    assert!(!drawn(&view, cx, Region::Settings("restart-now")));

    // Reset removes the key: back to the default.
    view.update(cx, |wb, cx| wb.daemon_setting_reset("notify.enabled", cx));
    wait_value(&view, cx, "notify.enabled", json!(true));
    assert!(!daemon_toml(daemon.home()).contains("enabled = false"));

    // A list of choices: remove one, add it back.
    view.update(cx, |wb, cx| wb.daemon_list_remove("notify.kinds", 0, cx));
    wait_until(cx, "seven kinds", |cx| {
        value(&view, cx, "notify.kinds").as_array().unwrap().len() == 7
    });
    assert!(!daemon_toml(daemon.home()).contains("\"approval_required\""));
    view.update(cx, |wb, cx| {
        wb.daemon_list_add("notify.kinds", "approval_required", cx)
    });
    wait_until(cx, "eight kinds", |cx| {
        value(&view, cx, "notify.kinds").as_array().unwrap().len() == 8
    });
    // Only the choices go in, each once.
    view.update(cx, |wb, cx| {
        wb.daemon_list_add("notify.kinds", "nonsense", cx)
    });
    view.update(cx, |wb, cx| {
        wb.daemon_list_add("notify.kinds", "task_done", cx)
    });
    cx.run_until_parked();
    assert_eq!(
        value(&view, cx, "notify.kinds").as_array().unwrap().len(),
        8
    );

    // A number typed in its box.
    edit_text(&view, cx, "notify.min_turn_secs", "45");
    wait_value(&view, cx, "notify.min_turn_secs", json!(45));
    assert!(daemon_toml(daemon.home()).contains("min_turn_secs = 45"));
}

#[gpui::test]
fn the_daemon_pages_are_searchable_and_every_key_has_wording(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start_with_config(
        "[mcp.docs]\ncommand = \"/bin/echo\"\nargs = [\"hi\"]\n\n[assistant.workspaces.proj]\nfiles = false\n",
    );
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "agents_tools");
    wait_listed(&view, cx);

    let wording = daemon_wording_keys();
    let keys: Vec<String> = view.read_with(cx, |wb, _| {
        wb.daemon_settings()
            .settings()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.key.clone())
            .collect()
    });
    assert!(keys.iter().any(|k| k == "mcp.docs"));
    assert!(keys.iter().any(|k| k == "assistant.workspaces.proj.files"));
    for key in &keys {
        if key.starts_with("mcp.") || key.starts_with("assistant.workspaces.") {
            continue;
        }
        assert!(wording.contains(&key.as_str()), "{key} has no wording");
    }

    // The search finds a page by what its daemon settings are called.
    for (query, page) in [
        ("notification centre", "app"),
        ("restore", "app"),
        ("approval", "agents_tools"),
        ("claude-agent-acp", "agents_tools"),
        ("how far back", "history"),
        ("terminal output", "assistant"),
    ] {
        view.update(cx, |wb, cx| wb.set_settings_query(query, cx));
        let pages = view.read_with(cx, |wb, _| wb.sheet_pages());
        assert!(pages.contains(&page), "{query}: {pages:?}");
    }
    // The MCP server is a read-only table with the way to the file.
    view.update(cx, |wb, cx| wb.set_settings_query("", cx));
    let table = view.read_with(cx, |wb, _| wb.daemon_settings().entry("mcp.docs").cloned());
    assert_eq!(
        table.unwrap().value,
        json!({"command": "/bin/echo", "args": ["hi"]})
    );
    let opened = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let sink = opened.clone();
    view.update(cx, |wb, _| {
        wb.set_open_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });
    view.update(cx, |wb, cx| wb.open_daemon_toml(cx));
    assert_eq!(*opened.borrow(), vec![daemon.home().join("daemon.toml")]);
}

// ---- restart ------------------------------------------------------------------------------------

#[gpui::test]
fn a_restart_field_shows_the_banner_and_restart_now_restarts_the_daemon(cx: &mut TestAppContext) {
    let process = ProcessDaemon::new();
    // The daemon starts with the first call.
    let _ = process.create_session(&["true"]);
    let config = process.dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    let connection = Connection::start(process.options());
    let (view, cx) = open_with(cx, store(&config), connection, None);
    wait_until(cx, "the connection", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && wb.sessions().read(cx).synced()
        })
    });
    open_page(&view, cx, "sessions_restore");
    wait_listed(&view, cx);
    assert!(!drawn(&view, cx, Region::Settings("restart-now")));
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);

    view.update(cx, |wb, cx| {
        wb.daemon_choose("daemon.restore_sessions", "stopped", cx);
    });
    wait_until(cx, "the pending restart", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().restart_pending())
    });
    view.read_with(cx, |wb, _| {
        let entry = wb
            .daemon_settings()
            .entry("daemon.restore_sessions")
            .unwrap();
        assert_eq!(entry.applies, Applies::Restart);
        assert!(entry.pending_restart);
    });
    assert!(
        drawn(&view, cx, Region::Settings("restart-now")),
        "the banner"
    );
    // The banner is on every page.
    open_page(&view, cx, "appearance");
    assert!(drawn(&view, cx, Region::Settings("restart-now")));

    click(&view, cx, Region::Settings("restart-now"));
    wait_until(cx, "the daemon to restart", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.daemon_busy().is_none()
                && wb.sessions().read(cx).connection_state() == ConnectionState::Connected
                && wb.sessions().read(cx).state().epoch != epoch
                && !wb.daemon_settings().restart_pending()
        })
    });
    assert_eq!(
        value(&view, cx, "daemon.restore_sessions"),
        json!("stopped")
    );
    assert!(
        !drawn(&view, cx, Region::Settings("restart-now")),
        "the banner is gone"
    );
    assert!(view.read_with(cx, |wb, _| wb.daemon_busy() != Some(DaemonBusy::Restarting)));
}

// ---- the assistant ------------------------------------------------------------------------------

fn assistants(env: &AssistantEnv, workspace: hutan_core::WorkspaceId) -> AssistantList {
    env.daemon
        .call(Request::AssistantList(AssistantListRequest {
            scope: Some(AssistantScope::Workspace),
            workspace_id: Some(workspace),
        }))
}

#[gpui::test]
fn a_workspace_backend_is_chosen_then_inherited_and_automatic_stores_no_model(
    cx: &mut TestAppContext,
) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let root = env.project("proj");
    let workspace = env.daemon.create_workspace("proj", &[&root]);
    // The workspace's assistant exists, on the provider `daemon.toml` names.
    let _: SessionResult = env.daemon.call(Request::AssistantOpen(AssistantOpen {
        scope: AssistantScope::Workspace,
        workspace_id: Some(workspace.id),
        new: false,
    }));
    let (view, cx) = common::assistant::open(cx, &env.daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace.id, window, cx)
    });
    open_page(&view, cx, "assistant");
    wait_listed(&view, cx);
    let scope = ScopeKey::Workspace(workspace.id);
    wait_until(cx, "the workspace to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.assistant_scopes(cx) == [ScopeKey::Global, scope]
        })
    });
    wait_until(cx, "what the assistant runs on", |cx| {
        view.read_with(cx, |wb, _| wb.assistant_defaults().running(scope).is_some())
    });
    view.read_with(cx, |wb, _| {
        let running = wb.assistant_defaults().running(scope).unwrap();
        assert_eq!(running.backend, AssistantBackend::Provider);
        assert_eq!(running.model, "m-1");
    });
    assert!(
        assistants(&env, workspace.id)
            .assistants
            .iter()
            .any(|a| a.default)
    );

    // Codex for the workspace: no assistant of the workspace runs on it, so none is default.
    view.update(cx, |wb, cx| {
        wb.assistant_choose_backend(scope, AssistantBackend::Codex, cx);
    });
    wait_until(cx, "the choice to be stored", |cx| {
        view.read_with(cx, |wb, _| {
            wb.assistant_defaults()
                .chosen(scope)
                .is_some_and(|c| c.is_some_and(|c| c.backend == AssistantBackend::Codex))
        })
    });
    assert!(
        assistants(&env, workspace.id)
            .assistants
            .iter()
            .all(|a| !a.default),
        "the provider assistant is not the default of a workspace set to Codex"
    );

    // Inherit: `assistant.reset`; the inherited backend (the provider of daemon.toml) answers.
    view.update(cx, |wb, cx| wb.assistant_inherit(scope, cx));
    wait_until(cx, "the reset", |cx| {
        view.read_with(cx, |wb, _| {
            wb.assistant_defaults().chosen(scope) == Some(None)
        })
    });
    let list = assistants(&env, workspace.id);
    let default = list
        .assistants
        .iter()
        .find(|a| a.default)
        .expect("a default");
    assert_eq!(default.backend, AssistantBackend::Provider);

    // A model, then Automatic: the daemon is told no model.
    view.update(cx, |wb, cx| {
        wb.assistant_configure(
            scope,
            AssistantBackend::Provider,
            Some("mock".into()),
            Some("m-2".into()),
            cx,
        );
    });
    wait_until(cx, "the model to be stored", |cx| {
        view.read_with(cx, |wb, _| {
            wb.assistant_defaults()
                .chosen(scope)
                .is_some_and(|c| c.is_some_and(|c| c.model.as_deref() == Some("m-2")))
        })
    });
    view.update(cx, |wb, cx| wb.assistant_choose_model(scope, None, cx));
    wait_until(cx, "Automatic to be stored", |cx| {
        view.read_with(cx, |wb, _| {
            wb.assistant_defaults().chosen(scope).is_some_and(|c| {
                c.is_some_and(|c| c.model.is_none() && c.provider.as_deref() == Some("mock"))
            })
        })
    });
    // Global is untouched by what the workspace did.
    view.read_with(cx, |wb, _| {
        assert_eq!(wb.assistant_defaults().chosen(ScopeKey::Global), None);
    });
}

#[gpui::test]
fn the_global_files_permission_is_written_and_decides_the_next_attachment(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    env.mock.route("POST", PATH, reply("read it"));
    let (view, cx) = common::assistant::open(cx, &env.daemon);
    let home = view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).home_workspace().unwrap().roots[0].clone()
    });
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("notes.txt"), "a note about herons").unwrap();
    let panel: Entity<AssistantPanel> = open_assistant(&view, cx);
    let (_, model, chat) = shown(&panel, cx);

    // Off by default: refused, naming the setting.
    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::file("notes.txt", None), cx);
    });
    model.update(cx, |m, cx| {
        m.edit(
            |input| {
                input.clear();
                input.insert("read my notes");
            },
            cx,
        );
    });
    assert!(chat.update(cx, |v, cx| v.send(cx)));
    wait_until(cx, "the refusal", |cx| {
        model.read_with(cx, |m, _| m.error().is_some())
    });
    let error = model.read_with(cx, |m, _| m.error().unwrap().to_string());
    assert!(error.contains("assistant.global_context.files"), "{error}");

    // Allow it in Settings: the file says so, and the next message is read.
    open_page(&view, cx, "assistant");
    wait_listed(&view, cx);
    view.update(cx, |wb, cx| {
        wb.daemon_toggle("assistant.global_context.files", cx);
    });
    wait_value(&view, cx, "assistant.global_context.files", json!(true));
    assert!(
        daemon_toml(env.daemon.home()).contains("[assistant.global_context]"),
        "{}",
        daemon_toml(env.daemon.home())
    );
    view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
    // The refused chip is marked; a fresh one is read.
    model.update(cx, |m, cx| m.remove_context(0, cx));
    model.update(cx, |m, cx| {
        m.attach_context(ContextChip::file("notes.txt", None), cx);
    });
    assert!(chat.update(cx, |v, cx| v.send(cx)));
    wait_answer(&model, cx, "read it");
    eventually("the provider to get the prompt", || {
        !env.mock.requests_to(PATH).is_empty()
    });
    assert!(last_prompt(&env.mock).contains("a note about herons"));
}

// ---- errors, a broken file, an older daemon -----------------------------------------------------

#[gpui::test]
fn a_refusal_shows_at_its_field_and_a_broken_file_shows_the_banner_with_open(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "notifications");
    wait_listed(&view, cx);

    // Validation: the daemon's own words, at the field; the box keeps the text.
    edit_text(&view, cx, "notify.min_turn_secs", "-5");
    wait_until(cx, "the refusal", |cx| {
        error_of(&view, cx, "notify.min_turn_secs").is_some()
    });
    let error = error_of(&view, cx, "notify.min_turn_secs").unwrap();
    assert!(error.contains("at least 0"), "{error}");
    assert_eq!(value(&view, cx, "notify.min_turn_secs"), json!(20));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().edit().is_some()));
    // Not a number: said before the daemon is asked.
    edit_text(&view, cx, "notify.min_turn_secs", "soon");
    assert!(
        error_of(&view, cx, "notify.min_turn_secs")
            .unwrap()
            .contains("whole number")
    );
    // A new, valid value clears the error.
    edit_text(&view, cx, "notify.min_turn_secs", "30");
    wait_value(&view, cx, "notify.min_turn_secs", json!(30));
    assert!(error_of(&view, cx, "notify.min_turn_secs").is_none());

    // A path that is not there.
    open_page(&view, cx, "sessions_restore");
    view.update(cx, |wb, cx| {
        wb.daemon_setting_set("workspaces.home_folder", Some(json!("/no/such/folder")), cx);
    });
    wait_until(cx, "the path to be refused", |cx| {
        error_of(&view, cx, "workspaces.home_folder").is_some()
    });
    assert!(
        error_of(&view, cx, "workspaces.home_folder")
            .unwrap()
            .contains("not usable")
    );

    // A file nobody can read: the daemon says where, and the banner offers the file.
    std::fs::write(daemon.home().join("daemon.toml"), "[notify\nenabled = \n").unwrap();
    view.update(cx, |wb, cx| wb.refresh_daemon_settings(cx));
    wait_until(cx, "the problem", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().problem().is_some())
    });
    let problem = view.read_with(cx, |wb, _| {
        wb.daemon_settings().problem().unwrap().to_owned()
    });
    assert!(problem.contains("daemon.toml"), "{problem}");
    assert!(problem.contains("line"), "{problem}");
    let opened = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let sink = opened.clone();
    view.update(cx, |wb, _| {
        wb.set_open_handler(move |path, _| sink.borrow_mut().push(path.to_owned()));
    });
    click(&view, cx, Region::Settings("open-daemon-toml"));
    assert_eq!(*opened.borrow(), vec![daemon.home().join("daemon.toml")]);

    // Fixing the file and asking again brings the page back.
    std::fs::write(
        daemon.home().join("daemon.toml"),
        "[notify]\nenabled = false\n",
    )
    .unwrap();
    view.update(cx, |wb, cx| wb.refresh_daemon_settings(cx));
    wait_until(cx, "the problem to go", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().problem().is_none())
    });
    assert!(!drawn(&view, cx, Region::Settings("open-daemon-toml")));
    assert_eq!(value(&view, cx, "notify.enabled"), json!(false));
}

#[gpui::test]
fn an_older_daemon_gets_the_update_note_and_the_file_path(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let connection = daemon.older_connection();
    let (view, cx) = open_with(cx, store(daemon.home()), connection, None);
    wait_synced(&view, cx);
    assert!(!view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .connection()
            .supports_daemon_settings()
    }));

    open_page(&view, cx, "notifications");
    wait_until(cx, "the capability to be found missing", |cx| {
        view.read_with(cx, |wb, _| wb.daemon_settings().unsupported())
    });
    // No daemon rows to change (the Dock badge is the desktop's own); the way to the file instead.
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx)),
        [
            "f:keep_running",
            "f:start_at_login",
            "f:setup_done",
            "f:ask_notifications",
            "f:ask_hooks",
            "f:on_last_window_close",
            "f:menu_bar",
            "f:dock_badge",
            "f:usage_warnings"
        ]
    );
    assert!(drawn(&view, cx, Region::Settings("open-daemon-toml")));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.daemon_toml_path()),
        Some(daemon.home().join("daemon.toml"))
    );
    // Nothing is sent to a daemon that cannot take it.
    view.update(cx, |wb, cx| wb.daemon_toggle("notify.enabled", cx));
    cx.run_until_parked();
    assert!(!daemon_toml(daemon.home()).contains("enabled = false"));
    assert!(view.read_with(cx, |wb, _| wb.daemon_settings().settings().is_none()));
}

// ---- the keyboard ---------------------------------------------------------------------------------

fn zone(view: &Entity<Workbench>, cx: &VisualTestContext) -> SheetFocus {
    view.read_with(cx, |wb, _| wb.sheet_focus_zone().unwrap())
}

fn page(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().namespace().to_owned()
    })
}

#[gpui::test]
fn tab_moves_between_search_pages_and_controls_and_arrows_move_inside(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    assert_eq!(zone(&view, cx), SheetFocus::Search);

    cx.simulate_keystrokes("tab");
    assert_eq!(zone(&view, cx), SheetFocus::Categories);
    // The arrow keys move in the list of pages and select the page.
    let first = page(&view, cx);
    cx.simulate_keystrokes("down");
    let second = page(&view, cx);
    assert_ne!(first, second);
    cx.simulate_keystrokes("down");
    assert_ne!(page(&view, cx), second);
    cx.simulate_keystrokes("up up");
    assert_eq!(page(&view, cx), first);
    cx.simulate_keystrokes("end");
    let last = view.read_with(cx, |wb, _| *wb.sheet_pages().last().unwrap());
    assert_eq!(page(&view, cx), last);
    cx.simulate_keystrokes("home");
    assert_eq!(page(&view, cx), first);

    // Controls, then back to the search box, and Shift-Tab goes the other way.
    cx.simulate_keystrokes("tab");
    assert_eq!(zone(&view, cx), SheetFocus::Controls);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned)),
        view.read_with(cx, |wb, cx| wb.sheet_focus_rows(cx).first().cloned())
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(zone(&view, cx), SheetFocus::Search);
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(zone(&view, cx), SheetFocus::Controls);
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(zone(&view, cx), SheetFocus::Categories);
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(zone(&view, cx), SheetFocus::Search);

    // Typing anywhere is searching.
    cx.simulate_keystrokes("tab");
    cx.simulate_input("dens");
    assert_eq!(zone(&view, cx), SheetFocus::Search);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().query().to_owned()),
        "dens"
    );
}

#[gpui::test]
fn an_open_dropdown_is_moved_with_the_arrows_picked_with_return_and_closed_with_escape(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "appearance");
    let options = view
        .read_with(cx, |wb, _| wb.sheet_kind("density"))
        .and_then(|k| match k {
            FieldKind::Enum(values) => Some(values),
            _ => None,
        })
        .expect("density is a choice");
    let current = value_of(&view, cx, "density");
    let at = options.iter().position(|o| *o == current).unwrap();
    let next = options[(at + 1) % options.len()].clone();
    assert!(options.len() > 1);

    // Tab to the controls, Down to the row, Return opens the choice.
    cx.simulate_keystrokes("tab tab");
    for _ in 0..30 {
        if view.read_with(cx, |wb, _| wb.sheet_focused_row() == Some("f:density")) {
            break;
        }
        cx.simulate_keystrokes("down");
    }
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_focused_row().map(str::to_owned)),
        Some("f:density".to_owned())
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .dropdown()
            .map(str::to_owned)),
        Some("density".to_owned())
    );

    // Escape closes it and changes nothing; the sheet stays.
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_none()
    }));
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().is_some()));
    assert_eq!(value_of(&view, cx, "density"), current);

    // Down moves from the current value, Return selects.
    cx.simulate_keystrokes("enter down");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_dropdown_cursor().map(str::to_owned)),
        Some(next.clone())
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(value_of(&view, cx, "density"), next);
    assert!(view.read_with(cx, |wb, _| {
        wb.settings_sheet().unwrap().dropdown().is_none()
    }));
    // Up goes back.
    cx.simulate_keystrokes("enter up enter");
    assert_eq!(value_of(&view, cx, "density"), current);
}

fn value_of(view: &Entity<Workbench>, cx: &VisualTestContext, field: &str) -> String {
    view.read_with(cx, |wb, _| {
        wb.sheet_value(field)
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default()
    })
}

#[gpui::test]
fn a_daemon_dropdown_is_driven_by_the_keyboard_too(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "control");
    wait_listed(&view, cx);
    assert_eq!(value(&view, cx, "control.agents"), json!("ask"));
    cx.simulate_keystrokes("tab tab enter");
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .unwrap()
            .dropdown()
            .map(str::to_owned)),
        Some("daemon:control.agents".to_owned())
    );
    // ask → allow
    cx.simulate_keystrokes("down enter");
    wait_value(&view, cx, "control.agents", json!("allow"));
    assert!(daemon_toml(daemon.home()).contains("agents = \"allow\""));
}

// ---- an empty search -----------------------------------------------------------------------------

#[gpui::test]
fn a_search_that_matches_nothing_says_so_instead_of_showing_a_page(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    open_page(&view, cx, "terminal");
    view.update(cx, |wb, cx| wb.set_settings_query("zzzzqqqq", cx));
    draw(cx);
    assert!(view.read_with(cx, |wb, _| wb.sheet_pages().is_empty()));
    // The page that was selected is not drawn; the note and its way out are.
    assert!(drawn(&view, cx, Region::Settings("clear-search")));
    assert!(!drawn(&view, cx, Region::Settings("reset-page")));
    assert_eq!(
        hutan_desktop::workbench::settings_labels::words::no_match("zzzzqqqq"),
        "No settings match “zzzzqqqq”"
    );
    // Tab has nowhere to go but the search box.
    cx.simulate_keystrokes("tab");
    assert_eq!(zone(&view, cx), SheetFocus::Search);

    click(&view, cx, Region::Settings("clear-search"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().query().to_owned()),
        ""
    );
    assert!(!view.read_with(cx, |wb, _| wb.sheet_pages().is_empty()));
    assert!(!drawn(&view, cx, Region::Settings("clear-search")));
}

// ---- the automatic backend -----------------------------------------------------------------------

#[gpui::test]
fn an_automatic_backend_says_so_in_the_header_and_a_choice_does_not(cx: &mut TestAppContext) {
    // `[assistant]` names nothing: the provider with a key is the automatic backend.
    let env = AssistantEnv::start(Options::default());
    let (view, cx) = common::assistant::open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the header", |cx| {
        panel.read_with(cx, |p, cx| p.backend_label(cx).starts_with("Automatic"))
    });
    assert_eq!(
        panel.read_with(cx, |p, cx| p.backend_label(cx)),
        "Automatic · mock · m-1"
    );
    // The Settings page says the same.
    open_page(&view, cx, "assistant");
    wait_listed(&view, cx);
    wait_until(cx, "what runs now", |cx| {
        view.read_with(cx, |wb, _| {
            wb.assistant_defaults().running(ScopeKey::Global).is_some()
        })
    });
    assert!(view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).assistant_is_automatic(None)
    }));

    // Chosen in Settings: it is a choice now.
    view.update(cx, |wb, cx| {
        wb.assistant_configure(
            ScopeKey::Global,
            AssistantBackend::Provider,
            Some("mock".into()),
            Some("m-1".into()),
            cx,
        );
    });
    wait_until(cx, "the header to stop saying Automatic", |cx| {
        panel.read_with(cx, |p, cx| p.backend_label(cx) == "mock · m-1")
    });
    // Inherit hands it back.
    view.update(cx, |wb, cx| wb.assistant_inherit(ScopeKey::Global, cx));
    wait_until(cx, "Automatic again", |cx| {
        panel.read_with(cx, |p, cx| p.backend_label(cx).starts_with("Automatic · "))
    });
}

#[gpui::test]
fn a_backend_named_in_daemon_toml_is_not_automatic(cx: &mut TestAppContext) {
    let env = AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    });
    let (view, cx) = common::assistant::open(cx, &env.daemon);
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the header", |cx| {
        panel.read_with(cx, |p, cx| p.backend_label(cx) == "mock · m-1")
    });
}
