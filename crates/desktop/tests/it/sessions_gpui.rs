#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GPUI tests against an in-process daemon: live sessions, terminal items, panels, attention,
//! lifecycle commands, settings hot reload, keymap, settings UI, and connection status.

use crate::common;

use std::path::Path;

use common::{TestDaemon, draw, open_with, screen_text, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_client::ConnectionState;
use hutan_core::{SessionId, SessionState};
use hutan_desktop::sessions::Attention;
use hutan_desktop::theme::Density;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{ItemRef, LayoutPreset};
use hutan_desktop::workbench::panels::{InspectorPanel, SessionsPanel};
use hutan_desktop::workbench::{Workbench, connection_label};
use hutan_protocol::messages::{Request, SessionInfo};
use hutan_settings::{SettingsPaths, SettingsStore};
use hutan_terminal_view::ViewStatus;

fn settings_for(daemon: &TestDaemon) -> SettingsStore {
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    open_with(cx, settings_for(daemon), daemon.connection(), None)
}

fn wait_synced(view: &Entity<Workbench>, cx: &mut VisualTestContext, sessions: usize) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.sessions().len() == sessions
        })
    });
}

fn open_session(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
}

fn screen_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
) -> Option<String> {
    view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(id)
            .map(|v| screen_text(&v.read(cx).model()))
    })
}

fn wait_screen(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId, needle: &str) {
    let mut last = String::new();
    let needle = needle.to_owned();
    wait_until(cx, &format!("`{needle}` on the screen"), |cx| {
        last = screen_of(view, cx, id).unwrap_or_default();
        last.contains(&needle)
    });
}

fn state_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
) -> hutan_desktop::workbench::model::WorkbenchState {
    view.read_with(cx, |wb, _| wb.state().clone())
}

fn session_state(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
) -> Option<SessionState> {
    view.read_with(cx, |wb, cx| {
        wb.sessions().read(cx).session(id).map(|s| s.state.clone())
    })
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).unwrap();
}

#[gpui::test]
fn a_session_item_shows_the_daemons_snapshot_and_takes_focus(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo hello-from-hutan; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);

    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "hello-from-hutan");

    // The welcome placeholder gave way to the session; its tab shows the session title.
    let state = state_of(&view, cx);
    assert_eq!(
        state.active_pane().unwrap().items,
        vec![ItemRef::Terminal {
            session: info.id.to_string()
        }]
    );
    // The window never repeats the tab's title: it names the project and the app.
    let window_title = cx.window_title().unwrap_or_default();
    assert!(
        window_title.ends_with("Hutan") && !window_title.contains(&info.title),
        "{window_title}"
    );
    // The terminal has keyboard focus and the store knows which session is looked at.
    let focused = view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(info.id).map(|v| v.focus_handle(cx))
    });
    cx.update(|window, _| assert!(focused.unwrap().is_focused(window)));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).focused()),
        Some(info.id)
    );

    // Typing reaches the PTY: the shell script's `sleep` ignores it, so use a fresh cat.
    let echo = daemon.create_session(&["cat"], None);
    wait_synced(&view, cx, 2);
    open_session(&view, cx, echo.id);
    wait_until(cx, "the cat session to attach", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(echo.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });
    cx.simulate_input("typed-by-test\n");
    wait_screen(&view, cx, echo.id, "typed-by-test");
}

#[gpui::test]
fn the_sessions_panel_lists_groups_and_filters(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = daemon.create_workspace("api", &[ws_dir.path()]);
    let make = |title: &str, ws: Option<hutan_core::WorkspaceId>| {
        let info: hutan_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
            hutan_protocol::messages::SessionCreate {
                workspace_id: ws,
                spec: hutan_protocol::messages::SessionSpec::Terminal(
                    hutan_protocol::messages::TerminalSpec {
                        command: vec!["sleep".into(), "600".into()],
                        title: Some(title.to_owned()),
                        ..Default::default()
                    },
                ),
            },
        ));
        info.session
    };
    let alpha = make("alpha shell", None);
    let beta = make("beta build", Some(ws.id));
    let gamma = make("gamma tests", Some(ws.id));
    // The old tree: every container expanded.
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[sidebar]\nshow_all_sessions = true\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 3);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock");

    let titles = |cx: &mut VisualTestContext| panel.read_with(cx, |p, cx| p.visible_titles(cx));
    // The projects first (`api`, whose two sessions are listed under it), Home last.
    assert_eq!(titles(cx), ["beta build", "gamma tests", "alpha shell"]);

    panel.update(cx, |p, cx| p.set_query("build", cx));
    assert_eq!(titles(cx), ["beta build"]);
    panel.update(cx, |p, cx| p.set_query("TESTS gamma", cx));
    assert_eq!(
        titles(cx),
        ["gamma tests"],
        "words match in any order, case-insensitively"
    );
    panel.update(cx, |p, cx| p.set_query("zzz", cx));
    assert!(titles(cx).is_empty());
    panel.update(cx, |p, cx| p.set_query("", cx));

    // Row clicks go through the action: the session's tab opens.
    cx.dispatch_action(actions::OpenSession(beta.id.to_string()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(beta.id)
    );
    // A second open focuses the existing tab instead of adding another.
    cx.dispatch_action(actions::OpenSession(gamma.id.to_string()));
    cx.dispatch_action(actions::OpenSession(beta.id.to_string()));
    let items = state_of(&view, cx).active_pane().unwrap().items.clone();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(beta.id)
    );

    // The context menu lists the documented entries.
    cx.dispatch_action(actions::ShowSessionMenu(actions::SessionMenuAt {
        session: alpha.id.to_string(),
        position: gpui::point(gpui::px(50.0), gpui::px(50.0)),
    }));
    let menu = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open");
    let labels: Vec<String> = menu.iter().map(|(l, _)| l.to_string()).collect();
    assert_eq!(
        labels,
        [
            "Open",
            "Open in Split Right",
            "Open in Split Down",
            "Mark as unread",
            "Snooze until it changes",
            "Mute notifications",
            "Rename…",
            "Restart",
            "Stop…",
            "Close session…",
            "Show in Resources",
            "Open File…",
            "Copy Session ID"
        ]
    );
    // FX-UI-B: the groups of the menu are under a line, and a greyed entry says why in its
    // tooltip, not in a sentence of its own.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.context_menu_groups()),
        ["Mark as unread", "Rename…", "Restart", "Show in Resources"]
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.context_menu_disabled()),
        ["Snooze until it changes"]
    );
    let tips = view.read_with(cx, |wb, _| wb.context_menu_tips());
    assert_eq!(
        tips[4].as_deref(),
        Some("Nothing waits for you here."),
        "{tips:?}"
    );
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    assert_eq!(state_of(&view, cx).panes().len(), 2, "open in split right");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(alpha.id)
    );
    view.update_in(cx, |wb, window, cx| {
        wb.copy_session_id(alpha.id, cx);
        let _ = window;
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some(alpha.id.to_string())
    );

    // Grouping by state and the row fields are settings.
    write(
        daemon.home(),
        "settings.toml",
        "[panels.sessions]\ngroup_by = \"none\"\nneeds_you = false\nrow = [\"title\"]\n",
    );
    wait_until(cx, "the panel settings", |cx| {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .get::<hutan_desktop::settings::SessionsPanelSettings>()
                .row
                == ["title"]
        })
    });
    assert_eq!(titles(cx).len(), 3);
}

