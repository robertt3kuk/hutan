#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Line comments in the Changes view (RC-1) against an in-process daemon: lines are selected with
//! real clicks and drags on the number gutters, comments are written and listed in the Review
//! tray, and "Send to agent" goes through the real `chat.send` (a daemon whose `codex app-server`
//! is `hutan-codex-fixture replay`) or `session.send_text` (a terminal that records what it
//! receives).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::agents::{Node, Reporter};
use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_core::{SessionId, WorkspaceId};
use hutan_desktop::workbench::{ChangesView, Region, ReviewComment, Workbench};
use hutan_protocol::assistant::{AttachmentKind, attachments_meta};
use hutan_protocol::chat::{ChatHistory, ChatHistoryRequest};
use hutan_protocol::messages::{Request, SessionCreate, SessionResult, SessionSpec, TerminalSpec};
use hutan_protocol::worktree::{WorktreeCreate, WorktreeCreated};
use hutan_settings::{SettingsPaths, SettingsStore};

// Fixtures ------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// A repository `<tmp>/repo` on `main` with `a.txt` (`one two three`), the workspace around it
/// and a worktree `feature/x` that changed `a.txt` to `one TWO three four`.
///
/// The diff of `a.txt` is: `0 @@`, `1  one`, `2 -two`, `3 +TWO`, `4  three`, `5 +four`.
struct Fx {
    chats: ChatEnv,
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    wt: PathBuf,
    workspace: WorkspaceId,
}

impl Fx {
    fn new() -> Self {
        Self::with("one\ntwo\nthree\n", "one\nTWO\nthree\nfour\n")
    }

    /// `base` is `a.txt` on `main`, `feature` what `feature/x` made of it.
    fn with(base: &str, feature: &str) -> Self {
        let chats = ChatEnv::new();
        let daemon = TestDaemon::start_with_codex(chats.fixture("plain-turn"));
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), base).unwrap();
        commit_all(&root, "first");
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        let created: WorktreeCreated = daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
        }));
        let wt = created.worktree.path;
        fs::write(wt.join("a.txt"), feature).unwrap();
        commit_all(&wt, "feature work");
        Self {
            chats,
            daemon,
            _keep: dir,
            wt,
            workspace,
        }
    }

    fn chat(&self) -> SessionId {
        self.daemon.create_chat(&self.wt).id
    }

    /// A terminal in the worktree that appends everything it receives to `<wt>/../received.txt`
    /// and that an agent reported in.
    fn terminal_agent(&self) -> (SessionId, PathBuf) {
        let log = self.wt.parent().unwrap().join("received.txt");
        let created: SessionResult = self.daemon.call(Request::SessionCreate(SessionCreate {
            workspace_id: Some(self.workspace),
            spec: SessionSpec::Terminal(TerminalSpec {
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    format!("cat >> {}", log.display()),
                ],
                cwd: Some(self.wt.clone()),
                title: Some("tui agent".into()),
                ..TerminalSpec::default()
            }),
        }));
        let id = created.session.id;
        Reporter::new(id, "omp-extension", "omp").report(&self.daemon, Node::main("m").working());
        (id, log)
    }

    fn history(&self, session: SessionId) -> ChatHistory {
        self.daemon.call(Request::ChatHistory(ChatHistoryRequest {
            session_id: session,
            before_seq: None,
            limit: 500,
        }))
    }

    /// The user messages of a chat, oldest first, with what was attached to each (kind, label).
    /// The text is the replayed agent's own echo, so the attachments are what tells them apart.
    fn user_messages(&self, session: SessionId) -> Vec<Vec<(AttachmentKind, String)>> {
        self.history(session)
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                hutan_core::agent::AgentEventKind::MessageEnd {
                    role: hutan_core::agent::MessageRole::User,
                    ..
                } => Some(
                    attachments_meta(event)
                        .into_iter()
                        .map(|m| (m.kind, m.label))
                        .collect(),
                ),
                _ => None,
            })
            .collect()
    }

    fn turn_completed(&self, session: SessionId) -> usize {
        self.history(session)
            .events
            .iter()
            .filter(|e| e.kind.name() == "turn_completed")
            .count()
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    fx: &Fx,
    state: Option<PathBuf>,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        state,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = fx.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    (view, cx)
}

