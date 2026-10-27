//! [`FileBuffer`]: the text of one open file, shared by every view of it, with its load state,
//! its undo history and — while some view of it is on screen — its syntax.
//!
//! The owner (the desktop's buffer store) reads the file through the daemon and hands the text
//! over with [`FileBuffer::set_loaded`]; views edit it through [`FileBuffer::edit`],
//! [`FileBuffer::undo`] and [`FileBuffer::redo`], which apply `hutan-buffer` transactions, feed
//! every edit to the parser (an incremental reparse, never a full one) and announce it with
//! [`BufferEdited`] so that the other views carry their selections across it. A tree-sitter tree
//! costs about 37 bytes per byte of source (`docs/perf.md` § Editor: syntax), so the buffer keeps
//! a [`Syntax`] only while [`FileBuffer::set_syntax_active`] says a view of it is visible, and
//! never for a file in large-file mode; the rope stays either way and the tree is parsed again on
//! show.
//!
//! E-10: the owner may give the buffer an agent's changes ([`FileBuffer::set_agent_changes`]);
//! the buffer then keeps the agent's hunks in its current text, recomputed in the background
//! after every change (at once after a reload or a Reject, after a pause while the user types,
//! after a longer one for a large or minified file) and carried across edits until then.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{AppContext as _, Context, EntityId, EventEmitter, Task};
use hutan_buffer::{Applied, Bias, Buffer, LineEnding, Selections, Text, Transaction};
use hutan_syntax::{Language, Syntax, SyntaxStats, SyntaxView};

use crate::agent::{self, AgentChanges, AgentHunk};
use crate::blame::{BlameData, BlameLine, BlameState, MAX_BLAME_BYTES};
use crate::diagnostics::{self, Diagnostic, DiagnosticCounts, DiagnosticInput};
use crate::git_gutter::GitBase;
use crate::rules::IndentUnit;

/// Typing that pauses longer than this starts a new undo step.
pub const TYPING_GROUP_PAUSE: Duration = Duration::from_secs(1);

/// How many edits a buffer remembers for [`FileBuffer::edits_since`].
pub const EDIT_LOG_LEN: usize = 256;

/// Why a file cannot be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    /// A NUL byte: not text.
    Binary,
    /// Not UTF-8 (Hutan does not convert encodings).
    NotUtf8,
    /// Above the daemon's hard limit (64 MiB).
    TooLarge,
    /// A folder, a device, …
    NotAFile,
    /// Gone.
    Missing,
    /// Outside what the daemon lets this window read.
    Forbidden,
    /// Anything else (the daemon is not connected, …).
    Other,
}

impl FailReason {
    /// Whether "Open with System" can help: the file exists and is a file.
    pub fn system_can_open(self) -> bool {
        matches!(
            self,
            FailReason::Binary | FailReason::NotUtf8 | FailReason::TooLarge
        )
    }
}

/// Where a buffer is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Being read.
    Loading,
    /// Larger than `editor.large_file_bytes`: nothing is read until the user confirms; it then
    /// opens without syntax.
    NeedsConfirm { size: u64 },
    /// The text is in.
    Ready,
    /// It cannot be shown.
    Failed { reason: FailReason, message: String },
}

/// The file on disk is no longer what the buffer was read from, and the buffer has changes of
/// its own (or a save was refused): the owner shows the conflict bar and never writes over the
/// file without being told to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// Somebody else changed the file. `disk` is its text now, when it could be read (for
    /// *Show diff* and *Reload*); `by` names the agent that wrote it when the daemon said so
    /// (`None`: not known).
    Changed {
        disk: Option<Arc<str>>,
        by: Option<Arc<str>>,
    },
    /// The file was deleted (or moved away).
    Deleted,
}

/// How an edit joins the undo history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coalesce {
    /// A step of its own.
    Step,
    /// Typing: joins the typing of the same view that came less than [`TYPING_GROUP_PAUSE`]
    /// before, so a typed word (or an input method's composition) is one undo step.
    Typing,
}

/// What changed the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Edit,
    Undo,
    Redo,
    /// The file changed on disk and the buffer took the new text (`apply_external`).
    Reload,
}

/// The text of the buffer changed by `transaction` (in the coordinates of the text before it).
/// `origin` is the view that made the change; every other view maps its selections through it.
#[derive(Debug, Clone)]
pub struct BufferEdited {
    pub transaction: Transaction,
    pub origin: Option<EntityId>,
    pub kind: EditKind,
    /// The buffer's [`version`](FileBuffer::version) after the change.
    pub version: u64,
}

struct OpenGroup {
    owner: EntityId,
    last: Instant,
}

/// An agent's changes to the file and where they are in the text now.
struct AgentState {
    changes: AgentChanges,
    /// The hunks, computed for version `computed` and carried across the edits since.
    hunks: Arc<[AgentHunk]>,
    computed: u64,
    /// A large or minified text: diffed again less often.
    heavy: bool,
    /// Bumped at every write of the agent (what *Follow agent* follows).
    epoch: u64,
    /// Where the agent's latest write starts, carried across edits.
    newest: Option<usize>,
}