#[gpui::test]
fn stop_and_restart_from_the_ui(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo started; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "started");

    // ⌘⇧W asks first while the session runs.
    cx.simulate_keystrokes("cmd-shift-w");
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert!(session_state(&view, cx, info.id).unwrap().is_live());

    // Stopping is a destructive confirmation: Return would cancel it, so the button is pressed.
    cx.simulate_keystrokes("cmd-shift-w");
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the session to stop", |cx| {
        session_state(&view, cx, info.id) == Some(SessionState::Stopped)
    });
    // The view says so and offers Restart / Close.
    let status = view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(info.id)
            .map(|v| v.read(cx).status().clone())
    });
    assert!(
        matches!(&status, Some(ViewStatus::Ended(text)) if text.contains("stopped")),
        "{status:?}"
    );
    // Stopping something that already ended says so instead of asking.
    cx.simulate_keystrokes("cmd-shift-w");
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));

    // Restart: the same tab continues on the new terminal without re-opening.
    cx.dispatch_action(actions::RestartSessionById(info.id.to_string()));
    wait_until(cx, "the session to run again", |cx| {
        session_state(&view, cx, info.id).is_some_and(|s| s.is_live())
    });
    wait_until(cx, "the view to go live", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        })
    });
    wait_screen(&view, cx, info.id, "started");
    assert_eq!(
        state_of(&view, cx).active_pane().unwrap().items.len(),
        1,
        "still one tab"
    );

    // Rename through the prompt: with the sidebar hidden the session has no row to edit.
    cx.dispatch_action(actions::HideLeftSidebar);
    draw(cx);
    cx.dispatch_action(actions::RenameSessionById(info.id.to_string()));
    view.update(cx, |wb, cx| wb.set_modal_input("renamed by test", cx));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new title", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .is_some_and(|s| s.title == "renamed by test")
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "renamed by test"
    );
}

#[gpui::test]
fn the_session_rename_prompt_opens_with_the_title_selected(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    // No row to edit in place (the sidebar is hidden): the prompt asks.
    cx.dispatch_action(actions::HideLeftSidebar);
    draw(cx);
    cx.dispatch_action(actions::RenameSessionById(info.id.to_string()));
    assert!(view.read_with(cx, |wb, _| wb.modal_open() && wb.modal_input_selected()));
    cx.simulate_input("Orchard review");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.modal().and_then(|m| m.input.clone())),
        Some("Orchard review".to_owned()),
        "typing replaces the old title"
    );
    assert!(!view.read_with(cx, |wb, _| wb.modal_input_selected()));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new title", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(info.id)
                .is_some_and(|s| s.title == "Orchard review")
        })
    });
}

#[gpui::test]
fn a_session_row_is_renamed_in_place(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let named = daemon.sh_titled("build", "sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock");
    let editing = |cx: &mut VisualTestContext| {
        panel.read_with(cx, |p, _| {
            p.renaming_session().map(|(id, text)| (id, text.to_owned()))
        })
    };

    // The row is on screen: Rename… edits its title where it is, with no dialog.
    cx.dispatch_action(actions::RenameSessionById(named.id.to_string()));
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));
    assert_eq!(editing(cx), Some((named.id, "build".to_owned())));
    assert!(panel.read_with(cx, |p, _| p.rename_selected()));
    cx.simulate_input("deploy");
    assert_eq!(editing(cx), Some((named.id, "deploy".to_owned())));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the new title", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(named.id)
                .is_some_and(|s| s.title == "deploy")
        })
    });
    assert_eq!(editing(cx), None);

    // Escape leaves the name alone.
    cx.dispatch_action(actions::RenameSessionById(named.id.to_string()));
    draw(cx);
    cx.simulate_input("nope");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(editing(cx), None);
    assert_eq!(daemon.session(named.id).title, "deploy");
}

#[gpui::test]
fn unread_attention_is_read_when_the_session_is_focused(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let go = daemon.home().join("go");
    // Already over before the desktop connects: not news.
    let old = daemon.sh_titled("old", "exit 1");
    let failing = daemon.sh_titled(
        "failing",
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 3",
            go.display()
        ),
    );
    // It works in a repository with an uncommitted file: a clean exit with nothing changed is
    // only ended, never ready for review (FX-UI-A 4).
    let repo = TestDaemon::dirty_repo(daemon.home());
    let finishing = daemon.sh_titled_in(
        "finishing",
        &format!(
            "while [ ! -e {0} ]; do sleep 0.05; done; exit 0",
            go.display()
        ),
        &repo,
    );
    let calm = daemon.sh_titled("calm", "sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 4);
    // Something else has focus (the welcome pane), so nothing is read.
    // What nobody has looked at yet: a failure or finished work that is still unseen.
    let needs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store
                .sessions()
                .iter()
                .filter(|info| {
                    let presentation = store.presentation(info);
                    presentation.attention.is_some() && presentation.filled
                })
                .map(|info| info.id)
                .collect::<Vec<_>>()
        })
    };
    wait_until(cx, "the old session to be seen as ended", |cx| {
        session_state(&view, cx, old.id).is_some_and(|s| !s.is_live())
    });
    assert!(needs(cx).is_empty(), "replayed history is not unread");

    std::fs::write(&go, "").unwrap();
    wait_until(cx, "both sessions to end", |cx| needs(cx).len() == 2);
    let presentation = |cx: &mut VisualTestContext, id| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.presentation(store.session(id).unwrap())
        })
    };
    let failed = presentation(cx, failing.id);
    assert_eq!(
        (failed.label.as_str(), failed.detail.as_str()),
        ("failed", "exit 3")
    );
    assert_eq!(failed.attention, Some(Attention::Failed));
    let finished = presentation(cx, finishing.id);
    assert_eq!(
        (finished.label.as_str(), finished.attention),
        ("review", Some(Attention::Review)),
        "finished work is never a request"
    );
    assert_eq!(presentation(cx, old.id).label, "exited");
    assert_eq!(presentation(cx, calm.id).label, "idle");

    // The sessions panel lists them under their groups first: Failed, then Ready for review.
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .unwrap();
    let titles = panel.read_with(cx, |p, cx| p.visible_titles(cx));
    let head: Vec<&str> = titles.iter().take(2).map(String::as_str).collect();
    assert_eq!(head, ["failing", "finishing"], "{titles:?}");

    // Each group is a muted label with its count over the same one-line rows; a row away from
    // its project row says which project it is in, and the lane has a glyph for every state
    // but idle.
    let rows = panel.read_with(cx, |p, cx| p.visible_rows(cx));
    assert_eq!(
        (rows[0].kind, rows[0].title.as_str()),
        (
            hutan_desktop::workbench::panels::RowKind::Section,
            "Failed · 1"
        )
    );
    assert!(
        rows.iter().any(
            |r| r.kind == hutan_desktop::workbench::panels::RowKind::Section
                && r.title == "Ready for review · 1"
        ),
        "{rows:?}"
    );
    let row = |title: &str| rows.iter().find(|r| r.title == title).unwrap();
    assert!(
        row("failing")
            .subtitle
            .as_ref()
            .is_some_and(|s| s.container.is_some()),
        "a Failed row names its project"
    );
    assert_eq!(row("failing").state.as_deref(), Some("Failed"));
    assert_eq!(row("finishing").state.as_deref(), Some("Ready for review"));
    let calm = row("calm");
    assert_eq!(calm.state, None, "an idle session has an empty lane");
    assert!(calm.subtitle.is_none(), "under its project row: one line");

    // ⌘⇧J jumps to the failure first (a request would come before it) and looks at it: a
    // failure the user has seen stays in its group, no longer unseen, and the jump skips it.
    cx.simulate_keystrokes("cmd-shift-j");
    let first = view
        .read_with(cx, |wb, _| wb.focused_session())
        .expect("a session is focused");
    assert_eq!(first, failing.id);
    let remaining = needs(cx);
    assert_eq!(remaining, [finishing.id], "the focused one is seen");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .in_attention(Attention::Failed)),
        [failing.id],
        "a seen failure stays listed in Failed"
    );
    cx.simulate_keystrokes("cmd-shift-j");
    let second = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_eq!(second, finishing.id);
    assert!(needs(cx).is_empty());
    cx.simulate_keystrokes("cmd-shift-j");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string)),
        Some("No requests, failures or work to review".to_owned())
    );
}

