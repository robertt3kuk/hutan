#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! `STAGING` (protocol 0.53, capability `worktree.stage`, ADR 0020 § Amendment): the Changes
//! view stages and unstages files and hunks, discards after a question and offers Undo, and
//! commits what is staged alone: through the file rows' checkboxes, the toolbar, the Stage pane
//! (*Stage hunk*, *Unstage hunk*, *Discard hunk…*, the keys `s` `u` `x` `j` `k`), the commit box
//! and the Finish form.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::workbench::{ChangesView, DeliveryForm, Region, Workbench};
use hutan_protocol::messages::Request;
use hutan_protocol::worktree::{WorktreeCreate, WorktreeCreated};
use hutan_settings::{SettingsPaths, SettingsStore};

fn git_in(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `a.txt` with twenty lines on `main`.
fn base() -> String {
    (1..=20).map(|n| format!("line {n}\n")).collect()
}

/// Lines 2 and 19 changed: what the worktree committed.
fn committed() -> String {
    base()
        .replace("line 2\n", "LINE 2\n")
        .replace("line 19\n", "LINE 19\n")
}

/// [`committed`] with lines 5 and 15 changed on top, uncommitted: two hunks against the index.
fn edited() -> String {
    committed()
        .replace("line 5\n", "FIVE\n")
        .replace("line 15\n", "FIFTEEN\n")
}

struct Fx {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
    wt: PathBuf,
    workspace: WorkspaceId,
}

impl Fx {
    /// A repo on `main` and a worktree `feature/x` that committed [`committed`]; the worktree
    /// then edits `a.txt` ([`edited`]) and gets a new `b.txt`, both uncommitted.
    fn new() -> Self {
        Self::build(true)
    }

    fn build(dirty: bool) -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git_in(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git_in(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), base()).unwrap();
        git_in(&root, &["add", "-A"]);
        git_in(&root, &["commit", "-q", "-m", "first"]);
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        let created: WorktreeCreated = daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/x".into(),
            base: None,
            path: None,
            no_setup: false,
        }));
        let wt = created.worktree.path;
        fs::write(wt.join("a.txt"), committed()).unwrap();
        git_in(&wt, &["add", "-A"]);
        git_in(&wt, &["commit", "-q", "-m", "feature work"]);
        if dirty {
            fs::write(wt.join("a.txt"), edited()).unwrap();
            fs::write(wt.join("b.txt"), "brand new\n").unwrap();
        }
        Self {
            daemon,
            _keep: keep,
            root,
            wt,
            workspace,
        }
    }

    fn a_txt(&self) -> String {
        fs::read_to_string(self.wt.join("a.txt")).unwrap()
    }

    /// The paths of the index against `HEAD`.
    fn staged(&self) -> Vec<String> {
        git_in(&self.wt, &["diff", "--cached", "--name-only"])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn open<'a>(cx: &'a mut TestAppContext, fx: &Fx) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let connection = fx.daemon.connection();
    open_connection(cx, fx, connection)
}

fn open_connection<'a>(
    cx: &'a mut TestAppContext,
    fx: &Fx,
    connection: hutan_client::Connection,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        connection,
        None,
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
    wait_until(cx, "the listing and the patch of a.txt", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("a.txt") && c.selected_diff().is_some() && c.info().is_some()
        })
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

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn drawn(view: &Entity<Workbench>, cx: &VisualTestContext, what: Region) -> bool {
    region(view, cx, what).is_some()
}

/// The file rows as `(path, staged, unstaged)`.
fn rows(changes: &Entity<ChangesView>, cx: &VisualTestContext) -> Vec<(String, bool, bool)> {
    changes.read_with(cx, |c, _| {
        c.files()
            .iter()
            .map(|f| (f.path.clone(), f.staged, f.unstaged))
            .collect()
    })
}