/// Opens the Changes view of `wt`, and waits for the patch of `a.txt` to be drawn.
fn open_changes(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    wt: &Path,
) -> Entity<ChangesView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(wt.to_owned(), window, cx)
    });
    draw(cx);
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&wt.display().to_string()))
        .expect("the Changes view was opened");
    wait_until(cx, "the patch of a.txt", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("a.txt") && c.selected_diff().is_some() && c.info().is_some()
        })
    });
    wait_until(cx, "the lines to be drawn", |cx| {
        region(view, cx, Region::ChangesLine(5)).is_some()
    });
    changes
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click_with(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    what: Region,
    modifiers: Modifiers,
) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), modifiers);
    draw(cx);
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    click_with(view, cx, what, Modifiers::none());
}

/// Presses on `from`, moves to `to` with the button held, and lets go.
fn drag(view: &Entity<Workbench>, cx: &mut VisualTestContext, from: Region, to: Region) {
    wait_until(cx, "the gutters to be drawn", |cx| {
        region(view, cx, from).is_some() && region(view, cx, to).is_some()
    });
    let (a, b) = (
        region(view, cx, from).unwrap().center(),
        region(view, cx, to).unwrap().center(),
    );
    cx.simulate_mouse_down(a, MouseButton::Left, Modifiers::none());
    draw(cx);
    cx.simulate_mouse_move(b, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(b, MouseButton::Left, Modifiers::none());
    draw(cx);
}

/// Adds a comment on diff lines `from..=to` of the shown file through the view's own methods.
fn comment_on(
    changes: &Entity<ChangesView>,
    cx: &mut VisualTestContext,
    from: usize,
    to: usize,
    text: &str,
) {
    let added = changes.update_in(cx, |c, window, cx| {
        c.select_line(from, false, cx);
        c.select_line(to, true, cx);
        assert!(c.open_comment_box(window, cx));
        c.set_comment_text(text, cx);
        c.add_comment(cx)
    });
    assert!(added, "the comment was added");
    draw(cx);
}

fn comments(changes: &Entity<ChangesView>, cx: &VisualTestContext) -> Vec<ReviewComment> {
    changes.read_with(cx, |c, _| c.comments())
}

fn texts(changes: &Entity<ChangesView>, cx: &VisualTestContext) -> Vec<String> {
    comments(changes, cx).into_iter().map(|c| c.text).collect()
}

fn targets(changes: &Entity<ChangesView>, cx: &VisualTestContext) -> Vec<SessionId> {
    changes.read_with(cx, |c, cx| {
        c.send_targets(cx).into_iter().map(|t| t.session).collect()
    })
}

fn wait_targets(changes: &Entity<ChangesView>, cx: &mut VisualTestContext, count: usize) {
    let changes = changes.clone();
    wait_until(cx, "the agents of the checkout", move |cx| {
        targets(&changes, cx).len() == count
    });
}

// Selecting and commenting --------------------------------------------------------------------

#[gpui::test]
fn lines_are_selected_by_click_shift_click_and_drag_and_commented_into_the_tray(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    let selection = |cx: &VisualTestContext| changes.read_with(cx, |c, _| c.selection());
    assert!(
        region(&view, cx, Region::Changes("review-send")).is_none(),
        "no tray without comments"
    );

    // A click on a line number selects the line; a shift-click grows the selection; a click on
    // another number starts again; a drag over several numbers selects them.
    click(&view, cx, Region::ChangesLine(2));
    assert_eq!(selection(cx), Some((2, 2)));
    click_with(&view, cx, Region::ChangesLine(3), Modifiers::shift());
    assert_eq!(selection(cx), Some((2, 3)));
    click(&view, cx, Region::ChangesLine(5));
    assert_eq!(selection(cx), Some((5, 5)));
    drag(&view, cx, Region::ChangesLine(2), Region::ChangesLine(4));
    assert_eq!(selection(cx), Some((2, 4)));

    // The "+ Comment" affordance opens the box; ⌘↩ adds the comment.
    wait_until(cx, "the + Comment affordance", |cx| {
        region(&view, cx, Region::Changes("comment-add")).is_some()
    });
    click(&view, cx, Region::Changes("comment-add"));
    assert!(changes.read_with(cx, |c, _| c.comment_box_open()));
    wait_until(cx, "the comment box", |cx| {
        region(&view, cx, Region::Changes("comment-box")).is_some()
    });
    changes.update(cx, |c, cx| c.set_comment_text("Why upper case?", cx));
    cx.simulate_keystrokes("cmd-enter");
    draw(cx);
    let added = comments(&changes, cx);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].text, "Why upper case?");
    assert_eq!(added[0].anchor(), "a.txt:2-3");
    assert_eq!(
        (added[0].base.as_str(), added[0].file.as_str()),
        ("main", "a.txt")
    );
    assert!(!changes.read_with(cx, |c, _| c.comment_box_open()));
    assert_eq!(selection(cx), None, "the selection went into the comment");
    // The tray counts it and the card hangs under its last line.
    assert_eq!(
        changes.read_with(cx, |c, _| c.review_summary()),
        "1 comment"
    );
    wait_until(cx, "the tray and the card", |cx| {
        region(&view, cx, Region::Changes("review-send")).is_some()
            && region(&view, cx, Region::ChangesTray(0)).is_some()
            && region(&view, cx, Region::ChangesComment(0, "card")).is_some()
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.comment_lines(added[0].id)),
        Some((2, 4)),
        "the comment is under its last line"
    );

    // `c` on a selected line opens the box too, ⌘↩ adds.
    click(&view, cx, Region::ChangesLine(5));
    assert!(changes.update_in(cx, |c, window, _| c.review_focused(window)));
    cx.simulate_keystrokes("c");
    assert!(changes.read_with(cx, |c, _| c.comment_box_open()));
    changes.update(cx, |c, cx| c.set_comment_text("Drop this.", cx));
    cx.simulate_keystrokes("cmd-enter");
    draw(cx);
    assert_eq!(texts(&changes, cx), ["Why upper case?", "Drop this."]);
    assert_eq!(
        changes.read_with(cx, |c, _| c.review_summary()),
        "2 comments"
    );

    // `]c` and `[c` walk the comments (the last one added has the focus).
    let ids: Vec<u64> = comments(&changes, cx).iter().map(|c| c.id).collect();
    assert_eq!(
        changes.read_with(cx, |c, _| c.focused_comment()),
        Some(ids[1])
    );
    cx.simulate_keystrokes("] c");
    assert_eq!(
        changes.read_with(cx, |c, _| c.focused_comment()),
        Some(ids[0])
    );
    cx.simulate_keystrokes("[ c");
    assert_eq!(
        changes.read_with(cx, |c, _| c.focused_comment()),
        Some(ids[1])
    );
    // A tray row jumps to its comment.
    click(&view, cx, Region::ChangesTray(0));
    assert_eq!(
        changes.read_with(cx, |c, _| c.focused_comment()),
        Some(ids[0])
    );

    // Edit saves new text; Resolve and Unresolve; Delete removes.
    click(&view, cx, Region::ChangesComment(0, "edit"));
    assert!(changes.read_with(cx, |c, _| c.comment_box_open()));
    changes.update(cx, |c, cx| c.set_comment_text("Why uppercase?", cx));
    cx.simulate_keystrokes("cmd-enter");
    draw(cx);
    assert_eq!(texts(&changes, cx), ["Why uppercase?", "Drop this."]);
    click(&view, cx, Region::ChangesComment(0, "resolve"));
    assert!(comments(&changes, cx)[0].resolved);
    assert_eq!(
        changes.read_with(cx, |c, _| c.review_summary()),
        "1 comment · 1 resolved"
    );
    click(&view, cx, Region::ChangesComment(0, "resolve"));
    let reopened = &comments(&changes, cx)[0];
    assert!(!reopened.resolved && reopened.keep_open);
    click(&view, cx, Region::ChangesComment(1, "delete"));
    assert_eq!(texts(&changes, cx), ["Why uppercase?"]);
}

