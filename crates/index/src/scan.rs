//! Reading transcript files incrementally.
//!
//! A file is looked at only when its size or mtime differs from the `sources` row. Transcripts
//! are append-only: a changed file is read from the stored byte offset, whole lines only.
//! Before trusting the offset the scanner re-reads the up-to-64 bytes in front of it and compares
//! their hash: a file that was truncated or rewritten (shorter, or different content before
//! the offset) is dropped from the index and read again from the start. Rows and the new offset
//! are committed together, in batches, so a crash never skips or repeats a line.

use std::collections::HashMap;
use std::fs::{self, File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use tracing::{debug, warn};

use crate::adapters::{
    Adapter, MAX_MESSAGE_CHUNKS, MAX_TOOL_CHUNKS, MAX_TOOL_ROWS, ParseState, Parsed,
};
use crate::error::{IndexError, Result};
use crate::redact::redact;
use crate::store::{self, SourceRow, Stamp};
use crate::summary::Summary;
use crate::text::{chunks, idents};
use crate::types::{Entry, Role};

/// Longest line read into memory; a longer one (a pasted image, a huge tool result) is skipped
/// and counted as an error.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// Lines per transaction.
const BATCH_LINES: usize = 1_000;
/// Bytes read per transaction, whichever limit comes first.
const BATCH_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes in front of the offset that must still match.
const TAIL_BYTES: usize = 64;
/// The first line of a file is looked at again, at most this long.
const HEAD_BYTES: u64 = 64 * 1024;

/// What the scanner did to the file system, for tests and the status of "disabled means no
/// reads".
#[derive(Debug, Default)]
pub struct Metrics {
    /// Files opened for reading.
    pub files_opened: AtomicU64,
    /// Bytes read from transcripts.
    pub bytes_read: AtomicU64,
    /// Files whose rows were dropped because they were truncated, rewritten, or of an older
    /// format.
    pub reindexed: AtomicU64,
}

impl Metrics {
    pub fn files_opened(&self) -> u64 {
        self.files_opened.load(Ordering::Relaxed)
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read.load(Ordering::Relaxed)
    }

    pub fn reindexed(&self) -> u64 {
        self.reindexed.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScanContext<'a> {
    /// Files last modified before this (Unix ms), and rows older than it, are not indexed;
    /// 0 keeps everything.
    pub cutoff_ms: u64,
    pub metrics: &'a Metrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// Missing, not a regular file, or older than the history range.
    Skipped,
    /// Size, mtime, and format version match the `sources` row: nothing was read.
    Unchanged,
    Indexed {
        /// Rows added.
        rows: u64,
        /// Lines that could not be read.
        errors: u64,
        /// The file's earlier rows were dropped first.
        reindexed: bool,
    },
}

pub fn mtime_ns(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

/// The last [`TAIL_BYTES`] bytes consumed so far.
#[derive(Debug, Default)]
struct Tail(Vec<u8>);

impl Tail {
    fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= TAIL_BYTES {
            self.0.clear();
            self.0.extend_from_slice(&bytes[bytes.len() - TAIL_BYTES..]);
            return;
        }
        self.0.extend_from_slice(bytes);
        if self.0.len() > TAIL_BYTES {
            let drop = self.0.len() - TAIL_BYTES;
            self.0.drain(..drop);
        }
    }

    fn hash(&self) -> u64 {
        fnv1a(&self.0)
    }
}

/// The bytes just before `offset`, if the file still has that many; `None` when the file is
/// shorter than `offset`.
fn read_tail(file: &mut File, offset: u64) -> Result<Option<Vec<u8>>> {
    let io = |e| IndexError::io("checking the end of an indexed file")(e);
    let len = file.metadata().map_err(io)?.len();
    if len < offset {
        return Ok(None);
    }
    let want = usize::try_from(offset.min(TAIL_BYTES as u64)).unwrap_or(TAIL_BYTES);
    file.seek(SeekFrom::Start(offset - want as u64))
        .map_err(io)?;
    let mut buf = vec![0; want];
    file.read_exact(&mut buf).map_err(io)?;
    Ok(Some(buf))
}

/// One line read from a transcript.
struct RawLine {
    /// Bytes consumed from the file, newline included.
    consumed: u64,
    /// Ended with `\n` (otherwise it is the unfinished last line of the file).
    terminated: bool,
    /// Longer than [`MAX_LINE_BYTES`]; `bytes` is empty.
    oversized: bool,
}

/// Reads up to and including the next newline into `bytes` (without the newline). `None` at
/// the end of the file. `tail` receives the last [`TAIL_BYTES`] bytes of what was consumed.
fn read_line(
    reader: &mut impl BufRead,
    bytes: &mut Vec<u8>,
    tail: &mut Tail,
) -> std::io::Result<Option<RawLine>> {
    bytes.clear();
    tail.0.clear();
    let mut consumed = 0_u64;
    let mut oversized = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok((consumed > 0).then_some(RawLine {
                consumed,
                terminated: false,
                oversized,
            }));
        }
        let (take, found) = match buf.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buf.len(), false),
        };
        let content = &buf[..if found { take - 1 } else { take }];
        if !oversized {
            if bytes.len() + content.len() > MAX_LINE_BYTES {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(content);
            }
        }
        tail.push(&buf[..take]);
        consumed += take as u64;
        reader.consume(take);
        if found {
            return Ok(Some(RawLine {
                consumed,
                terminated: true,
                oversized,
            }));
        }
    }
}