#[gpui::test]
fn settings_files_hot_reload_density_and_terminal_font(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo fonts; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "fonts");
    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
    let cell_height = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .and_then(|v| v.read(cx).cell_size())
                .map(|c| f32::from(c.height))
        })
        .unwrap()
    };
    let before = cell_height(cx);
    assert!((before - 13.0 * 1.2).abs() < 0.1, "{before}");
    // Terminal text keeps off the pane edge: 8 px compact, 12 px comfortable.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().padding),
        8.0
    );
    let pane = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(hutan_desktop::workbench::Region::Pane(
                wb.state().active_pane_id(),
            ))
        })
        .unwrap();
    let grid_cols = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .and_then(|v| v.read(cx).grid_size())
        })
        .unwrap()
        .0
    };
    let cell_w = 13.0 * 0.6;
    let expected = ((f32::from(pane.size.width) - 16.0) / cell_w).floor() as u16;
    assert_eq!(
        grid_cols(cx),
        expected,
        "columns come from the padded content box"
    );

    write(
        daemon.home(),
        "settings.toml",
        "[appearance]\ndensity = \"comfortable\"\n\n[terminal]\nfont_size = 20\nline_height = 1.5\n",
    );
    wait_until(cx, "the density to change", |cx| {
        view.read_with(cx, |wb, _| wb.density()) == Density::Comfortable
    });
    wait_until(cx, "the terminal font", |cx| {
        (cell_height(cx) - 30.0).abs() < 0.1
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        20.0
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().padding),
        12.0
    );
    // Panel geometry follows the tokens: the status bar is 24 px in both densities.
    draw(cx);
    let bar = view.read_with(cx, |wb, _| {
        wb.region_bounds(hutan_desktop::workbench::Region::StatusBar)
    });
    let height = f32::from(bar.unwrap().size.height);
    assert!((height - 24.0).abs() <= 1.0, "{height}");

    // A broken edit keeps the last good values and is reported; a fix recovers.
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = \"big\"\n",
    );
    wait_until(cx, "the error", |cx| {
        view.read_with(cx, |wb, _| !wb.settings_store().errors().is_empty())
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        20.0,
        "the last good value stays"
    );
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = 16\n",
    );
    wait_until(cx, "the fix", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 16.0
    });
    assert_eq!(view.read_with(cx, |wb, _| wb.density()), Density::Compact);
}

#[gpui::test]
fn changing_the_terminal_theme_updates_the_daemons_osc_11_answer(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let reply_len = "\x1b]11;rgb:1234/5678/9abc\x07".len();
    let script = format!(
        r"stty raw -echo; printf READY; dd bs=1 count=1 2>/dev/null; printf '\033]11;?\007'; dd bs=1 count={reply_len} 2>/dev/null | cat -v; printf '\nDONE\n'; sleep 600"
    );
    let info = daemon.sh(&script);
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "READY");

    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\ntheme = \"hutan-light\"\n",
    );
    wait_until(cx, "the light theme", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().palette.background)
            == hutan_terminal_view::Rgb(0xfa, 0xf9, 0xf5)
    });
    // `terminal.set_colors` travels through the handle; give the connection a moment.
    for _ in 0..30 {
        draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.simulate_input("g");
    wait_screen(&view, cx, info.id, "DONE");
    let screen = screen_of(&view, cx, info.id).unwrap();
    assert!(
        screen.contains("^[]11;rgb:fafa/f9f9/f5f5^G"),
        "the daemon answers with the theme the user sees:\n{screen}"
    );
}

