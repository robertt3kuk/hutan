//! Open documents and their synchronization with the server, with **coalescing**: edits only
//! mark what changed; a flush turns everything since the last one into one `didChange` per
//! document — a single range replacement when the server syncs incrementally, the whole text
//! when it syncs fully. A burst of a hundred keystrokes in one tick is one message, and a
//! server that stops reading costs one pending range per document, never a queue of edits.
//!
//! The dirty region is kept in O(1) per edit: the bytes `start..sent_end` of the text the server
//! has were replaced by the bytes `start..end` of the current text (everything before `start`
//! and after the two ends is identical).

use std::collections::HashMap;

use hutan_buffer::{Change, Text};
use ls_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, ServerCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncSaveOptions, Uri, VersionedTextDocumentIdentifier,
};

use crate::error::LspError;
use crate::position::{self, PositionEncoding};

/// Documents one client keeps open at most.
pub const MAX_DOCUMENTS: usize = 4096;

/// How the server wants documents (`capabilities.textDocumentSync`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncOptions {
    pub open_close: bool,
    /// `None`: no `didChange` at all.
    pub change: Option<ChangeKind>,
    /// `Some(include_text)`: send `didSave`.
    pub save: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Full,
    Incremental,
}

impl SyncOptions {
    pub fn from_capabilities(capabilities: &ServerCapabilities) -> Self {
        let kind = |kind: TextDocumentSyncKind| {
            if kind == TextDocumentSyncKind::INCREMENTAL {
                Some(ChangeKind::Incremental)
            } else if kind == TextDocumentSyncKind::FULL {
                Some(ChangeKind::Full)
            } else {
                None
            }
        };
        match &capabilities.text_document_sync {
            None => Self {
                open_close: false,
                change: None,
                save: None,
            },
            Some(TextDocumentSyncCapability::Kind(k)) => Self {
                open_close: kind(*k).is_some(),
                change: kind(*k),
                save: None,
            },
            Some(TextDocumentSyncCapability::Options(options)) => Self {
                open_close: options.open_close.unwrap_or(false),
                change: options.change.and_then(kind),
                save: match &options.save {
                    None | Some(TextDocumentSyncSaveOptions::Supported(false)) => None,
                    Some(TextDocumentSyncSaveOptions::Supported(true)) => Some(false),
                    Some(TextDocumentSyncSaveOptions::SaveOptions(save)) => {
                        Some(save.include_text.unwrap_or(false))
                    }
                },
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dirty {
    Clean,
    /// `sent[start..sent_end]` became `current[start..end]`.
    Range {
        start: usize,
        sent_end: usize,
        end: usize,
    },
    /// Unknown: send the whole text.
    Full,
}

#[derive(Debug)]
struct Document {
    language_id: String,
    version: i32,
    current: Text,
    /// What the server has (valid once `opened`).
    sent: Text,
    dirty: Dirty,
    /// The running server instance was sent `didOpen`.
    opened: bool,
    save_pending: bool,
}

/// One notification to send, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    Open(DidOpenTextDocumentParams),
    Change(DidChangeTextDocumentParams),
    Save(DidSaveTextDocumentParams),
    Close(DidCloseTextDocumentParams),
}

/// The open documents of one client.
#[derive(Debug, Default)]
pub struct Documents {
    docs: HashMap<Uri, Document>,
    /// Documents closed since the last flush that the running server had open.
    closing: Vec<Uri>,
    /// The server instance the `opened` flags and `sent` texts refer to.
    instance: u64,
}

impl Documents {
    pub fn open(
        &mut self,
        uri: Uri,
        language_id: &str,
        version: i32,
        text: Text,
    ) -> Result<(), LspError> {
        if self.docs.contains_key(&uri) {
            return Err(LspError::Document(format!(
                "{} is already open",
                uri.as_str()
            )));
        }
        if self.docs.len() >= MAX_DOCUMENTS {
            return Err(LspError::Limit("open documents"));
        }
        self.docs.insert(
            uri,
            Document {
                language_id: language_id.to_owned(),
                version,
                sent: text.clone(),
                current: text,
                dirty: Dirty::Clean,
                opened: false,
                save_pending: false,
            },
        );
        Ok(())
    }

    fn get_mut(&mut self, uri: &Uri) -> Result<&mut Document, LspError> {
        self.docs
            .get_mut(uri)
            .ok_or_else(|| LspError::Document(format!("{} is not open", uri.as_str())))
    }

    /// Records one applied transaction: `text` is the document after it, `changes` its
    /// [`Transaction::changes`](hutan_buffer::Transaction::changes) (old ranges in the text
    /// before, new ranges in `text`).
    pub fn change(
        &mut self,
        uri: &Uri,
        version: i32,
        text: Text,
        changes: &[Change],
    ) -> Result<(), LspError> {
        let doc = self.get_mut(uri)?;
        let before = doc.current.len();
        doc.version = version;
        let (Some(first), Some(last)) = (changes.first(), changes.last()) else {
            if text != doc.current {
                doc.dirty = Dirty::Full;
            }
            doc.current = text;
            return Ok(());
        };
        // The transaction's edits apply simultaneously; together they replace one span.
        let (s, e, new_e) = (first.old.start, last.old.end, last.new.end);
        let consistent = s <= e
            && e <= before
            && first.new.start == s
            && new_e >= s
            && before - (e - s) + (new_e - s) == text.len();
        doc.dirty = match doc.dirty {
            _ if !consistent => Dirty::Full,
            Dirty::Full => Dirty::Full,
            Dirty::Clean => Dirty::Range {
                start: s,
                sent_end: e,
                end: new_e,
            },
            Dirty::Range {
                start,
                sent_end,
                end,
            } => {
                // `end` is in the coordinates of the text before this transaction.
                let hi = end.max(e);
                Dirty::Range {
                    start: start.min(s),
                    sent_end: sent_end + (hi - end),
                    end: hi - e + new_e,
                }
            }
        };
        doc.current = text;
        Ok(())
    }

    /// Replaces the whole text (the caller does not know what changed).
    pub fn replace(&mut self, uri: &Uri, version: i32, text: Text) -> Result<(), LspError> {
        let doc = self.get_mut(uri)?;
        doc.version = version;
        if text != doc.current {
            doc.dirty = Dirty::Full;
        }
        doc.current = text;
        Ok(())
    }

    pub fn save(&mut self, uri: &Uri) -> Result<(), LspError> {
        self.get_mut(uri)?.save_pending = true;
        Ok(())
    }

    pub fn close(&mut self, uri: &Uri) -> Result<(), LspError> {
        let doc = self
            .docs
            .remove(uri)
            .ok_or_else(|| LspError::Document(format!("{} is not open", uri.as_str())))?;
        if doc.opened {
            self.closing.push(uri.clone());
        }
        Ok(())
    }

    /// A new server instance knows no document: everything is opened again with its current
    /// text at the next flush for `instance`.
    pub fn reset(&mut self, instance: u64) {
        self.instance = instance;
        self.closing.clear();
        for doc in self.docs.values_mut() {
            doc.opened = false;
            doc.dirty = Dirty::Clean;
            doc.save_pending = false;
            doc.sent = doc.current.clone();
        }
    }

    /// Whether a flush would send anything.
    #[cfg(test)]
    pub fn has_pending(&self) -> bool {
        !self.closing.is_empty()
            || self
                .docs
                .values()
                .any(|d| !d.opened || d.dirty != Dirty::Clean || d.save_pending)
    }

    /// Everything to send to `instance` since the last flush, in order (closes first, then per
    /// document open → change → save), and marks it sent. Nothing when the documents were
    /// reset for another instance meanwhile.
    pub fn flush(
        &mut self,
        instance: u64,
        sync: SyncOptions,
        encoding: PositionEncoding,
    ) -> Vec<Outgoing> {
        let mut out = Vec::new();
        if instance != self.instance {
            return out;
        }
        for uri in self.closing.drain(..) {
            if sync.open_close {
                out.push(Outgoing::Close(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri },
                }));
            }
        }
        for (uri, doc) in &mut self.docs {
            if !doc.opened {
                doc.opened = true;
                doc.dirty = Dirty::Clean;
                doc.sent = doc.current.clone();
                if sync.open_close {
                    out.push(Outgoing::Open(DidOpenTextDocumentParams {
                        text_document: TextDocumentItem {
                            uri: uri.clone(),
                            language_id: doc.language_id.clone(),
                            version: doc.version,
                            text: doc.current.to_string(),
                        },
                    }));
                }
            }
            if doc.dirty != Dirty::Clean {
                if let Some(kind) = sync.change {
                    let change = content_change(doc, kind, encoding);
                    out.push(Outgoing::Change(DidChangeTextDocumentParams {
                        text_document: VersionedTextDocumentIdentifier {
                            uri: uri.clone(),
                            version: doc.version,
                        },
                        content_changes: vec![change],
                    }));
                }
                doc.dirty = Dirty::Clean;
                doc.sent = doc.current.clone();
            }
            if doc.save_pending {
                doc.save_pending = false;
                if let Some(include_text) = sync.save {
                    out.push(Outgoing::Save(DidSaveTextDocumentParams {
                        text_document: TextDocumentIdentifier { uri: uri.clone() },
                        text: include_text.then(|| doc.current.to_string()),
                    }));
                }
            }
        }
        out
    }
}

fn full(doc: &Document) -> TextDocumentContentChangeEvent {
    TextDocumentContentChangeEvent {
        range: None,
        range_length: None,
        text: doc.current.to_string(),
    }
}

fn content_change(
    doc: &Document,
    kind: ChangeKind,
    encoding: PositionEncoding,
) -> TextDocumentContentChangeEvent {
    let Dirty::Range {
        start,
        sent_end,
        end,
    } = doc.dirty
    else {
        return full(doc);
    };
    if kind == ChangeKind::Full {
        return full(doc);
    }
    let range = position::range(&doc.sent, start, sent_end, encoding);
    let text = doc.current.slice(start..end);
    match (range, text) {
        (Ok(range), Ok(text)) => TextDocumentContentChangeEvent {
            range: Some(range),
            range_length: None,
            text,
        },
        // Inconsistent edits from the caller: the whole text is always right.
        _ => full(doc),
    }
}

#[cfg(test)]
mod tests {
    use hutan_buffer::{Buffer, Transaction};
    use ls_types::{Position, Range};

