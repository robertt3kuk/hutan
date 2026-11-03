#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Checks in the window (CHECKS-C): the rail's Checks tab, the Findings of the Context panel and
//! *Check with Mawas*, against a real in-process daemon. The findings come from what the daemon
//! sees of real sessions: two agents that reported themselves in one folder share a checkout,
//! and a folder with a `Makefile` has a test command for *Verify* to find. Nothing calls a model.

use crate::common;

use std::path::Path;

use common::assistant::{AssistantEnv, Options, open, open_assistant};
use common::{draw, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::workbench::panels::AssistantPanel;
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::checks::{AgentsFindings, AgentsFindingsRequest};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Request, SessionInfo,
};

fn started() -> AssistantEnv {
    AssistantEnv::start(Options {
        configured: true,
        ..Options::default()
    })
}

fn open_wide<'a>(
    cx: &'a mut TestAppContext,
    env: &AssistantEnv,
) -> (
    Entity<Workbench>,
    &'a mut VisualTestContext,
    Entity<AssistantPanel>,
) {
    let (view, cx) = open(cx, &env.daemon);
    cx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
    let panel = open_assistant(&view, cx);
    wait_until(cx, "the rail's tabs", |cx| {
        painted(&view, cx, "today-tab-checks")
    });
    (view, cx, panel)
}

fn painted(view: &Entity<Workbench>, cx: &VisualTestContext, name: &'static str) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(name)))
        .is_some()
}

fn finding_painted(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    name: &'static str,
    index: usize,
) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Finding(name, index)))
        .is_some()
}

fn click_tab(view: &Entity<Workbench>, cx: &mut VisualTestContext, name: &'static str) {
    draw(cx);
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Assistant(name)))
        .unwrap_or_else(|| panic!("{name} is drawn"))
        .center();
    cx.simulate_click(at, Modifiers::none());
}

fn click_finding(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    name: &'static str,
    index: usize,
) {
    draw(cx);
    let at = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Finding(name, index)))
        .unwrap_or_else(|| panic!("{name} {index} is drawn"))
        .center();
    cx.simulate_click(at, Modifiers::none());
}

/// A terminal in `dir` in which an agent reported itself in `state`.
fn agent_in(env: &AssistantEnv, title: &str, dir: &Path, state: AgentState) -> SessionInfo {
    let session = env.daemon.sh_titled_in(title, "sleep 600", dir);
    report(env, session.id, state, 1);
    session
}

fn report(env: &AssistantEnv, session: SessionId, state: AgentState, seq: u64) {
    let _: AgentApplied = env.daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session,
        source: "test-hooks".into(),
        agent: "claude".into(),
        state: Some(state),
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        pid: None,
        node: None,
        insight: None,
    }));
}

/// A project folder whose `make test` ends with `code`.
fn project(env: &AssistantEnv, name: &str, code: u8) -> std::path::PathBuf {
    let dir = env.base.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Makefile"), format!("test:\n\t@exit {code}\n")).unwrap();
    // The daemon finds two agents in one *checkout*: the folder is a git repository.
    let git = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(git.success());
    dir
}

fn daemon_findings(env: &AssistantEnv, include_dismissed: bool) -> AgentsFindings {
    env.daemon
        .call(Request::AgentsFindings(AgentsFindingsRequest {
            include_dismissed,
            ..Default::default()
        }))
}

/// Two agents in one folder: the daemon finds that they share a checkout.
fn shared_pair(env: &AssistantEnv, dir: &Path, state: AgentState) -> [SessionInfo; 2] {
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    [
        agent_in(env, &format!("{name}-a"), dir, state),
        agent_in(env, &format!("{name}-b"), dir, state),
    ]
}

/// The session a finding is about: its own, or the first agent its evidence names (a finding
/// about a checkout two agents share has no session of its own).
fn session_of(finding: &hutan_protocol::checks::Finding) -> Option<SessionId> {
    finding.scope.session_id.or_else(|| {
        finding
            .evidence
            .iter()
            .find_map(|e| e.reference.as_deref()?.parse().ok())
    })
}

fn row_of(
    panel: &Entity<AssistantPanel>,
    cx: &VisualTestContext,
    members: &[SessionInfo],
) -> Option<usize> {
    panel.read_with(cx, |p, cx| {
        p.checks_rows(cx)
            .iter()
            .position(|f| session_of(f).is_some_and(|id| members.iter().any(|m| m.id == id)))
    })
}

