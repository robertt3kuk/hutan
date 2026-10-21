//! The asynchronous side: a [`Syntax`] owns a worker thread that parses the latest text version
//! while the caller keeps reading the last finished tree.

use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::config::config;
use crate::error::SyntaxError;
use crate::language::Language;
use crate::map::{Side, map_backward, map_forward, map_spans};
use crate::parse::Parsers;
use crate::snapshot::{HighlightSpan, Snapshot};
use crate::text::{TextEdit, TextProvider};

/// Edits kept while no parse has finished. Past it the log is dropped and the next parse starts
/// from scratch (a parse that never finishes while thousands of edits pile up is not worth
/// unbounded memory); stale highlights disappear until it lands.
pub const MAX_LOGGED_EDITS: usize = 4096;

/// Counters of the worker, for tests, benches and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyntaxStats {
    /// Parses the worker started.
    pub started: u64,
    /// Of those, the parses that started from an earlier tree edited to the new text (the rest
    /// parsed from scratch).
    pub incremental: u64,
    /// Parses that finished and were published.
    pub completed: u64,
    /// Parses stopped because a newer version arrived (or the handle was dropped).
    pub cancelled: u64,
    /// Requests replaced by a newer one before the worker took them.
    pub superseded: u64,
    /// Parses that ended in an error other than cancellation.
    pub failed: u64,
    /// How long the last published parse took.
    pub last_parse: Duration,
}

struct Logged {
    version: u64,
    edit: TextEdit,
}

struct Request {
    version: u64,
    text: Arc<dyn TextProvider>,
}

struct InFlight {
    version: u64,
    cancel: Arc<AtomicBool>,
}

struct State {
    published: Option<Arc<Snapshot>>,
    /// Edits with a version after `published`'s, in order.
    log: Vec<Logged>,
    /// Edits of versions up to this one were dropped from the log; a snapshot older than it cannot
    /// seed an incremental parse or be mapped forward.
    lost_through: u64,
    pending: Option<Request>,
    in_flight: Option<InFlight>,
    /// The highest version any `edit` or `reparse` mentioned.
    latest: u64,
    /// The highest version passed to `reparse`.
    requested: Option<u64>,
    /// Why the worker could not load the language (a grammar the runtime rejects, a query that
    /// does not compile); nothing is parsed then.
    load_error: Option<SyntaxError>,
    stats: SyntaxStats,
}

struct Shared {
    state: Mutex<State>,
    work: Condvar,
    published: Condvar,
    shutdown: AtomicBool,
    notify: Option<Box<dyn Fn(u64) + Send + Sync>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // The state is only mutated in short, non-panicking sections; a poisoned lock still holds
        // consistent data.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Incremental syntax of one open text.
///
/// Feed it what happens to the text and read what it knows:
/// 1. [`Syntax::reparse`] with version 0 and the loaded text.
/// 2. For every change of the text: [`Syntax::edit`] once per edit (in order, each in the
///    coordinates after the previous), tagged with the buffer version it produced, then
///    [`Syntax::reparse`] with the new text snapshot (several edits may share one reparse).
/// 3. Draw with [`Syntax::view`]: the newest finished tree, with highlights carried across the
///    edits made since, never waiting for the parser.
///
/// A newer `reparse` cancels the parse in flight and replaces a request the worker has not taken
/// yet. The worker thread starts with the handle and stops when it is dropped (without waiting
/// for it).
///
/// Every cancelled parse is thrown away, so while versions arrive faster than a parse takes, no
/// tree is published. With incremental reparsing (about 2 ms for a keystroke in a 1 MB file) that
/// only happens to the *first* parse of a very large file, which has no earlier tree to start from.
///
/// The text provider must not panic (see [`TextProvider`]).
pub struct Syntax {
    language: Language,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl Syntax {
    /// Starts the worker for `language`; fails when the grammar is not compiled in. The worker
    /// loads the grammar and compiles its queries (tens of milliseconds the first time a language
    /// is used in the process) off the calling thread; a failure there is
    /// [`load_error`](Self::load_error).
    pub fn new(language: Language) -> Result<Syntax, SyntaxError> {
        Self::start(language, None)
    }

    /// Like [`Syntax::new`], calling `on_published(version)` from the worker thread after every
    /// published tree (to schedule a repaint). It must return quickly and must not call back into
    /// this `Syntax`'s blocking methods.
    pub fn with_notify(
        language: Language,
        on_published: impl Fn(u64) + Send + Sync + 'static,
    ) -> Result<Syntax, SyntaxError> {
        Self::start(language, Some(Box::new(on_published)))
    }

    fn start(
        language: Language,
        notify: Option<Box<dyn Fn(u64) + Send + Sync>>,
    ) -> Result<Syntax, SyntaxError> {
        if !language.is_compiled() {
            return Err(SyntaxError::NotCompiled(language));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                published: None,
                log: Vec::new(),
                lost_through: 0,
                pending: None,
                in_flight: None,
                latest: 0,
                requested: None,
                load_error: None,
                stats: SyntaxStats::default(),
            }),
            work: Condvar::new(),
            published: Condvar::new(),
            shutdown: AtomicBool::new(false),
            notify,
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(format!("hutan-syntax-{}", language.name()))
            .spawn(move || run_worker(&worker, language))
            .map_err(|e| SyntaxError::Thread(e.to_string()))?;
        Ok(Syntax { language, shared })
    }