#[gpui::test]
fn a_selection_stays_in_its_hunk_and_a_hunk_header_selects_nothing(cx: &mut TestAppContext) {
    // Thirty lines on main; the branch changes lines 2 and 28: two hunks.
    let base: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let edited: String = (1..=30)
        .map(|n| match n {
            2 => "line two changed\n".to_owned(),
            28 => "line 28 changed\n".to_owned(),
            n => format!("line {n}\n"),
        })
        .collect();
    let fx = Fx::with(&base, &edited);
    let (view, cx) = open(cx, &fx, None);
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(fx.wt.clone(), window, cx)
    });
    let changes = view
        .read_with(cx, |wb, _| wb.changes(&fx.wt.display().to_string()))
        .unwrap();
    wait_until(cx, "the patch", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected_diff()
                .is_some_and(|d| hutan_desktop::workbench::hunk_ranges(d).len() == 2)
        })
    });
    let hunks = changes.read_with(cx, |c, _| {
        hutan_desktop::workbench::hunk_ranges(c.selected_diff().unwrap())
    });
    changes.update(cx, |c, cx| {
        // The header selects nothing.
        c.select_line(hunks[0].start, false, cx);
        assert_eq!(c.selection(), None);
        c.select_line(hunks[0].start + 2, false, cx);
        // Extending into the other hunk is ignored: the selection stays where it was.
        c.select_line(hunks[1].start + 2, true, cx);
        assert_eq!(
            c.selection(),
            Some((hunks[1].start + 2, hunks[1].start + 2))
        );
        c.select_line(hunks[1].start + 3, true, cx);
        c.drag_to(hunks[0].start + 1, cx);
        assert_eq!(
            c.selection(),
            Some((hunks[1].start + 2, hunks[1].start + 3))
        );
    });
}

