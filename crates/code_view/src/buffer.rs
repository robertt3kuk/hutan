//! [`FileBuffer`]: the text of one open file, shared by every view of it, with its load state and
//! — while some view of it is on screen — its syntax.
//!
//! The owner (the desktop's buffer store) reads the file through the daemon and hands the text
//! over with [`FileBuffer::set_loaded`]; views only read. A tree-sitter tree costs about 37 bytes
//! per byte of source (`docs/perf.md` § Editor: syntax), so the buffer keeps a [`Syntax`] only
//! while [`FileBuffer::set_syntax_active`] says a view of it is visible, and never for a file in
//! large-file mode; the rope stays either way and the tree is parsed again on show.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Context, Task};
use hutan_buffer::{Buffer, LineEnding, Text};
use hutan_syntax::{Language, Syntax, SyntaxStats, SyntaxView};

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

/// The text of one file and what is known about it.
pub struct FileBuffer {
    path: PathBuf,
    state: LoadState,
    text: Text,
    line_ending: LineEnding,
    bom: bool,
    /// Bumped every time the text is replaced; caches of what was drawn key on it.
    version: u64,
    language: Option<Language>,
    /// Large-file mode: no syntax, read-only until the user says otherwise.
    large: bool,
    size: u64,
    syntax_active: bool,
    syntax: Option<Syntax>,
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
            .field("language", &self.language)
            .field("large", &self.large)
            .field("syntax", &self.syntax.is_some())
            .finish_non_exhaustive()
    }
}

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
            text: Text::new(),
            line_ending: LineEnding::default(),
            bom: false,
            version: 0,
            large: false,
            size: 0,
            syntax_active: false,
            syntax: None,
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

    /// The text (line endings normalized to `\n`, no byte-order mark: what is drawn).
    pub fn text(&self) -> &Text {
        &self.text
    }

    /// How the file ends its lines on disk.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    pub fn has_bom(&self) -> bool {
        self.bom
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn language(&self) -> Option<Language> {
        self.language
    }

    /// Large-file mode: opened without syntax.
    pub fn is_large(&self) -> bool {
        self.large
    }

    /// The size of the file on disk, in bytes.
    pub fn size(&self) -> u64 {
        self.size
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

    /// The file's text arrived (exactly as on disk); `large` opens it in large-file mode.
    pub fn set_loaded(&mut self, text: &str, large: bool, cx: &mut Context<Self>) {
        let buffer = Buffer::from_content(text);
        self.text = buffer.snapshot();
        self.line_ending = buffer.line_ending();
        self.bom = buffer.has_bom();
        self.size = self.size.max(text.len() as u64);
        self.version += 1;
        self.large = large;
        if self.language.is_none() {
            self.language = self
                .text
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
                syntax.reparse(self.version, self.text.clone());
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