/// The hunks of the Stage pane as `(staged, header)`.
fn hunks(changes: &Entity<ChangesView>, cx: &VisualTestContext) -> Vec<(bool, String)> {
    changes.read_with(cx, |c, _| {
        c.hunk_rows()
            .into_iter()
            .map(|h| (h.staged, h.header))
            .collect()
    })
}

/// Opens the Stage pane and waits for the two hunks of `a.txt`.
fn enter_stage_mode(
    view: &Entity<Workbench>,
    changes: &Entity<ChangesView>,
    cx: &mut VisualTestContext,
) {
    click(view, cx, Region::Changes("mode-stage"));
    wait_until(cx, "the two hunks of a.txt", |cx| {
        changes.read_with(cx, |c, _| c.stage_mode()) && hunks(changes, cx).len() == 2
    });
    wait_until(cx, "the hunk cards to be drawn", |cx| {
        drawn(view, cx, Region::ChangesStage(1, "card"))
    });
}

#[gpui::test]
fn a_file_rows_checkbox_stages_and_unstages_the_file(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    assert_eq!(
        rows(&changes, cx),
        [
            ("a.txt".to_owned(), false, true),
            ("b.txt".to_owned(), false, true),
        ]
    );

    click(&view, cx, Region::ChangesFileCheck(0));
    wait_until(cx, "a.txt to be staged", |cx| {
        rows(&changes, cx)[0] == ("a.txt".to_owned(), true, false)
    });
    assert_eq!(fx.staged(), ["a.txt"]);
    assert_eq!(fx.a_txt(), edited(), "the working tree is untouched");
    assert_eq!(rows(&changes, cx)[1], ("b.txt".to_owned(), false, true));

    click(&view, cx, Region::ChangesFileCheck(0));
    wait_until(cx, "a.txt to be unstaged", |cx| {
        rows(&changes, cx)[0] == ("a.txt".to_owned(), false, true)
    });
    assert!(fx.staged().is_empty());
}

#[gpui::test]
fn stage_all_unstage_all_and_the_commit_box_take_only_what_is_staged(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    wait_until(cx, "Stage all to be drawn", |cx| {
        drawn(&view, cx, Region::Changes("stage-all"))
    });
    assert!(
        !drawn(&view, cx, Region::Changes("commit-staged")),
        "nothing is staged: no commit box"
    );

    click(&view, cx, Region::Changes("stage-all"));
    wait_until(cx, "both files staged", |cx| {
        rows(&changes, cx).iter().all(|(_, staged, _)| *staged)
    });
    assert_eq!(fx.staged(), ["a.txt", "b.txt"]);

    click(&view, cx, Region::Changes("unstage-all"));
    wait_until(cx, "both files unstaged", |cx| {
        rows(&changes, cx).iter().all(|(_, staged, _)| !*staged)
    });
    assert!(fx.staged().is_empty());

    // Only a.txt, then the commit box: ⌘↩ commits, b.txt stays where it was.
    click(&view, cx, Region::ChangesFileCheck(0));
    wait_until(cx, "Commit staged… to be drawn", |cx| {
        drawn(&view, cx, Region::Changes("commit-staged"))
    });
    click(&view, cx, Region::Changes("commit-staged"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form()),
        Some(DeliveryForm::CommitStaged)
    );
    changes.update(cx, |c, cx| c.set_delivery_text("tune a.txt", cx));
    draw(cx);
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the commit", |cx| {
        changes.read_with(cx, |c, _| c.delivery_form().is_none())
            && git_in(&fx.wt, &["log", "-1", "--format=%s"]).trim() == "tune a.txt"
    });
    assert_eq!(
        git_in(&fx.wt, &["show", "--name-only", "--format=", "HEAD"]).trim(),
        "a.txt"
    );
    assert_eq!(
        git_in(&fx.wt, &["status", "--porcelain"]).trim(),
        "?? b.txt",
        "the rest stays uncommitted"
    );
    wait_until(cx, "the list to follow the commit", |cx| {
        rows(&changes, cx)
            == [
                ("a.txt".to_owned(), false, false),
                ("b.txt".to_owned(), false, true),
            ]
    });
    assert!(!drawn(&view, cx, Region::Changes("commit-staged")));
}

