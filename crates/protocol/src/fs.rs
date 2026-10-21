//! The daemon's file service (protocol 0.33, capabilities `fs` and `fs.watch`; ADR 0012
//! amendment 2026-10-21 decision 3, `docs/protocol.md` § Files).
//!
//! Files are read and written by the daemon, never by a client: the daemon is the one place
//! permissions are enforced, agents and the CLI need the same access, and a remote client
//! cannot read a local disk. Control frames are at most 4 MiB, so a file travels in chunks of
//! at most 1 MiB ([`CHUNK_MAX_BYTES`]); a file is at most 64 MiB ([`FILE_MAX_BYTES`]).
//!
//! The daemon is a byte-exact transport: `text` is the file's UTF-8 bytes as they are, a
//! byte-order mark and `\r\n` included. `bom` and `line_ending` only tell the client what it
//! found, so that it can show it and write the same bytes back.
//!
//! Paths are absolute. They must lie below a root the daemon tracks (the folders of its
//! workspaces and projects, and the git checkouts of its sessions); `picked: true` (a file the
//! user chose in a dialog) additionally allows any regular file, and counts for nothing when
//! the request comes from an agent. `..` and symbolic links that lead out of the roots are
//! refused; a symbolic link that stays inside them is followed.
//!
//! Errors: `bad_request` (a relative path, a bound exceeded, a chunk out of order),
//! `not_found`, `forbidden` (outside the roots, a symbolic link out of them, writing below
//! `.git`, an unreadable file; `data.reason` is `outside_roots`, `symlink`, `git_dir` or
//! `permission`), `unsupported` (`data.reason`: `binary`, `not_utf8`, `too_large` with
//! `data.size`, `not_a_file`), `conflict` (`data.current`: the file's version now, absent when
//! it is gone; `data.reason`: `version`, `exists` or `missing`), `invalid_state` (too many
//! uploads, an upload that expired, too many searches).

use std::path::PathBuf;

use hutan_core::{CheckpointId, ItemId, SessionId};
use serde::{Deserialize, Serialize};

/// Most bytes of text in one `fs.read` answer and one `fs.write.chunk`.
pub const CHUNK_MAX_BYTES: usize = 1 << 20;
/// Largest file the service reads or writes.
pub const FILE_MAX_BYTES: u64 = 64 << 20;
/// Uploads one daemon holds open at once.
pub const UPLOADS_MAX: usize = 8;
/// An upload that receives nothing for this long is discarded.
pub const UPLOAD_IDLE_SECS: u64 = 30;
/// Most entries of one `fs.list`.
pub const LIST_MAX: u32 = 5_000;
/// Most matches of one `fs.find`.
pub const FIND_MAX: u32 = 200;
/// Most paths of one root's finder index; more are not indexed (`FsFound.index_truncated`).
pub const FIND_INDEX_MAX_PATHS: usize = 200_000;
/// Most hits of one `fs.search`.
pub const SEARCH_MAX: u32 = 1_000;
/// Longest [`SearchHit::text`], in characters.
pub const SEARCH_TEXT_MAX_CHARS: usize = 512;
/// Searches one connection runs at once.
pub const SEARCHES_MAX: usize = 4;
/// Paths one connection may watch.
pub const WATCH_MAX_PATHS: usize = 256;
/// Most changes of one `fs.changed` event; more set `overflow`.
pub const CHANGES_MAX: usize = 500;
/// Changes are gathered for this long before an `fs.changed` event is sent.
pub const COALESCE_MS: u64 = 50;
/// Shortest time between two `fs.generation` events of one root.
pub const GENERATION_INTERVAL_MS: u64 = 1_000;

/// What identifies the content a client read: a write that expects it fails with `conflict`
/// when the file is another one now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileVersion {
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    pub size: u64,
    /// Inode: a file replaced by another (an atomic save, `git checkout`) differs even when
    /// the time and size happen to match.
    pub ino: u64,
}

/// How a file ends its lines, judged by the text of one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEnding {
    /// `\n` only, or no line break at all.
    #[default]
    Lf,
    /// `\r\n` only.
    Crlf,
    /// Both.
    Mixed,
}

/// `fs.read`: up to `max_bytes` of text from byte `offset`. Answered with [`FsFile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsRead {
    pub path: PathBuf,
    /// Where to start; 0 when absent. It must be the start of a character: the previous
    /// answer's `offset + text.len()` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// At most this many bytes (default and ceiling [`CHUNK_MAX_BYTES`]). The answer ends at a
    /// character boundary and may be shorter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub picked: bool,
}

