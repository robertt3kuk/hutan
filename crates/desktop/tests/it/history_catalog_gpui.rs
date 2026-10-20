#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Agent Session History panel over `history.sessions` (OL-3), against an in-process daemon
//! whose `daemon.toml` points the index at synthetic Claude / Codex / OMP transcripts in
//! temporary roots (never the real `~/.claude`, `~/.codex`, `~/.omp`): rows with their titles and
//! meta lines, the groups, the Workspace / Project / All scopes, the live filter with `repo:` and
//! `path:`, the ranked full-text results, the sort, the row's details and menu and every action
//! (Resume starts the right argv in a terminal), refresh and the 30 s timer, the off-state card
//! and its Enable flow, and a daemon without the catalog.

use crate::common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use common::history::{
    CLAUDE_ID, CODEX_ID, GONE_CWD, GONE_ID, HistoryEnv, LATE_ID, OMP_ID, UNRESUMABLE_ID,
};
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, PanelDest, PanelId};
use hutan_desktop::workbench::panels::{
    HistoryEntry, HistoryGroupBy, HistoryMode, HistoryPanel, HistorySessionItem,
};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::history::{HistoryScope, HistorySessionSort, HistorySource};
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

fn open_workbench<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    workspaces: usize,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.synced()
                && store.workspaces().len() == workspaces
                && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// Opens the panel in the right dock (wide, like the new layout) and returns it.
fn show_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<HistoryPanel> {
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(
                &PanelId::new("history"),
                PanelDest::Dock(DockPosition::Right),
            )
            .unwrap();
            s.open_panel(&PanelId::new("history"), DockPosition::Right);
            s.resize_dock(DockPosition::Right, 420.0);
        });
    });
    cx.dispatch_action(actions::OpenPanel("history".into()));
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<HistoryPanel>("history"))
        .expect("the History panel is drawn")
}

/// The catalog has arrived and the daemon is done scanning, with `count` sessions loaded.
fn wait_catalog(panel: &Entity<HistoryPanel>, cx: &mut VisualTestContext, count: u64) {
    pump_until(cx, "the catalog", |cx| {
        panel.read_with(cx, |p, _| {
            p.catalog()
                .is_some_and(|c| c.loaded >= count && !c.scan.scanning)
                && !p.loading()
        })
    });
}

fn keys(panel: &Entity<HistoryPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| p.items().iter().map(|i| i.key.clone()).collect())
}

fn titles(panel: &Entity<HistoryPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, _| {
        p.items()
            .iter()
            .map(HistorySessionItem::display_title)
            .collect()
    })
}

fn headers(panel: &Entity<HistoryPanel>, cx: &VisualTestContext) -> Vec<(String, usize)> {
    panel.read_with(cx, |p, _| {
        p.group_headers()
            .into_iter()
            .map(|(label, count, _)| (label, count))
            .collect()
    })
}

fn item_of(panel: &Entity<HistoryPanel>, cx: &VisualTestContext, key: &str) -> usize {
    panel
        .read_with(cx, |p, _| p.items().iter().position(|i| i.key == key))
        .unwrap_or_else(|| panic!("{key} is listed"))
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is drawn"))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let at = bounds(view, cx, region).center();
    cx.simulate_click(at, Modifiers::none());
}

/// Clicks the title line of a row (an open row's middle is its details, which swallow clicks).
fn click_title(view: &Entity<Workbench>, cx: &mut VisualTestContext, item: usize) {
    let row = bounds(view, cx, Region::HistoryItem("row", item));
    let at = row.origin + gpui::point(gpui::px(40.0), gpui::px(18.0));
    cx.simulate_click(at, Modifiers::none());
}