    use super::*;

    const INCREMENTAL: SyncOptions = SyncOptions {
        open_close: true,
        change: Some(ChangeKind::Incremental),
        save: Some(false),
    };

    fn uri() -> Uri {
        "file:///tmp/a.rs".parse().unwrap()
    }

    /// Applies `edits` one transaction each, recording them; returns the buffer.
    fn edit(docs: &mut Documents, buffer: &mut Buffer, tx: Transaction) {
        let applied = buffer.apply(tx).unwrap();
        docs.change(
            &uri(),
            applied.version as i32,
            buffer.text().clone(),
            &applied.transaction.changes(),
        )
        .unwrap();
    }

    /// Applies an incremental change event to a string the way a server would (UTF-8 columns).
    fn apply(text: &str, change: &TextDocumentContentChangeEvent) -> String {
        let Some(range) = change.range else {
            return change.text.clone();
        };
        let t = Text::from(text);
        let a = position::offset(&t, range.start, PositionEncoding::Utf8);
        let b = position::offset(&t, range.end, PositionEncoding::Utf8);
        format!("{}{}{}", &text[..a], change.text, &text[b..])
    }

    #[test]
    fn a_burst_of_edits_becomes_one_change_equal_to_the_final_text() {
        let mut docs = Documents::default();
        let mut buffer = Buffer::from_content("fn main() {\n    let x = 1;\n}\n");
        docs.open(uri(), "rust", 0, buffer.text().clone()).unwrap();
        let opened = docs.flush(0, INCREMENTAL, PositionEncoding::Utf8);
        assert!(matches!(opened.as_slice(), [Outgoing::Open(_)]));
        let mut server_text = buffer.text().to_string();

        // Typing, deleting, multi-cursor edits and edits on both sides of the dirty span.
        for i in 0..100 {
            let len = buffer.text().len();
            let tx = match i % 5 {
                0 => Transaction::insert(16, "y"),
                1 => Transaction::delete(15..16).unwrap(),
                2 => Transaction::new([
                    hutan_buffer::Edit::insert(0, "// "),
                    hutan_buffer::Edit::insert(len, "z"),
                ])
                .unwrap(),
                3 => Transaction::replace(3..7, "MAIN").unwrap(),
                _ => Transaction::insert(len - 1, "é😀"),
            };
            edit(&mut docs, &mut buffer, tx);
        }
        let out = docs.flush(0, INCREMENTAL, PositionEncoding::Utf8);
        let [Outgoing::Change(change)] = out.as_slice() else {
            panic!("expected one didChange, got {out:?}");
        };
        assert_eq!(change.content_changes.len(), 1);
        assert!(change.content_changes[0].range.is_some());
        server_text = apply(&server_text, &change.content_changes[0]);
        assert_eq!(server_text, buffer.text().to_string());
        assert_eq!(change.text_document.version, buffer.version() as i32);
        assert!(
            docs.flush(0, INCREMENTAL, PositionEncoding::Utf8)
                .is_empty()
        );
    }

