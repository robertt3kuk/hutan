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
use gpui::{Entity, TestAppContext, VisualTestContext};
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

    // Both start OMP and say they are idle: idle agents are one line — the first one no hook
    // speaks for wears a small ⓘ about hooks on that line, not a second line of its own.
    for session in [&agent, &other] {
        osc(&daemon, session, "7501;state=idle:app=omp");
        wait_state(&view, cx, session.id, TerminalAgentState::Idle);
    }
    wait_until(cx, "the ⓘ on the first agent", |cx| {
        drawn(&view, cx, Region::HooksHint(agent.id))
    });
    for title in ["shell", "agent", "other agent"] {
        assert!(row(&sidebar, cx, title).news.is_none(), "{title}");
    }
    assert!(
        !drawn(&view, cx, Region::HooksHint(other.id)),
        "the hint is said once, on the first card"
    );
    assert!(!drawn(&view, cx, Region::HooksHint(shell.id)));
    assert!(!drawn(&view, cx, Region::SessionNote(agent.id)));
    for session in [&agent, &other, &shell] {
        assert_eq!(
            height(&view, cx, session.id),
            one_line,
            "an idle agent, hint or not: one line"
        );
    }

    // The ⓘ opens Settings → Agent Integrations and is gone for good once it was used.
    let hint = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::HooksHint(agent.id)))
        .expect("the hint is drawn");
    common::click_at(cx, hint.center());
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .settings_sheet()
            .map(|s| s.namespace().to_owned())),
        Some("integrations".to_owned()),
        "the ⓘ goes to Agent integrations"
    );
    view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
    wait_until(cx, "the hint to end", |cx| {
        !view.read_with(cx, |wb, _| {
            wb.settings_store()
                .get::<hutan_desktop::settings::SessionsPanelSettings>()
                .hooks_hint
        })
    });
    assert!(!drawn(&view, cx, Region::HooksHint(agent.id)));
    assert!(
        !drawn(&view, cx, Region::HooksHint(other.id)),
        "it does not move to the next card"
    );

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
    // the card is one line again.
    osc(&daemon, &agent, "7501;state=working:app=omp");
    wait_state(&view, cx, agent.id, TerminalAgentState::Working);
    osc(&daemon, &agent, "7501;state=done:app=omp");
    wait_state(&view, cx, agent.id, TerminalAgentState::Done);
    wait_until(cx, "the done badge", |cx| {
        let card = row(&sidebar, cx, "agent");
        card.lane == BadgeState::DoneUnread && card.news.is_none()
    });

    // A shell that rings its bell while out of sight wears a bell (FX-UI-A 4: no ✓, no bold
    // name), and is still one line.
    daemon.send_input(shell.terminal_id.unwrap(), b"\x07\r");
    wait_until(cx, "the shell's bell", |cx| {
        drawn(&view, cx, Region::SessionBell(shell.id))
    });
    assert_eq!(row(&sidebar, cx, "shell").lane, BadgeState::Idle);
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

