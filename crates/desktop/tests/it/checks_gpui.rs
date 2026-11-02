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
    report(&env, created.session.id, AgentState::Working, 1);
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
    let (view, cx, panel) = open_wide(cx, &env);
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
