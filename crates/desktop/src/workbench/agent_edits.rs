//! The pending ledger of agents' edits (slice E-10): for every file an agent's turn changed,
//! the file's text before the turn (the **base**) and as the agent left it, so that a file tab
//! highlights the turn's hunks and can reject them one by one — after switching chats or
//! models, and after a restart of the desktop (the ledger is saved next to the layout).
//!
//! Agents write files themselves (direct writes; no approve-before-write gate in the editor).
//! What the ledger learns from:
//! - `fs.changed` with origin `agent {session}` (E-3) for a watched file: an open file (its
//!   buffer knows the text before the write), a file already in the ledger (it knows its last
//!   text on disk), or a file a `file_diff` of the session named (its base is the diff taken
//!   back). A Bash write (no `file_diff`) is caught the same way as long as the file is watched.
//! - `file_diff` events of every chat (`chat.event`): which files the turn is about to touch
//!   (they are watched from then on) and the patch that takes a write back.
//! - `turn_started`: a write of a new turn starts a new base (the hunks are the current turn's).
//! - Other changes (`external`, a rescan): the ledger's last text on disk follows; the base does
//!   not move, and the hunks are recomputed against the new text.
//!
//! The checkpoint of the turn's start (CK-2) is not reachable through the protocol yet, so it is
//! not a source; a write to a file that is neither open, nor in the ledger, nor named by a
//! `file_diff` has no base and is not tracked.
//!
//! Bounds: at most [`MAX_FILES`] files (the oldest go first), texts up to [`MAX_TEXT_BYTES`]
//! (a larger file is not tracked), hints of [`MAX_HINTS`] `file_diff` paths for
//! [`HINT_TTL_MS`]. The file holds file contents and is written owner-only (0600).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext as _, Context, Task};
use hutan_client::{ChatUpdate, ClientError};
use hutan_code_view::agent::{self, AgentChanges, AgentLabel};
use hutan_core::agent::{AgentEvent, AgentEventKind, Delivery, FileChange, FileChangeKind};
use hutan_core::{SessionId, TurnId};
use hutan_protocol::ErrorCode;
use hutan_protocol::fs::{ChangeKind, FsChange, FsOrigin};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use super::view::Workbench;

/// The ledger's file, next to the layout's.
pub const LEDGER_FILE: &str = "agent-edits.json";
/// Files the ledger keeps at most.
pub const MAX_FILES: usize = 64;
/// A file larger than this is not tracked.
pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// `file_diff` paths remembered at most, and for how long.
pub const MAX_HINTS: usize = 256;
pub const HINT_TTL_MS: u64 = 10 * 60 * 1000;
const FORMAT_VERSION: u32 = 1;

/// One file the agent changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub session_id: SessionId,
    /// The turn whose changes these are (`None`: not known, e.g. a write seen before any
    /// `turn_started`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// The session's title when it wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The file before the turn changed it (normalized).
    pub base: String,
    /// The file as the agent left it.
    pub agent: String,
    /// The file as last seen on disk.
    pub disk: String,
    pub updated_ms: u64,
}

impl LedgerEntry {
    /// What a buffer of the file is given.
    pub fn changes(&self) -> AgentChanges {
        AgentChanges {
            base: Arc::from(self.base.as_str()),
            agent: Arc::from(self.agent.as_str()),
            label: AgentLabel {
                session: self.session_id.to_string(),
                title: self.title.clone(),
            },
        }
    }
}

/// A file a `file_diff` named: the session and turn about to write it, and how to get its text
/// before the write back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hint {
    path: String,
    session: SessionId,
    turn: Option<TurnId>,
    kind: FileChangeKind,
    patch: Option<String>,
    at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    version: u32,
    #[serde(default)]
    files: BTreeMap<String, LedgerEntry>,
}

/// The ledger itself: GPUI-free.
#[derive(Debug, Default)]
pub struct Ledger {
    entries: BTreeMap<String, LedgerEntry>,
    /// The turn each session is in, as `turn_started` said.
    turns: HashMap<SessionId, TurnId>,
    hints: VecDeque<Hint>,
}