    /// The language this handle parses.
    pub fn language(&self) -> Language {
        self.language
    }

    /// Records that the text changed by `edit`, producing buffer version `version`. Call it for
    /// every edit, in order, before the matching [`reparse`](Self::reparse). Edits at or below
    /// the newest finished version are ignored. Cheap: no parsing happens here.
    pub fn edit(&self, version: u64, edit: TextEdit) {
        let mut state = self.shared.lock();
        state.latest = state.latest.max(version);
        if state
            .published
            .as_ref()
            .is_some_and(|p| p.version() >= version)
        {
            return;
        }
        if state.log.len() >= MAX_LOGGED_EDITS {
            let dropped = state.log.last().map_or(version, |l| l.version);
            state.log.clear();
            state.lost_through = state.lost_through.max(dropped);
        }
        state.log.push(Logged { version, edit });
    }

    /// Asks for a tree of `text`, the document at `version`: replaces a request not yet started
    /// and cancels a parse in flight for an older version. Returns whether the request was taken
    /// (`false` for a version not newer than the last request or than the finished tree).
    pub fn reparse(&self, version: u64, text: impl TextProvider + 'static) -> bool {
        self.reparse_shared(version, Arc::new(text))
    }

    /// [`reparse`](Self::reparse) for a snapshot that is already shared.
    pub fn reparse_shared(&self, version: u64, text: Arc<dyn TextProvider>) -> bool {
        let mut state = self.shared.lock();
        if state.load_error.is_some()
            || state.requested.is_some_and(|r| r >= version)
            || state
                .published
                .as_ref()
                .is_some_and(|p| p.version() >= version)
        {
            return false;
        }
        state.requested = Some(version);
        state.latest = state.latest.max(version);
        if state.pending.replace(Request { version, text }).is_some() {
            state.stats.superseded += 1;
        }
        if let Some(flight) = &state.in_flight
            && flight.version < version
        {
            flight.cancel.store(true, Ordering::Release);
        }
        drop(state);
        self.shared.work.notify_one();
        true
    }

    /// Throws the finished tree away (the document was replaced wholesale, not edited) and asks
    /// for a fresh parse of `text` at `version`, which must be newer than everything so far.
    pub fn reset(&self, version: u64, text: impl TextProvider + 'static) -> bool {
        {
            let mut state = self.shared.lock();
            state.log.clear();
            state.lost_through = state.lost_through.max(version);
        }
        self.reparse(version, text)
    }

    /// What can be drawn right now: the newest finished tree and the edits made since. Never
    /// waits for the parser. `None` before the first parse finishes.
    pub fn view(&self) -> Option<SyntaxView> {
        let state = self.shared.lock();
        let snapshot = Arc::clone(state.published.as_ref()?);
        let mappable = state.lost_through <= snapshot.version();
        let edits = if mappable {
            state.log.iter().map(|l| l.edit).collect()
        } else {
            Vec::new()
        };
        Some(SyntaxView {
            current: snapshot.version() >= state.latest,
            mappable,
            edits,
            snapshot,
        })
    }

    /// The newest finished tree, whatever has been edited since.
    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.shared.lock().published.clone()
    }

    /// [`SyntaxView::highlights`] of the current view; empty before the first parse.
    pub fn highlights(&self, range: Range<usize>) -> Vec<HighlightSpan> {
        self.view().map(|v| v.highlights(range)).unwrap_or_default()
    }

    /// Blocks until a tree of at least `version` has been published or `timeout` passes. For
    /// tests, benches and "format on save" style callers; the UI never needs it.
    pub fn wait_for(&self, version: u64, timeout: Duration) -> Option<Arc<Snapshot>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(snapshot) = &state.published
                && snapshot.version() >= version
            {
                return Some(Arc::clone(snapshot));
            }
            if state.load_error.is_some() {
                return None;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self
                .shared
                .published
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Why the language could not be loaded by the worker, if it could not. `None` while it loads
    /// and once it works.
    pub fn load_error(&self) -> Option<SyntaxError> {
        self.shared.lock().load_error.clone()
    }

    /// Worker counters.
    pub fn stats(&self) -> SyntaxStats {
        self.shared.lock().stats
    }
}

impl Drop for Syntax {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        {
            let state = self.shared.lock();
            if let Some(flight) = &state.in_flight {
                flight.cancel.store(true, Ordering::Release);
            }
        }
        self.shared.work.notify_all();
    }
}