fn new_sessions(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
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

fn session_ids(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<SessionId> {
    new_sessions(view, cx, &[]).iter().map(|s| s.id).collect()
}

fn wait_new_session(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    before: &[SessionId],
) -> SessionInfo {
    pump_until(cx, "the new session", |cx| {
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

fn switch_to(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: hutan_core::WorkspaceId) {
    view.update_in(cx, |wb, window, cx| wb.switch_workspace(id, window, cx));
    pump_until(cx, "the workspace to be active", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(id))
    });
}

fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// The script of a terminal with one finished command (`hutanrowword`), which the daemon's index
/// turns into a Hutan row.
const HUTAN_ROW_SCRIPT: &str = "printf '\\033]133;A\\007$ \\033]133;B\\007echo hutanrowword\\r\\n\\033]133;C\\007rowoutput\\r\\n\\033]133;D;0\\007'; \
     printf '\\033]133;A\\007$ \\033]133;B\\007'; sleep 600";

#[gpui::test]
fn the_catalog_groups_scopes_filters_sorts_and_searches(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    env.write_late_session();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let app_a = daemon.create_workspace("app-a", &[&env.project_a]);
    let app_b = daemon.create_workspace("app-b", &[&env.project_b]);
    let (view, cx) = open_workbench(cx, &daemon, 3);
    let home = view.read_with(cx, |wb, _| wb.active_workspace()).unwrap();
    let panel = show_panel(&view, cx);

    // ---- the catalog is there without a search --------------------------------------------------
    wait_catalog(&panel, cx, 6);
    assert_eq!(panel.read_with(cx, |p, _| p.mode()), HistoryMode::Catalog);
    assert_eq!(panel.read_with(cx, |p, _| p.scope()), HistoryScope::All);
    assert_eq!(
        panel.read_with(cx, |p, _| p.header_title()),
        "Agent Session History"
    );
    let answer = panel.read_with(cx, |p, _| p.catalog().cloned()).unwrap();
    assert_eq!((answer.matched, answer.loaded), (6, 6));
    assert!(answer.scan.files >= 6 && !answer.scan.scanning && answer.scan.finished_ms.is_some());

    // Last updated first: the late session (ten minutes ago), then Claude (an hour), Codex,
    // the unresumable one, the one whose directory is gone, and OMP from 2020.
    assert_eq!(
        keys(&panel, cx),
        [
            LATE_ID,
            CLAUDE_ID,
            CODEX_ID,
            UNRESUMABLE_ID,
            GONE_ID,
            OMP_ID
        ]
    );
    let shown = titles(&panel, cx);
    assert_eq!(
        shown[0], "late bloomer pelican",
        "no title: the first prompt"
    );
    assert_eq!(shown[1], "Scheduler stall investigation");
    assert_eq!(
        shown[2],
        "add retry with jitter to the capybara client hutantest"
    );
    assert_eq!(shown[5], "Axolotl cache design");
    let late = panel.read_with(cx, |p, _| p.items()[0].clone());
    let meta = late.meta(None, hutan_client::time::now_ms());
    assert!(
        meta.starts_with("Claude · 2 msgs · ") && meta.contains("m ago"),
        "{meta}"
    );
    assert_eq!(
        late.latest_turn(),
        Some(("Agent", "Pelicans bloom late.")),
        "the latest turn names its role"
    );
    assert_eq!(late.first_prompt.as_deref(), Some("late bloomer pelican"));

    // The first rows are drawn (the list only builds what the window shows).
    draw(cx);
    let row0 = bounds(&view, cx, Region::HistoryItem("row", 0));
    // 98 px with the divider under it (the probe is the padding box).
    assert!(row0.size.height >= gpui::px(97.0), "{:?}", row0.size);
    bounds(&view, cx, Region::History("count"));

    // ---- groups: by project (the default), folder, agent -----------------------------------------
    assert_eq!(
        panel.read_with(cx, |p, _| p.group_by()),
        HistoryGroupBy::Project
    );
    assert_eq!(
        headers(&panel, cx),
        [
            ("app-a".to_owned(), 3),
            ("app-b".to_owned(), 2),
            ("hutan-test/vanished".to_owned(), 1)
        ],
        "the project the daemon attributed, else the folder"
    );
    panel.update(cx, |p, cx| p.set_group_by(HistoryGroupBy::Agent, cx));
    assert_eq!(
        headers(&panel, cx),
        [
            ("Claude".to_owned(), 4),
            ("Codex".to_owned(), 1),
            ("OMP".to_owned(), 1)
        ]
    );
    panel.update(cx, |p, cx| p.set_group_by(HistoryGroupBy::Folder, cx));
    assert_eq!(
        headers(&panel, cx),
        [
            ("projects/app-a".to_owned(), 3),
            ("projects/app-b".to_owned(), 2),
            ("hutan-test/vanished".to_owned(), 1)
        ]
    );
    // The header button opens the dropdown; its entries pick the grouping.
    click(&view, cx, Region::History("group"));
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("group"));
    click(&view, cx, Region::HistoryItem("menu", 0));
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), None);
    assert_eq!(
        panel.read_with(cx, |p, _| p.group_by()),
        HistoryGroupBy::Project
    );

    // Groups fold: the header stays with its count, the rows go.
    draw(cx);
    click(&view, cx, Region::HistoryItem("group", 0));
    assert_eq!(
        panel.read_with(cx, |p, _| p.group_headers()[0].clone()),
        ("app-a".to_owned(), 3, true)
    );
    let rows = panel.read_with(cx, |p, _| {
        p.entries()
            .iter()
            .filter(|e| matches!(e, HistoryEntry::Row(_)))
            .count()
    });
    assert_eq!(rows, 3, "the folded group's three rows are not in the list");
    click(&view, cx, Region::HistoryItem("group", 0));
    assert!(!panel.read_with(cx, |p, _| p.group_headers()[0].2));

    // ---- scopes: Workspace / Project / All -------------------------------------------------------
    // Home is shown: nothing ran in it; no project is in view, so Project cannot be chosen.
    assert!(!panel.read_with(cx, |p, cx| p.project_scope_available(cx)));
    click(&view, cx, Region::History("scope-project"));
    assert_eq!(panel.read_with(cx, |p, _| p.scope()), HistoryScope::All);
    click(&view, cx, Region::History("scope-workspace"));
    assert_eq!(
        panel.read_with(cx, |p, _| p.scope()),
        HistoryScope::Workspace
    );
    pump_until(cx, "Home's sessions", |cx| {
        panel.read_with(cx, |p, _| {
            !p.loading() && p.catalog().is_some_and(|c| c.matched == 0)
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.catalog().map(|c| c.loaded)),
        Some(6),
        "loaded ignores the scope"
    );
    // Switching the shown workspace asks again by itself.
    switch_to(&view, cx, app_a.id);
    pump_until(cx, "app-a's sessions", |cx| keys(&panel, cx).len() == 3);
    assert_eq!(keys(&panel, cx), [LATE_ID, CLAUDE_ID, OMP_ID]);
    assert!(panel.read_with(cx, |p, cx| p.project_scope_available(cx)));
    click(&view, cx, Region::History("scope-project"));
    assert_eq!(panel.read_with(cx, |p, _| p.scope()), HistoryScope::Project);
    pump_until(cx, "the project's sessions", |cx| {
        panel.read_with(cx, |p, _| {
            !p.loading() && p.catalog().is_some_and(|c| c.matched == 3)
        })
    });
    assert_eq!(keys(&panel, cx), [LATE_ID, CLAUDE_ID, OMP_ID]);
    switch_to(&view, cx, app_b.id);
    pump_until(cx, "app-b's sessions", |cx| keys(&panel, cx).len() == 2);
    assert_eq!(keys(&panel, cx), [CODEX_ID, UNRESUMABLE_ID]);
    click(&view, cx, Region::History("scope-all"));
    pump_until(cx, "everything", |cx| keys(&panel, cx).len() == 6);
    switch_to(&view, cx, home);

    // ---- the live filter: metadata at once, the full text after the debounce ----------------------
    cx.dispatch_action(actions::SearchHistory);
    draw(cx);
    cx.simulate_input("axolotl");
    wait_until(cx, "the catalog filter", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 1)
    });
    assert_eq!(keys(&panel, cx), [OMP_ID]);
    assert!(
        panel.read_with(cx, |p, _| p.searching() && p.mode() == HistoryMode::Catalog),
        "the transcripts are searched after the 250 ms; the filter did not wait for them"
    );
    let answer = panel.read_with(cx, |p, _| p.catalog().cloned()).unwrap();
    assert_eq!((answer.matched, answer.loaded), (1, 6));
    // Then the ranked results replace the list: one flat row per session, with the evidence.
    pump_until(cx, "the full-text results", |cx| {
        panel.read_with(cx, |p, _| p.mode() == HistoryMode::Search && !p.searching())
    });
    assert_eq!(keys(&panel, cx), [OMP_ID]);
    assert!(panel.read_with(cx, |p, _| p.items()[0].evidence.is_some()));
    assert!(panel.read_with(cx, |p, _| p.group_headers().is_empty()));

    // Return does not wait for the debounce.
    panel.update(cx, |p, cx| p.set_query("", cx));
    cx.simulate_input("quokka");
    assert!(panel.read_with(cx, |p, _| p.searching()));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "Return's search", |cx| {
        panel.read_with(cx, |p, _| p.mode() == HistoryMode::Search && !p.searching())
    });
    assert_eq!(keys(&panel, cx), [CLAUDE_ID]);
    // Several sessions: ranked, ungrouped; three of them have the word.
    panel.update(cx, |p, cx| p.set_query("hutantest", cx));
    pump_until(cx, "three hits", |cx| {
        panel.read_with(cx, |p, _| {
            p.mode() == HistoryMode::Search && p.items().len() == 3
        })
    });
    // Escape clears the field and the catalog is back.
    cx.simulate_keystrokes("escape");
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), "");
    pump_until(cx, "the catalog again", |cx| keys(&panel, cx).len() == 6);
    assert_eq!(panel.read_with(cx, |p, _| p.mode()), HistoryMode::Catalog);

    // `repo:` and `path:` only exist in the catalog filter: no transcript search is sent.
    panel.update(cx, |p, cx| p.set_query("repo:app-a", cx));
    assert!(!panel.read_with(cx, |p, _| p.searching()));
    wait_until(cx, "repo:", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 3)
    });
    assert_eq!(keys(&panel, cx), [LATE_ID, CLAUDE_ID, OMP_ID]);
    panel.update(cx, |p, cx| p.set_query("path:app-b", cx));
    wait_until(cx, "path:", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 2)
    });
    assert_eq!(keys(&panel, cx), [CODEX_ID, UNRESUMABLE_ID]);
    panel.update(cx, |p, cx| p.set_query("pelican repo:app-a", cx));
    wait_until(cx, "words with repo:", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 1)
    });
    assert_eq!(keys(&panel, cx), [LATE_ID]);
    pump_until(cx, "no search was sent", |_| true);
    assert_eq!(panel.read_with(cx, |p, _| p.mode()), HistoryMode::Catalog);
    panel.update(cx, |p, cx| p.set_query("", cx));
    wait_until(cx, "everything again", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 6)
    });

    // ---- sources and sort --------------------------------------------------------------------------
    panel.update(cx, |p, cx| p.toggle_source(HistorySource::Codex, cx));
    wait_until(cx, "without Codex", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 5)
    });
    assert!(!keys(&panel, cx).contains(&CODEX_ID.to_owned()));
    assert!(!panel.read_with(cx, |p, _| p.source_shown(HistorySource::Codex)));
    // The sources menu stays open while it is used.
    click(&view, cx, Region::History("sources"));
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("sources"));
    click(&view, cx, Region::HistoryItem("menu", 2)); // Codex again
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("sources"));
    wait_until(cx, "with Codex", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 6)
    });
    assert!(
        panel.read_with(cx, |p, _| p.filters().sources.is_empty()),
        "all = none chosen"
    );
    panel.update(cx, |p, cx| p.close_menu(cx));
    // The last source stays.
    for source in [
        HistorySource::Hutan,
        HistorySource::Claude,
        HistorySource::Omp,
    ] {
        panel.update(cx, |p, cx| p.toggle_source(source, cx));
    }
    panel.update(cx, |p, cx| p.toggle_source(HistorySource::Codex, cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.filters().sources.clone()),
        [HistorySource::Codex]
    );
    wait_until(cx, "only Codex", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 1)
    });
    for source in [
        HistorySource::Hutan,
        HistorySource::Claude,
        HistorySource::Omp,
    ] {
        panel.update(cx, |p, cx| p.toggle_source(source, cx));
    }
    wait_until(cx, "all sources", |cx| {
        panel.read_with(cx, |p, _| !p.loading() && p.sessions().len() == 6)
    });

    // Created: by the first message, newest first — the late session started five hours ago.
    click(&view, cx, Region::History("sort"));
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("sort"));
    click(&view, cx, Region::HistoryItem("menu", 1));
    assert_eq!(
        panel.read_with(cx, |p, _| p.sort()),
        HistorySessionSort::Created
    );
    pump_until(cx, "the created order", |cx| {
        keys(&panel, cx).first().map(String::as_str) == Some(CLAUDE_ID)
    });
    assert_eq!(
        keys(&panel, cx),
        [
            CLAUDE_ID,
            CODEX_ID,
            UNRESUMABLE_ID,
            GONE_ID,
            LATE_ID,
            OMP_ID
        ]
    );
    panel.update(cx, |p, cx| p.set_sort(HistorySessionSort::Updated, cx));
    pump_until(cx, "the updated order", |cx| {
        keys(&panel, cx).first().map(String::as_str) == Some(LATE_ID)
    });

    // ---- the header follows the panel's width -------------------------------------------------------
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Right, 260.0));
    });
    draw(cx);
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.is_narrow()));
    assert_eq!(panel.read_with(cx, |p, _| p.header_title()), "History");
}