// Sending -------------------------------------------------------------------------------------

#[gpui::test]
fn send_to_agent_is_one_chat_message_with_the_comments_a_file_range_and_the_hunk(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let chat = fx.chat();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_targets(&changes, cx, 1);
    assert_eq!(targets(&changes, cx), [chat]);
    comment_on(&changes, cx, 2, 3, "Why uppercase?\nAnd keep a test.");
    comment_on(&changes, cx, 5, 5, "Drop this line.");

    click(&view, cx, Region::Changes("review-send"));
    wait_until(cx, "the message to reach the agent", |_| {
        !fx.chats.sent_texts().is_empty()
    });
    // What the user said: the header, the comments numbered with their places.
    let sent = fx.chats.sent_texts().remove(0);
    assert!(
        sent.starts_with(
            "Please address these review comments:\n\n\
             1. a.txt:2\n   Why uppercase?\n   And keep a test.\n\n\
             2. a.txt:4\n   Drop this line.\n"
        ),
        "{sent}"
    );
    // What was attached: the lines in the checkout and each hunk excerpt.
    assert!(sent.contains("TWO"), "{sent}");
    assert!(sent.contains("four"), "{sent}");
    assert!(sent.contains("-two\n+TWO"), "{sent}");
    let journaled = fx.user_messages(chat);
    assert_eq!(journaled.len(), 1);
    assert_eq!(
        journaled[0]
            .iter()
            .map(|(kind, _)| *kind)
            .collect::<Vec<_>>(),
        [
            AttachmentKind::File,
            AttachmentKind::PastedText,
            AttachmentKind::File,
            AttachmentKind::PastedText
        ]
    );
    assert!(sent.contains("lines 2-2"), "{sent}");
    assert!(sent.contains("lines 4-4"), "{sent}");

    // The comments are sent now: kept, and no longer drafts.
    wait_until(cx, "the comments to be marked sent", |cx| {
        comments(&changes, cx).iter().all(|c| c.sent_ms.is_some())
    });
    let sent_to = comments(&changes, cx)[0].sent_to.clone();
    assert!(sent_to.is_some());
    assert_eq!(
        changes.read_with(cx, |c, _| c.review_summary()),
        "0 comments · 2 sent"
    );
    assert!(changes.read_with(cx, |c, _| c.review_notice().is_none()));
}