impl Ledger {
    /// The ledger saved at `path` (empty when there is none or it cannot be read).
    pub fn load(path: &Path) -> Self {
        let mut ledger = Self::default();
        match std::fs::read_to_string(path) {
            Ok(json) => match serde_json::from_str::<Stored>(&json) {
                Ok(stored) if stored.version == FORMAT_VERSION => {
                    ledger.entries = stored
                        .files
                        .into_iter()
                        .filter(|(_, e)| e.base != e.agent)
                        .collect();
                }
                Ok(stored) => {
                    tracing::warn!(version = stored.version, "unknown agent ledger format");
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "agent ledger unreadable")
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(path = %path.display(), %error, "agent ledger unreadable"),
        }
        ledger
    }

    pub fn to_json(&self) -> String {
        let stored = Stored {
            version: FORMAT_VERSION,
            files: self.entries.clone(),
        };
        serde_json::to_string(&stored).unwrap_or_default()
    }

    pub fn entry(&self, path: &str) -> Option<&LedgerEntry> {
        self.entries.get(path)
    }

    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn turn_started(&mut self, session: SessionId, turn: TurnId) {
        self.turns.insert(session, turn);
    }

    pub fn current_turn(&self, session: SessionId) -> Option<TurnId> {
        self.turns.get(&session).copied()
    }

    /// A `file_diff` named `path` (canonical): remembered as a hint.
    pub fn note_diff(
        &mut self,
        path: String,
        session: SessionId,
        turn: Option<TurnId>,
        change: &FileChange,
        now_ms: u64,
    ) {
        self.hints
            .retain(|h| h.path != path && now_ms.saturating_sub(h.at_ms) < HINT_TTL_MS);
        while self.hints.len() >= MAX_HINTS {
            self.hints.pop_front();
        }
        self.hints.push_back(Hint {
            path,
            session,
            turn,
            kind: change.kind,
            patch: change.patch.clone(),
            at_ms: now_ms,
        });
    }

    /// Whether a recent `file_diff` named `path`.
    pub fn has_hint(&self, path: &str, now_ms: u64) -> bool {
        self.hints
            .iter()
            .any(|h| h.path == path && now_ms.saturating_sub(h.at_ms) < HINT_TTL_MS)
    }

    /// The text of `path` before a write that left `after`, from what a `file_diff` said.
    fn hinted_base(&self, path: &str, after: &str, now_ms: u64) -> Option<String> {
        let hint = self
            .hints
            .iter()
            .rev()
            .find(|h| h.path == path && now_ms.saturating_sub(h.at_ms) < HINT_TTL_MS)?;
        match hint.kind {
            FileChangeKind::Add => Some(String::new()),
            FileChangeKind::Delete => None,
            FileChangeKind::Update => {
                agent::reverse_patch(after, hint.patch.as_deref()?).map(|t| agent::normalize(&t))
            }
        }
    }

    /// The session `session` wrote `path`: it was `before` (normalized; `None` when not known)
    /// and is `after` now. Returns the file's changes as the ledger keeps them now (`None`: not
    /// tracked, or nothing left to show).
    pub fn agent_wrote(
        &mut self,
        path: &str,
        session: SessionId,
        title: Option<String>,
        before: Option<String>,
        after: &str,
        now_ms: u64,
    ) -> Option<AgentChanges> {
        let after = agent::normalize(after);
        let before = before.map(|b| agent::normalize(&b));
        if after.len() > MAX_TEXT_BYTES {
            self.entries.remove(path);
            return None;
        }
        let turn = self.turns.get(&session).copied();
        let same_turn = |e: &LedgerEntry| {
            // A turn not seen (the desktop started after it began) continues the entry's.
            e.session_id == session && (turn.is_none() || e.turn_id == turn)
        };
        let entry = match self.entries.remove(path) {
            Some(mut entry) if same_turn(&entry) => {
                // What happened since the agent's last write (the user's own edits, another
                // program) joins the base, except inside the agent's hunks.
                let now = before.unwrap_or_else(|| entry.disk.clone());
                entry.base = agent::rebase(&entry.base, &entry.agent, &now);
                entry.agent = after.clone();
                entry.disk = after;
                entry.updated_ms = now_ms;
                if title.is_some() {
                    entry.title = title;
                }
                entry
            }
            previous => {
                let base = before
                    .or_else(|| previous.map(|e| e.disk))
                    .or_else(|| self.hinted_base(path, &after, now_ms))?;
                if base.len() > MAX_TEXT_BYTES {
                    return None;
                }
                LedgerEntry {
                    session_id: session,
                    turn_id: turn.or_else(|| self.hint_turn(path, session)),
                    title,
                    base,
                    agent: after.clone(),
                    disk: after,
                    updated_ms: now_ms,
                }
            }
        };
        if entry.base == entry.agent {
            return None;
        }
        let changes = entry.changes();
        self.entries.insert(path.to_owned(), entry);
        self.trim();
        Some(changes)
    }

    fn hint_turn(&self, path: &str, session: SessionId) -> Option<TurnId> {
        self.hints
            .iter()
            .rev()
            .find(|h| h.path == path && h.session == session)
            .and_then(|h| h.turn)
    }

    /// The file is `text` on disk now (somebody else wrote it, or it was saved).
    pub fn disk_is(&mut self, path: &str, text: &str) {
        if let Some(entry) = self.entries.get_mut(path) {
            let text = agent::normalize(text);
            if text.len() <= MAX_TEXT_BYTES {
                entry.disk = text;
            }
        }
    }

    /// *Accept All*: the file's changes are kept and the file leaves the ledger.
    pub fn accept(&mut self, path: &str) -> bool {
        self.entries.remove(path).is_some()
    }

    /// The oldest files go past [`MAX_FILES`].
    fn trim(&mut self) {
        while self.entries.len() > MAX_FILES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.updated_ms)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }
}