#[gpui::test]
fn a_row_expands_has_a_menu_and_every_action_works(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    env.install_stand_ins();
    let shell = env.install_login_shell();
    let daemon = TestDaemon::start_with_login_shell(&env.daemon_toml(true), &shell);
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[terminal]\nshell = \"{}\"\n", shell.display()),
    )
    .unwrap();
    let app_a = daemon.create_workspace("app-a", &[&env.project_a]);
    let app_b = daemon.create_workspace("app-b", &[&env.project_b]);
    let (view, cx) = open_workbench(cx, &daemon, 3);
    let home = view.read_with(cx, |wb, _| wb.active_workspace()).unwrap();
    let panel = show_panel(&view, cx);
    wait_catalog(&panel, cx, 5);

    let opened: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        let opened = Rc::clone(&opened);
        wb.set_open_handler(move |path, _| opened.borrow_mut().push(path.to_owned()));
        let revealed = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| revealed.borrow_mut().push(path.to_owned()));
    });

    // ---- a click opens the row; the first prompt and the latest turns are there -----------------------
    let claude = item_of(&panel, cx, CLAUDE_ID);
    assert!(!panel.read_with(cx, |p, _| p.is_expanded(claude)));
    click(&view, cx, Region::HistoryItem("row", claude));
    assert!(panel.read_with(cx, |p, _| p.is_expanded(claude)));
    assert_eq!(panel.read_with(cx, |p, _| p.selected()), Some(claude));
    let item = panel.read_with(cx, |p, _| p.items()[claude].clone());
    assert!(
        item.first_prompt
            .as_deref()
            .is_some_and(|p| p.contains("quokka scheduler stall")),
        "{:?}",
        item.first_prompt
    );
    assert!(
        item.latest.len() >= 2,
        "the latest turns: {:?}",
        item.latest
    );
    // The buttons: Resume, Copy Resume Command, Open Log, Reveal Log, Open Working Directory,
    // Copy Session ID.
    for n in 0..6 {
        bounds(&view, cx, Region::HistoryItem("action", n));
    }

    // ---- Resume (a real click) runs the argv in a new terminal, in the row's project -------------------
    let before = session_ids(&view, cx);
    click(&view, cx, Region::HistoryItem("action", 0));
    let resumed = wait_new_session(&view, cx, &before);
    assert_eq!(resumed.command, ["claude", "--resume", CLAUDE_ID]);
    assert_eq!(resumed.cwd.as_deref(), Some(env.project_a.as_path()));
    assert_eq!(resumed.workspace_id, app_a.id);
    assert_eq!(resumed.title, "resume: Scheduler stall investigation");
    pump_until(cx, "the stand-in agent to run", |cx| {
        screen(&view, cx, resumed.id).contains(&format!("FAKE-claude --resume {CLAUDE_ID}"))
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(resumed.id)
    );
    switch_to(&view, cx, home);

    // ---- Copy Resume Command, Open Log, Reveal Log, Copy Session ID as buttons -----------------------
    click(&view, cx, Region::HistoryItem("action", 1));
    assert_eq!(
        clipboard(cx).as_deref(),
        Some(format!("claude --resume {CLAUDE_ID}").as_str())
    );
    let transcript = PathBuf::from(item.path.clone().expect("the transcript file"));
    assert!(transcript.is_absolute() && transcript.is_file());
    click(&view, cx, Region::HistoryItem("action", 2));
    assert_eq!(*opened.borrow(), std::slice::from_ref(&transcript));
    click(&view, cx, Region::HistoryItem("action", 3));
    assert_eq!(*revealed.borrow(), std::slice::from_ref(&transcript));
    click(&view, cx, Region::HistoryItem("action", 5));
    assert_eq!(clipboard(cx).as_deref(), Some(CLAUDE_ID));
    assert!(view.read_with(cx, |wb, _| {
        wb.notice()
            .is_some_and(|n| n.contains("Copied the session id"))
    }));
    // Open Working Directory shows the workspace that holds the folder.
    assert_ne!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(app_a.id)
    );
    click(&view, cx, Region::HistoryItem("action", 4));
    pump_until(cx, "the folder's workspace", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(app_a.id))
    });
    switch_to(&view, cx, home);
    // Another click on its title folds the row.
    click_title(&view, cx, claude);
    assert!(!panel.read_with(cx, |p, _| p.is_expanded(claude)));

    // ---- ⋯ and right-click open the same menu ------------------------------------------------------------
    let codex = item_of(&panel, cx, CODEX_ID);
    click(&view, cx, Region::HistoryItem("more", codex));
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("row"));
    // Resume, Copy Resume Command, Open Log, Reveal Log, Open Working Directory, Copy Session
    // ID, Copy Log Path.
    for n in 0..7 {
        bounds(&view, cx, Region::HistoryItem("menu", n));
    }
    click(&view, cx, Region::HistoryItem("menu", 5));
    assert_eq!(clipboard(cx).as_deref(), Some(CODEX_ID));
    assert_eq!(
        panel.read_with(cx, |p, _| p.menu_open()),
        None,
        "picking closes it"
    );

    let omp = item_of(&panel, cx, OMP_ID);
    let at = bounds(&view, cx, Region::HistoryItem("row", omp)).center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    assert_eq!(panel.read_with(cx, |p, _| p.menu_open()), Some("row"));
    let omp_log = PathBuf::from(
        panel
            .read_with(cx, |p, _| p.items()[omp].path.clone())
            .expect("the OMP transcript"),
    );
    click(&view, cx, Region::HistoryItem("menu", 6)); // Copy Log Path
    assert_eq!(
        clipboard(cx).as_deref(),
        Some(omp_log.to_string_lossy().as_ref())
    );
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    click(&view, cx, Region::HistoryItem("menu", 4)); // Open Working Directory (app-a)
    pump_until(cx, "OMP's workspace", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(app_a.id))
    });
    // The Codex session's folder belongs to app-b.
    click(&view, cx, Region::HistoryItem("more", codex));
    click(&view, cx, Region::HistoryItem("menu", 4));
    pump_until(cx, "Codex's workspace", |cx| {
        view.read_with(cx, |wb, _| wb.active_workspace() == Some(app_b.id))
    });
    switch_to(&view, cx, home);

    // A session without a resume command: Resume is off, and the details say why.
    let bad = item_of(&panel, cx, UNRESUMABLE_ID);
    click(&view, cx, Region::HistoryItem("row", bad));
    assert!(panel.read_with(cx, |p, _| p.is_expanded(bad)));
    let before = session_ids(&view, cx);
    click(&view, cx, Region::HistoryItem("action", 0));
    pump_until(cx, "nothing", |_| true);
    assert!(
        new_sessions(&view, cx, &before).is_empty(),
        "nothing was started"
    );
    click_title(&view, cx, bad);
    // A directory that no longer exists is said so, and nothing opens.
    let gone = item_of(&panel, cx, GONE_ID);
    click(&view, cx, Region::HistoryItem("more", gone));
    click(&view, cx, Region::HistoryItem("menu", 4));
    assert!(view.read_with(cx, |wb, _| {
        wb.notice()
            .is_some_and(|n| n.contains("no longer exists") && n.contains(GONE_CWD))
    }));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(home)
    );

    // ---- Return on the selected row resumes it -------------------------------------------------------------
    cx.dispatch_action(actions::SearchHistory);
    draw(cx);
    panel.update(cx, |p, cx| p.set_query("", cx));
    let before = session_ids(&view, cx);
    let codex = item_of(&panel, cx, CODEX_ID);
    // The last row is selected (the menu above selected it); ↑ walks over the rows, not the
    // group headers, in the order the groups list them: the "gone" row of its own group, the
    // unresumable one, then the Codex one (both in app-b).
    let (bad, gone) = (
        item_of(&panel, cx, UNRESUMABLE_ID),
        item_of(&panel, cx, GONE_ID),
    );
    assert_eq!(panel.read_with(cx, |p, _| p.selected()), Some(gone));
    let mut walked = Vec::new();
    for _ in 0..2 {
        cx.simulate_keystrokes("up");
        walked.push(panel.read_with(cx, |p, _| p.selected()).unwrap());
    }
    assert_eq!(walked, [bad, codex]);
    cx.simulate_keystrokes("enter");
    let started = wait_new_session(&view, cx, &before);
    assert_eq!(started.command, ["codex", "resume", CODEX_ID]);
    assert_eq!(started.cwd.as_deref(), Some(env.project_b.as_path()));

    // ---- a Hutan row opens its session ----------------------------------------------------------------------
    let hutan = daemon.sh_titled("hutan row source", HUTAN_ROW_SCRIPT);
    pump_until(cx, "the Hutan session", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(hutan.id).is_some()
        })
    });
    panel.update(cx, |p, cx| p.reload(false, cx));
    pump_until(cx, "the Hutan row", |cx| {
        panel.read_with(cx, |p, _| {
            p.items()
                .iter()
                .any(|i| i.source == HistorySource::Hutan && i.session_id == Some(hutan.id))
        })
    });
    let row = panel.read_with(cx, |p, _| {
        p.items()
            .iter()
            .position(|i| i.session_id == Some(hutan.id))
            .unwrap()
    });
    // Hutan rows have no log and no resume command: Open Session, Open Working Directory, Copy
    // Session ID.
    click(&view, cx, Region::HistoryItem("row", row));
    for n in 0..3 {
        bounds(&view, cx, Region::HistoryItem("action", n));
    }
    let before = session_ids(&view, cx);
    click(&view, cx, Region::HistoryItem("action", 0));
    pump_until(cx, "the session to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session() == Some(hutan.id))
    });
    assert!(
        new_sessions(&view, cx, &before).is_empty(),
        "nothing was started"
    );
}