#[gpui::test]
fn keymap_edits_rebind_live(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 0);
    let panes = |cx: &mut VisualTestContext| state_of(&view, cx).panes().len();
    // Splits start terminals asynchronously; let them land.
    let settle = |cx: &mut VisualTestContext| {
        for _ in 0..30 {
            draw(cx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the first split", |cx| panes(cx) == 2);

    write(
        daemon.home(),
        "keymap.toml",
        "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\n\n[[binding]]\nkeys = \"cmd-shift-d\"\naction = \"workbench::SplitRight\"\n",
    );
    wait_until(cx, "the keymap to reload", |cx| {
        cx.update(|window, _| {
            window
                .highest_precedence_binding_for_action(&actions::SplitRight)
                .is_some_and(|b| b.keystrokes()[0].to_string() == "⌘⇧D")
        })
    });
    cx.simulate_keystrokes("cmd-d");
    settle(cx);
    assert_eq!(panes(cx), 2, "⌘D is unbound now");
    cx.simulate_keystrokes("cmd-shift-d");
    wait_until(cx, "⌘⇧D splitting right", |cx| panes(cx) == 3);

    // Deleting the file restores the defaults.
    std::fs::remove_file(daemon.home().join("keymap.toml")).unwrap();
    wait_until(cx, "the default keymap", |cx| {
        cx.update(|window, _| {
            window
                .highest_precedence_binding_for_action(&actions::SplitRight)
                .is_some_and(|b| b.keystrokes()[0].to_string() == "⌘D")
        })
    });
}

#[gpui::test]
fn the_settings_sheet_writes_values_and_keeps_comments(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let file = daemon.home().join("settings.toml");
    write(
        daemon.home(),
        "settings.toml",
        "# my settings\n[terminal]\nfont_size = 13 # keep this\ncopy_on_select = false\n",
    );
    let (view, cx) = open(cx, &daemon);

    cx.simulate_keystrokes("cmd-,");
    let sheet = view.read_with(cx, |wb, _| {
        wb.settings_sheet().map(|s| s.namespace().to_owned())
    });
    assert_eq!(sheet.as_deref(), Some("appearance"));
    // Every panel's gear and the status theme item open their own namespace.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    draw(cx);
    let fields: Vec<String> = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .fields()
            .iter()
            .map(|f| f.dotted())
            .collect()
    });
    for expected in [
        "font_family",
        "font_size",
        "theme",
        "cursor.style",
        "cursor.blink",
        "option_as_meta",
    ] {
        assert!(
            fields.iter().any(|f| f == expected),
            "{expected} in {fields:?}"
        );
    }

    // A number box: type through the keyboard like a user.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("font_size", cx));
    cx.simulate_keystrokes("backspace backspace backspace backspace 1 7 enter");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("# my settings") && text.contains("# keep this"),
        "{text}"
    );
    assert!(text.contains("font_size = 17"), "{text}");
    wait_until(cx, "the new font size", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 17.0
    });

    // Invalid input stays in the box with an error.
    view.update(cx, |wb, cx| wb.sheet_begin_edit("font_size", cx));
    view.update(cx, |wb, cx| wb.sheet_set_edit_text("huge", cx));
    cx.simulate_keystrokes("enter");
    let error = view.read_with(cx, |wb, _| {
        wb.settings_sheet()
            .unwrap()
            .edit()
            .and_then(|e| e.error.clone())
    });
    assert_eq!(error.as_deref(), Some("expected a number"));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |wb, _| wb.settings_sheet().unwrap().edit().is_none()));

    // Toggle, dropdown, list editor, reset.
    view.update(cx, |wb, cx| wb.sheet_toggle("copy_on_select", cx));
    view.update(cx, |wb, cx| wb.sheet_choose("cursor.style", "bar", cx));
    view.update(cx, |wb, cx| wb.sheet_choose("cursor.style", "nonsense", cx));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("copy_on_select = true"), "{text}");
    assert!(
        text.contains("# my settings") && text.contains("# keep this"),
        "{text}"
    );
    let parsed: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(parsed["terminal"]["cursor"]["style"].as_str(), Some("bar"));
    view.update(cx, |wb, cx| wb.sheet_reset("font_size", cx));
    wait_until(cx, "the default font size", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 13.0
    });

    cx.simulate_keystrokes("escape");
    assert!(
        view.read_with(cx, |wb, _| wb.settings_sheet().is_none()),
        "Escape closes the sheet"
    );

    // Lists: add and remove items of `panels.sessions.row`.
    cx.dispatch_action(actions::OpenSettingsFor("panels.sessions".into()));
    view.update(cx, |wb, cx| wb.sheet_remove_list_item("row", 5, cx));
    // The parts come from a menu of the ones that exist (a list without a known set takes
    // typed text: see `known_sets_are_choices_and_numbers_stay_in_their_range`).
    view.update(cx, |wb, cx| wb.sheet_add_list_item("row", "age", cx));
    let row = view.read_with(cx, |wb, _| wb.sheet_value("row")).unwrap();
    assert_eq!(
        row,
        serde_json::json!(["state", "kind", "title", "folder", "placement", "age"])
    );

    // Order: move items up and down; the ends do nothing.
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 2, -1, cx));
    let row =
        |cx: &mut VisualTestContext| view.read_with(cx, |wb, _| wb.sheet_value("row")).unwrap();
    assert_eq!(
        row(cx),
        serde_json::json!(["state", "title", "kind", "folder", "placement", "age"])
    );
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 0, -1, cx));
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 5, 1, cx));
    assert_eq!(
        row(cx),
        serde_json::json!(["state", "title", "kind", "folder", "placement", "age"])
    );
    view.update(cx, |wb, cx| wb.sheet_move_list_item("row", 0, 1, cx));
    assert_eq!(
        row(cx),
        serde_json::json!(["title", "state", "kind", "folder", "placement", "age"])
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("# my settings"), "{text}");

    // Errors from the files show up in the store the sheet reads.
    write(
        daemon.home(),
        "settings.toml",
        "[terminal]\nfont_size = \"big\"\n[nope]\nx = 1\n",
    );
    wait_until(cx, "diagnostics", |cx| {
        view.read_with(cx, |wb, _| wb.settings_store().errors().len() >= 2)
    });
}

#[gpui::test]
fn save_layout_as_preset_writes_settings_and_hot_reloads_presets(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    cx.dispatch_action(actions::HideRightRail);
    cx.dispatch_action(actions::SaveLayoutAsPreset);
    assert!(view.read_with(cx, |wb, _| wb.modal_open()));
    view.update(cx, |wb, cx| wb.set_modal_input("My Layout", cx));
    cx.simulate_keystrokes("enter");

    let text = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();
    let parsed: toml::Table = toml::from_str(&text).unwrap();
    let preset = &parsed["layout"]["presets"]["My-Layout"];
    assert_eq!(preset["left"]["visible"].as_bool(), Some(true));
    assert_eq!(preset["right"]["visible"].as_bool(), Some(false));
    assert_eq!(preset["left"]["panels"].as_array().unwrap().len(), 2);
    assert_eq!(state_of(&view, cx).preset, "My-Layout");
    assert_eq!(
        parsed["layout"]["preset"].as_str(),
        Some("My-Layout"),
        "saving makes it the preset in use for the next start too"
    );
    wait_until(cx, "the palette to list the preset", |cx| {
        view.read_with(cx, |wb, _| {
            wb.commands()
                .iter()
                .any(|c| c.title.as_ref() == "Layout: Apply Preset My-Layout")
        })
    });

    // Switch away and back: the preset restores the docks.
    cx.dispatch_action(actions::ApplyPreset("focus".into()));
    assert!(!state_of(&view, cx).docks.left.visible);
    cx.dispatch_action(actions::ApplyPreset("My-Layout".into()));
    let state = state_of(&view, cx);
    assert!(state.docks.left.visible && !state.docks.right.visible);

    // Editing a preset definition on disk re-applies it when it is the current one.
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"My-Layout\"\n[layout.presets.My-Layout]\nleft = { panels = [\"sessions\"], size = 333, visible = true }\nright = { panels = [], size = 300, visible = false }\nbottom = { panels = [], size = 200, visible = false }\nstatus_bar = { left = [\"connection\"], right = [] }\n",
    );
    wait_until(cx, "the preset definition", |cx| {
        state_of(&view, cx).docks.left.size == 333.0
    });
    let bar = view.read_with(cx, |wb, _| wb.status_bar());
    assert_eq!(
        (bar.left, bar.right),
        (vec!["connection".to_owned()], Vec::<String>::new())
    );
}