/// Redacts, chunks, and inserts one entry of a session; returns the rows added.
fn index_entry(
    conn: &Connection,
    session_id: i64,
    entry: &Entry,
    ts_ms: Option<u64>,
    cutoff_ms: u64,
    state: &mut ParseState,
) -> Result<u64> {
    if cutoff_ms > 0 && ts_ms.is_some_and(|ts| ts < cutoff_ms) {
        return Ok(0);
    }
    let is_tool = entry.role == Role::Tool;
    if is_tool && state.tool_rows >= MAX_TOOL_ROWS {
        return Ok(0);
    }
    let text = redact(&entry.text);
    let limit = if is_tool {
        MAX_TOOL_CHUNKS
    } else {
        MAX_MESSAGE_CHUNKS
    };
    let mut added = 0;
    for piece in chunks(&text).into_iter().take(limit) {
        if is_tool && state.tool_rows >= MAX_TOOL_ROWS {
            break;
        }
        store::insert_entry(
            conn,
            session_id,
            entry.role,
            ts_ms,
            None,
            None,
            piece,
            &idents(piece),
        )?;
        added += 1;
        if is_tool {
            state.tool_rows += 1;
        }
    }
    Ok(added)
}

/// Indexes what is new in `path`. See the module comment for the rules.
pub fn process_file(
    conn: &mut Connection,
    adapter: &dyn Adapter,
    path: &Path,
    ctx: ScanContext<'_>,
) -> Result<FileOutcome> {
    let meta = match fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Ok(FileOutcome::Skipped),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileOutcome::Skipped),
        Err(e) => return Err(IndexError::io(format!("inspecting {}", path.display()))(e)),
    };
    let size = meta.len();
    let mtime = mtime_ns(&meta);
    let mtime_ms = u64::try_from(mtime / 1_000_000).unwrap_or(0);
    if ctx.cutoff_ms > 0 && mtime_ms < ctx.cutoff_ms {
        return Ok(FileOutcome::Skipped);
    }
    let path_text = path.to_string_lossy().into_owned();
    let version = adapter.format_version();
    let known = store::source_row(conn, &path_text)?;
    if let Some(row) = &known
        && row.size == size
        && row.mtime_ns == mtime
        && row.format_version == version
    {
        return Ok(FileOutcome::Unchanged);
    }

    let context = format!("reading {}", path.display());
    let mut file = File::open(path).map_err(IndexError::io(context.clone()))?;
    ctx.metrics.files_opened.fetch_add(1, Ordering::Relaxed);

    // Append, or start over.
    let mut tail = Tail::default();
    let mut line_tail = Tail::default();
    let resume = match &known {
        Some(row) if row.format_version == version => match read_tail(&mut file, row.offset)? {
            Some(bytes) if fnv1a(&bytes) == row.tail_hash => {
                tail.push(&bytes);
                Some(row)
            }
            _ => None,
        },
        _ => None,
    };
    let reindexed = known.is_some() && resume.is_none();
    let (mut state, mut offset, mut rows, mut errors) = match resume {
        Some(row) => (row.state.clone(), row.offset, row.rows, row.errors),
        None => (
            ParseState {
                key: adapter.key_from_path(path),
                summary: Summary {
                    subagent: adapter.is_subagent(path),
                    ..Summary::default()
                },
                ..ParseState::default()
            },
            0,
            0,
            0,
        ),
    };
    if reindexed {
        ctx.metrics.reindexed.fetch_add(1, Ordering::Relaxed);
        debug!(path = %path.display(), "file was rewritten or truncated; indexing it again");
    }

    let mut source_row = SourceRow {
        id: 0,
        path: path_text,
        source: adapter.source(),
        // 0/0 never matches a stat: until the final commit a crash resumes from the offset.
        size: 0,
        mtime_ns: 0,
        offset,
        tail_hash: tail.hash(),
        format_version: version,
        state: state.clone(),
        rows,
        errors,
        last_indexed_ms: now_ms(),
    };
    {
        let tx = conn
            .transaction()
            .map_err(IndexError::sqlite("starting a scan"))?;
        if let Some(row) = known.as_ref().filter(|_| reindexed) {
            store::delete_file_rows(&tx, row.id)?;
        }
        source_row.id = store::upsert_source(&tx, &source_row)?;
        tx.commit()
            .map_err(IndexError::sqlite("committing a scan"))?;
    }

    if resume.is_some() && adapter.rereads_head() {
        reread_head(path, adapter, &mut state);
    }

    file.seek(SeekFrom::Start(offset))
        .map_err(IndexError::io(context.clone()))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut line = Vec::new();
    let mut last_ts = None;
    let mut added_total = 0_u64;
    let errors_before = errors;
    let mut done = false;
    while !done {
        let tx = conn
            .transaction()
            .map_err(IndexError::sqlite("starting a batch"))?;
        let mut session_id: Option<i64> = None;
        let mut lines = 0;
        let mut bytes = 0_u64;
        while lines < BATCH_LINES && bytes < BATCH_BYTES {
            let Some(raw) = read_line(&mut reader, &mut line, &mut line_tail)
                .map_err(IndexError::io(context.clone()))?
            else {
                done = true;
                break;
            };
            ctx.metrics
                .bytes_read
                .fetch_add(raw.consumed, Ordering::Relaxed);
            if !raw.terminated && !(line.first() == Some(&b'{') && is_complete_json(&line)) {
                // An unfinished last line: leave it for when it is complete.
                done = true;
                break;
            }
            if !raw.terminated {
                done = true;
            }
            tail.push(&line_tail.0);
            offset += raw.consumed;
            bytes += raw.consumed;
            lines += 1;
            if raw.oversized {
                errors += 1;
                continue;
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                errors += 1;
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            match adapter.parse_line(text, &mut state) {
                Parsed::Skip => {}
                Parsed::Malformed => errors += 1,
                Parsed::Entries(entries) => {
                    for entry in &entries {
                        if entry.text.trim().is_empty() {
                            continue;
                        }
                        let ts = entry.ts_ms.or(last_ts).or(Some(mtime_ms));
                        last_ts = entry.ts_ms.or(last_ts);
                        let id = match session_id {
                            Some(id) => id,
                            None => {
                                let id = store::ensure_session(
                                    &tx,
                                    adapter.source(),
                                    &state.key,
                                    Some(source_row.id),
                                    state.title.as_deref(),
                                    state.cwd.as_deref(),
                                )?;
                                session_id = Some(id);
                                id
                            }
                        };
                        let added = index_entry(&tx, id, entry, ts, ctx.cutoff_ms, &mut state)?;
                        rows += added;
                        added_total += added;
                    }
                }
            }
        }
        // Title, cwd, key and the catalog summary may have changed on lines that carry no
        // text (and a file with nothing to index still has its place in the catalog).
        let id = store::ensure_session(
            &tx,
            adapter.source(),
            &state.key,
            Some(source_row.id),
            state.title.as_deref(),
            state.cwd.as_deref(),
        )?;
        store::save_summary(&tx, id, &state.summary, Some(mtime_ms))?;
        source_row.offset = offset;
        source_row.tail_hash = tail.hash();
        source_row.state = state.clone();
        source_row.rows = rows;
        source_row.errors = errors;
        source_row.last_indexed_ms = now_ms();
        if done {
            source_row.size = size;
            source_row.mtime_ns = mtime;
        }
        store::upsert_source(&tx, &source_row)?;
        tx.commit()
            .map_err(IndexError::sqlite("committing a batch"))?;
        if !done {
            std::thread::yield_now();
        }
    }
    Ok(FileOutcome::Indexed {
        rows: added_total,
        errors: errors - errors_before,
        reindexed,
    })
}