/// Writes `json` to `path` owner-only, atomically.
fn save_private(path: &Path, json: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The ledger of a window, with what it needs to stay saved and listening.
#[derive(Default)]
pub struct AgentLedger {
    pub(crate) ledger: Ledger,
    path: Option<PathBuf>,
    last_saved: Option<String>,
    save_task: Option<Task<()>>,
    listener: Option<Task<()>>,
    /// Files the ledger watches itself (no tab shows them).
    pub(crate) watched: HashSet<String>,
    /// Reads of files in the ledger after a change on disk.
    reads: HashMap<String, Task<()>>,
    /// Agent writes taken into the ledger (tests wait on it).
    pub(crate) writes_seen: u64,
}

impl std::fmt::Debug for AgentLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLedger")
            .field("files", &self.ledger.len())
            .finish_non_exhaustive()
    }
}

impl AgentLedger {
    /// The ledger saved next to the layout at `state_path` (none without persistence).
    pub fn for_state(state_path: Option<&Path>) -> Self {
        let path = state_path.map(|p| p.with_file_name(LEDGER_FILE));
        let ledger = path.as_deref().map(Ledger::load).unwrap_or_default();
        let last_saved = path.is_some().then(|| ledger.to_json());
        Self {
            ledger,
            path,
            last_saved,
            ..Self::default()
        }
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Agent writes taken so far (tests).
    pub fn writes_seen(&self) -> u64 {
        self.writes_seen
    }
}

impl Workbench {
    /// The pending ledger of agents' edits (tests, the file tabs).
    pub fn agent_ledger(&self) -> &AgentLedger {
        &self.agent_ledger
    }