#[gpui::test]
fn refresh_and_the_timer_keep_the_scroll_position(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    env.write_filler_sessions(40);
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let (view, cx) = open_workbench(cx, &daemon, 1);
    let panel = show_panel(&view, cx);
    wait_catalog(&panel, cx, 45);
    assert_eq!(panel.read_with(cx, |p, _| p.items().len()), 45);
    let count = |cx: &VisualTestContext| panel.read_with(cx, |p, _| p.items().len());

    // Scroll down to a row far from the top and note which session is there.
    panel.update(cx, |p, cx| {
        p.set_group_by(HistoryGroupBy::Agent, cx);
        p.scroll_to_entry(25);
    });
    draw(cx);
    let top = panel.read_with(cx, |p, _| p.first_visible_entry());
    assert_eq!(top, 25);
    let anchor = panel
        .read_with(cx, |p, _| p.entry_session(top))
        .expect("a row");

    // The refresh button asks the daemon to look at the files again: a new transcript shows.
    let extra = env.claude.join("-app-a/refresh-me-1.jsonl");
    let t = common::history::iso(common::history::now_ms() - 30_000);
    std::fs::write(
        &extra,
        format!(
            r#"{{"type":"user","timestamp":"{t}","cwd":"{}","sessionId":"refresh-me-1","message":{{"role":"user","content":[{{"type":"text","text":"brand new session zeppelin"}}]}}}}"#,
            env.project_a.display()
        ) + "\n",
    )
    .unwrap();
    click(&view, cx, Region::History("refresh"));
    pump_until(cx, "the new transcript", |cx| count(cx) == 46);
    assert!(keys(&panel, cx).contains(&"refresh-me-1".to_owned()));
    // The view stayed on the same row, although a row came in above it.
    let top = panel.read_with(cx, |p, _| p.first_visible_entry());
    assert_eq!(
        panel.read_with(cx, |p, _| p.entry_session(top)),
        Some(anchor.clone()),
        "the same session is at the top"
    );

    // The 30 s timer asks again while the panel is visible: a Hutan terminal appears.
    let hutan = daemon.sh_titled("timer source", HUTAN_ROW_SCRIPT);
    draw(cx);
    cx.executor().advance_clock(Duration::from_secs(31));
    pump_until(cx, "the timer's request", |cx| {
        panel.read_with(cx, |p, _| {
            p.items().iter().any(|i| i.session_id == Some(hutan.id))
        })
    });
    let top = panel.read_with(cx, |p, _| p.first_visible_entry());
    assert_eq!(
        panel.read_with(cx, |p, _| p.entry_session(top)),
        Some(anchor),
        "the timer kept the scroll position too"
    );
}

