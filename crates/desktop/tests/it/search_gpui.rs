#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Search panel (E-13b) over a real daemon: typing searches the active project through
//! `fs.search` (grouped by file, toggles and globs applied, Return opens at the match, the
//! editor's selection seeds ⌘⇧F); *Replace All* asks first and then changes each file in one
//! undo step — a clean file on disk (expecting the version the search saw), an unsaved buffer
//! in the buffer only, a file changed since the search skipped and listed; and typing in the
//! query while a search streams over 100 000 files stays within a frame (key → frame) and
//! draws the panel only when something changed.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::{SearchPanel, SearchRow, SearchStatus};
use hutan_desktop::workbench::search_replace::SkipReason;
use hutan_desktop::workbench::{FileOpen, Workbench, actions};
use hutan_settings::{SettingsPaths, SettingsStore};

fn open<'a>(
    cx: &'a mut TestAppContext,
    daemon: &TestDaemon,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    std::fs::write(
        daemon.home().join("settings.toml"),
        "[app]\nsetup_done = true\n",
    )
    .unwrap();
    let settings = SettingsStore::new(SettingsPaths::in_dir(daemon.home()));
    let (view, cx) = open_with(cx, settings, daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    (view, cx)
}

/// Opens `dir` as the project the window works in.
fn project(view: &Entity<Workbench>, cx: &mut VisualTestContext, dir: &Path) {
    let dir = dir.to_owned();
    view.update_in(cx, |wb, window, cx| {
        wb.open_project_path(dir.clone(), window, cx);
    });
    wait_until(cx, "the project to be the active one", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).files_root().as_deref() == Some(dir.as_path())
        })
    });
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

/// ⌘⇧F (or ⌘⇧H with `replace`): the panel, drawn, with the keyboard in its query.
fn search_panel(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    replace: bool,
) -> Entity<SearchPanel> {
    if replace {
        cx.dispatch_action(actions::ShowSearchReplace);
    } else {
        cx.dispatch_action(actions::ShowSearch);
    }
    draw(cx);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<SearchPanel>("search"))
        .expect("the Search panel is drawn")
}

/// The test executor's timers are fake: the typing pause (`panels.search.debounce_ms`, 150
/// by default) passes when the clock is moved.
fn pause(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(Duration::from_millis(200));
}

fn answered(panel: &Entity<SearchPanel>, cx: &mut VisualTestContext, after: u64) {
    wait_until(cx, "the answer of fs.search", |cx| {
        pause(cx);
        panel.read_with(cx, |p, _| p.answers() > after && !p.searching())
    });
}

fn listed(panel: &Entity<SearchPanel>, cx: &VisualTestContext) -> Vec<(String, usize)> {
    panel.read_with(cx, |p, _| {
        p.files()
            .iter()
            .map(|f| (f.path.clone(), f.match_count()))
            .collect()
    })
}

fn focused_file(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, (usize, usize))> {
    view.read_with(cx, |wb, cx| {
        wb.focused_file_view().map(|v| {
            let v = v.read(cx);
            (
                v.buffer().read(cx).path().display().to_string(),
                v.code().read(cx).position(cx),
            )
        })
    })
}

