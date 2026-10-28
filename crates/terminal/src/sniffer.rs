//! OSC sniffer: extracts session metadata from the raw byte stream *before* it reaches the VT
//! engine (Alacritty ignores OSC 133 and does not surface OSC 7/9/777).
//!
//! It mirrors the termination rules of the engine's parser (`vte`): an OSC string ends at BEL,
//! at ST (`ESC \`), at any other `ESC` (which starts a new escape sequence) and at CAN/SUB.
//! State carries across calls, so sequences split over arbitrary read boundaries are handled.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use hutan_protocol::blocks::Block;

use crate::agent_status::{self, AgentStatus, Parsed};
use crate::queries::is_color_query;

/// Longest OSC payload that is buffered; longer strings are discarded when they end.
const MAX_OSC_LEN: usize = 8 * 1024;

/// Semantic prompt marks (OSC 133, FinalTerm/iTerm2 shell integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// Prompt start.
    A,
    /// Prompt end / command input start.
    B,
    /// Command output start.
    C,
    /// Command finished, with its exit code when the shell reported one.
    D { exit_code: Option<i32> },
}

/// Out-of-band terminal events for session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMeta {
    /// OSC 0 / OSC 2. Empty string = title cleared.
    Title(String),
    /// BEL outside a control sequence (reported by the engine).
    Bell,
    /// OSC 9 (`title` empty) and OSC 777;notify.
    Notification { title: String, body: String },
    /// OSC 7 working directory.
    Cwd(PathBuf),
    /// OSC 133 A/B/C/D.
    PromptMark(PromptMark),
    /// OSC 7501: the coding agent in the terminal reports what it is doing (OMP 18.12+).
    AgentStatus(AgentStatus),
    /// OSC 7501 `state=clear`: the agent left the terminal.
    AgentCleared,
    /// A block of the block index changed (`terminal.block` event, protocol 0.5): the whole
    /// block as it is now, in the order the changes happened.
    Block(Block),
}

/// What the block index needs from the byte stream, with the stream offset just past the byte
/// that ended the sequence (see [`OscSniffer::feed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signal {
    Mark(PromptMark),
    Cwd(PathBuf),
}

/// Longest sequence of digits in one CSI parameter that is still tracked exactly.
const PARAM_SATURATION: u32 = 1 << 20;

/// A sequence that erases the scrollback: `CSI 3 J` or `ESC c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HistoryClear {
    /// Offset of its `ESC` in the scanned bytes (0 when it began in an earlier call).
    pub(crate) start: usize,
    /// Offset just past its last byte.
    pub(crate) end: usize,
    /// `ESC c` (full reset: the screen is erased too) rather than `CSI 3 J`.
    pub(crate) reset: bool,
}

/// What one [`StreamScanner::feed`] found in the bytes besides metadata events: every offset is
/// just past the byte that ended the sequence, in stream order.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Scan {
    /// OSC 4/10/11/12 colour queries: the engine has handled one once it consumed `bytes[..end]`.
    pub(crate) color_queries: Vec<usize>,
    /// Prompt marks and working directories.
    pub(crate) signals: Vec<(usize, Signal)>,
    /// `CSI 16 t` (the engine does not answer it).
    pub(crate) cell_sizes: Vec<usize>,
    /// `CSI 3 J` and `ESC c`.
    pub(crate) erases: Vec<HistoryClear>,
    /// Byte ranges (start, end) of whole OSC 7 / OSC 133 strings, `ESC ]` through BEL or ST,
    /// that began and ended in this read: nothing in the engine reacts to them, so it can be
    /// run over the bytes around them instead.
    pub(crate) skips: Vec<(usize, usize)>,
}

/// The first parameter of a CSI that may still turn out to be `3` (for `CSI 3 J`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearParam {
    Empty,
    Three,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// After `ESC`; `start` is its offset in the current call, `None` when it was read earlier.
    Escape {
        start: Option<usize>,
    },
    Osc,
    /// Inside `ESC [`: `value` is the number read so far and `plain` stays true while the
    /// sequence consists of digits only (no `;`, `:`, private markers or intermediates);
    /// `param` follows the first parameter for `CSI 3 J`.
    Csi {
        start: Option<usize>,
        value: u32,
        plain: bool,
        param: ClearParam,
    },
}

