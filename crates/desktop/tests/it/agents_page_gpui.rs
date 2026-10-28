#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Agents page (AG-1) against an in-process daemon: one retained center tab shown by the
//! nav and by the Agents overview, the shared presentation behind the page, the panel and
//! Today, a virtualized list, the keyboard, and a page that is not on screen doing no work.

use crate::common;

use std::time::Duration;

use common::agents::{Node, Reporter};
use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::sessions::Group;
use hutan_desktop::sessions::agents_text;
use hutan_desktop::workbench::chat::LoadStatus;
use hutan_desktop::workbench::model::{DockPosition, ItemRef};
use hutan_desktop::workbench::panels::{
    AgentsPage, AgentsPanel, AgentsRequest, TodaySection, today,
};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request};
use hutan_settings::{SettingsPaths, SettingsStore};

const PAGE: &str = "agents-dashboard";

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        view.read_with(cx, |wb, _| wb.region_bounds(what)).is_some()
    });
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(what))
        .unwrap()
        .center();
    // Press and release together: see `common::click_at`.
    common::click_at(cx, at);
}

fn page(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<AgentsPage> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<AgentsPage>(PAGE))
        .expect("the Agents page is drawn")
}

fn listed(page: &Entity<AgentsPage>, cx: &VisualTestContext) -> Vec<SessionId> {
    page.read_with(cx, |p, _| p.listed_sessions())
}

fn active_is_page(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.state().active_center_item()) == Some(ItemRef::panel(PAGE))
}

/// A shell session an agent reports from.
fn agent(daemon: &TestDaemon, title: &str, working: bool) -> SessionId {
    let id = daemon.sh_titled(title, "sleep 300").id;
    let rep = Reporter::new(id, "omp-extension", "omp");
    let root = Node::main("root").name("omp");
    rep.report(daemon, if working { root.working() } else { root.idle() });
    id
}

#[gpui::test]
fn the_nav_and_the_agents_overview_show_the_same_page_with_the_same_selection(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = agent(&daemon, "work", true);
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    assert!(active_is_page(&view, cx));
    let nav_page = page(&view, cx);
    let p = nav_page.clone();
    wait_until(cx, "the agent to be listed", move |cx| {
        listed(&p, cx).contains(&id)
    });
    nav_page.update(cx, |p, cx| p.select(id, cx));

    // ⌘⇧O shows the same entity, with the selection it had.
    cx.simulate_keystrokes("cmd-shift-o");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().preset.clone()),
        "overview"
    );
    let overview_page = page(&view, cx);
    assert_eq!(
        overview_page.entity_id(),
        nav_page.entity_id(),
        "one entity"
    );
    assert_eq!(
        overview_page.read_with(cx, |p, _| p.selected_session()),
        Some(id)
    );

    // ... and ⌘⇧O again restores the layout that was there before.
    cx.simulate_keystrokes("cmd-shift-o");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().preset.clone()),
        "default"
    );
}

#[gpui::test]
fn a_row_is_a_bar_inset_to_the_pages_16_px_padding(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = agent(&daemon, "work", true);
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the agent to be listed", move |cx| {
        listed(&p, cx).contains(&id)
    });
    page.update(cx, |p, cx| p.select(id, cx));
    draw(cx);
    let (row, list) = (
        region(&view, cx, Region::AgentRow(id)).expect("the row"),
        region(&view, cx, Region::AgentsPage("list")).expect("the list"),
    );
    // Probes measure the content box: the bar is 16 px from either edge of the list (±1).
    let (left, right) = (
        f32::from(row.left() - list.left()),
        f32::from(list.right() - row.right()),
    );
    assert!(
        (left - 16.0).abs() <= 1.5 && (right - 16.0).abs() <= 1.5,
        "the selected bar is inset 16 px, not edge to edge: {left} / {right}"
    );
}