/// Result of `fs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsFile {
    /// The canonical path.
    pub path: PathBuf,
    pub text: String,
    /// Where `text` starts; the next chunk starts at `offset + text.len()`.
    pub offset: u64,
    /// The file's size when it was opened.
    pub size: u64,
    /// `text` reaches the end of the file.
    pub eof: bool,
    pub version: FileVersion,
    pub line_ending: LineEnding,
    /// The file starts with a UTF-8 byte-order mark (it is the first character of the first
    /// chunk's `text`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub bom: bool,
}

/// `fs.write.begin`: starts writing the file at `path`. The text follows in
/// [`FsWriteChunk`]s and takes effect, atomically, with [`FsWriteCommit`]; until then the file
/// is untouched (the daemon writes a temporary file next to it).
///
/// - `expect`: the version the client read; the file must still have it (`conflict` with
///   `data.current` otherwise, also when the file is gone). Without it an existing file is
///   overwritten whatever it is.
/// - `create`: the file must not exist yet (`conflict`, `data.reason` `exists`, otherwise) and
///   its directory must; `expect` must be absent. Without it a missing file is `not_found`.
///
/// A mode the file has is kept. Writing below a `.git` directory is refused. From an agent
/// this is a mutating request: it waits for the user's approval under `[control] agents =
/// "ask"` (`docs/control.md`); the chunks and the commit that follow are covered by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteBegin {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect: Option<FileVersion>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub create: bool,
    /// The size to come, for an early `unsupported` (`too_large`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_hint: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub picked: bool,
}

/// Result of `fs.write.begin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsUpload {
    pub upload_id: String,
}

/// `fs.write.chunk`: the next piece of the text, answered with `Empty`. `seq` counts from 0 and
/// must be the next one; `text` is at most [`CHUNK_MAX_BYTES`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteChunk {
    pub upload_id: String,
    pub seq: u32,
    pub text: String,
}

/// `fs.write.commit`: writes the file; `total_bytes` must be what the chunks add up to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteCommit {
    pub upload_id: String,
    pub total_bytes: u64,
}

/// `fs.write.abort`: discards an upload, answered with `Empty`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWriteAbort {
    pub upload_id: String,
}

/// Result of `fs.write.commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWritten {
    /// The canonical path written.
    pub path: PathBuf,
    /// The file as it is now.
    pub version: FileVersion,
    /// Names this write: the `fs.changed` event it causes for this connection says
    /// `origin: {kind: "self", write_id}`, so that the client does not treat its own save as
    /// somebody else's edit.
    pub write_id: String,
}

/// What a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FsKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// `fs.stat`: what the path is (a symbolic link that stays inside the roots is followed;
/// `fs.list` shows links as `symlink` entries). Answered with [`FsEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsStat {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "is_false")]
    pub picked: bool,
}

/// One path of `fs.stat` or `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    /// Absolute; canonical for `fs.stat`.
    pub path: PathBuf,
    pub kind: FsKind,
    pub size: u64,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i64,
    pub ino: u64,
    /// Matched by a `.gitignore` / `.ignore` rule (only `fs.list` with `ignore`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignored: bool,
}

impl FsEntry {
    /// The version of a file entry.
    pub fn version(&self) -> FileVersion {
        FileVersion {
            mtime_ns: self.mtime_ns,
            size: self.size,
            ino: self.ino,
        }
    }
}

fn yes() -> bool {
    true
}

fn is_yes(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `fs.list`: the entries of a directory, directories first, then by name without regard to
/// case. Answered with [`FsListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsList {
    pub dir: PathBuf,
    /// Flag the entries `.gitignore` and `.ignore` rules match (default: yes). Nothing is
    /// hidden: the client decides what to show.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub ignore: bool,
    /// At most this many entries (default and ceiling [`LIST_MAX`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub picked: bool,
}

/// Result of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsListing {
    /// The canonical directory.
    pub dir: PathBuf,
    pub entries: Vec<FsEntry>,
    /// There are more entries than `limit`, or than the daemon looks at in one directory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

/// `fs.find`: the paths below `root` that match `query` as a fuzzy subsequence of the path,
/// best first, over an index the daemon builds on first use (hidden files in, `.git`, `target`,
/// `node_modules` and what `.gitignore` excludes out) and keeps while the root is watched or
/// for a few seconds. Answered with [`FsFound`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsFind {
    pub root: PathBuf,
    pub query: String,
    /// At most this many (default 50, ceiling [`FIND_MAX`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One match of `fs.find`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsFindMatch {
    /// Relative to the root.
    pub path: String,
    pub score: u32,
    /// Byte ranges `[start, end)` of `path` the query matched.
    pub matched: Vec<[u32; 2]>,
}

