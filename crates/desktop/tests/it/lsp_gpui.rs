#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Language servers in file tabs (slice E-12a) over a real daemon, with `hutan-fake-lsp` (the
//! scripted server of `hutan-lsp-client`) configured in `daemon.toml` for files `*.fake`:
//! diagnostics are replaced per file and follow edits, every severity has an underline style
//! and a glyph shape of its own, a publish reaches the screen in the next frame (B16b), `]d`
//! and F8 jump, `gd` crosses files and `⌃O` comes back, `K` shows the server's hover, the
//! trust bar starts the server, and the Problems panel lists and opens.
//!
//! Timers in a GPUI test run on its own clock: after edits the test advances it past the
//! 50 ms debounce of `doc.change`.

use crate::common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_code_view::{
    CodeView, FileBuffer, GlyphShape, LoadState, Severity, UnderlineStyle,
};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::lsp::CHANGE_DEBOUNCE;
use hutan_desktop::workbench::panels::{ProblemRow, ProblemsPanel};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_protocol::lsp::{LspDiagnosticsRequest, LspDiagnosticsResult, LspServerState};
use hutan_protocol::messages::Request;
use hutan_settings::{SettingsPaths, SettingsStore};
use serde_json::{Value, json};

fn fake_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-fake-lsp");
    assert!(
        bin.is_file(),
        "hutan-fake-lsp not built: run `cargo build -p hutan-lsp-client --bins` (or `just test`)"
    );
    bin
}

/// A daemon running `hutan-fake-lsp` with `script` for `*.fake` files below `root` (which has
/// the root marker `fake.toml`), and a workbench on it.
struct Env {
    daemon: TestDaemon,
    root: PathBuf,
    _work: tempfile::TempDir,
}

impl Env {
    /// `trust`: `[lsp] trust` (`"always"`, or `"ask"` for the trust bar); `script` gets the
    /// root (canonical), for scripts that name files.
    fn new(trust: &str, script: impl FnOnce(&Path) -> Value) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = work.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("fake.toml"), "").unwrap();
        let script_path = base.join("script.json");
        std::fs::write(&script_path, script(&root).to_string()).unwrap();
        let toml = format!(
            "[lsp]\ntrust = \"{trust}\"\n\n\
             [lsp.servers.fake]\ncommand = {:?}\nargs = [\"--script\", {:?}]\n\
             languages = [\"fake\"]\nroot_markers = [\"fake.toml\"]\n",
            fake_bin().display().to_string(),
            script_path.display().to_string(),
        );
        Self {
            daemon: TestDaemon::start_with_config(&toml),
            root,
            _work: work,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn open<'a>(
        &self,
        cx: &'a mut TestAppContext,
        settings_toml: &str,
    ) -> (Entity<Workbench>, &'a mut VisualTestContext) {
        std::fs::write(
            self.daemon.home().join("settings.toml"),
            format!("[app]\nsetup_done = true\n{settings_toml}"),
        )
        .unwrap();
        let settings = SettingsStore::new(SettingsPaths::in_dir(self.daemon.home()));
        let (view, cx) = open_with(cx, settings, self.daemon.connection(), None);
        wait_until(cx, "the first synchronisation", |cx| {
            view.read_with(cx, |wb, cx| wb.sessions().read(cx).synced())
        });
        (view, cx)
    }
}

struct Tab {
    path: String,
    #[allow(dead_code)]
    view: Entity<FileView>,
    code: Entity<CodeView>,
    buffer: Entity<FileBuffer>,
}

/// Writes `text` to `file`, opens it in a tab and waits until its text is in and the daemon
/// has it as a document.
fn open_file(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    file: &Path,
    text: &str,
) -> Tab {
    std::fs::write(file, text).unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(file), window, cx);
    });
    let path = file.display().to_string();
    wait_until(cx, "the file's tab", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.focused_file_view().is_some_and(|v| {
                let buffer = v.read(cx).buffer().read(cx);
                buffer.path().display().to_string() == path && *buffer.state() == LoadState::Ready
            })
        })
    });
    wait_until(cx, "the daemon's document", |cx| {
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).is_open(&path))
    });
    let file_view = view.read_with(cx, |wb, _| wb.focused_file_view().unwrap());
    let (code, buffer) = file_view.read_with(cx, |v, _| (v.code().clone(), v.buffer().clone()));
    Tab {
        path,
        view: file_view,
        code,
        buffer,
    }
}

