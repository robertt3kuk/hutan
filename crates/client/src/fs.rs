//! Typed calls of the daemon's file service (protocol 0.33, capabilities `fs`, `fs.watch`;
//! `docs/protocol.md` § Files). Files travel in chunks of at most 1 MiB; [`Client::fs_read_all`]
//! and [`Client::fs_write`] do the chunking. `fs.changed`, `fs.generation` and
//! `fs.search.progress` arrive on [`Client::events`](Client::events).

use std::path::Path;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::fs::{
    CHUNK_MAX_BYTES, FileVersion, FsCopy, FsCreate, FsEntry, FsFile, FsFind, FsFound, FsList,
    FsListing, FsMove, FsOpResults, FsRead, FsRename, FsRestore, FsSearch, FsSearchCancel,
    FsSearchResult, FsStat, FsTrash, FsTrashed, FsUnwatch, FsUpload, FsWatch, FsWatching,
    FsWriteAbort, FsWriteBegin, FsWriteChunk, FsWriteCommit, FsWritten, LineEnding,
};
use hutan_protocol::messages::{Empty, Request};

use crate::client::{Client, FS_CAPABILITY, FS_OPS_CAPABILITY, FS_WATCH_CAPABILITY};
use crate::error::ClientError;

/// A whole file as [`Client::fs_read_all`] read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsText {
    /// The canonical path.
    pub path: std::path::PathBuf,
    /// The file's text exactly as it is on disk (byte-order mark and `\r\n` included).
    pub text: String,
    /// The version of the file when it was read: what [`FsWriteBegin::expect`] takes.
    pub version: FileVersion,
    pub line_ending: LineEnding,
    pub bom: bool,
}

/// Reads of a file that changed while it was being read are restarted this many times.
const READ_ATTEMPTS: usize = 3;
/// The JSON form of one `fs.write.chunk` stays below this, so that a frame (4 MiB) holds it
/// even when the text is full of characters JSON escapes.
const CHUNK_JSON_MAX: usize = 3 << 20;

impl Client {
    /// True when the daemon advertised capability `fs` (protocol 0.33): the `fs.*` requests.
    pub fn supports_fs(&self) -> bool {
        self.has_capability(FS_CAPABILITY)
    }

    /// True when the daemon advertised capability `fs.watch`: `fs.watch` and the `fs.changed`
    /// and `fs.generation` events.
    pub fn supports_fs_watch(&self) -> bool {
        self.has_capability(FS_WATCH_CAPABILITY)
    }

    /// True when the daemon advertised capability `fs.ops` (protocol 0.54): `fs.create`,
    /// `fs.rename`, `fs.move`, `fs.copy`, `fs.trash` and `fs.restore`.
    pub fn supports_fs_ops(&self) -> bool {
        self.has_capability(FS_OPS_CAPABILITY)
    }

    /// `fs.read`: one chunk of a file.
    pub async fn fs_read(&self, req: FsRead) -> Result<FsFile, ClientError> {
        self.call(Request::FsRead(req)).await
    }