    /// Starts listening to the chats and watches the files of a restored ledger.
    pub(crate) fn start_agent_ledger(&mut self, cx: &mut Context<Self>) {
        let mut updates = self.sessions.read(cx).connection().chat_updates();
        self.agent_ledger.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let event = match updates.recv().await {
                    Ok(ChatUpdate::Event(event)) => event,
                    Ok(ChatUpdate::Resync { .. } | ChatUpdate::Queue(_))
                    | Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_agent_event(&event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        let paths: Vec<String> = self.agent_ledger.ledger.paths().cloned().collect();
        for path in paths {
            self.ledger_watch(&path, cx);
        }
    }

    fn on_agent_event(&mut self, event: &AgentEvent, cx: &mut Context<Self>) {
        if event.delivery == Delivery::Replay {
            return;
        }
        match &event.kind {
            AgentEventKind::TurnStarted => {
                if let Some(turn) = event.turn_id {
                    self.agent_ledger
                        .ledger
                        .turn_started(event.session_id, turn);
                }
            }
            AgentEventKind::FileDiff { changes, .. } => {
                for change in changes {
                    self.note_file_diff(event, change, cx);
                }
            }
            _ => {}
        }
    }

    /// A `file_diff` named a file: its canonical path is looked up, remembered with the patch,
    /// and watched, so the write that follows is seen.
    fn note_file_diff(&mut self, event: &AgentEvent, change: &FileChange, cx: &mut Context<Self>) {
        if change.kind == FileChangeKind::Delete {
            return;
        }
        let named = change.move_path.as_deref().unwrap_or(&change.path);
        let mut path = PathBuf::from(named);
        if path.is_relative() {
            let cwd = self
                .sessions
                .read(cx)
                .session(event.session_id)
                .and_then(|s| s.cwd.clone());
            let Some(cwd) = cwd else {
                return;
            };
            path = cwd.join(path);
        }
        let session = event.session_id;
        let turn = event
            .turn_id
            .or_else(|| self.agent_ledger.ledger.current_turn(session));
        let change = change.clone();
        let connection = self.sessions.read(cx).connection().clone();
        let canonical = cx.background_spawn(async move {
            match connection.fs_stat(&path, false).await {
                Ok(entry) => Some(entry.path),
                // Not there yet (the agent is about to create it): its folder's canonical path.
                Err(_) => {
                    let name = path.file_name()?.to_owned();
                    let parent = path.parent()?;
                    connection
                        .fs_stat(parent, false)
                        .await
                        .ok()
                        .map(|dir| dir.path.join(name))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let Some(canonical) = canonical.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let key = canonical.display().to_string();
                this.agent_ledger
                    .ledger
                    .note_diff(key.clone(), session, turn, &change, now_ms());
                this.ledger_watch(&key, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Watches `path` for the ledger when no tab does.
    fn ledger_watch(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.buffers.buffers.contains_key(path)
            || !self.agent_ledger.watched.insert(path.to_owned())
        {
            return;
        }
        self.listen_for_file_changes(cx);
        let connection = self.sessions.read(cx).connection().clone();
        let file = PathBuf::from(path);
        cx.background_spawn(async move {
            if let Err(error) = connection.fs_watch(&file, false).await {
                tracing::debug!(path = %file.display(), %error, "agent ledger: fs.watch failed");
            }
        })
        .detach();
    }

    /// Stops the ledger's own watch of `path` (a tab watches it now, or it left the ledger).
    pub(crate) fn ledger_unwatch(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.agent_ledger.watched.remove(path) && !self.buffers.buffers.contains_key(path) {
            self.unwatch_file(path, cx);
        }
    }

    /// Whether the ledger still wants `path` watched once its last tab closes.
    pub(crate) fn ledger_keeps(&self, path: &str) -> bool {
        self.agent_ledger.ledger.entry(path).is_some()
            || self.agent_ledger.ledger.has_hint(path, now_ms())
    }

    /// A tab of `path` closed: the ledger watches it from now on, or drops it when no hunk is
    /// left (`hunks_left`).
    pub(crate) fn ledger_tab_closed(
        &mut self,
        path: &str,
        hunks_left: bool,
        cx: &mut Context<Self>,
    ) {
        if !hunks_left && self.agent_ledger.ledger.accept(path) {
            self.schedule_ledger_save(cx);
        }
        if self.ledger_keeps(path) {
            // The tab's watch stays; the ledger owns it now.
            self.agent_ledger.watched.insert(path.to_owned());
            return;
        }
        self.unwatch_file(path, cx);
    }

    /// `fs.changed` for the files no tab shows: an agent's write enters the ledger, anybody
    /// else's moves its text on disk.
    pub(crate) fn ledger_files_changed(&mut self, changes: &[FsChange], cx: &mut Context<Self>) {
        for change in changes {
            let key = change.path.display().to_string();
            if self.buffers.buffers.contains_key(&key) || change.kind == ChangeKind::Removed {
                continue;
            }
            let agent = match &change.origin {
                Some(FsOrigin::Own { .. }) => continue,
                Some(FsOrigin::Agent { session_id, .. }) => Some(*session_id),
                Some(FsOrigin::External) | None => None,
            };
            let known = self.agent_ledger.ledger.entry(&key).is_some();
            if !known && !(agent.is_some() && self.agent_ledger.ledger.has_hint(&key, now_ms())) {
                continue;
            }
            let connection = self.sessions.read(cx).connection().clone();
            let file = change.path.clone();
            let read =
                cx.background_spawn(async move { connection.fs_read_all(&file, false).await });
            let path = key.clone();
            let task = cx.spawn(async move |this, cx| {
                let read = read.await;
                this.update(cx, |this, cx| {
                    this.agent_ledger.reads.remove(&path);
                    match read {
                        Ok(read) => match agent {
                            Some(session) => {
                                this.ledger_agent_wrote(&path, session, None, &read.text, cx);
                            }
                            None => {
                                this.agent_ledger.ledger.disk_is(&path, &read.text);
                                this.schedule_ledger_save(cx);
                            }
                        },
                        Err(error) => {
                            if !matches!(&error, ClientError::Remote(body) if body.code == ErrorCode::NotFound)
                            {
                                tracing::debug!(path, %error, "agent ledger: reading failed");
                            }
                        }
                    }
                })
                .ok();
            });
            self.agent_ledger.reads.insert(key, task);
        }
    }

    /// The session `session` wrote `path` (its text was `before`, when the caller knows it,
    /// and is `after` now). Returns the file's changes for its buffer.
    pub(crate) fn ledger_agent_wrote(
        &mut self,
        path: &str,
        session: SessionId,
        before: Option<&str>,
        after: &str,
        cx: &mut Context<Self>,
    ) -> Option<AgentChanges> {
        let title = self
            .sessions
            .read(cx)
            .session(session)
            .map(|s| s.title.clone())
            .filter(|t| !t.is_empty());
        let before = before.map(str::to_owned);
        let changes =
            self.agent_ledger
                .ledger
                .agent_wrote(path, session, title, before, after, now_ms());
        self.agent_ledger.writes_seen += 1;
        if changes.is_some() {
            self.ledger_watch(path, cx);
        }
        self.schedule_ledger_save(cx);
        cx.notify();
        changes
    }

    /// *Accept All* in the file of `path`: its changes are kept, its highlight goes, and it
    /// leaves the ledger.
    pub(crate) fn accept_agent_changes(&mut self, path: &str, cx: &mut Context<Self>) {
        self.agent_ledger.ledger.accept(path);
        if let Some(buffer) = self.buffers.get(path).cloned() {
            buffer.update(cx, |b, cx| b.set_agent_changes(None, cx));
        }
        self.ledger_unwatch(path, cx);
        self.schedule_ledger_save(cx);
        cx.notify();
    }

    /// The ledger's changes of `path`, for a buffer that read it.
    pub(crate) fn ledger_changes(&self, path: &str) -> Option<AgentChanges> {
        self.agent_ledger
            .ledger
            .entry(path)
            .map(LedgerEntry::changes)
    }

    fn schedule_ledger_save(&mut self, cx: &mut Context<Self>) {
        if self.agent_ledger.path.is_none() {
            return;
        }
        self.agent_ledger.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(super::SAVE_DEBOUNCE).await;
            this.update(cx, |this, _| this.flush_agent_ledger()).ok();
        }));
    }

    /// Writes the ledger now if it changed since the last write (the debounce timer, quitting,
    /// tests).
    pub fn flush_agent_ledger(&mut self) {
        let Some(path) = self.agent_ledger.path.clone() else {
            return;
        };
        let json = self.agent_ledger.ledger.to_json();
        if self.agent_ledger.last_saved.as_deref() == Some(json.as_str()) {
            return;
        }
        match save_private(&path, &json) {
            Ok(()) => self.agent_ledger.last_saved = Some(json),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot save the agent ledger")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        "01K7F3B5N2Q9W8E4R6T1Y0V3KM".parse().unwrap()
    }

    fn update(patch: &str) -> FileChange {
        FileChange {
            path: "/w/a.rs".to_owned(),
            kind: FileChangeKind::Update,
            move_path: None,
            patch: Some(patch.to_owned()),
            before: None,
            after: None,
        }
    }

    #[test]
    fn a_write_with_a_known_text_before_is_tracked() {
        let mut ledger = Ledger::default();
        let changes = ledger
            .agent_wrote(
                "/w/a.rs",
                session(),
                None,
                Some("a\nb\n".into()),
                "a\nB\n",
                1,
            )
            .unwrap();
        assert_eq!(&*changes.base, "a\nb\n");
        assert_eq!(&*changes.agent, "a\nB\n");
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn writes_of_one_turn_keep_the_first_base_and_a_new_turn_starts_over() {
        let mut ledger = Ledger::default();
        let turn1 = TurnId::new();
        ledger.turn_started(session(), turn1);
        ledger.agent_wrote(
            "/w/a.rs",
            session(),
            None,
            Some("1\n2\n3\n".into()),
            "1\nTWO\n3\n",
            1,
        );
        // A second write of the same turn (no text before given: the ledger's own is used).
        let changes = ledger
            .agent_wrote("/w/a.rs", session(), None, None, "1\nTWO\n3\nfour\n", 2)
            .unwrap();
        assert_eq!(&*changes.base, "1\n2\n3\n", "the turn's base stays");
        ledger.turn_started(session(), TurnId::new());
        let changes = ledger
            .agent_wrote("/w/a.rs", session(), None, None, "ONE\nTWO\n3\nfour\n", 3)
            .unwrap();
        assert_eq!(
            &*changes.base, "1\nTWO\n3\nfour\n",
            "a new turn starts from the file as the last turn left it"
        );
    }

    #[test]
    fn the_users_edits_between_writes_join_the_base() {
        let mut ledger = Ledger::default();
        ledger.agent_wrote(
            "/w/a.rs",
            session(),
            None,
            Some("a\nb\nc\n".into()),
            "a\nB\nc\n",
            1,
        );
        // The user added a line at the end and saved; the agent then wrote again.
        let changes = ledger
            .agent_wrote(
                "/w/a.rs",
                session(),
                None,
                Some("a\nB\nc\nmine\n".into()),
                "A\nB\nc\nmine\n",
                2,
            )
            .unwrap();
        assert_eq!(&*changes.base, "a\nb\nc\nmine\n");
    }

    #[test]
    fn a_hinted_update_is_taken_back_to_its_base() {
        let mut ledger = Ledger::default();
        let patch = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n";
        ledger.note_diff("/w/a.rs".into(), session(), None, &update(patch), 1);
        assert!(ledger.has_hint("/w/a.rs", 2));
        let changes = ledger
            .agent_wrote("/w/a.rs", session(), None, None, "a\nB\n", 2)
            .unwrap();
        assert_eq!(&*changes.base, "a\nb\n");
        // A created file's base is empty.
        let created = FileChange {
            kind: FileChangeKind::Add,
            patch: Some("hi\n".into()),
            ..update("")
        };
        ledger.note_diff("/w/new.txt".into(), session(), None, &created, 3);
        let changes = ledger
            .agent_wrote("/w/new.txt", session(), None, None, "hi\n", 4)
            .unwrap();
        assert_eq!(&*changes.base, "");
        // Old hints are forgotten.
        assert!(!ledger.has_hint("/w/a.rs", 2 + HINT_TTL_MS));
    }

    #[test]
    fn without_a_base_nothing_is_tracked() {
        let mut ledger = Ledger::default();
        assert!(
            ledger
                .agent_wrote("/w/a.rs", session(), None, None, "x\n", 1)
                .is_none()
        );
        assert!(ledger.is_empty());
    }

    #[test]
    fn a_write_back_to_the_base_leaves_nothing() {
        let mut ledger = Ledger::default();
        ledger.agent_wrote("/w/a.rs", session(), None, Some("a\n".into()), "b\n", 1);
        assert!(
            ledger
                .agent_wrote("/w/a.rs", session(), None, None, "a\n", 2)
                .is_none()
        );
        assert!(ledger.is_empty());
    }

    #[test]
    fn large_files_and_too_many_files_are_bounded() {
        let mut ledger = Ledger::default();
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(
            ledger
                .agent_wrote("/w/big", session(), None, Some(String::new()), &big, 1)
                .is_none()
        );
        for n in 0..(MAX_FILES as u64 + 3) {
            ledger.agent_wrote(
                &format!("/w/{n}"),
                session(),
                None,
                Some("a\n".into()),
                "b\n",
                n,
            );
        }
        assert_eq!(ledger.len(), MAX_FILES);
        assert!(ledger.entry("/w/0").is_none(), "the oldest went first");
    }

    #[test]
    fn the_ledger_survives_a_save_and_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let mut ledger = Ledger::default();
        ledger.agent_wrote(
            "/w/a.rs",
            session(),
            Some("Codex".into()),
            Some("a\r\nb\r\n".into()),
            "a\nB\n",
            1,
        );
        save_private(&path, &ledger.to_json()).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let loaded = Ledger::load(&path);
        let entry = loaded.entry("/w/a.rs").unwrap();
        assert_eq!(entry.base, "a\nb\n", "normalized");
        assert_eq!(entry.title.as_deref(), Some("Codex"));
        assert_eq!(loaded.entry("/w/a.rs"), ledger.entry("/w/a.rs"));
    }
}