/// One pass over the raw byte stream that finds everything Hutan has to know before the bytes
/// reach the VT engine: the OSC metadata (title, working directory, prompt marks,
/// notifications) and colour queries, `CSI 16 t`, and the sequences that erase the scrollback.
/// All of them are `ESC`-introduced, so the ground state skips to the next `ESC` a word at a
/// time (an output flood is almost all ground state).
///
/// OSC termination mirrors the engine's parser (`vte`): an OSC string ends at BEL, at ST
/// (`ESC \`), at any other `ESC` (which starts a new escape sequence) and at CAN/SUB. State
/// carries across calls, so sequences split over arbitrary read boundaries are handled.
#[derive(Debug)]
pub(crate) struct StreamScanner {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
    /// Offset in the current call of the `ESC` that opened the OSC string being read (`None`
    /// when it was opened by an earlier call).
    osc_start: Option<usize>,
    /// An OSC string for the engine to skip ended at an `ESC`, which is its ST when a `\`
    /// follows at once: where the string started.
    st_skip: Option<usize>,
}

/// How an OSC string ended.
#[derive(Debug, Clone, Copy)]
enum Ended {
    Bel,
    Esc,
    Abort,
}

/// Offset of the first `ESC` in `bytes`, eight bytes at a time.
fn find_esc(bytes: &[u8]) -> Option<usize> {
    const ESC: u64 = 0x1b1b_1b1b_1b1b_1b1b;
    const LOW: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let (words, rest) = bytes.as_chunks::<8>();
    let mut base = 0;
    for word in words {
        let x = u64::from_le_bytes(*word) ^ ESC;
        // A zero byte of `x` is an ESC; the classic "has zero byte" test is exact for the
        // lowest one, which is all that is needed.
        let zero = x.wrapping_sub(LOW) & !x & HIGH;
        if zero != 0 {
            return Some(base + (zero.trailing_zeros() / 8) as usize);
        }
        base += 8;
    }
    rest.iter().position(|&b| b == 0x1b).map(|at| base + at)
}