#[gpui::test]
fn the_checks_tab_lists_findings_filters_by_the_focus_and_keeps_a_dismissal(
    cx: &mut TestAppContext,
) {
    let env = started();
    let dir = project(&env, "shared", 0);
    let pair = shared_pair(&env, &dir, AgentState::Working);
    let elsewhere = env.daemon.create_workspace("elsewhere", &[]);
    let (view, cx, panel) = open_wide(cx, &env);

    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "a read", |cx| {
        panel.read_with(cx, |p, cx| p.checks_findings(cx).is_some())
    });
    wait_until(cx, "the findings", |cx| row_of(&panel, cx, &pair).is_some());
    let at = row_of(&panel, cx, &pair).unwrap();
    wait_until(cx, "the row", |cx| {
        finding_painted(&view, cx, "checks-row", at)
    });
    let answer = panel
        .read_with(cx, |p, cx| p.checks_findings(cx).cloned())
        .unwrap();
    let finding = answer
        .findings
        .iter()
        .find(|f| f.detector == "shared_checkout")
        .expect("the shared checkout is found");
    assert_eq!(finding.confidence.as_str(), "exact");
    // What the checks could not see of a terminal agent is said under the list.
    wait_until(cx, "the coverage line", |cx| {
        painted(&view, cx, "checks-coverage")
    });

    // The Focus filters it: a workspace nobody works in has nothing.
    panel.update(cx, |p, cx| p.set_focus(Some(elsewhere.id), cx));
    wait_until(cx, "the Focus", |cx| {
        panel.read_with(cx, |p, cx| p.rail_scope(cx) == Some(elsewhere.id))
    });
    wait_until(cx, "an empty list", |cx| {
        panel.read_with(cx, |p, cx| {
            p.checks_findings(cx).is_some_and(|a| {
                a.findings
                    .iter()
                    .all(|f| f.scope.workspace_id == elsewhere.id)
            })
        }) && !finding_painted(&view, cx, "checks-row", 0)
    });
    panel.update(cx, |p, cx| p.set_focus(None, cx));
    wait_until(cx, "the list again", |cx| {
        row_of(&panel, cx, &pair).is_some()
    });

    // Dismiss hides the row at once and the daemon keeps it hidden.
    let at = row_of(&panel, cx, &pair).unwrap();
    wait_until(cx, "the row", |cx| {
        finding_painted(&view, cx, "checks-dismiss", at)
    });
    click_finding(&view, cx, "checks-dismiss", at);
    wait_until(cx, "the finding gone", |cx| {
        row_of(&panel, cx, &pair).is_none()
    });
    let after = daemon_findings(&env, false);
    assert!(
        after
            .findings
            .iter()
            .all(|f| f.detector != "shared_checkout"),
        "{after:?}"
    );
    assert!(after.dismissed_hidden >= 1, "{after:?}");
    // Another tab and back: it is read again and is still hidden.
    click_tab(&view, cx, "today-tab-now");
    click_tab(&view, cx, "today-tab-checks");
    for _ in 0..4 {
        draw(cx);
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(3));
    }
    assert!(row_of(&panel, cx, &pair).is_none());
}