/// Where the diagnostics of `buffer` are now: (line, column in bytes, severity).
fn diagnostics(
    buffer: &Entity<FileBuffer>,
    cx: &mut VisualTestContext,
) -> Vec<(usize, usize, Severity)> {
    buffer.read_with(cx, |b, _| {
        let text = b.text();
        b.diagnostics()
            .iter()
            .map(|d| {
                let at = text.byte_to_point(d.bytes.start).unwrap();
                (at.line, at.column, d.severity)
            })
            .collect()
    })
}

fn caret(code: &Entity<CodeView>, cx: &mut VisualTestContext) -> usize {
    code.read_with(cx, |c, _| c.selections().primary().head)
}

fn focused_path(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, cx| {
        wb.focused_file_view()
            .map(|v| v.read(cx).buffer().read(cx).path().display().to_string())
    })
}

/// Lets the debounce of `doc.change` run out (the test clock does not move by itself).
fn flush_edits(cx: &mut VisualTestContext) {
    cx.executor()
        .advance_clock(CHANGE_DEBOUNCE + Duration::from_millis(10));
    cx.run_until_parked();
}

#[gpui::test]
fn diagnostics_have_a_style_and_a_shape_per_severity_and_follow_edits_until_replaced(
    cx: &mut TestAppContext,
) {
    let env = Env::new("always", |_| json!({"diagnostics": true, "warnings": true}));
    let (view, cx) = env.open(cx, "");
    let other = open_file(&view, cx, &env.file("b.fake"), "an error here\n");
    let tab = open_file(
        &view,
        cx,
        &env.file("a.fake"),
        "let x = error;\nwarning: unused\nfine\n",
    );
    wait_until(cx, "both diagnostics of a.fake", |cx| {
        diagnostics(&tab.buffer, cx).len() == 2
    });
    assert_eq!(
        diagnostics(&tab.buffer, cx),
        [(0, 8, Severity::Error), (1, 0, Severity::Warning)]
    );
    wait_until(cx, "the diagnostic of b.fake", |cx| {
        diagnostics(&other.buffer, cx).len() == 1
    });

    // No cue is colour alone: an underline style and a gutter glyph shape per severity.
    draw(cx);
    let drawn = tab.code.read_with(cx, |c, _| c.drawn_diagnostics().to_vec());
    let summary: Vec<_> = drawn
        .iter()
        .map(|d| (d.line, d.severity, d.underline, d.glyph))
        .collect();
    assert_eq!(
        summary,
        [
            (
                0,
                Severity::Error,
                UnderlineStyle::Wavy,
                Some(GlyphShape::Disc)
            ),
            (
                1,
                Severity::Warning,
                UnderlineStyle::Dotted,
                Some(GlyphShape::Triangle)
            ),
        ]
    );

    // Typing above them: until the next publish, the anchors keep them on their text.
    let seen = view.read_with(cx, |wb, cx| wb.lsp().read(cx).diagnostics_seen());
    cx.simulate_input("// top");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        diagnostics(&tab.buffer, cx),
        [(1, 8, Severity::Error), (2, 0, Severity::Warning)],
        "the edit moved them with their text, before any publish"
    );
    flush_edits(cx);
    wait_until(cx, "the publish after the edit", |cx| {
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).diagnostics_seen() > seen)
    });
    assert_eq!(
        diagnostics(&tab.buffer, cx),
        [(1, 8, Severity::Error), (2, 0, Severity::Warning)],
        "the server agrees with where the anchors put them"
    );

    // The next publish replaces the file's set; the other file keeps its own.
    let seen = view.read_with(cx, |wb, cx| wb.lsp().read(cx).diagnostics_seen());
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("all fine now");
    flush_edits(cx);
    wait_until(cx, "the publish that clears a.fake", |cx| {
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).diagnostics_seen() > seen)
            && diagnostics(&tab.buffer, cx).is_empty()
    });
    draw(cx);
    assert!(
        tab.code
            .read_with(cx, |c, _| c.drawn_diagnostics().is_empty())
    );
    assert_eq!(diagnostics(&other.buffer, cx), [(0, 3, Severity::Error)]);
    let server_text = view.read_with(cx, |wb, cx| {
        wb.lsp().read(cx).is_synced(&tab.path)
    });
    assert!(server_text, "nothing of a.fake waits to be sent");
}