#[gpui::test]
fn typing_searches_the_project_and_return_opens_the_match(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    write(&root, "src/a.rs", "fn main() {\n    let needle = 1;\n    needle + needle\n}\n");
    write(&root, "src/b.rs", "// Needle in a comment\nfn needle_2() {}\n");
    write(&root, "docs/notes.md", "no match here\nneedle7 and needle8\n");
    write(&root, "target/skip.rs", "needle in a build folder\n");
    project(&view, cx, &root);

    let panel = search_panel(&view, cx, false);
    cx.simulate_input("needle");
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), "needle");
    // Nothing is asked before the typing pauses.
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| (p.status().clone(), p.searches())),
        (SearchStatus::Waiting, 0)
    );
    answered(&panel, cx, 0);
    assert_eq!(panel.read_with(cx, |p, _| p.searches()), 1, "one search for six keys");
    // File by file, in path order; `target` is left out; smart case finds `Needle` too.
    assert_eq!(
        listed(&panel, cx),
        [
            ("docs/notes.md".to_owned(), 2),
            ("src/a.rs".to_owned(), 3),
            ("src/b.rs".to_owned(), 2),
        ]
    );
    let rows = panel.read_with(cx, |p, _| p.rows().to_vec());
    assert_eq!(rows.len(), 3 + 7, "a header per file and a row per match");
    assert_eq!(rows[0], SearchRow::File { file: 0 });
    assert_eq!(panel.read_with(cx, |p, _| p.selected()), Some(1));

    // A regular expression, a whole word, and globs narrow it.
    panel.update(cx, |p, cx| p.toggle_option("regex", cx));
    panel.update(cx, |p, cx| p.set_query(r"needle\d", cx));
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    assert_eq!(listed(&panel, cx), [("docs/notes.md".to_owned(), 2)]);
    panel.update(cx, |p, cx| p.toggle_option("regex", cx));
    panel.update(cx, |p, cx| p.toggle_option("whole_word", cx));
    panel.update(cx, |p, cx| p.set_query("needle", cx));
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    assert_eq!(
        listed(&panel, cx),
        [("src/a.rs".to_owned(), 3), ("src/b.rs".to_owned(), 1)],
        "whole words: not needle7, needle_2"
    );
    panel.update(cx, |p, cx| p.toggle_option("whole_word", cx));
    panel.update(cx, |p, cx| p.set_exclude("docs/**", cx));
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    assert!(!listed(&panel, cx).iter().any(|(p, _)| p.starts_with("docs")));
    panel.update(cx, |p, cx| {
        p.set_exclude("", cx);
        p.set_include("*.md", cx);
    });
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    assert_eq!(listed(&panel, cx), [("docs/notes.md".to_owned(), 2)]);
    panel.update(cx, |p, cx| p.set_include("", cx));
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);

    // ↓ ↓ ↓ from the query moves over matches (file rows skipped), Return opens at the match.
    cx.simulate_keystrokes("down down");
    // Rows: 0 notes.md, 1-2 its matches, 3 a.rs, 4 `    let needle = 1;`.
    assert_eq!(panel.read_with(cx, |p, _| p.selected()), Some(4));
    cx.simulate_keystrokes("enter");
    let a = root.join("src/a.rs").display().to_string();
    wait_until(cx, "a.rs at line 2, column 9", |cx| {
        focused_file(&view, cx) == Some((a.clone(), (2, 9)))
    });

    // ⌘⇧F with a selection in the editor searches for it.
    let code = view.read_with(cx, |wb, cx| {
        wb.focused_file_view().unwrap().read(cx).code().clone()
    });
    // `main` on the first line.
    code.update(cx, |code, cx| code.select(3..7, cx));
    let panel = search_panel(&view, cx, false);
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), "main");
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    assert_eq!(listed(&panel, cx), [("src/a.rs".to_owned(), 1)]);
    // Typing replaces the seeded query: it is selected.
    cx.simulate_input("needle");
    assert_eq!(panel.read_with(cx, |p, _| p.query().to_owned()), "needle");
}