/// Result of `fs.find`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsFound {
    /// The canonical root.
    pub root: PathBuf,
    pub matches: Vec<FsFindMatch>,
    /// Paths in the index.
    pub indexed: u32,
    /// The root has more files than [`FIND_INDEX_MAX_PATHS`]; the rest are not found.
    #[serde(default, skip_serializing_if = "is_false")]
    pub index_truncated: bool,
}

/// `fs.search`: the lines below `root` that match, file by file in path order; binary files
/// and files above 4 MiB are skipped, `.gitignore` rules, `.git`, `target` and `node_modules`
/// are honoured. The answer comes when the search ends; meanwhile the connection receives
/// `fs.search.progress` events, and `fs.search.cancel` with the same `search_id` (sent on this
/// connection: the daemon reads it while the search runs) ends it early with what was found
/// (`cancelled`). Answered with [`FsSearchResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearch {
    /// Chosen by the client, unique among its running searches.
    pub search_id: String,
    pub root: PathBuf,
    pub pattern: String,
    /// `pattern` is a regular expression; otherwise it is literal text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub regex: bool,
    /// Case-sensitive matching; absent means smart case (sensitive when `pattern` has an
    /// upper-case letter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_sensitive: Option<bool>,
    /// Glob patterns relative to the root that restrict the files; a leading `!` excludes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    /// At most this many hits (default 200, ceiling [`SEARCH_MAX`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One matching line of `fs.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    /// Relative to the root.
    pub path: String,
    /// One-based.
    pub line: u32,
    /// The line without its break, cut at [`SEARCH_TEXT_MAX_CHARS`] characters.
    pub text: String,
    /// Byte ranges `[start, end)` of `text` the pattern matched (those inside the cut).
    pub matched: Vec<[u32; 2]>,
}

/// Result of `fs.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearchResult {
    pub search_id: String,
    pub hits: Vec<SearchHit>,
    pub files_searched: u64,
    /// There are more hits than `limit`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// `fs.search.cancel` ended the search.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancelled: bool,
}

/// `fs.search.cancel`: ends the search of this connection named `search_id`; answered with
/// `Empty`, also when it has ended already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsSearchCancel {
    pub search_id: String,
}

/// A path to watch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWatchPath {
    pub path: PathBuf,
    /// A directory's whole subtree; otherwise the file, or the entries of the directory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub recursive: bool,
}

/// `fs.watch`: the connection receives `fs.changed` for these paths (open documents, expanded
/// folders) and `fs.generation` for their roots, until `fs.unwatch` or until it ends. The
/// daemon watches a root only while some connection holds a path in it. At most
/// [`WATCH_MAX_PATHS`] paths per connection; a path watched twice counts once. Answered with
/// [`FsWatching`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWatch {
    pub paths: Vec<FsWatchPath>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub picked: bool,
}

/// `fs.unwatch`: releases paths (a path that was not watched is ignored). Answered with
/// [`FsWatching`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsUnwatch {
    pub paths: Vec<PathBuf>,
}

/// Result of `fs.watch` and `fs.unwatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsWatching {
    /// Paths this connection watches now.
    pub watching: u32,
    /// The roots watched on its behalf, canonical: what `fs.generation` names.
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
    /// Only where the platform pairs the two names; otherwise a `removed` and a `created`.
    Renamed,
}

/// How sure the daemon is that an agent made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConfidence {
    /// A `file_diff` of the session's journal names the path, shortly before.
    ToolCall,
    /// An agent of the session was working in a folder that contains the path.
    Session,
}

/// Who made a change. A heuristic, and the daemon says so: the user's own save is exact
/// (`self`), and so is a restore of files (`restore`); the rest is inferred. A terminal agent
/// without a Hutan integration is `external`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FsOrigin {
    /// This connection's own `fs.write` named `write_id`.
    #[serde(rename = "self")]
    Own {
        write_id: String,
    },
    Agent {
        session_id: SessionId,
        /// The journal item of the tool call (`confidence: tool_call` only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<ItemId>,
        confidence: AgentConfidence,
    },
    /// Protocol 0.38 (capability `checkpoint.restore`): the daemon restored the file
    /// (`checkpoint.restore`); `checkpoint_id` is the restore's own checkpoint, whose restore
    /// is Undo.
    Restore {
        checkpoint_id: CheckpointId,
    },
    External,
}

/// One change of `fs.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// The old path of a `renamed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PathBuf>,
    /// The file after the change (absent for `removed`, directories and what vanished again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<FileVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<FsOrigin>,
}
