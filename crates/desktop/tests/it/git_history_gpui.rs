#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! GIT-HISTORY in the desktop, over a real daemon and real git repositories: the blame column
//! of a file tab (it toggles, says who changed each line, marks what is typed since as
//! uncommitted and is asked again after a save), *Show File History* (the commits of a file, the
//! diff of the selected one, *Open at this revision* as a read-only tab, a click on a line's
//! blame opening the history at its commit) and the places that offer them.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Focusable as _, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use hutan_code_view::{BlameLine, CodeViewEvent, LoadState};
use hutan_desktop::workbench::file_history::{FileHistoryView, RevisionState, RevisionView};
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn git_as(dir: &Path, who: &str, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", who)
        .env(
            "GIT_AUTHOR_EMAIL",
            format!("{}@example.com", who.to_lowercase()),
        )
        .env("GIT_COMMITTER_NAME", who)
        .env(
            "GIT_COMMITTER_EMAIL",
            format!("{}@example.com", who.to_lowercase()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(dir: &Path, who: &str, message: &str) {
    git_as(dir, who, &["add", "-A"]);
    git_as(dir, who, &["commit", "-q", "-m", message]);
}

struct Fx {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
}

const V1: &str = "alpha\nbeta\ngamma\n";
const V2: &str = "alpha\nBETA\ngamma\n";
const V3: &str = "alpha\nBETA\ngamma\ndelta\n";

impl Fx {
    /// `a.txt` in three commits: Ada creates it, Grace shouts line 2, Ada adds a line.
    fn new() -> Self {
        let daemon = TestDaemon::start();
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git_as(&root, "Ada", &["init", "-q", "-b", "main"]);
        for (key, value) in [("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")] {
            git_as(&root, "Ada", &["config", key, value]);
        }
        fs::write(root.join("a.txt"), V1).unwrap();
        commit(&root, "Ada", "create a");
        fs::write(root.join("a.txt"), V2).unwrap();
        commit(&root, "Grace", "shout beta");
        fs::write(root.join("a.txt"), V3).unwrap();
        commit(&root, "Ada", "add delta");
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

fn open<'a>(cx: &'a mut TestAppContext, fx: &Fx) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    fs::write(
        fx.daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    (view, cx)
}

/// Opens `file` and waits until the focused tab shows its text; the code view has the keyboard.
fn open_file(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &Path,
) -> Entity<hutan_desktop::workbench::file_view::FileView> {
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

fn who(
    file: &Entity<hutan_desktop::workbench::file_view::FileView>,
    row: usize,
    cx: &VisualTestContext,
) -> String {
    file.read_with(cx, |v, cx| match v.buffer().read(cx).blame_line(row) {
        Some(BlameLine::Commit(c)) => format!("{}: {}", c.author, c.subject),
        Some(BlameLine::Uncommitted) => "uncommitted".to_owned(),
        None => "none".to_owned(),
    })
}

#[gpui::test]
fn the_blame_column_toggles_follows_edits_and_is_asked_again_after_a_save(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let file = open_file(&view, cx, &fx.file());
    assert!(!file.read_with(cx, |v, _| v.blame_on()));
    assert_eq!(who(&file, 0, cx), "none", "no blame until it is asked for");

    // Toggle Blame, through the action the palette and the tab menu run.
    cx.dispatch_action(hutan_code_view::ToggleBlame);
    wait_until(cx, "the blame of the file", |cx| {
        file.read_with(cx, |v, cx| v.buffer().read(cx).has_blame())
    });
    assert!(file.read_with(cx, |v, _| v.blame_on()));
    assert_eq!(who(&file, 0, cx), "Ada: create a");
    assert_eq!(who(&file, 1, cx), "Grace: shout beta");
    assert_eq!(who(&file, 2, cx), "Ada: create a");
    assert_eq!(who(&file, 3, cx), "Ada: add delta");
    draw(cx);
    let code = file.read_with(cx, |v, _| v.code().clone());
    assert!(code.read_with(cx, |c, _| c.blame_gutter()));

    // Typing at the start of the file: that line is uncommitted, the others keep their commit.
    code.update(cx, |c, cx| {
        c.set_selections(hutan_buffer::Selections::cursor(0), cx);
    });
    cx.simulate_input("NEW ");
    assert_eq!(who(&file, 0, cx), "uncommitted");
    assert_eq!(who(&file, 1, cx), "Grace: shout beta");
    assert_eq!(who(&file, 3, cx), "Ada: add delta");

    // Saving writes the line into the working tree: blamed again, it is still no commit's.
    let epoch = file.read_with(cx, |v, cx| v.buffer().read(cx).blame_epoch());
    cx.dispatch_action(hutan_code_view::Save);
    wait_until(cx, "the file to be saved and blamed again", |cx| {
        file.read_with(cx, |v, cx| {
            let buffer = v.buffer().read(cx);
            !buffer.is_dirty() && buffer.blame_epoch() > epoch
        })
    });
    assert_eq!(who(&file, 0, cx), "uncommitted");
    assert_eq!(who(&file, 1, cx), "Grace: shout beta");

    // Toggling it off takes the blame away.
    cx.dispatch_action(hutan_code_view::ToggleBlame);
    wait_until(cx, "the blame to go", |cx| {
        file.read_with(cx, |v, cx| !v.buffer().read(cx).has_blame())
    });
    assert!(!code.read_with(cx, |c, _| c.blame_gutter()));
}

#[gpui::test]
fn inline_blame_is_a_setting_that_asks_for_the_blame_by_itself(cx: &mut TestAppContext) {
    let fx = Fx::new();
    fs::write(
        fx.daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n[editor]\ninline_blame = true\n",
    )
    .unwrap();
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let file = open_file(&view, cx, &fx.file());
    wait_until(cx, "the blame of the file", |cx| {
        file.read_with(cx, |v, cx| v.buffer().read(cx).has_blame())
    });
    let code = file.read_with(cx, |v, _| v.code().clone());
    assert!(code.read_with(cx, |c, _| c.inline_blame()));
    assert!(!code.read_with(cx, |c, _| c.blame_gutter()), "no column");
    assert_eq!(who(&file, 1, cx), "Grace: shout beta");
}

fn history_of(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &Path,
    select: Option<String>,
) -> Entity<FileHistoryView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file_history(file, select, window, cx);
    });
    draw(cx);
    view.read_with(cx, |wb, _| wb.file_history(&file.display().to_string()))
        .expect("the history tab was opened")
}

#[gpui::test]
fn file_history_lists_the_commits_and_shows_the_diff_of_the_selected_one(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let history = history_of(&view, cx, &fx.file(), None);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(ItemRef::history(fx.file().display().to_string()))
    );
    wait_until(cx, "the three commits and the newest one's diff", |cx| {
        history.read_with(cx, |h, _| {
            h.commits().len() == 3 && h.selected_diff().is_some()
        })
    });
    history.read_with(cx, |h, _| {
        let subjects: Vec<&str> = h.commits().iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["add delta", "shout beta", "create a"]);
        assert_eq!(h.selected().unwrap().subject, "add delta");
        assert!(!h.more());
        let diff = h.selected_diff().unwrap();
        assert!(diff.lines.iter().any(|l| l.text == "delta"), "{diff:?}");
        assert_eq!(h.title(), "History · a.txt");
    });

    // Selecting an older commit shows its diff for the file.
    let second = history.read_with(cx, |h, _| h.commits()[1].id.clone());
    history.update(cx, |h, cx| h.select(&second, cx));
    wait_until(cx, "the diff of the second commit", |cx| {
        history.read_with(cx, |h, _| {
            h.selected_diff()
                .is_some_and(|d| d.lines.iter().any(|l| l.text == "BETA"))
        })
    });
    assert_eq!(
        history.read_with(cx, |h, _| h.selected_message().map(str::to_owned)),
        Some("shout beta\n".to_owned())
    );
    draw(cx);
    // The rows are drawn, and the tab says what it is.
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::FileHistoryRow(2)).is_some()
    }));
    let item = ItemRef::history(fx.file().display().to_string());
    let title = view.read_with(cx, |wb, cx| wb.item_title(&item, cx));
    assert_eq!(title.as_ref(), "History · a.txt");
}

