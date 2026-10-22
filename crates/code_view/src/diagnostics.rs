//! Diagnostics in the code view (slice E-12a): what a language server found, kept on the
//! [`FileBuffer`](crate::FileBuffer) and drawn by every view of it.
//!
//! - **No colour-only cue** (`docs/spikes/2026-10-21-editor.md` §2.5): each severity has an
//!   underline *style* ([`UnderlineStyle`]: an error is wavy, a warning dotted, information and
//!   hints a thin straight line) and a gutter glyph of its own *shape* ([`GlyphShape`]: a disc,
//!   a triangle, a diamond, a ring), as well as its colour (`editor.diagnostic.*`).
//! - **Anchors.** The owner hands the buffer a file's whole set ([`DiagnosticInput`], LSP
//!   positions in UTF-16) whenever a server publishes; the set replaces the previous one. Until
//!   the next publish every edit maps the ranges through its transaction, so a diagnostic stays
//!   on the text it is about while the user types above or inside it.
//! - **Messages** show on hover and as one quiet hint after the end of the caret's line, never
//!   on every line (Helix #1462).

use std::ops::Range;
use std::sync::Arc;

use hutan_buffer::{Bias, Text, Transaction, Utf16Point};

/// How serious a diagnostic is. Ordered from the most serious.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// How the text a diagnostic is about is underlined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    /// A wave (errors).
    Wavy,
    /// A row of dots (warnings).
    Dotted,
    /// A thin straight line (information and hints).
    Straight,
}

/// The shape of a diagnostic's glyph in the gutter, the status bar and the Problems panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphShape {
    /// A filled circle (errors).
    Disc,
    /// A triangle pointing up (warnings).
    Triangle,
    /// A square standing on a corner (information).
    Diamond,
    /// An outlined circle (hints).
    Ring,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Error,
        Severity::Warning,
        Severity::Info,
        Severity::Hint,
    ];

    pub fn underline(self) -> UnderlineStyle {
        match self {
            Severity::Error => UnderlineStyle::Wavy,
            Severity::Warning => UnderlineStyle::Dotted,
            Severity::Info | Severity::Hint => UnderlineStyle::Straight,
        }
    }

    pub fn glyph(self) -> GlyphShape {
        match self {
            Severity::Error => GlyphShape::Disc,
            Severity::Warning => GlyphShape::Triangle,
            Severity::Info => GlyphShape::Diamond,
            Severity::Hint => GlyphShape::Ring,
        }
    }

    /// The glyph as a character of the same shape, for text (status bar, lists, popovers).
    pub fn symbol(self) -> &'static str {
        match self.glyph() {
            GlyphShape::Disc => "●",
            GlyphShape::Triangle => "▲",
            GlyphShape::Diamond => "◆",
            GlyphShape::Ring => "○",
        }
    }

    /// `error`, `warning`, `info`, `hint`.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        }
    }
}

/// One diagnostic as a server published it: LSP positions (zero-based line, UTF-16 column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticInput {
    pub start: Utf16Point,
    pub end: Utf16Point,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

/// One diagnostic where it is in the buffer's text now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Byte range in the current text (may be empty: a point).
    pub bytes: Range<usize>,
    pub severity: Severity,
    pub message: Arc<str>,
    pub source: Option<Arc<str>>,
    pub code: Option<Arc<str>>,
}

impl Diagnostic {
    /// The first line of the message (what the end-of-line hint shows).
    pub fn first_line(&self) -> &str {
        self.message.lines().next().unwrap_or_default()
    }

    /// `source(code)` or `source` or `code`, when the server said.
    pub fn origin(&self) -> Option<String> {
        match (&self.source, &self.code) {
            (Some(source), Some(code)) => Some(format!("{source}({code})")),
            (Some(source), None) => Some(source.to_string()),
            (None, Some(code)) => Some(code.to_string()),
            (None, None) => None,
        }
    }

    /// Whether the diagnostic is about byte `offset` (a point diagnostic touches its offset).
    pub fn touches(&self, offset: usize) -> bool {
        self.bytes.contains(&offset) || self.bytes.start == offset || self.bytes.end == offset
    }
}

/// How many diagnostics of each severity a file has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct DiagnosticCounts {
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub hints: usize,
}

