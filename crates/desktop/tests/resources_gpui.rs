#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The resources panel (M13-D) against an in-process daemon with real shells in real PTYs:
//! it subscribes while it is drawn and not otherwise, draws the process tree of a session,
//! signals and kills through the daemon (asking first where it must), sorts, filters, marks the
//! agents that reported a pid, greys out what the harness cannot do, and "Show in Resources"
//! reveals a session. Processes are only ever signalled when the test started them.

mod common;

use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_core::{NodeKind, SessionId};
use hutan_desktop::settings::{ResourcesSettings, ResourcesSort};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::{DockPosition, LayoutPreset, PanelDest};
use hutan_desktop::workbench::panels::{
    RESOURCES_WATCH, ResourceCommand, ResourceInterrupt, ResourceLine, ResourceTarget,
    ResourcesLink, ResourcesPanel,
};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request, SessionRef};
use hutan_protocol::resources::{
    ProcessState, ResourcesSubscribe, ResourcesSubscribed, SignalName,
};
use hutan_protocol::tree::NodeReport;
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let store = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    // The shortest interval the daemon allows keeps the tests quick.
    store
        .write_user("resources.interval_ms", toml::Value::Integer(1000))
        .unwrap();
    let (view, cx) = open_with(cx, store, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// Reveals the panel in a bottom dock tall enough to hold the rows of a test. (It is a tab of
/// the right sidebar by default; the rows have room to spread out in a wide dock.)
fn show(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<ResourcesPanel> {
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(&"resources".into(), PanelDest::Dock(DockPosition::Bottom))
                .unwrap();
            s.docks.bottom.size = 700.0;
        });
    });
    cx.dispatch_action(actions::ShowResources);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<ResourcesPanel>("resources"))
        .expect("the resources panel is drawn in the bottom dock")
}

fn lines(panel: &Entity<ResourcesPanel>, cx: &VisualTestContext) -> Vec<ResourceLine> {
    panel.read_with(cx, |p, cx| p.lines(cx))
}

fn wait_lines(
    panel: &Entity<ResourcesPanel>,
    cx: &mut VisualTestContext,
    what: &str,
    ready: impl Fn(&[ResourceLine]) -> bool,
) -> Vec<ResourceLine> {
    let mut found = Vec::new();
    wait_until(cx, what, |cx| {
        found = lines(panel, cx);
        ready(&found)
    });
    found
}

fn session_line(lines: &[ResourceLine], id: SessionId) -> Option<usize> {
    lines
        .iter()
        .position(|l| matches!(l, ResourceLine::Session(s) if s.session == id))
}

/// `(index, pid)` of the processes of `session` named `name`.
fn processes(lines: &[ResourceLine], session: SessionId, name: &str) -> Vec<(usize, u32)> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| match l {
            ResourceLine::Process { session: s, line } if *s == session && line.name == name => {
                Some((i, line.pid))
            }
            _ => None,
        })
        .collect()
}

fn sessions_of(lines: &[ResourceLine]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| match l {
            ResourceLine::Session(s) => Some(s.title.clone()),
            _ => None,
        })
        .collect()
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

fn menu(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("a menu is open")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect()
}

/// Runs the entry of the open menu whose label starts with `label`.
fn run_entry(view: &Entity<Workbench>, cx: &mut VisualTestContext, label: &str) {
    let entries = menu(view, cx);
    let index = entries
        .iter()
        .position(|e| e.starts_with(label))
        .unwrap_or_else(|| panic!("no entry {label} in {entries:?}"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(index, window, cx));
}

fn confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    assert!(view.read_with(cx, |wb, _| wb.modal_open()), "a question");
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
}

fn modal_text(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| {
        let modal = wb.modal().expect("a question is open");
        format!("{} {}", modal.title, modal.message)
    })
}