/// What the git gutter compares the text with and the hunks of that comparison.
struct GitState {
    base: GitBase,
    /// The hunks, computed for version `computed` and carried across the edits since.
    hunks: Arc<[AgentHunk]>,
    computed: u64,
    /// A large or minified text: diffed again less often.
    heavy: bool,
}

/// The text of one file and what is known about it.
pub struct FileBuffer {
    path: PathBuf,
    state: LoadState,
    buffer: Buffer,
    /// Bumped every time the text changes; caches of what was drawn and the parser key on it.
    version: u64,
    /// The last [`EDIT_LOG_LEN`] edits with the version each made (cleared by a load).
    edit_log: VecDeque<(u64, Transaction)>,
    language: Option<Language>,
    indent: IndentUnit,
    /// Large-file mode: no syntax, read-only until the user says otherwise.
    large: bool,
    /// Never editable (see [`FileBuffer::is_read_only`]).
    read_only: bool,
    size: u64,
    syntax_active: bool,
    syntax: Option<Syntax>,
    /// The typing group that is open in the undo history, if any.
    group: Option<OpenGroup>,
    conflict: Option<Conflict>,
    saving: bool,
    /// E-10: an agent's changes, while the owner says there are some.
    agent: Option<AgentState>,
    /// `editor.agent_highlight`: the hunks are computed (and drawn) only while it is on.
    agent_highlight: bool,
    /// The background diff that will bring the hunks up to date.
    agent_task: Option<Task<()>>,
    /// The span of the text the last reload changed (in the text after it).
    last_reload: Option<Range<usize>>,
    /// EDITOR-POLISH: the file in the index (or at `HEAD`) and where the buffer differs from it.
    git: Option<GitState>,
    /// `editor.git_gutter`: the hunks are computed (and drawn) only while it is on.
    git_gutter: bool,
    /// The background diff that will bring the git hunks up to date.
    git_task: Option<Task<()>>,
    /// E-12a: what the language servers found, where it is in the text now (sorted).
    diagnostics: Arc<[Diagnostic]>,
    /// Bumped every time a server publishes a new set.
    diagnostics_epoch: u64,
    /// GIT-HISTORY: which commit last changed each line, while the owner says there is a blame
    /// (`blame.rs`).
    blame: Option<BlameState>,
    /// Bumped whenever the blame is replaced or taken away.
    blame_epoch: u64,
    /// Wakes the views when the parser published a tree.
    published: Arc<Mutex<mpsc::Sender<()>>>,
    _repaint: Task<()>,
}

impl std::fmt::Debug for FileBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBuffer")
            .field("path", &self.path)
            .field("state", &self.state)
            .field("version", &self.version)
            .field("dirty", &self.is_dirty())
            .field("language", &self.language)
            .field("large", &self.large)
            .field("syntax", &self.syntax.is_some())
            .field("conflict", &self.conflict.is_some())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BufferEdited> for FileBuffer {}

