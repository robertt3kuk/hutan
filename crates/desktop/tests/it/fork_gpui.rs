#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! CHAT-FORK-B: fork a chat from any turn (protocol 0.56 / 0.58, ADR 0023), seen from the window
//! against an in-process daemon whose Codex and ACP harnesses replay recorded transcripts and
//! whose terminal agents are stand-in scripts: *Fork…* in the turn footer, the popover per
//! support level (exact, head, summary only) with the choice a fallback asks for, *Where* with
//! the destination remembered, the branch, the paths a checkpoint left out, *Restore files* and
//! *Archive the original*, the message hover bar, the sidebar row's ⑂, the session menu, ⌘K
//! *Chat: Fork from Here*, the fork under its parent in the sidebar and the header chip that
//! opens the parent, and *Fork Session* typed through a terminal agent's own CLI.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::chat::{ChatEnv, fixtures, held_turns};
use common::history::HistoryEnv;
use common::{TestDaemon, click_at, draw, open_with, settle, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use hutan_chat_view::Body;
use hutan_core::{ForkMode, SessionId, WorkspaceId};
use hutan_desktop::workbench::chat::LoadStatus;
use hutan_desktop::workbench::panels::{RowKind, SessionsPanel, SidebarRow};
use hutan_desktop::workbench::{Region, Workbench, actions};
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Request, SessionInfo, SessionResult,
};
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::Value;

/// What the recorded transcripts say the user asked, whatever the test sends.
const PROMPT: &str = "Reply with the single word pong and nothing else.";

// ---- harness ----------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
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

fn git_env() -> std::collections::BTreeMap<String, String> {
    [
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ]
    .into()
}