#[gpui::test]
fn a_published_diagnostic_is_drawn_in_the_next_frame(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| json!({"diagnostics": true}));
    let (view, cx) = env.open(cx, "");
    let tab = open_file(&view, cx, &env.file("a.fake"), "clean\n");
    draw(cx);
    let epoch = tab.buffer.read_with(cx, |b, _| b.diagnostics_epoch());
    let before = tab.code.read_with(cx, |c, _| c.stats().frames);

    // An edit the server answers with an error; the test pumps GPUI (which draws what it
    // invalidated) until the diagnostics are on the buffer, and draws nothing itself.
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("an error");
    flush_edits(cx);
    let deadline = std::time::Instant::now() + common::WAIT;
    while tab.buffer.read_with(cx, |b, _| b.diagnostics_epoch()) == epoch {
        assert!(std::time::Instant::now() < deadline, "no publish arrived");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    let arrived = tab.buffer.read_with(cx, |b, _| b.diagnostics_epoch());
    let at_arrival = tab.code.read_with(cx, |c, _| c.stats().frames);
    assert_eq!(diagnostics(&tab.buffer, cx), [(0, 3, Severity::Error)]);

    // B16b: the first frame after the publish drew the squiggle, at the latest the next one.
    draw(cx);
    let (stats_drawn, drawn) = tab.code.read_with(cx, |c, _| {
        (c.stats().diagnostics_drawn, c.drawn_diagnostics().to_vec())
    });
    let (epoch_drawn, frame) = stats_drawn.expect("a frame drew diagnostics");
    assert_eq!(epoch_drawn, arrived);
    assert!(
        frame > before && frame <= at_arrival + 1,
        "drawn in frame {frame}; {at_arrival} frames when the publish was on the buffer"
    );
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].underline, UnderlineStyle::Wavy);
}

