#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The agent monitor (M13-B) against an in-process daemon: agents report through the real
//! `agent.report` / `agent.release` requests (root, subagents, usage, release, a daemon
//! restart), and the panel, the badges and the status bar follow the daemon's observation tree.

use crate::common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::agents::{Node, Reporter, usage_context};
use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_core::{NodeOutcome, SessionId};
use hutan_desktop::sessions::AgentAttentionChanged;
use hutan_desktop::settings::{AgentsScope, AgentsSettings};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, ItemRef, LayoutPreset};
use hutan_desktop::workbench::panels::{AGENTS_TICK, AgentsPanel, SectionView};
use hutan_desktop::workbench::{BadgeSite, Region, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
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

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Reveals the panel (it shares the right dock with the inspector).
fn show_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<AgentsPanel> {
    cx.dispatch_action(actions::OpenPanel("agents".into()));
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<AgentsPanel>("agents"))
        .expect("the agents panel is drawn in the right dock")
}

fn sections(panel: &Entity<AgentsPanel>, cx: &VisualTestContext) -> Vec<SectionView> {
    panel.read_with(cx, |p, cx| p.sections(now_ms(), cx))
}

/// `(label, glyph)` of every listed node of the first section.
fn glyphs(panel: &Entity<AgentsPanel>, cx: &VisualTestContext) -> Vec<(String, &'static str)> {
    sections(panel, cx)
        .first()
        .map(|s| {
            s.rows
                .iter()
                .map(|r| (r.label.clone(), r.glyph.id()))
                .collect()
        })
        .unwrap_or_default()
}

fn settings(view: &Entity<Workbench>, cx: &VisualTestContext) -> SettingsStore {
    view.read_with(cx, |wb, _| wb.settings_store().clone())
}

fn set(view: &Entity<Workbench>, cx: &VisualTestContext, key: &str, value: toml::Value) {
    settings(view, cx).write_user(key, value).unwrap();
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn right_click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let at = region(view, cx, what).unwrap().center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
}

/// A shell session that stays alive for the test.
fn session(daemon: &TestDaemon, title: &str) -> SessionId {
    daemon.sh_titled(title, "sleep 300").id
}

fn row_index(
    panel: &Entity<AgentsPanel>,
    cx: &VisualTestContext,
    session: SessionId,
    node: &str,
) -> usize {
    let mut index = 0;
    for section in sections(panel, cx) {
        for row in &section.rows {
            if section.session == session && row.node_id == node {
                return index;
            }
            index += 1;
        }
    }
    panic!("no row {node} of {session}");
}

fn wait_rows(panel: &Entity<AgentsPanel>, cx: &mut VisualTestContext, what: &str, rows: usize) {
    let panel = panel.clone();
    wait_until(cx, what, move |cx| {
        sections(&panel, cx)
            .first()
            .is_some_and(|s| s.rows.len() == rows)
    });
}

// ---- the panel -----------------------------------------------------------------------------

#[gpui::test]
fn the_panel_is_registered_in_the_right_dock_of_the_default_and_overview_presets(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let spec = view.read_with(cx, |wb, _| {
        wb.panel_registry().get(&"agents".into()).cloned()
    });
    let spec = spec.expect("the agents panel is registered");
    assert_eq!(spec.default_dock, DockPosition::Right);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.right.contains(&"agents".into()));
    for preset in [
        LayoutPreset::default_preset(),
        LayoutPreset::overview_preset(),
    ] {
        assert!(
            preset.docks.right.contains(&"agents".into()),
            "{}",
            preset.name
        );
    }
    // Movable like every panel: to the bottom dock.
    cx.dispatch_action(actions::OpenPanel("agents".into()));
    cx.dispatch_action(actions::MoveActivePanel(
        hutan_desktop::workbench::model::PanelDest::Dock(DockPosition::Bottom),
    ));
    draw(cx);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.bottom.contains(&"agents".into()));
    assert!(!state.docks.right.contains(&"agents".into()));
}