fn alive(pid: u32) -> bool {
    Process::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn kill_session(daemon: &TestDaemon, id: SessionId) {
    let _: serde_json::Value = daemon.call(Request::SessionKill(SessionRef { session_id: id }));
}

fn live(daemon: &TestDaemon, id: SessionId) -> bool {
    daemon.session(id).state.is_live()
}

/// Whether the daemon has a sampler running with a baseline: a probe subscription (on a
/// connection of its own, gone again at once) gets CPU figures for a busy process only when
/// somebody sampled before.
fn daemon_is_sampling(daemon: &TestDaemon) -> bool {
    let subscribed: ResourcesSubscribed =
        daemon.call(Request::ResourcesSubscribe(ResourcesSubscribe {
            interval_ms: None,
        }));
    subscribed
        .sample
        .sessions
        .iter()
        .flat_map(|s| &s.processes)
        .any(|p| p.cpu_percent.is_some())
}

fn pause(cx: &mut VisualTestContext, millis: u64) {
    let until = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < until {
        draw(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Lets the panel's watcher look a few times, as it does while time passes in the app.
fn let_time_pass(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.executor().advance_clock(RESOURCES_WATCH);
        draw(cx);
    }
}

fn node(id: &str, kind: NodeKind) -> NodeReport {
    NodeReport {
        id: id.to_owned(),
        parent: None,
        kind,
        name: Some(id.to_owned()),
        agent_type: None,
        outcome: None,
        model: None,
        activity: None,
        usage: None,
        transcript_path: None,
    }
}

/// An agent report with an optional pid (what `hutan agent report --pid` sends).
fn report(
    daemon: &TestDaemon,
    session: SessionId,
    seq: u64,
    node: Option<NodeReport>,
    pid: Option<u32>,
) {
    let applied: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session,
        source: "test-agent".into(),
        agent: "fake".into(),
        state: Some(AgentState::Working),
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node,
        pid,
    }));
    assert!(applied.applied, "the daemon ignored a report");
}

const TREE: &str = "sleep 60 & sleep 60 & wait";

// ---- registration -----------------------------------------------------------------------------

#[gpui::test]
fn the_panel_is_a_tab_of_the_right_sidebar_and_has_commands_and_a_key(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let spec = view
        .read_with(cx, |wb, _| {
            wb.panel_registry().get(&"resources".into()).cloned()
        })
        .expect("the resources panel is registered");
    assert_eq!(spec.default_dock, DockPosition::Right);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.right.contains(&"resources".into()));
    assert_eq!(
        state.docks.right.active,
        Some("history".into()),
        "a tab of the strip until asked for"
    );
    assert!(state.docks.bottom.panels.is_empty() && !state.docks.bottom.visible);
    for preset in LayoutPreset::builtin() {
        assert!(
            preset.docks.right.contains(&"resources".into()),
            "{}",
            preset.name
        );
    }

    // ⌘K "Resources: Show Panel" reveals it.
    cx.simulate_keystrokes("cmd-shift-p");
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    for wanted in ["Resources: Show Panel", "Resources: Toggle Panel"] {
        assert!(titles.iter().any(|t| t.as_ref() == wanted), "{wanted}");
    }
    cx.simulate_input("resources: show");
    cx.simulate_keystrokes("enter");
    draw(cx);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.right.visible);
    assert_eq!(state.docks.right.active, Some("resources".into()));

    // ⌥⌘R hides it again (the sidebar it is the page of), and shows it once more.
    cx.simulate_keystrokes("cmd-alt-r");
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.state().docks.right.visible));
    cx.simulate_keystrokes("cmd-alt-r");
    draw(cx);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.right.visible);
    assert_eq!(state.docks.right.active, Some("resources".into()));
}

// ---- subscription -----------------------------------------------------------------------------