impl DiagnosticCounts {
    pub fn of(diagnostics: &[Diagnostic]) -> Self {
        let mut counts = Self::default();
        for d in diagnostics {
            match d.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Info => counts.infos += 1,
                Severity::Hint => counts.hints += 1,
            }
        }
        counts
    }

    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.infos + self.hints
    }

    pub fn get(&self, severity: Severity) -> usize {
        match severity {
            Severity::Error => self.errors,
            Severity::Warning => self.warnings,
            Severity::Info => self.infos,
            Severity::Hint => self.hints,
        }
    }
}

/// What a frame drew for a diagnostic on one line (tests check that no cue is colour alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawnDiagnostic {
    /// Zero-based line.
    pub line: usize,
    pub severity: Severity,
    pub underline: UnderlineStyle,
    /// The gutter glyph of the line (the most serious diagnostic on it), on the first entry of
    /// the line only.
    pub glyph: Option<GlyphShape>,
}

/// The inputs placed in `text` (clamped the way LSP clients treat server positions), sorted by
/// where they start, the most serious first among equals.
pub(crate) fn place(text: &Text, inputs: Vec<DiagnosticInput>) -> Vec<Diagnostic> {
    let mut placed: Vec<Diagnostic> = inputs
        .into_iter()
        .map(|input| {
            let start = text.utf16_point_to_byte_clamped(input.start);
            let end = text.utf16_point_to_byte_clamped(input.end).max(start);
            Diagnostic {
                bytes: start..end,
                severity: input.severity,
                message: Arc::from(input.message),
                source: input.source.map(Arc::from),
                code: input.code.map(Arc::from),
            }
        })
        .collect();
    sort(&mut placed);
    placed
}

fn sort(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|a, b| {
        a.bytes
            .start
            .cmp(&b.bytes.start)
            .then(a.severity.cmp(&b.severity))
            .then(a.bytes.end.cmp(&b.bytes.end))
    });
}

/// The diagnostics after `transaction`: each range follows the text it covers (typing at its
/// start pushes it, typing at its end does not grow it; a deleted range becomes a point).
pub(crate) fn map(diagnostics: &[Diagnostic], transaction: &Transaction) -> Vec<Diagnostic> {
    let mut mapped: Vec<Diagnostic> = diagnostics
        .iter()
        .map(|d| {
            let start = transaction.map_offset(d.bytes.start, Bias::Right);
            let end = if d.bytes.is_empty() {
                start
            } else {
                transaction.map_offset(d.bytes.end, Bias::Left).max(start)
            };
            Diagnostic {
                bytes: start..end,
                ..d.clone()
            }
        })
        .collect();
    sort(&mut mapped);
    mapped
}

/// The index of the diagnostic after byte `head` (wrapping to the first), or before it.
pub(crate) fn step(diagnostics: &[Diagnostic], head: usize, forward: bool) -> Option<usize> {
    if diagnostics.is_empty() {
        return None;
    }
    let index = if forward {
        diagnostics
            .iter()
            .position(|d| d.bytes.start > head)
            .unwrap_or(0)
    } else {
        diagnostics
            .iter()
            .rposition(|d| d.bytes.start < head)
            .unwrap_or(diagnostics.len() - 1)
    };
    Some(index)
}

impl crate::view::CodeView {
    /// The diagnostics the last frame drew, line by line (tests: style and glyph per severity).
    pub fn drawn_diagnostics(&self) -> &[DrawnDiagnostic] {
        &self.stats.last_diagnostics
    }

    /// Moves the caret to the next diagnostic (`]d`, F8), wrapping around.
    pub fn next_diagnostic(&mut self, cx: &mut gpui::Context<Self>) {
        self.step_diagnostic(true, cx);
    }

    /// Moves the caret to the previous diagnostic (`[d`, ⇧F8), wrapping around.
    pub fn previous_diagnostic(&mut self, cx: &mut gpui::Context<Self>) {
        self.step_diagnostic(false, cx);
    }

    pub(crate) fn step_diagnostic(&mut self, forward: bool, cx: &mut gpui::Context<Self>) {
        let diagnostics = self.buffer().read(cx).diagnostics();
        let head = self.selections.primary().head;
        let Some(index) = step(&diagnostics, head, forward) else {
            return;
        };
        let at = diagnostics[index].bytes.start;
        self.select(at..at, cx);
    }

