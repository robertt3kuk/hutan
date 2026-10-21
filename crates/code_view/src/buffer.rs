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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Context, EntityId, EventEmitter, Task};
use hutan_buffer::{Applied, Buffer, LineEnding, Text, Transaction};
use hutan_syntax::{Language, Syntax, SyntaxStats, SyntaxView};

use crate::rules::IndentUnit;

/// Typing that pauses longer than this starts a new undo step.
pub const TYPING_GROUP_PAUSE: Duration = Duration::from_secs(1);

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
    /// *Show diff* and *Reload*).
    Changed { disk: Option<Arc<str>> },
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

/// The text of one file and what is known about it.
pub struct FileBuffer {
    path: PathBuf,
    state: LoadState,
    buffer: Buffer,
    /// Bumped every time the text changes; caches of what was drawn and the parser key on it.
    version: u64,
    language: Option<Language>,
    indent: IndentUnit,
    /// Large-file mode: no syntax, read-only until the user says otherwise.
    large: bool,
    size: u64,
    syntax_active: bool,
    syntax: Option<Syntax>,
    /// The typing group that is open in the undo history, if any.
    group: Option<OpenGroup>,
    conflict: Option<Conflict>,
    saving: bool,
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
            indent: IndentUnit::default(),
            large: false,
            size: 0,
            syntax_active: false,
            syntax: None,
            group: None,
            conflict: None,
            saving: false,
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

    /// Whether the text may be changed: it is in, and the file is not in large-file mode.
    pub fn is_editable(&self) -> bool {
        self.is_ready() && !self.large
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
    /// changed.
    pub fn reload(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if !self.is_ready() {
            return false;
        }
        self.break_group();
        let before = self.buffer.snapshot();
        let changed = match self.buffer.apply_external(text) {
            Ok(Some(applied)) => {
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

    // ---- syntax ----------------------------------------------------------------------------

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