#[gpui::test]
fn the_panel_subscribes_only_while_it_is_visible(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // A busy process: a CPU figure is easy to tell from none at all.
    let busy = daemon.sh_titled("busy", "yes > /dev/null & wait");
    let (view, cx) = open(cx, &daemon);
    assert!(
        view.read_with(cx, |wb, _| wb.panel::<ResourcesPanel>("resources"))
            .is_none(),
        "not built before it is shown"
    );
    // Nobody samples yet.
    assert!(!daemon_is_sampling(&daemon));
    pause(cx, 500);

    let panel = show(&view, cx);
    wait_until(cx, "the first sample", |cx| {
        panel.read_with(cx, |p, _| p.samples() > 0)
    });
    assert!(panel.read_with(cx, |p, _| p.subscribed()));
    assert_eq!(
        panel.read_with(cx, |p, _| p.subscribed_interval_ms()),
        Some(1000)
    );
    wait_until(cx, "the daemon to sample for the panel", |_| {
        std::thread::sleep(Duration::from_millis(300));
        daemon_is_sampling(&daemon)
    });

    // Hidden: the panel lets go, and the daemon stops (its baseline is gone).
    cx.dispatch_action(actions::ToggleResources);
    draw(cx);
    assert!(!view.read_with(cx, |wb, _| wb.state().docks.bottom.visible));
    let_time_pass(cx);
    assert!(
        !panel.read_with(cx, |p, _| p.subscribed()),
        "unsubscribed once hidden"
    );
    wait_until(cx, "the daemon to stop sampling", |_| {
        std::thread::sleep(Duration::from_millis(300));
        !daemon_is_sampling(&daemon)
    });
    pause(cx, 500);

    // Shown again: it subscribes again, with fresh data.
    cx.dispatch_action(actions::ToggleResources);
    draw(cx);
    wait_until(cx, "the panel to subscribe again", |cx| {
        panel.read_with(cx, |p, _| p.subscribed() && p.samples() > 0)
    });
    assert_eq!(panel.read_with(cx, |p, cx| p.link(cx)), ResourcesLink::Live);
    kill_session(&daemon, busy.id);
}

// ---- the tree ---------------------------------------------------------------------------------

#[gpui::test]
fn a_session_shows_its_process_tree_and_a_child_can_be_killed(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("tree", TREE);
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);

    let all = wait_lines(&panel, cx, "the tree of the session", |lines| {
        session_line(lines, tree.id).is_some()
            && processes(lines, tree.id, "sleep").len() == 2
            && lines.iter().any(|l| {
                matches!(l, ResourceLine::Process { session, line } if *session == tree.id && line.depth == 0)
            })
    });
    // The daemon first, then the session, the shell, its two sleeps one level down.
    assert!(matches!(&all[0], ResourceLine::Daemon(_)));
    let at = session_line(&all, tree.id).unwrap();
    let ResourceLine::Session(session) = &all[at] else {
        unreachable!()
    };
    assert_eq!(session.totals.processes, 3, "{session:?}");
    let shell = all
        .iter()
        .find_map(|l| match l {
            ResourceLine::Process { session, line } if *session == tree.id && line.depth == 0 => {
                Some(line)
            }
            _ => None,
        })
        .expect("the shell is the root of the tree");
    assert!(shell.command.contains("sleep 60"), "{}", shell.command);
    let sleepers = processes(&all, tree.id, "sleep");
    for (index, _) in &sleepers {
        let ResourceLine::Process { line, .. } = &all[*index] else {
            unreachable!()
        };
        assert_eq!(line.depth, 1);
        assert!(line.rss > 0 && line.threads >= 1);
    }

    // The menu of a sleep: the signals; Kill asks first, and only that sleep dies.
    let (victim_index, victim) = sleepers[0];
    let (_, other) = sleepers[1];
    right_click(&view, cx, Region::ResourceRow(victim_index));
    assert_eq!(
        menu(&view, cx),
        [
            "Open Session",
            "Interrupt (SIGINT)",
            "Terminate (SIGTERM)",
            "Kill (SIGKILL)…",
            "Stop (SIGSTOP)"
        ]
    );
    run_entry(&view, cx, "Kill (SIGKILL)");
    draw(cx);
    let text = modal_text(&view, cx);
    assert!(
        text.contains("sleep") && text.contains(&victim.to_string()),
        "{text}"
    );
    // Cancelling changes nothing.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pause(cx, 400);
    assert!(alive(victim));

    right_click(&view, cx, Region::ResourceRow(victim_index));
    run_entry(&view, cx, "Kill (SIGKILL)");
    confirm(&view, cx);
    wait_lines(&panel, cx, "the killed sleep to leave the tree", |lines| {
        let left = processes(lines, tree.id, "sleep");
        left.len() == 1 && left[0].1 == other
    });
    assert!(!alive(victim), "the sleep is gone");
    assert!(alive(other), "its sibling is not");
    assert!(live(&daemon, tree.id));
    kill_session(&daemon, tree.id);
}