/// The newest finished tree of a [`Syntax`] and the edits made since it was parsed.
#[derive(Clone, Debug)]
pub struct SyntaxView {
    snapshot: Arc<Snapshot>,
    edits: Vec<TextEdit>,
    mappable: bool,
    current: bool,
}

impl SyntaxView {
    /// The finished tree. Its offsets are those of the text at its [`Snapshot::version`], not
    /// of the current text when [`is_current`](Self::is_current) is false.
    pub fn snapshot(&self) -> &Arc<Snapshot> {
        &self.snapshot
    }

    /// Whether the tree is for the newest version the handle heard of.
    pub fn is_current(&self) -> bool {
        self.current
    }

    /// Highlights of `range` in the coordinates of the *current* text. For a stale tree they are
    /// computed on the old tree and carried across the edits made since (see [`map_spans`]); they
    /// are empty when the edits since were too many to keep.
    pub fn highlights(&self, range: Range<usize>) -> Vec<HighlightSpan> {
        if !self.mappable {
            return Vec::new();
        }
        if self.edits.is_empty() {
            return self.snapshot.highlights(range);
        }
        let old = map_backward(&self.edits, range.start, Side::Before)
            ..map_backward(&self.edits, range.end, Side::After);
        let spans = self.snapshot.highlights(old);
        map_spans(spans, &self.edits, &range)
    }

    /// An offset of the tree's text carried to the current text (`None` when the edits since were
    /// too many to keep).
    pub fn map_offset(&self, offset: usize) -> Option<usize> {
        self.mappable
            .then(|| map_forward(&self.edits, offset, Side::Before))
    }
}

fn run_worker(shared: &Shared, language: Language) {
    let loaded = config(language).and_then(|config| Parsers::new(config).map(|p| (config, p)));
    let (config, mut parsers) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::error!(%error, language = language.name(), "syntax language failed to load");
            shared.lock().load_error = Some(error);
            shared.published.notify_all();
            return;
        }
    };
    loop {
        let (request, cancel, base, edits) = {
            let mut state = shared.lock();
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if state.pending.is_some() {
                    break;
                }
                state = shared
                    .work
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            let Some(request) = state.pending.take() else {
                continue;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            state.in_flight = Some(InFlight {
                version: request.version,
                cancel: Arc::clone(&cancel),
            });
            state.stats.started += 1;
            let base = state
                .published
                .clone()
                .filter(|p| state.lost_through <= p.version() && p.version() < request.version);
            let edits: Vec<TextEdit> = base
                .as_ref()
                .map(|b| {
                    state
                        .log
                        .iter()
                        .filter(|l| l.version > b.version() && l.version <= request.version)
                        .map(|l| l.edit)
                        .collect()
                })
                .unwrap_or_default();
            if base.is_some() {
                state.stats.incremental += 1;
            }
            (request, cancel, base, edits)
        };

        let started = Instant::now();
        let cancelled =
            || cancel.load(Ordering::Acquire) || shared.shutdown.load(Ordering::Acquire);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let old = base.as_ref().map(|b| b.edited_tree(&edits));
            Snapshot::parse_with(
                config,
                &mut parsers,
                old.as_ref(),
                Arc::clone(&request.text),
                request.version,
                &cancelled,
            )
        }));
        let elapsed = started.elapsed();

        let mut published_version = None;
        {
            let mut state = shared.lock();
            state.in_flight = None;
            match outcome {
                Ok(Ok(snapshot)) => {
                    let version = snapshot.version();
                    state.stats.completed += 1;
                    state.stats.last_parse = elapsed;
                    state.log.retain(|l| l.version > version);
                    if state.lost_through <= version {
                        state.lost_through = 0;
                    }
                    state.published = Some(Arc::new(snapshot));
                    published_version = Some(version);
                }
                Ok(Err(SyntaxError::Cancelled)) => state.stats.cancelled += 1,
                Ok(Err(error)) => {
                    state.stats.failed += 1;
                    tracing::warn!(%error, "syntax parse failed");
                }
                Err(_) => {
                    state.stats.failed += 1;
                    tracing::warn!("syntax parse panicked; the parser was reset");
                    // A panic unwinds out of tree-sitter's parse and leaves the parser mid-input.
                    if let Ok(fresh) = Parsers::new(config) {
                        parsers = fresh;
                    }
                }
            }
        }
        if let Some(version) = published_version {
            shared.published.notify_all();
            if let Some(notify) = &shared.notify {
                notify(version);
            }
        }
    }
}