#[gpui::test]
fn the_tree_shows_the_right_glyph_per_state_and_outcome_in_a_stable_order(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    set(
        &view,
        cx,
        "agents.show_finished",
        toml::Value::Boolean(true),
    );
    let panel = show_panel(&view, cx);
    assert!(
        sections(&panel, cx).is_empty(),
        "a session without agents has no section"
    );

    let rep = Reporter::new(id, "omp-extension", "omp");
    rep.report(
        &daemon,
        Node::main("root")
            .working()
            .name("omp")
            .model("gpt-6")
            .activity("planning the change"),
    );
    rep.report(
        &daemon,
        Node::sub("n1", "root")
            .working()
            .name("scout")
            .agent_type("Explore")
            .model("haiku")
            .activity("reading   src/lib.rs\nline two")
            .usage(usage_context(1_500)),
    );
    rep.report(&daemon, Node::sub("n2", "root").blocked().name("writer"));
    rep.report(&daemon, Node::sub("n3", "root").idle().name("waiter"));
    for (id, name, outcome) in [
        ("n4", "done", NodeOutcome::Completed),
        ("n5", "broke", NodeOutcome::Failed),
        ("n6", "stopped", NodeOutcome::Interrupted),
        ("n7", "lost", NodeOutcome::Disconnected),
    ] {
        rep.report(&daemon, Node::sub(id, "root").name(name).finished(outcome));
    }
    wait_rows(&panel, cx, "the whole tree", 8);

    let listed = glyphs(&panel, cx);
    let expected: Vec<(String, &str)> = [
        ("omp", "state.working"),
        ("scout", "state.working"),
        ("writer", "state.blocked"),
        ("waiter", "state.idle"),
        ("done", "state.completed"),
        ("broke", "state.failed"),
        ("stopped", "state.interrupted"),
        ("lost", "state.disconnected"),
    ]
    .map(|(l, g)| (l.to_owned(), g))
    .to_vec();
    assert_eq!(listed, expected);

    let first = sections(&panel, cx).remove(0);
    assert_eq!(first.heading(), format!("{} › work", first.workspace));
    let root = &first.rows[0];
    assert_eq!((root.depth, root.model.as_deref()), (0, Some("gpt-6")));
    assert_eq!(root.activity.as_deref(), Some("planning the change"));
    assert!(root.animated && root.duration.is_some());
    let scout = &first.rows[1];
    assert_eq!((scout.depth, scout.meaning), (1, "working"));
    assert_eq!(scout.agent_type.as_deref(), Some("Explore"));
    assert_eq!(scout.model.as_deref(), Some("haiku"));
    assert_eq!(scout.usage.as_deref(), Some("1.5k tok · context"));
    assert_eq!(
        scout.activity.as_deref(),
        Some("reading src/lib.rs line two"),
        "one short line"
    );
    let writer = &first.rows[2];
    assert!(writer.blocked && !writer.finished);
    let done = &first.rows[4];
    assert!(done.finished && done.activity.is_none() && done.duration.is_some());
    // No counter was sent: no usage line, never zeros.
    assert_eq!(first.rows[2].usage, None);

    // More reports change the nodes, not their places.
    rep.report(
        &daemon,
        Node::sub("n3", "root").working().activity("thinking"),
    );
    rep.report(
        &daemon,
        Node::sub("n2", "root").working().activity("editing"),
    );
    let panel2 = panel.clone();
    wait_until(cx, "the updates", move |cx| {
        sections(&panel2, cx)
            .first()
            .is_some_and(|s| s.rows[3].meaning == "working" && s.rows[2].meaning == "working")
    });
    let after: Vec<String> = glyphs(&panel, cx).into_iter().map(|(l, _)| l).collect();
    let before: Vec<String> = expected.iter().map(|(l, _)| l.clone()).collect();
    assert_eq!(after, before, "ordering is stable");
}