#[gpui::test]
fn verify_shows_the_command_first_then_the_result_of_a_passing_and_a_failing_run(
    cx: &mut TestAppContext,
) {
    let env = started();
    let passing = project(&env, "passing", 0);
    let failing = project(&env, "failing", 2);
    let good = shared_pair(&env, &passing, AgentState::Working);
    let bad = shared_pair(&env, &failing, AgentState::Working);
    let (view, cx, panel) = open_wide(cx, &env);
    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "both findings", |cx| {
        row_of(&panel, cx, &good).is_some() && row_of(&panel, cx, &bad).is_some()
    });
    // Their turns are running (the findings are about agents at work): the daemon says so in the
    // plan, and the user may run the tests anyway.

    for (members, passed) in [(&good, true), (&bad, false)] {
        let at = row_of(&panel, cx, members).unwrap();
        let session = panel
            .read_with(cx, |p, cx| p.checks_rows(cx).get(at).and_then(session_of))
            .unwrap();
        wait_until(cx, "the Verify button", |cx| {
            finding_painted(&view, cx, "checks-verify", at)
        });
        click_finding(&view, cx, "checks-verify", at);
        // The resolved command first; nothing has run.
        wait_until(cx, "the command", |cx| {
            panel.read_with(cx, |p, _| {
                p.checks_verify_step(session)
                    .is_some_and(|s| s.starts_with("ready: make test"))
            })
        });
        wait_until(cx, "its Run button", |cx| {
            finding_painted(&view, cx, "checks-verify-run", at)
                || finding_painted(&view, cx, "checks-verify-anyway", at)
        });
        let name = if finding_painted(&view, cx, "checks-verify-run", at) {
            "checks-verify-run"
        } else {
            "checks-verify-anyway"
        };
        click_finding(&view, cx, name, at);
        wait_until(cx, "the result", |cx| {
            panel.read_with(cx, |p, _| {
                p.checks_verify_step(session)
                    .is_some_and(|s| s.starts_with("done:"))
            })
        });
        let step = panel
            .read_with(cx, |p, _| p.checks_verify_step(session))
            .unwrap();
        if passed {
            assert_eq!(step, "done: passed exit 0: make test");
        } else {
            assert_eq!(step, "done: failed exit 2: make test");
        }
        // The findings are read again after the run, and the row it started from may be gone:
        // the result stays, on its row or by itself, until it is closed.
        let close = |cx: &VisualTestContext| {
            (0..8usize)
                .chain(5000..5004)
                .find(|index| finding_painted(&view, cx, "checks-verify-cancel", *index))
        };
        wait_until(cx, "the result's Close button", |cx| close(cx).is_some());
        click_finding(&view, cx, "checks-verify-cancel", close(cx).unwrap());
        wait_until(cx, "the result closed", |cx| {
            panel.read_with(cx, |p, _| p.checks_verify_step(session).is_none())
        });
    }
}

#[gpui::test]
fn the_context_panel_lists_the_findings_of_the_session_and_verifies_it(cx: &mut TestAppContext) {
    use hutan_desktop::workbench::actions;
    use hutan_desktop::workbench::panels::ContextPanel;

    let env = started();
    let failing = project(&env, "failing", 2);
    let pair = shared_pair(&env, &failing, AgentState::Working);
    let (view, cx) = open(cx, &env.daemon);
    cx.simulate_resize(gpui::size(gpui::px(3000.0), gpui::px(1200.0)));
    cx.dispatch_action(actions::OpenPanel("context".into()));
    wait_until(cx, "the Context panel", |cx| {
        view.read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
            .is_some()
    });
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
        .unwrap();
    cx.dispatch_action(actions::OpenSession(pair[0].id.to_string()));
    wait_until(cx, "the session followed", |cx| {
        panel.read_with(cx, |p, _| p.session() == Some(pair[0].id))
    });

    // The checkout the two agents share is a finding about this session, with the Checks tab's
    // actions.
    wait_until(cx, "the findings", |cx| {
        panel.read_with(cx, |p, _| {
            p.findings_listed()
                .iter()
                .any(|f| f.detector == "shared_checkout")
        })
    });
    wait_until(cx, "the row", |cx| {
        finding_painted(&view, cx, "findings-row", 0)
            && finding_painted(&view, cx, "findings-dismiss", 0)
    });

    // Verify: the command first (the agent's turn runs, so the daemon says so), then the result.
    wait_until(cx, "the Verify button", |cx| {
        finding_painted(&view, cx, "findings-verify-session", 0)
    });
    click_finding(&view, cx, "findings-verify-session", 0);
    wait_until(cx, "the command", |cx| {
        panel.read_with(cx, |p, _| {
            p.findings_verify_step()
                .is_some_and(|s| s.starts_with("ready: make test"))
        })
    });
    wait_until(cx, "Run anyway", |cx| {
        finding_painted(&view, cx, "findings-verify-anyway", 9000)
    });
    click_finding(&view, cx, "findings-verify-anyway", 9000);
    wait_until(cx, "the result", |cx| {
        panel.read_with(cx, |p, _| {
            p.findings_verify_step()
                .is_some_and(|s| s == "done: failed exit 2: make test")
        })
    });
}

