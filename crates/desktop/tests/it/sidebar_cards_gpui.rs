#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! SIDEBAR-CARDS: the sidebar's rows. A shell is one line; a coding agent is a card whose lane
//! carries its harness glyph and the state's badge and whose second line (the *news line*) exists
//! only when something is going on — a question, a failure, the tool it runs, the first line of
//! an unread reply; the first agent no hook speaks for says what hooks add and links to *Agent
//! integrations…*; `+N −M` shows on the project rows and in the Changes panel only when asked.
//! A real daemon, real PTYs (`cat` echoes the OSC 7501 bytes OMP writes), the Codex fixture for a
//! chat, real git repositories.

use crate::common;
use crate::common::chat::{ChatEnv, held_turns};

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_desktop::sessions::Unread;
use hutan_desktop::sessions::card::NewsKind;
use hutan_desktop::workbench::panels::{ChangesPanel, RowKind, SessionsPanel, SidebarRow};
use hutan_desktop::workbench::ui::BadgeState;
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::messages::{
    Request, SessionInfo, SessionRename, SessionResult, TerminalAgentState,
};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
    )
    .unwrap();
    open_connected(cx, daemon, daemon.connection())
}

fn open_connected<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
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
    (view, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..6 {
        cx.executor().advance_clock(Duration::from_millis(60));
        draw(cx);
    }
}

fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<SessionsPanel> {
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock")
}

fn rows(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext) -> Vec<SidebarRow> {
    panel.read_with(cx, |p, cx| p.visible_rows(cx))
}

fn row(panel: &Entity<SessionsPanel>, cx: &mut VisualTestContext, title: &str) -> SidebarRow {
    rows(panel, cx)
        .into_iter()
        .find(|r| r.kind == RowKind::Session && r.title == title)
        .unwrap_or_else(|| panic!("no session row `{title}`"))
}

/// A terminal that echoes what it is sent, called `title` by the user (so the agent's own title
/// never renames it).
fn session(daemon: &TestDaemon, title: &str) -> SessionInfo {
    let created = daemon.create_session(&["sh", "-c", "exec cat"], None);
    let renamed: SessionResult = daemon.call(Request::SessionRename(SessionRename {
        session_id: created.id,
        title: title.to_owned(),
    }));
    renamed.session
}

/// What OMP writes (ST-terminated); the newline makes `cat` echo the line back as output.
fn osc(daemon: &TestDaemon, session: &SessionInfo, payload: &str) {
    daemon.send_input(
        session.terminal_id.unwrap(),
        format!("\x1b]{payload}\x1b\\\r").as_bytes(),
    );
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn wait_state(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    id: SessionId,
    state: TerminalAgentState,
) {
    wait_until(cx, &format!("the agent to be {state:?}"), |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(id)
                .and_then(|s| s.terminal_agent.as_ref())
                .is_some_and(|a| a.state == state)
        })
    });
}

fn height(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) -> f32 {
    draw(cx);
    view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::SessionRow(id))
            .map(|b| f32::from(b.size.height))
    })
    .expect("the row is drawn")
}

