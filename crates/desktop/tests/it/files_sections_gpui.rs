#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-5: decorations and sections of the Files panel — the language servers' diagnostics beside
//! files and rolled up to their folders (with the scripted `hutan-fake-lsp`), agents' marks and
//! the *Changed by agents* section from the ledger, *Open Editors*, the sections' state per
//! root, the Changes rail icon's count badge and the scope chip's *working* dot. Real daemon,
//! real sockets; timers are the test clock's.

use crate::common;
use crate::lsp_gpui::{Env, open_file};

use std::path::{Path, PathBuf};

use common::chat::ChatEnv;
use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::{
    FILES_DIAGNOSTICS_READ_EVERY, FilesDiagCounts, FilesPanel, FilesSection,
};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_protocol::chat::{ChatSend, ChatSent};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::json;

fn settings(daemon: &TestDaemon, extra: &str) -> SettingsStore {
    std::fs::write(
        daemon.home().join("settings.toml"),
        format!("[app]\nsetup_done = true\n{extra}"),
    )
    .unwrap();
    SettingsStore::new(SettingsPaths::in_dir(daemon.home()))
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

fn open_tab(view: &Entity<Workbench>, cx: &mut VisualTestContext, file: &Path) {
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let key = file.display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.buffers()
                .get(&key)
                .is_some_and(|b| b.read(cx).is_ready())
                && wb.active_center_file().as_deref() == Some(key.as_str())
        })
    });
}

fn files_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<FilesPanel> {
    cx.dispatch_action(actions::ShowFiles);
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<FilesPanel>("files"))
        .expect("the Files panel is drawn")
}

fn counts(errors: usize, warnings: usize) -> Option<FilesDiagCounts> {
    Some(FilesDiagCounts { errors, warnings })
}

/// Waits for the diagnostics of `path`, moving the test clock past the read throttle on the way
/// (a trigger during the cooldown reads when it is over).
fn wait_diag(
    panel: &Entity<FilesPanel>,
    cx: &mut VisualTestContext,
    path: &Path,
    wanted: Option<FilesDiagCounts>,
) {
    wait_until(cx, &format!("{wanted:?} on {}", path.display()), |cx| {
        cx.executor()
            .advance_clock(FILES_DIAGNOSTICS_READ_EVERY + std::time::Duration::from_millis(10));
        panel.read_with(cx, |p, _| p.diagnostics().of(path) == wanted)
    });
}

#[gpui::test]
fn diagnostics_mark_files_and_roll_up_to_the_folders_that_hold_them(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| json!({"diagnostics": true, "warnings": true}));
    let (view, cx) = env.open(cx, "");
    std::fs::create_dir_all(env.file("sub/deep")).unwrap();
    let deep = env.file("sub/deep/a.fake");
    let side = env.file("sub/b.fake");
    let a = open_file(&view, cx, &deep, "one error\nwarning two\n");
    let b = open_file(&view, cx, &side, "warning only\n");
    wait_until(cx, "the servers' diagnostics on the buffers", |cx| {
        a.buffer.read_with(cx, |b, _| b.diagnostics().len() == 2)
            && b.buffer.read_with(cx, |b, _| b.diagnostics().len() == 1)
    });
    let panel = files_panel(&view, cx);

    // A file shows its own counts; every folder above it the sum; the root has no row.
    wait_diag(&panel, cx, &deep, counts(1, 1));
    wait_diag(&panel, cx, &side, counts(0, 1));
    wait_diag(&panel, cx, &env.file("sub/deep"), counts(1, 1));
    wait_diag(&panel, cx, &env.file("sub"), counts(1, 2));
    assert_eq!(
        panel.read_with(cx, |p, _| p.diagnostics().of(&env.file(""))),
        None
    );

    // A file published later is marked without anybody asking: a notice reads again.
    let c = env.file("sub/c.fake");
    let _c = open_file(&view, cx, &c, "error error\n");
    wait_diag(&panel, cx, &c, counts(2, 0));
    wait_diag(&panel, cx, &env.file("sub"), counts(3, 2));

    // `errors` asks the daemon for errors only: the warnings go.
    panel.update(cx, |p, cx| {
        p.set_option("show_diagnostics", "errors".into(), cx);
    });
    wait_diag(&panel, cx, &side, None);
    wait_diag(&panel, cx, &deep, counts(1, 0));
    wait_diag(&panel, cx, &env.file("sub"), counts(3, 0));

    // `off` clears the marks and nothing is asked any more.
    panel.update(cx, |p, cx| {
        p.set_option("show_diagnostics", "off".into(), cx)
    });
    wait_diag(&panel, cx, &deep, None);
    let reads = panel.read_with(cx, |p, _| p.diagnostics_reads());
    let _d = open_file(&view, cx, &env.file("sub/d.fake"), "error\n");
    cx.executor()
        .advance_clock(FILES_DIAGNOSTICS_READ_EVERY * 2);
    draw(cx);
    assert_eq!(
        panel.read_with(cx, |p, _| p.diagnostics_reads()),
        reads,
        "off reads nothing"
    );
}