/// `<dir>/repo`: a repository on `main` with `a.txt` and `b.txt`.
fn repository(dir: &Path) -> PathBuf {
    git(dir, &["init", "-q", "-b", "main", "repo"]);
    let root = dir.join("repo");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    fs::write(root.join("b.txt"), "beta\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

fn create_chat(
    daemon: &TestDaemon,
    agent: &str,
    cwd: &Path,
    workspace: Option<WorkspaceId>,
    title: &str,
) -> SessionInfo {
    let created: SessionResult = daemon.call(Request::ChatCreate(ChatCreate {
        extra_dirs: Vec::new(),
        contain: None,
        workspace_id: workspace,
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

fn show_chat(view: &Entity<Workbench>, cx: &mut VisualTestContext, session: SessionId) {
    view.update_in(cx, |wb, window, cx| wb.open_session(session, window, cx));
    wait_until(cx, "the chat to load", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.chat_model_of(session)
                .is_some_and(|m| *m.read(cx).status() == LoadStatus::Ready)
                && wb.chat_view_of(session).is_some()
        })
    });
    // The frame on screen is this chat's: the regions of the one shown before are gone.
    settle(cx);
}

fn bounds_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn wait_region(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_until(cx, &format!("{region:?} to be drawn"), |cx| {
        bounds_of(view, cx, region).is_some()
    });
}

/// Waits for the control, then clicks the frame on screen's bounds of it.
fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region) {
    wait_region(view, cx, region);
    let bounds = bounds_of(view, cx, region).unwrap();
    click_at(cx, bounds.center());
    draw(cx);
}

fn footers(view: &Entity<Workbench>, cx: &VisualTestContext, session: SessionId) -> Vec<usize> {
    view.read_with(cx, |wb, cx| {
        wb.chat_model_of(session)
            .map(|m| {
                m.read(cx)
                    .transcript()
                    .entries()
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches!(e.body, Body::Footer(_)))
                    .map(|(at, _)| at)
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// The entries of the user's messages.
fn user_entries(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    session: SessionId,
) -> Vec<usize> {
    view.read_with(cx, |wb, cx| {
        wb.chat_model_of(session)
            .map(|m| {
                m.read(cx)
                    .transcript()
                    .entries()
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches!(e.body, Body::User(_)))
                    .map(|(at, _)| at)
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn info(view: &Entity<Workbench>, cx: &VisualTestContext, id: SessionId) -> Option<SessionInfo> {
    view.read_with(cx, |wb, cx| wb.sessions().read(cx).session(id).cloned())
}

/// Waits until the daemon says no turn runs in the chat (the *Fork…* buttons are greyed while
/// one does).
fn idle(view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId) {
    wait_until(cx, "no turn in progress", |cx| {
        info(view, cx, id)
            .and_then(|s| s.chat)
            .is_some_and(|chat| !chat.turn_active)
    });
}

/// Sends a message and waits for its footer.
fn say(
    daemon: &TestDaemon,
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
    text: &str,
) {
    let before = footers(view, cx, session).len();
    let _: ChatSent = daemon.call(Request::ChatSend(ChatSend {
        session_id: session,
        text: text.to_owned(),
        attachments: vec![],
    }));
    wait_until(cx, "the turn's footer", |cx| {
        footers(view, cx, session).len() > before
    });
    idle(view, cx, session);
}

fn lines(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| wb.fork_popover_lines(cx))
}

fn has(lines: &[String], prefix: &str) -> bool {
    lines.iter().any(|line| line.starts_with(prefix))
}

fn line<'a>(lines: &'a [String], prefix: &str) -> &'a str {
    lines
        .iter()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line `{prefix}…` in {lines:#?}"))
}

/// Waits for the popover of `session` to be open and to have slid in.
fn popover(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    session: SessionId,
) -> Vec<String> {
    wait_until(cx, "the fork popover", |cx| {
        view.read_with(cx, |wb, _| wb.fork_popover_session()) == Some(session)
    });
    wait_region(view, cx, Region::Fork("fork"));
    settle(cx);
    lines(view, cx)
}

fn dismiss(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("escape");
    draw(cx);
    wait_until(cx, "the popover to close", |cx| {
        view.read_with(cx, |wb, _| wb.fork_popover_session().is_none())
    });
}

/// The fork of `parent` once the window lists it.
fn fork_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    parent: SessionId,
    known: usize,
) -> SessionInfo {
    wait_until(cx, "the fork to be listed", |cx| {
        forks_of(view, cx, parent).len() > known
    });
    forks_of(view, cx, parent).pop().unwrap()
}

fn forks_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    parent: SessionId,
) -> Vec<SessionInfo> {
    let mut forks: Vec<SessionInfo> = view.read_with(cx, |wb, cx| {
        wb.sessions()
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| {
                s.forked_from
                    .as_ref()
                    .is_some_and(|o| o.session_id == parent)
            })
            .cloned()
            .collect()
    });
    forks.sort_by_key(|s| s.created_at_ms);
    forks
}

fn shown(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<SessionId> {
    view.read_with(cx, |wb, _| wb.shown_session())
}

fn rows(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Vec<SidebarRow> {
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<SessionsPanel>("sessions"))
        .expect("the sessions panel is drawn in the left dock");
    panel.read_with(cx, |p, cx| p.visible_rows(cx))
}

fn notice(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, _| {
        wb.notice().map(|n| n.to_string()).unwrap_or_default()
    })
}

// ---- Codex (exact) ----------------------------------------------------------------------------

/// A Codex chat whose turns wait on gates, then one `fork-turn` transcript per fork.
struct Codex {
    env: ChatEnv,
    daemon: TestDaemon,
    session: SessionInfo,
    root: PathBuf,
    gates: Vec<PathBuf>,
}

fn codex(turns: usize, forks: usize, in_git: bool) -> Codex {
    let env = ChatEnv::new();
    let base = env.cwd();
    let root = if in_git {
        repository(&base)
    } else {
        base.clone()
    };
    let gates: Vec<PathBuf> = (0..turns).map(|n| base.join(format!("gate-{n}"))).collect();
    let held = held_turns(&base, &gates);
    let script = base.join("script");
    fs::create_dir_all(&script).unwrap();
    fs::copy(&held, script.join("00-held.jsonl")).unwrap();
    for n in 0..forks {
        fs::copy(
            fixtures().join("fork-turn.jsonl"),
            script.join(format!("{:02}-fork.jsonl", n + 1)),
        )
        .unwrap();
    }
    let daemon = TestDaemon::start_with_codex_tweaked(env.replay(&script), |c| {
        c.checkpoints.git_env = git_env();
    });
    let workspace = daemon.create_workspace("repo", &[&root]);
    let session = create_chat(&daemon, "codex", &root, Some(workspace.id), "Auth work");
    Codex {
        env,
        daemon,
        session,
        root,
        gates,
    }
}

impl Codex {
    /// Turn `n`: the agent runs while `edit` changes files, then the turn ends.
    fn turn(
        &self,
        view: &Entity<Workbench>,
        cx: &mut VisualTestContext,
        n: usize,
        edit: impl FnOnce(&Path),
    ) {
        let before = footers(view, cx, self.session.id).len();
        let _: ChatSent = self.daemon.call(Request::ChatSend(ChatSend {
            session_id: self.session.id,
            text: format!("turn {n}"),
            attachments: vec![],
        }));
        edit(&self.root);
        fs::write(&self.gates[n], "").unwrap();
        wait_until(cx, "the turn's footer", |cx| {
            footers(view, cx, self.session.id).len() > before
        });
        idle(view, cx, self.session.id);
    }
}

#[gpui::test]
fn a_codex_chat_forks_from_a_footer_here_into_a_worktree_and_with_its_files_restored(
    cx: &mut TestAppContext,
) {
    let s = codex(2, 4, true);
    let parent = s.session.id;
    let (view, cx) = open(cx, &s.daemon);
    show_chat(&view, cx, parent);
    // Turn 1 edits, deletes and adds (one secret-named file a checkpoint leaves out); turn 2
    // edits a.txt again.
    s.turn(&view, cx, 0, |root| {
        fs::write(root.join("a.txt"), "alpha t1\n").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("new.txt"), "new t1\n").unwrap();
        fs::write(root.join(".env.local"), "TOKEN=1\n").unwrap();
    });
    s.turn(&view, cx, 1, |root| {
        fs::write(root.join("a.txt"), "alpha t2\n").unwrap();
    });
    let footer = footers(&view, cx, parent);
    assert_eq!(footer.len(), 2);
    // Fork… is on every turn's footer, with the turn's checkpoint line where there is one.
    wait_region(&view, cx, Region::ChatFork(footer[0]));
    wait_region(&view, cx, Region::ChatFork(footer[1]));
    wait_region(&view, cx, Region::ChatRewind(footer[0]));

    // ---- this folder: exact, nothing else asked --------------------------------------------
    click(&view, cx, Region::ChatFork(footer[0]));
    let said = popover(&view, cx, parent);
    assert_eq!(said[0], "title: Fork from turn 1", "{said:#?}");
    assert_eq!(line(&said, "quote:"), format!("quote: “{PROMPT}”"));
    assert_eq!(line(&said, "where:"), "where: this folder");
    assert_eq!(
        line(&said, "cut:"),
        "cut: Exact fork · Codex keeps turn 1; the original is not changed"
    );
    assert!(
        has(&said, "option: Restore files to the end of turn 1 [off]"),
        "{said:#?}"
    );
    assert!(
        has(&said, "option: Archive the original [off]"),
        "{said:#?}"
    );
    assert_eq!(line(&said, "button:"), "button: Fork");
    click(&view, cx, Region::Fork("fork"));
    let first = fork_of(&view, cx, parent, 0);
    let origin = first.forked_from.clone().unwrap();
    assert_eq!((origin.session_id, origin.ordinal), (parent, 1));
    assert_eq!(origin.mode, ForkMode::Exact);
    assert_eq!(first.cwd.as_deref(), Some(s.root.as_path()));
    wait_until(&mut *cx, "the fork is shown", |cx| {
        shown(&view, cx) == Some(first.id)
    });
    assert!(view.read_with(cx, |wb, _| wb.fork_popover_session().is_none()));
    assert!(
        notice(&view, cx).starts_with("Forked at turn 1"),
        "{}",
        notice(&view, cx)
    );
    // Codex was asked to cut its thread at the turn's own id.
    let forks: Vec<Value> = s
        .env
        .received()
        .into_iter()
        .filter(|(method, _)| method == "thread/fork")
        .map(|(_, msg)| msg)
        .collect();
    assert_eq!(forks.len(), 1);
    assert!(
        forks[0]["params"]["lastTurnId"].is_string(),
        "{:?}",
        forks[0]
    );

    // ---- the header chip names the parent and its turn, and opens it ------------------------
    show_chat(&view, cx, first.id);
    wait_region(&view, cx, Region::Chat("forked-from"));
    click(&view, cx, Region::Chat("forked-from"));
    wait_until(&mut *cx, "the parent is shown", |cx| {
        shown(&view, cx) == Some(parent)
    });

    // ---- lineage: the fork's row hangs under its parent -------------------------------------
    let listed = rows(&view, cx);
    let at = listed
        .iter()
        .position(|r| r.kind == RowKind::Session && r.title == "Auth work")
        .unwrap_or_else(|| panic!("no parent row in {listed:?}"));
    assert_eq!(listed[at + 1].title, first.title, "{listed:?}");
    assert_eq!(listed[at + 1].depth, listed[at].depth + 1, "{listed:?}");

    // ---- a new worktree: the remembered destination, the branch, what was left out ----------
    show_chat(&view, cx, parent);
    click(&view, cx, Region::ChatFork(footer[0]));
    let said = popover(&view, cx, parent);
    assert_eq!(
        line(&said, "where:"),
        "where: this folder",
        "not chosen yet"
    );
    click(&view, cx, Region::Fork("where-worktree"));
    let said = lines(&view, cx);
    assert_eq!(line(&said, "where:"), "where: new worktree");
    assert!(
        line(&said, "worktree:").contains("files as turn 1 left them"),
        "{said:#?}"
    );
    assert!(
        has(&said, "left out: .env.local"),
        "the secret-named file the checkpoint left out is listed: {said:#?}"
    );
    assert!(
        !has(&said, "option: Restore files"),
        "restoring is for this folder: {said:#?}"
    );
    wait_region(&view, cx, Region::Fork("branch"));
    cx.simulate_input("fork/try-t1");
    draw(cx);
    assert_eq!(line(&lines(&view, cx), "branch:"), "branch: fork/try-t1");
    click(&view, cx, Region::Fork("fork"));
    let second = fork_of(&view, cx, parent, 1);
    let worktree = second.cwd.clone().expect("the fork works in a folder");
    assert_ne!(worktree, s.root);
    assert!(
        git(&s.root, &["worktree", "list", "--porcelain"]).contains("refs/heads/fork/try-t1"),
        "the branch exists"
    );
    // The files of turn 1, not the source checkout's: edited, deleted, added, secret left out.
    assert_eq!(
        fs::read_to_string(worktree.join("a.txt")).unwrap(),
        "alpha t1\n"
    );
    assert!(!worktree.join("b.txt").exists());
    assert_eq!(
        fs::read_to_string(worktree.join("new.txt")).unwrap(),
        "new t1\n"
    );
    assert!(!worktree.join(".env.local").exists());
    assert_eq!(
        fs::read_to_string(s.root.join("a.txt")).unwrap(),
        "alpha t2\n"
    );
    wait_until(&mut *cx, "the worktree fork is shown", |cx| {
        shown(&view, cx) == Some(second.id)
    });
    let said = notice(&view, cx);
    assert!(said.contains("into worktree fork/try-t1"), "{said}");
    assert!(said.contains("left out"), "{said}");
    // The destination is remembered for the next fork.
    let settings = fs::read_to_string(s.daemon.home().join("settings.toml")).unwrap();
    assert!(
        settings.contains("fork_where = \"new_worktree\""),
        "{settings}"
    );
    show_chat(&view, cx, parent);
    click(&view, cx, Region::ChatFork(footer[1]));
    let said = popover(&view, cx, parent);
    assert_eq!(line(&said, "where:"), "where: new worktree");

    // ---- this folder again, with the files of the turn put back and the original archived ---
    click(&view, cx, Region::Fork("where-folder"));
    let said = lines(&view, cx);
    assert!(
        has(
            &said,
            "option: Restore files to the end of turn 2 [unavailable]"
        ),
        "the latest turn has nothing after it to put back: {said:#?}"
    );
    dismiss(&view, cx);
    click(&view, cx, Region::ChatFork(footer[0]));
    popover(&view, cx, parent);
    click(&view, cx, Region::Fork("where-folder"));
    click(&view, cx, Region::Fork("restore"));
    click(&view, cx, Region::Fork("archive"));
    let said = lines(&view, cx);
    assert!(
        has(&said, "option: Restore files to the end of turn 1 [on]"),
        "{said:#?}"
    );
    assert!(has(&said, "option: Archive the original [on]"), "{said:#?}");
    click(&view, cx, Region::Fork("fork"));
    let third = fork_of(&view, cx, parent, 2);
    assert_eq!(third.forked_from.as_ref().unwrap().ordinal, 1);
    assert_eq!(
        fs::read_to_string(s.root.join("a.txt")).unwrap(),
        "alpha t1\n"
    );
    wait_until(&mut *cx, "the original to be archived", |cx| {
        info(&view, cx, parent).is_none()
    });
    let said = notice(&view, cx);
    assert!(said.contains("restored"), "{said}");
    assert!(said.contains("the original is archived"), "{said}");
}

#[gpui::test]
fn the_message_bar_the_menu_k_and_the_sidebar_row_all_reach_the_same_fork(cx: &mut TestAppContext) {
    let s = codex(2, 2, false);
    let parent = s.session.id;
    let (view, cx) = open(cx, &s.daemon);
    show_chat(&view, cx, parent);
    s.turn(&view, cx, 0, |_| {});
    s.turn(&view, cx, 1, |_| {});
    let users = user_entries(&view, cx, parent);
    assert_eq!(users.len(), 2);

    // The pointer on a user's message brings its bar: copy, fork (no rewind: nothing changed).
    wait_region(&view, cx, Region::ChatEntry(users[0]));
    let message = bounds_of(&view, cx, Region::ChatEntry(users[0])).unwrap();
    cx.simulate_mouse_move(message.center(), None, Modifiers::none());
    draw(cx);
    wait_region(&view, cx, Region::ChatMessageAction(users[0], "bar"));
    assert!(bounds_of(&view, cx, Region::ChatMessageAction(users[0], "rewind")).is_none());
    click(&view, cx, Region::ChatMessageAction(users[0], "copy"));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some(PROMPT)
    );
    click(&view, cx, Region::ChatMessageAction(users[0], "fork"));
    let said = popover(&view, cx, parent);
    assert_eq!(said[0], "title: Fork from turn 1", "{said:#?}");
    dismiss(&view, cx);

    // ⌘K: the turn the chat is at (it follows its end: the newest).
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("Fork from Here");
    assert!(view.read_with(cx, |wb, _| {
        wb.palette_titles()
            .iter()
            .any(|t| t.as_ref() == "Chat: Fork from Here")
    }));
    cx.simulate_keystrokes("enter");
    let said = popover(&view, cx, parent);
    assert_eq!(said[0], "title: Fork from turn 2", "{said:#?}");
    dismiss(&view, cx);

    // The session menu's Fork….
    view.update_in(cx, |_, window, cx| {
        window.dispatch_action(
            Box::new(actions::ShowSessionMenu(actions::SessionMenuAt {
                session: parent.to_string(),
                position: point(px(300.0), px(200.0)),
            })),
            cx,
        );
    });
    draw(cx);
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .unwrap_or_default()
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    let at = labels
        .iter()
        .position(|label| label == "Fork…")
        .unwrap_or_else(|| panic!("no Fork… in {labels:?}"));
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(at, window, cx));
    draw(cx);
    let said = popover(&view, cx, parent);
    assert_eq!(said[0], "title: Fork from turn 2", "{said:#?}");
    dismiss(&view, cx);

    // The sidebar row's ⑂ asks nothing of Codex in this folder: the fork is made and shown.
    wait_region(&view, cx, Region::SessionFork(parent));
    click(&view, cx, Region::SessionFork(parent));
    let child = fork_of(&view, cx, parent, 0);
    assert_eq!(child.forked_from.as_ref().unwrap().ordinal, 2);
    wait_until(&mut *cx, "the fork is shown", |cx| {
        shown(&view, cx) == Some(child.id)
    });
    assert!(view.read_with(cx, |wb, _| wb.fork_popover_session().is_none()));
}

// ---- ACP harnesses (head, summary only, a refusal) ---------------------------------------------

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

fn acp_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"));
    fs::read_to_string(path).unwrap()
}

/// A daemon whose `agent` replays one transcript per spawn: `scripts` are `(name, text)`.
fn acp_daemon(agent: &str, dir: &Path, scripts: &[(&str, String)]) -> TestDaemon {
    let scripted = dir.join("script");
    fs::create_dir_all(&scripted).unwrap();
    for (index, (name, text)) in scripts.iter().enumerate() {
        fs::write(
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
            text,
        )
        .unwrap();
    }
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.{agent}]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n",
        bin = acp_bin(),
        received = dir.join("received.jsonl"),
    ))
}