/// FX-UI-A 1: `/context` is a turn for the harness and none for the user (`hutan_core::insight::
/// is_measure_message`): when it ends nobody has to look at it — no unseen mark, no tab check, no
/// news line — and the next real turn is news as before.
#[gpui::test]
fn a_measurement_never_leaves_a_chat_unread_but_the_next_turn_does(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let gates = [env.work.path().join("go1"), env.work.path().join("go2")];
    let transcript = held_turns(env.work.path(), &gates);
    // The fake agent echoes the message it was recorded with: the first turn is the measurement.
    let first_turn_ends = std::fs::read_to_string(&transcript)
        .unwrap()
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("\"dir\":\"hold\""))
        .nth(1)
        .map(|(n, _)| n)
        .unwrap();
    let edited: Vec<String> = std::fs::read_to_string(&transcript)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(n, line)| {
            if n < first_turn_ends {
                line.replace(
                    "Reply with the single word pong and nothing else.",
                    hutan_core::insight::MEASURE_COMMAND,
                )
            } else {
                line.to_owned()
            }
        })
        .collect();
    std::fs::write(&transcript, edited.join("\n") + "\n").unwrap();
    let daemon = TestDaemon::start_with_codex(env.replay(&transcript));
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
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
    let send = |text: &str| {
        let _: hutan_protocol::chat::ChatSent =
            daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
                session_id: chat.id,
                text: text.to_owned(),
                attachments: Vec::new(),
            }));
    };
    let turn_active = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(chat.id)
                .and_then(|s| s.chat.as_ref())
                .is_some_and(|c| c.turn_active)
        })
    };

    send(hutan_core::insight::MEASURE_COMMAND);
    wait_until(cx, "the measurement to run", |cx| turn_active(cx));
    std::fs::write(&gates[0], "").unwrap();
    wait_until(cx, "the measurement to end", |cx| !turn_active(cx));
    wait_until(cx, "no mark for the measurement", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).unread(chat.id))
            .is_none()
    });
    settle(cx);
    let card = row(&sidebar, cx, &title);
    assert_eq!(card.lane, BadgeState::Idle, "no done badge");
    assert!(card.news.is_none(), "no news line");
    assert!(
        view.read_with(cx, |wb, cx| wb
            .sessions()
            .read(cx)
            .reply_line(chat.id)
            .is_none()),
        "no reply line kept"
    );

    send("Reply with the single word pong and nothing else.");
    wait_until(cx, "the turn to run", |cx| turn_active(cx));
    std::fs::write(&gates[1], "").unwrap();
    wait_until(cx, "the reply on the card", |cx| {
        let card = row(&sidebar, cx, &title);
        card.lane == BadgeState::DoneUnread
            && card
                .news
                .is_some_and(|n| n.kind == NewsKind::Reply && n.text == "pong")
    });
}

/// FX-UI-A 3: an agent chat wears its harness's monogram like an agent in a terminal (never the
/// generic robot), and its tab leads with that glyph and the state's badge — the arc while a turn
/// runs, the check once it ended unseen — as the sidebar's row does.
#[gpui::test]
fn a_chat_wears_its_harness_monogram_and_its_tab_the_state_badge(cx: &mut TestAppContext) {
    use hutan_desktop::icons::Icon;
    use hutan_desktop::workbench::panels::{harness_icon, session_icon};

    let env = ChatEnv::new();
    let gate = env.work.path().join("go");
    let transcript = held_turns(env.work.path(), std::slice::from_ref(&gate));
    let daemon = TestDaemon::start_with_codex(env.replay(&transcript));
    let chat = daemon.create_chat(&env.cwd());
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
    wait_until(cx, "the chat's row", |cx| {
        rows(&sidebar, cx)
            .iter()
            .any(|r| r.kind == RowKind::Session)
    });

    // The monogram of the harness behind the chat; a hook's own spelling is the same harness.
    let info = view
        .read_with(cx, |wb, cx| {
            wb.sessions().read(cx).session(chat.id).cloned()
        })
        .unwrap();
    assert_eq!(session_icon(&info), Icon::HarnessCodex);
    assert_eq!(harness_icon("Claude Code"), Icon::HarnessClaude);
    assert_eq!(harness_icon("omp"), Icon::HarnessOmp);
    assert_eq!(harness_icon("zsh"), Icon::HarnessUnknown);
    let item = hutan_desktop::workbench::model::ItemRef::Chat {
        session: chat.id.to_string(),
    };
    view.update_in(cx, |wb, window, cx| wb.open_session(chat.id, window, cx));
    settle(cx);
    let pane = view.read_with(cx, |wb, _| wb.state().first_pane_id());
    let (glyph, _) = view.read_with(cx, |wb, cx| wb.item_icon_and_title(&item, cx));
    assert_eq!(glyph, Icon::HarnessCodex);

    // Idle: the glyph alone. A turn: the arc on the glyph of the tab.
    assert!(!drawn(&view, cx, Region::TabState(pane, 0)));
    let _: hutan_protocol::chat::ChatSent =
        daemon.call(Request::ChatSend(hutan_protocol::chat::ChatSend {
            session_id: chat.id,
            text: "Reply with the single word pong and nothing else.".to_owned(),
            attachments: Vec::new(),
        }));
    wait_until(
        cx,
        "the badge on the tab's glyph while the turn runs",
        |cx| drawn(&view, cx, Region::TabState(pane, 0)),
    );
    // The sidebar says the same of the same session.
    let title = rows(&sidebar, cx)
        .into_iter()
        .find(|r| r.kind == RowKind::Session)
        .unwrap()
        .title;
    assert_eq!(row(&sidebar, cx, &title).lane, BadgeState::Working);
    std::fs::write(&gate, "").unwrap();
    wait_until(cx, "the turn to end", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(chat.id)
                .and_then(|s| s.chat.as_ref())
                .is_some_and(|c| !c.turn_active)
        })
    });
}