    #[test]
    fn edits_far_apart_send_the_span_between_them() {
        let mut docs = Documents::default();
        let mut buffer = Buffer::from_content("abcdefghij");
        docs.open(uri(), "text", 0, buffer.text().clone()).unwrap();
        docs.flush(0, INCREMENTAL, PositionEncoding::Utf16);
        edit(&mut docs, &mut buffer, Transaction::insert(8, "X"));
        edit(&mut docs, &mut buffer, Transaction::delete(1..2).unwrap());
        let out = docs.flush(0, INCREMENTAL, PositionEncoding::Utf16);
        let [Outgoing::Change(change)] = out.as_slice() else {
            panic!("{out:?}");
        };
        let event = &change.content_changes[0];
        assert_eq!(
            event.range,
            Some(Range::new(Position::new(0, 1), Position::new(0, 8)))
        );
        assert_eq!(event.text, "cdefghX");
    }

    #[test]
    fn full_sync_and_inconsistent_edits_send_the_whole_text() {
        let mut docs = Documents::default();
        let mut buffer = Buffer::from_content("one\ntwo\n");
        docs.open(uri(), "text", 0, buffer.text().clone()).unwrap();
        let full_sync = SyncOptions {
            change: Some(ChangeKind::Full),
            ..INCREMENTAL
        };
        docs.flush(0, full_sync, PositionEncoding::Utf16);
        edit(&mut docs, &mut buffer, Transaction::insert(0, "zero\n"));
        let out = docs.flush(0, full_sync, PositionEncoding::Utf16);
        let [Outgoing::Change(change)] = out.as_slice() else {
            panic!("{out:?}");
        };
        assert_eq!(change.content_changes[0].range, None);
        assert_eq!(change.content_changes[0].text, "zero\none\ntwo\n");

        // Changes that do not add up to the new text are not trusted.
        docs.change(
            &uri(),
            9,
            Text::from("something else"),
            &[Change {
                old: 0..1,
                new: 0..1,
            }],
        )
        .unwrap();
        let out = docs.flush(0, INCREMENTAL, PositionEncoding::Utf16);
        let [Outgoing::Change(change)] = out.as_slice() else {
            panic!("{out:?}");
        };
        assert_eq!(change.content_changes[0].range, None);
        assert_eq!(change.content_changes[0].text, "something else");
    }

