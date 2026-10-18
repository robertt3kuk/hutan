#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Orca-style polish (OL-4): the window title follows the center, the nav rows' metrics, the
//! Agent Dashboard drawer, the Assistant header at narrow widths, and the icons.

mod common;

use common::{TestDaemon, offline_connection, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::icons::Icon;
use hutan_desktop::workbench::model::{DockPosition, FocusTarget, ItemRef, PanelId};
use hutan_desktop::workbench::{DRAWER_MAX_WIDTH, DashboardColumn, Region, Workbench};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open_offline(
    cx: &mut TestAppContext,
    assistant: bool,
) -> (Entity<Workbench>, &mut VisualTestContext) {
    let (connection, settings, _) = offline_connection();
    if assistant {
        settings
            .write_user("assistant.enabled", toml::Value::Boolean(true))
            .unwrap();
    }
    open_with(cx, settings, connection, None)
}

fn open_daemon<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    open_with(cx, settings, daemon.connection(), None)
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    region: Region,
) -> gpui::Bounds<gpui::Pixels> {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(region))
            .is_some()
    });
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap()
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
        .is_some()
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    let at = bounds(view, cx, region).center();
    cx.simulate_click(at, Modifiers::none());
}

fn drawer_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.agents_drawer_open())
}

// ---- the window title -------------------------------------------------------------------------

#[gpui::test]
fn the_window_title_follows_the_center_item_never_a_sidebar_page(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, true);
    common::draw(cx);
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));

    // Left pages: Tasks, then the Assistant.
    click(&view, cx, Region::Nav("tasks"));
    common::draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().focus),
        FocusTarget::Dock(DockPosition::Left),
        "the page has the keyboard"
    );
    assert_eq!(view.read_with(cx, |wb, cx| wb.active_title(cx)), "Welcome");
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));
    click(&view, cx, Region::Nav("assistant"));
    common::draw(cx);
    assert_eq!(cx.window_title().as_deref(), Some("Welcome — Hutan"));

    // Every tab of the right sidebar: History, Changes, Requests, Agents, Resources.
    for index in 0..5 {
        click(&view, cx, Region::DockTab(DockPosition::Right, index));
        common::draw(cx);
        assert_eq!(
            view.read_with(cx, |wb, _| wb.state().focus),
            FocusTarget::Dock(DockPosition::Right)
        );
        assert_eq!(
            cx.window_title().as_deref(),
            Some("Welcome — Hutan"),
            "right tab {index}"
        );
    }
}

// ---- the nav ----------------------------------------------------------------------------------

#[gpui::test]
fn nav_rows_are_thirty_pixels_with_a_real_icon(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, true);
    let entries = view.read_with(cx, |wb, cx| wb.nav_entries(cx));
    let icons: Vec<(String, Option<Icon>)> = entries.into_iter().map(|e| (e.key, e.icon)).collect();
    assert_eq!(
        icons,
        [
            ("search".to_owned(), Some(Icon::Search)),
            ("tasks".to_owned(), Some(Icon::ListChecks)),
            ("agents".to_owned(), Some(Icon::LayoutDashboard)),
            ("assistant".to_owned(), Some(Icon::Sparkles)),
        ]
    );
    let mut previous_bottom = None;
    for key in ["search", "tasks", "agents", "assistant"] {
        let row = bounds(&view, cx, Region::Nav(key));
        let height = f32::from(row.size.height);
        assert!(
            (28.0..=32.0).contains(&height),
            "{key} is {height} px high, Orca's rows are about 30"
        );
        if let Some(bottom) = previous_bottom {
            // 2 px between two rows.
            let gap: f32 = f32::from(row.origin.y) - bottom;
            assert!((gap - 2.0).abs() <= 1.0, "{key}: gap {gap}");
        }
        previous_bottom = Some(f32::from(row.bottom()));
        // 8 px of padding around the list: the row spans the sidebar minus 2 × 8 px.
        let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
        let inset: f32 = f32::from(row.origin.x - dock.origin.x);
        assert!((inset - 8.0).abs() <= 1.0, "{key}: inset {inset}");
    }
}