    /// Reads the whole file chunk by chunk. `picked`: the user chose the file (allowed outside
    /// the workspace roots, never for an agent). A file that changes meanwhile is read again
    /// from the start, up to three times, then the call fails with `conflict`.
    pub async fn fs_read_all(&self, path: &Path, picked: bool) -> Result<FsText, ClientError> {
        'attempt: for _ in 0..READ_ATTEMPTS {
            let mut text = String::new();
            let mut first: Option<FsFile> = None;
            loop {
                let chunk = self
                    .fs_read(FsRead {
                        path: path.to_owned(),
                        offset: Some(text.len() as u64),
                        max_bytes: None,
                        picked,
                    })
                    .await?;
                if first
                    .as_ref()
                    .is_some_and(|first| first.version != chunk.version)
                {
                    continue 'attempt;
                }
                text.push_str(&chunk.text);
                let eof = chunk.eof;
                if first.is_none() {
                    first = Some(chunk);
                }
                if eof {
                    break;
                }
            }
            let Some(first) = first else { continue };
            return Ok(FsText {
                path: first.path,
                text,
                version: first.version,
                line_ending: first.line_ending,
                bom: first.bom,
            });
        }
        let mut error = ErrorBody::new(
            ErrorCode::Conflict,
            format!("{} kept changing while it was read", path.display()),
        );
        error.data = Some(serde_json::json!({"reason": "changed_while_reading"}));
        Err(ClientError::Remote(error))
    }

    /// `fs.write.begin`.
    pub async fn fs_write_begin(&self, req: FsWriteBegin) -> Result<FsUpload, ClientError> {
        self.call(Request::FsWriteBegin(req)).await
    }

    /// `fs.write.chunk`.
    pub async fn fs_write_chunk(&self, req: FsWriteChunk) -> Result<(), ClientError> {
        self.call::<Empty>(Request::FsWriteChunk(req))
            .await
            .map(|_| ())
    }

    /// `fs.write.commit`.
    pub async fn fs_write_commit(&self, req: FsWriteCommit) -> Result<FsWritten, ClientError> {
        self.call(Request::FsWriteCommit(req)).await
    }

    /// `fs.write.abort`.
    pub async fn fs_write_abort(&self, upload_id: &str) -> Result<(), ClientError> {
        self.call::<Empty>(Request::FsWriteAbort(FsWriteAbort {
            upload_id: upload_id.to_owned(),
        }))
        .await
        .map(|_| ())
    }

    /// Writes `text` as the file `req.path`: begin, the chunks, commit; a failed chunk or
    /// commit aborts the upload. `size_hint` is filled in. A refused write (`conflict` with
    /// the file's current version in `data.current`, `forbidden`, a denied approval) is the
    /// error of the call.
    pub async fn fs_write(
        &self,
        mut req: FsWriteBegin,
        text: &str,
    ) -> Result<FsWritten, ClientError> {
        req.size_hint = Some(text.len() as u64);
        let upload = self.fs_write_begin(req).await?;
        let sent = async {
            for (seq, piece) in chunks(text).enumerate() {
                self.fs_write_chunk(FsWriteChunk {
                    upload_id: upload.upload_id.clone(),
                    seq: u32::try_from(seq).unwrap_or(u32::MAX),
                    text: piece.to_owned(),
                })
                .await?;
            }
            self.fs_write_commit(FsWriteCommit {
                upload_id: upload.upload_id.clone(),
                total_bytes: text.len() as u64,
            })
            .await
        }
        .await;
        if sent.is_err() {
            // Already gone when the daemon refused the chunk or commit; a no-op then.
            let _ = self.fs_write_abort(&upload.upload_id).await;
        }
        sent
    }

    /// `fs.create`: an empty file or a folder, never over something that exists.
    pub async fn fs_create(&self, req: FsCreate) -> Result<FsEntry, ClientError> {
        self.call(Request::FsCreate(req)).await
    }

    /// `fs.rename`: `from` gets the path `to` in the same root, never over something.
    pub async fn fs_rename(&self, req: FsRename) -> Result<FsEntry, ClientError> {
        self.call(Request::FsRename(req)).await
    }

    /// `fs.move`: a batch of renames; each item has its own entry or error.
    pub async fn fs_move(&self, req: FsMove) -> Result<FsOpResults, ClientError> {
        self.call(Request::FsMove(req)).await
    }

    /// `fs.copy`: a batch of copies; each item has its own entry or error.
    pub async fn fs_copy(&self, req: FsCopy) -> Result<FsOpResults, ClientError> {
        self.call(Request::FsCopy(req)).await
    }

    /// `fs.trash`: moves paths to the system Trash; the ticket in the answer puts them back.
    pub async fn fs_trash(&self, req: FsTrash) -> Result<FsTrashed, ClientError> {
        self.call(Request::FsTrash(req)).await
    }

    /// `fs.restore`: puts back the items of a trash ticket.
    pub async fn fs_restore(&self, req: FsRestore) -> Result<FsOpResults, ClientError> {
        self.call(Request::FsRestore(req)).await
    }

    /// `fs.stat`.
    pub async fn fs_stat(&self, path: &Path, picked: bool) -> Result<FsEntry, ClientError> {
        self.call(Request::FsStat(FsStat {
            path: path.to_owned(),
            picked,
        }))
        .await
    }

    /// `fs.list`.
    pub async fn fs_list(&self, req: FsList) -> Result<FsListing, ClientError> {
        self.call(Request::FsList(req)).await
    }

    /// `fs.find`.
    pub async fn fs_find(&self, req: FsFind) -> Result<FsFound, ClientError> {
        self.call(Request::FsFind(req)).await
    }

    /// `fs.search`: answered when the search ends; `fs.search.progress` events arrive meanwhile
    /// and [`fs_search_cancel`](Self::fs_search_cancel) ends it early.
    pub async fn fs_search(&self, req: FsSearch) -> Result<FsSearchResult, ClientError> {
        self.call(Request::FsSearch(req)).await
    }

    /// `fs.search.cancel`.
    pub async fn fs_search_cancel(&self, search_id: &str) -> Result<(), ClientError> {
        self.call::<Empty>(Request::FsSearchCancel(FsSearchCancel {
            search_id: search_id.to_owned(),
        }))
        .await
        .map(|_| ())
    }

    /// `fs.watch`: this connection gets `fs.changed` for the paths.
    pub async fn fs_watch(&self, req: FsWatch) -> Result<FsWatching, ClientError> {
        self.call(Request::FsWatch(req)).await
    }

    /// `fs.unwatch`.
    pub async fn fs_unwatch(&self, req: FsUnwatch) -> Result<FsWatching, ClientError> {
        self.call(Request::FsUnwatch(req)).await
    }
}

/// `text` in pieces of at most 1 MiB whose JSON form stays below [`CHUNK_JSON_MAX`], cut on
/// character boundaries. Empty text is one empty piece less: a commit of zero bytes follows no
/// chunk.
fn chunks(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut take = rest.len().min(CHUNK_MAX_BYTES);
        loop {
            while !rest.is_char_boundary(take) {
                take -= 1;
            }
            let piece = &rest[..take];
            if json_len(piece) <= CHUNK_JSON_MAX || take <= 1 {
                rest = &rest[take..];
                return Some(piece);
            }
            take /= 2;
        }
    })
}

/// The length of `text` as a JSON string, quotes excluded.
fn json_len(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf8(),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_in_pieces_that_fit_a_frame_and_join_again() {
        assert_eq!(chunks("").count(), 0);
        assert_eq!(chunks("abc").collect::<Vec<_>>(), ["abc"]);
        let text = format!("{}é{}", "x".repeat(CHUNK_MAX_BYTES - 1), "y".repeat(10));
        let pieces: Vec<&str> = chunks(&text).collect();
        assert!(pieces.len() >= 2);
        assert!(pieces.iter().all(|p| p.len() <= CHUNK_MAX_BYTES));
        assert_eq!(pieces.concat(), text);
        // Control characters escape to six bytes each.
        let nasty = "\u{1}".repeat(2 * CHUNK_MAX_BYTES);
        let pieces: Vec<&str> = chunks(&nasty).collect();
        assert!(pieces.iter().all(|p| json_len(p) <= CHUNK_JSON_MAX));
        assert_eq!(pieces.concat(), nasty);
    }
}