#[gpui::test]
fn workspace_settings_apply_while_their_session_is_focused(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".hutan")).unwrap();
    std::fs::write(
        root.path().join(".hutan/settings.toml"),
        "[terminal]\nfont_size = 20\n",
    )
    .unwrap();
    let ws = daemon.create_workspace("proj", &[root.path()]);
    let plain = daemon.sh("sleep 600");
    let project = daemon.create_session(&["sleep", "600"], Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 2);

    // Home has a root too: its folder, which carries no workspace settings here.
    open_session(&view, cx, plain.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        Some(std::fs::canonicalize(&daemon.paths.default_home_folder).unwrap())
    );
    assert_eq!(view.read_with(cx, |wb, _| wb.project_root().cloned()), None);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size),
        13.0
    );

    // A project in no workspace layers only its own file.
    open_session(&view, cx, project.id);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.project_root().cloned()),
        Some(std::fs::canonicalize(root.path()).unwrap()),
        "the project's folder is stored canonical"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.workspace_root().cloned()),
        None
    );
    wait_until(cx, "the workspace override", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 20.0
    });

    open_session(&view, cx, plain.id);
    wait_until(cx, "the override to end", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_settings().font_size) == 13.0
    });
}

#[gpui::test]
fn new_terminal_starts_in_the_focused_workspace_root_or_home(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let root = tempfile::tempdir().unwrap();
    let ws = daemon.create_workspace("proj", &[root.path()]);
    let member = daemon.create_session(&["sleep", "600"], Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);

    // Nothing focused: Home and its folder.
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let first = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    let info = daemon.session(first);
    assert_same_dir(info.cwd.as_deref(), &daemon.paths.default_home_folder);
    assert_ne!(info.workspace_id, ws.id);

    // From a session of `proj`: same workspace, in its root.
    open_session(&view, cx, member.id);
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the second new session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != member.id && s != first)
    });
    let second = daemon.session(view.read_with(cx, |wb, _| wb.focused_session()).unwrap());
    assert_eq!(second.workspace_id, ws.id);
    assert_same_dir(second.cwd.as_deref(), root.path());
}

/// `terminal.shell` and `terminal.env`: the user's values, overridden per variable by the
/// workspace's `.hutan/settings.toml`, reach `session.create` and so the new terminal's process.
/// The shell is a stand-in script that prints what it was started with.
#[gpui::test]
fn terminal_shell_and_env_settings_reach_new_sessions_with_workspace_overrides(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt as _;

    let daemon = TestDaemon::start();
    let tools = tempfile::tempdir().unwrap();
    let shell = tools.path().join("stand-in-shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\necho \"mark=$B1_MARK user=$B1_USER_ONLY ws=$B1_WS_ONLY shell=$SHELL\"\nexec cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    // macOS vets the first execution of a new executable, which can take tens of seconds on a
    // loaded machine; do that here, outside the timeouts the test is about.
    let _ = std::process::Command::new(&shell)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    write(
        daemon.home(),
        "settings.toml",
        &format!(
            "[terminal]\nshell = \"{}\"\n[terminal.env]\nB1_MARK = \"user\"\nB1_USER_ONLY = \"u\"\n",
            shell.display()
        ),
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".hutan")).unwrap();
    std::fs::write(
        root.path().join(".hutan/settings.toml"),
        "[terminal.env]\nB1_MARK = \"workspace\"\nB1_WS_ONLY = \"w\"\n",
    )
    .unwrap();
    let ws = daemon.create_workspace("proj", &[root.path()]);
    let member = daemon.create_session(&["sleep", "600"], Some(ws.id));
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);

    // Home carries no workspace file: the user's values and shell.
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the Home terminal", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let home = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    wait_screen(
        &view,
        cx,
        home,
        &format!("mark=user user=u ws= shell={}", shell.display()),
    );

    // `proj`: its `terminal.env` wins per variable and adds its own; the rest stays.
    open_session(&view, cx, member.id);
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the project terminal", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != member.id && s != home)
    });
    let project = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_eq!(daemon.session(project).workspace_id, ws.id);
    wait_screen(
        &view,
        cx,
        project,
        &format!("mark=workspace user=u ws=w shell={}", shell.display()),
    );
}

/// The settings sheet's variable editor writes `terminal.env`, and the next terminal starts with
/// what it holds: a variable added there is in the shell, and one removed there is not.
#[gpui::test]
fn the_variable_editor_feeds_the_next_terminal(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;

    let daemon = TestDaemon::start();
    let tools = tempfile::tempdir().unwrap();
    let shell = tools.path().join("stand-in-shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\necho \"edited=$B2_EDITED other=$B2_OTHER\"\nexec cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    // macOS vets the first execution of a new executable, which can take a long time.
    let _ = std::process::Command::new(&shell)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    write(
        daemon.home(),
        "settings.toml",
        &format!("[terminal]\nshell = \"{}\"\n", shell.display()),
    );
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 0);

    // Add two variables the way a person does: `NAME=value` in the box, and in the row.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    view.update(cx, |wb, cx| wb.sheet_begin_edit("env", cx));
    view.update(cx, |wb, cx| {
        wb.sheet_set_edit_text("B2_EDITED=from-the-editor", cx)
    });
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    view.update(cx, |wb, cx| wb.sheet_begin_edit("env", cx));
    view.update(cx, |wb, cx| {
        wb.sheet_set_edit_text("B2_OTHER=gone-soon", cx)
    });
    view.update(cx, |wb, cx| wb.sheet_commit_edit(cx));
    let text = std::fs::read_to_string(daemon.home().join("settings.toml")).unwrap();
    let parsed: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        parsed["terminal"]["env"]["B2_EDITED"].as_str(),
        Some("from-the-editor")
    );
    assert!(text.contains("shell ="), "the shell stays: {text}");
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the first terminal", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()).is_some()
    });
    let first = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    wait_screen(&view, cx, first, "edited=from-the-editor other=gone-soon");

    // Remove one: the next terminal does not have it, the one running keeps what it had.
    cx.dispatch_action(actions::OpenSettingsFor("terminal".into()));
    view.update(cx, |wb, cx| wb.sheet_map_remove("env", "B2_OTHER", cx));
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("cmd-t");
    wait_until(cx, "the second terminal", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != first)
    });
    let second = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    wait_screen(&view, cx, second, "edited=from-the-editor other=");
    assert!(
        !screen_of(&view, cx, second)
            .unwrap_or_default()
            .contains("gone-soon")
    );
    assert!(
        screen_of(&view, cx, first)
            .unwrap_or_default()
            .contains("gone-soon")
    );
}