fn is_complete_json(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
}

/// Feeds the first line of the file to the adapter, only for what it puts into `state`.
fn reread_head(path: &Path, adapter: &dyn Adapter, state: &mut ParseState) {
    let Ok(file) = File::open(path) else { return };
    let mut head = Vec::new();
    if BufReader::new(file.take(HEAD_BYTES))
        .read_until(b'\n', &mut head)
        .is_err()
    {
        return;
    }
    if let Ok(text) = std::str::from_utf8(&head) {
        // Entries of this line are dropped: the line itself was indexed (and folded into the
        // summary) when it was appended; only the titles it carries are taken again.
        let folded = state.summary.clone();
        let _ = adapter.parse_line(text.trim_end(), state);
        let (custom, generated) = (
            state.summary.custom_title.clone(),
            state.summary.generated_title.clone(),
        );
        state.summary = folded;
        state.summary.custom_title = custom;
        state.summary.generated_title = generated;
    }
}

/// What one `stat` pass found below a source root.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The transcript files that are new or whose stamp differs from the `sources` table.
    pub changed: Vec<PathBuf>,
    /// Every transcript file the adapter reads (inside the history range), changed or not.
    pub total: u64,
}

/// The transcript files below `root` that `adapter` reads and whose stamp differs from the
/// `sources` table (new or changed). Only `readdir` and `stat` run; no file is opened.
pub fn changed_files(
    conn: &Connection,
    adapter: &dyn Adapter,
    root: &Path,
    cutoff_ms: u64,
) -> Result<Vec<PathBuf>> {
    Ok(list_files(conn, adapter, root, cutoff_ms)?.changed)
}

