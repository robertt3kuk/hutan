//! Renderer tests: the bytes `HostRenderer` produces are fed to a `vt100::Parser` sized like the
//! host, and the resulting screen is compared with what the daemon's render DTOs describe.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use hutan_cli::HostRenderer;
use hutan_protocol::render::{
    CellFlags, Color, Cursor, CursorShape, Link, Modes, MouseMode, Palette, Row, Run,
    TerminalFrame, TerminalSnapshot,
};

const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";

fn run(text: &str) -> Run {
    Run {
        text: text.to_owned(),
        fg: Color::Default,
        bg: Color::Default,
        flags: CellFlags::empty(),
        underline_color: None,
        link: None,
    }
}

fn row(text: &str) -> Row {
    Row {
        runs: if text.is_empty() {
            vec![]
        } else {
            vec![run(text)]
        },
        wrapped: false,
    }
}

fn runs(runs: Vec<Run>) -> Row {
    Row {
        runs,
        wrapped: false,
    }
}

fn cursor(col: u16, row: u16) -> Cursor {
    Cursor {
        col,
        row,
        shape: CursorShape::Block,
        visible: true,
        blinking: false,
    }
}

fn snapshot(cols: u16, lines: Vec<Row>) -> TerminalSnapshot {
    TerminalSnapshot {
        frame: 1,
        cols,
        rows: u16::try_from(lines.len()).unwrap(),
        cursor: cursor(0, 0),
        modes: Modes::default(),
        palette: Palette::default(),
        title: None,
        lines,
        links: vec![],
        history_len: 0,
        viewport_top: 0,
    }
}

fn frame(dirty: Vec<(u16, Row)>) -> TerminalFrame {
    TerminalFrame {
        frame: 2,
        scrolled: 0,
        dirty,
        cursor: cursor(0, 0),
        modes: None,
        title: None,
        links: vec![],
        history_len: 0,
        viewport_top: 0,
    }
}

/// A renderer and a host terminal emulator of the same size.
struct Host {
    renderer: HostRenderer,
    parser: vt100::Parser,
}