#[gpui::test]
fn the_menu_picks_another_agent_and_send_now_sends_one_comment(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let first = fx.chat();
    let second = fx.chat();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_targets(&changes, cx, 2);
    // The newest chat is the default.
    assert_eq!(targets(&changes, cx), [second, first]);
    comment_on(&changes, cx, 2, 3, "Why uppercase?");
    comment_on(&changes, cx, 5, 5, "Drop this line.");

    // ▾ lists the agents of the checkout and the way to start a new one.
    click(&view, cx, Region::Changes("review-menu"));
    assert!(changes.read_with(cx, |c, _| c.review_menu_open()));
    wait_until(cx, "the menu", |cx| {
        region(&view, cx, Region::ChangesTarget(1)).is_some()
            && region(&view, cx, Region::Changes("review-new-agent")).is_some()
    });
    // Picking the first chat sends the whole review there, not to the default.
    click(&view, cx, Region::ChangesTarget(1));
    wait_until(cx, "the first chat to get the review", |_| {
        fx.user_messages(first).len() == 1
    });
    assert!(fx.user_messages(second).is_empty());
    assert!(!changes.read_with(cx, |c, _| c.review_menu_open()));
    // Two comments: a file range and an excerpt each.
    assert_eq!(
        fx.user_messages(first)[0].len(),
        4,
        "{:?}",
        fx.user_messages(first)
    );
    wait_until(cx, "both comments sent", |cx| {
        comments(&changes, cx).iter().all(|c| c.sent_ms.is_some())
    });
    wait_until(cx, "the turn to end", |_| fx.turn_completed(first) == 1);

    // "Send now" on one comment goes to the default target, alone.
    comment_on(&changes, cx, 1, 1, "Nit: context line.");
    wait_until(cx, "the card", |cx| {
        region(&view, cx, Region::ChangesComment(0, "send")).is_some()
    });
    // The comments are ordered by line: the new one is first.
    assert_eq!(texts(&changes, cx)[0], "Nit: context line.");
    click(&view, cx, Region::ChangesComment(0, "send"));
    wait_until(cx, "the default chat to get the comment", |_| {
        fx.user_messages(second).len() == 1
    });
    // One comment, alone: what the agent was told, last of everything the fake server got.
    wait_until(cx, "both messages to reach the agents", |_| {
        fx.chats.sent_texts().len() == 2
    });
    let told = fx.chats.sent_texts().pop().unwrap();
    assert!(told.contains("1. a.txt:1\n   Nit: context line."), "{told}");
    assert!(!told.contains("Drop this line."), "{told}");
    // One file range and one excerpt were attached.
    let attached = &fx.user_messages(second)[0];
    assert_eq!(
        attached.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
        [AttachmentKind::File, AttachmentKind::PastedText]
    );
    assert!(told.contains("lines 1-1"), "{told}");
    wait_until(cx, "the comment to be marked sent", |cx| {
        comments(&changes, cx)[0].sent_ms.is_some()
    });
}

#[gpui::test]
fn a_terminal_agent_gets_the_review_as_a_bracketed_paste(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (terminal, log) = fx.terminal_agent();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_targets(&changes, cx, 1);
    let found = changes.read_with(cx, |c, cx| c.send_targets(cx));
    assert_eq!((found[0].session, found[0].chat), (terminal, false));
    comment_on(&changes, cx, 2, 3, "Why uppercase?");

    // ⌘↩ with the view focused sends the review (a click on a line number focuses the view).
    click(&view, cx, Region::ChangesLine(1));
    cx.simulate_keystrokes("cmd-enter");
    // The terminal got ESC[200~ … ESC[201~ and Enter, as plain text without attachments.
    let expected_text = "Please address these review comments:\n\n\
        1. a.txt:2\n   Why uppercase?\n   ```diff\n    one\n   -two\n   +TWO\n    three\n   +four\n   ```\n";
    wait_until(cx, "the terminal to receive the paste", |_| {
        fs::read(&log).is_ok_and(|bytes| bytes.ends_with(b"\x1b[201~\n"))
    });
    assert_eq!(
        fs::read(&log).unwrap(),
        format!("\x1b[200~{expected_text}\x1b[201~\n").into_bytes()
    );
    wait_until(cx, "the comment to be marked sent", |cx| {
        comments(&changes, cx)[0].sent_ms.is_some()
    });
}