/// A window on a project whose chat waits on an approval (so it is *working*) after the test
/// rewrote line 5 of the open `notes.txt` the way the agent's shell would: the ledger has it.
struct Scripted {
    env: ChatEnv,
    daemon: TestDaemon,
    view: Entity<Workbench>,
    file: PathBuf,
    key: String,
}

fn scripted_agent_write(cx: &mut TestAppContext) -> (Scripted, &mut VisualTestContext) {
    let env = ChatEnv::new();
    let daemon = TestDaemon::start_with_codex(env.fixture("approval-touch"));
    let info = daemon.create_chat(&env.cwd());
    let settings = settings(&daemon, "");
    let state = daemon.home().join("desktop-state.json");
    let (view, cx) = open_with(cx, settings, daemon.connection(), Some(state));
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    project(&view, cx, &env.cwd());
    let base: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let file = env.cwd().join("notes.txt");
    std::fs::write(&file, &base).unwrap();
    open_tab(&view, cx, &file);
    let key = file.display().to_string();
    let _: ChatSent = daemon.call(Request::ChatSend(ChatSend {
        session_id: info.id,
        text: "Run `touch hi.txt` in a shell, then reply with the word done.".to_owned(),
        attachments: Vec::new(),
    }));
    wait_until(cx, "the turn to wait on its approval", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).approvals().len() == 1)
    });
    std::fs::write(&file, base.replace("line 5\n", "line five\n")).unwrap();
    wait_until(cx, "the agent's write in the ledger", |cx| {
        view.read_with(cx, |wb, _| wb.agent_ledger().ledger().entry(&key).is_some())
    });
    (
        Scripted {
            env,
            daemon,
            view,
            file,
            key,
        },
        cx,
    )
}