#[gpui::test]
fn open_at_this_revision_is_a_read_only_tab_of_the_text_then(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let history = history_of(&view, cx, &fx.file(), None);
    wait_until(cx, "the commits", |cx| {
        history.read_with(cx, |h, _| h.commits().len() == 3)
    });
    let first = history.read_with(cx, |h, _| h.commits()[2].id.clone());
    history.update(cx, |h, cx| h.select(&first, cx));
    draw(cx);
    history.update(cx, |h, cx| h.open_revision(cx));
    draw(cx);
    let path = fx.file().display().to_string();
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        Some(ItemRef::revision(path.clone(), first.clone()))
    );
    let revision: Entity<RevisionView> = view
        .read_with(cx, |wb, _| wb.revision(&path, &first))
        .expect("the revision view");
    wait_until(cx, "the text of that revision", |cx| {
        revision.read_with(cx, |r, _| *r.state() == RevisionState::Ready)
    });
    revision.read_with(cx, |r, cx| {
        assert_eq!(r.buffer().read(cx).text().to_string(), V1);
        assert!(r.buffer().read(cx).is_read_only());
        assert!(!r.buffer().read(cx).is_editable());
        assert_eq!(r.title(), format!("a.txt @ {}", &first[..7]));
    });
    // It is not in the window's buffer store: nothing watches or saves it.
    assert!(view.read_with(cx, |wb, _| wb.buffers().is_empty()));
}