    #[test]
    fn close_open_save_order_and_reset() {
        let mut docs = Documents::default();
        docs.open(uri(), "text", 0, Text::from("a")).unwrap();
        // Opened and closed before any flush: the server never hears of it.
        docs.close(&uri()).unwrap();
        assert!(
            docs.flush(0, INCREMENTAL, PositionEncoding::Utf8)
                .is_empty()
        );

        docs.open(uri(), "text", 1, Text::from("a")).unwrap();
        docs.save(&uri()).unwrap();
        let out = docs.flush(0, INCREMENTAL, PositionEncoding::Utf8);
        assert!(matches!(
            out.as_slice(),
            [Outgoing::Open(_), Outgoing::Save(_)]
        ));
        docs.close(&uri()).unwrap();
        docs.open(uri(), "text", 2, Text::from("b")).unwrap();
        let out = docs.flush(0, INCREMENTAL, PositionEncoding::Utf8);
        assert!(matches!(
            out.as_slice(),
            [Outgoing::Close(_), Outgoing::Open(open)] if open.text_document.text == "b"
        ));

        // A restarted server is sent every document again.
        docs.reset(7);
        assert!(docs.has_pending());
        assert!(
            docs.flush(0, INCREMENTAL, PositionEncoding::Utf8)
                .is_empty()
        );
        let out = docs.flush(7, INCREMENTAL, PositionEncoding::Utf8);
        assert!(matches!(out.as_slice(), [Outgoing::Open(_)]));
        assert!(!docs.has_pending());
        assert!(docs.open(uri(), "text", 3, Text::from("c")).is_err());
    }
}