#[gpui::test]
fn the_off_state_is_a_short_card_then_unticked_sources_and_an_enable_button(
    cx: &mut TestAppContext,
) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    let daemon = TestDaemon::start_with_config(&env.daemon_toml(false));
    let (view, cx) = open_workbench(cx, &daemon, 1);
    let panel = show_panel(&view, cx);
    pump_until(cx, "the index status", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some())
    });
    assert!(!panel.read_with(cx, |p, _| p.status().unwrap().enabled));
    // First: one short card and a quiet button, no form (`daemon.toml` has every source on,
    // and still nothing is ticked here).
    pump_until(cx, "the short card", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::History("setup")).is_some()
        })
    });
    for region in [
        Region::HistoryItem("source", 0),
        Region::History("enable"),
        Region::History("search"),
    ] {
        assert!(
            view.read_with(cx, |wb, _| wb.region_bounds(region))
                .is_none(),
            "{region:?}: the card does not show the form until it is asked for"
        );
    }
    assert!(!panel.read_with(cx, |p, _| p.card_open()));
    assert_eq!(panel.read_with(cx, |p, _| p.card_sources()), [false; 4]);

    // *Set up history…* shows the four sources, unticked, with Enable and Settings.
    click(&view, cx, Region::History("setup"));
    pump_until(cx, "the card's sources", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::HistoryItem("source", 3)).is_some()
        })
    });
    for region in [
        Region::History("enable"),
        Region::History("settings"),
        Region::History("not-now"),
        Region::HistoryItem("source", 0),
    ] {
        bounds(&view, cx, region);
    }
    assert!(panel.read_with(cx, |p, _| p.card_open()));
    assert_eq!(
        panel.read_with(cx, |p, _| p.card_sources()),
        [false; 4],
        "nothing is ticked until the user ticks it"
    );
    assert!(!daemon.paths.index_database.exists(), "nothing is read yet");

    // Enable does nothing while no source is ticked.
    click(&view, cx, Region::History("enable"));
    pump_until(cx, "nothing", |_| true);
    assert!(!panel.read_with(cx, |p, _| p.status().unwrap().enabled));
    assert!(!daemon.paths.index_database.exists(), "still nothing read");

    // *Not now* folds the card; the ticks are kept for the next time.
    click(&view, cx, Region::HistoryItem("source", 1));
    click(&view, cx, Region::History("not-now"));
    pump_until(cx, "the short card", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::History("setup")).is_some()
        })
    });
    click(&view, cx, Region::History("setup"));
    pump_until(cx, "the sources", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::HistoryItem("source", 0)).is_some()
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.card_sources()),
        [false, true, false, false]
    );
    click(&view, cx, Region::HistoryItem("source", 1));
    assert_eq!(panel.read_with(cx, |p, _| p.card_sources()), [false; 4]);

    // Hutan, Claude and OMP ticked, Codex not; Enable writes the sources to daemon.toml and
    // turns indexing on.
    for n in [0, 1, 3] {
        click(&view, cx, Region::HistoryItem("source", n));
    }
    assert_eq!(
        panel.read_with(cx, |p, _| p.card_sources()),
        [true, true, false, true]
    );
    click(&view, cx, Region::History("enable"));
    pump_until(cx, "indexing to be on", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| s.enabled))
    });
    let toml = std::fs::read_to_string(&daemon.paths.config_file).unwrap();
    assert!(toml.contains("codex = false"), "{toml}");
    assert!(
        !toml.contains("claude = false") && !toml.contains("omp = false"),
        "{toml}"
    );
    wait_catalog(&panel, cx, 1);
    pump_until(cx, "the transcripts", |cx| keys(&panel, cx).len() == 4);
    let sessions: Vec<HistorySource> =
        panel.read_with(cx, |p, _| p.items().iter().map(|i| i.source).collect());
    assert!(
        !sessions.contains(&HistorySource::Codex),
        "Codex was not ticked: {sessions:?}"
    );
    // The panel has its search field, scan strip and list bar now.
    for region in [
        Region::History("search"),
        Region::History("scan"),
        Region::History("count"),
    ] {
        bounds(&view, cx, region);
    }
    // Disable (behind the scan strip) brings the card back.
    click(&view, cx, Region::History("scan"));
    click(&view, cx, Region::History("disable"));
    pump_until(cx, "indexing to be off", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| !s.enabled))
    });
    assert!(panel.read_with(cx, |p, _| p.sessions().is_empty()));
    pump_until(cx, "the short card again", |cx| {
        draw(cx);
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::History("setup")).is_some()
        })
    });
}