#[gpui::test]
fn the_menu_button_opens_the_same_menu_and_a_signal_goes_without_asking(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("tree", TREE);
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    let all = wait_lines(&panel, cx, "two sleeps", |l| {
        processes(l, tree.id, "sleep").len() == 2
    });
    let (index, pid) = processes(&all, tree.id, "sleep")[0];

    click(&view, cx, Region::ResourceMenu(index));
    assert!(menu(&view, cx).iter().any(|e| e.starts_with("Terminate")));
    run_entry(&view, cx, "Terminate (SIGTERM)");
    assert!(
        !view.read_with(cx, |wb, _| wb.modal_open()),
        "no question for SIGTERM"
    );
    wait_lines(&panel, cx, "the terminated sleep to leave", |l| {
        processes(l, tree.id, "sleep")
            .iter()
            .all(|(_, p)| *p != pid)
    });

    // Stop, and the row says so and offers Continue.
    let rest = lines(&panel, cx);
    let (index, pid) = processes(&rest, tree.id, "sleep")[0];
    right_click(&view, cx, Region::ResourceRow(index));
    run_entry(&view, cx, "Stop (SIGSTOP)");
    let stopped = |l: &[ResourceLine], want: bool| {
        l.iter().any(|line| {
            matches!(line, ResourceLine::Process { line, .. }
                if line.pid == pid && (line.state == ProcessState::Stopped) == want)
        })
    };
    let l = wait_lines(&panel, cx, "the sleep to be stopped", |l| stopped(l, true));
    let at = processes(&l, tree.id, "sleep")
        .into_iter()
        .find(|(_, p)| *p == pid)
        .unwrap()
        .0;
    right_click(&view, cx, Region::ResourceRow(at));
    run_entry(&view, cx, "Continue (SIGCONT)");
    wait_lines(&panel, cx, "the sleep to run again", |l| stopped(l, false));
    kill_session(&daemon, tree.id);
}

#[gpui::test]
fn kill_session_names_what_dies_asks_and_ends_it(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("tree", TREE);
    let other = daemon.sh_titled("bystander", "sleep 60");
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    let all = wait_lines(&panel, cx, "both sessions", |l| {
        processes(l, tree.id, "sleep").len() == 2 && session_line(l, other.id).is_some()
    });
    let pids: Vec<u32> = processes(&all, tree.id, "sleep")
        .iter()
        .map(|(_, p)| *p)
        .collect();

    let at = session_line(&all, tree.id).unwrap();
    right_click(&view, cx, Region::ResourceRow(at));
    assert_eq!(
        menu(&view, cx),
        ["Open Session", "Stop Session…", "Kill Session…"]
    );
    run_entry(&view, cx, "Kill Session");
    draw(cx);
    let text = modal_text(&view, cx);
    assert!(text.contains("“tree”"), "{text}");
    assert!(text.contains("3 processes"), "{text}");
    assert!(
        text.contains(&format!("sleep ({})", pids[0]))
            || text.contains(&format!("sleep ({})", pids[1])),
        "{text}"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pause(cx, 300);
    assert!(live(&daemon, tree.id), "asking does not kill");

    right_click(&view, cx, Region::ResourceRow(at));
    run_entry(&view, cx, "Kill Session");
    confirm(&view, cx);
    wait_lines(&panel, cx, "the session to leave the list", |l| {
        session_line(l, tree.id).is_none()
    });
    assert!(!live(&daemon, tree.id));
    for pid in pids {
        assert!(!alive(pid), "{pid} was killed with its session");
    }
    assert!(live(&daemon, other.id), "another session is untouched");
    kill_session(&daemon, other.id);
}

// ---- sort and filter --------------------------------------------------------------------------

#[gpui::test]
fn sessions_sort_by_cpu_memory_and_name_and_the_filter_narrows_them(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    // The busy session is named so that A to Z puts it last.
    let busy = daemon.sh_titled("zeta", "yes > /dev/null & wait");
    let idle = daemon.sh_titled("alpha", "sleep 60");
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);

    wait_lines(&panel, cx, "a CPU figure for yes", |l| {
        l.iter().any(|line| {
            matches!(line, ResourceLine::Process { line, .. }
                if line.name == "yes" && line.cpu.is_some_and(|c| c > 5.0))
        }) && session_line(l, idle.id).is_some()
    });
    assert_eq!(
        sessions_of(&lines(&panel, cx)),
        ["zeta", "alpha"],
        "busiest first"
    );
    assert_eq!(
        panel.read_with(cx, |p, _| p.sort()),
        (ResourcesSort::Cpu, false)
    );

    click(&view, cx, Region::ResourceControl("sort-name"));
    draw(cx);
    assert_eq!(sessions_of(&lines(&panel, cx)), ["alpha", "zeta"]);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_store()
            .get::<ResourcesSettings>()
            .sort),
        ResourcesSort::Name,
        "the choice is saved as `resources.sort`"
    );
    // A second click on the active column flips it.
    click(&view, cx, Region::ResourceControl("sort-name"));
    draw(cx);
    assert_eq!(sessions_of(&lines(&panel, cx)), ["zeta", "alpha"]);
    assert_eq!(
        panel.read_with(cx, |p, _| p.sort()),
        (ResourcesSort::Name, true)
    );
    click(&view, cx, Region::ResourceControl("sort-memory"));
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.sort()),
        (ResourcesSort::Memory, false)
    );
    click(&view, cx, Region::ResourceControl("sort-cpu"));
    draw(cx);
    assert_eq!(sessions_of(&lines(&panel, cx)), ["zeta", "alpha"]);

    // Typing in the filter box: by process name, then by session title and by command line.
    click(&view, cx, Region::ResourceControl("filter"));
    pause(cx, 100);
    cx.simulate_input("yes");
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.filter().to_owned()), "yes");
    let l = lines(&panel, cx);
    assert_eq!(sessions_of(&l), ["zeta"]);
    assert_eq!(processes(&l, busy.id, "yes").len(), 1);
    assert!(
        !l.iter().any(|line| matches!(line, ResourceLine::Daemon(_))),
        "the daemon row is filtered too"
    );
    panel.update(cx, |p, cx| p.set_filter("ALPHA", cx));
    assert_eq!(sessions_of(&lines(&panel, cx)), ["alpha"]);
    panel.update(cx, |p, cx| p.set_filter("sleep 60", cx));
    assert_eq!(
        sessions_of(&lines(&panel, cx)),
        ["alpha"],
        "a command line matches"
    );
    panel.update(cx, |p, cx| p.set_filter("no such thing", cx));
    assert!(sessions_of(&lines(&panel, cx)).is_empty());
    draw(cx);
    panel.update(cx, |p, cx| p.set_filter("", cx));
    assert_eq!(sessions_of(&lines(&panel, cx)).len(), 2);
    kill_session(&daemon, busy.id);
    kill_session(&daemon, idle.id);
}

