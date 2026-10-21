//! The GPUI-free parts against a real daemon: real PTYs and sockets decide what the model must
//! accept and what the encoders must produce.
//!
//! Each test runs a program that switches terminal modes on and echoes its input with `cat -v`,
//! so the bytes the view would send come back as visible text on the daemon's screen.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use std::time::Duration;

use hutan_client::{Client, ConnectOptions};
use hutan_core::TerminalId;
use hutan_daemon::{DaemonConfig, Paths};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, Request, SessionCreate, SessionResult, SessionSpec, TerminalAck,
    TerminalAttach, TerminalAttached, TerminalHistory, TerminalHistoryRows, TerminalResize,
    TerminalSize, TerminalSpec,
};
use hutan_protocol::render::{MouseMode, TerminalRender};
use hutan_terminal_view::keys::{Key, KeyInput, KeyOptions, Mods, encode_key};
use hutan_terminal_view::model::{TerminalModel, row_cells, row_width};
use hutan_terminal_view::mouse::{Button, MouseAction, MouseEvent, encode_mouse};
use hutan_terminal_view::paste::paste_bytes;
use hutan_terminal_view::selection::{Point, Selection, SelectionKind, selected_text};
use serde_json::Value;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(20);

struct Attached {
    client: Client,
    terminal: TerminalId,
    renders: Receiver<TerminalRender>,
    model: TerminalModel,
}

