#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! EX-5: decorations and sections of the Files panel — the language servers' diagnostics beside
//! files and rolled up to their folders (with the scripted `hutan-fake-lsp`), agents' marks and
//! the *Changed by agents* section from the ledger, *Open Editors*, the sections' state per
//! root, the Changes rail icon's count badge and the scope chip's *working* dot. Real daemon,
//! real sockets; timers are the test clock's.

use crate::common;
use crate::lsp_gpui::{Env, open_file};

use std::path::Path;

use common::{draw, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_desktop::workbench::panels::{FILES_DIAGNOSTICS_READ_EVERY, FilesDiagCounts, FilesPanel};
use hutan_desktop::workbench::{Workbench, actions};
use serde_json::json;

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