/// A terminal in `cwd` that echoes what it is sent, called `title`.
fn session_in(daemon: &TestDaemon, title: &str, cwd: &Path) -> SessionInfo {
    use hutan_protocol::messages::{SessionCreate, SessionSpec, TerminalSpec};
    let created: SessionResult = daemon.call(Request::SessionCreate(SessionCreate {
        workspace_id: None,
        spec: SessionSpec::Terminal(TerminalSpec {
            command: vec!["sh".to_owned(), "-c".to_owned(), "exec cat".to_owned()],
            cwd: Some(cwd.to_path_buf()),
            ..TerminalSpec::default()
        }),
    }));
    let renamed: SessionResult = daemon.call(Request::SessionRename(SessionRename {
        session_id: created.session.id,
        title: title.to_owned(),
    }));
    renamed.session
}

fn live(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) -> bool {
    view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .session(id)
            .is_some_and(|s| s.state.is_live())
    })
}

/// FX-UI-A 4: a session that ended with exit 0 is a hollow ring and a faint name, keeps the glyph
/// of the agent that ran in it (the daemon forgets the agent at the exit) and is not *Ready for
/// review* — unless it left uncommitted files in its checkout, which are the work to review.
#[gpui::test]
fn a_session_that_ended_cleanly_is_a_hollow_ring_and_ready_for_review_only_with_changes(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::icons::Icon;
    use hutan_desktop::workbench::panels::{session_icon, session_icon_with};

    let daemon = TestDaemon::start();
    let keep = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(keep.path()).unwrap().join("repo");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("kept.txt"), "one\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    // In Home, whose rows are always listed; the other one in its repository.
    let clean = session(&daemon, "clean");
    let dirty = session_in(&daemon, "dirty", &root);
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
    wait_until(cx, "the rows", |cx| rows(&sidebar, cx).len() >= 3);

    // OMP runs in both; the window sees it.
    for session in [&clean, &dirty] {
        osc(&daemon, session, "7501;state=idle:app=omp");
        wait_state(&view, cx, session.id, TerminalAgentState::Idle);
    }
    wait_until(cx, "the window to remember the agent", |cx| {
        view.read_with(cx, |wb, cx| {
            let store = wb.sessions().read(cx);
            store.remembered_agent(clean.id) == Some("omp")
                && store.remembered_agent(dirty.id) == Some("omp")
        })
    });
    // The dirty checkout is counted before the sessions end.
    std::fs::write(root.join("new.txt"), "x\n").unwrap();
    wait_until(cx, "the checkout's file to be counted", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).changed_files_at(&root) == Some(1)
        })
    });

    // Both programs exit 0 (EOF on `cat`).
    for session in [&clean, &dirty] {
        daemon.send_input(session.terminal_id.unwrap(), b"\x04");
    }
    wait_until(cx, "both to end", |cx| {
        !live(&view, cx, clean.id) && !live(&view, cx, dirty.id)
    });

    // Nothing to review in the clean one: a hollow ring, no mark, still OMP's glyph.
    wait_until(cx, "the clean one is a hollow ring", |cx| {
        row(&sidebar, cx, "clean").lane == BadgeState::Ended
    });
    // The one with files left in its checkout is work to review.
    wait_until(cx, "the dirty one is ready for review", |cx| {
        row(&sidebar, cx, "dirty").lane == BadgeState::DoneUnread
    });
    let sections: Vec<String> = rows(&sidebar, cx)
        .into_iter()
        .filter(|r| r.kind == RowKind::Section)
        .map(|r| r.title)
        .collect();
    assert!(
        sections
            .iter()
            .any(|s| s.starts_with("Ready for review · 1")),
        "{sections:?}"
    );
    view.read_with(cx, |wb, cx| {
        let store = wb.sessions().read(cx);
        assert_eq!(store.unread(clean.id), None);
        assert_eq!(store.unread(dirty.id), Some(Unread::Done));
        let info = store.session(clean.id).unwrap();
        // The daemon no longer says what ran in it; the window does.
        assert_eq!(session_icon(info), Icon::SessionTerminal);
        assert_eq!(
            session_icon_with(info, store.remembered_agent(clean.id)),
            Icon::HarnessOmp
        );
    });
}

