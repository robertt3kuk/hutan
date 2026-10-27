#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The History panel (M17-B) end to end, against an in-process daemon whose `daemon.toml`
//! points the index at synthetic Claude / Codex / OMP transcripts in temporary roots:
//! disabled → **Enable indexing** (a real click) → indexed → search shows one row per session
//! with highlighted snippets → filters narrow → Resume starts the hit's `resume_argv` in a new
//! terminal session → ⌘J `?` finds the same hits → Open block → Clear empties → Rebuild fills.
//!
//! Resume never runs the real `claude`, `codex` or `omp`: `terminal.shell` names a stand-in login
//! shell that puts stand-in agents on `PATH`, which is how the daemon (in its default login mode)
//! finds an explicit command.

use crate::common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use common::history::{CLAUDE_ID, CODEX_ID, GONE_CWD, GONE_ID, HistoryEnv, OMP_ID, UNRESUMABLE_ID};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::PanelId;
use hutan_desktop::workbench::panels::{HistoryConfirm, HistoryPanel};
use hutan_desktop::workbench::{HistoryState, Region, Workbench};
use hutan_protocol::history::{HistoryRole, HistoryScope, HistorySource};
use hutan_protocol::messages::SessionInfo;
use hutan_settings::{SettingsPaths, SettingsStore};

/// Like [`wait_until`], but the GPUI clock moves too: debounce and poll timers are fake time.
fn pump_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        condition(cx)
    });
}

fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<HistoryPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn")
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    draw(cx);
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn sources(panel: &Entity<HistoryPanel>, cx: &mut VisualTestContext) -> Vec<HistorySource> {
    let mut found: Vec<HistorySource> =
        panel.read_with(cx, |p, _| p.hits().iter().map(|hit| hit.source).collect());
    found.sort_by_key(|s| s.as_str());
    found
}

fn rows(panel: &Entity<HistoryPanel>, cx: &mut VisualTestContext) -> u64 {
    panel.read_with(cx, |p, _| {
        p.status()
            .map_or(0, |s| s.sources.iter().map(|s| s.rows).sum())
    })
}

/// Types `text` into the search field (focused through the palette command) and waits for the
/// debounced search to answer.
fn search(
    panel: &Entity<HistoryPanel>,
    cx: &mut VisualTestContext,
    text: &str,
    until: impl Fn(&HistoryPanel) -> bool,
) {
    cx.dispatch_action(actions::SearchHistory);
    draw(cx);
    panel.update(cx, |p, cx| p.set_query("", cx));
    cx.simulate_input(text);
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), text);
    // Nothing happens before the debounce is over.
    assert!(panel.read_with(cx, |p, _| p.searching()));
    pump_until(cx, &format!("the hits of `{text}`"), |cx| {
        panel.read_with(cx, |p, _| !p.searching() && until(p))
    });
}

fn new_sessions(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    before: &[SessionId],
) -> Vec<SessionInfo> {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| !before.contains(&s.id))
            .cloned()
            .collect()
    })
}