#[gpui::test]
fn a_panel_the_set_has_no_icon_for_keeps_its_glyph(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.open_panel(&PanelId::new("notifications"), DockPosition::Left);
        });
    });
    let entries = view.read_with(cx, |wb, cx| wb.nav_entries(cx));
    let notifications = entries.iter().find(|e| e.key == "notifications").unwrap();
    assert_eq!(notifications.icon, None);
    assert!(!notifications.glyph.is_empty());
    let resources_icon = Icon::for_panel("resources");
    assert_eq!(resources_icon, Some(Icon::Gauge));
}

// ---- the Agent Dashboard drawer ---------------------------------------------------------------

fn report(daemon: &TestDaemon, session: SessionId, state: AgentState, message: &str, seq: u64) {
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: session,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(state),
        message: Some(message.into()),
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
}

struct Board {
    needs: SessionId,
    working: SessionId,
    done: SessionId,
    idle: SessionId,
}

/// Four agents, one per column. Plain shells are not cards, so each session is an agent that
/// reports. The done one ends while the window watches ([`finish_turn`]): an unseen end is
/// what the window shows as done.
fn start_board(daemon: &TestDaemon) -> Board {
    let needs = daemon.sh_titled("Fix the build", "sleep 600").id;
    let working = daemon.sh_titled("Write the docs", "sleep 600").id;
    let idle = daemon.sh_titled("Waiting", "sleep 600").id;
    let done = daemon.sh_titled("One-off", "read line; exit 0").id;
    report(daemon, needs, AgentState::Blocked, "wants to run rm -rf", 1);
    report(daemon, working, AgentState::Working, "editing README.md", 1);
    report(daemon, idle, AgentState::Idle, "ready", 1);
    report(daemon, done, AgentState::Working, "a quick job", 1);
    Board {
        needs,
        working,
        done,
        idle,
    }
}

/// The done agent's process ends while the window is open.
fn finish_turn(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    daemon: &TestDaemon,
    board: &Board,
) {
    wait_until(cx, "the window to see the working agent", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(board.done)
                .is_some_and(|s| s.agent.is_some())
        })
    });
    let terminal = daemon.session(board.done).terminal_id.expect("a terminal");
    daemon.send_input(terminal, b"\n");
}

fn wait_board(view: &Entity<Workbench>, cx: &mut VisualTestContext, board: &Board) {
    wait_until(cx, "the sessions in their columns", |cx| {
        view.read_with(cx, |wb, cx| {
            let columns = wb.agents_dashboard(cx);
            let has = |column: DashboardColumn, id: SessionId| {
                columns
                    .iter()
                    .find(|(c, _)| *c == column)
                    .is_some_and(|(_, cards)| cards.iter().any(|card| card.session == id))
            };
            has(DashboardColumn::NeedsYou, board.needs)
                && has(DashboardColumn::Working, board.working)
                && has(DashboardColumn::Done, board.done)
                && has(DashboardColumn::Idle, board.idle)
        })
    });
}