impl Attached {
    /// Runs `script` with `sh -c` in a 60 × 12 terminal and attaches to it.
    async fn start(daemon_paths: &Paths, script: &str) -> Self {
        let client = Client::connect(ConnectOptions {
            socket: Some(daemon_paths.socket.clone()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Other,
                version: "terminal-view-test".into(),
                origin: None,
            },
        })
        .await
        .unwrap();
        let size = TerminalSize {
            cols: 60,
            rows: 12,
            pixel_width: 0,
            pixel_height: 0,
        };
        let created: SessionResult = client
            .call(Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: vec!["sh".into(), "-c".into(), script.into()],
                    size: Some(size),
                    ..TerminalSpec::default()
                }),
            }))
            .await
            .unwrap();
        let terminal = created.session.terminal_id.expect("terminal");
        let renders = client.terminal_frames(terminal);
        let _: TerminalAttached = client
            .call(Request::TerminalAttach(TerminalAttach {
                terminal_id: terminal,
                size: Some(size),
            }))
            .await
            .unwrap();
        Self {
            client,
            terminal,
            renders,
            model: TerminalModel::new(),
        }
    }

    /// Applies renders (acking each, as the view does) until `done` holds for the model.
    async fn until(&mut self, what: &str, done: impl Fn(&TerminalModel) -> bool) {
        let result = timeout(WAIT, async {
            while !done(&self.model) {
                let render = self.renders.recv().await.expect("connection stays open");
                let frame = match &render {
                    TerminalRender::Snapshot(s) => s.frame,
                    TerminalRender::Frame(f) => f.frame,
                };
                self.model.apply(render);
                let _: Value = self
                    .client
                    .call(Request::TerminalAck(TerminalAck {
                        terminal_id: self.terminal,
                        frame,
                    }))
                    .await
                    .unwrap();
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "timed out waiting for {what}; screen:\n{}",
            screen(&self.model)
        );
    }

    async fn send(&self, bytes: &[u8]) {
        self.client.send_input(self.terminal, bytes).await.unwrap();
    }
}

fn screen(model: &TerminalModel) -> String {
    model
        .viewport()
        .iter()
        .map(TerminalModel::row_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_line(model: &TerminalModel, line: &str) -> bool {
    model
        .viewport()
        .iter()
        .any(|r| TerminalModel::row_text(r) == line)
}

async fn daemon() -> (tempfile::TempDir, hutan_daemon::RunningDaemon, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_home(dir.path());
    let running = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    (dir, running, paths)
}

async fn stop(running: hutan_daemon::RunningDaemon) {
    running.shutdown();
    timeout(WAIT, running.wait()).await.unwrap().unwrap();
}

fn key(key: Key) -> KeyInput {
    KeyInput {
        key,
        mods: Mods::NONE,
    }
}

/// Turns the tty's echo off, then switches the modes on (the test waits for them, so echo is off
/// by the time it sends a byte: the tty echoes what arrives while it is on, which would add a
/// second copy of the typed bytes to the screen), then echoes the input with `cat -v`.
const SCRIPT: &str =
    "stty -echo; printf '\\033[?2004h\\033[?1000h\\033[?1006h\\033[?1h\\033[>1u'; cat -v";

#[tokio::test(flavor = "multi_thread")]
async fn keys_mouse_and_paste_bytes_arrive_as_the_terminal_expects() {
    let (_dir, running, paths) = daemon().await;
    let mut t = Attached::start(&paths, SCRIPT).await;
    // The modes the program switched on reach the model.
    t.until("modes", |m| {
        let modes = m.modes();
        modes.bracketed_paste
            && modes.mouse == MouseMode::Click
            && modes.mouse_sgr
            && modes.app_cursor
            && modes.kitty_keyboard == 1
    })
    .await;
    let modes = t.model.modes();
    let opts = KeyOptions {
        app_cursor: modes.app_cursor,
        app_keypad: modes.app_keypad,
        kitty_flags: modes.kitty_keyboard,
        option_as_meta: true,
    };

    // Application cursor mode: Up is SS3 A. Echoed by `cat -v` on the next line.
    t.send(&encode_key(&key(Key::Up), opts).unwrap()).await;
    t.send(b"\r").await;
    t.until("up arrow", |m| has_line(m, "^[OA")).await;

    // Kitty flag 1: Ctrl-C is CSI 99;5 u, Escape CSI 27 u.
    let ctrl_c = KeyInput {
        key: Key::Char('c'),
        mods: Mods {
            ctrl: true,
            ..Mods::NONE
        },
    };
    t.send(&encode_key(&ctrl_c, opts).unwrap()).await;
    t.send(&encode_key(&key(Key::Escape), opts).unwrap()).await;
    t.send(b"\r").await;
    t.until("kitty codes", |m| has_line(m, "^[[99;5u^[[27u"))
        .await;

    // SGR mouse press at column 3, row 2 (0-based 2, 1).
    let press = MouseEvent {
        action: MouseAction::Press(Button::Left),
        mods: Mods::NONE,
        col: 2,
        row: 1,
    };
    t.send(&encode_mouse(&press, modes.mouse, modes.mouse_sgr).unwrap())
        .await;
    t.send(b"\r").await;
    t.until("mouse report", |m| has_line(m, "^[[<0;3;2M")).await;

    // Bracketed paste markers wrap the text, and an embedded terminator is stripped.
    t.send(&paste_bytes("hi\u{1b}[201~there", modes.bracketed_paste))
        .await;
    t.send(b"\r").await;
    t.until("paste", |m| has_line(m, "^[[200~hithere^[[201~"))
        .await;

    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wide_characters_links_and_styles_follow_the_documented_row_shape() {
    let (_dir, running, paths) = daemon().await;
    let script = "printf 'a\\xe6\\x97\\xa5\\xe6\\x9c\\xac\\xe8\\xaa\\x9eb\\n'; \
                  printf '\\033]8;;https://example.com\\033\\\\link\\033]8;;\\033\\\\ \\033[1;3;4:3;58;2;255;0;0mfancy\\033[0m\\n'; \
                  printf '\\033]2;my title\\007'; sleep 30";
    let mut t = Attached::start(&paths, script).await;
    t.until("content", |m| {
        has_line(m, "a日本語b") && m.title() == Some("my title")
    })
    .await;

    let wide = t
        .model
        .viewport()
        .iter()
        .find(|r| TerminalModel::row_text(r) == "a日本語b")
        .unwrap();
    // a (1) + 3 wide characters (6) + b (1).
    assert_eq!(row_width(wide), 8);
    let cells: Vec<_> = row_cells(wide)
        .iter()
        .map(|c| (c.col, c.width, c.text))
        .collect();
    assert_eq!(
        cells,
        [
            (0, 1, "a"),
            (1, 2, "日"),
            (3, 2, "本"),
            (5, 2, "語"),
            (7, 1, "b")
        ]
    );

    // Selecting across the wide characters extracts them whole.
    let line = t
        .model
        .viewport()
        .iter()
        .position(|r| TerminalModel::row_text(r) == "a日本語b")
        .unwrap();
    let abs = t.model.viewport_line(u16::try_from(line).unwrap());
    let mut selection =
        Selection::new(SelectionKind::Simple, Point { line: abs, col: 2 }, &t.model);
    selection.extend(Point { line: abs, col: 6 });
    let text = selected_text(&t.model, selection.span(&t.model).unwrap());
    assert_eq!(text, "日本語");

    // OSC 8: the run carries a link id, resolved through the link table.
    let linked = t
        .model
        .viewport()
        .iter()
        .flat_map(|r| r.runs.iter())
        .find(|r| r.text == "link")
        .expect("linked run");
    assert_eq!(
        t.model.link(linked.link.unwrap()),
        Some("https://example.com")
    );

    // SGR attributes survive the trip.
    use hutan_protocol::render::{CellFlags, Color};
    let fancy = t
        .model
        .viewport()
        .iter()
        .flat_map(|r| r.runs.iter())
        .find(|r| r.text == "fancy")
        .expect("styled run");
    assert!(
        fancy
            .flags
            .contains(CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERCURL)
    );
    assert_eq!(fancy.underline_color, Some(Color::Rgb(255, 0, 0)));

    stop(running).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_pages_merge_into_the_scrollback_cache() {
    let (_dir, running, paths) = daemon().await;
    let mut t = Attached::start(&paths, "seq 1 100; sleep 30").await;
    t.until("output", |m| has_line(m, "100") && m.history_len() >= 80)
        .await;
    let len = t.model.history_len();
    assert!(len >= 80, "history_len {len}");

    // Page like the view does: 40 lines at a time, newest first.
    let mut from = 0u32;
    while from < len {
        let page: TerminalHistoryRows = t
            .client
            .call(Request::TerminalHistory(TerminalHistory {
                terminal_id: t.terminal,
                from_line: from,
                count: 40,
            }))
            .await
            .unwrap();
        assert!(!page.rows.is_empty());
        from += u32::try_from(page.rows.len()).unwrap();
        t.model.apply_history(page.from_line, page.rows);
    }
    assert_eq!(t.model.first_missing_history(0, len), None);

    // Absolute line 0 is the oldest retained line: "1" (the screen still shows the newest).
    assert_eq!(
        t.model.row_at(0).map(TerminalModel::row_text).as_deref(),
        Some("1")
    );
    let above_viewport = i64::try_from(t.model.history_total()).unwrap() - 1;
    let n = t
        .model
        .row_at(above_viewport)
        .map(TerminalModel::row_text)
        .unwrap();
    let first_on_screen = TerminalModel::row_text(&t.model.viewport()[0]);
    assert_eq!(
        n.parse::<u32>().unwrap() + 1,
        first_on_screen.parse::<u32>().unwrap(),
        "history continues directly above the viewport"
    );

    // A selection spanning scrollback and screen extracts consecutive numbers.
    let end = t.model.viewport_line(1);
    let mut selection = Selection::new(SelectionKind::Line, Point { line: 0, col: 0 }, &t.model);
    selection.extend(Point { line: end, col: 5 });
    let text = selected_text(&t.model, selection.span(&t.model).unwrap());
    let numbers: Vec<u32> = text.lines().map(|l| l.parse().unwrap()).collect();
    assert_eq!(numbers[0], 1);
    assert!(numbers.windows(2).all(|w| w[1] == w[0] + 1), "{numbers:?}");

    stop(running).await;
}

/// What the view's reading-place anchor (`TerminalView::after_apply`) relies on: a width change
/// reflows rows that wrap and renumbers nothing else, so a line that does not wrap keeps its
/// absolute number and the lines above the viewport keep theirs.
#[tokio::test(flavor = "multi_thread")]
async fn a_width_change_keeps_the_line_numbers_of_lines_that_do_not_wrap() {
    let (_dir, running, paths) = daemon().await;
    let mut t = Attached::start(&paths, "seq 1 150; sleep 30").await;
    t.until("output", |m| has_line(m, "150") && m.history_len() >= 130)
        .await;
    let (top, len) = (t.model.viewport_top(), t.model.history_len());
    let first = TerminalModel::row_text(&t.model.viewport()[0]);

    let size = TerminalSize {
        cols: 35,
        rows: 12,
        pixel_width: 0,
        pixel_height: 0,
    };
    let _: Value = t
        .client
        .call(Request::TerminalResize(TerminalResize {
            terminal_id: t.terminal,
            size,
        }))
        .await
        .unwrap();
    t.until("the new width", |m| m.cols() == 35).await;

    assert_eq!(t.model.viewport_top(), top);
    assert_eq!(t.model.history_len(), len);
    assert_eq!(TerminalModel::row_text(&t.model.viewport()[0]), first);

    stop(running).await;
}