fn named(name: &str) -> (&str, String) {
    (name, acp_fixture(name))
}

#[gpui::test]
fn an_omp_chat_asks_for_the_cut_it_cannot_make_and_never_forks_without_the_answer(
    cx: &mut TestAppContext,
) {
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().canonicalize().unwrap();
    let daemon = acp_daemon(
        "omp",
        &cwd,
        &[named("two-turns"), named("fork-head"), named("plain-turn")],
    );
    let parent = create_chat(&daemon, "omp", &cwd, None, "Planner").id;
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, parent);
    say(&daemon, &view, cx, parent, "first");
    say(&daemon, &view, cx, parent, "second");
    let footer = footers(&view, cx, parent);
    wait_until(cx, "OMP's fork support", |cx| {
        info(&view, cx, parent)
            .and_then(|s| s.chat)
            .is_some_and(|chat| chat.fork.is_some())
    });

    // An earlier turn: OMP forks only at its newest, so the popover asks.
    click(&view, cx, Region::ChatFork(footer[0]));
    let said = popover(&view, cx, parent);
    assert_eq!(said[0], "title: Fork from turn 1", "{said:#?}");
    assert!(
        line(&said, "cut:").starts_with("cut: choose — omp forks only at its newest turn"),
        "{said:#?}"
    );
    assert_eq!(
        line(&said, "choice: Fork at the latest turn"),
        "choice: Fork at the latest turn"
    );
    assert!(
        has(&said, "choice: Fork with a summary of turn 1"),
        "{said:#?}"
    );
    assert_eq!(line(&said, "button:"), "button: Fork [disabled]");
    // Nothing happens without the answer.
    click(&view, cx, Region::Fork("fork"));
    settle(cx);
    assert!(forks_of(&view, cx, parent).is_empty());
    assert_eq!(
        view.read_with(cx, |wb, _| wb.fork_popover_session()),
        Some(parent)
    );
    // The latest turn, chosen: the conversation as it is now.
    click(&view, cx, Region::ForkCut(0));
    let said = lines(&view, cx);
    assert!(
        has(&said, "choice: Fork at the latest turn [chosen]"),
        "{said:#?}"
    );
    assert_eq!(line(&said, "button:"), "button: Fork at the latest turn");
    click(&view, cx, Region::Fork("fork"));
    let head = fork_of(&view, cx, parent, 0);
    let origin = head.forked_from.clone().unwrap();
    assert_eq!((origin.mode, origin.ordinal), (ForkMode::Head, 2));

    // The newest turn is a cut OMP can make.
    show_chat(&view, cx, parent);
    click(&view, cx, Region::ChatFork(footer[1]));
    let said = popover(&view, cx, parent);
    assert_eq!(
        line(&said, "cut:"),
        "cut: Exact fork · OMP keeps turns 1–2; the original is not changed"
    );
    assert_eq!(line(&said, "button:"), "button: Fork");
    dismiss(&view, cx);

    // A summary of the earlier turn is the other answer.
    click(&view, cx, Region::ChatFork(footer[0]));
    popover(&view, cx, parent);
    click(&view, cx, Region::ForkCut(1));
    assert_eq!(
        line(&lines(&view, cx), "button:"),
        "button: Fork with a summary"
    );
    click(&view, cx, Region::Fork("fork"));
    let summary = fork_of(&view, cx, parent, 1);
    let origin = summary.forked_from.clone().unwrap();
    assert_eq!((origin.mode, origin.ordinal), (ForkMode::Summary, 1));
    assert!(notice(&view, cx).starts_with("Forked with a summary of turn 1"));
}