impl FileBuffer {
    /// A buffer for `path` (absolute, canonical), still loading.
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        // One pending wake-up is enough: views draw the newest tree whenever they draw.
        let (sender, mut receiver) = mpsc::channel::<()>(0);
        let repaint = cx.spawn(async move |this, cx| {
            while receiver.next().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            language: Language::from_path(&path).filter(|l| l.is_compiled()),
            path,
            state: LoadState::Loading,
            buffer: Buffer::new(),
            version: 0,
            edit_log: VecDeque::new(),
            indent: IndentUnit::default(),
            large: false,
            read_only: false,
            size: 0,
            syntax_active: false,
            syntax: None,
            group: None,
            conflict: None,
            saving: false,
            agent: None,
            agent_highlight: true,
            agent_task: None,
            last_reload: None,
            git: None,
            git_gutter: true,
            git_task: None,
            diagnostics: Arc::from(Vec::new()),
            diagnostics_epoch: 0,
            blame: None,
            blame_epoch: 0,
            published: Arc::new(Mutex::new(sender)),
            _repaint: repaint,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file name, for titles.
    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }

    pub fn state(&self) -> &LoadState {
        &self.state
    }

    pub fn is_ready(&self) -> bool {
        self.state == LoadState::Ready
    }

    /// Whether the text may be changed: it is in, the file is not in large-file mode and the
    /// buffer was not made read-only.
    pub fn is_editable(&self) -> bool {
        self.is_ready() && !self.large && !self.read_only
    }

    /// The buffer shows a file that must not be changed from here (a file inside the running app
    /// bundle): no edits, and it is not offered to language servers.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if self.read_only != read_only {
            self.read_only = read_only;
            cx.notify();
        }
    }

    /// The text (line endings normalized to `\n`, no byte-order mark: what is drawn).
    pub fn text(&self) -> &Text {
        self.buffer.text()
    }

    /// How the file ends its lines on disk (and is written back).
    pub fn line_ending(&self) -> LineEnding {
        self.buffer.line_ending()
    }

    pub fn has_bom(&self) -> bool {
        self.buffer.has_bom()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The edits that took the text from `version` to the current one, as one transaction (in
    /// the coordinates of the text at `version`); `None` when the text was replaced since
    /// (a load) or more than [`EDIT_LOG_LEN`] edits ago. A view that keeps something derived
    /// from the text (the soft wrap's rows) carries it across the edits instead of building
    /// it again.
    pub fn edits_since(&self, version: u64) -> Option<Transaction> {
        if version == self.version {
            return Some(Transaction::empty());
        }
        let first = version + 1;
        let mut next = first;
        let mut composed: Option<Transaction> = None;
        for (at, transaction) in self.edit_log.iter().skip_while(|(at, _)| *at < first) {
            if *at != next {
                return None;
            }
            next += 1;
            composed = match composed {
                None => Some(transaction.clone()),
                Some(before) => Some(before.compose(transaction).ok()?),
            };
        }
        composed.filter(|_| next == self.version + 1)
    }

    pub fn language(&self) -> Option<Language> {
        self.language
    }

    /// What one level of indentation is in this file (detected when it was read).
    pub fn indent_unit(&self) -> IndentUnit {
        self.indent
    }

    /// Large-file mode: opened without syntax.
    pub fn is_large(&self) -> bool {
        self.large
    }

    /// The size of the file on disk, in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether the text differs from what was last read or saved.
    pub fn is_dirty(&self) -> bool {
        self.is_ready() && self.buffer.is_dirty()
    }

    pub fn can_undo(&self) -> bool {
        self.buffer.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.buffer.can_redo()
    }

    /// The conflict with the file on disk, if there is one.
    pub fn conflict(&self) -> Option<&Conflict> {
        self.conflict.as_ref()
    }

    pub fn set_conflict(&mut self, conflict: Option<Conflict>, cx: &mut Context<Self>) {
        if self.conflict != conflict {
            self.conflict = conflict;
            cx.notify();
        }
    }

    /// A save is on its way to the daemon.
    pub fn is_saving(&self) -> bool {
        self.saving
    }

    pub fn set_saving(&mut self, saving: bool, cx: &mut Context<Self>) {
        if self.saving != saving {
            self.saving = saving;
            cx.notify();
        }
    }

    /// Moves to `state` (loading, waiting for a confirmation, failed).
    pub fn set_state(&mut self, state: LoadState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    /// The size the file had when it was looked up.
    pub fn set_size(&mut self, size: u64) {
        self.size = size;
    }

    /// The file's text arrived (exactly as on disk); `large` opens it in large-file mode. The
    /// undo history starts over.
    pub fn set_loaded(&mut self, text: &str, large: bool, cx: &mut Context<Self>) {
        self.buffer = Buffer::from_content(text);
        self.group = None;
        self.conflict = None;
        self.size = self.size.max(text.len() as u64);
        self.version += 1;
        self.edit_log.clear();
        self.large = large;
        self.indent = IndentUnit::detect(self.buffer.text());
        if self.language.is_none() {
            self.language = self
                .buffer
                .text()
                .line_text(0)
                .ok()
                .and_then(|first| Language::from_shebang(&first))
                .filter(|l| l.is_compiled());
        }
        self.state = LoadState::Ready;
        // A tree of the old text is of no use.
        self.syntax = None;
        self.sync_syntax();
        self.last_reload = None;
        // Positions of another text mean nothing; the next publish brings them back.
        if !self.diagnostics.is_empty() {
            self.diagnostics = Arc::from(Vec::new());
            self.diagnostics_epoch += 1;
        }
        if let Some(git) = &mut self.git {
            // Positions of another text mean nothing.
            git.hunks = Arc::from(Vec::new());
            git.computed = 0;
        }
        self.schedule_agent_diff(None, cx);
        self.schedule_git_diff(None, cx);
        cx.notify();
    }

    // ---- editing ---------------------------------------------------------------------------

    /// Applies `transaction` (made by the view `origin`) as one atomic change, and as an undo
    /// step of its own or joined to the typing before it (`coalesce`). `None` when the buffer
    /// is read-only or the transaction does not fit the text.
    pub fn edit(
        &mut self,
        transaction: Transaction,
        coalesce: Coalesce,
        origin: EntityId,
        cx: &mut Context<Self>,
    ) -> Option<Applied> {
        if !self.is_editable() || transaction.is_empty() {
            return None;
        }
        let now = Instant::now();
        match coalesce {
            Coalesce::Typing => {
                let joins = self.group.as_ref().is_some_and(|g| {
                    g.owner == origin && now.saturating_duration_since(g.last) < TYPING_GROUP_PAUSE
                });
                if !joins {
                    self.break_group();
                    self.buffer.begin_group();
                }
                self.group = Some(OpenGroup {
                    owner: origin,
                    last: now,
                });
            }
            Coalesce::Step => self.break_group(),
        }
        let before = self.buffer.snapshot();
        match self.buffer.apply(transaction) {
            Ok(applied) => {
                self.changed(&before, &applied, Some(origin), EditKind::Edit, cx);
                Some(applied)
            }
            Err(error) => {
                tracing::warn!(path = %self.path.display(), "edit refused: {error}");
                None
            }
        }
    }

    /// Ends the typing step that is open: the next edit is an undo step of its own (the caret
    /// moved, a command ran, the file is being saved).
    pub fn break_group(&mut self) {
        if self.group.take().is_some() {
            self.buffer.end_group();
        }
    }

    /// Undoes the last step; the transaction's `selection_after` is the selection to restore.
    pub fn undo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<Applied> {
        if !self.is_editable() {
            return None;
        }
        // `Buffer::undo` closes the open group itself.
        self.group = None;
        let before = self.buffer.snapshot();
        let applied = self.buffer.undo()?;
        self.changed(&before, &applied, Some(origin), EditKind::Undo, cx);
        Some(applied)
    }

    /// Redoes the step undone last.
    pub fn redo(&mut self, origin: EntityId, cx: &mut Context<Self>) -> Option<Applied> {
        if !self.is_editable() {
            return None;
        }
        self.group = None;
        let before = self.buffer.snapshot();
        let applied = self.buffer.redo()?;
        self.changed(&before, &applied, Some(origin), EditKind::Redo, cx);
        Some(applied)
    }

    /// The file changed on disk and the buffer takes its text (`text` exactly as on disk): the
    /// minimal difference is applied as one undo step, so selections in unchanged regions stay
    /// and undo steps back over it; the buffer is clean afterwards. Returns whether the text
    /// changed; [`last_reload`](Self::last_reload) says where.
    pub fn reload(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.is_ready() {
            return false;
        }
        self.break_group();
        let before = self.buffer.snapshot();
        let changed = match self.buffer.apply_external(text) {
            Ok(Some(applied)) => {
                let changes = applied.transaction.changes();
                self.last_reload = changes
                    .first()
                    .zip(changes.last())
                    .map(|(first, last)| first.new.start..last.new.end);
                self.changed(&before, &applied, None, EditKind::Reload, cx);
                true
            }
            Ok(None) => false,
            Err(error) => {
                tracing::warn!(path = %self.path.display(), "reload refused: {error}");
                return false;
            }
        };
        self.buffer.mark_saved();
        self.size = text.len() as u64;
        self.conflict = None;
        cx.notify();
        changed
    }

    /// The span of the text (as it is after it) that the last reload changed.
    pub fn last_reload(&self) -> Option<Range<usize>> {
        self.last_reload.clone()
    }

    /// The bytes to write (byte-order mark and line endings as the file had them) and the
    /// version they are of. Ends the open typing step, so that what is typed while the save is
    /// on its way is a step after the saved state.
    pub fn bytes_to_save(&mut self) -> (u64, String) {
        self.break_group();
        let bytes = self.buffer.to_bytes();
        // The text is UTF-8 and the BOM and line breaks it adds are too.
        let text = String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        (self.version, text)
    }

    /// The bytes of `version` reached the disk (`size` of them): the buffer is clean again,
    /// unless it was edited meanwhile.
    pub fn saved(&mut self, version: u64, size: u64, cx: &mut Context<Self>) {
        if version == self.version {
            self.break_group();
            self.buffer.mark_saved();
        }
        self.size = size;
        self.conflict = None;
        self.saving = false;
        cx.notify();
    }

    // ---- Vim -------------------------------------------------------------------------------

    /// Lends the text model to a Vim host for one key (E-8): the typing step that is open ends
    /// first, so the key's change is a step of its own. Hand it back with
    /// [`restore`](Self::restore) before anything else reads the buffer.
    pub(crate) fn lend(&mut self) -> Buffer {
        self.break_group();
        std::mem::take(&mut self.buffer)
    }

    /// Takes the text model back from a Vim host, with what the key did (`steps`, in order,
    /// made by the view `origin`): the parser and the other views hear of each step as of any
    /// edit.
    pub(crate) fn restore(
        &mut self,
        buffer: Buffer,
        steps: Vec<crate::vim_host::HostStep>,
        origin: EntityId,
        cx: &mut Context<Self>,
    ) {
        self.buffer = buffer;
        if steps.is_empty() {
            return;
        }
        let mut reset = false;
        for step in steps {
            reset |= !self.announce(&step.before, &step.applied, Some(origin), step.kind, cx);
        }
        self.reparse(reset);
        self.schedule_agent_diff(Some(agent::DIFF_DELAY), cx);
        self.schedule_git_diff(Some(agent::DIFF_DELAY), cx);
        cx.notify();
    }

    fn changed(
        &mut self,
        before: &Text,
        applied: &Applied,
        origin: Option<EntityId>,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) {
        let fed = self.announce(before, applied, origin, kind, cx);
        self.reparse(!fed);
        // A change from the disk is diffed at once; typing waits for a pause.
        let delay = (kind != EditKind::Reload).then_some(agent::DIFF_DELAY);
        self.schedule_agent_diff(delay, cx);
        self.schedule_git_diff(delay, cx);
        cx.notify();
    }

    /// A new version: the parser gets the edits of `applied` (no parse yet) and the views hear
    /// of it. `false` when the edits could not be fed (the next parse starts over).
    fn announce(
        &mut self,
        before: &Text,
        applied: &Applied,
        origin: Option<EntityId>,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) -> bool {
        self.version += 1;
        if self.edit_log.len() >= EDIT_LOG_LEN {
            self.edit_log.pop_front();
        }
        self.edit_log
            .push_back((self.version, applied.transaction.clone()));
        self.remap_blame();
        self.map_agent_hunks(&applied.transaction);
        self.map_git_hunks(&applied.transaction);
        if !self.diagnostics.is_empty() {
            self.diagnostics = Arc::from(diagnostics::map(&self.diagnostics, &applied.transaction));
        }
        let mut fed = true;
        if let Some(syntax) = &self.syntax {
            match hutan_syntax::edits_of_transaction(before, &applied.transaction) {
                Ok(edits) => {
                    for edit in edits {
                        syntax.edit(self.version, edit);
                    }
                }
                // Cannot happen for a transaction that applied to `before`; a fresh parse is
                // the safe answer.
                Err(_) => fed = false,
            }
        }
        cx.emit(BufferEdited {
            transaction: applied.transaction.clone(),
            origin,
            kind,
            version: self.version,
        });
        fed
    }

    /// Asks the parser for a tree of the current text: incrementally, or from scratch (`reset`).
    fn reparse(&self, reset: bool) {
        if let Some(syntax) = &self.syntax {
            if reset {
                syntax.reset(self.version, self.buffer.snapshot());
            } else {
                syntax.reparse(self.version, self.buffer.snapshot());
            }
        }
    }

    // ---- blame (GIT-HISTORY) -----------------------------------------------------------------

    /// Gives the buffer the blame of a text (or takes it away): every line of the current text
    /// is matched to a line of it. A text over [`MAX_BLAME_BYTES`] is not blamed.
    pub fn set_blame(&mut self, data: Option<BlameData>, cx: &mut Context<Self>) {
        self.blame = match data {
            Some(data) if self.buffer.snapshot().len() <= MAX_BLAME_BYTES => {
                let current = self.current_text();
                Some(BlameState::new(data, &current))
            }
            _ => None,
        };
        self.blame_epoch += 1;
        cx.notify();
    }

    /// Whether the buffer holds a blame.
    pub fn has_blame(&self) -> bool {
        self.blame.is_some()
    }

    /// Bumped whenever the blame is replaced or taken away.
    pub fn blame_epoch(&self) -> u64 {
        self.blame_epoch
    }

    /// What line `row` of the current text is blamed on; `None` without a blame or past the
    /// last line of text.
    pub fn blame_line(&self, row: usize) -> Option<BlameLine<'_>> {
        self.blame.as_ref()?.line(row)
    }

    /// Width of the gutter's blame column in cells, from the commits the blame names.
    pub fn blame_cells(&self) -> usize {
        self.blame
            .as_ref()
            .map_or(crate::blame::COLUMN_MIN_CELLS, BlameState::cells)
    }

    /// How many lines the blame covers now.
    pub fn blame_lines(&self) -> usize {
        self.blame.as_ref().map_or(0, BlameState::lines)
    }

    fn current_text(&self) -> String {
        let text = self.buffer.snapshot();
        text.slice(0..text.len()).unwrap_or_default()
    }

    /// The text changed: its lines are matched to the blamed ones again.
    fn remap_blame(&mut self) {
        if self.blame.is_none() {
            return;
        }
        if self.buffer.snapshot().len() > MAX_BLAME_BYTES {
            self.blame = None;
            self.blame_epoch += 1;
            return;
        }
        let current = self.current_text();
        if let Some(blame) = &mut self.blame {
            blame.remap(&current);
        }
    }

    // ---- an agent's changes (E-10) -----------------------------------------------------------

    /// Gives the buffer an agent's changes to the file (or takes them away): the hunks are
    /// computed in the background, at once.
    pub fn set_agent_changes(&mut self, changes: Option<AgentChanges>, cx: &mut Context<Self>) {
        match (changes, &mut self.agent) {
            (None, _) => {
                self.agent = None;
                self.agent_task = None;
            }
            (Some(changes), Some(state)) => {
                if state.changes == changes {
                    return;
                }
                state.changes = changes;
            }
            (Some(changes), None) => {
                self.agent = Some(AgentState {
                    changes,
                    hunks: Arc::from(Vec::new()),
                    computed: 0,
                    heavy: false,
                    epoch: 0,
                    newest: None,
                });
            }
        }
        self.schedule_agent_diff(None, cx);
        cx.notify();
    }

    /// The agent wrote the file again and the buffer took its text: `changes` are the turn's
    /// changes now, and the latest write starts at byte `at` (what *Follow agent* scrolls to).
    pub fn agent_wrote(
        &mut self,
        changes: AgentChanges,
        at: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        self.set_agent_changes(Some(changes), cx);
        if let Some(state) = &mut self.agent {
            state.epoch += 1;
            state.newest = at.map(|at| at.min(self.buffer.text().len()));
        }
        cx.notify();
    }

    /// The agent's changes the buffer was given, if any.
    pub fn agent_changes(&self) -> Option<&AgentChanges> {
        self.agent.as_ref().map(|s| &s.changes)
    }

    /// The agent's hunks in the current text (none while `editor.agent_highlight` is off).
    /// Between an edit and the next diff they are carried across the edit.
    pub fn agent_hunks(&self) -> Arc<[AgentHunk]> {
        match &self.agent {
            Some(state) if self.agent_highlight => state.hunks.clone(),
            _ => Arc::from(Vec::new()),
        }
    }

    /// Whether the hunks are those of the current text (tests wait on it).
    pub fn agent_hunks_current(&self) -> bool {
        self.agent_task.is_none()
            && self
                .agent
                .as_ref()
                .is_none_or(|s| !self.agent_highlight || s.computed == self.version)
    }

    /// Bumped at every write of the agent ([`agent_wrote`](Self::agent_wrote)).
    pub fn agent_epoch(&self) -> u64 {
        self.agent.as_ref().map_or(0, |s| s.epoch)
    }

    /// The hunk the agent's latest write is in (or the first one after it, else the last).
    pub fn newest_agent_hunk(&self) -> Option<usize> {
        let hunks = self.agent_hunks();
        if hunks.is_empty() {
            return None;
        }
        let at = self.agent.as_ref()?.newest?;
        hunks
            .iter()
            .position(|h| h.bytes.end >= at)
            .or(Some(hunks.len() - 1))
    }

    /// The hunk at byte `offset` (a removed hunk counts at its position).
    pub fn agent_hunk_at(&self, offset: usize) -> Option<usize> {
        self.agent_hunks().iter().position(|h| {
            h.bytes.contains(&offset) || h.bytes.start == offset || h.bytes.end == offset
        })
    }

    /// `editor.agent_highlight`: off, no hunk is computed or drawn.
    pub fn set_agent_highlight(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.agent_highlight == on {
            return;
        }
        self.agent_highlight = on;
        if on {
            self.schedule_agent_diff(None, cx);
        } else {
            self.agent_task = None;
        }
        cx.notify();
    }

    /// *Reject*: puts the base's text back in place of hunk `index`, as one undo step made by
    /// the view `origin` (`selections`: that view's carets, restored by undo). Never deletes
    /// the file: a file the agent created rejects to empty.
    pub fn reject_agent_hunk(
        &mut self,
        index: usize,
        origin: EntityId,
        selections: Option<Selections>,
        cx: &mut Context<Self>,
    ) -> Option<Applied> {
        let hunk = self.agent_hunks().get(index)?.clone();
        if !self.agent_hunks_current() {
            // Hunks carried across an edit are approximate; Reject waits for exact ones.
            return None;
        }
        let after = Selections::cursor(hunk.bytes.start);
        let transaction = Transaction::replace(hunk.bytes.clone(), hunk.base_text.to_string())
            .ok()?
            .with_selections(selections, Some(after));
        let applied = self.edit(transaction, Coalesce::Step, origin, cx)?;
        self.schedule_agent_diff(None, cx);
        Some(applied)
    }

    /// The hunks follow an edit until the next diff replaces them.
    fn map_agent_hunks(&mut self, transaction: &Transaction) {
        let Some(state) = &mut self.agent else {
            return;
        };
        state.hunks = map_hunks(&state.hunks, transaction);
        state.newest = state
            .newest
            .map(|at| transaction.map_offset(at, Bias::Left));
    }

    /// Computes the hunks again in the background, after `delay` (a longer one for a large
    /// or minified text) or at once. A newer call replaces an older one.
    fn schedule_agent_diff(&mut self, delay: Option<Duration>, cx: &mut Context<Self>) {
        let Some(state) = &self.agent else {
            return;
        };
        if !self.agent_highlight || self.large || !self.is_ready() {
            self.agent_task = None;
            return;
        }
        let delay = delay.map(|d| {
            if state.heavy {
                agent::SLOW_DIFF_DELAY
            } else {
                d
            }
        });
        let base = state.changes.base.clone();
        let agent_text = state.changes.agent.clone();
        self.agent_task = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            let Ok((text, version)) = this.update(cx, |b, _| (b.buffer.snapshot(), b.version))
            else {
                return;
            };
            let computed = cx
                .background_spawn(async move {
                    let current = text.to_string();
                    let heavy = agent::is_heavy(&current);
                    (agent::agent_hunks(&base, &agent_text, &current), heavy)
                })
                .await;
            this.update(cx, |b, cx| b.take_agent_hunks(version, computed, cx))
                .ok();
        }));
    }

    fn take_agent_hunks(
        &mut self,
        version: u64,
        (hunks, heavy): (Vec<AgentHunk>, bool),
        cx: &mut Context<Self>,
    ) {
        self.agent_task = None;
        let Some(state) = &mut self.agent else {
            return;
        };
        if version != self.version {
            // Edited meanwhile; the edit asked for another diff.
            return;
        }
        state.hunks = Arc::from(hunks);
        state.computed = version;
        state.heavy = heavy;
        cx.notify();
    }

    // ---- git change marks (EDITOR-POLISH) ------------------------------------------------------

    /// Gives the buffer the file as the index holds it (or `HEAD`), or takes it away (a file
    /// git does not know): the hunks are computed in the background, at once.
    pub fn set_git_base(&mut self, base: Option<GitBase>, cx: &mut Context<Self>) {
        match (base, &mut self.git) {
            (None, None) => return,
            (None, Some(_)) => {
                self.git = None;
                self.git_task = None;
            }
            (Some(base), Some(state)) => {
                if state.base == base {
                    return;
                }
                state.base = base;
            }
            (Some(base), None) => {
                self.git = Some(GitState {
                    base,
                    hunks: Arc::from(Vec::new()),
                    computed: 0,
                    heavy: false,
                });
            }
        }
        self.schedule_git_diff(None, cx);
        cx.notify();
    }

    /// What the marks are drawn against, when the buffer has a base.
    pub fn git_base(&self) -> Option<&GitBase> {
        self.git.as_ref().map(|s| &s.base)
    }

    /// The buffer's changes against the git base, in the current text (none while
    /// `editor.git_gutter` is off). Between an edit and the next diff they are carried across
    /// the edit.
    pub fn git_hunks(&self) -> Arc<[AgentHunk]> {
        match &self.git {
            Some(state) if self.git_gutter => state.hunks.clone(),
            _ => Arc::from(Vec::new()),
        }
    }

    /// Whether the git hunks are those of the current text (tests wait on it).
    pub fn git_hunks_current(&self) -> bool {
        self.git_task.is_none()
            && self
                .git
                .as_ref()
                .is_none_or(|s| !self.git_gutter || s.computed == self.version)
    }

    /// The git hunk whose lines include line `line` (0-based); a removed hunk counts at the line
    /// its mark is drawn above.
    pub fn git_hunk_at_line(&self, line: usize) -> Option<usize> {
        let text = self.buffer.text();
        self.git_hunks().iter().position(|h| {
            let start = h.bytes.start.min(text.len());
            let Ok(first) = text.line_of_byte(text.floor_char_boundary(start)) else {
                return false;
            };
            if h.bytes.is_empty() {
                return first == line;
            }
            let end = h.bytes.end.min(text.len()).saturating_sub(1).max(start);
            let last = text
                .line_of_byte(text.floor_char_boundary(end))
                .unwrap_or(first);
            (first..=last).contains(&line)
        })
    }

    /// `editor.git_gutter`: off, no hunk is computed or drawn.
    pub fn set_git_gutter(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.git_gutter == on {
            return;
        }
        self.git_gutter = on;
        if on {
            self.schedule_git_diff(None, cx);
        } else {
            self.git_task = None;
        }
        cx.notify();
    }

    /// *Revert hunk*: puts the base's text back in place of git hunk `index`, as one undo step
    /// made by the view `origin` (`selections`: that view's carets, restored by undo).
    pub fn revert_git_hunk(
        &mut self,
        index: usize,
        origin: EntityId,
        selections: Option<Selections>,
        cx: &mut Context<Self>,
    ) -> Option<Applied> {
        let hunk = self.git_hunks().get(index)?.clone();
        if !self.git_hunks_current() {
            // Hunks carried across an edit are approximate; Revert waits for exact ones.
            return None;
        }
        let after = Selections::cursor(hunk.bytes.start);
        let transaction = Transaction::replace(hunk.bytes.clone(), hunk.base_text.to_string())
            .ok()?
            .with_selections(selections, Some(after));
        let applied = self.edit(transaction, Coalesce::Step, origin, cx)?;
        self.schedule_git_diff(None, cx);
        Some(applied)
    }

    /// The git hunks follow an edit until the next diff replaces them.
    fn map_git_hunks(&mut self, transaction: &Transaction) {
        let Some(state) = &mut self.git else {
            return;
        };
        state.hunks = map_hunks(&state.hunks, transaction);
    }

    /// Computes the git hunks again in the background, after `delay` (a longer one for a large
    /// or minified text) or at once. A newer call replaces an older one.
    fn schedule_git_diff(&mut self, delay: Option<Duration>, cx: &mut Context<Self>) {
        let Some(state) = &self.git else {
            return;
        };
        if !self.git_gutter || self.large || !self.is_ready() {
            self.git_task = None;
            return;
        }
        let delay = delay.map(|d| {
            if state.heavy {
                agent::SLOW_DIFF_DELAY
            } else {
                d
            }
        });
        let base = state.base.text.clone();
        self.git_task = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            let Ok((text, version)) = this.update(cx, |b, _| (b.buffer.snapshot(), b.version))
            else {
                return;
            };
            let computed = cx
                .background_spawn(async move {
                    let current = text.to_string();
                    let heavy = agent::is_heavy(&current);
                    (agent::hunks_between(&base, &current), heavy)
                })
                .await;
            this.update(cx, |b, cx| b.take_git_hunks(version, computed, cx))
                .ok();
        }));
    }

    fn take_git_hunks(
        &mut self,
        version: u64,
        (hunks, heavy): (Vec<AgentHunk>, bool),
        cx: &mut Context<Self>,
    ) {
        if version != self.version {
            // Edited meanwhile; the edit asked for another diff (which replaced this task).
            return;
        }
        self.git_task = None;
        let Some(state) = &mut self.git else {
            return;
        };
        state.hunks = Arc::from(hunks);
        state.computed = version;
        state.heavy = heavy;
        cx.notify();
    }

    // ---- diagnostics (E-12a) ---------------------------------------------------------------

    /// A server published the file's diagnostics: `inputs` replace the set (an empty list
    /// clears it). Positions are placed in the current text; the views draw them at their next
    /// frame.
    pub fn set_diagnostics(&mut self, inputs: Vec<DiagnosticInput>, cx: &mut Context<Self>) {
        if inputs.is_empty() && self.diagnostics.is_empty() {
            return;
        }
        self.diagnostics = Arc::from(diagnostics::place(self.buffer.text(), inputs));
        self.diagnostics_epoch += 1;
        cx.notify();
    }

    /// The diagnostics in the current text, sorted by where they start. Between two publishes
    /// they follow the edits.
    pub fn diagnostics(&self) -> Arc<[Diagnostic]> {
        self.diagnostics.clone()
    }

    /// Bumped at every publish (tests, the B16b budget).
    pub fn diagnostics_epoch(&self) -> u64 {
        self.diagnostics_epoch
    }

    pub fn diagnostic_counts(&self) -> DiagnosticCounts {
        DiagnosticCounts::of(&self.diagnostics)
    }

    /// The diagnostics about byte `offset`, the most serious first.
    pub fn diagnostics_at(&self, offset: usize) -> Vec<Diagnostic> {
        let mut found: Vec<Diagnostic> = self
            .diagnostics
            .iter()
            .filter(|d| d.touches(offset))
            .cloned()
            .collect();
        found.sort_by_key(|d| d.severity);
        found
    }

    // ---- syntax ----------------------------------------------------------------------------

    /// Whether some view of the buffer is on screen: the syntax is kept (and parsed) only then.
    pub fn syntax_active(&self) -> bool {
        self.syntax_active
    }

    /// Whether some view of the buffer is on screen: the syntax is kept (and parsed) only then.
    pub fn set_syntax_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.syntax_active == active {
            return;
        }
        self.syntax_active = active;
        self.sync_syntax();
        cx.notify();
    }

    fn sync_syntax(&mut self) {
        let wanted = self.syntax_active && !self.large && self.is_ready();
        if !wanted {
            self.syntax = None;
            return;
        }
        if self.syntax.is_some() {
            return;
        }
        let Some(language) = self.language else {
            return;
        };
        let published = Arc::clone(&self.published);
        let started = Syntax::with_notify(language, move |_| {
            // Full means a wake-up is already pending.
            let _ = published
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_send(());
        });
        match started {
            Ok(syntax) => {
                syntax.reparse(self.version, self.buffer.snapshot());
                self.syntax = Some(syntax);
            }
            // The view draws without colors.
            Err(error) => tracing::warn!(path = %self.path.display(), "no syntax: {error}"),
        }
    }

    /// What the parser can tell right now (never waits); `None` without syntax or before the
    /// first tree.
    pub fn syntax_view(&self) -> Option<SyntaxView> {
        self.syntax.as_ref()?.view()
    }

    /// Whether the buffer holds a syntax handle now.
    pub fn has_syntax(&self) -> bool {
        self.syntax.is_some()
    }

    /// The parser's counters (tests, diagnostics).
    pub fn syntax_stats(&self) -> Option<SyntaxStats> {
        self.syntax.as_ref().map(Syntax::stats)
    }

    /// The newest finished tree (tests: to watch it being freed).
    pub fn syntax_snapshot(&self) -> Option<Arc<hutan_syntax::Snapshot>> {
        self.syntax.as_ref()?.snapshot()
    }

    /// Blocks until the tree of the current text is published or `timeout` passes (tests and
    /// benchmarks; the UI never waits for the parser).
    pub fn wait_for_syntax(&self, timeout: std::time::Duration) -> bool {
        self.syntax
            .as_ref()
            .is_some_and(|s| s.wait_for(self.version, timeout).is_some())
    }
}