fn session_ids(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<SessionId> {
    new_sessions(view, cx, &[]).iter().map(|s| s.id).collect()
}

fn wait_new_session(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    before: &[SessionId],
) -> SessionInfo {
    pump_until(cx, "the resumed session", |cx| {
        !new_sessions(view, cx, before).is_empty()
    });
    let mut created = new_sessions(view, cx, before);
    assert_eq!(created.len(), 1, "exactly one session: {created:?}");
    created.remove(0)
}

fn screen(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> String {
    view.read_with(cx, |wb, _| wb.terminal_view_of(id))
        .map(|v| v.read_with(cx, |v, _| v.visible_text().join("\n")))
        .unwrap_or_default()
}

fn workspace_of(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> String {
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        let info = store.session(id).expect("the session exists");
        store
            .workspace(info.workspace_id)
            .map(|w| format!("{}:{:?}", w.name, w.roots))
            .expect("the workspace exists")
    })
}

#[gpui::test]
fn the_history_panel_searches_filters_resumes_and_manages_the_index(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    env.install_stand_ins();
    let shell = env.install_login_shell();
    // Captured at start (not on the first Resume), so the click does not race the capture.
    let daemon = TestDaemon::start_with_login_shell(&env.daemon_toml(false), &shell);
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[terminal]\nshell = \"{}\"\n", shell.display()),
    )
    .unwrap();
    let app_a = daemon.create_workspace("app-a", &[&env.project_a]);
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced() && store.workspaces().len() == 2 && wb.active_workspace().is_some()
        })
    });
    let home = view.read_with(cx, |wb, _| wb.active_workspace()).unwrap();

    // ---- it is a tab of the right sidebar's strip (the fourth icon, after Files; Changes shows first) -----------
    let docks = view.read_with(cx, |wb, _| wb.state().docks.clone());
    assert_eq!(docks.right.panels.get(3), Some(&PanelId::new("history")));
    assert_eq!(docks.right.active, Some(PanelId::new("files")));
    assert_eq!(docks.left.active, Some(PanelId::new("sessions")));
    let palette = view.read_with(cx, |wb, _| {
        wb.commands()
            .iter()
            .map(|c| c.title.to_string())
            .collect::<Vec<_>>()
    });
    for wanted in ["History: Search History…", "Panel: Open History"] {
        assert!(
            palette.iter().any(|t| t == wanted),
            "{wanted} in the palette"
        );
    }

    // ---- disabled: an explanation and a button; nothing has been read ---------------------------
    cx.dispatch_action(actions::SearchHistory);
    let history = panel(&view, cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some(PanelId::new("history")),
        "the palette command reveals the panel"
    );
    pump_until(cx, "the index status", |cx| {
        history.read_with(cx, |p, _| p.status().is_some())
    });
    assert!(!history.read_with(cx, |p, _| p.status().unwrap().enabled));
    assert!(
        !daemon.paths.index_database.exists(),
        "no index exists while indexing is off"
    );
    // A search is not sent while indexing is off.
    history.update(cx, |p, cx| p.set_query("hutantest", cx));
    pump_until(cx, "nothing", |_| true);
    assert!(history.read_with(cx, |p, _| p.hits().is_empty() && !p.searching()));
    history.update(cx, |p, cx| p.set_query("", cx));

    // ---- Set up history…, tick every source, Enable (real clicks) → indexed ----------------------
    click(&view, cx, Region::History("setup"));
    pump_until(cx, "the sources", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::HistoryItem("source", 3)).is_some()
        })
    });
    for n in 0..4 {
        click(&view, cx, Region::HistoryItem("source", n));
    }
    click(&view, cx, Region::History("enable"));
    pump_until(cx, "indexing to be enabled", |cx| {
        history.read_with(cx, |p, _| p.status().is_some_and(|s| s.enabled))
    });
    pump_until(cx, "the transcripts to be indexed", |cx| {
        history.read_with(cx, |p, _| {
            p.status().is_some_and(|s| {
                ["claude", "codex", "omp"].iter().all(|name| {
                    s.sources
                        .iter()
                        .any(|source| source.name == *name && source.rows > 0)
                })
            })
        })
    });
    assert!(daemon.paths.index_database.exists());
    assert!(rows(&history, cx) >= 10, "rows: {}", rows(&history, cx));

    // ---- search: one row per session, the best snippet with its matches marked ------------------
    search(&history, cx, "stall", |p| !p.hits().is_empty());
    let hits = history.read_with(cx, |p, _| p.hits().to_vec());
    assert_eq!(hits.len(), 1, "{hits:?}");
    let hit = &hits[0];
    assert_eq!(hit.source, HistorySource::Claude);
    assert_eq!(hit.session_key, CLAUDE_ID);
    assert_eq!(hit.title.as_deref(), Some("Scheduler stall investigation"));
    assert!(hit.matches >= 3, "grouped: {} matching rows", hit.matches);
    assert!(
        hit.snippet.contains('«') && hit.snippet.contains('»'),
        "{}",
        hit.snippet
    );
    assert_eq!(
        hit.resume_argv.as_deref(),
        Some(
            &[
                "claude".to_owned(),
                "--resume".to_owned(),
                CLAUDE_ID.to_owned()
            ][..]
        )
    );

    // The Claude hit names its transcript; "Reveal File" shows that file in Finder (here: the
    // window's reveal handler), a Hutan hit has no file.
    let transcript = std::path::PathBuf::from(hit.path.clone().expect("the transcript file"));
    assert!(
        transcript.is_absolute() && transcript.is_file(),
        "{transcript:?}"
    );
    assert_eq!(
        hutan_desktop::workbench::panels::transcript_path(hit),
        Some(transcript.as_path())
    );
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        let revealed = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| revealed.borrow_mut().push(path.to_owned()));
    });
    cx.dispatch_action(actions::RevealHistoryFile(
        transcript.to_string_lossy().into_owned(),
    ));
    assert_eq!(*revealed.borrow(), std::slice::from_ref(&transcript));
    // A file that is gone is not revealed; the window says so.
    cx.dispatch_action(actions::RevealHistoryFile(
        env.base.join("gone.jsonl").to_string_lossy().into_owned(),
    ));
    assert_eq!(revealed.borrow().len(), 1);
    assert!(view.read_with(cx, |wb, _| {
        wb.notice().is_some_and(|n| n.contains("no longer exists"))
    }));

    // Three sessions share the word: one row each, from three sources.
    search(&history, cx, "hutantest", |p| p.hits().len() == 3);
    assert_eq!(
        sources(&history, cx),
        [
            HistorySource::Claude,
            HistorySource::Codex,
            HistorySource::Omp
        ]
    );

    // ---- filters narrow the same search ---------------------------------------------------------
    history.update(cx, |p, cx| {
        p.edit_filters(|f| f.toggle_source(HistorySource::Codex), cx);
    });
    pump_until(cx, "the source filter", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().len() == 1)
    });
    assert_eq!(sources(&history, cx), [HistorySource::Codex]);
    history.update(cx, |p, cx| {
        p.edit_filters(|f| f.toggle_source(HistorySource::Codex), cx);
        p.edit_filters(|f| f.toggle_role(HistoryRole::Assistant), cx);
    });
    pump_until(cx, "the role filter", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().is_empty())
    });
    history.update(cx, |p, cx| {
        p.edit_filters(|f| f.toggle_role(HistoryRole::Assistant), cx);
        p.edit_filters(|f| f.toggle_role(HistoryRole::User), cx);
    });
    pump_until(cx, "the user role", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().len() == 3)
    });
    history.update(cx, |p, cx| {
        p.edit_filters(|f| f.toggle_role(HistoryRole::User), cx);
        p.edit_filters(
            |f| f.range = hutan_desktop::settings::HistoryRange::Week,
            cx,
        );
    });
    pump_until(cx, "the date filter", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().len() == 2)
    });
    assert_eq!(
        sources(&history, cx),
        [HistorySource::Claude, HistorySource::Codex],
        "the 2020 OMP session is older than a week"
    );
    // Project: only sessions that ran inside the active workspace's folder (Home here: none).
    history.update(cx, |p, cx| {
        p.edit_filters(|f| f.range = hutan_desktop::settings::HistoryRange::All, cx);
        p.set_scope(HistoryScope::Workspace, cx);
    });
    pump_until(cx, "the project filter", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().is_empty())
    });
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(app_a.id, window, cx)
    });
    pump_until(cx, "the workspace to be active", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(app_a.id))
    });
    // Switching the workspace asks again by itself.
    pump_until(cx, "the workspace's sessions", |cx| {
        history.read_with(cx, |p, _| !p.searching() && p.hits().len() == 2)
    });
    assert_eq!(
        sources(&history, cx),
        [HistorySource::Claude, HistorySource::Omp],
        "both ran in app-a"
    );
    history.update(cx, |p, cx| p.set_scope(HistoryScope::All, cx));
    view.update_in(cx, |wb, window, cx| wb.switch_workspace(home, window, cx));
    pump_until(cx, "Home to be active", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(home))
    });

    // ---- Resume: Enter on the selected row runs the hit's command in a new terminal -------------
    // The Claude session ran in app-a; Home is active, so `workspace.open` finds app-a and the
    // window switches there.
    search(&history, cx, "stall", |p| p.hits().len() == 1);
    let before = session_ids(&view, cx);
    cx.simulate_keystrokes("down enter");
    let resumed = wait_new_session(&view, cx, &before);
    assert_eq!(resumed.command, ["claude", "--resume", CLAUDE_ID]);
    assert_eq!(resumed.cwd.as_deref(), Some(env.project_a.as_path()));
    assert_eq!(resumed.workspace_id, app_a.id);
    assert_eq!(resumed.title, "resume: Scheduler stall investigation");
    pump_until(cx, "the stand-in agent to run", |cx| {
        screen(&view, cx, resumed.id).contains(&format!("FAKE-claude --resume {CLAUDE_ID}"))
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(app_a.id)
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(resumed.id),
        "the new session is shown"
    );

    // A hit in a folder no workspace holds: workspace.open creates its workspace.
    let workspaces_before = view.read_with(cx, |wb, cx| wb.sessions().read(cx).workspaces().len());
    search(&history, cx, "capybara", |p| p.hits().len() == 1);
    let before = session_ids(&view, cx);
    cx.simulate_keystrokes("down enter");
    let codex = wait_new_session(&view, cx, &before);
    assert_eq!(codex.command, ["codex", "resume", CODEX_ID]);
    assert_eq!(codex.cwd.as_deref(), Some(env.project_b.as_path()));
    assert_ne!(codex.workspace_id, app_a.id);
    assert!(workspace_of(&view, cx, codex.id).contains("app-b"));
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).workspaces().len()),
        workspaces_before + 1
    );

    // ---- ⌘J: `?` searches the history inline ---------------------------------------------------
    cx.simulate_keystrokes("cmd-p");
    cx.simulate_input("?hutantest");
    pump_until(cx, "the inline hits", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker()
                .is_some_and(|p| p.history_state == HistoryState::Done && p.matches.len() == 3)
        })
    });
    let titles = view.read_with(cx, |wb, _| wb.picker_titles());
    assert!(
        titles.iter().any(|t| t == "Scheduler stall investigation"),
        "{titles:?}"
    );
    assert!(
        titles.iter().any(|t| t == "Axolotl cache design"),
        "{titles:?}"
    );
    // Enter resumes the hit that can be resumed.
    view.update(cx, |wb, cx| wb.set_picker_query("?axolotl", cx));
    pump_until(cx, "the axolotl hit", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker().is_some_and(|p| {
                p.history_state == HistoryState::Done && wb.picker_titles().len() == 1
            })
        })
    });
    let before = session_ids(&view, cx);
    cx.simulate_keystrokes("enter");
    let omp = wait_new_session(&view, cx, &before);
    assert_eq!(omp.command, ["omp", format!("--resume={OMP_ID}").as_str()]);
    assert_eq!(omp.cwd.as_deref(), Some(env.project_a.as_path()));
    assert_eq!(
        omp.workspace_id, app_a.id,
        "app-a holds the folder: no other workspace is involved"
    );
    assert!(
        view.read_with(cx, |wb, _| wb.picker().is_none()),
        "Enter closes ⌘J"
    );

    // A hit without a resume command: Enter shows it in the panel instead.
    cx.simulate_keystrokes("cmd-p");
    cx.simulate_input("?pangolin");
    pump_until(cx, "the pangolin hit", |cx| {
        view.read_with(cx, |wb, _| {
            wb.picker().is_some_and(|p| {
                p.history_state == HistoryState::Done && wb.picker_titles().len() == 1
            })
        })
    });
    let before = session_ids(&view, cx);
    cx.simulate_keystrokes("enter");
    pump_until(cx, "the panel to show the hit", |cx| {
        history.read_with(cx, |p, _| {
            !p.searching() && p.query() == "pangolin" && p.selected() == Some(0)
        })
    });
    let shown = history.read_with(cx, |p, _| p.hits()[0].clone());
    assert_eq!(shown.session_key, UNRESUMABLE_ID);
    assert!(shown.resume_argv.is_none(), "{shown:?}");
    assert!(
        new_sessions(&view, cx, &before).is_empty(),
        "nothing was started"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some(PanelId::new("history"))
    );
    // Enter on a row that cannot be resumed (and is no Hutan record) does nothing.
    cx.simulate_keystrokes("enter");
    pump_until(cx, "nothing", |_| true);
    assert!(new_sessions(&view, cx, &before).is_empty());

    // A recorded directory that is gone: the session starts in the active workspace's folder
    // and the window says so.
    search(&history, cx, "zebrafish", |p| p.hits().len() == 1);
    let active_root = view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        store
            .active_workspace()
            .and_then(|w| w.roots.first().cloned())
    });
    let before = session_ids(&view, cx);
    cx.simulate_keystrokes("down enter");
    let gone = wait_new_session(&view, cx, &before);
    assert_eq!(gone.command, ["claude", "--resume", GONE_ID]);
    assert_eq!(gone.cwd, active_root, "it starts where the window is");
    pump_until(cx, "the notice", |cx| {
        view.read_with(cx, |wb, _| {
            wb.notice()
                .is_some_and(|n| n.contains("no longer exists") && n.contains(GONE_CWD))
        })
    });

    // ---- a Hutan block: Open block scrolls its terminal to it ----------------------------------
    let block_session = daemon.sh_titled(
        "block source",
        "printf '\\033]133;A\\007$ \\033]133;B\\007echo hutanblockword\\r\\n\\033]133;C\\007hutanoutput\\r\\n\\033]133;D;0\\007'; \
         printf '\\033]133;A\\007$ \\033]133;B\\007'; sleep 600",
    );
    pump_until(cx, "the session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(block_session.id).is_some()
        })
    });
    search(&history, cx, "hutanblockword", |p| !p.hits().is_empty());
    let block_hit = history.read_with(cx, |p, _| p.hits()[0].clone());
    assert_eq!(block_hit.source, HistorySource::Hutan);
    assert_eq!(block_hit.session_key, block_session.id.to_string());
    assert!(block_hit.resume_argv.is_none());
    // The footer's counts follow: the Hutan record found here is counted without reopening.
    pump_until(cx, "the footer to count the Hutan record", |cx| {
        history.read_with(cx, |p, _| {
            p.status()
                .is_some_and(|s| s.sources.iter().any(|s| s.name == "hutan" && s.rows > 0))
        })
    });
    let terminal = block_session.terminal_id.expect("a terminal");
    let block = daemon
        .blocks(terminal)
        .into_iter()
        .find(|b| {
            b.command
                .as_deref()
                .is_some_and(|c| c.contains("hutanblockword"))
        })
        .expect("the daemon has the block");
    // The hit names the block it came from: the terminal that ran it and the block's id.
    assert_eq!(block_hit.terminal_id, Some(terminal));
    assert_eq!(block_hit.block_id, Some(block.id));
    let target = hutan_desktop::workbench::panels::HutanTarget {
        session: block_session.id.to_string(),
        block: Some(hutan_desktop::workbench::panels::BlockTarget {
            terminal_id: terminal,
            block_id: block.id,
        }),
    };
    assert_eq!(
        hutan_desktop::workbench::panels::hutan_target(&block_hit),
        Some(target)
    );
    cx.simulate_keystrokes("down enter");
    pump_until(cx, "the terminal to scroll to the block", |cx| {
        view.read_with(cx, |wb, _| wb.terminal_view_of(block_session.id))
            .is_some_and(|v| v.read_with(cx, |v, _| v.highlighted_block() == Some(block.id)))
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(block_session.id)
    );

    // ---- Clear (asks first) and Rebuild: the controls are behind the scan strip ------------------
    assert!(rows(&history, cx) > 0);
    click(&view, cx, Region::History("scan"));
    assert!(history.read_with(cx, |p, _| p.scan_open()));
    click(&view, cx, Region::History("clear"));
    assert_eq!(
        history.read_with(cx, |p, _| p.pending_confirmation()),
        Some(HistoryConfirm::Clear)
    );
    history.update(cx, |p, cx| p.cancel_confirmation(cx));
    draw(cx);
    assert!(rows(&history, cx) > 0, "cancelling keeps the index");
    click(&view, cx, Region::History("clear"));
    click(&view, cx, Region::History("confirm"));
    pump_until(cx, "the index to be empty", |cx| {
        history.read_with(cx, |p, _| {
            p.pending_confirmation().is_none()
                && !p.searching()
                && p.hits().is_empty()
                && p.status()
                    .is_some_and(|s| s.enabled && s.sources.iter().all(|s| s.rows == 0))
        })
    });
    assert_eq!(
        history.read_with(cx, |p, _| p.error().map(str::to_owned)),
        None
    );

    click(&view, cx, Region::History("rebuild"));
    assert_eq!(
        history.read_with(cx, |p, _| p.pending_confirmation()),
        Some(HistoryConfirm::Rebuild)
    );
    click(&view, cx, Region::History("confirm"));
    pump_until(cx, "the transcripts to be read again", |cx| {
        rows(&history, cx) >= 10
    });
    search(&history, cx, "hutantest", |p| p.hits().len() == 3);
    // The Hutan record was deleted with the index and cannot come back.
    search(&history, cx, "hutanblockword", |_| true);
    assert!(history.read_with(cx, |p, _| p.hits().is_empty()));
}