#[gpui::test]
fn a_release_finishes_a_node_and_a_restart_leaves_the_main_agent_unknown(cx: &mut TestAppContext) {
    let mut daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    set(
        &view,
        cx,
        "agents.show_finished",
        toml::Value::Boolean(true),
    );
    let panel = show_panel(&view, cx);
    let rep = Reporter::new(id, "omp-extension", "omp");
    rep.report(
        &daemon,
        Node::main("root")
            .working()
            .name("omp")
            .resume(&["sleep", "600"]),
    );
    rep.report(&daemon, Node::sub("a", "root").working().name("alpha"));
    rep.report(&daemon, Node::sub("b", "root").working().name("beta"));
    wait_rows(&panel, cx, "three agents", 3);
    rep.release(&daemon, Some("b"), Some(NodeOutcome::Failed));
    let p = panel.clone();
    wait_until(cx, "beta to end", move |cx| {
        glyphs(&p, cx)
            .last()
            .is_some_and(|(l, g)| l == "beta" && *g == "state.failed")
    });

    // The stored tree is written within a second; wait for it before the daemon goes down.
    let database = daemon.paths.database.clone();
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let storage = hutan_storage::Storage::open(&database).unwrap();
        let stored = storage.agent_trees().list().unwrap();
        storage.close();
        if stored
            .iter()
            .any(|(s, json)| *s == id && json.contains("beta"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the tree was never stored"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon.stop();
    daemon.restart();
    rep.restart_numbering();

    let p = panel.clone();
    wait_until(cx, "the restored tree", move |cx| {
        glyphs(&p, cx)
            == [
                ("omp".to_owned(), "state.unknown"),
                ("alpha".to_owned(), "state.disconnected"),
                ("beta".to_owned(), "state.failed"),
            ]
    });
    // The resumed agent reports again and is alive.
    rep.report(&daemon, Node::main("root").idle().name("omp"));
    let p = panel.clone();
    wait_until(cx, "the main agent to report again", move |cx| {
        glyphs(&p, cx)
            .first()
            .is_some_and(|(_, g)| *g == "state.idle")
    });
}

#[gpui::test]
fn finished_subtrees_dim_and_collapse_until_unfolded(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    let rep = Reporter::new(id, "omp-extension", "omp");
    rep.report(&daemon, Node::main("root").working().name("omp"));
    rep.report(
        &daemon,
        Node::sub("n2", "root")
            .name("planner")
            .finished(NodeOutcome::Completed),
    );
    rep.report(
        &daemon,
        Node::sub("n3", "n2")
            .name("reader")
            .finished(NodeOutcome::Completed),
    );
    rep.report(&daemon, Node::sub("n4", "root").working().name("coder"));
    // Finished agents are off by default: only the main agent and the live one show.
    wait_rows(&panel, cx, "the live agents", 2);
    assert_eq!(
        glyphs(&panel, cx),
        [
            ("omp".to_owned(), "state.working"),
            ("coder".to_owned(), "state.working")
        ]
    );

    set(
        &view,
        cx,
        "agents.show_finished",
        toml::Value::Boolean(true),
    );
    draw(cx);
    wait_rows(&panel, cx, "the finished subtree folded", 3);
    let section = sections(&panel, cx).remove(0);
    let mid = &section.rows[1];
    assert_eq!(mid.label, "planner");
    assert!(mid.finished && mid.foldable && mid.collapsed);
    assert_eq!(mid.hidden, 1, "its child is folded into it");

    let index = row_index(&panel, cx, id, "n2");
    click(&view, cx, Region::MonitorFold(index));
    wait_rows(&panel, cx, "the subtree unfolded", 4);
    let labels: Vec<String> = glyphs(&panel, cx).into_iter().map(|(l, _)| l).collect();
    assert_eq!(labels, ["omp", "planner", "reader", "coder"]);
    let reader = &sections(&panel, cx)[0].rows[2];
    assert_eq!(reader.depth, 2);
    // A second click folds it again.
    click(&view, cx, Region::MonitorFold(index));
    wait_rows(&panel, cx, "the subtree folded again", 3);
}

// ---- filters -------------------------------------------------------------------------------

#[gpui::test]
fn the_chips_filter_the_tree_and_write_the_settings(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let here = session(&daemon, "here");
    let elsewhere_dir = tempfile::tempdir().unwrap();
    let other = daemon.create_workspace("elsewhere", &[elsewhere_dir.path()]);
    let there = daemon
        .create_session(&["sh", "-c", "sleep 300"], Some(other.id))
        .id;
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);

    let rep = Reporter::new(here, "omp-extension", "omp");
    rep.report(&daemon, Node::main("root").working().name("omp"));
    rep.report(&daemon, Node::sub("a", "root").working().name("alpha"));
    rep.report(&daemon, Node::sub("b", "root").blocked().name("beta"));
    rep.report(
        &daemon,
        Node::sub("c", "root")
            .name("gamma")
            .finished(NodeOutcome::Completed),
    );
    let far = Reporter::new(there, "omp-extension", "omp");
    far.report(&daemon, Node::main("root").working().name("omp"));
    wait_rows(&panel, cx, "the live agents of this workspace", 3);

    // Defaults: finished off, blocked-only off, this workspace.
    let current = settings(&view, cx).get::<AgentsSettings>();
    assert_eq!(*current, AgentsSettings::default());
    assert_eq!(current.scope, AgentsScope::ActiveWorkspace);
    assert_eq!(
        glyphs(&panel, cx),
        [
            ("omp".to_owned(), "state.working"),
            ("alpha".to_owned(), "state.working"),
            ("beta".to_owned(), "state.blocked"),
        ]
    );
    assert_eq!(
        sections(&panel, cx).len(),
        1,
        "the other workspace is out of scope"
    );

    // Finished: the finished agent is listed, dimmed.
    click(&view, cx, Region::MonitorChip("finished"));
    wait_rows(&panel, cx, "finished agents", 4);
    assert!(settings(&view, cx).get::<AgentsSettings>().show_finished);
    assert_eq!(
        glyphs(&panel, cx).last().unwrap(),
        &("gamma".to_owned(), "state.completed")
    );
    click(&view, cx, Region::MonitorChip("finished"));
    wait_rows(&panel, cx, "finished agents hidden again", 3);
    assert!(!settings(&view, cx).get::<AgentsSettings>().show_finished);

    // Blocked only: the blocked agent and the main agent above it.
    click(&view, cx, Region::MonitorChip("blocked"));
    wait_rows(&panel, cx, "blocked agents only", 2);
    assert_eq!(
        glyphs(&panel, cx),
        [
            ("omp".to_owned(), "state.working"),
            ("beta".to_owned(), "state.blocked")
        ]
    );
    assert!(settings(&view, cx).get::<AgentsSettings>().only_blocked);
    // The other session has nothing blocked, so even with every workspace it has no section.
    // This workspace → the focused session → every workspace.
    click(&view, cx, Region::MonitorChip("scope"));
    wait_until(cx, "the focused session", |cx| {
        settings(&view, cx).get::<AgentsSettings>().scope == AgentsScope::Session
    });
    click(&view, cx, Region::MonitorChip("scope"));
    wait_until(cx, "all workspaces", |cx| {
        settings(&view, cx).get::<AgentsSettings>().scope == AgentsScope::All
    });
    assert_eq!(sections(&panel, cx).len(), 1);
    click(&view, cx, Region::MonitorChip("blocked"));
    let p = panel.clone();
    wait_until(cx, "both sections", move |cx| sections(&p, cx).len() == 2);
    let all = sections(&panel, cx);
    assert!(all.iter().any(|s| s.title == "here"));
    assert!(all.iter().any(|s| s.workspace == "elsewhere"));

    // Back to this workspace.
    click(&view, cx, Region::MonitorChip("scope"));
    wait_until(cx, "this workspace", |cx| {
        settings(&view, cx).get::<AgentsSettings>().scope == AgentsScope::ActiveWorkspace
    });
    let p = panel.clone();
    wait_until(cx, "this workspace again", move |cx| {
        sections(&p, cx).len() == 1
    });
    assert_eq!(
        settings(&view, cx).get::<AgentsSettings>().scope,
        AgentsScope::ActiveWorkspace
    );
}

// ---- clicks and the node menu ----------------------------------------------------------------

#[gpui::test]
fn a_click_focuses_the_session_of_the_node(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let first = session(&daemon, "first");
    let second = session(&daemon, "second");
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    for (id, name) in [(first, "one"), (second, "two")] {
        let rep = Reporter::new(id, "omp-extension", "omp");
        rep.report(&daemon, Node::main("root").working().name("omp"));
        rep.report(&daemon, Node::sub("s", "root").working().name(name));
    }
    let p = panel.clone();
    wait_until(cx, "both trees", move |cx| {
        let all = sections(&p, cx);
        all.len() == 2 && all.iter().all(|s| s.rows.len() == 2)
    });
    view.update_in(cx, |wb, window, cx| wb.open_session(first, window, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(first)
    );

    let index = row_index(&panel, cx, second, "s");
    click(&view, cx, Region::MonitorRow(index));
    wait_until(cx, "the second session to be focused", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(second)
    });
    // And back: the first session's main agent.
    let index = row_index(&panel, cx, first, "root");
    click(&view, cx, Region::MonitorRow(index));
    wait_until(cx, "the first session to be focused", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(first)
    });
}

