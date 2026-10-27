#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EDITOR-POLISH's git change marks in the desktop, over a real daemon and real git
//! repositories: a tab's buffer gets the file as the index holds it (or `HEAD` when nothing of
//! it is staged), typing makes marks and undo clears them, an untracked file has none,
//! `editor.git_gutter = false` turns the comparison off, and *Stage hunk* of the peek stages
//! only that change (saving the file first when it has unsaved changes).

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};
use hutan_code_view::{GitBaseKind, HunkKind, LoadState, agent};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::{FileOpen, Workbench};
use hutan_settings::{SettingsPaths, SettingsStore};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@example.com")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const COMMITTED: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

struct Fx {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), COMMITTED).unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "create a"]);
        daemon.create_workspace("proj", &[&root]);
        Self {
            daemon,
            _keep: keep,
            root,
        }
    }

    fn file(&self) -> PathBuf {
        self.root.join("a.txt")
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    fx: &Fx,
    settings: &str,
) -> (Entity<Workbench>, &'a mut VisualTestContext, SettingsStore) {
    fs::write(
        fx.daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{settings}"),
    )
    .unwrap();
    let store = SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home()));
    let (view, cx) = open_with(cx, store.clone(), fx.daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx, store)
}

fn open_file(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &Path,
) -> Entity<FileView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let canonical = file.canonicalize().unwrap().display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == canonical
                    && *buffer.state() == LoadState::Ready
            })
        })
    });
    draw(cx);
    let file_view = view
        .read_with(cx, |wb, _| wb.focused_file_view())
        .expect("a file tab");
    let code = file_view.read_with(cx, |v, _| v.code().clone());
    cx.update(|window, cx| {
        let focus = code.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    file_view
}

/// Waits until the buffer has a git base of `kind` and its hunks are those of its text.
fn wait_for_base(file: &Entity<FileView>, kind: GitBaseKind, cx: &mut VisualTestContext) {
    wait_until(cx, "the file's git base", |cx| {
        cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
        file.read_with(cx, |v, cx| {
            let b = v.buffer().read(cx);
            b.git_base().is_some_and(|base| base.kind == kind) && b.git_hunks_current()
        })
    });
}

fn kinds(file: &Entity<FileView>, cx: &VisualTestContext) -> Vec<HunkKind> {
    file.read_with(cx, |v, cx| {
        v.buffer()
            .read(cx)
            .git_hunks()
            .iter()
            .map(|h| h.kind())
            .collect()
    })
}

fn wait_for_kinds(file: &Entity<FileView>, cx: &mut VisualTestContext, want: &[HunkKind]) {
    wait_until(cx, &format!("the hunks {want:?}"), |cx| {
        cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
        file.read_with(cx, |v, cx| v.buffer().read(cx).git_hunks_current())
            && kinds(file, cx) == want
    });
}

fn edit_line(file: &Entity<FileView>, line: &str, with: &str, cx: &mut VisualTestContext) {
    let code = file.read_with(cx, |v, _| v.code().clone());
    let at = file.read_with(cx, |v, cx| {
        v.buffer().read(cx).text().to_string().find(line).unwrap()
    });
    code.update(cx, |c, cx| {
        c.set_selections(
            hutan_buffer::Selections::single(hutan_buffer::Selection::new(at, at + line.len())),
            cx,
        );
    });
    cx.simulate_input(with);
}

#[gpui::test]
fn a_clean_file_has_the_marks_of_what_is_typed_and_undo_clears_them(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx, _) = open(cx, &fx, "");
    let file = open_file(&view, cx, &fx.file());
    wait_for_base(&file, GitBaseKind::Head, cx);
    assert!(kinds(&file, cx).is_empty(), "nothing changed yet");

    edit_line(&file, "three", "THREE", cx);
    wait_for_kinds(&file, cx, &[HunkKind::Modified]);
    cx.dispatch_action(hutan_code_view::Undo);
    wait_for_kinds(&file, cx, &[]);
}

#[gpui::test]
fn the_marks_are_against_the_index_when_the_file_has_staged_changes(cx: &mut TestAppContext) {
    let fx = Fx::new();
    // Line two is staged as "TWO"; the working tree has it too.
    let staged = COMMITTED.replace("two\n", "TWO\n");
    fs::write(fx.file(), &staged).unwrap();
    git(&fx.root, &["add", "a.txt"]);
    let (view, cx, _) = open(cx, &fx, "");
    let file = open_file(&view, cx, &fx.file());
    wait_for_base(&file, GitBaseKind::Index, cx);
    assert!(
        kinds(&file, cx).is_empty(),
        "the staged change is not a mark: the index has it"
    );
    // A change after it is a mark; the base text says what the index holds.
    edit_line(&file, "five", "5", cx);
    wait_for_kinds(&file, cx, &[HunkKind::Modified]);
    let base = file.read_with(cx, |v, cx| {
        v.buffer().read(cx).git_base().unwrap().text.to_string()
    });
    assert_eq!(base, staged);
}