// ---- agents -----------------------------------------------------------------------------------

#[gpui::test]
fn agents_mark_their_process_and_a_subagent_without_one_cannot_be_interrupted(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("agents", TREE);
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    let all = wait_lines(&panel, cx, "two sleeps", |l| {
        processes(l, tree.id, "sleep").len() == 2
    });
    let (_, worker_pid) = processes(&all, tree.id, "sleep")[0];

    // One subagent is a process of the session; one has none (a thread, say).
    report(
        &daemon,
        tree.id,
        1,
        Some(node("worker", NodeKind::Sub)),
        Some(worker_pid),
    );
    report(
        &daemon,
        tree.id,
        2,
        Some(node("thread", NodeKind::Sub)),
        None,
    );
    let all = wait_lines(&panel, cx, "the agent marks", |l| {
        l.iter().any(|line| matches!(line, ResourceLine::Agent(_)))
            && l.iter().any(
                |line| matches!(line, ResourceLine::Process { line, .. } if line.agent.is_some()),
            )
    });
    let (marked_index, mark) = all
        .iter()
        .enumerate()
        .find_map(|(i, l)| match l {
            ResourceLine::Process { line, .. } if line.agent.is_some() => {
                Some((i, (line.pid, line.agent.clone().unwrap())))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(mark.0, worker_pid);
    assert_eq!(mark.1.label, "worker");
    assert_eq!(mark.1.kind, NodeKind::Sub);
    assert_eq!(mark.1.interrupt, ResourceInterrupt::Allowed { force: true });

    // The marked process offers the agent's interrupt next to the signals.
    right_click(&view, cx, Region::ResourceRow(marked_index));
    let entries = menu(&view, cx);
    assert!(
        entries.contains(&"Interrupt Agent".to_owned()),
        "{entries:?}"
    );
    assert!(
        entries.contains(&"Interrupt Agent, Then Terminate".to_owned()),
        "{entries:?}"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);

    // The agent that is no process: listed below the tree, its interrupt greyed out with why.
    let thread_index = all
        .iter()
        .position(|l| matches!(l, ResourceLine::Agent(a) if a.mark.label == "thread"))
        .unwrap();
    let ResourceLine::Agent(thread) = &all[thread_index] else {
        unreachable!()
    };
    let ResourceInterrupt::Unsupported(reason) = &thread.mark.interrupt else {
        panic!("a subagent without a process cannot be interrupted here")
    };
    right_click(&view, cx, Region::ResourceRow(thread_index));
    let entries = menu(&view, cx);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(
        entries[1].starts_with("Interrupt Agent — ") && entries[1].contains(reason.as_str()),
        "{entries:?}"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.context_menu_disabled()),
        [entries[1].clone()]
    );
    // A greyed-out entry does nothing.
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(1, window, cx));
    assert!(!view.read_with(cx, |wb, _| wb.modal_open()));

    // The daemon says the same when asked anyway: the error shows on that row.
    let target = ResourceTarget::Agent(tree.id, "thread".to_owned());
    panel.update(cx, |p, cx| {
        p.run(
            ResourceCommand::InterruptAgent {
                session: tree.id,
                node: Some("thread".to_owned()),
                force: false,
            },
            cx,
        );
    });
    wait_until(cx, "the refusal", |cx| {
        panel.read_with(cx, |p, _| p.error(&target).is_some())
    });
    let error = panel.read_with(cx, |p, _| p.error(&target).unwrap().to_owned());
    assert!(error.starts_with("invalid_state:"), "{error}");
    assert!(!panel.read_with(cx, |p, _| p.busy(&target)));
    draw(cx);
    panel.update(cx, |p, cx| p.dismiss_error(&target, cx));
    assert!(!panel.read_with(cx, |p, _| p.error(&target).is_some()));

    // Interrupting the marked subagent signals its process.
    let marked_index = lines(&panel, cx)
        .iter()
        .position(|l| matches!(l, ResourceLine::Process { line, .. } if line.pid == worker_pid))
        .unwrap();
    // (A shell's background jobs ignore SIGINT, so the force variant is what ends this one:
    // SIGINT, then SIGTERM two seconds later.)
    right_click(&view, cx, Region::ResourceRow(marked_index));
    run_entry(&view, cx, "Interrupt Agent, Then Terminate");
    wait_lines(&panel, cx, "the interrupted process to leave", |l| {
        processes(l, tree.id, "sleep")
            .iter()
            .all(|(_, p)| *p != worker_pid)
    });
    assert!(!alive(worker_pid));
    kill_session(&daemon, tree.id);
}

#[gpui::test]
fn a_refused_signal_shows_its_code_on_the_row(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("tree", "sleep 60");
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    wait_lines(&panel, cx, "the session", |l| {
        session_line(l, tree.id).is_some()
    });
    // pid 1 is never a session's process: `forbidden`.
    let target = ResourceTarget::Process(tree.id, 1);
    panel.update(cx, |p, cx| {
        p.run(
            ResourceCommand::Signal {
                session: tree.id,
                pid: 1,
                signal: SignalName::Term,
            },
            cx,
        );
    });
    wait_until(cx, "the refusal", |cx| {
        panel.read_with(cx, |p, _| p.error(&target).is_some())
    });
    let error = panel.read_with(cx, |p, _| p.error(&target).unwrap().to_owned());
    assert!(error.starts_with("forbidden:"), "{error}");
    kill_session(&daemon, tree.id);
}

// ---- Show in Resources ------------------------------------------------------------------------

#[gpui::test]
fn show_in_resources_reveals_the_session_from_its_context_menu(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let one = daemon.sh_titled("one", "sleep 60");
    let two = daemon.sh_titled("two", "sleep 60");
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "both sessions", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.session(one.id).is_some() && store.session(two.id).is_some()
        })
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().docks.right.active.clone()),
        Some("history".into())
    );

    cx.dispatch_action(actions::ShowSessionMenu(actions::SessionMenuAt {
        session: two.id.to_string(),
        position: gpui::point(gpui::px(200.0), gpui::px(200.0)),
    }));
    assert!(menu(&view, cx).iter().any(|e| e == "Show in Resources"));
    run_entry(&view, cx, "Show in Resources");
    draw(cx);
    let state = view.read_with(cx, |wb, _| wb.state().clone());
    assert!(state.docks.right.visible);
    assert_eq!(state.docks.right.active, Some("resources".into()));
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ResourcesPanel>("resources"))
        .expect("drawn");
    assert_eq!(panel.read_with(cx, |p, _| p.highlighted()), Some(two.id));
    wait_lines(&panel, cx, "the session", |l| {
        session_line(l, two.id).is_some()
    });
    kill_session(&daemon, one.id);
    kill_session(&daemon, two.id);
}