#[gpui::test]
fn vim_jumps_between_diagnostics_goes_to_definitions_across_files_and_back(
    cx: &mut TestAppContext,
) {
    let env = Env::new("always", |root| {
        json!({
            "diagnostics": true,
            "warnings": true,
            "hover": "**alpha** is a fake symbol\n\n```\nfn alpha() -> u8\n```",
            "definition": {
                "uri": format!("file://{}", root.join("b.fake").display()),
                "range": {"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 8}},
            },
        })
    });
    let (view, cx) = env.open(cx, "[editor]\nvim_mode = true\n");
    std::fs::write(env.file("b.fake"), "one\ntwo\nfn alpha\n").unwrap();
    let tab = open_file(
        &view,
        cx,
        &env.file("a.fake"),
        "use alpha;\nlet x = error;\nwarning: unused\n",
    );
    wait_until(cx, "the diagnostics of a.fake", |cx| {
        diagnostics(&tab.buffer, cx).len() == 2
    });
    draw(cx);

    // `]d` / `[d` move between them, wrapping around.
    assert_eq!(caret(&tab.code, cx), 0);
    cx.simulate_input("]d");
    assert_eq!(caret(&tab.code, cx), 11 + 8, "the error on line 2");
    cx.simulate_input("]d");
    assert_eq!(caret(&tab.code, cx), 11 + 15, "the warning on line 3");
    cx.simulate_input("]d");
    assert_eq!(caret(&tab.code, cx), 11 + 8, "wrapped to the first");
    cx.simulate_input("[d");
    assert_eq!(caret(&tab.code, cx), 11 + 15, "and back");

    // `K` asks the server and shows its text, markdown as plain text.
    cx.simulate_input("gg");
    cx.simulate_input("w");
    let at = caret(&tab.code, cx);
    assert_eq!(at, 4, "on `alpha`");
    cx.simulate_input("K");
    wait_until(cx, "the hover popover with the server's text", |cx| {
        tab.code.read_with(cx, |c, _| {
            c.hover()
                .is_some_and(|h| h.text().contains("is a fake symbol"))
        })
    });
    let hover = tab.code.read_with(cx, |c, _| c.hover().unwrap().text());
    assert!(hover.contains("alpha is a fake symbol"), "{hover}");
    assert!(hover.contains("fn alpha() -> u8"), "{hover}");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(tab.code.read_with(cx, |c, _| c.hover().is_none()));

    // `gd` opens the other file at the definition and pushes where it left.
    cx.simulate_input("gd");
    let b = env.file("b.fake").display().to_string();
    wait_until(cx, "b.fake at the definition", |cx| {
        focused_path(&view, cx).as_deref() == Some(b.as_str())
    });
    let b_code = view.read_with(cx, |wb, cx| {
        wb.focused_file_view().unwrap().read(cx).code().clone()
    });
    wait_until(cx, "the caret on the definition", |cx| {
        caret(&b_code, cx) == 8 + 3
    });
    let jumps = view.read_with(cx, |wb, cx| wb.lsp().read(cx).jumps().to_vec());
    assert_eq!(jumps.len(), 1);
    assert_eq!(jumps[0].path, tab.path);
    assert_eq!((jumps[0].position.line, jumps[0].position.character), (0, 4));

    // `⌃O` comes back to where `gd` was pressed.
    cx.simulate_keystrokes("ctrl-o");
    wait_until(cx, "back in a.fake", |cx| {
        focused_path(&view, cx).as_deref() == Some(tab.path.as_str())
    });
    let a_code = view.read_with(cx, |wb, cx| {
        wb.focused_file_view().unwrap().read(cx).code().clone()
    });
    wait_until(cx, "the caret where it was", |cx| caret(&a_code, cx) == at);
    assert!(
        view.read_with(cx, |wb, cx| wb.lsp().read(cx).jumps().is_empty()),
        "the jump was taken back"
    );
}

#[gpui::test]
fn without_vim_f8_and_f12_jump(cx: &mut TestAppContext) {
    let env = Env::new("always", |root| {
        json!({
            "diagnostics": true,
            "definition": {
                "uri": format!("file://{}", root.join("a.fake").display()),
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 2}},
            },
        })
    });
    let (view, cx) = env.open(cx, "");
    let tab = open_file(
        &view,
        cx,
        &env.file("a.fake"),
        "x error\ny error\nfn\n",
    );
    wait_until(cx, "the diagnostics", |cx| {
        diagnostics(&tab.buffer, cx).len() == 2
    });
    cx.simulate_keystrokes("f8");
    assert_eq!(caret(&tab.code, cx), 2);
    cx.simulate_keystrokes("f8");
    assert_eq!(caret(&tab.code, cx), 10);
    cx.simulate_keystrokes("shift-f8");
    assert_eq!(caret(&tab.code, cx), 2);

    // F12 in the same file: the caret goes there and *Go Back* (⌃O) returns.
    cx.simulate_keystrokes("f12");
    wait_until(cx, "the definition", |cx| caret(&tab.code, cx) == 16);
    cx.simulate_keystrokes("ctrl-o");
    wait_until(cx, "back", |cx| caret(&tab.code, cx) == 2);
}

#[gpui::test]
fn the_trust_bar_shows_for_an_untrusted_folder_and_trust_starts_the_server(
    cx: &mut TestAppContext,
) {
    let env = Env::new("ask", |_| json!({"diagnostics": true}));
    let (view, cx) = env.open(cx, "");
    let tab = open_file(&view, cx, &env.file("a.fake"), "an error\n");
    wait_until(cx, "the trust bar", |cx| {
        view.read_with(cx, |wb, _| {
            wb.region_bounds(Region::FileLsp("trust")).is_some()
        })
    });
    let server = view.read_with(cx, |wb, cx| wb.lsp().read(cx).server_of(&tab.path).unwrap());
    assert_eq!(server.state, LspServerState::Untrusted);
    assert!(server.trust_pending);
    assert_eq!(server.root.as_deref(), Some(env.root.as_path()));
    assert!(
        diagnostics(&tab.buffer, cx).is_empty(),
        "no server runs before the user trusts the folder"
    );

    let trust = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::FileLsp("trust")))
        .unwrap();
    cx.simulate_click(trust.center(), Modifiers::none());
    wait_until(cx, "the server to run and publish", |cx| {
        diagnostics(&tab.buffer, cx).len() == 1
    });
    wait_until(cx, "the bar to go", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.lsp().read(cx).bar(&tab.path).is_none()
                && wb.region_bounds(Region::FileLsp("trust")).is_none()
        })
    });
    let state = view.read_with(cx, |wb, cx| {
        wb.lsp().read(cx).server_of(&tab.path).map(|s| s.state)
    });
    assert!(
        matches!(
            state,
            Some(LspServerState::Starting | LspServerState::Indexing | LspServerState::Ready)
        ),
        "{state:?}"
    );
}

