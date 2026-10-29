#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! CONTEXT-PANEL: the Context panel of the right dock over a real daemon whose harnesses replay
//! recorded transcripts — OMP and Claude Code through the ACP fixture agent
//! (`insight-omp.jsonl`, `insight-claude.jsonl`), Codex through its recorded app-server
//! (`plain-turn`). The numbers the panel shows are the fixtures' (the daemon's own tests pin the
//! same ones on the wire), the measure-context flow runs against the fixtures' `/context`, and
//! every harness says what it does not report.

use crate::common;

use std::path::{Path, PathBuf};

use common::chat::ChatEnv;
use common::{TestDaemon, click_at, draw, open_with, settle, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::SessionId;
use hutan_core::insight::{Exactness, InsightField};
use hutan_desktop::workbench::panels::context_model as model;
use hutan_desktop::workbench::panels::{ContextEmpty, ContextLoad, ContextMeasure, ContextPanel};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::messages::{Request, SessionInfo, SessionResult};
use hutan_settings::{SettingsPaths, SettingsStore};

// ---- harness ----------------------------------------------------------------------------------

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

/// A daemon whose harness `agent` (`omp`, `claude`) replays the ACP fixture `name`.
fn acp_daemon(agent: &str, name: &str) -> TestDaemon {
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.{agent}]\nbin = {bin:?}\nargs = [\"replay\", {t:?}, \"acp\"]\n",
        bin = acp_bin(),
        t = transcript(name),
    ))
}

fn create_chat(daemon: &TestDaemon, agent: &str, cwd: &Path, title: &str) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: None,
        cwd: cwd.to_path_buf(),
        agent: agent.to_owned(),
        model: None,
        approval_policy: None,
        sandbox: None,
        title: Some(title.to_owned()),
        mode: None,
        provider_id: None,
        system_prompt: None,
        account: None,
        safety: None,
    }));
    created.session
}

fn send(daemon: &TestDaemon, session: SessionId, text: &str) {
    let _: ChatSent = daemon.call(Request::ChatSend(ChatSend {
        session_id: session,
        text: text.to_owned(),
        attachments: vec![],
    }));
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[history]\nenabled = false\n",
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

/// Shows the chat in the active pane.
fn show_chat(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to be shown", |cx| {
        view.read_with(cx, |wb, _| wb.shown_session()) == Some(session)
    });
    settle(cx);
}

/// Opens the Context panel (it is on the right rail) and waits for its view.
fn panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<ContextPanel> {
    cx.dispatch_action(actions::OpenPanel("context".into()));
    wait_until(cx, "the Context panel", |cx| {
        view.read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
            .is_some_and(|panel| panel.read_with(cx, |p, _| p.is_visible()))
    });
    view.read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"))
        .unwrap()
}

/// The turns the panel's insight has ended.
fn turns_of(panel: &Entity<ContextPanel>, cx: &VisualTestContext) -> u64 {
    panel.read_with(cx, |p, _| {
        p.insight()
            .and_then(|i| i.totals.as_ref())
            .map_or(0, |t| t.turns)
    })
}

fn wait_turns(panel: &Entity<ContextPanel>, cx: &mut VisualTestContext, turns: u64) {
    wait_until(cx, &format!("{turns} turn(s) in the panel"), |cx| {
        turns_of(panel, cx) >= turns
    });
}

fn bounds(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn wait_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        bounds(view, cx, region).is_some()
    });
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_region(view, cx, region);
    let at = bounds(view, cx, region).unwrap().center();
    click_at(cx, at);
    draw(cx);
}

// ---- OMP ------------------------------------------------------------------------------------------