#[gpui::test]
fn closing_a_tab_detaches_and_the_layout_reattaches_after_a_restart(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let a = daemon.sh("echo session-a; sleep 600");
    let b = daemon.sh("echo session-b; sleep 600");
    let state_path = daemon.home().join("desktop-state.json");
    {
        let (view, cx) = open_with(
            cx,
            settings_for(&daemon),
            daemon.connection(),
            Some(state_path.clone()),
        );
        wait_synced(&view, cx, 2);
        open_session(&view, cx, a.id);
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: b.id.to_string(),
            edge: hutan_desktop::workbench::model::Edge::Right,
        }));
        wait_screen(&view, cx, a.id, "session-a");
        wait_screen(&view, cx, b.id, "session-b");
        assert_eq!(
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
            2
        );

        // ⌘W detaches: the session keeps running, the attachment ends.
        cx.simulate_keystrokes("cmd-w");
        wait_until(cx, "the tab to close", |cx| {
            state_of(&view, cx).panes().len() == 1
        });
        assert!(!view.read_with(cx, |wb, _| wb.holds(b.id)));
        assert!(session_state(&view, cx, b.id).unwrap().is_live());
        view.update(cx, |wb, _| wb.flush_save());
        // Put it back so the saved layout holds both.
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: b.id.to_string(),
            edge: hutan_desktop::workbench::model::Edge::Bottom,
        }));
        view.update(cx, |wb, _| wb.flush_save());
    }
    // A new app instance restores the layout and re-attaches every open terminal item.
    let (view, cx) = open_with(
        cx,
        settings_for(&daemon),
        daemon.connection(),
        Some(state_path),
    );
    assert_eq!(state_of(&view, cx).panes().len(), 2);
    wait_screen(&view, cx, a.id, "session-a");
    wait_screen(&view, cx, b.id, "session-b");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        2
    );
}

#[gpui::test]
fn items_of_vanished_sessions_do_not_attach(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let live = daemon.sh("sleep 600");
    let ghost = SessionId::new();
    let state_path = daemon.home().join("desktop-state.json");
    let mut state = hutan_desktop::workbench::model::WorkbenchState::default();
    let pane = state.active_pane_id();
    for id in [live.id, ghost] {
        state
            .add_item(
                pane,
                ItemRef::Terminal {
                    session: id.to_string(),
                },
            )
            .unwrap();
    }
    std::fs::write(&state_path, state.to_json().unwrap()).unwrap();

    let (view, cx) = open_with(
        cx,
        settings_for(&daemon),
        daemon.connection(),
        Some(state_path),
    );
    wait_synced(&view, cx, 1);
    wait_until(cx, "the live session to attach", |cx| {
        view.read_with(cx, |wb, _| wb.holds(live.id))
    });
    assert!(
        !view.read_with(cx, |wb, _| wb.holds(ghost)),
        "a missing session shows a placeholder"
    );
    // Closing the placeholder tab is an ordinary tab close.
    cx.dispatch_action(actions::ClosePaneItem);
    draw(cx);
}

#[gpui::test]
fn status_items_follow_the_connection_through_loss_and_recovery(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let info = daemon.sh("echo status; sleep 600");
    let connection = daemon.proxied_connection();
    let (view, cx) = open_with(cx, settings_for(&daemon), connection, None);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "status");
    let conn = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
    };
    assert_eq!(conn(cx), ConnectionState::Connected);
    assert_eq!(
        connection_label(ConnectionState::Connected).0,
        "hutand connected"
    );
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);

    // Connection loss: the link drops and reconnecting fails for a while.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "reconnecting", |cx| {
        matches!(conn(cx), ConnectionState::Reconnecting { .. })
    });
    assert!(connection_label(conn(cx)).0.starts_with("reconnecting"));
    wait_until(cx, "the terminal to show it", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Reconnecting)
        })
    });

    // Recovery in the same epoch: the terminal re-attaches with a fresh snapshot.
    daemon.proxy().block(false);
    wait_until(cx, "the connection to recover", |cx| {
        conn(cx) == ConnectionState::Connected
    });
    wait_until(cx, "the terminal to go live again", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        })
    });
    wait_screen(&view, cx, info.id, "status");
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch),
        epoch
    );

    // The daemon itself goes away and comes back with a new epoch: it respawns the session in a
    // new terminal, and the open item follows it there.
    daemon.stop();
    wait_until(cx, "reconnecting after the daemon stopped", |cx| {
        matches!(conn(cx), ConnectionState::Reconnecting { .. })
    });
    daemon.restart();
    wait_until(cx, "the new daemon", |cx| {
        conn(cx) == ConnectionState::Connected
            && view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch) != epoch
    });
    wait_until(cx, "the item to follow the respawned session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        }) && attached_to_current_daemon(&view, cx, info.id)
    });
    wait_screen(&view, cx, info.id, "status");
    assert!(view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .sessions()
            .iter()
            .any(|s| s.id == info.id)
    }));
}

/// Whether the open view of `id` is attached to a terminal of the daemon the window is
/// connected to now (terminal ids start over with every daemon, so the epoch tells).
fn attached_to_current_daemon(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> bool {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store
            .attached(id)
            .is_some_and(|t| t.read(cx).daemon_epoch() == store.state().epoch)
    })
}

fn attached_terminal(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    id: SessionId,
) -> Option<hutan_core::TerminalId> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .attached(id)
            .and_then(|t| t.read(cx).terminal())
    })
}

#[gpui::test]
fn terminal_tabs_in_every_pane_reattach_to_respawned_sessions_and_take_input(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let first = daemon.create_session(&["cat"], None);
    let second = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 2);
    open_session(&view, cx, first.id);
    wait_until(cx, "the first terminal", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(first.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });
    cx.dispatch_action(actions::SplitRight);
    wait_until(cx, "the split", |cx| state_of(&view, cx).panes().len() == 2);
    open_session(&view, cx, second.id);
    wait_until(cx, "both terminals to be ready", |cx| {
        [first.id, second.id].iter().all(|id| {
            view.read_with(cx, |wb, cx| {
                wb.terminal_view_of(*id)
                    .is_some_and(|v| v.read(cx).model().is_ready())
            })
        })
    });
    cx.simulate_input("before-restart\n");

    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    daemon.stop();
    daemon.restart();
    wait_until(cx, "the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.connection_state() == ConnectionState::Connected && store.state().epoch != epoch
        })
    });
    wait_until(cx, "every tab to follow its respawned session", |cx| {
        [first.id, second.id].iter().all(|id| {
            view.read_with(cx, |wb, cx| {
                wb.terminal_view_of(*id).is_some_and(|v| {
                    let v = v.read(cx);
                    *v.status() == ViewStatus::Live && v.model().is_ready()
                })
            }) && attached_to_current_daemon(&view, cx, *id)
        })
    });
    // The old screen is gone with the old shell; the respawned one is empty.
    assert!(
        !screen_of(&view, cx, second.id)
            .unwrap()
            .contains("before-restart")
    );

    // Input goes to the live shell of the focused tab (the split's, `second`).
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(second.id)
    );
    cx.simulate_input("after-restart\n");
    wait_screen(&view, cx, second.id, "after-restart");
    // And the daemon agrees: the session runs in the terminal the tab is attached to.
    assert_eq!(
        daemon.session(second.id).terminal_id,
        attached_terminal(&view, cx, second.id)
    );
}

