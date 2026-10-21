//! Between what a language server says (its negotiated position encoding, `ls-types`) and the
//! wire of protocol 0.39 (UTF-16 columns, plain DTOs, bounded).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hutan_buffer::Text;
use hutan_lsp_client::ls_types::{
    Diagnostic, DiagnosticSeverity, Hover, HoverContents, MarkedString, NumberOrString, Position,
    Range, Uri,
};
use hutan_lsp_client::position::{self, PositionEncoding};
use hutan_protocol::lsp::{
    DIAGNOSTIC_MESSAGE_MAX_BYTES, HOVER_MAX_BYTES, LspDiagnostic, LspPosition, LspRange,
    LspRelated, LspSeverity, RELATED_MAX,
};

/// The texts positions are converted against, by canonical path: the open documents' mirrors and
/// files read for the occasion. A path without a text keeps the server's columns as they are
/// (right whenever the encoding is UTF-16 or the line is ASCII).
pub type Texts = HashMap<PathBuf, Text>;

/// `text` cut to at most `max` bytes at a character boundary.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

pub fn path_of(uri: &Uri) -> Option<PathBuf> {
    uri.to_file_path().map(std::borrow::Cow::into_owned)
}

pub fn uri_of(path: &Path) -> Option<Uri> {
    Uri::from_file_path(path)
}

/// A position of the server in the wire's UTF-16 columns.
pub fn to_wire(text: Option<&Text>, encoding: PositionEncoding, at: Position) -> LspPosition {
    let raw = LspPosition {
        line: at.line,
        character: at.character,
    };
    let (Some(text), PositionEncoding::Utf8) = (text, encoding) else {
        return raw;
    };
    let offset = position::offset(text, at, encoding);
    position::position(text, offset, PositionEncoding::Utf16)
        .map(|p| LspPosition {
            line: p.line,
            character: p.character,
        })
        .unwrap_or(raw)
}

pub fn range_to_wire(text: Option<&Text>, encoding: PositionEncoding, range: Range) -> LspRange {
    LspRange {
        start: to_wire(text, encoding, range.start),
        end: to_wire(text, encoding, range.end),
    }
}

/// A position of the wire (UTF-16) in the server's encoding, against the document's text.
pub fn to_server(text: &Text, encoding: PositionEncoding, at: LspPosition) -> Position {
    let utf16 = Position::new(at.line, at.character);
    let offset = position::offset(text, utf16, PositionEncoding::Utf16);
    position::position(text, offset, encoding).unwrap_or(utf16)
}

fn severity(severity: Option<DiagnosticSeverity>) -> Option<LspSeverity> {
    let severity = severity?;
    Some(if severity == DiagnosticSeverity::ERROR {
        LspSeverity::Error
    } else if severity == DiagnosticSeverity::WARNING {
        LspSeverity::Warning
    } else if severity == DiagnosticSeverity::INFORMATION {
        LspSeverity::Information
    } else {
        LspSeverity::Hint
    })
}

/// One diagnostic as the wire carries it: columns in UTF-16, the message at most
/// [`DIAGNOSTIC_MESSAGE_MAX_BYTES`], at most [`RELATED_MAX`] related places (files only).
pub fn diagnostic(
    diagnostic: &Diagnostic,
    path: &Path,
    texts: &Texts,
    encoding: PositionEncoding,
) -> LspDiagnostic {
    let related = diagnostic
        .related_information
        .iter()
        .flatten()
        .filter_map(|info| {
            let related = path_of(&info.location.uri)?;
            let text = texts.get(&related);
            Some(LspRelated {
                range: range_to_wire(text, encoding, info.location.range),
                path: related,
                message: clip(&info.message, DIAGNOSTIC_MESSAGE_MAX_BYTES),
            })
        })
        .take(RELATED_MAX)
        .collect();
    LspDiagnostic {
        range: range_to_wire(texts.get(path), encoding, diagnostic.range),
        severity: severity(diagnostic.severity),
        code: diagnostic.code.as_ref().map(|code| match code {
            NumberOrString::Number(number) => number.to_string(),
            NumberOrString::String(text) => clip(text, 128),
        }),
        source: diagnostic.source.as_deref().map(|s| clip(s, 128)),
        message: clip(&diagnostic.message, DIAGNOSTIC_MESSAGE_MAX_BYTES),
        related,
    }
}