impl StreamScanner {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            overflow: false,
            osc_start: None,
            st_skip: None,
        }
    }

    /// Scans `bytes`, appending any complete metadata events to `out` and everything the
    /// emulator has to stop at to `scan`.
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<TerminalMeta>, scan: &mut Scan) {
        let mut i = 0;
        while i < bytes.len() {
            if self.state == State::Ground {
                // Fast path: only ESC can leave the ground state.
                match find_esc(&bytes[i..]) {
                    Some(offset) => {
                        self.state = State::Escape {
                            start: Some(i + offset),
                        };
                        i += offset + 1;
                        continue;
                    }
                    None => return,
                }
            }
            let byte = bytes[i];
            i += 1;
            // CAN and SUB abort a sequence (an OSC string completes); ESC restarts one.
            match byte {
                0x18 | 0x1a => {
                    self.st_skip = None;
                    if self.state == State::Osc {
                        self.finish(out, scan, i, Ended::Abort);
                    }
                    self.state = State::Ground;
                    continue;
                }
                0x1b => {
                    self.st_skip = None;
                    if self.state == State::Osc {
                        self.finish(out, scan, i, Ended::Esc);
                    }
                    self.state = State::Escape { start: Some(i - 1) };
                    continue;
                }
                _ => {}
            }
            self.state = match self.state {
                State::Ground => State::Ground,
                State::Escape { start } => {
                    let st_skip = self.st_skip.take();
                    match byte {
                        b']' => {
                            self.buf.clear();
                            self.overflow = false;
                            self.osc_start = start;
                            State::Osc
                        }
                        b'[' => State::Csi {
                            start,
                            value: 0,
                            plain: true,
                            param: ClearParam::Empty,
                        },
                        b'c' => {
                            scan.erases.push(HistoryClear {
                                start: start.unwrap_or(0),
                                end: i,
                                reset: true,
                            });
                            State::Ground
                        }
                        // `ESC \` right after the OSC string it terminates: part of it.
                        b'\\' => {
                            if let Some(first) = st_skip {
                                scan.skips.push((first, i));
                            }
                            State::Ground
                        }
                        _ => State::Ground,
                    }
                }
                State::Osc => {
                    match byte {
                        0x07 => {
                            self.finish(out, scan, i, Ended::Bel);
                            self.state = State::Ground;
                            continue;
                        }
                        0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                        _ if self.buf.len() < MAX_OSC_LEN => self.buf.push(byte),
                        _ => self.overflow = true,
                    }
                    State::Osc
                }
                State::Csi {
                    start,
                    value,
                    plain,
                    param,
                } => match byte {
                    b'0'..=b'9' => State::Csi {
                        start,
                        value: value
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte - b'0'))
                            .min(PARAM_SATURATION),
                        plain,
                        param: if param == ClearParam::Empty && byte == b'3' {
                            ClearParam::Three
                        } else {
                            ClearParam::Other
                        },
                    },
                    0x20..=0x3f => State::Csi {
                        start,
                        value,
                        plain: false,
                        param: ClearParam::Other,
                    },
                    b't' => {
                        if plain && value == 16 {
                            scan.cell_sizes.push(i);
                        }
                        State::Ground
                    }
                    b'J' => {
                        if param == ClearParam::Three {
                            scan.erases.push(HistoryClear {
                                start: start.unwrap_or(0),
                                end: i,
                                reset: false,
                            });
                        }
                        State::Ground
                    }
                    0x40..=0x7e => State::Ground,
                    // Other C0 controls and DEL are executed inside a sequence without ending it.
                    _ => self.state,
                },
            };
        }
        // Whatever is still open began before the next call's first byte.
        self.st_skip = None;
        self.osc_start = None;
        self.state = match self.state {
            State::Escape { .. } => State::Escape { start: None },
            State::Csi {
                value,
                plain,
                param,
                ..
            } => State::Csi {
                start: None,
                value,
                plain,
                param,
            },
            other => other,
        };
    }

    /// Completes the OSC string that ended just before offset `end`.
    fn finish(&mut self, out: &mut Vec<TerminalMeta>, scan: &mut Scan, end: usize, ended: Ended) {
        if !self.overflow {
            if let Some(meta) = parse_osc(&self.buf) {
                match &meta {
                    TerminalMeta::PromptMark(mark) => {
                        scan.signals.push((end, Signal::Mark(*mark)));
                    }
                    TerminalMeta::Cwd(cwd) => scan.signals.push((end, Signal::Cwd(cwd.clone()))),
                    _ => {}
                }
                // The engine ignores these two; when the whole string is in this read the
                // emulator does not hand it over (see `Scan::skips`).
                if matches!(meta, TerminalMeta::PromptMark(_) | TerminalMeta::Cwd(_))
                    && let Some(start) = self.osc_start
                {
                    match ended {
                        Ended::Bel => scan.skips.push((start, end)),
                        Ended::Esc => self.st_skip = Some(start),
                        Ended::Abort => {}
                    }
                }
                out.push(meta);
            }
            if is_color_query(&self.buf) {
                scan.color_queries.push(end);
            }
        }
        self.buf.clear();
        self.overflow = false;
    }
}