#[gpui::test]
fn replace_all_is_one_undo_step_per_file_and_skips_a_file_changed_since(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let closed = write(&root, "closed.txt", "one needle\r\ntwo needle needle\r\n");
    let dirty = write(&root, "dirty.txt", "needle here\n");
    let changed = write(&root, "changed.txt", "a needle\n");
    let regex = write(&root, "user_ids.rs", "let user_id = order_id;\n");
    project(&view, cx, &root);

    // dirty.txt is open with unsaved text above the match.
    let dirty_key = dirty.display().to_string();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&dirty), window, cx);
    });
    wait_until(cx, "dirty.txt's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.buffers()
                .get(&dirty_key)
                .is_some_and(|b| b.read(cx).is_ready())
        })
    });
    draw(cx);
    cx.simulate_input("mine ");
    let dirty_buffer = view.read_with(cx, |wb, _| wb.buffers().get(&dirty_key).unwrap().clone());
    assert!(dirty_buffer.read_with(cx, |b, _| b.is_dirty()));

    let panel = search_panel(&view, cx, true);
    assert!(panel.read_with(cx, |p, _| p.replace_shown()));
    cx.simulate_input("needle");
    answered(&panel, cx, 0);
    // The dirty buffer's line starts with `mine ` but the disk's does not: the search reads the
    // disk; the replace finds the line changed in the buffer. Search for what both have.
    assert_eq!(
        listed(&panel, cx),
        [
            ("changed.txt".to_owned(), 1),
            ("closed.txt".to_owned(), 3),
            ("dirty.txt".to_owned(), 1),
        ]
    );
    wait_until(cx, "the versions the search saw", |cx| {
        panel.read_with(cx, |p, _| p.versions_ready())
    });
    panel.update(cx, |p, cx| p.set_replacement("pin", cx));
    // Untick the first match of closed.txt's second line.
    let rows = panel.read_with(cx, |p, _| p.rows().to_vec());
    let untick = rows
        .iter()
        .position(|r| {
            *r == SearchRow::Match {
                file: 1,
                hit: 1,
                range: Some(0),
            }
        })
        .unwrap();
    panel.update(cx, |p, cx| p.toggle_row(untick, cx));
    // changed.txt changes after the search.
    std::fs::write(&changed, "a needle, and more\n").unwrap();

    panel.update(cx, |p, cx| p.ask_replace_all(cx));
    assert_eq!(
        panel.read_with(cx, |p, _| p.confirm().map(|c| c.question())),
        Some("Replace 4 matches in 3 files?".to_owned())
    );
    assert_eq!(
        std::fs::read_to_string(&closed).unwrap(),
        "one needle\r\ntwo needle needle\r\n",
        "nothing happens before the answer"
    );
    let focused_before = view.read_with(cx, |wb, _| wb.state().active_center_item());
    panel.update_in(cx, |p, window, cx| p.confirm_replace(window, cx));
    wait_until(cx, "the replace", |cx| {
        panel.read_with(cx, |p, _| !p.replacing() && p.last_replace().is_some())
    });
    let outcome = panel.read_with(cx, |p, _| p.last_replace().cloned().unwrap());

    // closed.txt: written (CRLF kept), the unticked match left, its tab opened behind.
    assert_eq!(
        std::fs::read_to_string(&closed).unwrap(),
        "one pin\r\ntwo needle pin\r\n"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.state().active_center_item()),
        focused_before,
        "the tab opened in the background"
    );
    // dirty.txt: the line in the buffer is not the line searched — skipped, not overwritten.
    // changed.txt: changed since the search — skipped and listed.
    let mut skipped: Vec<(String, SkipReason)> = outcome
        .skipped
        .iter()
        .map(|s| (s.relative.clone(), s.reason.clone()))
        .collect();
    skipped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        skipped,
        [
            ("changed.txt".to_owned(), SkipReason::Changed),
            ("dirty.txt".to_owned(), SkipReason::Changed),
        ]
    );
    assert_eq!((outcome.matches, outcome.files), (2, 1));
    assert_eq!(std::fs::read_to_string(&changed).unwrap(), "a needle, and more\n");
    assert_eq!(std::fs::read_to_string(&dirty).unwrap(), "needle here\n");
    let changed_row = panel.read_with(cx, |p, _| {
        p.files()
            .iter()
            .find(|f| f.path == "changed.txt")
            .and_then(|f| f.skipped.clone())
    });
    assert_eq!(changed_row.as_deref(), Some("changed since the search"));
    // The replaced matches left the list; the unticked one stayed.
    assert_eq!(
        listed(&panel, cx)
            .into_iter()
            .find(|(p, _)| p == "closed.txt"),
        Some(("closed.txt".to_owned(), 1))
    );

    // ⌘Z in closed.txt's tab takes its replacements back, in one step.
    let closed_key = closed.display().to_string();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&closed), window, cx);
    });
    wait_until(cx, "closed.txt focused", |cx| {
        focused_file(&view, cx).is_some_and(|(path, _)| path == closed_key)
    });
    draw(cx);
    let closed_buffer = view.read_with(cx, |wb, _| wb.buffers().get(&closed_key).unwrap().clone());
    assert!(closed_buffer.read_with(cx, |b, _| !b.is_dirty()));
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(
        closed_buffer.read_with(cx, |b, _| b.text().slice(0..b.text().len()).unwrap()),
        "one needle\ntwo needle needle\n"
    );
    assert!(closed_buffer.read_with(cx, |b, _| b.is_dirty() && !b.can_undo()));
    // The unsaved buffer of dirty.txt kept its own text.
    assert_eq!(
        dirty_buffer.read_with(cx, |b, _| b.text().slice(0..b.text().len()).unwrap()),
        "mine needle here\n"
    );

    // An unsaved buffer whose lines are the ones searched takes the replace in the buffer and
    // stays unsaved; a regular expression's groups expand.
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(&regex), window, cx);
    });
    let regex_key = regex.display().to_string();
    wait_until(cx, "user_ids.rs focused", |cx| {
        focused_file(&view, cx).is_some_and(|(path, _)| path == regex_key)
    });
    draw(cx);
    // Make it unsaved without touching the matched line: a new last line.
    let regex_buffer = view.read_with(cx, |wb, _| wb.buffers().get(&regex_key).unwrap().clone());
    cx.simulate_keystrokes("cmd-down");
    cx.simulate_input("// end");
    assert!(regex_buffer.read_with(cx, |b, _| b.is_dirty()));
    let panel = search_panel(&view, cx, true);
    panel.update(cx, |p, cx| {
        p.toggle_option("regex", cx);
        p.set_query(r"(\w+)_id", cx);
        p.set_replacement("${1}Id", cx);
    });
    let before = panel.read_with(cx, |p, _| p.answers());
    answered(&panel, cx, before);
    wait_until(cx, "the versions", |cx| {
        panel.read_with(cx, |p, _| p.versions_ready())
    });
    assert_eq!(listed(&panel, cx), [("user_ids.rs".to_owned(), 2)]);
    panel.update(cx, |p, cx| p.ask_replace_all(cx));
    panel.update_in(cx, |p, window, cx| p.confirm_replace(window, cx));
    wait_until(cx, "the second replace", |cx| {
        panel.read_with(cx, |p, _| !p.replacing() && p.last_replace().is_some_and(|o| o.files == 1))
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p.last_replace().unwrap().in_buffer),
        1
    );
    assert_eq!(
        regex_buffer.read_with(cx, |b, _| b.text().slice(0..b.text().len()).unwrap()),
        "let userId = orderId;\n// end"
    );
    assert!(regex_buffer.read_with(cx, |b, _| b.is_dirty()));
    assert_eq!(
        std::fs::read_to_string(&regex).unwrap(),
        "let user_id = order_id;\n",
        "an unsaved buffer is not written"
    );
}