#[gpui::test]
fn the_agents_row_opens_a_kanban_drawer_and_the_nav_row_or_escape_closes_it(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let board = start_board(&daemon);
    let (view, cx) = open_daemon(cx, &daemon);
    finish_turn(&view, cx, &daemon, &board);
    wait_board(&view, cx, &board);
    assert!(!drawer_open(&view, cx));
    assert!(!painted(&view, cx, Region::AgentsDrawer));

    // The nav row opens it, and a second click closes it.
    click(&view, cx, Region::Nav("agents"));
    assert!(drawer_open(&view, cx));
    let drawer = bounds(&view, cx, Region::AgentsDrawer);
    let selected: Vec<String> = view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx)
            .into_iter()
            .filter(|e| e.selected)
            .map(|e| e.key)
            .collect()
    });
    assert_eq!(selected, ["agents"], "the open drawer marks its row");
    click(&view, cx, Region::Nav("agents"));
    assert!(!drawer_open(&view, cx));
    common::draw(cx);
    assert!(!painted(&view, cx, Region::AgentsDrawer));

    // Escape closes it (the drawer holds the keyboard while it is open).
    click(&view, cx, Region::Nav("agents"));
    assert!(drawer_open(&view, cx));
    cx.simulate_keystrokes("escape");
    assert!(!drawer_open(&view, cx));
    // ... and hands the keyboard back to the workbench.
    cx.simulate_keystrokes("escape");

    // The palette command toggles it too.
    cx.dispatch_action(hutan_desktop::workbench::actions::ToggleAgentsDrawer);
    assert!(drawer_open(&view, cx));
    cx.dispatch_action(hutan_desktop::workbench::actions::ToggleAgentsDrawer);
    assert!(!drawer_open(&view, cx));

    // It is a drawer over the center: from the left sidebar's right edge, between the title bar
    // and the status bar, at most 1294 px wide.
    click(&view, cx, Region::Nav("agents"));
    let drawer_now = bounds(&view, cx, Region::AgentsDrawer);
    assert_eq!(drawer_now.size, drawer.size, "the same drawer");
    let left = bounds(&view, cx, Region::Dock(DockPosition::Left));
    let titlebar = bounds(&view, cx, Region::Titlebar);
    let status = bounds(&view, cx, Region::StatusBar);
    let window = cx.update(|window, _| window.viewport_size());
    let near = |a: gpui::Pixels, b: gpui::Pixels| (f32::from(a) - f32::from(b)).abs() <= 3.0;
    assert!(near(drawer_now.origin.x, left.right()), "left edge");
    assert!(
        near(drawer_now.origin.y, titlebar.bottom()),
        "below the title bar"
    );
    assert!(
        near(drawer_now.bottom(), status.origin.y),
        "above the status bar"
    );
    let expected = (f32::from(window.width) - f32::from(left.right())).min(DRAWER_MAX_WIDTH);
    assert!(
        (f32::from(drawer_now.size.width) - expected).abs() <= 3.0,
        "width {} of {expected}",
        drawer_now.size.width
    );
    // The left sidebar keeps the Projects list.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.left.active.clone()),
        Some(PanelId::new("sessions"))
    );
}

#[gpui::test]
fn the_drawer_lists_the_sessions_in_the_column_of_their_state_and_a_card_opens_its_session(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let board = start_board(&daemon);
    let (view, cx) = open_daemon(cx, &daemon);
    finish_turn(&view, cx, &daemon, &board);
    wait_board(&view, cx, &board);
    click(&view, cx, Region::Nav("agents"));

    // Columns, left to right: Needs you · Working · Done · Idle.
    let order: Vec<DashboardColumn> = view.read_with(cx, |wb, cx| {
        wb.agents_dashboard(cx)
            .into_iter()
            .map(|(c, _)| c)
            .collect()
    });
    assert_eq!(
        order,
        [
            DashboardColumn::NeedsYou,
            DashboardColumn::Working,
            DashboardColumn::Done,
            DashboardColumn::Idle
        ]
    );
    let mut previous_right = None;
    for column in order {
        let header = bounds(&view, cx, Region::DrawerColumn(column));
        assert!(
            f32::from(header.size.width) >= 219.0,
            "{column:?} is at least 220 px wide, got {}",
            header.size.width
        );
        if let Some(right) = previous_right {
            assert!(header.origin.x >= right, "{column:?} starts after the last");
        }
        previous_right = Some(header.right());
    }

    // Each card sits under the header of its column; its text says what the card is.
    for (id, column) in [
        (board.needs, DashboardColumn::NeedsYou),
        (board.working, DashboardColumn::Working),
        (board.done, DashboardColumn::Done),
        (board.idle, DashboardColumn::Idle),
    ] {
        let header = bounds(&view, cx, Region::DrawerColumn(column));
        let card = bounds(&view, cx, Region::DrawerCard(id));
        assert!(
            card.origin.x >= header.origin.x - gpui::px(1.0)
                && card.right() <= header.right() + gpui::px(1.0),
            "{column:?}: the card is inside its column"
        );
        assert!(
            card.origin.y > header.origin.y,
            "{column:?}: below the header"
        );
    }
    let cards = view.read_with(cx, |wb, cx| wb.agents_dashboard(cx));
    let card_of = |column: DashboardColumn| {
        cards
            .iter()
            .find(|(c, _)| *c == column)
            .unwrap()
            .1
            .first()
            .unwrap()
            .clone()
    };
    let needs = card_of(DashboardColumn::NeedsYou);
    assert_eq!(needs.title, "Fix the build");
    assert_eq!(needs.agent, "omp");
    assert_eq!(needs.message, "wants to run rm -rf");
    assert!(!needs.age.is_empty());
    assert_eq!(
        card_of(DashboardColumn::Working).message,
        "editing README.md"
    );

    // Clicking a card opens the session and closes the drawer.
    click(&view, cx, Region::DrawerCard(board.working));
    assert!(!drawer_open(&view, cx));
    let active = view.read_with(cx, |wb, _| wb.state().active_item());
    assert_eq!(
        active,
        Some(ItemRef::Terminal {
            session: board.working.to_string()
        })
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.active_title(cx)),
        "Write the docs"
    );
}