fn marked(string: &MarkedString) -> String {
    match string {
        MarkedString::String(text) => text.clone(),
        MarkedString::LanguageString(code) => {
            format!("```{}\n{}\n```", code.language, code.value)
        }
    }
}

/// What a hover says as one text (Markdown or plain): the parts joined by a blank line, at most
/// [`HOVER_MAX_BYTES`]; `None` when it says nothing.
pub fn hover_text(hover: &Hover) -> Option<String> {
    let text = match &hover.contents {
        HoverContents::Scalar(part) => marked(part),
        HoverContents::Array(parts) => parts.iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(markup) => markup.value.clone(),
    };
    (!text.trim().is_empty()).then(|| clip(&text, HOVER_MAX_BYTES))
}

#[cfg(test)]
mod tests {
    use hutan_lsp_client::ls_types::{LanguageString, MarkupContent, MarkupKind};

    use super::*;

    #[test]
    fn utf8_columns_become_utf16_and_utf16_ones_stay() {
        // "é" is two bytes and one UTF-16 unit; the emoji is four bytes and two units.
        let text = Text::from("aé😀x\nsecond\n");
        let after_emoji_utf8 = Position::new(0, 7);
        assert_eq!(
            to_wire(Some(&text), PositionEncoding::Utf8, after_emoji_utf8),
            LspPosition {
                line: 0,
                character: 4
            }
        );
        assert_eq!(
            to_wire(Some(&text), PositionEncoding::Utf16, Position::new(0, 4)),
            LspPosition {
                line: 0,
                character: 4
            }
        );
        // Without the text the server's column is all there is.
        assert_eq!(
            to_wire(None, PositionEncoding::Utf8, after_emoji_utf8).character,
            7
        );
        // And back: the wire's UTF-16 column in the server's encoding.
        let wire = LspPosition {
            line: 0,
            character: 4,
        };
        assert_eq!(to_server(&text, PositionEncoding::Utf8, wire).character, 7);
        assert_eq!(to_server(&text, PositionEncoding::Utf16, wire).character, 4);
    }

    #[test]
    fn diagnostics_are_clipped_and_carry_code_source_and_severity() {
        let long = "é".repeat(DIAGNOSTIC_MESSAGE_MAX_BYTES);
        let raw = Diagnostic {
            range: Range::new(Position::new(1, 0), Position::new(1, 3)),
            severity: Some(DiagnosticSeverity::WARNING),
            code: Some(NumberOrString::Number(42)),
            source: Some("fake".to_owned()),
            message: long,
            ..Diagnostic::default()
        };
        let wire = diagnostic(
            &raw,
            Path::new("/r/a.rs"),
            &Texts::new(),
            PositionEncoding::Utf16,
        );
        assert_eq!(wire.severity, Some(LspSeverity::Warning));
        assert_eq!(wire.code.as_deref(), Some("42"));
        assert_eq!(wire.source.as_deref(), Some("fake"));
        assert!(wire.message.len() <= DIAGNOSTIC_MESSAGE_MAX_BYTES);
        assert!(wire.message.chars().all(|c| c == 'é'));
        assert_eq!(wire.range.end.character, 3);
    }

    #[test]
    fn hover_parts_join_and_an_empty_hover_is_nothing() {
        let hover = |contents| Hover {
            contents,
            range: None,
        };
        assert_eq!(
            hover_text(&hover(HoverContents::Array(vec![
                MarkedString::LanguageString(LanguageString {
                    language: "rust".to_owned(),
                    value: "fn f()".to_owned()
                }),
                MarkedString::String("docs".to_owned()),
            ]))),
            Some("```rust\nfn f()\n```\n\ndocs".to_owned())
        );
        assert_eq!(
            hover_text(&hover(HoverContents::Markup(MarkupContent {
                kind: MarkupKind::PlainText,
                value: "  ".to_owned()
            }))),
            None
        );
    }
}