/// The recorded OMP exchange: two prompts (the first with a `read`), `usage_update` 11 200 of
/// 200 000 with the session's cost 0.02958 then 0.03636.
#[gpui::test]
fn an_omp_chat_fills_the_panel_with_the_fixtures_numbers_and_follows_pushes(
    cx: &mut TestAppContext,
) {
    let daemon = acp_daemon("omp", "insight-omp");
    let work = tempfile::tempdir().unwrap();
    let chat = create_chat(&daemon, "omp", work.path(), "Migrate auth");
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, chat.id);
    let panel = panel(&view, cx);
    // Nothing has been said yet: the chat is known, nothing is reported.
    wait_until(cx, "the insight to be read", |cx| {
        panel.read_with(cx, |p, _| *p.load() == ContextLoad::Ready)
    });
    assert_eq!(panel.read_with(cx, |p, _| p.session()), Some(chat.id));
    assert_eq!(
        panel.read_with(cx, |p, _| p.empty().cloned()),
        Some(ContextEmpty::Nothing)
    );

    // The turns arrive by push (`agent.insight_updated`), without a refresh.
    send(&daemon, chat.id, "Read hello.txt and tell me what it says.");
    wait_turns(&panel, cx, 1);
    send(&daemon, chat.id, "Say it again briefly.");
    wait_turns(&panel, cx, 2);
    settle(cx);

    let insight = panel.read_with(cx, |p, _| p.insight().cloned()).unwrap();
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window, context.exactness),
        (11_200, Some(200_000), Exactness::Reported)
    );
    assert_eq!(model::meter_text(context), "11k / 200k · 5 %");
    // The panel shows what the daemon holds, block by block.
    let totals = model::totals_pairs(&insight);
    let get = |label: &str| {
        totals
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("Input"), Some("9.4k"));
    assert_eq!(get("Output"), Some("64"));
    assert_eq!(get("Cache read"), Some("24k"));
    assert_eq!(get("Cache write"), None, "OMP reported none: absent, not 0");
    assert_eq!(get("Cost"), Some("≈ $0.04 est."));
    assert_eq!(get("Cache hit"), Some("72 %"));
    assert_eq!(model::tools_summary(&insight).as_deref(), Some("1 call"));
    assert_eq!(insight.unsupported, [InsightField::Reasoning]);
    assert_eq!(
        model::unsupported_note("omp", &insight.unsupported).as_deref(),
        Some("OMP does not report reasoning tokens.")
    );

    // The turn rows: newest first, with what each turn used.
    let rows = panel.read_with(cx, |p, _| p.turn_lines().to_vec());
    let labels: Vec<_> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["T2", "T1"]);
    assert_eq!(rows[1].tools.as_deref(), Some("1 tool call"));
    assert_eq!(rows[1].tokens.as_deref(), Some("8.2k in · 52 out"));
    assert_eq!(rows[1].cost.as_deref(), Some("$0.03"));
    assert_eq!(rows[0].tools, None, "no tool call in the second turn");
    // The first lines of the prompts come from the transcript the window holds.
    wait_until(cx, "the prompts to be read", |cx| {
        panel.read_with(cx, |p, _| p.turn_lines().iter().all(|r| r.prompt.is_some()))
    });
    let rows = panel.read_with(cx, |p, _| p.turn_lines().to_vec());
    assert_eq!(
        rows[1].prompt.as_deref(),
        Some("Read hello.txt and tell me what it says.")
    );
    assert_eq!(rows[0].prompt.as_deref(), Some("Say it again briefly."));
    for region in [
        Region::Context("meter"),
        Region::Context("totals"),
        Region::Context("tools"),
        Region::ContextTurn(0),
        Region::ContextTurn(1),
    ] {
        wait_region(&view, cx, region);
    }
}

