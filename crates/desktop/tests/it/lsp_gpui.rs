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
use hutan_code_view::{CodeView, FileBuffer, GlyphShape, LoadState, Severity, UnderlineStyle};
use hutan_desktop::workbench::file_view::FileView;
use hutan_desktop::workbench::lsp::CHANGE_DEBOUNCE;
use hutan_desktop::workbench::panels::{ProblemRow, ProblemsPanel};
use hutan_desktop::workbench::{FileOpen, Region, Workbench, actions};
use hutan_protocol::lsp::{
    LspDiagnosticsRequest, LspDiagnosticsResult, LspServerState, LspStatus, LspStatusRequest,
};
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
pub(crate) struct Env {
    daemon: TestDaemon,
    root: PathBuf,
    script: PathBuf,
    _work: tempfile::TempDir,
}

impl Env {
    /// `trust`: `[lsp] trust` (`"always"`, or `"ask"` for the trust bar); `script` gets the
    /// root (canonical), for scripts that name files.
    pub(crate) fn new(trust: &str, script: impl FnOnce(&Path) -> Value) -> Self {
        Self::with_extra(trust, script, "")
    }

    /// Like [`new`](Self::new), with `extra` appended to `daemon.toml` (more servers). The
    /// root also has a `Cargo.toml`, so a Rust server's root is the same folder.
    pub(crate) fn with_extra(
        trust: &str,
        script: impl FnOnce(&Path) -> Value,
        extra: &str,
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = work.path().canonicalize().unwrap();
        let root = base.join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("fake.toml"), "").unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let script_path = base.join("script.json");
        std::fs::write(&script_path, script(&root).to_string()).unwrap();
        let toml = format!(
            "[lsp]\ntrust = \"{trust}\"\n\n\
             [lsp.servers.fake]\ncommand = {:?}\nargs = [\"--script\", {:?}]\n\
             languages = [\"fake\"]\nroot_markers = [\"fake.toml\"]\n\n{extra}",
            fake_bin().display().to_string(),
            script_path.display().to_string(),
        );
        Self {
            daemon: TestDaemon::start_with_config(&toml),
            root,
            script: script_path,
            _work: work,
        }
    }

    pub(crate) fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub(crate) fn open<'a>(
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

pub(crate) struct Tab {
    pub(crate) path: String,
    #[allow(dead_code)]
    pub(crate) view: Entity<FileView>,
    pub(crate) code: Entity<CodeView>,
    pub(crate) buffer: Entity<FileBuffer>,
}

/// Writes `text` to `file`, opens it in a tab and waits until its text is in and the daemon
/// has it as a document.
pub(crate) fn open_file(
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

pub(crate) fn caret(code: &Entity<CodeView>, cx: &mut VisualTestContext) -> usize {
    code.read_with(cx, |c, _| c.selections().primary().head)
}

fn focused_path(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |wb, cx| {
        wb.focused_file_view()
            .map(|v| v.read(cx).buffer().read(cx).path().display().to_string())
    })
}