#[gpui::test]
fn the_problems_panel_lists_by_file_filters_and_opens_at_the_position(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| json!({"diagnostics": true, "warnings": true}));
    let (view, cx) = env.open(cx, "");
    let a = open_file(
        &view,
        cx,
        &env.file("a.fake"),
        "one error\nwarning two\n",
    );
    let b = open_file(&view, cx, &env.file("b.fake"), "\n  error\n");
    wait_until(cx, "every diagnostic", |cx| {
        diagnostics(&a.buffer, cx).len() == 2 && diagnostics(&b.buffer, cx).len() == 1
    });

    // ⌘⇧M: the panel, the keyboard in it.
    cx.simulate_keystrokes("cmd-shift-m");
    wait_until(cx, "the Problems panel's list", |cx| {
        view.read_with(cx, |wb, _| wb.panel::<ProblemsPanel>("problems"))
            .is_some_and(|p| p.read_with(cx, |p, _| p.listed().len() == 3))
    });
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ProblemsPanel>("problems"))
        .unwrap();
    let places: Vec<String> = panel.read_with(cx, |p, _| p.listed().iter().map(|p| p.place()).collect());
    assert_eq!(
        places,
        [
            format!("{}:1:5", a.path),
            format!("{}:2:1", a.path),
            format!("{}:2:3", b.path),
        ]
    );
    let headers = panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .filter(|r| matches!(r, ProblemRow::File { .. }))
            .count()
    });
    assert_eq!(headers, 2, "grouped by file");

    // Parity: the same items `lsp.diagnostics {}` (what `hutan diagnostics` reads) answers.
    let answer: LspDiagnosticsResult = env
        .daemon
        .call(Request::LspDiagnostics(LspDiagnosticsRequest::default()));
    let mut daemon_places: Vec<String> = answer
        .files
        .iter()
        .flat_map(|f| {
            f.diagnostics.iter().map(move |d| {
                format!(
                    "{}:{}:{}",
                    f.path.display(),
                    d.range.start.line + 1,
                    d.range.start.character + 1
                )
            })
        })
        .collect();
    daemon_places.sort();
    let mut sorted = places.clone();
    sorted.sort();
    assert_eq!(sorted, daemon_places);

    // ↓↓ Return: b.fake with the caret on its error, the keyboard there.
    assert_eq!(panel.read_with(cx, |p, _| p.selected()), Some(0));
    cx.simulate_keystrokes("down down enter");
    wait_until(cx, "b.fake at its error", |cx| {
        focused_path(&view, cx).as_deref() == Some(b.path.as_str()) && caret(&b.code, cx) == 3
    });

    // A click on a row opens it too.
    let row = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::ProblemsRow(1)))
        .expect("the warning's row is drawn");
    cx.simulate_click(row.center(), Modifiers::none());
    wait_until(cx, "a.fake at its warning", |cx| {
        focused_path(&view, cx).as_deref() == Some(a.path.as_str()) && caret(&a.code, cx) == 10
    });

    // The filter: errors only.
    cx.dispatch_action(actions::ShowProblems);
    draw(cx);
    for _ in 0..2 {
        let control = view
            .read_with(cx, |wb, _| wb.region_bounds(Region::Problems("severity")))
            .expect("the filter control");
        cx.simulate_click(control.center(), Modifiers::none());
        draw(cx);
    }
    wait_until(cx, "errors only", |cx| {
        panel.read_with(cx, |p, _| {
            p.settled() && p.listed().iter().all(|p| p.severity == Severity::Error)
                && p.listed().len() == 2
        })
    });

    // A new publish refreshes the list.
    let reads = panel.read_with(cx, |p, _| p.reads());
    view.update_in(cx, |wb, window, cx| {
        wb.open_file(FileOpen::picked(env.file("b.fake")), window, cx);
    });
    wait_until(cx, "b.fake focused", |cx| {
        focused_path(&view, cx).as_deref() == Some(b.path.as_str())
    });
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_input("no problem");
    flush_edits(cx);
    wait_until(cx, "the list without b.fake", |cx| {
        panel.read_with(cx, |p, _| {
            p.reads() > reads && p.settled() && p.listed().len() == 1
        })
    });
}