/// Pumps GPUI until `condition` holds; panics after `limit`.
fn wait_long(
    cx: &mut VisualTestContext,
    what: &str,
    limit: Duration,
    mut condition: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + limit;
    while !condition(cx) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        pause(cx);
        draw(cx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn typing_while_a_search_streams_over_a_hundred_thousand_files_stays_within_a_frame(
    cx: &mut TestAppContext,
) {
    const DIRS: usize = 100;
    const FILES: usize = 1_000;
    let daemon = TestDaemon::start();
    let (view, cx) = open(cx, &daemon);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let body = "fn handler() { let value = compute(); }\n".repeat(8);
    for d in 0..DIRS {
        let folder = root.join(format!("pkg{d:03}/src"));
        std::fs::create_dir_all(&folder).unwrap();
        for f in 0..FILES {
            std::fs::write(folder.join(format!("module_{f:04}.rs")), &body).unwrap();
        }
    }
    project(&view, cx, &root);
    let panel = search_panel(&view, cx, false);

    let mut times = Vec::new();
    // Letters the files do not have: every search looks at all 100 000 of them.
    for key in ["q", "z", "x", "y"] {
        // A search over the 100 000 files is streaming (its progress arrived) when the key
        // comes.
        wait_long(cx, "a streaming search", Duration::from_secs(120), |cx| {
            panel.read_with(cx, |p, _| {
                p.query().is_empty()
                    || matches!(p.status(), SearchStatus::Running { files_searched, .. } if *files_searched > 0)
            })
        });
        let start = Instant::now();
        cx.simulate_input(key);
        draw(cx);
        times.push(start.elapsed());
        assert!(panel.read_with(cx, |p, _| p.query().ends_with(key)));
    }

    // While it streams the panel is drawn for its progress reports, not for every frame.
    wait_long(cx, "the search of `qzxy` to stream", Duration::from_secs(120), |cx| {
        panel.read_with(cx, |p, _| {
            matches!(p.status(), SearchStatus::Running { files_searched, .. } if *files_searched > 0)
        })
    });
    let (renders, reports) = panel.read_with(cx, |p, _| (p.renders(), p.progress_reports()));
    let mut frames = 0;
    for _ in 0..40 {
        draw(cx);
        frames += 1;
        std::thread::sleep(Duration::from_millis(10));
        if !panel.read_with(cx, |p, _| p.searching()) {
            break;
        }
    }
    let (renders_after, reports_after) =
        panel.read_with(cx, |p, _| (p.renders(), p.progress_reports()));
    let drawn = renders_after - renders;
    let reported = reports_after - reports;
    eprintln!(
        "E-13b search, 100 000 files: key → frame {times:?}; {frames} frames drew the panel {drawn} times for {reported} progress reports"
    );
    // One drawing per report, plus the answer's.
    assert!(drawn <= reported + 2, "{drawn} drawings for {reported} reports");
    // The search is cancelled with the query cleared; it ends.
    panel.update(cx, |p, cx| p.set_query("", cx));
    draw(cx);
    assert_eq!(panel.read_with(cx, |p, _| p.status().clone()), SearchStatus::Idle);

    let worst = times.iter().max().copied().unwrap_or_default();
    let budget = if cfg!(debug_assertions) {
        // Unoptimized desktop: the number is recorded, the budget is release's.
        Duration::from_millis(500)
    } else {
        // B4-style: the key's handling and its frame fit in one 60 Hz frame.
        Duration::from_micros(16_700)
    };
    assert!(worst <= budget, "worst {worst:?} over {budget:?}");
}