/// FX-UI-A 4: a plain shell that rings its bell (or notifies) while out of sight gets a bell on
/// its row and its tab — not a bold name, a ✓ or a place in *Ready for review*; an agent that
/// finishes a turn does keep its ✓.
#[gpui::test]
fn a_shell_that_rings_gets_only_a_bell(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let shell = session(&daemon, "shell");
    let other = session(&daemon, "other");
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
    wait_until(cx, "the rows", |cx| rows(&sidebar, cx).len() >= 3);
    // The shell is not the one on screen.
    view.update_in(cx, |wb, window, cx| wb.open_session(other.id, window, cx));
    settle(cx);

    daemon.send_input(shell.terminal_id.unwrap(), b"\x07\r");
    wait_until(cx, "the shell's bell", |cx| {
        drawn(&view, cx, Region::SessionBell(shell.id))
    });
    assert_eq!(row(&sidebar, cx, "shell").lane, BadgeState::Idle);
    assert!(
        !rows(&sidebar, cx)
            .iter()
            .any(|r| r.kind == RowKind::Section && r.title.starts_with("Ready for review")),
        "a bell is not work to review"
    );
    assert!(!drawn(&view, cx, Region::SessionUnread(shell.id)));
    assert!(!drawn(&view, cx, Region::SessionBell(other.id)));

    // Looking at the shell takes the bell away.
    view.update_in(cx, |wb, window, cx| wb.open_session(shell.id, window, cx));
    settle(cx);
    wait_until(cx, "the bell to go", |cx| {
        !drawn(&view, cx, Region::SessionBell(shell.id))
    });
}

/// FX-UI-A 6: the project row is calm — the uncommitted files are one counter with the Changes
/// glyph (explained by its tooltip), and the session count (`5/7`, explained too) appears only
/// while the project is collapsed: open, its rows are the sessions.
#[gpui::test]
fn a_project_row_counts_what_is_uncommitted_and_its_sessions_only_while_collapsed(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let keep = tempfile::tempdir().unwrap();
    let root = TestDaemon::dirty_repo(keep.path());
    let worker = session_in(&daemon, "worker", &root);
    let (view, cx) = open(cx, &daemon);
    let sidebar = panel(&view, cx);
    let project = |cx: &mut VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .workspaces()
                .iter()
                .find(|w| w.roots.contains(&root))
                .map(|w| w.id)
        })
    };
    wait_until(cx, "the project", |cx| project(cx).is_some());
    let id = project(cx).unwrap();
    wait_until(cx, "the uncommitted counter", |cx| {
        drawn(&view, cx, Region::ProjectChanged(id))
    });
    let collapsed = |cx: &mut VisualTestContext| {
        rows(&sidebar, cx)
            .into_iter()
            .find(|r| r.kind == RowKind::Project && r.title == "repo")
            .map(|r| r.collapsed)
    };
    wait_until(cx, "the project row", |cx| collapsed(cx).is_some());

    // Collapsed: the counter says how many sessions it holds.
    if collapsed(cx) == Some(false) {
        sidebar.update(cx, |p, cx| p.toggle_container(id, cx));
    }
    wait_until(cx, "the row to fold", |cx| collapsed(cx) == Some(true));
    wait_until(cx, "the session count", |cx| {
        drawn(&view, cx, Region::ProjectCount(id))
    });
    assert!(drawn(&view, cx, Region::ProjectChanged(id)));

    // Open: the sessions are its rows; the count goes, the uncommitted files stay.
    sidebar.update(cx, |p, cx| p.toggle_container(id, cx));
    wait_until(cx, "the row to open", |cx| collapsed(cx) == Some(false));
    wait_until(cx, "the session's row", |cx| {
        rows(&sidebar, cx)
            .iter()
            .any(|r| r.kind == RowKind::Session && r.title == worker.title)
    });
    assert!(!drawn(&view, cx, Region::ProjectCount(id)));
    assert!(drawn(&view, cx, Region::ProjectChanged(id)));
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