#[gpui::test]
fn an_agent_with_an_unknown_state_is_one_session_everywhere_and_never_working(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = daemon.sh_titled("quiet", "sleep 300").id;
    // The root has no state: no evidence of what the agent does.
    Reporter::new(id, "omp-extension", "omp").report(&daemon, Node::main("root").name("omp"));
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the unconfirmed group", move |cx| {
        p.read_with(cx, |p, _| p.listed_groups().contains(&Group::Unconfirmed))
    });
    assert_eq!(listed(&page, cx), [id]);
    let counts = page.read_with(cx, |p, cx| p.counts(cx));
    assert_eq!(
        (counts.sessions, counts.unconfirmed, counts.working),
        (1, 1, 0)
    );
    // Today: a "State unavailable" count, not a false Working row.
    let rail = view.read_with(cx, |wb, cx| today(wb.sessions().read(cx)));
    assert_eq!(rail.unconfirmed, 1);
    assert!(rail.working.is_empty());
    assert_eq!(
        rail.sections(),
        [TodaySection::Working],
        "Today draws the one section that has something in it"
    );
    // The panel lists the same session.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "agents".into(),
    ));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<AgentsPanel>("agents"))
        .expect("the panel");
    let sections = panel.read_with(cx, |p, cx| p.sections(0, cx));
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].session, id);
}

#[gpui::test]
fn a_plain_agent_report_is_one_session_in_the_page_and_the_panel_with_the_same_words(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = daemon.sh_titled("reporter", "sleep 300").id;
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        pid: None,
        session_id: id,
        source: "test".into(),
        agent: "omp".into(),
        state: Some(AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    }));
    assert!(applied.applied);
    // A plain shell nobody reports from is no agent: only `reporter` is listed anywhere.
    daemon.sh_titled("just a shell", "sleep 300");
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the reporting agent", move |cx| {
        p.read_with(cx, |p, _| p.listed_sessions().len() == 1)
    });
    assert_eq!(listed(&page, cx), [id]);
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "agents".into(),
    ));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<AgentsPanel>("agents"))
        .expect("the panel");
    let sections = panel.read_with(cx, |p, cx| p.sections(0, cx));
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].session, id);
    assert_eq!(sections[0].word, "Working");
    assert_eq!(sections[0].rows.len(), 1);
    let (panel_counts, page_counts) = (
        panel.read_with(cx, |p, cx| p.counts(cx)),
        page.read_with(cx, |p, cx| p.counts(cx)),
    );
    assert_eq!(panel_counts, page_counts);
    assert_eq!(panel_counts.sessions, 1);
}

#[gpui::test]
fn the_keyboard_walks_the_rows_return_opens_and_escape_closes_the_innermost_first(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let first = agent(&daemon, "first", true);
    let second = agent(&daemon, "second", false);
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "both agents", move |cx| listed(&p, cx).len() == 2);

    cx.simulate_keystrokes("down");
    let on_first_row = page.read_with(cx, |p, _| p.selected_session());
    assert_eq!(on_first_row, listed(&page, cx).first().copied());
    cx.simulate_keystrokes("end");
    assert_eq!(
        page.read_with(cx, |p, _| p.selected_session()),
        listed(&page, cx).last().copied()
    );
    cx.simulate_keystrokes("home");
    assert_eq!(
        page.read_with(cx, |p, _| p.selected_session()),
        on_first_row
    );

    // Space expands the details; Escape closes them and leaves the page where it is.
    let selected = on_first_row.unwrap();
    cx.simulate_keystrokes("space");
    assert!(page.read_with(cx, |p, _| p.details_open(selected)));
    cx.simulate_keystrokes("escape");
    assert!(!page.read_with(cx, |p, _| p.details_open(selected)));
    assert!(active_is_page(&view, cx), "Escape never closes the page");
    cx.simulate_keystrokes("escape");
    assert!(active_is_page(&view, cx));

    // Return opens the selected session (it never answers anything).
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(!active_is_page(&view, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(selected)
    );
    let _ = (first, second);
}