#[gpui::test]
fn a_click_on_a_blame_opens_the_history_at_its_commit(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let file = open_file(&view, cx, &fx.file());
    cx.dispatch_action(hutan_code_view::ToggleBlame);
    wait_until(cx, "the blame of the file", |cx| {
        file.read_with(cx, |v, cx| v.buffer().read(cx).has_blame())
    });
    let code = file.read_with(cx, |v, _| v.code().clone());
    // The click on line 2's blame (the code view's own mouse test is in `hutan-code-view`).
    code.update(cx, |_, cx| cx.emit(CodeViewEvent::BlameClicked { row: 1 }));
    draw(cx);
    let history = view
        .read_with(cx, |wb, _| {
            wb.file_history(&fx.file().display().to_string())
        })
        .expect("the history tab");
    wait_until(cx, "the commit of line 2 to be selected", |cx| {
        history.read_with(cx, |h, _| {
            h.selected().is_some_and(|c| c.subject == "shout beta")
        })
    });
}

#[gpui::test]
fn the_places_that_offer_blame_and_history(cx: &mut TestAppContext) {
    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    let file = open_file(&view, cx, &fx.file());
    let _ = file;

    // The file tab's menu (right click on the tab).
    let (pane, index) = view.read_with(cx, |wb, _| {
        let pane = wb.state().active_pane_id();
        let index = wb.state().pane(pane).unwrap().active;
        (pane, index)
    });
    let tab = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::Tab(pane, index)))
        .expect("the tab is drawn");
    cx.simulate_mouse_down(tab.center(), MouseButton::Right, Modifiers::none());
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the tab menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert!(labels.contains(&"Toggle Blame".to_owned()), "{labels:?}");
    assert!(
        labels.contains(&"Show File History".to_owned()),
        "{labels:?}"
    );
    let at = labels
        .iter()
        .position(|l| l == "Show File History")
        .unwrap();
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(at, window, cx));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .file_history(&fx.file().display().to_string()))
            .is_some()
    );

    // The Files panel's context menu for a file, and for a folder (no history of its own).
    cx.dispatch_action(actions::ShowFilesMenu(actions::FilesMenuAt {
        path: fx.file().display().to_string(),
        file: true,
        position: gpui::point(gpui::px(40.0), gpui::px(40.0)),
    }));
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the Files menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(labels, ["Open", "Show File History", "Copy Path"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    cx.dispatch_action(actions::ShowFilesMenu(actions::FilesMenuAt {
        path: fx.root.display().to_string(),
        file: false,
        position: gpui::point(gpui::px(40.0), gpui::px(40.0)),
    }));
    let labels: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the folder menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(labels, ["Copy Path"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));

    // The palette's commands exist.
    let commands: Vec<String> = view.read_with(cx, |wb, _| {
        wb.commands().iter().map(|c| c.title.to_string()).collect()
    });
    assert!(commands.iter().any(|t| t == "Editor: Toggle Blame"));
    assert!(commands.iter().any(|t| t == "Git: Show File History"));
}

#[gpui::test]
fn the_changes_view_has_a_history_segment_with_the_commits_and_their_diffs(
    cx: &mut TestAppContext,
) {
    use hutan_desktop::workbench::{ChangesView, Scope};

    let fx = Fx::new();
    let (view, cx) = open(cx, &fx);
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(fx.root.clone(), window, cx);
    });
    draw(cx);
    let changes: Entity<ChangesView> = view
        .read_with(cx, |wb, _| wb.changes(&fx.root.display().to_string()))
        .expect("the Changes view was opened");
    wait_until(cx, "the first listing", |cx| {
        changes.read_with(cx, |c, _| c.refreshed_ms().is_some())
    });
    assert!(!changes.read_with(cx, |c, _| c.history_shown()));

    // The segment beside Committed and All.
    let segment = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::Changes("changes-scope-history"))
        })
        .expect("the History segment is drawn");
    cx.simulate_click(segment.center(), Modifiers::none());
    assert_eq!(changes.read_with(cx, |c, _| c.scope()), Scope::History);
    wait_until(
        cx,
        "the commits, the newest one's files and its diff",
        |cx| {
            changes.read_with(cx, |c, _| {
                c.history_commits().len() == 3 && c.history_diff().is_some()
            })
        },
    );
    changes.read_with(cx, |c, _| {
        let subjects: Vec<&str> = c
            .history_commits()
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect();
        assert_eq!(subjects, ["add delta", "shout beta", "create a"]);
        assert_eq!(c.history_selected().unwrap().subject, "add delta");
        let files: Vec<&str> = c.history_files().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["a.txt"]);
        assert_eq!(c.history_file(), Some("a.txt"));
        assert!(
            c.history_diff()
                .unwrap()
                .lines
                .iter()
                .any(|l| l.text == "delta")
        );
        assert_eq!(c.history_message(), Some("add delta\n"));
        assert!(!c.history_more());
    });
    draw(cx);
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::ChangesCommit(2)).is_some()
    }));
    assert!(view.read_with(cx, |wb, _| {
        wb.region_bounds(Region::ChangesHistoryFile(0)).is_some()
    }));

    // Another commit: its files and the diff of its first file.
    let second = changes.read_with(cx, |c, _| c.history_commits()[1].id.clone());
    changes.update(cx, |c, cx| c.select_commit(&second, cx));
    wait_until(cx, "the second commit's diff", |cx| {
        changes.read_with(cx, |c, _| {
            c.history_diff()
                .is_some_and(|d| d.lines.iter().any(|l| l.text == "BETA"))
        })
    });

    // The other segments bring the changes back; the uncommitted ones are not mixed in.
    let committed = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::Changes("changes-scope-committed"))
        })
        .expect("the Committed segment is drawn");
    cx.simulate_click(committed.center(), Modifiers::none());
    assert_eq!(changes.read_with(cx, |c, _| c.scope()), Scope::Committed);
    assert!(!changes.read_with(cx, |c, _| c.history_shown()));
}