#[gpui::test]
fn a_terminal_tab_of_a_session_that_ended_stays_ended_after_a_restart(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start_with_config("[daemon]\nrestore_sessions = \"stopped\"\n");
    let info = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_until(cx, "the terminal", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id)
                .is_some_and(|v| *v.read(cx).status() == ViewStatus::Live)
        })
    });
    let epoch = view.read_with(cx, |wb, cx| wb.sessions().read(cx).state().epoch);
    daemon.stop();
    daemon.restart_with(|_| {});
    wait_until(cx, "the new daemon", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.connection_state() == ConnectionState::Connected && store.state().epoch != epoch
        })
    });
    wait_until(cx, "the tab to say the session ended", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(info.id).is_some_and(|v| {
                matches!(v.read(cx).status(), ViewStatus::Ended(text) if text.contains("stopped"))
            })
        })
    });
    // It stays ended.
    for _ in 0..30 {
        draw(cx);
    }
    assert!(view.read_with(cx, |wb, cx| {
        wb.terminal_view_of(info.id)
            .is_some_and(|v| matches!(v.read(cx).status(), ViewStatus::Ended(_)))
    }));
}

#[gpui::test]
fn the_inspector_describes_the_focused_session(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo inspected; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "inspected");
    // Requests is a tab of the right sidebar; History is the one showing.
    cx.dispatch_action(actions::OpenPanel("inspector".into()));
    draw(cx);
    let inspector = view
        .read_with(cx, |wb, _| wb.panel::<InspectorPanel>("inspector"))
        .expect("the inspector is drawn in the right sidebar");
    let current: SessionInfo = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(info.id).cloned()
        })
        .unwrap();
    let rows = inspector.read_with(cx, |p, cx| p.info_rows(&current, cx));
    let get = |label: &str| {
        rows.iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(get("ID"), info.id.to_string());
    assert_eq!(get("Kind"), "Terminal");
    assert_eq!(get("Command"), "sh -c echo inspected; sleep 600");
    assert_eq!(get("State"), "idle");
    assert_eq!(get("Terminal"), info.terminal_id.unwrap().to_string());
    assert!(get("Size").contains('×'), "{}", get("Size"));
    // Local time with its zone, then how long ago.
    let created = get("Created");
    assert!(
        created.contains(" +") || created.contains(" -"),
        "a zone: {created}"
    );
    assert!(!created.contains("UTC"), "{created}");
    assert!(created.ends_with("(just now)"), "{created}");
}

#[gpui::test]
fn terminal_notifications_flag_attention_but_replays_after_a_reconnect_do_not(
    cx: &mut TestAppContext,
) {
    let mut daemon = TestDaemon::start();
    let first = daemon.home().join("first");
    let second = daemon.home().join("second");
    // A coding agent says it runs (OSC 7501) before it notifies: a plain shell's notification is
    // only a bell (FX-UI-A 4), an agent's is finished work.
    let info = daemon.sh_titled(
        "notifier",
        &format!(
            "printf '\\033]7501;state=idle:app=omp\\007'; while [ ! -e {0} ]; do sleep 0.05; done; printf '\\033]9;while you were away\\007'; while [ ! -e {1} ]; do sleep 0.05; done; printf '\\033]9;built\\007'; sleep 600",
            first.display(),
            second.display()
        ),
    );
    let connection = daemon.proxied_connection();
    let (view, cx) = open_with(cx, settings_for(&daemon), connection, None);
    wait_synced(&view, cx, 1);
    let needs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).in_attention(Attention::Review)
        })
    };
    assert!(needs(cx).is_empty());

    // The notification happens while the desktop is disconnected; the daemon replays it on
    // reconnect, which must not count as news.
    daemon.proxy().block(true);
    daemon.proxy().sever();
    wait_until(cx, "the connection to drop", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(
                wb.sessions().read(cx).connection_state(),
                ConnectionState::Reconnecting { .. }
            )
        })
    });
    std::fs::write(&first, "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    daemon.proxy().block(false);
    wait_until(cx, "the connection to recover", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).connection_state())
            == ConnectionState::Connected
    });
    // Let the replay window pass, then the replayed notification must have left no trace.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
    while std::time::Instant::now() < deadline {
        draw(cx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        needs(cx).is_empty(),
        "a replayed notification is not unread"
    );

    // A live notification of an unfocused session is.
    std::fs::write(&second, "").unwrap();
    wait_until(cx, "the live notification", |cx| needs(cx) == [info.id]);
    let presentation = view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store.presentation(store.session(info.id).unwrap())
    });
    assert_eq!(
        (presentation.label.as_str(), presentation.detail.as_str()),
        ("review", "notification")
    );
    assert_eq!(
        presentation.attention,
        Some(Attention::Review),
        "a notification is finished work, never a request"
    );

    // Focusing the session reads it; the process keeps running.
    open_session(&view, cx, info.id);
    assert!(needs(cx).is_empty());
    assert!(session_state(&view, cx, info.id).unwrap().is_live());
}

#[gpui::test]
fn the_overview_preset_shows_the_board_from_every_path(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let info = daemon.sh("echo boards; sleep 600");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, info.id);
    wait_screen(&view, cx, info.id, "boards");
    let terminal = ItemRef::Terminal {
        session: info.id.to_string(),
    };
    let board_shown = |cx: &mut VisualTestContext| {
        let s = state_of(&view, cx);
        s.preset == LayoutPreset::OVERVIEW
            && s.panes().len() == 1
            && s.active_pane().unwrap().active_item() == Some(&ItemRef::panel("agents-dashboard"))
            && matches!(
                s.focus,
                hutan_desktop::workbench::model::FocusTarget::Pane(_)
            )
    };

    // Settings file (hot reload).
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"overview\"\n",
    );
    wait_until(cx, "the overview from settings.toml", |cx| board_shown(cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .panel::<hutan_desktop::workbench::panels::AgentsPage>(
                "agents-dashboard"
            ))
        .is_some()
    );
    // Leaving through the settings file restores the stashed layout exactly.
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"default\"\n",
    );
    wait_until(cx, "the default layout from settings.toml", |cx| {
        state_of(&view, cx).preset == LayoutPreset::DEFAULT
    });
    let s = state_of(&view, cx);
    assert_eq!(s.active_pane().unwrap().active_item(), Some(&terminal));
    assert!(
        s.panes()
            .iter()
            .all(|p| !p.items.contains(&ItemRef::panel("agents-dashboard"))),
        "the board left the center"
    );

    // Palette / menu path, then ⌘⇧O and a card click leave again.
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    assert!(board_shown(cx));
    cx.simulate_keystrokes("cmd-shift-o");
    assert_eq!(
        state_of(&view, cx).active_pane().unwrap().active_item(),
        Some(&terminal)
    );
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    assert!(board_shown(cx));
    cx.dispatch_action(actions::ApplyPreset("default".into()));
    let s = state_of(&view, cx);
    assert_eq!(s.preset, LayoutPreset::DEFAULT);
    assert_eq!(
        s.active_pane().unwrap().active_item(),
        Some(&terminal),
        "stash restored"
    );
    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    cx.dispatch_action(actions::OpenSessionFromOverview(info.id.to_string()));
    assert_eq!(state_of(&view, cx).preset, LayoutPreset::DEFAULT);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(info.id)
    );
}