#[gpui::test]
fn without_an_agent_in_the_checkout_nothing_is_sent_and_the_tray_says_so(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    comment_on(&changes, cx, 2, 3, "Why uppercase?");
    assert!(targets(&changes, cx).is_empty());
    click(&view, cx, Region::Changes("review-send"));
    let notice = changes.read_with(cx, |c, _| c.review_notice().map(str::to_owned));
    assert!(
        notice
            .as_deref()
            .is_some_and(|n| n.contains("No agent works")),
        "{notice:?}"
    );
    assert!(comments(&changes, cx)[0].sent_ms.is_none(), "still a draft");
}

// Outdated, resolved --------------------------------------------------------------------------

#[gpui::test]
fn after_refresh_changed_lines_are_outdated_or_resolve_what_was_sent_and_moved_ones_follow(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let chat = fx.chat();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_targets(&changes, cx, 1);
    comment_on(&changes, cx, 5, 5, "Drop `four`."); // sent, its line stays
    comment_on(&changes, cx, 2, 3, "Why uppercase?"); // sent, its lines change
    click(&view, cx, Region::Changes("review-send"));
    wait_until(cx, "the review to be sent", |cx| {
        comments(&changes, cx).iter().all(|c| c.sent_ms.is_some())
    });
    wait_until(cx, "the turn to end", |_| fx.turn_completed(chat) == 1);
    comment_on(&changes, cx, 4, 4, "Context draft."); // a draft on a line that stays
    comment_on(&changes, cx, 3, 3, "A draft on a changed line.");
    assert_eq!(texts(&changes, cx).len(), 4);

    // The agent rewrites `TWO` and adds a line on top: every line below moves down by one.
    fs::write(fx.wt.join("a.txt"), "zero\none\nTWO!\nthree\nfour\n").unwrap();
    click(&view, cx, Region::Changes("refresh"));
    wait_until(cx, "the outdated flags", |cx| {
        comments(&changes, cx).iter().any(|c| c.outdated)
    });
    wait_until(cx, "the refresh to settle", |cx| {
        let list = comments(&changes, cx);
        list.iter().filter(|c| c.outdated).count() == 2
            && list.iter().filter(|c| c.resolved).count() == 1
    });
    let by_text = |cx: &VisualTestContext, text: &str| {
        comments(&changes, cx)
            .into_iter()
            .find(|c| c.text == text)
            .unwrap()
    };
    // Sent, and its lines changed: resolved by the refresh.
    let changed_sent = by_text(cx, "Why uppercase?");
    assert!(changed_sent.outdated && changed_sent.resolved);
    // Sent, lines untouched: still open, and found one line lower now.
    let kept = by_text(cx, "Drop `four`.");
    assert!(!kept.outdated && !kept.resolved);
    assert_eq!(kept.anchor(), "a.txt:5");
    // Drafts: the changed line is outdated (nothing resolves a draft); the other one moved.
    let outdated = by_text(cx, "A draft on a changed line.");
    assert!(outdated.outdated && !outdated.resolved);
    let moved = by_text(cx, "Context draft.");
    assert!(!moved.outdated && !moved.resolved);
    assert_eq!(moved.anchor(), "a.txt:4");
    // The outdated one is drawn with its original lines.
    wait_until(cx, "the outdated card", |cx| {
        let ordered = comments(&changes, cx);
        let at = ordered
            .iter()
            .position(|c| c.text == "A draft on a changed line.")
            .unwrap();
        region(&view, cx, Region::ChangesComment(at, "card")).is_some()
    });

    // The user reopens the resolved one: later refreshes leave it open; Resolve closes it.
    let index = comments(&changes, cx)
        .iter()
        .position(|c| c.text == "Why uppercase?")
        .unwrap();
    click(&view, cx, Region::ChangesComment(index, "resolve"));
    assert!(!by_text(cx, "Why uppercase?").resolved);
    click(&view, cx, Region::Changes("refresh"));
    wait_until(cx, "the second refresh", |cx| {
        changes.read_with(cx, |c, _| c.selected_diff().is_some())
    });
    draw(cx);
    assert!(
        !by_text(cx, "Why uppercase?").resolved,
        "a reopened comment stays open"
    );
}