#[gpui::test]
fn an_agents_write_is_marked_in_the_tree_and_listed_under_changed_by_agents(
    cx: &mut TestAppContext,
) {
    let (it, cx) = scripted_agent_write(cx);
    let (view, file) = (&it.view, &it.file);
    // A second tab, so that opening the agent's file from the section is a change.
    let other = it.env.cwd().join("other.txt");
    std::fs::write(&other, "x\n").unwrap();
    open_tab(view, cx, &other);
    let panel = files_panel(view, cx);

    // The section lists the file with who changed it and how much; the first hunk is line 5.
    wait_until(cx, "the section's row", |cx| {
        panel.read_with(cx, |p, cx| p.changed_by_agents(cx).len() == 1)
    });
    let listed = panel.read_with(cx, |p, cx| p.changed_by_agents(cx));
    assert_eq!(listed[0].path, *file);
    assert_eq!(listed[0].who, "Codex");
    assert_eq!((listed[0].hunks, listed[0].line), (1, 4));
    assert_eq!(listed[0].summary(), "Codex · 1 hunk");

    // The tree marks the file (◆); a folder would roll it up, the root has no row.
    panel.update(cx, |p, cx| p.refresh(cx));
    wait_until(cx, "the tree to list the file", |cx| {
        panel.read_with(cx, |p, _| p.rows().iter().any(|r| r.path == *file))
    });
    assert_eq!(
        panel.read_with(cx, |p, cx| p.agent_mark_of(file, cx).map(|a| a.who)),
        Some("Codex".to_owned())
    );
    assert!(panel.read_with(cx, |p, cx| p.agent_mark_of(&other, cx).is_none()));
    assert!(!panel.read_with(cx, |p, cx| p.agent_mark_in(&it.env.cwd(), cx)));

    // A click on the row opens the file at its first hunk and focuses its tab.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_center_file()),
        Some(other.display().to_string())
    );
    let row = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::FilesSection("changed-by-agents", 0))
        })
        .expect("the row is drawn");
    cx.simulate_click(row.center(), Modifiers::none());
    wait_until(cx, "the agent's file to be shown", |cx| {
        view.read_with(cx, |wb, _| {
            wb.active_center_file().as_deref() == Some(it.key.as_str())
        })
    });

    // The section folds into its header and opens again.
    let header = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::FilesSection("changed-by-agents", usize::MAX))
        })
        .expect("the header is drawn");
    cx.simulate_click(header.center(), Modifiers::none());
    assert!(panel.read_with(cx, |p, _| p.section_folded(FilesSection::ChangedByAgents)));
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::FilesSection("changed-by-agents", 0)))
            .is_none(),
        "a folded section draws no rows"
    );
    panel.update(cx, |p, cx| {
        p.toggle_section(FilesSection::ChangedByAgents, cx)
    });
    draw(cx);

    // Accept All in the file's bar takes it out of the ledger and the section.
    let accept = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::FileAgent("accept")))
        .expect("the bar's Accept All is drawn");
    cx.simulate_click(accept.center(), Modifiers::none());
    wait_until(cx, "the section to go", |cx| {
        panel.read_with(cx, |p, cx| p.changed_by_agents(cx).is_empty())
    });
    assert!(panel.read_with(cx, |p, cx| p.agent_mark_of(file, cx).is_none()));
    let _ = &it.daemon;
}

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

fn open_editor_names(panel: &Entity<FilesPanel>, cx: &VisualTestContext) -> Vec<String> {
    panel.read_with(cx, |p, cx| {
        p.open_editors(cx)
            .iter()
            .map(|e| {
                format!(
                    "{}{}",
                    e.path.file_name().unwrap().to_string_lossy(),
                    if e.dirty { " ●" } else { "" }
                )
            })
            .collect()
    })
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, region: Region, mods: Modifiers) {
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(region))
        .unwrap_or_else(|| panic!("{region:?} is not drawn"));
    cx.simulate_click(bounds.center(), mods);
}