/// A terminal in `dir`, in workspace `workspace`, in which an agent reported itself working.
fn agent_in_workspace(
    env: &AssistantEnv,
    workspace: hutan_core::WorkspaceId,
    title: &str,
    dir: &Path,
) -> SessionInfo {
    use hutan_protocol::messages::{SessionCreate, SessionResult, SessionSpec, TerminalSpec};
    let created: SessionResult = env.daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: Some(workspace),
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".into(), "-c".into(), "sleep 600".into()],
            title: Some(title.to_owned()),
            cwd: Some(dir.to_path_buf()),
            ..TerminalSpec::default()
        }),
    }));
    report(env, created.session.id, AgentState::Working, 1);
    created.session
}

#[gpui::test]
fn check_with_mawas_sets_the_focus_and_fills_the_composer_with_the_findings_and_sends_nothing(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::actions;

    let env = started();
    let dir = project(&env, "shared", 0);
    let workspace = env.daemon.create_workspace("shared", &[&dir]);
    let first = agent_in_workspace(&env, workspace.id, "first", &dir);
    let _second = agent_in_workspace(&env, workspace.id, "second", &dir);
    let (_view, cx, panel) = open_wide(cx, &env);
    wait_until(cx, "Mawas's chat", |cx| {
        panel.read_with(cx, |p, cx| {
            p.session(cx).is_some_and(|s| p.chat_model(s).is_some())
        })
    });
    let session = panel.read_with(cx, |p, cx| p.session(cx)).unwrap();
    let model = panel.read_with(cx, |p, _| p.chat_model(session)).unwrap();

    // The workspace's ⋯ offers it and the session's does too.
    cx.dispatch_action(actions::CheckWithMawas(actions::CheckAt {
        workspace: workspace.id,
        session: None,
    }));
    wait_until(cx, "the prepared ask", |cx| {
        model.read_with(cx, |m, _| m.draft_message().contains("agents_findings"))
    });
    let draft = model.read_with(cx, |m, _| m.draft_message());
    assert!(draft.starts_with("Check the workspace shared"), "{draft}");
    assert!(draft.contains(&workspace.id.to_string()), "{draft}");
    assert!(draft.contains("write to one checkout"), "{draft}");
    wait_until(cx, "the Focus", |cx| {
        panel.read_with(cx, |p, cx| p.focus(cx) == Some(workspace.id))
    });
    // Nothing was sent: the conversation holds no turn.
    let history: hutan_protocol::chat::ChatHistory = env.daemon.call(Request::ChatHistory(
        hutan_protocol::chat::ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 50,
        },
    ));
    assert!(history.events.is_empty(), "{history:?}");

    // A session's ask names the session.
    cx.dispatch_action(actions::CheckWithMawas(actions::CheckAt {
        workspace: workspace.id,
        session: Some(first.id),
    }));
    wait_until(cx, "the session's ask", |cx| {
        model.read_with(cx, |m, _| {
            m.draft_message()
                .contains(&format!("agents_findings with session_id {}", first.id))
        })
    });
}

/// An agent of the pair that changed `path`, as its integration reports it.
fn report_changed_file(env: &AssistantEnv, session: SessionId, seq: u64, path: &str) {
    use hutan_core::agent::FileChangeKind;
    use hutan_core::insight::{InsightReport, InsightSourceKind, ReportedFile};
    let _: AgentApplied = env.daemon.call(Request::AgentReport(AgentReportRequest {
        session_id: session,
        source: "test-hooks".into(),
        agent: "claude".into(),
        state: None,
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        pid: None,
        node: None,
        insight: Some(InsightReport {
            source: InsightSourceKind::Hooks,
            files: vec![ReportedFile {
                path: path.into(),
                kind: FileChangeKind::Update,
            }],
            ..InsightReport::default()
        }),
    }));
}

fn bounds_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    name: &'static str,
    index: usize,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(Region::Finding(name, index)))
}

/// The rows of the Checks tab that are about one of `members`.
fn rows_of(
    panel: &Entity<AssistantPanel>,
    cx: &VisualTestContext,
    members: &[SessionInfo],
) -> Vec<usize> {
    panel.read_with(cx, |p, cx| {
        p.checks_rows(cx)
            .iter()
            .enumerate()
            .filter(|(_, f)| session_of(f).is_some_and(|id| members.iter().any(|m| m.id == id)))
            .map(|(at, _)| at)
            .collect()
    })
}