#[gpui::test]
fn measure_context_sends_slash_context_and_draws_the_breakdown(cx: &mut TestAppContext) {
    let daemon = acp_daemon("omp", "insight-omp");
    let work = tempfile::tempdir().unwrap();
    let chat = create_chat(&daemon, "omp", work.path(), "Migrate auth");
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, chat.id);
    let panel = panel(&view, cx);
    send(&daemon, chat.id, "Read hello.txt and tell me what it says.");
    wait_turns(&panel, cx, 1);
    send(&daemon, chat.id, "Say it again briefly.");
    wait_turns(&panel, cx, 2);
    wait_until(cx, "no turn to run", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(chat.id)
                .and_then(|s| s.chat.as_ref())
                .is_some_and(|c| !c.turn_active)
        })
    });
    assert!(
        panel.read_with(cx, |p, _| p
            .insight()
            .and_then(|i| i.context.as_ref())
            .is_some_and(|c| c.breakdown.is_empty())),
        "nothing is measured until the button is pressed"
    );
    click(&view, cx, Region::Context("measure"));
    wait_until(cx, "the breakdown", |cx| {
        panel.read_with(cx, |p, _| {
            p.insight()
                .and_then(|i| i.context.as_ref())
                .is_some_and(|c| !c.breakdown.is_empty())
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.measure_state().clone()),
        ContextMeasure::Idle
    );
    let context = panel
        .read_with(cx, |p, _| p.insight().and_then(|i| i.context.clone()))
        .unwrap();
    let breakdown = model::breakdown(&context).expect("a breakdown");
    let words: Vec<_> = breakdown
        .lines
        .iter()
        .map(|l| (l.label.as_str(), l.tokens_text.as_str()))
        .collect();
    assert_eq!(
        words,
        [
            ("System prompt", "2.6k"),
            ("System tools", "4k"),
            ("System context", "113"),
            ("Messages", "4.6k"),
            ("Auto-compact buf", "30k"),
            ("Free", "159k"),
        ]
    );
    assert_eq!(context.used, 11_200, "the harness's own figure stays");
    assert_eq!(context.compact_at, Some(170_000));
    wait_region(&view, cx, Region::Context("breakdown"));
    // The button now says what pressing it does next.
    wait_region(&view, cx, Region::Context("measure"));
    // `/context` is not a turn of the conversation.
    assert_eq!(turns_of(&panel, cx), 2);
    // The journal has it (the exchange is there for `hutan chat log`), the chat view does not
    // show it: no `/context` bubble, no table, and no Fork… or Rewind… footer for it.
    wait_until(cx, "the measurement's footer", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(chat.id).is_some_and(|m| {
                let t = m.read(cx).transcript();
                t.entries().iter().any(
                    |e| matches!(&e.body, hutan_chat_view::Body::User(m) if m.text == "/context"),
                ) && matches!(
                    t.entries().last().map(|e| &e.body),
                    Some(hutan_chat_view::Body::Footer(_))
                )
            })
        })
    });
    let (hidden, shown_footer, measured_footer) = view.read_with(cx, |wb, cx| {
        let model = wb.chat_model_of(chat.id).unwrap();
        let t = model.read(cx).transcript();
        let entries = t.entries();
        let start = entries
            .iter()
            .position(|e| matches!(&e.body, hutan_chat_view::Body::User(m) if m.text == "/context"))
            .unwrap();
        let footers: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e.body, hutan_chat_view::Body::Footer(_)))
            .map(|(at, _)| at)
            .collect();
        (
            entries[start..].iter().all(|e| t.is_measurement(e)),
            footers[footers.len() - 2],
            footers[footers.len() - 1],
        )
    });
    assert!(
        hidden,
        "the command, the table and its footer are one measurement"
    );
    assert!(
        view.read_with(cx, |wb, cx| {
            let model = wb.chat_model_of(chat.id).unwrap();
            let t = model.read(cx);
            !t.transcript()
                .is_measurement(&t.transcript().entries()[shown_footer])
        }),
        "the turn before it is an ordinary one"
    );
    // The footer of the last real turn offers Fork…; the measurement's has no footer at all.
    wait_region(&view, cx, Region::ChatFork(shown_footer));
    assert!(bounds(&view, cx, Region::ChatFork(measured_footer)).is_none());
    assert!(bounds(&view, cx, Region::ChatRewind(measured_footer)).is_none());
    assert!(bounds(&view, cx, Region::ChatCheckpoint(measured_footer)).is_none());
    // "The latest turn" of the window — what the sidebar's ⑂, the popover's default and ⌘K read
    // — skips the measurement: turn 2 of 2, not the hidden `/context` turn.
    let ended = view.read_with(cx, |wb, cx| {
        let model = wb.chat_model_of(chat.id).unwrap();
        let model = model.read(cx);
        hutan_desktop::workbench::chat::ended_turns(model.transcript(), !model.has_earlier())
    });
    assert_eq!(ended.len(), 2, "{ended:?}");
    assert_eq!(ended.last().map(|e| e.ordinal), Some(Some(2)));
    assert_eq!(ended.last().map(|e| e.newest), Some(true));
    assert!(ended.iter().all(|e| e.footer < measured_footer));
}

// ---- Claude Code ------------------------------------------------------------------------------