#[gpui::test]
fn an_agent_that_cannot_fork_offers_a_summary_and_nothing_else(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let cwd = work.path().canonicalize().unwrap();
    // OMP's two turns, without `session/fork` among what it advertises.
    let without_fork = acp_fixture("two-turns").replace("\"fork\":{},", "");
    assert_ne!(without_fork, acp_fixture("two-turns"));
    let daemon = acp_daemon(
        "omp",
        &cwd,
        &[("two-turns-no-fork", without_fork), named("plain-turn")],
    );
    let parent = create_chat(&daemon, "omp", &cwd, None, "Planner").id;
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, parent);
    say(&daemon, &view, cx, parent, "first");
    say(&daemon, &view, cx, parent, "second");
    let footer = footers(&view, cx, parent);
    wait_until(cx, "the agent's fork support", |cx| {
        info(&view, cx, parent)
            .and_then(|s| s.chat)
            .is_some_and(|chat| chat.fork.is_some())
    });
    // Even the newest turn: there is no native fork at all.
    click(&view, cx, Region::ChatFork(footer[1]));
    let said = popover(&view, cx, parent);
    assert!(
        line(&said, "cut:").contains("does not offer session/fork"),
        "{said:#?}"
    );
    assert!(
        has(&said, "choice: Fork with a summary of turns 1–2"),
        "{said:#?}"
    );
    assert!(!has(&said, "choice: Fork at the latest turn"), "{said:#?}");
    assert_eq!(line(&said, "button:"), "button: Fork [disabled]");
    click(&view, cx, Region::ForkCut(0));
    click(&view, cx, Region::Fork("fork"));
    let fork = fork_of(&view, cx, parent, 0);
    assert_eq!(fork.forked_from.unwrap().mode, ForkMode::Summary);
}