#[gpui::test]
fn a_verify_shows_its_command_and_result_on_the_row_that_started_it_and_on_no_other_row_of_the_session(
    cx: &mut TestAppContext,
) {
    let env = started();
    let dir = project(&env, "shared", 0);
    let pair = shared_pair(&env, &dir, AgentState::Working);
    // Both changed one file in one checkout: two findings about the same agent.
    for agent in &pair {
        report_changed_file(&env, agent.id, 2, "shared.txt");
    }
    let (view, cx, panel) = open_wide(cx, &env);
    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "two findings about the pair", |cx| {
        rows_of(&panel, cx, &pair).len() >= 2
    });
    let rows = rows_of(&panel, cx, &pair);
    let (a, b) = (rows[0], rows[1]);
    let findings = panel.read_with(cx, |p, cx| p.checks_rows(cx));
    let session = session_of(&findings[a]).unwrap();
    assert_eq!(
        session_of(&findings[b]),
        Some(session),
        "the two rows act on one session: {findings:?}"
    );
    for at in [a, b] {
        wait_until(cx, "a Verify button", |cx| {
            finding_painted(&view, cx, "checks-verify", at)
        });
    }

    // Verify on the second row: its command is shown there, and nowhere else.
    click_finding(&view, cx, "checks-verify", b);
    wait_until(cx, "the command on its row", |cx| {
        panel.read_with(cx, |p, _| {
            p.checks_verify_step(session)
                .is_some_and(|s| s.starts_with("ready: make test"))
        }) && finding_painted(&view, cx, "checks-verify-step", b)
    });
    draw(cx);
    assert!(
        !finding_painted(&view, cx, "checks-verify-step", a),
        "the other row of the session shows nothing"
    );
    let run = if finding_painted(&view, cx, "checks-verify-run", b) {
        "checks-verify-run"
    } else {
        "checks-verify-anyway"
    };
    click_finding(&view, cx, run, b);
    wait_until(cx, "the result on its row", |cx| {
        panel.read_with(cx, |p, _| {
            p.checks_verify_step(session)
                .is_some_and(|s| s.starts_with("done:"))
        }) && finding_painted(&view, cx, "checks-verify-step", b)
    });
    draw(cx);
    assert!(!finding_painted(&view, cx, "checks-verify-step", a));

    // Verify on the first row takes the state with it.
    click_finding(&view, cx, "checks-verify", a);
    wait_until(cx, "the state on the first row", |cx| {
        finding_painted(&view, cx, "checks-verify-step", a)
    });
    draw(cx);
    assert!(
        !finding_painted(&view, cx, "checks-verify-step", b),
        "one state, on the row that started it last"
    );
    click_finding(&view, cx, "checks-verify-cancel", a);
    wait_until(cx, "the state closed", |cx| {
        panel.read_with(cx, |p, _| p.checks_verify_step(session).is_none())
    });
}

#[gpui::test]
fn a_finding_about_two_agents_names_both_by_title_and_its_evidence_shows_no_session_id(
    cx: &mut TestAppContext,
) {
    let env = started();
    let dir = project(&env, "shared", 0);
    let pair = shared_pair(&env, &dir, AgentState::Working);
    let (view, cx, panel) = open_wide(cx, &env);
    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "the finding", |cx| row_of(&panel, cx, &pair).is_some());
    let at = row_of(&panel, cx, &pair).unwrap();
    let (meta, evidence) = panel
        .read_with(cx, |p, cx| p.checks_row_lines(at, cx))
        .unwrap();
    for agent in &pair {
        assert!(meta.contains(&agent.title), "{meta}");
        assert!(
            evidence.iter().any(|line| line.contains(&agent.title)),
            "{evidence:?}"
        );
        for line in evidence.iter().chain([&meta]) {
            assert!(!line.contains(&agent.id.to_string()), "{line}");
        }
    }
    assert_eq!(evidence.len(), 2, "one line for each agent: {evidence:?}");

    // Opening the row shows the evidence: the row grows.
    wait_until(cx, "the row", |cx| {
        bounds_of(&view, cx, "checks-row", at).is_some()
            && bounds_of(&view, cx, "checks-head", at).is_some()
    });
    let closed = bounds_of(&view, cx, "checks-row", at).unwrap().size.height;
    click_finding(&view, cx, "checks-head", at);
    wait_until(cx, "the evidence", |cx| {
        bounds_of(&view, cx, "checks-row", at).is_some_and(|b| b.size.height > closed)
    });
}