#[gpui::test]
fn a_claude_chat_shows_cache_writes_files_a_failed_tool_and_an_estimated_breakdown(
    cx: &mut TestAppContext,
) {
    let daemon = acp_daemon("claude", "insight-claude");
    let work = tempfile::tempdir().unwrap();
    let chat = create_chat(&daemon, "claude", work.path(), "Fix it");
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, chat.id);
    let panel = panel(&view, cx);
    send(&daemon, chat.id, "Fix it.");
    wait_turns(&panel, cx, 1);
    wait_until(cx, "no turn to run", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .session(chat.id)
                .and_then(|s| s.chat.as_ref())
                .is_some_and(|c| !c.turn_active)
        })
    });
    let insight = panel.read_with(cx, |p, _| p.insight().cloned()).unwrap();
    let totals = model::totals_pairs(&insight);
    let get = |label: &str| {
        totals
            .iter()
            .find(|(l, _)| l == label)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("Cache write").as_deref(), Some("9k"));
    assert_eq!(get("Cost").as_deref(), Some("≈ $0.31 est."));
    assert_eq!(
        model::tools_summary(&insight).as_deref(),
        Some("2 calls · 1 failed")
    );
    let files = model::file_lines(&insight);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/tmp/hutan-fixture/src/lib.rs");
    assert_eq!(files[0].stat.as_deref(), Some("+2 −1"));
    assert_eq!(
        model::files_summary(&insight).as_deref(),
        Some("1 touched · +2 −1")
    );
    wait_region(&view, cx, Region::Context("files"));

    // The breakdown Claude Code prints is rounded by the harness: it is written as an estimate.
    click(&view, cx, Region::Context("measure"));
    wait_until(cx, "the breakdown", |cx| {
        panel.read_with(cx, |p, _| {
            p.insight()
                .and_then(|i| i.context.as_ref())
                .is_some_and(|c| !c.breakdown.is_empty())
        })
    });
    let context = panel
        .read_with(cx, |p, _| p.insight().and_then(|i| i.context.clone()))
        .unwrap();
    assert_eq!(context.breakdown_exactness, Some(Exactness::Estimated));
    let breakdown = model::breakdown(&context).expect("a breakdown");
    assert!(breakdown.estimated);
    assert_eq!(model::meter_text(&context), "42k / 200k · 21 %");
}

// ---- Codex ----------------------------------------------------------------------------------------

#[gpui::test]
fn a_codex_chat_shows_tokens_not_cost_and_offers_no_breakdown(cx: &mut TestAppContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("plain-turn"));
    let chat = env.cwd();
    let chat = {
        let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: chat,
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some("Fix the flaky test".to_owned()),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }));
        created.session
    };
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, chat.id);
    let panel = panel(&view, cx);
    send(
        &daemon,
        chat.id,
        "Reply with the single word pong and nothing else.",
    );
    wait_turns(&panel, cx, 1);
    settle(cx);
    let insight = panel.read_with(cx, |p, _| p.insight().cloned()).unwrap();
    let context = insight.context.as_ref().expect("a context");
    // The latest request's size (Codex's `last`), not the thread's running total.
    assert_eq!(model::meter_text(context), "22k / 475k · 4 %");
    assert_eq!(
        insight.unsupported,
        [InsightField::Breakdown, InsightField::Cost]
    );
    assert!(!insight.can_measure);
    let totals = model::totals_pairs(&insight);
    assert_eq!(
        totals.last(),
        Some(&("Cost".to_owned(), "not reported by Codex".to_owned())),
        "Codex reports tokens, not cost: said, not drawn as $0.00"
    );
    assert_eq!(
        model::unsupported_note("codex", &insight.unsupported).as_deref(),
        Some("Codex does not report the context by category or cost.")
    );
    wait_region(&view, cx, Region::Context("notes"));
    wait_region(&view, cx, Region::Context("meter"));
    assert!(
        bounds(&view, cx, Region::Context("measure")).is_none(),
        "no Measure context for a harness that has no /context"
    );
    // No breakdown block is drawn for it either.
    assert!(bounds(&view, cx, Region::Context("breakdown")).is_none());
}

// ---- the timeline: Fork… and Rewind… on a turn's row --------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A Codex chat in a git repository whose two turns are held until the test says go, so each
/// turn can change files as the agent would: the first edits `a.txt`, the second changes none.
struct Timeline {
    env: ChatEnv,
    daemon: TestDaemon,
    session: SessionInfo,
    root: PathBuf,
    gates: Vec<PathBuf>,
}