fn parse_osc(payload: &[u8]) -> Option<TerminalMeta> {
    let (ps, pt) = match payload.iter().position(|&b| b == b';') {
        Some(at) => (&payload[..at], &payload[at + 1..]),
        None => (payload, &payload[payload.len()..]),
    };
    match ps {
        b"7501" => Some(match agent_status::parse(pt)? {
            Parsed::Status(status) => TerminalMeta::AgentStatus(status),
            Parsed::Cleared => TerminalMeta::AgentCleared,
        }),
        b"0" | b"2" => Some(TerminalMeta::Title(
            String::from_utf8_lossy(pt).into_owned(),
        )),
        b"7" => parse_cwd(pt).map(TerminalMeta::Cwd),
        // `9;4;…` is the ConEmu progress report, not a notification.
        b"9" if pt.starts_with(b"4;") => None,
        b"9" => Some(TerminalMeta::Notification {
            title: String::new(),
            body: String::from_utf8_lossy(pt).into_owned(),
        }),
        b"777" => {
            let mut parts = pt.splitn(3, |&b| b == b';');
            if parts.next()? != b"notify" {
                return None;
            }
            let title = String::from_utf8_lossy(parts.next()?).into_owned();
            let body = parts
                .next()
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .into_owned();
            Some(TerminalMeta::Notification { title, body })
        }
        b"133" => parse_prompt_mark(pt).map(TerminalMeta::PromptMark),
        _ => None,
    }
}

fn parse_prompt_mark(pt: &[u8]) -> Option<PromptMark> {
    let mut parts = pt.split(|&b| b == b';');
    match parts.next()? {
        b"A" => Some(PromptMark::A),
        b"B" => Some(PromptMark::B),
        b"C" => Some(PromptMark::C),
        b"D" => {
            let exit_code = parts
                .next()
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.parse().ok());
            Some(PromptMark::D { exit_code })
        }
        _ => None,
    }
}

/// `file://host/path` → percent-decoded path. The host is ignored (shells report their own
/// hostname, which may differ from the daemon's `gethostname` under mDNS/VPN renames).
fn parse_cwd(url: &[u8]) -> Option<PathBuf> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&b| b == b'/')?..];
    let mut decoded = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'%'
            && let Some(byte) = path.get(i + 1..i + 3).and_then(hex_pair)
        {
            decoded.push(byte);
            i += 3;
            continue;
        }
        decoded.push(path[i]);
        i += 1;
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

fn hex_pair(pair: &[u8]) -> Option<u8> {
    let digit = |b: u8| char::from(b).to_digit(16);
    Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
}