#[gpui::test]
fn nothing_running_lists_only_the_daemon_and_a_later_session_appears(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    let all = wait_lines(&panel, cx, "the first sample", |l| !l.is_empty());
    assert!(
        all.iter().all(|l| matches!(l, ResourceLine::Daemon(_))),
        "only the daemon: {all:?}"
    );
    assert_eq!(panel.read_with(cx, |p, cx| p.link(cx)), ResourcesLink::Live);
    let id = daemon.sh_titled("late", "sleep 60").id;
    wait_lines(&panel, cx, "the new session", |l| {
        session_line(l, id).is_some()
    });
    kill_session(&daemon, id);
    wait_lines(&panel, cx, "the session to leave", |l| {
        session_line(l, id).is_none()
    });
}

const PROCESS_MENU: [&str; 5] = [
    "Open Session",
    "Interrupt (SIGINT)",
    "Terminate (SIGTERM)",
    "Kill (SIGKILL)…",
    "Stop (SIGSTOP)",
];

/// The row's `⋯` and a right-click open the menu; Kill asks first and Cancel leaves the process.
fn menu_and_kill_question(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    index: usize,
    victim: u32,
    what: &str,
) {
    click(view, cx, Region::ResourceMenu(index));
    draw(cx);
    assert_eq!(menu(view, cx), PROCESS_MENU, "{what}: the ⋯ button");
    // Pressing it again, or another gesture, replaces the menu rather than eating the click.
    right_click(view, cx, Region::ResourceRow(index));
    draw(cx);
    assert_eq!(menu(view, cx), PROCESS_MENU, "{what}: right-click");
    // The menu is painted: its entries have places on screen.
    for entry in 0..PROCESS_MENU.len() {
        assert!(
            region(view, cx, Region::MenuEntry(entry)).is_some(),
            "{what}: entry {entry} is drawn"
        );
    }
    run_entry(view, cx, "Kill (SIGKILL)");
    draw(cx);
    let text = modal_text(view, cx);
    assert!(
        text.contains("sleep") && text.contains(&victim.to_string()),
        "{what}: {text}"
    );
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    pause(cx, 300);
    assert!(alive(victim), "{what}: cancelling kills nothing");
}