fn timeline() -> Timeline {
    let env = ChatEnv::new();
    let base = env.cwd();
    git(&base, &["init", "-q", "-b", "main", "repo"]);
    let root = base.join("repo");
    std::fs::write(root.join("a.txt"), "alpha\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let gates: Vec<PathBuf> = (0..2).map(|n| base.join(format!("gate-{n}"))).collect();
    let held = common::chat::held_turns(&base, &gates);
    let script = base.join("script");
    std::fs::create_dir_all(&script).unwrap();
    std::fs::copy(&held, script.join("00-held.jsonl")).unwrap();
    let daemon = TestDaemon::start_with_codex_tweaked(env.replay(&script), |c| {
        c.checkpoints.git_env = [
            ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ]
        .into();
    });
    let session = create_chat(&daemon, "codex", &root, "Auth work");
    Timeline {
        env,
        daemon,
        session,
        root,
        gates,
    }
}

impl Timeline {
    /// Turn `n`: the agent runs while `edit` changes files, then the turn ends and the panel
    /// lists it.
    fn turn(
        &self,
        panel: &Entity<ContextPanel>,
        cx: &mut VisualTestContext,
        n: usize,
        edit: impl FnOnce(&Path),
    ) {
        send(&self.daemon, self.session.id, &format!("turn {n}"));
        edit(&self.root);
        std::fs::write(&self.gates[n], "").unwrap();
        wait_turns(panel, cx, n as u64 + 1);
        wait_until(cx, "no turn to run", |cx| {
            panel.read_with(cx, |p, cx| {
                p.fork_gate(cx) == hutan_desktop::workbench::panels::ContextForkGate::Ready
            })
        });
    }
}

#[gpui::test]
fn a_turn_row_forks_at_its_turn_and_rewinds_the_files_it_changed(cx: &mut TestAppContext) {
    let t = timeline();
    let (view, cx) = open(cx, &t.daemon);
    show_chat(&view, cx, t.session.id);
    let panel = panel(&view, cx);
    t.turn(&panel, cx, 0, |root| {
        std::fs::write(root.join("a.txt"), "alpha t1\n").unwrap();
    });
    t.turn(&panel, cx, 1, |_| {});
    let rows = panel.read_with(cx, |p, _| p.turn_lines().to_vec());
    assert_eq!(rows.len(), 2);
    // The fixture's turns report no file change of their own (the files here are the
    // checkpoint's business), so the rows say none.
    assert!(rows.iter().all(|r| r.files.is_none()));
    wait_region(&view, cx, Region::ContextTurnAction(0, "fork"));
    wait_region(&view, cx, Region::ContextTurnAction(1, "fork"));
    // Rewind… only where the turn changed files and has a checkpoint to restore.
    wait_region(&view, cx, Region::ContextTurnAction(1, "rewind"));
    assert!(
        bounds(&view, cx, Region::ContextTurnAction(0, "rewind")).is_none(),
        "a turn that changed nothing has nothing to rewind"
    );

    // Fork… on the first turn's row opens CHAT-FORK-B's popover at that turn.
    click(&view, cx, Region::ContextTurnAction(1, "fork"));
    wait_until(cx, "the fork popover", |cx| {
        view.read_with(cx, |wb, _| wb.fork_popover_session()) == Some(t.session.id)
    });
    wait_region(&view, cx, Region::Fork("fork"));
    settle(cx);
    let said = view.read_with(cx, |wb, cx| wb.fork_popover_lines(cx));
    assert_eq!(said[0], "title: Fork from turn 1", "{said:#?}");
    cx.simulate_keystrokes("escape");
    draw(cx);
    wait_until(cx, "the popover to close", |cx| {
        view.read_with(cx, |wb, _| wb.fork_popover_session().is_none())
    });

    // Rewind… opens the preview of that turn's checkpoint.
    click(&view, cx, Region::ContextTurnAction(1, "rewind"));
    wait_until(cx, "the rewind preview", |cx| {
        view.read_with(cx, |wb, _| wb.rewind_sheet().is_some())
    });
    let _ = &t.env;
}

// ---- following, and the honest empty states ----------------------------------------------------

fn wait_empty(
    panel: &Entity<ContextPanel>,
    cx: &mut VisualTestContext,
    what: &str,
    mut check: impl FnMut(Option<&ContextEmpty>) -> bool,
) {
    wait_until(cx, what, |cx| panel.read_with(cx, |p, _| check(p.empty())));
}

#[gpui::test]
fn the_panel_follows_the_session_in_view_and_says_why_it_has_nothing_to_show(
    cx: &mut TestAppContext,
) {
    let daemon = acp_daemon("omp", "insight-omp");
    let work = tempfile::tempdir().unwrap();
    let chat = create_chat(&daemon, "omp", work.path(), "Migrate auth");
    let shell = daemon.create_session(&["sh", "-c", "exec cat"], None);
    let agent = daemon.create_session(&["sh", "-c", "exec cat"], None);
    let _: hutan_protocol::messages::AgentApplied = daemon.call(Request::AgentReport(
        hutan_protocol::messages::AgentReportRequest {
            pid: None,
            session_id: agent.id,
            source: "test-hooks".into(),
            agent: "omp".into(),
            state: Some(hutan_protocol::messages::AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        },
    ));
    let (view, cx) = open(cx, &daemon);
    let panel = panel(&view, cx);
    // No session in view yet.
    wait_empty(&panel, cx, "the no-session state", |e| {
        e == Some(&ContextEmpty::NoSession)
    });
    assert_eq!(panel.read_with(cx, |p, _| p.session()), None);
    wait_region(&view, cx, Region::Context("empty"));

    // A shell is no agent.
    show_chat(&view, cx, shell.id);
    wait_empty(&panel, cx, "the shell's state", |e| {
        e == Some(&ContextEmpty::NotAnAgent)
    });
    assert_eq!(panel.read_with(cx, |p, _| p.session()), Some(shell.id));

    // A terminal with an agent: its state is all Hutan has; no Measure, no turns, no Fork.
    show_chat(&view, cx, agent.id);
    wait_empty(
        &panel,
        cx,
        "the terminal agent's state",
        |e| matches!(e, Some(ContextEmpty::Terminal { app: Some(app), .. }) if app == "omp"),
    );
    assert_eq!(panel.read_with(cx, |p, _| p.session()), Some(agent.id));
    assert!(bounds(&view, cx, Region::Context("measure")).is_none());
    assert!(panel.read_with(cx, |p, _| p.turn_lines().is_empty()));

    // A chat: the panel moves to it and reads it.
    show_chat(&view, cx, chat.id);
    wait_until(cx, "the chat's insight", |cx| {
        panel.read_with(cx, |p, _| {
            p.session() == Some(chat.id) && *p.load() == ContextLoad::Ready
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.empty().cloned()),
        Some(ContextEmpty::Nothing),
        "a chat nothing has reported on says so"
    );
}

#[gpui::test]
fn the_context_panel_is_on_the_right_rail_after_changes_and_follows_the_layout(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let rail = view.read_with(cx, |wb, _| {
        wb.state()
            .docks
            .right
            .panels
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    });
    let at = |name: &str| rail.iter().position(|p| p == name).unwrap();
    assert_eq!(at("context"), at("changes") + 1, "{rail:?}");
    assert!(
        view.read_with(cx, |wb, _| wb
            .panel_registry()
            .get(&hutan_desktop::workbench::model::PanelId::new("context"))
            .is_some()),
        "a registered panel"
    );
    // The panel does nothing while it is not on screen.
    let shown = view.read_with(cx, |wb, _| wb.panel::<ContextPanel>("context"));
    assert!(shown.is_none_or(|p| !p.read_with(cx, |p, _| p.is_visible())));
}

#[gpui::test]
fn a_file_row_opens_the_file_and_has_no_diff_outside_a_checkout(cx: &mut TestAppContext) {
    // The recorded Claude exchange edits an absolute path under /tmp/hutan-fixture.
    let daemon = acp_daemon("claude", "insight-claude");
    let work = tempfile::tempdir().unwrap();
    let chat = create_chat(&daemon, "claude", work.path(), "Fix it");
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, chat.id);
    let panel = panel(&view, cx);
    send(&daemon, chat.id, "Fix it.");
    wait_turns(&panel, cx, 1);
    let path = PathBuf::from("/tmp/hutan-fixture/src/lib.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "fn main() {}\n").unwrap();
    wait_region(&view, cx, Region::ContextFile(0, "open"));
    assert!(
        bounds(&view, cx, Region::ContextFile(0, "diff")).is_none(),
        "the chat's folder is no git checkout: no diff to show"
    );
    click(&view, cx, Region::ContextFile(0, "open"));
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .active_center_item()
                .and_then(|item| {
                    item.as_file()
                        .map(|p| p.ends_with("hutan-fixture/src/lib.rs"))
                })
                .unwrap_or(false)
        })
    });
    std::fs::remove_file(&path).ok();
}