/// Rows the page builds for one frame once everything settled (and that a frame builds none
/// again when nothing changed).
fn rows_in_a_frame(
    view: &Entity<Workbench>,
    page: &Entity<AgentsPage>,
    cx: &mut VisualTestContext,
) -> usize {
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(50));
        draw(cx);
    }
    let rows = page.read_with(cx, |p, _| p.rows_rendered());
    // One frame that redraws the page (the list builds the rows in view again).
    page.update(cx, |_, cx| cx.notify());
    draw(cx);
    let one_frame = page.read_with(cx, |p, _| p.rows_rendered()) - rows;
    // "Nothing changed, so the rows are not built again" is about frames in which the store did
    // not change: the daemon's last answers (a session's state, its title) can still land while
    // the test measures, under load even after the list shows every session, and a store that
    // changed rebuilds the rows as it should. So: draw until five frames pass without the
    // store's revision moving, and hold the page to building nothing in those.
    wait_until(cx, "five frames of a store that did not change", |cx| {
        draw(cx);
        let revision = store_revision(view, cx);
        let builds = page.read_with(cx, |p, _| p.listing_builds());
        for _ in 0..5 {
            draw(cx);
        }
        if store_revision(view, cx) != revision {
            return false;
        }
        assert_eq!(
            page.read_with(cx, |p, _| p.listing_builds()),
            builds,
            "nothing changed, so the rows are not built again"
        );
        true
    });
    one_frame
}

/// The revision of the store the page lists from: it moves when the daemon's state does.
fn store_revision(view: &Entity<Workbench>, cx: &VisualTestContext) -> u64 {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).revision())
}

#[gpui::test]
fn the_list_builds_only_the_rows_in_view_whether_there_are_none_one_or_hundreds(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    // None: the teaching state, no list at all, and Today has no section to draw.
    assert_eq!(rows_in_a_frame(&view, &page, cx), 0, "no agents, no rows");
    assert!(view.read_with(cx, |wb, cx| {
        today(wb.sessions().read(cx)).sections().is_empty()
    }));
    assert!(listed(&page, cx).is_empty());

    // The numbers of sessions a viewport can show many times over.
    let mut made = 0usize;
    let mut built = Vec::new();
    for target in [1usize, 50, 200] {
        while made < target {
            agent(&daemon, &format!("agent-{made}"), made.is_multiple_of(2));
            made += 1;
        }
        let p = page.clone();
        wait_until(cx, "every agent to be listed", move |cx| {
            listed(&p, cx).len() >= target
        });
        built.push(rows_in_a_frame(&view, &page, cx));
    }
    // A viewport holds a bounded number of rows however many sessions exist.
    assert!(built[0] >= 1, "{built:?}");
    assert!(
        built.iter().all(|rows| *rows <= 40),
        "a frame built {built:?} rows for 1, 50 and 200 sessions"
    );
    assert!(
        built[2] <= built[1] + 4,
        "4x the sessions must not mean more rows per frame: {built:?}"
    );
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

#[gpui::test]
fn a_request_is_answered_only_by_its_option_button_never_by_return_on_the_row(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    // The chat asks for a permission: one pending approval at the daemon.
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(info.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    cx.simulate_input("Run `touch hi.txt` in a shell, then reply with the word done.");
    cx.simulate_keystrokes("cmd-enter");
    let pending = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len())
    };
    wait_until(cx, "the approval", |cx| pending(&view, cx) == 1);

    // The page lists the chat under Requests with its request expanded.
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the chat on the page", move |cx| {
        p.read_with(cx, |p, _| p.listed_groups().contains(&Group::Requests))
    });
    page.update(cx, |p, cx| {
        p.apply_request(
            AgentsRequest {
                session: Some(info.id.to_string()),
                review: true,
                ..AgentsRequest::default()
            },
            cx,
        );
    });
    wait_until(cx, "the request's options", |cx| {
        region(&view, cx, Region::AgentOption(info.id, 0)).is_some()
    });
    assert_eq!(page.read_with(cx, |p, _| p.reviewing()), Some(info.id));

    // Return on the selected row opens the session; the approval is untouched.
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(!active_is_page(&view, cx), "Return opened the session");
    assert_eq!(pending(&view, cx), 1, "Return never approves");
    assert!(!env.received().iter().any(|(m, _)| m == "response"));

    // Back on the page, the explicit option answers it, once, with exactly that option.
    click(&view, cx, Region::Nav("agents"));
    wait_until(cx, "the option to be drawn again", |cx| {
        region(&view, cx, Region::AgentOption(info.id, 0)).is_some()
    });
    click(&view, cx, Region::AgentOption(info.id, 0));
    wait_until(cx, "the daemon to resolve it", |cx| pending(&view, cx) == 0);
    let answers: Vec<_> = env
        .received()
        .into_iter()
        .filter(|(m, _)| m == "response")
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].1["result"]["decision"], "accept");
}