    /// F12: the owner looks up the definition of the symbol at the caret.
    pub fn go_to_definition_at_caret(&mut self, cx: &mut gpui::Context<Self>) {
        let offset = self.selections.primary().head;
        cx.emit(crate::view::CodeViewEvent::GoToDefinition { offset });
    }

    /// Go to definition landed in this file at an LSP position (zero-based line, UTF-16
    /// column): where the caret was becomes the newest entry of Vim's jumplist (`⌃O` comes
    /// back), and the caret goes there.
    pub fn jump_to_utf16(&mut self, line: usize, character: usize, cx: &mut gpui::Context<Self>) {
        self.vim_push_jump(cx);
        let at = self
            .buffer()
            .read(cx)
            .text()
            .utf16_point_to_byte_clamped(Utf16Point { line, character });
        self.select(at..at, cx);
    }

    /// The view was reached by a jump from another file: `⌃O` goes back there until Vim makes
    /// a jump of its own.
    pub fn arrived_by_jump(&mut self) {
        self.jump_arrival = Some(self.vim_jump_count().unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(line: usize, from: usize, to: usize, severity: Severity) -> DiagnosticInput {
        DiagnosticInput {
            start: Utf16Point {
                line,
                character: from,
            },
            end: Utf16Point {
                line,
                character: to,
            },
            severity,
            message: format!("{} here", severity.label()),
            source: Some("fake".into()),
            code: None,
        }
    }

    #[test]
    fn every_severity_has_its_own_underline_and_glyph() {
        let styles: Vec<_> = Severity::ALL.iter().map(|s| s.underline()).collect();
        assert_eq!(
            styles,
            [
                UnderlineStyle::Wavy,
                UnderlineStyle::Dotted,
                UnderlineStyle::Straight,
                UnderlineStyle::Straight
            ]
        );
        let glyphs: std::collections::HashSet<_> = Severity::ALL.iter().map(|s| s.glyph()).collect();
        assert_eq!(glyphs.len(), 4, "four shapes");
        let symbols: std::collections::HashSet<_> =
            Severity::ALL.iter().map(|s| s.symbol()).collect();
        assert_eq!(symbols.len(), 4);
    }

    #[test]
    fn utf16_positions_land_on_bytes_and_follow_edits() {
        // "é" is one UTF-16 unit and two bytes; "😀" two units and four bytes.
        let text = Text::from("let é😀 = error;\nnext\n");
        let placed = place(
            &text,
            vec![
                input(0, 10, 15, Severity::Error),
                input(1, 0, 4, Severity::Warning),
            ],
        );
        let at = |d: &Diagnostic| text.slice(d.bytes.clone()).unwrap();
        assert_eq!(at(&placed[0]), "error");
        assert_eq!(at(&placed[1]), "next");

        // Typing before the error moves it; typing at its end does not grow it.
        let tx = Transaction::insert(0, "pub ");
        let moved = map(&placed, &tx);
        let after = Text::from(tx.apply_to_string(&text.to_string()).unwrap().as_str());
        assert_eq!(after.slice(moved[0].bytes.clone()).unwrap(), "error");
        let end = moved[0].bytes.end;
        let tx = Transaction::insert(end, "s");
        let grown = map(&moved, &tx);
        assert_eq!(grown[0].bytes, moved[0].bytes);
        // Deleting its text leaves a point where it was.
        let tx = Transaction::delete(grown[0].bytes.clone()).unwrap();
        let gone = map(&grown, &tx);
        assert!(gone[0].bytes.is_empty());
    }

    #[test]
    fn stepping_wraps_both_ways() {
        let text = Text::from("a error b error c\n");
        let placed = place(
            &text,
            vec![input(0, 2, 7, Severity::Error), input(0, 10, 15, Severity::Error)],
        );
        assert_eq!(step(&placed, 0, true), Some(0));
        assert_eq!(step(&placed, 2, true), Some(1));
        assert_eq!(step(&placed, 12, true), Some(0));
        assert_eq!(step(&placed, 12, false), Some(1));
        assert_eq!(step(&placed, 5, false), Some(0));
        assert_eq!(step(&placed, 0, false), Some(1));
        assert_eq!(step(&[], 0, true), None);
    }
}