#[cfg(test)]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(chunks: &[&[u8]]) -> Vec<TerminalMeta> {
        let mut scanner = StreamScanner::new();
        let mut out = Vec::new();
        for chunk in chunks {
            scanner.feed(chunk, &mut out, &mut Scan::default());
        }
        out
    }

    #[test]
    fn title_with_bel_and_st() {
        let got = sniff(&[b"\x1b]0;one\x07x\x1b]2;two\x1b\\"]);
        assert_eq!(
            got,
            [
                TerminalMeta::Title("one".into()),
                TerminalMeta::Title("two".into())
            ]
        );
    }

    #[test]
    fn split_across_every_byte() {
        let stream: &[u8] = b"hi\x1b]7;file://host/tmp/a%20b\x07\x1b]133;D;3\x1b\\\x1b]777;notify;T;B;with;semis\x07";
        let whole = sniff(&[stream]);
        let split: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(sniff(&split), whole);
        assert_eq!(
            whole,
            [
                TerminalMeta::Cwd(PathBuf::from("/tmp/a b")),
                TerminalMeta::PromptMark(PromptMark::D { exit_code: Some(3) }),
                TerminalMeta::Notification {
                    title: "T".into(),
                    body: "B;with;semis".into()
                },
            ]
        );
    }

    #[test]
    fn prompt_marks_and_notifications() {
        let got =
            sniff(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D\x07\x1b]9;hello\x07"]);
        assert_eq!(
            got,
            [
                TerminalMeta::PromptMark(PromptMark::A),
                TerminalMeta::PromptMark(PromptMark::B),
                TerminalMeta::PromptMark(PromptMark::C),
                TerminalMeta::PromptMark(PromptMark::D { exit_code: None }),
                TerminalMeta::Notification {
                    title: String::new(),
                    body: "hello".into()
                },
            ]
        );
    }

    #[test]
    fn progress_and_unknown_osc_ignored() {
        assert!(
            sniff(&[b"\x1b]9;4;1;50\x07\x1b]52;c;aGk=\x07\x1b]4;1;rgb:00/00/00\x07"]).is_empty()
        );
    }

    #[test]
    fn omp_program_status_in_its_own_format() {
        use crate::agent_status::AgentPhase;
        let status = |phase, kind: Option<&str>, message: Option<&str>| {
            TerminalMeta::AgentStatus(AgentStatus {
                phase,
                app: Some("omp".into()),
                kind: kind.map(str::to_owned),
                message: message.map(str::to_owned),
            })
        };
        // ST-terminated, as OMP writes it; one report of each state, the question as base64.
        let stream: &[u8] = b"\x1b]7501;state=idle:app=omp\x1b\\\
            \x1b]7501;state=working:app=omp\x1b\\\
            \x1b]7501;state=blocked:kind=permission:app=omp:msg=UnVuIHRoZSB0ZXN0cz8=\x1b\\\
            \x1b]7501;state=error:app=omp:msg=b2s=\x07\
            \x1b]7501;state=done:app=omp\x1b\\\
            \x1b]7501;state=clear\x1b\\";
        let whole = sniff(&[stream]);
        assert_eq!(
            whole,
            [
                status(AgentPhase::Idle, None, None),
                status(AgentPhase::Working, None, None),
                status(
                    AgentPhase::Blocked,
                    Some("permission"),
                    Some("Run the tests?")
                ),
                status(AgentPhase::Error, None, Some("ok")),
                status(AgentPhase::Done, None, None),
                TerminalMeta::AgentCleared,
            ]
        );
        let split: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(sniff(&split), whole);
    }

    #[test]
    fn a_program_status_that_is_not_one_is_ignored() {
        assert!(sniff(&[b"\x1b]7501;\x07\x1b]7501;state=sleeping\x07\x1b]7501\x07"]).is_empty());
    }

    #[test]
    fn esc_inside_osc_ends_it_and_starts_a_new_sequence() {
        let got = sniff(&[b"\x1b]0;a\x1b]0;b\x07"]);
        assert_eq!(
            got,
            [
                TerminalMeta::Title("a".into()),
                TerminalMeta::Title("b".into())
            ]
        );
    }

    #[test]
    fn oversized_osc_is_dropped_and_parser_recovers() {
        let mut stream = b"\x1b]0;".to_vec();
        stream.extend(std::iter::repeat_n(b'x', MAX_OSC_LEN + 10));
        stream.extend_from_slice(b"\x07\x1b]0;ok\x07");
        assert_eq!(sniff(&[&stream]), [TerminalMeta::Title("ok".into())]);
    }

    fn scan_all(chunks: &[&[u8]]) -> Scan {
        let mut scanner = StreamScanner::new();
        let mut total = Scan::default();
        for chunk in chunks {
            let mut scan = Scan::default();
            scanner.feed(chunk, &mut Vec::new(), &mut scan);
            total.cell_sizes.extend(scan.cell_sizes);
            total.erases.extend(scan.erases);
        }
        total
    }

    #[test]
    fn finds_cell_size_queries_in_one_chunk_and_across_chunks() {
        assert_eq!(scan_all(&[b"ab\x1b[16tcd\x1b[16t"]).cell_sizes, [7, 14]);
        assert_eq!(scan_all(&[b"\x1b[", b"1", b"6", b"t"]).cell_sizes, [1]);
    }

    #[test]
    fn ignores_other_sequences_than_cell_size_queries() {
        for stream in [
            &b"\x1b[18t"[..],
            b"\x1b[?16t",
            b"\x1b[16;1t",
            b"\x1b[116t",
            b"\x1b[16 t",
            b"\x1b[16m",
            b"\x1b[16\x18t",
            b"\x1b]16t",
            b"[16t",
        ] {
            assert!(scan_all(&[stream]).cell_sizes.is_empty(), "{stream:?}");
        }
    }

    fn skips(chunks: &[&[u8]]) -> Vec<Vec<(usize, usize)>> {
        let mut scanner = StreamScanner::new();
        chunks
            .iter()
            .map(|chunk| {
                let mut scan = Scan::default();
                scanner.feed(chunk, &mut Vec::new(), &mut scan);
                scan.skips
            })
            .collect()
    }

    #[test]
    fn whole_mark_and_directory_strings_are_skippable_others_are_not() {
        // BEL and ST terminated, back to back, with text around.
        assert_eq!(
            skips(&[b"a\x1b]133;A\x07b\x1b]7;file://h/tmp\x1b\\c\x1b]133;D;0\x07"]),
            [[(1, 9), (10, 28), (29, 39)]]
        );
        // Titles, notifications, colour queries, unknown marks and aborted strings stay.
        for stream in [
            &b"\x1b]0;t\x07"[..],
            b"\x1b]9;n\x07",
            b"\x1b]11;?\x07",
            b"\x1b]133;Z\x07",
            b"\x1b]7;nonsense\x07",
            b"\x1b]133;A\x18",
            b"\x1b]133;A\x1bx",
            b"\x1b]133;A\x1b",
        ] {
            assert_eq!(
                skips(&[stream]),
                [Vec::<(usize, usize)>::new()],
                "{stream:?}"
            );
        }
    }

    #[test]
    fn a_string_split_across_reads_is_never_skipped() {
        assert_eq!(
            skips(&[b"x\x1b]13", b"3;A\x07y", b"\x1b]133;B", b"\x1b", b"\\z"]),
            [vec![], vec![], vec![], vec![], vec![]]
        );
        // The next string, whole in one read, is.
        assert_eq!(
            skips(&[b"\x1b]133;B", b"\x07\x1b]133;A\x07"]),
            [vec![], vec![(1, 9)]]
        );
    }

    #[test]
    fn escape_restarts_a_sequence() {
        assert_eq!(scan_all(&[b"\x1b[1\x1b[16t"]).cell_sizes, [8]);
    }

    #[test]
    fn finds_erase_saved_lines_and_reset() {
        let found = scan_all(&[b"ab\x1b[3Jcd\x1bcef\x1b[2J\x1b[?3J\x1b[13J\x1b[3;J"]).erases;
        assert_eq!(
            found,
            [
                HistoryClear {
                    start: 2,
                    end: 6,
                    reset: false
                },
                HistoryClear {
                    start: 8,
                    end: 10,
                    reset: true
                },
            ]
        );
    }

    #[test]
    fn an_erase_split_across_reads_starts_at_zero_of_its_last_read() {
        let found = scan_all(&[b"x\x1b", b"[3", b"Jy"]).erases;
        assert_eq!(
            found,
            [HistoryClear {
                start: 0,
                end: 1,
                reset: false
            }]
        );
        let found = scan_all(&[b"\x1bc"]).erases;
        assert_eq!(found.len(), 1);
        assert!(found[0].reset);
    }

    #[test]
    fn find_esc_agrees_with_position_at_every_alignment() {
        let mut bytes = vec![b'a'; 70];
        for esc_at in 0..bytes.len() {
            bytes[esc_at] = 0x1b;
            for from in 0..=esc_at.min(9) {
                assert_eq!(
                    find_esc(&bytes[from..]),
                    bytes[from..].iter().position(|&b| b == 0x1b),
                    "esc at {esc_at}, from {from}"
                );
            }
            bytes[esc_at] = b'a';
        }
        assert_eq!(find_esc(&bytes), None);
        // Bytes next to ESC in value must not be mistaken for it (the borrow of the classic
        // zero-byte test can only flag higher bytes).
        assert_eq!(
            find_esc(&[0x1a, 0x1c, 0x1b, 0x1a, 0x1c, 0x1b, 0, 0x9b, 0x1b]),
            Some(2)
        );
    }

    /// xorshift64*: deterministic, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// A stream of text, the sequences the scanners care about, near misses and noise.
    fn stream(rng: &mut Rng, len: usize) -> Vec<u8> {
        const PIECES: &[&[u8]] = &[
            b"plain text line\r\n",
            b"y\n",
            b"\x1b[16t",
            b"\x1b[3J",
            b"\x1bc",
            b"\x1b[31m",
            b"\x1b[?3J",
            b"\x1b[13J",
            b"\x1b[3;J",
            b"\x1b[18t",
            b"\x1b[116t",
            b"\x1b[16;1t",
            b"\x1b[2J",
            b"\x1b]133;A\x07",
            b"\x1b]133;B\x1b\\",
            b"\x1b]133;C\x07",
            b"\x1b]133;D;7\x07",
            b"\x1b]7;file://host/tmp/a%20b\x07",
            b"\x1b]0;title\x07",
            b"\x1b]2;two\x1b\\",
            b"\x1b]9;note\x07",
            b"\x1b]9;4;1;50\x07",
            b"\x1b]777;notify;T;B\x07",
            b"\x1b]11;?\x07",
            b"\x1b]4;1;?\x1b\\",
            b"\x1b]10;?",
            b"\x1b]133;A",
            b"\x1b[",
            b"\x1b[16",
            b"\x1b",
            b"\x1b\x1b",
            b"\x18",
            b"\x1a",
            b"\x07",
            b"\x7f",
            b"\x1b]",
            b"\x01\x02\x03",
            b"\xe6\xbc\xa2\xe5\xad\x97",
            b"\x9b16t",
        ];
        let mut out = Vec::with_capacity(len + 64);
        while out.len() < len {
            if rng.below(40) == 0 {
                // A payload longer than the OSC buffer.
                out.extend_from_slice(b"\x1b]0;");
                out.extend(std::iter::repeat_n(b'x', MAX_OSC_LEN + rng.below(40)));
                if rng.below(2) == 0 {
                    out.push(0x07);
                }
            } else {
                out.extend_from_slice(PIECES[rng.below(PIECES.len())]);
            }
        }
        out
    }

    #[test]
    fn one_pass_equals_the_three_scanners_on_random_streams_and_chunkings() {
        let mut events = 0;
        for seed in 1..=60u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let len = 2_000 + rng.below(30_000);
            let bytes = stream(&mut rng, len);
            let mut merged = StreamScanner::new();
            let mut separate = reference::Separate::new();
            let (mut merged_meta, mut separate_meta) = (Vec::new(), Vec::new());
            let mut at = 0;
            while at < bytes.len() {
                // Mostly small reads (every boundary inside a sequence gets hit), sometimes
                // large ones.
                let take = match rng.below(8) {
                    0 => 1,
                    1..=4 => 1 + rng.below(24),
                    5 | 6 => 1 + rng.below(700),
                    _ => 1 + rng.below(9000),
                }
                .min(bytes.len() - at);
                let chunk = &bytes[at..at + take];
                let mut found = Scan::default();
                merged.feed(chunk, &mut merged_meta, &mut found);
                let expected = separate.feed(chunk, &mut separate_meta);
                // The old scanners know nothing of skips; they are checked below.
                let skips = std::mem::take(&mut found.skips);
                for &(start, end) in &skips {
                    assert!(start < end && end <= chunk.len(), "seed {seed}: {skips:?}");
                    assert_eq!(&chunk[start..start + 2], b"\x1b]", "seed {seed}");
                    assert!(
                        chunk[end - 1] == 0x07 || chunk[end - 2..end] == *b"\x1b\\",
                        "seed {seed}"
                    );
                    // A mark or directory string whose signal is reported at its end or one
                    // byte before it (ST).
                    assert!(
                        expected
                            .signals
                            .iter()
                            .any(|(at, _)| *at == end || *at + 1 == end),
                        "seed {seed}: {skips:?}"
                    );
                }
                events += skips.len();
                assert_eq!(found, expected, "seed {seed}, chunk at {at}+{take}");
                events += found.color_queries.len()
                    + found.signals.len()
                    + found.cell_sizes.len()
                    + found.erases.len();
                at += take;
            }
            assert_eq!(merged_meta, separate_meta, "seed {seed}");
            events += merged_meta.len();
        }
        assert!(events > 500, "the streams found only {events} events");
    }
}