#[gpui::test]
fn finish_offers_commit_staged_only_when_something_is_staged(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);

    // Nothing staged: the form has no such choice.
    changes.update_in(cx, |c, window, cx| c.open_finish(window, cx));
    draw(cx);
    assert!(!drawn(&view, cx, Region::Changes("finish-staged")));
    changes.update(cx, |c, cx| c.close_finish(cx));

    click(&view, cx, Region::ChangesFileCheck(0));
    wait_until(cx, "a.txt to be staged", |cx| rows(&changes, cx)[0].1);
    changes.update_in(cx, |c, window, cx| c.open_finish(window, cx));
    draw(cx);
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_staged_only()),
        Some(false)
    );
    click(&view, cx, Region::Changes("finish-staged"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_staged_only()),
        Some(true)
    );

    changes.update(cx, |c, cx| {
        c.set_finish_push(false, cx);
        c.set_finish_message("deliver a", cx);
        c.confirm_finish(cx);
    });
    wait_until(cx, "Finish", |cx| {
        changes.read_with(cx, |c, _| {
            c.finished().is_some() || c.finish_error().is_some()
        })
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_error().map(str::to_owned)),
        None
    );
    assert!(git_in(&fx.wt, &["log", "--format=%s"]).contains("deliver a"));
    assert_eq!(
        git_in(&fx.wt, &["status", "--porcelain"]).trim(),
        "?? b.txt",
        "b.txt was not committed"
    );
    // The merge brought the commit into the main checkout, without b.txt.
    assert_eq!(fs::read_to_string(fx.root.join("a.txt")).unwrap(), edited());
    assert!(!fx.root.join("b.txt").exists());
}

#[gpui::test]
fn the_stage_pane_stages_and_unstages_single_hunks(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    assert!(!drawn(&view, cx, Region::ChangesStage(0, "card")));
    enter_stage_mode(&view, &changes, cx);
    let both = hunks(&changes, cx);
    assert!(both.iter().all(|(staged, _)| !staged));

    // Stage hunk on the first: it moves under Staged, and only its lines are in the index.
    click(&view, cx, Region::ChangesStage(0, "stage"));
    wait_until(cx, "one staged hunk", |cx| {
        hunks(&changes, cx).iter().filter(|(s, _)| *s).count() == 1
    });
    let now = hunks(&changes, cx);
    assert_eq!(now[0], (false, both[1].1.clone()));
    assert_eq!(now[1], (true, both[0].1.clone()));
    let cached = git_in(&fx.wt, &["diff", "--cached", "-U0", "a.txt"]);
    assert!(cached.contains("+FIVE"), "{cached}");
    assert!(!cached.contains("FIFTEEN"), "{cached}");
    assert_eq!(fx.a_txt(), edited(), "the working tree is untouched");
    assert_eq!(rows(&changes, cx)[0], ("a.txt".to_owned(), true, true));

    // Unstage hunk puts it back.
    click(&view, cx, Region::ChangesStage(1, "unstage"));
    wait_until(cx, "no staged hunk", |cx| {
        hunks(&changes, cx).iter().all(|(s, _)| !*s) && hunks(&changes, cx).len() == 2
    });
    assert!(fx.staged().is_empty());

    // The file's own buttons stage and unstage all of it.
    click(&view, cx, Region::Changes("stage-file"));
    wait_until(cx, "both hunks staged", |cx| {
        hunks(&changes, cx).len() == 2 && hunks(&changes, cx).iter().all(|(s, _)| *s)
    });
    click(&view, cx, Region::Changes("unstage-file"));
    wait_until(cx, "both hunks unstaged", |cx| {
        hunks(&changes, cx).len() == 2 && hunks(&changes, cx).iter().all(|(s, _)| !*s)
    });

    // Review gives the branch diff back.
    click(&view, cx, Region::Changes("mode-review"));
    assert!(!changes.read_with(cx, |c, _| c.stage_mode()));
    wait_until(cx, "the branch diff", |cx| {
        drawn(&view, cx, Region::ChangesHunk(0))
    });
    assert!(!drawn(&view, cx, Region::ChangesStage(0, "card")));
}