/// `hunks` carried across `transaction`: each start and end moves with the text around it.
fn map_hunks(hunks: &[AgentHunk], transaction: &Transaction) -> Arc<[AgentHunk]> {
    hunks
        .iter()
        .map(|h| {
            let start = transaction.map_offset(h.bytes.start, Bias::Right);
            let end = transaction.map_offset(h.bytes.end, Bias::Left).max(start);
            AgentHunk {
                bytes: start..end,
                base_text: h.base_text.clone(),
            }
        })
        .collect()
}

/// The last component of `path`, or the whole path when it has none.
pub fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The language name shown for `language` (`Rust`, `Plain Text`).
pub fn language_label(language: Option<Language>) -> &'static str {
    match language {
        None => "Plain Text",
        Some(language) => match language {
            Language::Rust => "Rust",
            Language::TypeScript => "TypeScript",
            Language::Tsx => "TSX",
            Language::JavaScript => "JavaScript",
            Language::Python => "Python",
            Language::Go => "Go",
            Language::Json => "JSON",
            Language::Toml => "TOML",
            Language::Yaml => "YAML",
            Language::Markdown => "Markdown",
            Language::Bash => "Shell",
            Language::Html => "HTML",
            Language::Css => "CSS",
            Language::C => "C",
            Language::Cpp => "C++",
        },
    }
}