impl Host {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            renderer: HostRenderer::new(cols, rows),
            parser: vt100::Parser::new(rows, cols, 0),
        }
    }

    fn snapshot(&mut self, s: &TerminalSnapshot) -> Vec<u8> {
        let mut out = Vec::new();
        self.renderer.apply_snapshot(s, &mut out);
        self.parser.process(&out);
        out
    }

    fn frame(&mut self, f: &TerminalFrame) -> Vec<u8> {
        let mut out = Vec::new();
        self.renderer.apply_frame(f, &mut out);
        self.parser.process(&out);
        out
    }

    fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        let mut out = Vec::new();
        self.renderer.resize_host(cols, rows, &mut out);
        self.parser.screen_mut().set_size(rows, cols);
        self.parser.process(&out);
        out
    }

    fn line(&self, row: u16) -> String {
        let (_, cols) = self.parser.screen().size();
        (0..cols)
            .map(|c| {
                let cell = self.parser.screen().cell(row, c).unwrap();
                if cell.has_contents() {
                    cell.contents().to_owned()
                } else if cell.is_wide_continuation() {
                    String::new()
                } else {
                    " ".to_owned()
                }
            })
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn cell(&self, row: u16, col: u16) -> &vt100::Cell {
        self.parser.screen().cell(row, col).unwrap()
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn numbered_lines(n: usize) -> Vec<Row> {
    (0..n).map(|i| row(&format!("line{i}"))).collect()
}

#[test]
fn snapshot_paints_text_on_every_row() {
    let mut host = Host::new(80, 24);
    host.snapshot(&snapshot(80, numbered_lines(24)));
    for i in 0..24 {
        assert_eq!(host.line(i), format!("line{i}"));
    }
}

#[test]
fn colors_and_styles_arrive_on_the_host() {
    let mut styled = run("a");
    styled.fg = Color::Indexed(1);
    styled.flags = CellFlags::BOLD | CellFlags::ITALIC | CellFlags::UNDERLINE;
    let mut bright = run("b");
    bright.fg = Color::Indexed(9);
    bright.bg = Color::Indexed(200);
    bright.flags = CellFlags::INVERSE | CellFlags::DIM;
    let mut rgb = run("c");
    rgb.fg = Color::Rgb(10, 20, 30);
    rgb.bg = Color::Rgb(40, 50, 60);
    let plain = run("d");

    let mut host = Host::new(20, 3);
    host.snapshot(&snapshot(
        20,
        vec![runs(vec![styled, bright, rgb, plain]), row(""), row("")],
    ));

    let a = host.cell(0, 0);
    assert_eq!(a.contents(), "a");
    assert_eq!(a.fgcolor(), vt100::Color::Idx(1));
    assert!(a.bold() && a.italic() && a.underline() && !a.inverse());
    let b = host.cell(0, 1);
    assert_eq!(b.fgcolor(), vt100::Color::Idx(9));
    assert_eq!(b.bgcolor(), vt100::Color::Idx(200));
    assert!(b.inverse() && b.dim() && !b.bold());
    let c = host.cell(0, 2);
    assert_eq!(c.fgcolor(), vt100::Color::Rgb(10, 20, 30));
    assert_eq!(c.bgcolor(), vt100::Color::Rgb(40, 50, 60));
    let d = host.cell(0, 3);
    assert_eq!(d.fgcolor(), vt100::Color::Default);
    assert_eq!(d.bgcolor(), vt100::Color::Default);
    assert!(!d.bold() && !d.italic() && !d.underline() && !d.inverse() && !d.dim());
}

#[test]
fn sgr_sequences_cover_every_style_flag() {
    let cases: &[(CellFlags, &str)] = &[
        (CellFlags::BOLD, "\x1b[0;1m"),
        (CellFlags::DIM, "\x1b[0;2m"),
        (CellFlags::ITALIC, "\x1b[0;3m"),
        (CellFlags::UNDERLINE, "\x1b[0;4m"),
        (CellFlags::DOUBLE_UNDERLINE, "\x1b[0;4:2m"),
        (CellFlags::UNDERCURL, "\x1b[0;4:3m"),
        (CellFlags::DOTTED_UNDERLINE, "\x1b[0;4:4m"),
        (CellFlags::DASHED_UNDERLINE, "\x1b[0;4:5m"),
        (CellFlags::INVERSE, "\x1b[0;7m"),
        (CellFlags::HIDDEN, "\x1b[0;8m"),
        (CellFlags::STRIKETHROUGH, "\x1b[0;9m"),
    ];
    for (flags, expected) in cases {
        let mut r = run("x");
        r.flags = *flags;
        let mut host = Host::new(10, 1);
        let out = host.snapshot(&snapshot(10, vec![runs(vec![r])]));
        assert!(
            contains(&out, expected),
            "{flags:?}: expected {expected:?} in {:?}",
            String::from_utf8_lossy(&out)
        );
    }
}

#[test]
fn undercurl_and_underline_color_use_colon_forms() {
    let mut r = run("x");
    r.flags = CellFlags::UNDERCURL;
    r.underline_color = Some(Color::Rgb(1, 2, 3));
    let mut indexed = run("y");
    indexed.flags = CellFlags::UNDERLINE;
    indexed.underline_color = Some(Color::Indexed(5));
    let mut host = Host::new(10, 1);
    let out = host.snapshot(&snapshot(10, vec![runs(vec![r, indexed])]));
    assert!(contains(&out, "\x1b[0;4:3;58:2::1:2:3m"));
    assert!(contains(&out, "\x1b[0;4;58:5:5m"));
}

#[test]
fn color_encodings_distinguish_ansi_bright_and_256() {
    let mut r = run("x");
    r.fg = Color::Indexed(3);
    r.bg = Color::Indexed(12);
    let mut s = run("y");
    s.fg = Color::Indexed(100);
    s.bg = Color::Rgb(7, 8, 9);
    let mut host = Host::new(10, 1);
    let out = host.snapshot(&snapshot(10, vec![runs(vec![r, s])]));
    assert!(contains(&out, "\x1b[0;33;104m"));
    assert!(contains(&out, "\x1b[0;38;5;100;48;2;7;8;9m"));
}

#[test]
fn style_is_reset_before_the_line_is_erased() {
    let mut r = run("hi");
    r.bg = Color::Indexed(4);
    let mut host = Host::new(10, 1);
    let out = host.snapshot(&snapshot(10, vec![runs(vec![r])]));
    assert!(contains(&out, "hi\x1b[0m\x1b[K"));
    assert_eq!(host.cell(0, 5).bgcolor(), vt100::Color::Default);
}

#[test]
fn wide_runs_occupy_two_columns() {
    let mut wide = run("世界");
    wide.flags = CellFlags::WIDE;
    let mut host = Host::new(10, 1);
    host.snapshot(&snapshot(10, vec![runs(vec![wide, run("x")])]));
    assert!(host.cell(0, 0).is_wide());
    assert_eq!(host.cell(0, 0).contents(), "世");
    assert!(host.cell(0, 1).is_wide_continuation());
    assert_eq!(host.cell(0, 2).contents(), "界");
    assert_eq!(host.cell(0, 4).contents(), "x");
}

#[test]
fn combining_marks_take_no_column() {
    let mut host = Host::new(10, 1);
    host.snapshot(&snapshot(10, vec![row("e\u{301}x")]));
    assert_eq!(host.cell(0, 0).contents(), "e\u{301}");
    assert_eq!(host.cell(0, 1).contents(), "x");
}

#[test]
fn cursor_position_visibility_and_shape() {
    let mut host = Host::new(20, 5);
    let mut s = snapshot(20, vec![row("abc"), row(""), row(""), row(""), row("")]);
    s.cursor = Cursor {
        col: 7,
        row: 3,
        shape: CursorShape::Bar,
        visible: true,
        blinking: true,
    };
    let out = host.snapshot(&s);
    assert_eq!(host.parser.screen().cursor_position(), (3, 7));
    assert!(!host.parser.screen().hide_cursor());
    assert!(contains(&out, "\x1b[5 q"));

    let mut f = frame(vec![]);
    f.cursor = Cursor {
        col: 2,
        row: 1,
        shape: CursorShape::Underline,
        visible: true,
        blinking: false,
    };
    let out = host.frame(&f);
    assert_eq!(host.parser.screen().cursor_position(), (1, 2));
    assert!(contains(&out, "\x1b[4 q"));

    f.cursor.visible = false;
    host.frame(&f);
    assert!(host.parser.screen().hide_cursor());

    f.cursor.visible = true;
    f.cursor.shape = CursorShape::HollowBlock;
    let out = host.frame(&f);
    assert!(!host.parser.screen().hide_cursor());
    assert!(contains(&out, "\x1b[2 q"));
}

#[test]
fn frame_rewrites_only_dirty_rows() {
    let full: Vec<Row> = (0..24).map(|_| row(&"x".repeat(80))).collect();
    let mut host = Host::new(80, 24);
    let snapshot_bytes = host.snapshot(&snapshot(80, full));
    assert!(snapshot_bytes.len() > 24 * 80);

    let mut f = frame(vec![(11, row("changed"))]);
    f.cursor = cursor(3, 20);
    let out = host.frame(&f);
    assert!(out.len() < 100, "frame wrote {} bytes", out.len());
    assert!(contains(&out, "\x1b[12;1Hchanged"));
    for other in (0..24).filter(|r| *r != 11) {
        assert!(!contains(&out, &format!("\x1b[{};1H", other + 1)));
    }
    assert_eq!(host.line(11), "changed");
    assert_eq!(host.line(10), "x".repeat(80));
    assert_eq!(host.line(12), "x".repeat(80));
}

#[test]
fn rows_the_host_already_shows_are_not_rewritten() {
    let mut host = Host::new(20, 3);
    host.snapshot(&snapshot(20, vec![row("one"), row("two"), row("three")]));
    let out = host.frame(&frame(vec![(1, row("two"))]));
    assert!(out.is_empty(), "unexpected output {out:?}");
}

#[test]
fn cursor_only_frame_moves_the_cursor_and_nothing_else() {
    let mut host = Host::new(20, 3);
    host.snapshot(&snapshot(20, vec![row("one"), row("two"), row("three")]));
    let mut f = frame(vec![]);
    f.cursor = cursor(4, 2);
    let out = host.frame(&f);
    assert_eq!(out, b"\x1b[?2026h\x1b[3;5H\x1b[?2026l");
    assert_eq!(host.parser.screen().cursor_position(), (2, 4));
}

#[test]
fn every_frame_and_snapshot_is_one_synchronized_update() {
    let mut host = Host::new(20, 3);
    let out = host.snapshot(&snapshot(20, vec![row("one"), row("two"), row("three")]));
    assert!(out.starts_with(BEGIN) && out.ends_with(END));
    assert_eq!(out.windows(BEGIN.len()).filter(|w| *w == BEGIN).count(), 1);
    let out = host.frame(&frame(vec![(0, row("uno"))]));
    assert!(out.starts_with(BEGIN) && out.ends_with(END));
    let mut out = Vec::new();
    host.renderer.resize_host(10, 2, &mut out);
    assert!(out.starts_with(BEGIN) && out.ends_with(END));
}

#[test]
fn scrolled_frame_uses_host_scroll_and_rewrites_only_new_rows() {
    let mut host = Host::new(80, 24);
    host.snapshot(&snapshot(80, numbered_lines(24)));

    let mut f = frame(vec![(23, row("line24"))]);
    f.scrolled = 1;
    let out = host.frame(&f);
    assert!(out.len() < 100, "frame wrote {} bytes", out.len());
    assert!(contains(&out, "\x1b[1S"));
    for i in 0..24 {
        assert_eq!(host.line(i), format!("line{}", i + 1));
    }

    let mut f = frame(vec![(0, row("top")), (1, row("line1"))]);
    f.scrolled = -2;
    let out = host.frame(&f);
    assert!(out.len() < 130, "frame wrote {} bytes", out.len());
    assert!(contains(&out, "\x1b[2T"));
    assert_eq!(host.line(0), "top");
    assert_eq!(host.line(1), "line1");
    assert_eq!(host.line(2), "line1");
    assert_eq!(host.line(23), "line22");
}

#[test]
fn scroll_larger_than_the_screen_blanks_it() {
    let mut host = Host::new(20, 4);
    host.snapshot(&snapshot(20, numbered_lines(4)));
    let mut f = frame(vec![(3, row("fresh"))]);
    f.scrolled = 100;
    host.frame(&f);
    assert_eq!(host.line(0), "");
    assert_eq!(host.line(3), "fresh");
}

#[test]
fn smaller_host_clips_columns_rows_and_hides_offscreen_cursor() {
    let remote = vec![
        row("abcdefghij"),
        row("0123456789"),
        row("second"),
        row("below"),
        row("gone"),
    ];
    let mut s = snapshot(10, remote);
    s.cursor = cursor(8, 4);
    let mut host = Host::new(6, 3);
    host.snapshot(&s);
    assert_eq!(host.line(0), "abcdef");
    assert_eq!(host.line(1), "012345");
    assert_eq!(host.line(2), "second");
    assert!(host.parser.screen().hide_cursor());

    // A frame touching rows beyond the host changes nothing visible.
    let mut f = frame(vec![(4, row("zzz"))]);
    f.cursor = s.cursor;
    let out = host.frame(&f);
    assert!(out.is_empty(), "unexpected output {out:?}");
}

#[test]
fn wide_character_straddling_the_clip_edge_is_dropped() {
    let mut wide = run("世");
    wide.flags = CellFlags::WIDE;
    let mut host = Host::new(6, 1);
    host.snapshot(&snapshot(8, vec![runs(vec![run("abcde"), wide])]));
    assert_eq!(host.line(0), "abcde");
    assert!(!host.cell(0, 5).has_contents());
}

#[test]
fn larger_host_pads_with_blank_space() {
    let mut host = Host::new(12, 6);
    host.snapshot(&snapshot(4, vec![row("abcd"), row("efgh")]));
    assert_eq!(host.line(0), "abcd");
    assert_eq!(host.line(1), "efgh");
    for r in 2..6 {
        assert_eq!(host.line(r), "");
    }
}

#[test]
fn smaller_remote_snapshot_erases_what_the_previous_one_drew() {
    let mut host = Host::new(10, 5);
    host.snapshot(&snapshot(10, numbered_lines(5)));
    host.snapshot(&snapshot(4, vec![row("ab"), row("cd")]));
    assert_eq!(host.line(0), "ab");
    assert_eq!(host.line(1), "cd");
    for r in 2..5 {
        assert_eq!(host.line(r), "");
    }
}

#[test]
fn resize_host_repaints_clipped_and_padded() {
    let mut host = Host::new(20, 6);
    let mut s = snapshot(10, numbered_lines(6));
    s.cursor = cursor(3, 5);
    host.snapshot(&s);

    host.resize(5, 3);
    assert_eq!(host.line(0), "line0");
    assert_eq!(host.line(2), "line2");
    assert!(host.parser.screen().hide_cursor(), "cursor is off screen");

    host.resize(30, 8);
    for r in 0..6 {
        assert_eq!(host.line(r), format!("line{r}"));
    }
    assert_eq!(host.line(6), "");
    assert_eq!(host.parser.screen().cursor_position(), (5, 3));
    assert!(!host.parser.screen().hide_cursor());
}

#[test]
fn input_modes_are_mirrored_and_withdrawn() {
    let mut host = Host::new(20, 3);
    let mut s = snapshot(20, vec![row(""), row(""), row("")]);
    s.modes = Modes {
        app_cursor: true,
        bracketed_paste: true,
        mouse: MouseMode::Drag,
        mouse_sgr: true,
        focus_events: true,
        kitty_keyboard: 5,
        ..Modes::default()
    };
    let out = host.snapshot(&s);
    let screen = host.parser.screen();
    assert!(screen.application_cursor());
    assert!(screen.bracketed_paste());
    assert_eq!(
        screen.mouse_protocol_mode(),
        vt100::MouseProtocolMode::ButtonMotion
    );
    assert_eq!(
        screen.mouse_protocol_encoding(),
        vt100::MouseProtocolEncoding::Sgr
    );
    assert!(contains(&out, "\x1b[?1004h"));
    assert!(contains(&out, "\x1b[>5u"));

    // Mouse mode switches by turning the old one off; kitty flags replace, not stack.
    let mut f = frame(vec![]);
    f.modes = Some(Modes {
        app_cursor: true,
        mouse: MouseMode::Motion,
        mouse_sgr: true,
        kitty_keyboard: 1,
        ..Modes::default()
    });
    let out = host.frame(&f);
    let screen = host.parser.screen();
    assert!(!screen.bracketed_paste());
    assert_eq!(
        screen.mouse_protocol_mode(),
        vt100::MouseProtocolMode::AnyMotion
    );
    assert!(contains(&out, "\x1b[?1002l\x1b[?1003h"));
    assert!(contains(&out, "\x1b[?1004l"));
    assert!(contains(&out, "\x1b[<u\x1b[>1u"));

    // Unchanged modes cost nothing.
    f.modes = Some(f.modes.unwrap());
    assert!(host.frame(&f).is_empty());

    f.modes = Some(Modes::default());
    let out = host.frame(&f);
    let screen = host.parser.screen();
    assert!(!screen.application_cursor());
    assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
    assert!(contains(&out, "\x1b[<u"));
    assert!(!contains(&out, "\x1b[>"));
}

#[test]
fn enter_and_leave_bracket_the_session() {
    let mut host = Host::new(20, 3);
    let mut out = Vec::new();
    host.renderer.enter(&mut out);
    host.parser.process(&out);
    assert!(host.parser.screen().alternate_screen());

    let mut s = snapshot(20, vec![row("hi"), row(""), row("")]);
    s.modes = Modes {
        bracketed_paste: true,
        mouse: MouseMode::Click,
        mouse_sgr: true,
        app_keypad: true,
        kitty_keyboard: 3,
        ..Modes::default()
    };
    host.snapshot(&s);
    assert!(host.parser.screen().bracketed_paste());

    let mut out = Vec::new();
    host.renderer.leave(&mut out);
    host.parser.process(&out);
    let screen = host.parser.screen();
    assert!(!screen.alternate_screen());
    assert!(!screen.bracketed_paste());
    assert!(!screen.application_keypad());
    assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
    assert!(!screen.hide_cursor());
    assert!(contains(&out, "\x1b[<u"));
    assert!(contains(&out, "\x1b[0 q"));

    // Re-entering (after a suspend) turns the mirrored modes back on.
    let mut out = Vec::new();
    host.renderer.enter(&mut out);
    host.parser.process(&out);
    assert!(host.parser.screen().bracketed_paste());
    assert!(contains(&out, "\x1b[>3u"));
}

#[test]
fn hyperlinks_use_osc8_and_close_at_the_end_of_the_row() {
    let mut linked = run("docs");
    linked.link = Some(7);
    let mut s = snapshot(20, vec![runs(vec![run("see "), linked, run(" now")])]);
    s.links = vec![Link {
        id: 7,
        uri: "https://example.com/a?b=c".into(),
    }];
    let mut host = Host::new(20, 1);
    let out = host.snapshot(&s);
    assert!(contains(
        &out,
        "see \x1b]8;id=hutan-7;https://example.com/a?b=c\x1b\\docs\x1b]8;;\x1b\\ now"
    ));
    assert_eq!(host.line(0), "see docs now");

    // A link running to the end of the row is still closed.
    let mut tail = run("end");
    tail.link = Some(7);
    let mut f = frame(vec![(0, runs(vec![tail]))]);
    f.links = vec![];
    let out = host.frame(&f);
    assert!(contains(&out, "end\x1b]8;;\x1b\\"));
}

#[test]
fn links_learned_in_a_frame_are_usable_in_that_frame() {
    let mut s = snapshot(20, vec![row("")]);
    s.links = vec![];
    let mut host = Host::new(20, 1);
    host.snapshot(&s);
    let mut linked = run("go");
    linked.link = Some(2);
    let mut f = frame(vec![(0, runs(vec![linked]))]);
    f.links = vec![Link {
        id: 2,
        uri: "https://x.test/".into(),
    }];
    let out = host.frame(&f);
    assert!(contains(&out, "\x1b]8;id=hutan-2;https://x.test/\x1b\\go"));
}

#[test]
fn remote_content_cannot_inject_escape_sequences() {
    let mut evil = run("a\x1b[2Jb\x07c");
    evil.link = Some(1);
    let mut s = snapshot(20, vec![runs(vec![evil])]);
    s.links = vec![Link {
        id: 1,
        uri: "https://x.test/\x1b]0;pwn\x07".into(),
    }];
    let mut host = Host::new(20, 1);
    let out = host.snapshot(&s);
    assert!(!contains(&out, "\x1b[2Jb"));
    assert!(!contains(&out, "pwn"));
    assert!(!out.contains(&0x07));
    assert!(!contains(&out, "\x1b]8;id"));
    assert_eq!(host.line(0), "a [2Jb c");
}

#[test]
fn frames_before_the_first_snapshot_are_ignored() {
    let mut host = Host::new(20, 3);
    assert!(host.frame(&frame(vec![(0, row("early"))])).is_empty());
    assert_eq!(host.line(0), "");
}

#[test]
fn snapshot_after_frames_replaces_everything() {
    let mut host = Host::new(20, 3);
    host.snapshot(&snapshot(20, numbered_lines(3)));
    host.frame(&frame(vec![(1, row("edited"))]));
    host.snapshot(&snapshot(20, vec![row("new0"), row("new1"), row("new2")]));
    assert_eq!(host.line(0), "new0");
    assert_eq!(host.line(1), "new1");
    assert_eq!(host.line(2), "new2");
}