#[gpui::test]
fn the_selected_session_survives_updates_and_a_reorder_under_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let mut reporters = Vec::new();
    let mut ids = Vec::new();
    for title in ["alpha", "beta", "gamma"] {
        let id = daemon.sh_titled(title, "sleep 300").id;
        let rep = Reporter::new(id, "omp-extension", "omp");
        rep.report(&daemon, Node::main("root").name("omp").idle());
        reporters.push(rep);
        ids.push(id);
    }
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the three agents", move |cx| listed(&p, cx).len() == 3);
    let beta = ids[1];
    page.update(cx, |p, cx| p.select(beta, cx));
    assert_eq!(page.read_with(cx, |p, _| p.selected_session()), Some(beta));
    // The other two start working: they move above the selected row; the selection stays.
    for rep in [&reporters[0], &reporters[2]] {
        rep.report(&daemon, Node::main("root").name("omp").working());
    }
    let p = page.clone();
    wait_until(cx, "the groups to change", move |cx| {
        p.read_with(cx, |p, _| p.listed_groups().contains(&Group::Working))
    });
    for _ in 0..10 {
        draw(cx);
    }
    assert_eq!(
        page.read_with(cx, |p, _| p.selected_session()),
        Some(beta),
        "the selection follows the session, not the row index"
    );
}

#[gpui::test]
fn a_page_that_is_not_on_screen_does_no_work_when_the_store_changes(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = agent(&daemon, "work", true);
    let (view, cx) = open(cx, &daemon);
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the agent", move |cx| listed(&p, cx).contains(&id));
    // Another tab takes the center: the page is hidden.
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    for _ in 0..5 {
        draw(cx);
    }
    assert!(!page.read_with(cx, |p, _| p.is_visible()));
    let (renders, builds) = page.read_with(cx, |p, _| (p.renders(), p.listing_builds()));
    // The store changes: more agents report.
    let other = agent(&daemon, "another", true);
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    wait_until(cx, "the store to know it", move |cx| {
        sessions.read_with(cx, |s, _| s.agent_presentation().get(other).is_some())
    });
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(100));
        draw(cx);
    }
    assert_eq!(
        page.read_with(cx, |p, _| (p.renders(), p.listing_builds())),
        (renders, builds),
        "a hidden page renders nothing and builds nothing"
    );
    // Shown again, it has the new agent.
    click(&view, cx, Region::Nav("agents"));
    let p = page.clone();
    wait_until(cx, "the page to catch up", move |cx| {
        listed(&p, cx).contains(&other)
    });
}

/// A chat that waits for a permission: one pending approval at the daemon, shown on the page.
#[gpui::test]
fn a_session_waiting_for_a_request_says_approval_and_never_working(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| wb.open_session(info.id, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(info.id)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
        })
    });
    cx.simulate_input("Run `touch hi.txt` in a shell, then reply with the word done.");
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the approval", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len() == 1)
    });
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the chat under Requests", move |cx| {
        p.read_with(cx, |p, _| p.listed_groups().contains(&Group::Requests))
    });
    // One state word for the record, and it is the attention, not the work underneath.
    let record = view
        .read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .agent_presentation()
                .get(info.id)
                .cloned()
        })
        .expect("the record");
    assert_eq!(agents_text::state_word(&record), "Approval");
    let (glyph, tone) = agents_text::lead(&record);
    assert_eq!(glyph.id(), "state.approval");
    assert_eq!(tone, hutan_desktop::sessions::Tone::Warning);
    let detail = agents_text::reason_detail(&record).expect("the reason");
    assert!(!detail.starts_with("Permission:"), "{detail}");
    assert!(
        !page
            .read_with(cx, |p, _| p.listed_groups())
            .contains(&Group::Working),
        "no Working group beside Requests for the same session"
    );
    assert!(region(&view, cx, Region::AgentRow(info.id)).is_some());
    // The panel's head says the same word.
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "agents".into(),
    ));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<AgentsPanel>("agents"))
        .expect("the panel");
    let sections = panel.read_with(cx, |p, cx| p.sections(0, cx));
    assert_eq!(sections[0].word, "Approval");
    assert_eq!(sections[0].detail.as_deref(), Some(detail.as_str()));
}