#[gpui::test]
fn the_row_menu_opens_from_the_button_and_right_click_in_the_dock_and_as_a_tab(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let tree = daemon.sh_titled("tree", TREE);
    let (view, cx) = open(cx, &daemon);
    let panel = show(&view, cx);
    let all = wait_lines(&panel, cx, "two sleeps", |l| {
        processes(l, tree.id, "sleep").len() == 2
    });
    let (index, victim) = processes(&all, tree.id, "sleep")[0];

    // In the bottom dock, with the filter box holding the keyboard.
    click(&view, cx, Region::ResourceControl("filter"));
    pause(cx, 100);
    menu_and_kill_question(&view, cx, index, victim, "dock, filter focused");

    // With a terminal tab holding the keyboard.
    view.update_in(cx, |wb, window, cx| wb.open_session(tree.id, window, cx));
    draw(cx);
    pause(cx, 300);
    draw(cx);
    menu_and_kill_question(&view, cx, index, victim, "dock, terminal focused");

    // With nothing holding it: the action still reaches the workbench.
    cx.update(|window, cx| window.blur(cx));
    draw(cx);
    menu_and_kill_question(&view, cx, index, victim, "dock, nothing focused");

    // As a tab in the center.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            s.move_panel(
                &hutan_desktop::workbench::model::PanelId::new("resources"),
                hutan_desktop::workbench::model::PanelDest::Center,
            )
            .unwrap();
        })
    });
    draw(cx);
    menu_and_kill_question(&view, cx, index, victim, "center tab");
    kill_session(&daemon, tree.id);
}