#[gpui::test]
fn a_refusal_of_the_daemon_turns_into_the_choice_it_names(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let base = work.path().canonicalize().unwrap();
    let repo = repository(&base);
    let daemon = acp_daemon(
        "claude",
        &base,
        &[named("claude-two-turns"), named("claude-two-turns")],
    );
    let workspace = daemon.create_workspace("repo", &[&repo]);
    let parent = create_chat(&daemon, "claude", &repo, Some(workspace.id), "Claude work").id;
    let (view, cx) = open(cx, &daemon);
    show_chat(&view, cx, parent);
    say(&daemon, &view, cx, parent, "first");
    say(&daemon, &view, cx, parent, "second");
    let footer = footers(&view, cx, parent);
    wait_until(cx, "the adapter's fork support", |cx| {
        info(&view, cx, parent)
            .and_then(|s| s.chat)
            .is_some_and(|chat| chat.fork.is_some())
    });

    // Into a worktree, at the head the adapter can cut at: its transcript belongs to the old
    // folder, so the daemon refuses and names what would work.
    click(&view, cx, Region::ChatFork(footer[1]));
    popover(&view, cx, parent);
    click(&view, cx, Region::Fork("where-worktree"));
    click(&view, cx, Region::Fork("fork"));
    wait_until(cx, "the refusal", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.fork_popover_lines(cx)
                .iter()
                .any(|l| l.starts_with("cut: choose"))
        })
    });
    let said = lines(&view, cx);
    assert!(
        has(&said, "choice: Fork with a summary of turns 1–2"),
        "{said:#?}"
    );
    assert!(!has(&said, "choice: Fork at the latest turn"), "{said:#?}");
    assert_eq!(line(&said, "button:"), "button: Fork [disabled]");
    assert!(forks_of(&view, cx, parent).is_empty());
    assert!(
        git(&repo, &["worktree", "list", "--porcelain"])
            .lines()
            .count()
            <= 4,
        "no worktree was made by a refused fork"
    );
    // The user picks it; the worktree gets a new conversation.
    click(&view, cx, Region::ForkCut(0));
    click(&view, cx, Region::Fork("fork"));
    let fork = fork_of(&view, cx, parent, 0);
    assert_eq!(fork.forked_from.as_ref().unwrap().mode, ForkMode::Summary);
    let worktree = fork.cwd.clone().unwrap();
    assert_ne!(worktree, repo);
    assert!(git(&repo, &["worktree", "list", "--porcelain"]).contains(worktree.to_str().unwrap()));
}