/// In a 1920 pt window the page is wider than 640 pt; in a 1000 × 700 pt window with the right
/// panel open at 480 pt it is narrower.
#[gpui::test]
fn the_toolbar_folds_into_the_more_menu_below_640_pt_and_nothing_overflows(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = agent(&daemon, "work", true);
    let (view, cx) = open(cx, &daemon);
    // What is proved is the page in a narrow center, with the panel pushed in beside it (EX-2:
    // a peek would leave the center alone).
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("layout.rail_peek", toml::Value::Boolean(false))
        .unwrap();
    click(&view, cx, Region::Nav("agents"));
    let page = page(&view, cx);
    let p = page.clone();
    wait_until(cx, "the agent", move |cx| listed(&p, cx).contains(&id));
    let labels = |cx: &mut VisualTestContext| -> Vec<String> {
        view.read_with(cx, |wb, _| wb.context_menu_entries())
            .expect("a menu is open")
            .into_iter()
            .map(|(label, _)| label.to_string())
            .collect()
    };
    let close_menu = |cx: &mut VisualTestContext| {
        view.update_in(cx, |wb, window, cx| {
            wb.run_menu_entry(usize::MAX, window, cx)
        });
    };

    // A wide window leaves the page room: scope, search and ⋯ side by side.
    let p = page.clone();
    wait_until(cx, "the unfolded toolbar", move |cx| {
        !p.read_with(cx, |p, _| p.folded())
    });
    for what in ["scope", "search", "more"] {
        assert!(
            region(&view, cx, Region::AgentsPage(what)).is_some(),
            "{what} is on the toolbar of a wide page"
        );
    }
    click(&view, cx, Region::AgentsPage("more"));
    assert_eq!(labels(cx), ["Group by workspace", "Show finished"]);
    close_menu(cx);

    // The 1000 × 700 pt window with the right panel open leaves it under 640 pt: the scope folds
    // into ⋯, the search stays.
    cx.simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(700.0)));
    cx.dispatch_action(hutan_desktop::workbench::actions::OpenPanel(
        "agents".into(),
    ));
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| s.resize_dock(DockPosition::Right, 480.0));
    });
    let p = page.clone();
    wait_until(cx, "the folded toolbar", move |cx| {
        p.read_with(cx, |p, _| p.folded())
    });
    draw(cx);
    assert!(region(&view, cx, Region::AgentsPage("scope")).is_none());
    let whole = region(&view, cx, Region::AgentsPage("page")).expect("the page");
    for what in ["search", "more"] {
        let at = region(&view, cx, Region::AgentsPage(what))
            .unwrap_or_else(|| panic!("{what} stays on the folded toolbar"));
        assert!(
            at.origin.x >= whole.origin.x && at.right() <= whole.right(),
            "{what} {at:?} sticks out of the page {whole:?}"
        );
    }
    let search = region(&view, cx, Region::AgentsPage("search")).unwrap();
    assert!(search.size.width >= gpui::px(96.0), "{search:?}");
    click(&view, cx, Region::AgentsPage("more"));
    let entries = labels(cx);
    assert_eq!(
        entries.first().map(String::as_str),
        Some("Scope: All workspaces")
    );
    assert!(
        entries.iter().any(|l| l == "Group by workspace"),
        "{entries:?}"
    );
    assert!(entries.iter().any(|l| l == "Show finished"), "{entries:?}");
    // Choosing a scope from ⋯ works like the button did.
    let workspace = entries
        .iter()
        .position(|l| l.starts_with("Scope: ") && l != "Scope: All workspaces")
        .expect("a workspace scope");
    view.update_in(cx, |wb, window, cx| {
        wb.run_menu_entry(workspace, window, cx)
    });
    assert_ne!(
        page.read_with(cx, |p, _| p.scope()),
        hutan_desktop::sessions::Scope::All
    );
}