#[gpui::test]
fn the_overview_preset_applies_at_startup_from_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    write(
        daemon.home(),
        "settings.toml",
        "[layout]\npreset = \"overview\"\n",
    );
    let (view, cx) = open(cx, &daemon);
    let s = state_of(&view, cx);
    assert_eq!(s.preset, LayoutPreset::OVERVIEW);
    assert_eq!(
        s.active_pane().unwrap().active_item(),
        Some(&ItemRef::panel("agents-dashboard"))
    );
}

#[gpui::test]
fn splits_start_a_terminal_in_the_focused_sessions_directory(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    // In Home, as before protocol 0.19: without a workspace the daemon would make the project of
    // `dir`, which this version's sidebar does not show (M18-B).
    let home = daemon
        .call::<hutan_protocol::messages::WorkspaceList>(Request::WorkspaceList(
            hutan_protocol::messages::Empty {},
        ))
        .workspaces
        .into_iter()
        .find(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Home)
        .map(|w| w.id);
    let created: hutan_protocol::messages::SessionResult = daemon.call(Request::SessionCreate(
        hutan_protocol::messages::SessionCreate {
            workspace_id: home,
            spec: hutan_protocol::messages::SessionSpec::Terminal(
                hutan_protocol::messages::TerminalSpec {
                    command: vec!["sleep".into(), "600".into()],
                    cwd: Some(dir.path().to_path_buf()),
                    ..Default::default()
                },
            ),
        },
    ));
    let origin = created.session;
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, origin.id);

    // The pane split (the palette; ⌘D splits inside the tab now): a new pane with a NEW
    // session, started where the focused one is.
    cx.dispatch_action(actions::SplitRight);
    wait_until(cx, "the new session in the split", |cx| {
        state_of(&view, cx).panes().len() == 2
            && view
                .read_with(cx, |wb, _| wb.focused_session())
                .is_some_and(|s| s != origin.id)
    });
    let right = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_same_dir(daemon.session(right).cwd.as_deref(), dir.path());
    assert_eq!(state_of(&view, cx).active_pane().unwrap().items.len(), 1);

    // Split down from that one splits again, from its directory.
    cx.dispatch_action(actions::SplitDown);
    wait_until(cx, "the third pane", |cx| {
        state_of(&view, cx).panes().len() == 3
    });
    wait_until(cx, "its session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| s != origin.id && s != right)
    });
    let below = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_same_dir(daemon.session(below).cwd.as_deref(), dir.path());

    // From a panel (not a terminal): the Home folder.
    cx.dispatch_action(actions::OpenPanel("sessions".into()));
    cx.dispatch_action(actions::OpenActivePanelAsTab);
    cx.simulate_keystrokes("cmd-d");
    wait_until(cx, "the split from a panel", |cx| {
        state_of(&view, cx).panes().len() == 4
    });
    wait_until(cx, "its session", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session())
            .is_some_and(|s| ![origin.id, right, below].contains(&s))
    });
    let fourth = view.read_with(cx, |wb, _| wb.focused_session()).unwrap();
    assert_same_dir(
        daemon.session(fourth).cwd.as_deref(),
        &daemon.paths.default_home_folder,
    );
}

#[gpui::test]
fn the_agents_overview_keeps_every_session_attached_and_brings_the_split_layout_back(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let omp = daemon.sh_titled("omp", "echo omp-screen; sleep 600");
    let tests = daemon.sh_titled("tests", "echo tests-screen; sleep 600");
    let failing = daemon.sh_titled("failing", "echo boom; exit 1");
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 3);
    open_session(&view, cx, omp.id);
    wait_screen(&view, cx, omp.id, "omp-screen");
    // Two more panes; the last one opened has focus, the others are unfocused.
    for (session, edge) in [
        (&tests, hutan_desktop::workbench::model::Edge::Right),
        (&failing, hutan_desktop::workbench::model::Edge::Bottom),
    ] {
        cx.dispatch_action(actions::OpenSessionInSplit(actions::SessionSplit {
            session: session.id.to_string(),
            edge,
        }));
    }
    wait_screen(&view, cx, omp.id, "omp-screen");
    wait_screen(&view, cx, tests.id, "tests-screen");
    wait_screen(&view, cx, failing.id, "boom");
    assert_eq!(state_of(&view, cx).panes().len(), 3);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(failing.id)
    );

    cx.dispatch_action(actions::ApplyPreset("overview".into()));
    let s = state_of(&view, cx);
    assert_eq!(s.panes().len(), 1);
    assert_eq!(
        s.active_pane().unwrap().items,
        vec![ItemRef::panel("agents-dashboard")],
        "only the board is visible; the tabs wait in the stash"
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).attached_count()),
        3,
        "hidden sessions stay attached"
    );

    // Leaving the overview brings the split layout back with live terminals.
    cx.simulate_keystrokes("cmd-shift-o");
    assert_eq!(state_of(&view, cx).panes().len(), 3);
    wait_screen(&view, cx, omp.id, "omp-screen");
    wait_screen(&view, cx, failing.id, "boom");
}

/// The new pane inherits the session's cwd, which follows OSC 7 from the user's login shell;
/// shells like fish report the resolved path (`/private/var/…` for `/var/…` on macOS).
fn assert_same_dir(actual: Option<&std::path::Path>, expected: &std::path::Path) {
    let actual = actual.expect("session has a cwd");
    assert_eq!(
        std::fs::canonicalize(actual).unwrap(),
        std::fs::canonicalize(expected).unwrap(),
        "{} is not {}",
        actual.display(),
        expected.display()
    );
}

/// The benchmark key injection (`HUTAN_BENCH_INPUT`) types through the window like real key
/// events. Dispatching from inside a `Workbench` update used to re-enter the leased entity: a
/// panic that aborts the process when it happens in a window callback.
#[gpui::test]
fn scripted_keys_reach_the_focused_terminal_without_reentering_the_workbench(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::bench::{Script, run};

    let daemon = TestDaemon::start();
    let echo = daemon.create_session(&["cat"], None);
    let (view, cx) = open(cx, &daemon);
    wait_synced(&view, cx, 1);
    open_session(&view, cx, echo.id);
    wait_until(cx, "the cat session to attach", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.terminal_view_of(echo.id)
                .is_some_and(|v| v.read(cx).model().is_ready())
        })
    });

    let script =
        Script::parse(r#"{"interval_ms":5,"keys":["s","c","r","i","p","t","enter"]}"#).unwrap();
    view.update_in(cx, |_, window, cx| run(script, window, cx));
    let mut last = String::new();
    wait_until(cx, "the scripted keys on the screen", |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(5));
        last = screen_of(&view, cx, echo.id).unwrap_or_default();
        // The PTY echoes the line, then `cat` prints it again.
        last.matches("script").count() >= 2
    });
    assert!(last.matches("script").count() >= 2, "{last}");
}