#[gpui::test]
fn the_keys_s_u_x_j_k_act_on_the_focused_hunk(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    enter_stage_mode(&view, &changes, cx);
    changes.update_in(cx, |c, window, cx| c.focus_review(window, cx));
    draw(cx);
    let focus = |cx: &VisualTestContext| changes.read_with(cx, |c, _| c.hunk_focus());
    assert_eq!(focus(cx), Some(0));

    cx.simulate_keystrokes("j");
    assert_eq!(focus(cx), Some(1));
    cx.simulate_keystrokes("j");
    assert_eq!(focus(cx), Some(1), "no wrapping past the last hunk");
    cx.simulate_keystrokes("k");
    assert_eq!(focus(cx), Some(0));

    // `s` stages the focused hunk (the first).
    cx.simulate_keystrokes("s");
    wait_until(cx, "the first hunk to be staged", |cx| {
        hunks(&changes, cx).iter().filter(|(s, _)| *s).count() == 1
    });
    let cached = git_in(&fx.wt, &["diff", "--cached", "-U0", "a.txt"]);
    assert!(
        cached.contains("+FIVE") && !cached.contains("FIFTEEN"),
        "{cached}"
    );
    // The focus stays on row 0, now the other (unstaged) hunk; `u` does nothing there.
    assert_eq!(focus(cx), Some(0));
    cx.simulate_keystrokes("u");
    draw(cx);
    assert_eq!(fx.staged(), ["a.txt"]);
    assert_eq!(hunks(&changes, cx).iter().filter(|(s, _)| *s).count(), 1);

    // `j` to the staged hunk, `u` unstages it.
    cx.simulate_keystrokes("j u");
    wait_until(cx, "nothing staged", |cx| {
        hunks(&changes, cx).len() == 2 && hunks(&changes, cx).iter().all(|(s, _)| !*s)
    });
    assert!(fx.staged().is_empty());

    // `x` asks first; Esc says no and nothing changes.
    cx.simulate_keystrokes("x");
    assert!(changes.read_with(cx, |c, _| c.discard_question().is_some()));
    wait_until(cx, "the question to be drawn", |cx| {
        drawn(&view, cx, Region::Changes("discard-ask"))
    });
    cx.simulate_keystrokes("escape");
    assert!(changes.read_with(cx, |c, _| c.discard_question().is_none()));
    assert_eq!(fx.a_txt(), edited());
    assert!(changes.read_with(cx, |c, _| c.undo_offer().is_none()));

    // `x`, Return: the hunk goes, and Undo is offered.
    let gone = hunks(&changes, cx)[focus(cx).unwrap()].1.clone();
    cx.simulate_keystrokes("x");
    cx.simulate_keystrokes("enter");
    wait_until(cx, "the discard", |cx| {
        hunks(&changes, cx).len() == 1 && changes.read_with(cx, |c, _| c.undo_offer().is_some())
    });
    assert_ne!(hunks(&changes, cx)[0].1, gone);
    let text = fx.a_txt();
    assert!(
        text.contains("FIVE") != text.contains("FIFTEEN"),
        "exactly one of the two edits is left: {text}"
    );
}