/// Lets the debounce of `doc.change` run out (the test clock does not move by itself).
pub(crate) fn flush_edits(cx: &mut VisualTestContext) {
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
    let drawn = tab
        .code
        .read_with(cx, |c, _| c.drawn_diagnostics().to_vec());
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
    let server_text = view.read_with(cx, |wb, cx| wb.lsp().read(cx).is_synced(&tab.path));
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
    assert_eq!(
        (jumps[0].position.line, jumps[0].position.character),
        (0, 4)
    );

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
    let tab = open_file(&view, cx, &env.file("a.fake"), "x error\ny error\nfn\n");
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

/// The built-in `rust-analyzer` pointed at `program`.
fn rust_server(program: &Path) -> String {
    format!(
        "[lsp.servers.rust-analyzer]\ncommand = {:?}\n",
        program.display().to_string()
    )
}

fn bar_of(tab: &Tab, cx: &mut VisualTestContext) -> Option<String> {
    tab.view.read_with(cx, |v, cx| v.lsp_bar_text(cx))
}

#[gpui::test]
fn the_trust_bar_names_every_server_of_the_folder_and_not_now_covers_the_folder(
    cx: &mut TestAppContext,
) {
    let env = Env::with_extra("ask", |_| json!({}), &rust_server(&fake_bin()));
    let (view, cx) = env.open(cx, "");
    let folder = env.root.display().to_string();
    let fake = open_file(&view, cx, &env.file("a.fake"), "x\n");
    wait_until(cx, "the bar of the one server known", |cx| {
        bar_of(&fake, cx).is_some_and(|text| {
            text == format!(
                "Language server for Plain Text runs code from this folder (build scripts, tools). Trust {folder}?"
            )
        })
    });

    // A Rust file of the same folder: its bar names both servers trusting would start, and
    // so does the first file's.
    let rust = open_file(&view, cx, &env.file("main.rs"), "fn main() {}\n");
    wait_until(cx, "the bar naming both", |cx| {
        bar_of(&rust, cx).is_some_and(|text| {
            text == format!(
                "Language servers for Rust and Plain Text run code from this folder (build scripts, tools). Trust {folder}?"
            )
        })
    });
    assert_eq!(
        bar_of(&fake, cx).as_deref(),
        Some(
            format!(
                "Language servers for Plain Text and Rust run code from this folder (build scripts, tools). Trust {folder}?"
            )
            .as_str()
        )
    );

    // Not now: one answer for the folder; no bar in any tab of it, nor for the next file.
    draw(cx);
    let not_now = view
        .read_with(cx, |wb, _| wb.region_bounds(Region::FileLsp("not-now")))
        .expect("the Not now button");
    cx.simulate_click(not_now.center(), Modifiers::none());
    wait_until(cx, "no bar", |cx| {
        bar_of(&rust, cx).is_none() && bar_of(&fake, cx).is_none()
    });
    let next = open_file(&view, cx, &env.file("b.fake"), "y\n");
    draw(cx);
    assert_eq!(bar_of(&next, cx), None);
    assert!(
        view.read_with(cx, |wb, _| wb.region_bounds(Region::FileLsp("trust")))
            .is_none()
    );

    // Nothing was decided: the folder still waits, no server runs.
    let status: LspStatus = env
        .daemon
        .call(Request::LspStatus(LspStatusRequest::default()));
    assert!(status.trust.is_empty(), "{:?}", status.trust);
    assert_eq!(status.servers.len(), 2);
    assert!(
        status
            .servers
            .iter()
            .all(|s| s.state == LspServerState::Untrusted && s.trust_pending),
        "{:?}",
        status.servers
    );
}

#[gpui::test]
fn the_problems_panel_shows_each_servers_state_and_restarts_a_failed_one(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;
    let programs = tempfile::tempdir().unwrap();
    let rust_analyzer = programs.path().join("rust-analyzer");
    // Its interpreter does not exist: the server cannot start at all (failed at once, no
    // crash loop to wait out).
    std::fs::write(&rust_analyzer, "#!/nonexistent/interpreter\n").unwrap();
    std::fs::set_permissions(&rust_analyzer, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = Env::with_extra(
        "always",
        |_| json!({"diagnostics": true}),
        &rust_server(&rust_analyzer),
    );
    let (view, cx) = env.open(cx, "");
    let fake = open_file(&view, cx, &env.file("a.fake"), "an error\n");
    open_file(&view, cx, &env.file("main.rs"), "fn main() {}\n");
    wait_until(cx, "the fake server's error", |cx| {
        diagnostics(&fake.buffer, cx).len() == 1
    });

    cx.simulate_keystrokes("cmd-shift-m");
    wait_until(cx, "the Problems panel", |cx| {
        view.read_with(cx, |wb, _| wb.panel::<ProblemsPanel>("problems"))
            .is_some()
    });
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ProblemsPanel>("problems"))
        .unwrap();
    wait_until(cx, "a line per server: one ready, one failed", |cx| {
        panel.read_with(cx, |p, _| {
            let states: Vec<_> = p.servers().iter().map(|s| s.state).collect();
            p.settled() && states == [LspServerState::Ready, LspServerState::Failed]
        })
    });
    let (lines, starting) = panel.read_with(cx, |p, _| {
        (
            p.servers().iter().map(|s| s.text()).collect::<Vec<_>>(),
            p.still_starting(),
        )
    });
    assert_eq!(lines[0], "Plain Text · ready");
    assert!(lines[1].starts_with("Rust · failed: "), "{lines:?}");
    assert!(
        lines[1].len() > "Rust · failed: ".len(),
        "the reason is said: {lines:?}"
    );
    assert!(!starting, "nothing is starting: no 'still starting' note");

    // Restart only on the failed line.
    draw(cx);
    assert!(
        view.read_with(cx, |wb, _| wb
            .region_bounds(Region::ProblemsServerRestart(0)))
            .is_none()
    );
    let restart = view
        .read_with(cx, |wb, _| {
            wb.region_bounds(Region::ProblemsServerRestart(1))
        })
        .expect("Restart on the failed server's line");

    // The user mends the server; Restart starts it.
    std::fs::write(
        &rust_analyzer,
        format!(
            "#!/bin/sh\nexec {:?} --script {:?}\n",
            fake_bin().display().to_string(),
            env.script.display().to_string()
        ),
    )
    .unwrap();
    cx.simulate_click(restart.center(), Modifiers::none());
    wait_until(cx, "both servers ready", |cx| {
        panel.read_with(cx, |p, _| {
            p.settled()
                && p.servers().len() == 2
                && p.servers().iter().all(|s| s.state == LspServerState::Ready)
        })
    });
    assert_eq!(
        panel.read_with(cx, |p, _| p
            .servers()
            .iter()
            .map(|s| s.text())
            .collect::<Vec<_>>()),
        ["Plain Text · ready", "Rust · ready"]
    );
}

#[gpui::test]
fn the_problems_panel_lists_by_file_filters_and_opens_at_the_position(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| json!({"diagnostics": true, "warnings": true}));
    let (view, cx) = env.open(cx, "");
    let a = open_file(&view, cx, &env.file("a.fake"), "one error\nwarning two\n");
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
    let places: Vec<String> =
        panel.read_with(cx, |p, _| p.listed().iter().map(|p| p.place()).collect());
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
            p.settled()
                && p.listed().iter().all(|p| p.severity == Severity::Error)
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

#[gpui::test]
fn vim_ctrl_bracket_goes_to_the_definition_and_ctrl_t_comes_back(cx: &mut TestAppContext) {
    let env = Env::new("always", |root| {
        json!({
            "definition": {
                "uri": format!("file://{}", root.join("b.fake").display()),
                "range": {"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 8}},
            },
        })
    });
    let (view, cx) = env.open(cx, "[editor]\nvim_mode = true\n");
    std::fs::write(env.file("b.fake"), "one\ntwo\nfn alpha\n").unwrap();
    let tab = open_file(&view, cx, &env.file("a.fake"), "use alpha;\n");
    draw(cx);
    cx.simulate_input("w");
    let at = caret(&tab.code, cx);
    assert_eq!(at, 4, "on `alpha`");

    // ⌃] opens the definition, ⌃T returns to where it was pressed.
    cx.simulate_keystrokes("ctrl-]");
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
    cx.simulate_keystrokes("ctrl-t");
    wait_until(cx, "back in a.fake", |cx| {
        focused_path(&view, cx).as_deref() == Some(tab.path.as_str())
    });
    let a_code = view.read_with(cx, |wb, cx| {
        wb.focused_file_view().unwrap().read(cx).code().clone()
    });
    wait_until(cx, "the caret where ⌃] was pressed", |cx| {
        caret(&a_code, cx) == at
    });
}