/// [`changed_files`], and how many transcript files there are in all.
pub fn list_files(
    conn: &Connection,
    adapter: &dyn Adapter,
    root: &Path,
    cutoff_ms: u64,
) -> Result<Listing> {
    let known: HashMap<String, Stamp> = store::all_stamps(conn)?;
    let version = adapter.format_version();
    let mut listing = Listing::default();
    for_each_file(root, &mut |path, meta| {
        if !adapter.accepts(path) {
            return;
        }
        let mtime = mtime_ns(meta);
        if cutoff_ms > 0 && u64::try_from(mtime / 1_000_000).unwrap_or(0) < cutoff_ms {
            return;
        }
        listing.total += 1;
        let stamp = Stamp {
            size: meta.len(),
            mtime_ns: mtime,
            format_version: version,
        };
        if known.get(path.to_string_lossy().as_ref()) != Some(&stamp) {
            listing.changed.push(path.to_owned());
        }
    });
    Ok(listing)
}

/// Calls `visit` for every regular file below `root` (symlinks are not followed).
pub fn for_each_file(root: &Path, visit: &mut dyn FnMut(&Path, &Metadata)) {
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(dir = %dir.display(), error = %e, "cannot read a history directory");
                }
                continue;
            }
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                visit(&entry.path(), &meta);
            }
        }
    }
}