#[gpui::test]
fn the_severity_word_stays_at_the_top_left_of_a_title_that_wraps(cx: &mut TestAppContext) {
    let env = started();
    let dir = project(&env, "shared", 0);
    let pair = shared_pair(&env, &dir, AgentState::Working);
    let (view, cx, panel) = open_wide(cx, &env);
    // The narrowest rail the settings allow, so that the title wraps.
    view.read_with(cx, |wb, _| wb.settings_store().clone())
        .write_user("assistant.rail_width_rem", toml::Value::Float(17.5))
        .unwrap();
    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "the finding", |cx| row_of(&panel, cx, &pair).is_some());
    let at = row_of(&panel, cx, &pair).unwrap();
    wait_until(cx, "the head and its severity word", |cx| {
        bounds_of(&view, cx, "checks-head", at).is_some()
            && bounds_of(&view, cx, "checks-severity", at).is_some()
    });
    let head = bounds_of(&view, cx, "checks-head", at).unwrap();
    let word = bounds_of(&view, cx, "checks-severity", at).unwrap();
    assert!(
        head.size.height > word.size.height * 1.5,
        "the title wraps in the narrow rail: head {head:?}, word {word:?}"
    );
    assert_eq!(
        word.origin.y, head.origin.y,
        "the word is on the first line, never on the last"
    );
    assert_eq!(word.origin.x, head.origin.x, "and at the start of it");
}

#[gpui::test]
fn show_dismissed_lists_what_was_dismissed_and_restore_lists_it_again(cx: &mut TestAppContext) {
    let env = started();
    let dir = project(&env, "shared", 0);
    let pair = shared_pair(&env, &dir, AgentState::Working);
    let (view, cx, panel) = open_wide(cx, &env);
    click_tab(&view, cx, "today-tab-checks");
    wait_until(cx, "the finding", |cx| row_of(&panel, cx, &pair).is_some());
    let at = row_of(&panel, cx, &pair).unwrap();
    wait_until(cx, "its Dismiss button", |cx| {
        finding_painted(&view, cx, "checks-dismiss", at)
    });
    click_finding(&view, cx, "checks-dismiss", at);
    wait_until(cx, "the finding gone", |cx| {
        row_of(&panel, cx, &pair).is_none()
    });
    assert!(!panel.read_with(cx, |p, _| p.checks_show_dismissed()));

    // The tab says one is hidden and offers to show it.
    wait_until(cx, "Show dismissed", |cx| {
        painted(&view, cx, "checks-dismissed-toggle")
    });
    click_tab(&view, cx, "checks-dismissed-toggle");
    wait_until(cx, "the dismissed finding with Restore", |cx| {
        panel.read_with(cx, |p, cx| {
            p.checks_show_dismissed()
                && p.checks_dismissed_rows(cx)
                    .iter()
                    .any(|f| f.detector == "shared_checkout" && f.dismissed)
        }) && finding_painted(&view, cx, "checks-restore", 7000)
    });
    // Listing it changed nothing at the daemon.
    assert!(
        daemon_findings(&env, false)
            .findings
            .iter()
            .all(|f| f.detector != "shared_checkout")
    );

    // Restore: the row is back among the findings, the daemon lists it again.
    click_finding(&view, cx, "checks-restore", 7000);
    wait_until(cx, "the finding listed again", |cx| {
        row_of(&panel, cx, &pair).is_some()
            && panel.read_with(cx, |p, cx| p.checks_dismissed_rows(cx).is_empty())
    });
    assert!(
        daemon_findings(&env, false)
            .findings
            .iter()
            .any(|f| f.detector == "shared_checkout")
    );
    // Nothing is dismissed now; Hide dismissed turns the list off.
    wait_until(cx, "Hide dismissed", |cx| {
        painted(&view, cx, "checks-dismissed-toggle")
    });
    click_tab(&view, cx, "checks-dismissed-toggle");
    wait_until(cx, "the list off", |cx| {
        !panel.read_with(cx, |p, _| p.checks_show_dismissed())
    });
}
