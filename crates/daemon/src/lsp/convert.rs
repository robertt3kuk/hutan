//! Between what a language server says (its negotiated position encoding, `ls-types`) and the
//! wire of protocol 0.39 (UTF-16 columns, plain DTOs, bounded).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hutan_buffer::Text;
use hutan_lsp_client::ls_types::{
    CompletionItem, CompletionItemKind, CompletionItemTag, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, Documentation, Hover, HoverContents, InsertTextFormat, MarkedString,
    NumberOrString, ParameterLabel, Position, Range, SignatureHelp, TextEdit, Uri,
};
use hutan_lsp_client::position::{self, PositionEncoding};
use hutan_protocol::lsp::{
    COMPLETION_DETAIL_MAX_BYTES, COMPLETION_DOC_MAX_BYTES, COMPLETION_EDITS_MAX,
    COMPLETION_LABEL_MAX_BYTES, COMPLETION_TEXT_MAX_BYTES, DIAGNOSTIC_MESSAGE_MAX_BYTES,
    HOVER_MAX_BYTES, LspCompletionItem, LspCompletionKind, LspDiagnostic, LspParameter,
    LspPosition, LspRange, LspRelated, LspSeverity, LspSignature, LspSignatureHelp, LspTextEdit,
    PARAMETERS_MAX, RELATED_MAX, SIGNATURES_MAX,
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

fn documentation(doc: &Documentation, max: usize) -> Option<String> {
    let text = match doc {
        Documentation::String(text) => text,
        Documentation::MarkupContent(markup) => &markup.value,
    };
    (!text.trim().is_empty()).then(|| clip(text, max))
}

fn completion_kind(kind: CompletionItemKind) -> LspCompletionKind {
    use LspCompletionKind as K;
    const KINDS: [(CompletionItemKind, LspCompletionKind); 25] = [
        (CompletionItemKind::TEXT, K::Text),
        (CompletionItemKind::METHOD, K::Method),
        (CompletionItemKind::FUNCTION, K::Function),
        (CompletionItemKind::CONSTRUCTOR, K::Constructor),
        (CompletionItemKind::FIELD, K::Field),
        (CompletionItemKind::VARIABLE, K::Variable),
        (CompletionItemKind::CLASS, K::Class),
        (CompletionItemKind::INTERFACE, K::Interface),
        (CompletionItemKind::MODULE, K::Module),
        (CompletionItemKind::PROPERTY, K::Property),
        (CompletionItemKind::UNIT, K::Unit),
        (CompletionItemKind::VALUE, K::Value),
        (CompletionItemKind::ENUM, K::Enum),
        (CompletionItemKind::KEYWORD, K::Keyword),
        (CompletionItemKind::SNIPPET, K::Snippet),
        (CompletionItemKind::COLOR, K::Color),
        (CompletionItemKind::FILE, K::File),
        (CompletionItemKind::REFERENCE, K::Reference),
        (CompletionItemKind::FOLDER, K::Folder),
        (CompletionItemKind::ENUM_MEMBER, K::EnumMember),
        (CompletionItemKind::CONSTANT, K::Constant),
        (CompletionItemKind::STRUCT, K::Struct),
        (CompletionItemKind::EVENT, K::Event),
        (CompletionItemKind::OPERATOR, K::Operator),
        (CompletionItemKind::TYPE_PARAMETER, K::TypeParameter),
    ];
    KINDS
        .iter()
        .find(|(lsp, _)| *lsp == kind)
        .map_or(K::Other, |(_, wire)| *wire)
}

fn text_edit(
    text: &Text,
    encoding: PositionEncoding,
    range: Range,
    new_text: &str,
) -> Option<LspTextEdit> {
    (new_text.len() <= COMPLETION_TEXT_MAX_BYTES).then(|| LspTextEdit {
        range: range_to_wire(Some(text), encoding, range),
        new_text: new_text.to_owned(),
    })
}

/// A completion item on the wire, bounded: `None` when what it inserts is over the bounds
/// (cutting code would insert something else). `doc_max`: the most bytes of documentation
/// kept ([`COMPLETION_DOC_MAX_BYTES`] in a list, [`HOVER_MAX_BYTES`] resolved).
pub fn completion_item(
    item: &CompletionItem,
    text: &Text,
    encoding: PositionEncoding,
    doc_max: usize,
) -> Option<LspCompletionItem> {
    let edit = match &item.text_edit {
        None => None,
        Some(CompletionTextEdit::Edit(edit)) => {
            Some(text_edit(text, encoding, edit.range, &edit.new_text)?)
        }
        // Hutan inserts (it does not announce `insertReplaceSupport`; a server may still send
        // both ranges).
        Some(CompletionTextEdit::InsertAndReplace(edit)) => {
            Some(text_edit(text, encoding, edit.insert, &edit.new_text)?)
        }
    };
    let insert_text = match &item.insert_text {
        Some(insert) if insert.len() > COMPLETION_TEXT_MAX_BYTES => return None,
        other => other.clone(),
    };
    let extra: &[TextEdit] = item.additional_text_edits.as_deref().unwrap_or_default();
    if extra.len() > COMPLETION_EDITS_MAX {
        return None;
    }
    let additional_edits = extra
        .iter()
        .map(|e| text_edit(text, encoding, e.range, &e.new_text))
        .collect::<Option<Vec<_>>>()?;
    let detail = item.detail.clone().or_else(|| {
        item.label_details
            .as_ref()
            .and_then(|d| d.description.clone())
    });
    Some(LspCompletionItem {
        label: clip(&item.label, COMPLETION_LABEL_MAX_BYTES),
        kind: item.kind.map(completion_kind),
        detail: detail
            .filter(|d| !d.trim().is_empty())
            .map(|d| clip(&d, COMPLETION_DETAIL_MAX_BYTES)),
        documentation: item
            .documentation
            .as_ref()
            .and_then(|d| documentation(d, doc_max)),
        sort_text: item
            .sort_text
            .as_deref()
            .map(|s| clip(s, COMPLETION_LABEL_MAX_BYTES)),
        filter_text: item
            .filter_text
            .as_deref()
            .map(|s| clip(s, COMPLETION_LABEL_MAX_BYTES)),
        insert_text,
        snippet: item.insert_text_format == Some(InsertTextFormat::SNIPPET),
        edit,
        additional_edits,
        preselect: item.preselect == Some(true),
        deprecated: item.deprecated == Some(true)
            || item
                .tags
                .as_ref()
                .is_some_and(|t| t.contains(&CompletionItemTag::DEPRECATED)),
        resolvable: false,
    })
}

/// Byte offset in `label` of `utf16` UTF-16 units (clamped to its end).
fn label_byte(label: &str, utf16: u32) -> u32 {
    let mut units = 0u32;
    for (at, c) in label.char_indices() {
        if units >= utf16 {
            return u32::try_from(at).unwrap_or(u32::MAX);
        }
        units += u32::try_from(c.len_utf16()).unwrap_or(2);
    }
    u32::try_from(label.len()).unwrap_or(u32::MAX)
}

/// A signature help on the wire: at most [`SIGNATURES_MAX`] signatures of at most
/// [`PARAMETERS_MAX`] parameters, each parameter a byte range of its signature's label.
pub fn signature_help(help: &SignatureHelp) -> LspSignatureHelp {
    let signatures: Vec<LspSignature> = help
        .signatures
        .iter()
        .take(SIGNATURES_MAX)
        .map(|signature| {
            let label = clip(&signature.label, COMPLETION_DOC_MAX_BYTES);
            let mut searched = 0usize;
            let parameters = signature
                .parameters
                .iter()
                .flatten()
                .take(PARAMETERS_MAX)
                .map(|p| {
                    let (start, end) = match &p.label {
                        ParameterLabel::LabelOffsets([a, b]) => {
                            (label_byte(&label, *a), label_byte(&label, *b))
                        }
                        // The first place after the parameter before.
                        ParameterLabel::Simple(name) => {
                            match label.get(searched..).and_then(|rest| rest.find(name.as_str())) {
                                Some(at) if !name.is_empty() => {
                                    let start = searched + at;
                                    searched = start + name.len();
                                    (
                                        u32::try_from(start).unwrap_or(u32::MAX),
                                        u32::try_from(searched).unwrap_or(u32::MAX),
                                    )
                                }
                                _ => (0, 0),
                            }
                        }
                    };
                    LspParameter {
                        start,
                        end: end.max(start),
                        documentation: p
                            .documentation
                            .as_ref()
                            .and_then(|d| documentation(d, COMPLETION_DETAIL_MAX_BYTES)),
                    }
                })
                .collect();
            LspSignature {
                documentation: signature
                    .documentation
                    .as_ref()
                    .and_then(|d| documentation(d, COMPLETION_DOC_MAX_BYTES)),
                active_parameter: signature.active_parameter.or(help.active_parameter),
                label,
                parameters,
            }
        })
        .collect();
    let active = help.active_signature.unwrap_or(0);
    LspSignatureHelp {
        active_signature: if (active as usize) < signatures.len() {
            active
        } else {
            0
        },
        signatures,
    }
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