#[gpui::test]
fn open_editors_lists_the_files_of_two_or_more_tabs_and_clicks_focus_and_close_them(
    cx: &mut TestAppContext,
) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with(cx, settings(&daemon, ""), daemon.connection(), None);
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let (_dir, root) = scratch();
    let (_other_dir, other_root) = scratch();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let (a, b, c) = (
        root.join("a.txt"),
        root.join("sub/b.txt"),
        root.join("c.txt"),
    );
    for file in [&a, &b, &c] {
        std::fs::write(file, "text\n").unwrap();
    }
    let outside = other_root.join("out.txt");
    std::fs::write(&outside, "elsewhere\n").unwrap();
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);

    // One file open: no section (auto wants two); two: both, in tab order.
    open_tab(&view, cx, &a);
    wait_until(cx, "the feed to know the tab", |cx| {
        view.read_with(cx, |wb, cx| wb.files_feed_len(cx) == 1)
    });
    assert!(open_editor_names(&panel, cx).is_empty());
    open_tab(&view, cx, &b);
    wait_until(cx, "two editors", |cx| {
        open_editor_names(&panel, cx).len() == 2
    });
    assert_eq!(open_editor_names(&panel, cx), ["a.txt", "b.txt"]);

    // A file outside the root is a tab but not a row.
    open_tab(&view, cx, &outside);
    wait_until(cx, "the third tab in the feed", |cx| {
        view.read_with(cx, |wb, cx| wb.files_feed_len(cx) == 3)
    });
    assert_eq!(open_editor_names(&panel, cx), ["a.txt", "b.txt"]);

    // Unsaved text is a dot on the row.
    open_tab(&view, cx, &b);
    cx.simulate_input("typed ");
    wait_until(cx, "the unsaved dot", |cx| {
        open_editor_names(&panel, cx) == ["a.txt", "b.txt ●"]
    });
    draw(cx);

    // A click on a row focuses its tab; the active one is the feed's.
    click(
        &view,
        cx,
        Region::FilesSection("open-editors", 0),
        Modifiers::none(),
    );
    wait_until(cx, "a.txt to be shown", |cx| {
        view.read_with(cx, |wb, _| {
            wb.active_center_file().as_deref() == Some(a.display().to_string().as_str())
        })
    });

    // The close button closes a clean file's tab at once…
    draw(cx);
    click(
        &view,
        cx,
        Region::FilesSection("open-close", 0),
        Modifiers::none(),
    );
    wait_until(cx, "one editor left", |cx| {
        view.read_with(cx, |wb, cx| wb.files_feed_len(cx) == 2)
    });
    // …and with one file left the auto section is gone; `always` lists it, `never` hides it.
    assert!(open_editor_names(&panel, cx).is_empty());
    panel.update(cx, |p, cx| {
        p.set_option("open_editors", "always".into(), cx)
    });
    assert_eq!(open_editor_names(&panel, cx), ["b.txt ●"]);
    panel.update(cx, |p, cx| p.set_option("open_editors", "never".into(), cx));
    assert!(open_editor_names(&panel, cx).is_empty());
    panel.update(cx, |p, cx| {
        p.set_option("open_editors", "always".into(), cx)
    });

    // The file with unsaved text asks before its tab closes (the tab's own rule).
    draw(cx);
    click(
        &view,
        cx,
        Region::FilesSection("open-close", 0),
        Modifiers::none(),
    );
    wait_until(cx, "the question about unsaved changes", |cx| {
        view.read_with(cx, |wb, _| wb.modal().is_some())
    });
    assert_eq!(open_editor_names(&panel, cx), ["b.txt ●"], "still open");
    let _ = &c;
}

#[gpui::test]
fn open_editors_close_with_cmd_click_and_middle_click(cx: &mut TestAppContext) {
    let daemon = TestDaemon::start();
    let (view, cx) = open_with(
        cx,
        settings(&daemon, "[panels.files]\nopen_editors = \"always\"\n"),
        daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
    });
    let (_dir, root) = scratch();
    let files: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
        .iter()
        .map(|name| {
            let file = root.join(name);
            std::fs::write(&file, "text\n").unwrap();
            file
        })
        .collect();
    project(&view, cx, &root);
    let panel = files_panel(&view, cx);
    for file in &files {
        open_tab(&view, cx, file);
    }
    wait_until(cx, "three editors", |cx| {
        open_editor_names(&panel, cx).len() == 3
    });
    draw(cx);

    // ⌘-click on the first row closes a.txt.
    click(
        &view,
        cx,
        Region::FilesSection("open-editors", 0),
        Modifiers::command(),
    );
    wait_until(cx, "a.txt to close", |cx| {
        open_editor_names(&panel, cx) == ["b.txt", "c.txt"]
    });
    draw(cx);

    // A middle click on the first row closes b.txt.
    let at = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::FilesSection("open-editors", 0))
        })
        .expect("the row is drawn")
        .center();
    cx.simulate_mouse_down(at, gpui::MouseButton::Middle, Modifiers::none());
    cx.simulate_mouse_up(at, gpui::MouseButton::Middle, Modifiers::none());
    wait_until(cx, "b.txt to close", |cx| {
        open_editor_names(&panel, cx) == ["c.txt"]
    });
}