#[gpui::test]
fn opening_another_nav_row_closes_the_drawer(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    click(&view, cx, Region::Nav("agents"));
    assert!(drawer_open(&view, cx));
    click(&view, cx, Region::Nav("tasks"));
    assert!(!drawer_open(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.left.active.clone()),
        Some(PanelId::new("tasks"))
    );
}

// ---- the Assistant page at narrow widths ---------------------------------------------------------

#[gpui::test]
fn the_assistant_header_is_one_compact_row_that_fits_the_sidebar_from_220_to_500(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx, true);
    click(&view, cx, Region::Nav("assistant"));
    for width in [220.0_f32, 280.0, 350.0, 500.0] {
        view.update(cx, |wb, cx| {
            wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Left, width));
        });
        common::draw(cx);
        let dock = bounds(&view, cx, Region::Dock(DockPosition::Left));
        let header = bounds(&view, cx, Region::Assistant("header"));
        assert!(
            f32::from(header.size.height) <= 40.0,
            "one row at {width}: {} px",
            header.size.height
        );
        for region in ["scope", "backend"] {
            let element = bounds(&view, cx, Region::Assistant(region));
            assert!(
                element.size.width < dock.size.width,
                "{region} is {} wide in a {width} px sidebar",
                element.size.width
            );
            assert!(
                element.origin.x >= dock.origin.x - gpui::px(1.0)
                    && element.right() <= dock.right() + gpui::px(1.0),
                "{region} sticks out of a {width} px sidebar: {:?} in {:?}",
                element,
                dock
            );
            // One row: the controls share the header's line.
            assert!(
                element.origin.y >= header.origin.y - gpui::px(1.0)
                    && element.bottom() <= header.bottom() + gpui::px(1.0),
                "{region} is in the header's row at {width}"
            );
        }
        // The retired second row and its buttons are gone.
        assert!(!painted(&view, cx, Region::Assistant("global")));
        assert!(!painted(&view, cx, Region::Assistant("workspace")));
    }
}

// ---- icons ------------------------------------------------------------------------------------------

#[gpui::test]
fn every_panel_of_the_right_strip_has_an_icon_of_the_set(cx: &mut TestAppContext) {
    let (view, cx) = open_offline(cx, false);
    let right = view.read_with(cx, |wb, _| wb.state().docks.right.panels.clone());
    assert!(!right.is_empty());
    for panel in right {
        assert!(
            Icon::for_panel(panel.as_str()).is_some(),
            "{panel} has no icon"
        );
    }
}

#[gpui::test]
fn opening_the_overview_panel_shows_the_board_in_the_center_not_in_the_sidebar(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open_offline(cx, false);
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "overview".into(),
    ));
    let s = view.read_with(cx, |wb, _| wb.state().clone());
    assert_eq!(s.preset, "overview");
    assert!(s.docks.left.panels.iter().all(|p| p.as_str() != "overview"));
    assert!(
        s.active_pane()
            .unwrap()
            .items
            .contains(&ItemRef::panel("overview"))
    );
}