#[gpui::test]
fn a_click_on_the_node_of_a_chat_focuses_its_chat_tab(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    let rep = Reporter::new(chat.id, "codex", "codex");
    rep.report(&daemon, Node::main("root").working().name("codex"));
    rep.report(&daemon, Node::sub("s", "root").working().name("helper"));
    wait_rows(&panel, cx, "the chat's tree", 2);
    let index = row_index(&panel, cx, chat.id, "s");
    click(&view, cx, Region::MonitorRow(index));
    wait_until(cx, "the chat tab", |cx| {
        view.read_with(cx, |wb, _| wb.state().active_item())
            == Some(ItemRef::Chat {
                session: chat.id.to_string(),
            })
    });
}

#[gpui::test]
fn the_node_menu_opens_the_transcript_and_copies_the_node_id(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    let revealed: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    view.update(cx, |wb, _| {
        let revealed = Rc::clone(&revealed);
        wb.set_reveal_handler(move |path, _| revealed.borrow_mut().push(path.to_owned()));
    });
    let panel = show_panel(&view, cx);
    set(
        &view,
        cx,
        "agents.show_finished",
        toml::Value::Boolean(true),
    );
    let rep = Reporter::new(id, "claude-hooks", "claude");
    rep.report(&daemon, Node::main("root").working().name("claude"));
    rep.report(
        &daemon,
        Node::sub("agent-7", "root")
            .name("Explore")
            .transcript("/tmp/hutan-test/agent-7.jsonl")
            .finished(NodeOutcome::Completed),
    );
    rep.report(&daemon, Node::sub("agent-8", "root").working().name("Plan"));
    wait_rows(&panel, cx, "three agents", 3);

    right_click(
        &view,
        cx,
        Region::MonitorRow(row_index(&panel, cx, id, "agent-7")),
    );
    let entries: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the node menu is open")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(entries, ["Open Session", "Open Transcript", "Copy Node ID"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    assert_eq!(
        *revealed.borrow(),
        [PathBuf::from("/tmp/hutan-test/agent-7.jsonl")]
    );
    right_click(
        &view,
        cx,
        Region::MonitorRow(row_index(&panel, cx, id, "agent-7")),
    );
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(2, window, cx));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("agent-7")
    );

    // A node without a transcript has no such entry; "Open Session" focuses its session.
    right_click(
        &view,
        cx,
        Region::MonitorRow(row_index(&panel, cx, id, "agent-8")),
    );
    let entries: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .unwrap()
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(entries, ["Open Session", "Copy Node ID"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    wait_until(cx, "the session to be focused", |cx| {
        view.read_with(cx, |wb, _| wb.focused_session()) == Some(id)
    });
}

// ---- badges and attention --------------------------------------------------------------------

#[gpui::test]
fn a_blocked_agent_badges_the_row_the_tab_and_the_card_and_clears_on_resolution(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    let events: Rc<RefCell<Vec<AgentAttentionChanged>>> = Rc::default();
    let _subscription = cx.update(|_, cx| {
        let events = Rc::clone(&events);
        cx.subscribe(&sessions, move |_, event: &AgentAttentionChanged, _| {
            events.borrow_mut().push(event.clone());
        })
    });
    // The session is a tab, so the tab can carry a badge too.
    view.update_in(cx, |wb, window, cx| wb.open_session(id, window, cx));
    draw(cx);
    let badge = |cx: &VisualTestContext| sessions.read_with(cx, |s, _| s.agent_badge(id));
    assert!(badge(cx).is_empty());
    for site in [BadgeSite::Row, BadgeSite::Tab] {
        assert!(region(&view, cx, Region::AgentBadge(site, id)).is_none());
    }

    let rep = Reporter::new(id, "omp-extension", "omp");
    rep.report(&daemon, Node::main("root").working().name("omp"));
    rep.report(&daemon, Node::sub("n1", "root").working().name("worker"));
    rep.report(
        &daemon,
        Node::sub("n2", "root")
            .blocked()
            .name("reviewer")
            .activity("approve rm -rf"),
    );
    wait_until(cx, "the badge", |cx| !badge(cx).blocked.is_empty());
    let b = badge(cx);
    assert_eq!(
        b.running_subagents, 2,
        "the worker and the blocked reviewer"
    );
    assert_eq!(b.running_text().as_deref(), Some("2"));
    assert_eq!(
        b.blocked_icon(),
        Some(hutan_desktop::icons::Icon::StateBlocked)
    );
    assert_eq!(b.tooltip(), "2 subagents running\nBlocked: reviewer");

    // Row and tab.
    for site in [BadgeSite::Row, BadgeSite::Tab] {
        wait_until(cx, &format!("the {site:?} badge"), |cx| {
            region(&view, cx, Region::AgentBadge(site, id)).is_some()
        });
    }

    // The status bar counts the session as needing the user, and the event fired once.
    sessions.read_with(cx, |s, _| {
        assert_eq!(s.needs_you(), [id]);
        let info = s.session(id).unwrap();
        let presentation = s.presentation(info);
        assert_eq!(presentation.label, "blocked");
        assert!(presentation.needs_you);
    });
    wait_until(cx, "the attention event", |_| !events.borrow().is_empty());
    assert_eq!(
        *events.borrow(),
        [AgentAttentionChanged {
            session_id: id,
            blocked_nodes: vec!["n2".to_owned()]
        }]
    );
    // The same node reporting again is not news.
    rep.report(
        &daemon,
        Node::sub("n2", "root")
            .blocked()
            .name("reviewer")
            .activity("still waiting"),
    );
    // A second blocked node is.
    rep.report(&daemon, Node::sub("n1", "root").blocked().name("worker"));
    wait_until(cx, "the second event", |_| events.borrow().len() == 2);
    assert_eq!(events.borrow()[1].blocked_nodes, ["n1", "n2"]);

    // Resolution: the blocked marker goes, the running count stays while they work.
    rep.report(&daemon, Node::sub("n2", "root").working().name("reviewer"));
    rep.report(&daemon, Node::sub("n1", "root").working().name("worker"));
    wait_until(cx, "the marker to clear", |cx| badge(cx).blocked.is_empty());
    assert_eq!(badge(cx).running_text().as_deref(), Some("2"));
    assert!(sessions.read_with(cx, |s, _| s.needs_you().is_empty()));
    assert_eq!(events.borrow().len(), 2, "resolving is not an event");
    // Blocking again after it cleared is news again.
    rep.report(&daemon, Node::sub("n2", "root").blocked().name("reviewer"));
    wait_until(cx, "the third event", |_| events.borrow().len() == 3);
    rep.report(&daemon, Node::sub("n2", "root").working().name("reviewer"));

    // The subagents finish: nothing is left to show anywhere.
    rep.release(&daemon, Some("n2"), None);
    rep.release(&daemon, Some("n1"), None);
    wait_until(cx, "the badge to go", |cx| badge(cx).is_empty());
    for site in [BadgeSite::Row, BadgeSite::Tab] {
        wait_until(cx, &format!("the {site:?} badge to clear"), |cx| {
            region(&view, cx, Region::AgentBadge(site, id)).is_none()
        });
    }
}

#[gpui::test]
fn a_blocked_agent_in_another_workspace_still_counts_for_the_status_bar(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let dir = tempfile::tempdir().unwrap();
    let other = daemon.create_workspace("elsewhere", &[dir.path()]);
    let there = daemon
        .create_session(&["sh", "-c", "sleep 300"], Some(other.id))
        .id;
    let (view, cx) = open(cx, &daemon);
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    let rep = Reporter::new(there, "omp-extension", "omp");
    rep.report(&daemon, Node::main("root").working().name("omp"));
    rep.report(&daemon, Node::sub("r", "root").blocked().name("reviewer"));
    wait_until(cx, "the session to need you", |cx| {
        sessions.read_with(cx, |s, _| s.needs_you() == [there])
    });
    let by_workspace = sessions.read_with(cx, |s, _| s.needs_you_by_workspace());
    assert_eq!(by_workspace.get(&other.id), Some(&1));
}

// ---- the clock -------------------------------------------------------------------------------

#[gpui::test]
fn durations_tick_once_a_second_only_while_the_panel_is_drawn_and_something_runs(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let id = session(&daemon, "work");
    let (view, cx) = open(cx, &daemon);
    let panel = show_panel(&view, cx);
    assert!(!panel.read_with(cx, |p, _| p.ticking()), "nothing runs yet");

    let rep = Reporter::new(id, "omp-extension", "omp");
    rep.report(&daemon, Node::main("root").working().name("omp"));
    rep.report(&daemon, Node::sub("a", "root").working().name("alpha"));
    wait_rows(&panel, cx, "two agents", 2);
    wait_until(cx, "the ticker to start", |cx| {
        panel.read_with(cx, |p, _| p.ticking())
    });

    let advance = |cx: &mut VisualTestContext, seconds: u64| {
        for _ in 0..seconds {
            cx.executor().advance_clock(AGENTS_TICK);
            draw(cx);
        }
    };
    let start = panel.read_with(cx, |p, _| p.ticks());
    advance(cx, 3);
    let drawn = panel.read_with(cx, |p, _| p.ticks());
    assert!(
        drawn >= start + 3,
        "one refresh per second: {start} → {drawn}"
    );

    // Hidden (the right dock is toggled away): at most the tick in flight, then nothing.
    cx.dispatch_action(actions::ToggleRightDock);
    draw(cx);
    advance(cx, 1);
    let at_hide = panel.read_with(cx, |p, _| p.ticks());
    advance(cx, 5);
    assert_eq!(
        panel.read_with(cx, |p, _| p.ticks()),
        at_hide,
        "a hidden panel does not tick"
    );
    assert!(!panel.read_with(cx, |p, _| p.ticking()));

    // Visible again: it ticks again.
    cx.dispatch_action(actions::ToggleRightDock);
    draw(cx);
    assert!(panel.read_with(cx, |p, _| p.ticking()));
    advance(cx, 2);
    assert!(panel.read_with(cx, |p, _| p.ticks()) >= at_hide + 2);

    // Nothing running (both agents end; finished agents are not listed): no ticking.
    rep.release(&daemon, None, Some(NodeOutcome::Completed));
    wait_until(cx, "the ticker to stop", |cx| {
        !panel.read_with(cx, |p, _| p.ticking())
    });
    let idle = panel.read_with(cx, |p, _| p.ticks());
    advance(cx, 3);
    assert_eq!(panel.read_with(cx, |p, _| p.ticks()), idle);
}

// ---- ⌘K ---------------------------------------------------------------------------------------

#[gpui::test]
fn the_palette_shows_the_monitor_and_only_blocked_agents(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    // Something else is the active tab of the right dock.
    cx.dispatch_action(actions::OpenPanel("inspector".into()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("inspector".into())
    );

    cx.simulate_keystrokes("cmd-shift-p");
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    for wanted in ["Agents: Show Monitor", "Agents: Show Only Blocked"] {
        assert!(
            titles.iter().any(|t| t.as_ref() == wanted),
            "{wanted} in {titles:?}"
        );
    }
    cx.simulate_input("show monitor");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("agents".into())
    );
    assert!(!settings(&view, cx).get::<AgentsSettings>().only_blocked);

    cx.dispatch_action(actions::OpenPanel("inspector".into()));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("only blocked");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(settings(&view, cx).get::<AgentsSettings>().only_blocked);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("agents".into())
    );
}