// Persistence ---------------------------------------------------------------------------------

#[gpui::test]
fn comments_are_saved_with_the_layout_and_come_back_when_the_desktop_reopens(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let state = fx.daemon.home().join("desktop-state.json");
    // What an earlier desktop saved: one draft, anchored on `+four`, and one on lines that are
    // not in the diff any more.
    let mut kept = ReviewComment {
        id: 7,
        base: "main".into(),
        file: "a.txt".into(),
        lines: vec![hutan_desktop::workbench::SnippetLine {
            mark: hutan_desktop::workbench::Mark::Add,
            text: "four".into(),
            old: None,
            new: Some(9),
        }],
        excerpt: Vec::new(),
        text: "Saved draft.".into(),
        created_ms: 1,
        sent_ms: None,
        sent_to: None,
        resolved: false,
        keep_open: false,
        outdated: false,
    };
    let mut gone = kept.clone();
    gone.id = 8;
    gone.text = "Saved, but the line is gone.".into();
    gone.lines[0].text = "no such line".into();
    kept.excerpt = kept.lines.clone();
    let mut saved = serde_json::Map::new();
    saved.insert(
        fx.wt.display().to_string(),
        serde_json::to_value(vec![&kept, &gone]).unwrap(),
    );
    fs::write(
        &state,
        serde_json::json!({ "review_comments": saved }).to_string(),
    )
    .unwrap();

    let (view, cx) = open(cx, &fx, Some(state.clone()));
    let changes = open_changes(&view, cx, &fx.wt);
    wait_until(cx, "the saved comments to be found again", |cx| {
        comments(&changes, cx).len() == 2 && comments(&changes, cx).iter().any(|c| c.outdated)
    });
    let list = comments(&changes, cx);
    let draft = list.iter().find(|c| c.id == 7).unwrap();
    assert!(!draft.outdated);
    // `four` is line 4 of the file now: the comment followed it.
    assert_eq!(draft.anchor(), "a.txt:4");
    assert!(list.iter().find(|c| c.id == 8).unwrap().outdated);

    // A new comment is saved beside them; deleting one saves again.
    comment_on(&changes, cx, 2, 3, "Fresh.");
    view.update(cx, |wb, _| wb.flush_save());
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    let on_disk = json["review_comments"][fx.wt.display().to_string()]
        .as_array()
        .unwrap();
    assert_eq!(on_disk.len(), 3, "{json}");
    assert!(json["docks"].is_object(), "the layout is still there");
    assert!(
        !on_disk.iter().any(|c| c.get("outdated").is_some()),
        "what is computed is not saved: {json}"
    );
    let first = comments(&changes, cx)[0].id;
    changes.update(cx, |c, cx| c.delete_comment(first, cx));
    view.update(cx, |wb, _| wb.flush_save());
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state).unwrap()).unwrap();
    assert_eq!(
        json["review_comments"][fx.wt.display().to_string()]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// A new agent ---------------------------------------------------------------------------------

#[gpui::test]
fn start_a_new_agent_opens_the_start_sheet_on_the_checkout_and_sends_it_the_review(
    cx: &mut TestAppContext,
) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    comment_on(&changes, cx, 2, 3, "Why uppercase?");

    click(&view, cx, Region::Changes("review-menu"));
    wait_until(cx, "the menu", |cx| {
        region(&view, cx, Region::Changes("review-new-agent")).is_some()
    });
    click(&view, cx, Region::Changes("review-new-agent"));
    // The Start… sheet is open on the worktree, with the review as the prompt; nothing is sent
    // yet, so the comment is still a draft.
    wait_until(cx, "the sheet on the worktree", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher().is_some_and(|l| {
                matches!(
                    l.place(),
                    hutan_desktop::workbench::LaunchWhere::Existing(_)
                )
            })
        })
    });
    let first = view.read_with(cx, |wb, _| wb.launcher().unwrap().prompt().to_owned());
    assert!(
        first.starts_with(
            "Please address these review comments:\n\n1. a.txt:2\n   Why uppercase?\n   ```diff\n"
        ),
        "{first}"
    );
    assert!(comments(&changes, cx)[0].is_draft());
    assert!(fx.chats.sent_texts().is_empty());

    // Starting the agent sends it the review; then the comment is sent.
    wait_until(cx, "Codex to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "codex" && h.enabled())
        })
    });
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(
            hutan_desktop::workbench::LaunchAgent::Harness("codex".into()),
            cx,
        );
    });
    cx.simulate_keystrokes("cmd-enter");
    // The first prompt to this agent and model asks before it is sent.
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the review to reach the new agent", |_| {
        fx.chats.sent_texts().len() == 1
    });
    assert_eq!(fx.chats.sent_texts()[0], first.trim());
    wait_until(cx, "the comment to be marked sent", |cx| {
        comments(&changes, cx)[0].sent_ms.is_some()
    });
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_none()));
}