#[gpui::test]
fn discarding_a_hunk_asks_first_and_undo_puts_it_back(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let changes = open_changes(&view, cx, &fx.wt);
    enter_stage_mode(&view, &changes, cx);

    // Discard hunk… asks; Cancel leaves everything.
    click(&view, cx, Region::ChangesStage(0, "discard"));
    let question = changes
        .read_with(cx, |c, _| c.discard_question().map(str::to_owned))
        .expect("a question");
    assert!(question.contains("a.txt"), "{question}");
    click(&view, cx, Region::Changes("discard-cancel"));
    assert!(changes.read_with(cx, |c, _| c.discard_question().is_none()));
    assert_eq!(fx.a_txt(), edited());

    // Again, and Discard.
    click(&view, cx, Region::ChangesStage(0, "discard"));
    click(&view, cx, Region::Changes("discard-confirm"));
    wait_until(cx, "the discard and its Undo offer", |cx| {
        changes.read_with(cx, |c, _| c.undo_offer().is_some()) && hunks(&changes, cx).len() == 1
    });
    let text = fx.a_txt();
    assert!(!text.contains("FIVE") && text.contains("FIFTEEN"), "{text}");
    let offer = changes
        .read_with(cx, |c, _| c.undo_offer().map(str::to_owned))
        .unwrap();
    assert!(offer.starts_with("Discarded"), "{offer}");

    // Undo restores the working tree and the hunk comes back.
    click(&view, cx, Region::Changes("undo-discard"));
    wait_until(cx, "the discard to be undone", |cx| {
        changes.read_with(cx, |c, _| c.undo_offer().is_none())
    });
    assert_eq!(fx.a_txt(), edited());
    wait_until(cx, "both hunks again", |cx| hunks(&changes, cx).len() == 2);

    // Discard file… takes all the unstaged changes of the file, Undo too.
    click(&view, cx, Region::Changes("discard-file"));
    assert!(changes.read_with(cx, |c, _| c.discard_question().is_some()));
    click(&view, cx, Region::Changes("discard-confirm"));
    wait_until(cx, "the file to be discarded", |cx| {
        changes.read_with(cx, |c, _| c.undo_offer().is_some()) && fx.a_txt() == committed()
    });
    click(&view, cx, Region::Changes("undo-discard"));
    wait_until(cx, "the file to be back", |cx| {
        fx.a_txt() == edited() && changes.read_with(cx, |c, _| c.undo_offer().is_none())
    });

    // Dismiss drops an offer without undoing.
    click(&view, cx, Region::ChangesStage(0, "discard"));
    click(&view, cx, Region::Changes("discard-confirm"));
    wait_until(cx, "the offer", |cx| {
        changes.read_with(cx, |c, _| c.undo_offer().is_some())
    });
    click(&view, cx, Region::Changes("undo-dismiss"));
    assert!(changes.read_with(cx, |c, _| c.undo_offer().is_none()));
    assert_ne!(fx.a_txt(), edited());
}

#[gpui::test]
fn a_daemon_without_staging_gets_no_staging_controls(cx: &mut TestAppContext) {
    let mut fx = Fx::new();
    let connection = fx.daemon.connection_without_worktree_stage();
    let (view, cx) = open_connection(cx, &fx, connection);
    let changes = open_changes(&view, cx, &fx.wt);
    draw(cx);
    assert!(!changes.read_with(cx, |c, cx| c.staging_supported(cx)));
    assert!(!drawn(&view, cx, Region::ChangesFileCheck(0)));
    assert!(!drawn(&view, cx, Region::Changes("stage-all")));
    assert!(!drawn(&view, cx, Region::Changes("mode-stage")));
    changes.update(cx, |c, cx| c.set_stage_mode(true, cx));
    assert!(!changes.read_with(cx, |c, _| c.stage_mode()));
}

#[gpui::test]
fn a_committed_only_file_has_no_stage_pane(cx: &mut TestAppContext) {
    let fx = Fx::build(false);
    let (view, cx) = open(cx, &fx);
    let _changes = open_changes(&view, cx, &fx.wt);
    draw(cx);
    assert!(!drawn(&view, cx, Region::Changes("mode-stage")));
    assert!(!drawn(&view, cx, Region::ChangesFileCheck(0)));
}