// ---- terminal agents --------------------------------------------------------------------------

#[gpui::test]
fn a_terminal_agent_forks_through_its_own_cli_in_a_new_tab_once_its_session_id_is_known(
    cx: &mut TestAppContext,
) {
    let env = HistoryEnv::new();
    env.install_stand_ins();
    let shell = env.install_login_shell();
    let daemon = TestDaemon::start_with_login_shell(&env.daemon_toml(false), &shell);
    fs::write(
        daemon.home().join("settings.toml"),
        format!(
            "[app]\nsetup_done = true\n[history]\nenabled = false\n[terminal]\nshell = \"{}\"\n",
            shell.display()
        ),
    )
    .unwrap();
    let workspace = daemon.create_workspace("app-a", &[&env.project_a]);
    let report = |session: SessionId, agent: &str, id: Option<&str>| {
        let _: AgentApplied = daemon.call(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session,
            source: "test-hooks".into(),
            agent: agent.into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: id.map(str::to_owned),
            resume_argv: None,
            node: None,
        }));
    };
    let cases = [
        ("omp", Some("omp-7f3a"), vec!["omp", "--fork", "omp-7f3a"]),
        (
            "Claude Code",
            Some("claude-91"),
            vec!["claude", "--resume", "claude-91", "--fork-session"],
        ),
        ("codex", Some("codex-5"), vec!["codex", "fork", "codex-5"]),
    ];
    let mut sessions = Vec::new();
    for (agent, id, _) in &cases {
        let session = daemon.create_session(&["sh", "-c", "exec cat"], Some(workspace.id));
        report(session.id, agent, *id);
        sessions.push(session);
    }
    let unknown = daemon.create_session(&["sh", "-c", "exec cat"], Some(workspace.id));
    report(unknown.id, "claude", None);
    let plain = daemon.create_session(&["sh", "-c", "exec cat"], Some(workspace.id));
    let (view, cx) = open(cx, &daemon);
    wait_until(cx, "the agents' reports", |cx| {
        sessions
            .iter()
            .chain([&unknown])
            .all(|s| info(&view, cx, s.id).is_some_and(|i| i.agent.is_some()))
    });

    let menu =
        |view: &Entity<Workbench>, cx: &mut VisualTestContext, id: SessionId| -> Vec<String> {
            view.update_in(cx, |_, window, cx| {
                window.dispatch_action(
                    Box::new(actions::ShowSessionMenu(actions::SessionMenuAt {
                        session: id.to_string(),
                        position: point(px(300.0), px(200.0)),
                    })),
                    cx,
                );
            });
            draw(cx);
            view.read_with(cx, |wb, _| wb.context_menu_entries())
                .unwrap_or_default()
                .into_iter()
                .map(|(label, _)| label.to_string())
                .collect()
        };
    // No session id yet: greyed, with the reason. A plain shell has no such entry.
    let labels = menu(&view, cx, unknown.id);
    assert!(
        labels
            .iter()
            .any(|l| l == "Fork Session (claude has not reported its session id)"),
        "{labels:?}"
    );
    cx.simulate_keystrokes("escape");
    assert!(
        !menu(&view, cx, plain.id)
            .iter()
            .any(|l| l.starts_with("Fork Session"))
    );
    cx.simulate_keystrokes("escape");

    for (session, (_, _, argv)) in sessions.iter().zip(&cases) {
        let before: Vec<SessionId> = view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .map(|s| s.id)
                .collect()
        });
        let labels = menu(&view, cx, session.id);
        let at = labels
            .iter()
            .position(|l| l == "Fork Session")
            .unwrap_or_else(|| panic!("no Fork Session in {labels:?}"));
        view.update_in(cx, |wb, window, cx| wb.run_menu_entry(at, window, cx));
        wait_until(cx, "the forked terminal", |cx| {
            view.read_with(cx, |wb, cx| {
                wb.sessions()
                    .read(cx)
                    .sessions()
                    .iter()
                    .any(|s| !before.contains(&s.id) && s.command == *argv)
            })
        });
        let forked = view.read_with(cx, |wb, cx| {
            wb.sessions()
                .read(cx)
                .sessions()
                .iter()
                .find(|s| !before.contains(&s.id))
                .cloned()
                .unwrap()
        });
        assert_eq!(forked.workspace_id, workspace.id);
        assert!(forked.title.starts_with("fork: "), "{}", forked.title);
        // It is a tab of its own, running the agent's own fork command.
        wait_until(cx, "its tab", |cx| shown(&view, cx) == Some(forked.id));
        wait_until(cx, "the command ran", |cx| {
            view.read_with(cx, |wb, _| wb.terminal_view_of(forked.id))
                .map(|v| v.read_with(cx, |v, _| v.visible_text().join("\n")))
                .is_some_and(|text| text.contains(&format!("FAKE-{}", argv[0])))
        });
    }
}