#[gpui::test]
fn a_file_git_does_not_track_has_no_marks(cx: &mut TestAppContext) {
    let fx = Fx::new();
    fs::write(fx.root.join("new.txt"), "x\ny\n").unwrap();
    let (view, cx, _) = open(cx, &fx, "");
    let file = open_file(&view, cx, &fx.root.join("new.txt"));
    // Opened and shown: no answer can make a base for an untracked file.
    draw(cx);
    edit_line(&file, "x", "XX", cx);
    cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
    draw(cx);
    assert!(file.read_with(cx, |v, cx| v.buffer().read(cx).git_base().is_none()));
    assert!(kinds(&file, cx).is_empty());
}

#[gpui::test]
fn the_setting_turns_the_comparison_off_and_on(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx, settings) = open(cx, &fx, "[editor]\ngit_gutter = false\n");
    let file = open_file(&view, cx, &fx.file());
    draw(cx);
    assert!(file.read_with(cx, |v, cx| v.buffer().read(cx).git_base().is_none()));
    edit_line(&file, "three", "THREE", cx);
    cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
    draw(cx);
    assert!(kinds(&file, cx).is_empty());

    // Live: turning it on fetches the base and marks the change.
    settings
        .write_user("editor.git_gutter", toml::Value::Boolean(true))
        .unwrap();
    wait_for_kinds(&file, cx, &[HunkKind::Modified]);
}

#[gpui::test]
fn stage_hunk_stages_that_change_only_and_saves_an_unsaved_file_first(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx, _) = open(cx, &fx, "");
    let file = open_file(&view, cx, &fx.file());
    wait_for_base(&file, GitBaseKind::Head, cx);
    // Two changes far apart (two git hunks), neither saved.
    edit_line(&file, "two", "TWO", cx);
    edit_line(&file, "nine", "NINE", cx);
    wait_for_kinds(&file, cx, &[HunkKind::Modified, HunkKind::Modified]);
    assert!(file.read_with(cx, |v, cx| v.buffer().read(cx).is_dirty()));
    let code = file.read_with(cx, |v, _| v.code().clone());
    wait_until(cx, "Stage hunk to be offered", |cx| {
        code.read_with(cx, |c, _| c.git_stage_available())
    });

    // Stage the second one: the file is saved (the disk has both), only NINE is staged.
    code.update(cx, |c, cx| c.stage_git_hunk(1, cx));
    wait_until(cx, "the second change to be staged", |cx| {
        git(&fx.root, &["diff", "--cached", "--name-only"]).contains("a.txt")
            && file.read_with(cx, |v, cx| !v.buffer().read(cx).is_dirty())
    });
    let staged = git(&fx.root, &["show", ":a.txt"]);
    assert_eq!(staged, COMMITTED.replace("nine\n", "NINE\n"));
    let on_disk = fs::read_to_string(fx.file()).unwrap();
    assert_eq!(
        on_disk,
        COMMITTED
            .replace("two\n", "TWO\n")
            .replace("nine\n", "NINE\n")
    );

    // The marks are against the index now: only the first change is left.
    wait_until(cx, "the base to be the index", |cx| {
        cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
        file.read_with(cx, |v, cx| {
            let b = v.buffer().read(cx);
            b.git_base()
                .is_some_and(|base| base.kind == GitBaseKind::Index)
                && b.git_hunks_current()
        })
    });
    assert_eq!(kinds(&file, cx), [HunkKind::Modified]);
    let (first, text) = file.read_with(cx, |v, cx| {
        let b = v.buffer().read(cx);
        let text = b.text().to_string();
        (text[b.git_hunks()[0].bytes.clone()].to_owned(), text)
    });
    assert_eq!(first, "TWO\n");
    assert!(text.contains("NINE"));

    // And the first one, now that the file is saved: no save needed.
    code.update(cx, |c, cx| c.stage_git_hunk(0, cx));
    wait_until(cx, "everything to be staged", |cx| {
        cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
        git(&fx.root, &["diff", "--name-only"]).is_empty()
            && file.read_with(cx, |v, cx| {
                let b = v.buffer().read(cx);
                b.git_hunks_current() && b.git_hunks().is_empty()
            })
    });
    assert_eq!(git(&fx.root, &["show", ":a.txt"]), on_disk);
}

#[gpui::test]
fn two_changes_next_to_each_other_stage_one_at_a_time(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx, _) = open(cx, &fx, "");
    let file = open_file(&view, cx, &fx.file());
    wait_for_base(&file, GitBaseKind::Head, cx);
    // Lines 2 and 5: close enough that git's `-U3` makes one hunk of them.
    edit_line(&file, "two", "TWO", cx);
    edit_line(&file, "five", "FIVE", cx);
    cx.dispatch_action(hutan_code_view::Save);
    wait_until(cx, "the save", |cx| {
        file.read_with(cx, |v, cx| !v.buffer().read(cx).is_dirty())
    });
    wait_for_kinds(&file, cx, &[HunkKind::Modified, HunkKind::Modified]);
    let code = file.read_with(cx, |v, _| v.code().clone());
    wait_until(cx, "Stage hunk to be offered", |cx| {
        code.read_with(cx, |c, _| c.git_stage_available())
    });
    code.update(cx, |c, cx| c.stage_git_hunk(1, cx));
    wait_until(cx, "FIVE to be staged", |_| {
        git(&fx.root, &["show", ":a.txt"]).contains("FIVE")
    });
    assert_eq!(
        git(&fx.root, &["show", ":a.txt"]),
        COMMITTED.replace("five\n", "FIVE\n"),
        "the neighbouring change in the same git hunk stays unstaged"
    );
}