#[gpui::test]
fn a_daemon_without_the_catalog_gets_the_search_alone(cx: &mut TestAppContext) {
    let env = HistoryEnv::new();
    env.write_fixtures();
    let mut daemon = TestDaemon::start_with_config(&env.daemon_toml(true));
    let connection = daemon.connection_without_history_sessions();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        connection,
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let panel = show_panel(&view, cx);
    pump_until(cx, "the index status", |cx| {
        panel.read_with(cx, |p, _| p.status().is_some_and(|s| s.enabled))
    });
    assert!(!panel.read_with(cx, |p, cx| p.catalog_supported(cx)));
    // Nothing was asked of the catalog; the scope control and the search field are there.
    pump_until(cx, "nothing", |_| true);
    assert!(panel.read_with(cx, |p, _| p.catalog().is_none()
        && p.catalog_error().is_none()));
    bounds(&view, cx, Region::History("search"));
    bounds(&view, cx, Region::History("scope-all"));
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::History("group")))
            .is_none(),
        "no group-by without a catalog"
    );
    // The full-text search works as before.
    cx.dispatch_action(actions::SearchHistory);
    draw(cx);
    cx.simulate_input("stall");
    pump_until(cx, "the hits", |cx| {
        panel.read_with(cx, |p, _| !p.searching() && !p.hits().is_empty())
    });
    assert_eq!(keys(&panel, cx), [CLAUDE_ID]);
    assert_eq!(panel.read_with(cx, |p, _| p.mode()), HistoryMode::Search);
}