fn drawn(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) -> bool {
    draw(cx);
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

#[gpui::test]
fn a_shell_stays_one_line_and_an_agent_card_grows_a_line_only_with_news(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let shell = session(&daemon, "shell");
    let agent = session(&daemon, "agent");
    let other = session(&daemon, "other agent");
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
    let card_extra = {
        use hutan_desktop::theme::{Density, DensityExt as _};
        let tokens = Density::Compact.ui();
        tokens.sidebar_card - tokens.sidebar_row
    };

    // Plain terminals: the prompt glyph, no badge, one line.
    wait_until(cx, "the rows", |cx| rows(&sidebar, cx).len() >= 4);
    for title in ["shell", "agent", "other agent"] {
        let plain = row(&sidebar, cx, title);
        assert_eq!(plain.lane, BadgeState::Idle, "{title}");
        assert!(plain.news.is_none(), "{title}");
    }
    let one_line = height(&view, cx, shell.id);

    // Both start OMP and say they are idle: idle agents are one line too — except the first one
    // no hook speaks for, which says what hooks add and links to the integrations.
    for session in [&agent, &other] {
        osc(&daemon, session, "7501;state=idle:app=omp");
        wait_state(&view, cx, session.id, TerminalAgentState::Idle);
    }
    wait_until(cx, "the hint on the first agent", |cx| {
        row(&sidebar, cx, "agent")
            .news
            .is_some_and(|n| n.kind == NewsKind::HooksHint)
    });
    assert!(
        row(&sidebar, cx, "other agent").news.is_none(),
        "the hint is said once, on the first card"
    );
    assert!(row(&sidebar, cx, "shell").news.is_none());
    assert!(drawn(&view, cx, Region::HooksHint(agent.id)));
    assert!(!drawn(&view, cx, Region::HooksHint(other.id)));
    assert!(!drawn(&view, cx, Region::HooksHint(shell.id)));
    assert!(!drawn(&view, cx, Region::SessionNote(agent.id)));
    assert_eq!(
        height(&view, cx, other.id),
        one_line,
        "an idle agent: one line"
    );
    assert_eq!(
        height(&view, cx, agent.id),
        one_line + card_extra,
        "the card with the hint grew by the news line"
    );
    assert_eq!(height(&view, cx, shell.id), one_line);

    // The link opens Settings → Agent Integrations.
    let hint = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::HooksHint(agent.id)))
        .expect("the hint is drawn");
    cx.simulate_click(
        gpui::point(
            hint.origin.x + hint.size.width - gpui::px(20.0),
            hint.origin.y + hint.size.height / 2.0,
        ),
        Modifiers::none(),
    );
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("integrations".to_owned()),
        "the link goes to Agent integrations"
    );
    view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
    draw(cx);

    // A turn: the lane's arc; nothing to say about it yet, so the hint stays where it was.
    osc(&daemon, &other, "7501;state=working:app=omp");
    wait_state(&view, cx, other.id, TerminalAgentState::Working);
    assert_eq!(row(&sidebar, cx, "other agent").lane, BadgeState::Working);
    assert!(row(&sidebar, cx, "other agent").news.is_none());

    // It asks: a request badge and the question, in the news line.
    let question = "Overwrite the lockfile?";
    osc(
        &daemon,
        &agent,
        &format!(
            "7501;state=blocked:kind=permission:app=omp:msg={}",
            b64(question)
        ),
    );
    wait_state(&view, cx, agent.id, TerminalAgentState::Blocked);
    wait_until(cx, "the question", |cx| {
        let card = row(&sidebar, cx, "agent");
        card.lane == BadgeState::Request
            && card
                .news
                .is_some_and(|n| n.kind == NewsKind::Request && n.text == question)
    });
    assert!(drawn(&view, cx, Region::SessionNote(agent.id)));
    assert!(
        !drawn(&view, cx, Region::HooksHint(agent.id)),
        "news replaces the hint"
    );
    assert_eq!(height(&view, cx, agent.id), one_line + card_extra);

    // It fails: the failure badge and the message.
    osc(
        &daemon,
        &agent,
        &format!("7501;state=error:app=omp:msg={}", b64("Rate limited")),
    );
    wait_state(&view, cx, agent.id, TerminalAgentState::Error);
    wait_until(cx, "the failure", |cx| {
        let card = row(&sidebar, cx, "agent");
        card.lane == BadgeState::Failed
            && card
                .news
                .is_some_and(|n| n.kind == NewsKind::Failure && n.text == "Rate limited")
    });

    // The next turn ends well: finished and unread (the ✓ badge) — and nothing more to say, so
    // the card is back to the hint.
    osc(&daemon, &agent, "7501;state=working:app=omp");
    wait_state(&view, cx, agent.id, TerminalAgentState::Working);
    osc(&daemon, &agent, "7501;state=done:app=omp");
    wait_state(&view, cx, agent.id, TerminalAgentState::Done);
    wait_until(cx, "the done badge", |cx| {
        let card = row(&sidebar, cx, "agent");
        card.lane == BadgeState::DoneUnread
            && card.news.is_some_and(|n| n.kind == NewsKind::HooksHint)
    });

    // A shell that rings its bell while out of sight is unread, and still one line.
    daemon.send_input(shell.terminal_id.unwrap(), b"\x07\r");
    wait_until(cx, "the shell's mark", |cx| {
        row(&sidebar, cx, "shell").lane == BadgeState::DoneUnread
    });
    assert!(row(&sidebar, cx, "shell").news.is_none());
    assert_eq!(height(&view, cx, shell.id), one_line);
}