// The send key -------------------------------------------------------------------------------

#[gpui::test]
fn with_the_cmd_enter_send_key_return_in_the_comment_box_adds_a_line(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    click(&view, cx, Region::ChangesLine(2));
    cx.simulate_keystrokes("c");
    assert!(changes.read_with(cx, |c, _| c.comment_box_open()));
    cx.simulate_input("Why");
    cx.simulate_keystrokes("enter");
    cx.simulate_input("upper");
    assert!(changes.read_with(cx, |c, _| c.comment_box_open()));
    assert!(comments(&changes, cx).is_empty(), "Return did not add it");
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(texts(&changes, cx), ["Why\nupper"]);
}

#[gpui::test]
fn with_the_enter_send_key_return_adds_the_comment_and_sends_the_review(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (_terminal, log) = fx.terminal_agent();
    fs::write(
        fx.daemon.home().join("settings.toml"),
        "[chat]\nsend_key = \"enter\"\n",
    )
    .unwrap();
    let (view, cx) = open(cx, &fx, None);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_targets(&changes, cx, 1);

    // ⇧Return and ⌥Return add lines; Return adds the comment.
    click(&view, cx, Region::ChangesLine(2));
    cx.simulate_keystrokes("c");
    cx.simulate_input("Why");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("upper");
    cx.simulate_keystrokes("alt-enter");
    cx.simulate_input("?");
    assert!(comments(&changes, cx).is_empty());
    cx.simulate_keystrokes("enter");
    assert_eq!(texts(&changes, cx), ["Why\nupper\n?"]);
    assert!(!changes.read_with(cx, |c, _| c.comment_box_open()));

    // ⌘Return adds from the box too.
    click(&view, cx, Region::ChangesLine(5));
    cx.simulate_keystrokes("c");
    cx.simulate_input("Drop this.");
    cx.simulate_keystrokes("cmd-enter");
    assert_eq!(texts(&changes, cx).len(), 2);

    // Return with the diff focused sends the review.
    click(&view, cx, Region::ChangesLine(1));
    assert!(changes.update_in(cx, |c, window, _| c.review_focused(window)));
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the terminal to receive the paste", |_| {
        fs::read(&log).is_ok_and(|bytes| bytes.ends_with(b"\x1b[201~\n"))
    });
    wait_until(cx, "the comments to be marked sent", |cx| {
        comments(&changes, cx).iter().all(|c| c.sent_ms.is_some())
    });
}
