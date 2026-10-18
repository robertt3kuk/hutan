//! The three scanners the emulator ran one after the other over every read before
//! [`super::StreamScanner`] replaced them, kept verbatim as the reference the merged pass is
//! compared with (byte offsets, event order and state across arbitrary chunking must be
//! identical).
#![allow(clippy::expect_used, clippy::unwrap_used)] // tests assert by panicking

use super::{
    HistoryClear, MAX_OSC_LEN, PARAM_SATURATION, Signal, TerminalMeta, is_color_query, parse_osc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OscState {
    Ground,
    Escape,
    Osc,
}

#[derive(Debug)]
pub(crate) struct OscSniffer {
    state: OscState,
    buf: Vec<u8>,
    overflow: bool,
}

impl OscSniffer {
    pub(crate) fn new() -> Self {
        Self {
            state: OscState::Ground,
            buf: Vec::new(),
            overflow: false,
        }
    }

    /// Scans `bytes`, appending any complete metadata events to `out` and, for every colour
    /// query (OSC 4/10/11/12 with a `?`), the offset just past the byte that ended it to
    /// `color_queries`: the engine has handled the query once it consumed `bytes[..offset]`.
    /// Prompt marks and working directories also go to `signals`, each with its end offset.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<TerminalMeta>,
        color_queries: &mut Vec<usize>,
        signals: &mut Vec<(usize, Signal)>,
    ) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                OscState::Ground => {
                    // Fast path: only ESC can leave the ground state.
                    match bytes[i..].iter().position(|&b| b == 0x1b) {
                        Some(offset) => {
                            i += offset + 1;
                            self.state = OscState::Escape;
                        }
                        None => return,
                    }
                }
                OscState::Escape => {
                    match bytes[i] {
                        b']' => {
                            self.buf.clear();
                            self.overflow = false;
                            self.state = OscState::Osc;
                        }
                        0x1b => {}
                        _ => self.state = OscState::Ground,
                    }
                    i += 1;
                }
                OscState::Osc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        0x07 | 0x18 | 0x1a => {
                            self.finish(out, color_queries, signals, i);
                            self.state = OscState::Ground;
                        }
                        0x1b => {
                            self.finish(out, color_queries, signals, i);
                            self.state = OscState::Escape;
                        }
                        0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                        _ if self.buf.len() < MAX_OSC_LEN => self.buf.push(b),
                        _ => self.overflow = true,
                    }
                }
            }
        }
    }

    /// Completes the OSC string that ended just before offset `end`.
    fn finish(
        &mut self,
        out: &mut Vec<TerminalMeta>,
        color_queries: &mut Vec<usize>,
        signals: &mut Vec<(usize, Signal)>,
        end: usize,
    ) {
        if !self.overflow {
            if let Some(meta) = parse_osc(&self.buf) {
                match &meta {
                    TerminalMeta::PromptMark(mark) => signals.push((end, Signal::Mark(*mark))),
                    TerminalMeta::Cwd(cwd) => signals.push((end, Signal::Cwd(cwd.clone()))),
                    _ => {}
                }
                out.push(meta);
            }
            if is_color_query(&self.buf) {
                color_queries.push(end);
            }
        }
        self.buf.clear();
        self.overflow = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CsiState {
    Ground,
    Escape,
    /// Inside `ESC [`: `value` is the number read so far; `plain` stays true while the
    /// sequence consists of digits only (no `;`, `:`, private markers or intermediates).
    Csi {
        value: u32,
        plain: bool,
    },
}

/// Finds `CSI 1 6 t` in a byte stream.
#[derive(Debug)]
pub(crate) struct CellSizeScanner {
    state: CsiState,
}

impl CellSizeScanner {
    pub(crate) fn new() -> Self {
        Self {
            state: CsiState::Ground,
        }
    }

    /// Appends to `ends` the offset just past every `CSI 16 t` completed in `bytes`.
    pub(crate) fn feed(&mut self, bytes: &[u8], ends: &mut Vec<usize>) {
        for (offset, &byte) in bytes.iter().enumerate() {
            self.state = match (self.state, byte) {
                // CAN and SUB abort a sequence; ESC restarts one.
                (_, 0x18 | 0x1a) => CsiState::Ground,
                (_, 0x1b) => CsiState::Escape,
                (CsiState::Ground, _) => CsiState::Ground,
                (CsiState::Escape, b'[') => CsiState::Csi {
                    value: 0,
                    plain: true,
                },
                (CsiState::Escape, _) => CsiState::Ground,
                (CsiState::Csi { value, plain }, b'0'..=b'9') => CsiState::Csi {
                    value: value
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'))
                        .min(PARAM_SATURATION),
                    plain,
                },
                (CsiState::Csi { value, .. }, 0x20..=0x3f) => CsiState::Csi {
                    value,
                    plain: false,
                },
                (CsiState::Csi { value, plain }, b't') => {
                    if plain && value == 16 {
                        ends.push(offset + 1);
                    }
                    CsiState::Ground
                }
                (CsiState::Csi { .. }, 0x40..=0x7e) => CsiState::Ground,
                // Other C0 controls and DEL are executed inside a sequence without ending it.
                (state, _) => state,
            };
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearParam {
    Empty,
    Three,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearState {
    Ground,
    /// After `ESC`; `start` is its offset in the current call, `None` when it was read earlier.
    Escape {
        start: Option<usize>,
    },
    Csi {
        start: Option<usize>,
        param: ClearParam,
    },
}

/// Finds `CSI 3 J` (erase saved lines) and `ESC c` (full reset) in a byte stream, so the engine
/// can be run up to just before and just after them: the absolute line numbering of the block
/// index needs the rows scrolled before the erase counted separately from those after it.
#[derive(Debug)]
pub(crate) struct HistoryClearScanner {
    state: ClearState,
}

impl HistoryClearScanner {
    pub(crate) fn new() -> Self {
        Self {
            state: ClearState::Ground,
        }
    }

    /// Appends every erase completed in `bytes` to `found`, in stream order.
    pub(crate) fn feed(&mut self, bytes: &[u8], found: &mut Vec<HistoryClear>) {
        for (offset, &byte) in bytes.iter().enumerate() {
            self.state = match (self.state, byte) {
                (_, 0x18 | 0x1a) => ClearState::Ground,
                (_, 0x1b) => ClearState::Escape {
                    start: Some(offset),
                },
                (ClearState::Ground, _) => ClearState::Ground,
                (ClearState::Escape { start }, b'[') => ClearState::Csi {
                    start,
                    param: ClearParam::Empty,
                },
                (ClearState::Escape { start }, b'c') => {
                    found.push(HistoryClear {
                        start: start.unwrap_or(0),
                        end: offset + 1,
                        reset: true,
                    });
                    ClearState::Ground
                }
                (ClearState::Escape { .. }, _) => ClearState::Ground,
                (
                    ClearState::Csi {
                        start,
                        param: ClearParam::Empty,
                    },
                    b'3',
                ) => ClearState::Csi {
                    start,
                    param: ClearParam::Three,
                },
                (ClearState::Csi { start, .. }, b'0'..=b'9' | 0x20..=0x3f) => ClearState::Csi {
                    start,
                    param: ClearParam::Other,
                },
                (ClearState::Csi { start, param }, b'J') => {
                    if param == ClearParam::Three {
                        found.push(HistoryClear {
                            start: start.unwrap_or(0),
                            end: offset + 1,
                            reset: false,
                        });
                    }
                    ClearState::Ground
                }
                (ClearState::Csi { .. }, 0x40..=0x7e) => ClearState::Ground,
                // Other C0 controls and DEL are executed inside a sequence without ending it.
                (state, _) => state,
            };
        }
        // Whatever is still open began before the next call's first byte.
        self.state = match self.state {
            ClearState::Escape { .. } => ClearState::Escape { start: None },
            ClearState::Csi { param, .. } => ClearState::Csi { start: None, param },
            ClearState::Ground => ClearState::Ground,
        };
    }
}

/// The old pipeline: three scanners, each over the whole chunk.
pub(super) struct Separate {
    osc: OscSniffer,
    cell: CellSizeScanner,
    clear: HistoryClearScanner,
}

impl Separate {
    pub(super) fn new() -> Self {
        Self {
            osc: OscSniffer::new(),
            cell: CellSizeScanner::new(),
            clear: HistoryClearScanner::new(),
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8], out: &mut Vec<TerminalMeta>) -> super::Scan {
        let mut scan = super::Scan::default();
        self.osc
            .feed(bytes, out, &mut scan.color_queries, &mut scan.signals);
        self.cell.feed(bytes, &mut scan.cell_sizes);
        self.clear.feed(bytes, &mut scan.erases);
        scan
    }
}