#[gpui::test]
fn an_unread_reply_is_the_first_line_of_the_card_until_the_chat_is_looked_at(
    cx: &mut TestAppContext,
) {
    let env = ChatEnv::new();
    let gate = env.work.path().join("go");
    let transcript = held_turns(env.work.path(), std::slice::from_ref(&gate));
    let daemon = TestDaemon::start_with_codex(env.replay(&transcript));
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);

    // A chat between turns: one line, no badge, and no hint (hooks are for terminals).
    wait_until(cx, "the chat's row", |cx| {
        rows(&sidebar, cx)
            .iter()
            .any(|r| r.kind == RowKind::Session)
    });
    let title = rows(&sidebar, cx)
        .into_iter()
        .find(|r| r.kind == RowKind::Session)
        .unwrap()
        .title;
    let idle = row(&sidebar, cx, &title);
    assert_eq!(idle.lane, BadgeState::Idle);
    assert!(idle.news.is_none());
    let one_line = height(&view, cx, chat.id);

    // The turn runs (held at its start): the arc.
    let _: hutan_protocol::chat::ChatSent =
        daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: chat.id,
            text: "Reply with the single word pong and nothing else.".to_owned(),
            attachments: Vec::new(),
        }));
    wait_until(cx, "the turn to run", |cx| {
        row(&sidebar, cx, &title).lane == BadgeState::Working
    });

    // It ends while nobody looks: finished, unread, and the first line of the reply under the
    // name.
    std::fs::write(&gate, "").unwrap();
    wait_until(cx, "the reply on the card", |cx| {
        let card = row(&sidebar, cx, &title);
        card.lane == BadgeState::DoneUnread
            && card
                .news
                .is_some_and(|n| n.kind == NewsKind::Reply && n.text == "pong")
    });
    assert!(drawn(&view, cx, Region::SessionNote(chat.id)));
    assert!(height(&view, cx, chat.id) > one_line);
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).unread(chat.id)),
        Some(Unread::Turn)
    );

    // Looking at the chat reads it: one line again, no badge.
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    settle(cx);
    wait_until(cx, "the card to quiet down", |cx| {
        let card = row(&sidebar, cx, &title);
        card.lane == BadgeState::Idle && card.news.is_none()
    });
    assert_eq!(height(&view, cx, chat.id), one_line);
}

// ---- diffstat -------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[gpui::test]
fn plus_and_minus_counts_show_on_project_rows_and_in_the_changes_panel_only_when_asked(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let keep = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(keep.path()).unwrap().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("kept.txt"), "one\ntwo\nthree\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    // Uncommitted: three lines added to a new file, one removed from a tracked one.
    std::fs::write(root.join("new.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(root.join("kept.txt"), "one\nthree\n").unwrap();
    let workspace = daemon.create_workspace("ws", &[&root]).id;
    let (view, cx) = open(cx, &daemon);
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    let sidebar = panel(&view, cx);

    // Off by default: the project row says how many files changed, never the lines.
    let project = |cx: &mut VisualTestContext| {
        rows(&sidebar, cx)
            .into_iter()
            .find(|r| r.kind == RowKind::Project)
    };
    wait_until(cx, "the project's files to be counted", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store
                .workspaces()
                .iter()
                .find(|w| w.roots.contains(&root))
                .is_some_and(|w| store.changed_files_of(w) == 2)
        })
    });
    draw(cx);
    let listed = rows(&sidebar, cx);
    assert_eq!(
        project(cx)
            .unwrap_or_else(|| panic!("the project row in {listed:?}"))
            .diffstat,
        None
    );

    // On: `+N −M` of its uncommitted files.
    let settings = view.read_with(cx, |wb, _| wb.settings_store().clone());
    settings
        .write_user("panels.sessions.show_diffstat", toml::Value::Boolean(true))
        .unwrap();
    wait_until(cx, "the diffstat on the row", |cx| {
        project(cx).is_some_and(|r| r.diffstat.as_deref() == Some("+3 −1"))
    });
    settings
        .write_user("panels.sessions.show_diffstat", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the diffstat to go", |cx| {
        project(cx).is_some_and(|r| r.diffstat.is_none())
    });

    // The Changes panel: the files and their status; the counts only with its own setting.
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    let changes = view
        .read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn");
    wait_until(cx, "the checkout's files", |cx| {
        changes.read_with(cx, |p, _| p.counts(&root).is_some_and(|c| c.files == 2))
    });
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ChangesPanelFile(0, 0))
            .is_some()),
        "the file rows are listed"
    );
    assert!(!drawn(&view, cx, Region::ChangesPanelCounts(0)));
    assert!(!drawn(&view, cx, Region::ChangesPanelFileCounts(0, 0)));
    settings
        .write_user("panels.changes.show_diffstat", toml::Value::Boolean(true))
        .unwrap();
    wait_until(cx, "the counts in the panel", |cx| {
        drawn(&view, cx, Region::ChangesPanelCounts(0))
            && drawn(&view, cx, Region::ChangesPanelFileCounts(0, 0))
    });
}
