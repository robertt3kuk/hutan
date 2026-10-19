//! The indexer: one low-priority worker thread that owns the write connection, an FSEvents
//! watcher that tells it which files changed, and the cloneable [`IndexHandle`] for everything
//! else.
//!
//! Nothing runs on a timer. The worker sleeps in `recv` until a file event, a Hutan record, or
//! a command arrives; at startup it does one `stat` pass over the source roots and reads only
//! the files whose stamp changed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use tracing::{debug, error, info, warn};

use crate::adapters::{Adapter, MAX_TOOL_CHUNKS, adapter_for};
use crate::error::{IndexError, Result};
use crate::redact::redact;
use crate::scan::{self, FileOutcome, Metrics, ScanContext, now_ms};
use crate::search;
use crate::store;
use crate::sys;
use crate::text::{chunks, idents};
use crate::titles::{CODEX_TITLES_FILE, read_codex_titles};
use crate::types::{
    CatalogRow, Hit, HutanRecord, Role, ScanInfo, SearchQuery, SessionText, SessionTextQuery,
    Source, Status,
};

/// Quiet time after the last file event before the changed files are read.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// A steady stream of events still gets read at least this often.
const MAX_DELAY: Duration = Duration::from_secs(2);
/// Messages waiting for the worker before producers drop (events) or wait (commands).
const QUEUE: usize = 4096;
const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone)]
pub struct IndexConfig {
    /// `index.sqlite`.
    pub db_path: PathBuf,
    /// Index only the last N days (files and rows); 0 keeps everything.
    pub range_days: u32,
    /// Transcript sources to read, with their roots. A source that is not listed is never read.
    pub roots: Vec<(Source, PathBuf)>,
    /// Accept Hutan records ([`IndexHandle::push_hutan`]).
    pub hutan: bool,
}

enum Msg {
    Fs(Vec<PathBuf>),
    Hutan(Box<HutanRecord>),
    Flush(SyncSender<()>),
    Clear(SyncSender<Result<()>>),
    Rebuild,
    /// Look at every transcript file again (the stat pass), then answer.
    Refresh(SyncSender<()>),
    Stop,
}

#[derive(Debug)]
struct Shared {
    db_path: PathBuf,
    tx: SyncSender<Msg>,
    /// Events were dropped because the queue was full: the worker repeats the stat pass.
    overflow: AtomicBool,
    /// [`Indexer::stop`] was called: a scan in progress ends at the next file.
    stopping: AtomicBool,
    /// Hutan records dropped because the queue was full.
    dropped: AtomicU64,
    metrics: Arc<Metrics>,
    sources: Vec<Source>,
    /// Progress of the stat pass, for the catalog's "Scanned N files".
    scan: Mutex<ScanInfo>,
}

/// Cheap, cloneable access to a running [`Indexer`].
#[derive(Debug, Clone)]
pub struct IndexHandle {
    shared: Arc<Shared>,
}

impl IndexHandle {
    /// Best hit per session for `query`, best first.
    pub fn search(&self, query: &SearchQuery) -> Result<Vec<Hit>> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        search::search(&conn, query)
    }

    /// The newest rows of one session, oldest first (what assistant transcript attachments
    /// read).
    pub fn session_text(&self, query: &SessionTextQuery) -> Result<Vec<SessionText>> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        store::session_text(&conn, &query.key, &query.sources, &query.roles, query.limit)
    }

    pub fn status(&self) -> Result<Status> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        Ok(Status {
            sources: store::source_status(&conn, &self.shared.sources)?,
            db_bytes: store::database_bytes(&self.shared.db_path),
        })
    }

    /// Every session of the enabled sources that has something to show, unfiltered and
    /// unsorted (`history.sessions` attributes, filters, and sorts them).
    pub fn catalog(&self) -> Result<Vec<CatalogRow>> {
        let conn = store::open_read_only(&self.shared.db_path)?;
        store::catalog(&conn, &self.shared.sources)
    }

    /// The last (or running) look at the transcript files.
    pub fn scan_info(&self) -> ScanInfo {
        self.shared
            .scan
            .lock()
            .map(|scan| scan.clone())
            .unwrap_or_default()
    }

    /// Looks at the transcript files again (new and changed ones are read) and returns `true`
    /// once that is done; `false` when it is not done within `wait` (a scan that is already
    /// running first finishes; the refresh still happens afterwards).
    pub fn refresh(&self, wait: Duration) -> Result<bool> {
        let (reply, done) = sync_channel(1);
        match self.shared.tx.try_send(Msg::Refresh(reply)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Ok(false),
            Err(TrySendError::Disconnected(_)) => return Err(IndexError::Stopped),
        }
        match done.recv_timeout(wait) {
            Ok(()) => Ok(true),
            Err(RecvTimeoutError::Timeout) => Ok(false),
            Err(RecvTimeoutError::Disconnected) => Err(IndexError::Stopped),
        }
    }

    /// Queues one Hutan record. `false` when the queue is full (the record is dropped and
    /// counted) or the indexer has stopped.
    pub fn push_hutan(&self, record: HutanRecord) -> bool {
        match self.shared.tx.try_send(Msg::Hutan(Box::new(record))) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Hutan records dropped so far because the worker could not keep up.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Reads the files with pending file events now (without the debounce), then returns once
    /// everything queued before this call has been applied.
    pub fn flush(&self) -> Result<()> {
        let (reply, done) = sync_channel(1);
        self.shared
            .tx
            .send(Msg::Flush(reply))
            .map_err(|_| IndexError::Stopped)?;
        done.recv().map_err(|_| IndexError::Stopped)
    }

    /// Deletes the index database and starts an empty one. Files already indexed are read again
    /// only when they change or on [`rebuild`](Self::rebuild).
    pub fn clear(&self) -> Result<()> {
        let (reply, done) = sync_channel(1);
        self.shared
            .tx
            .send(Msg::Clear(reply))
            .map_err(|_| IndexError::Stopped)?;
        done.recv().map_err(|_| IndexError::Stopped)?
    }

    /// Forgets everything read from transcript files and reads all of them again, in the
    /// background. Hutan rows are kept.
    pub fn rebuild(&self) -> Result<()> {
        self.shared
            .tx
            .send(Msg::Rebuild)
            .map_err(|_| IndexError::Stopped)
    }

    /// What the scanner has done to the file system.
    pub fn metrics(&self) -> &Metrics {
        &self.shared.metrics
    }

    pub fn db_path(&self) -> &Path {
        &self.shared.db_path
    }
}

/// A running indexer. Dropping it stops the worker and the watcher.
#[derive(Debug)]
pub struct Indexer {
    handle: IndexHandle,
    thread: Option<JoinHandle<()>>,
    watcher: Option<RecommendedWatcher>,
}

impl Indexer {
    /// Opens the database, starts the watcher on the existing source roots and the worker, which
    /// begins with the stat pass. Returns before that pass finishes.
    pub fn start(config: IndexConfig) -> Result<Self> {
        let conn = store::open(&config.db_path)?;
        let roots: Vec<Root> = config
            .roots
            .iter()
            .filter_map(|(source, root)| {
                let adapter: Arc<dyn Adapter> = Arc::from(adapter_for(*source)?);
                // FSEvents reports canonical paths; compare against the canonical root.
                let path = root.canonicalize().unwrap_or_else(|_| root.clone());
                Some(Root { path, adapter })
            })
            .collect();
        let mut sources: Vec<Source> = config.roots.iter().map(|(source, _)| *source).collect();
        if config.hutan {
            sources.push(Source::Hutan);
        }
        sources.sort();
        sources.dedup();

        let (tx, rx) = sync_channel(QUEUE);
        let shared = Arc::new(Shared {
            db_path: config.db_path.clone(),
            tx: tx.clone(),
            overflow: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            metrics: Arc::new(Metrics::default()),
            sources,
            // The first stat pass is about to start.
            scan: Mutex::new(ScanInfo {
                scanning: true,
                ..ScanInfo::default()
            }),
        });

        let watcher = watch_roots(&roots, &tx, &shared)?;
        let titles_file = roots
            .iter()
            .find(|root| root.adapter.source() == Source::Codex)
            .and_then(|root| root.path.parent())
            .map(|home| home.join(CODEX_TITLES_FILE));
        let worker = Worker {
            titles_file,
            titles_stamp: None,
            conn: Some(conn),
            db_path: config.db_path,
            range_days: config.range_days,
            roots,
            hutan: config.hutan,
            shared: Arc::clone(&shared),
            pending: BTreeSet::new(),
            first_pending: None,
            last_event: None,
        };
        let thread = std::thread::Builder::new()
            .name("hutan-index".to_owned())
            .spawn(move || worker.run(&rx))
            .map_err(IndexError::io("starting the history indexer"))?;
        Ok(Self {
            handle: IndexHandle { shared },
            thread: Some(thread),
            watcher,
        })
    }

    pub fn handle(&self) -> IndexHandle {
        self.handle.clone()
    }

    /// Stops the watcher and the worker (after it finishes the file it is reading, not the scan
    /// it is in) and waits for it. Idempotent.
    pub fn stop(&mut self) {
        self.watcher = None;
        if let Some(thread) = self.thread.take() {
            self.handle.shared.stopping.store(true, Ordering::Release);
            // A failed send means the worker is already gone.
            let _ = self.handle.shared.tx.send(Msg::Stop);
            if thread.join().is_err() {
                error!("the history indexer panicked");
            }
        }
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch_roots(
    roots: &[Root],
    tx: &SyncSender<Msg>,
    shared: &Arc<Shared>,
) -> Result<Option<RecommendedWatcher>> {
    if roots.is_empty() {
        return Ok(None);
    }
    let event_tx = tx.clone();
    let shared = Arc::clone(shared);
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) => {
                if let Err(TrySendError::Full(_)) = event_tx.try_send(Msg::Fs(event.paths)) {
                    shared.overflow.store(true, Ordering::Release);
                }
            }
            Err(error) => warn!(%error, "history watcher error"),
        })
        .map_err(|e| IndexError::Io {
            context: "creating the history file watcher".to_owned(),
            source: std::io::Error::other(e),
        })?;
    for root in roots {
        if !root.path.is_dir() {
            info!(root = %root.path.display(), "history source directory does not exist; not watching it");
            continue;
        }
        if let Err(error) = watcher.watch(&root.path, RecursiveMode::Recursive) {
            warn!(root = %root.path.display(), %error, "cannot watch a history source directory");
        }
    }
    Ok(Some(watcher))
}

struct Root {
    path: PathBuf,
    adapter: Arc<dyn Adapter>,
}

struct Worker {
    /// Codex's `session_index.jsonl`, when Codex is a source.
    titles_file: Option<PathBuf>,
    /// Size and mtime of that file when it was last read (`Some(None)`: it was missing).
    titles_stamp: Option<Option<(u64, i64)>>,
    conn: Option<Connection>,
    db_path: PathBuf,
    range_days: u32,
    roots: Vec<Root>,
    hutan: bool,
    shared: Arc<Shared>,
    pending: BTreeSet<PathBuf>,
    first_pending: Option<Instant>,
    last_event: Option<Instant>,
}

impl Worker {
    fn run(mut self, rx: &Receiver<Msg>) {
        sys::lower_current_thread_priority();
        self.startup();
        loop {
            let message = match self.next_deadline() {
                None => match rx.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
                Some(wait) => match rx.recv_timeout(wait) {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => {
                        self.read_pending();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                },
            };
            match message {
                Msg::Fs(paths) => {
                    let now = Instant::now();
                    self.first_pending.get_or_insert(now);
                    self.last_event = Some(now);
                    self.pending.extend(paths);
                }
                Msg::Hutan(record) => self.index_hutan(&record),
                Msg::Flush(reply) => {
                    self.read_pending();
                    let _ = reply.send(());
                }
                Msg::Clear(reply) => {
                    let _ = reply.send(self.clear());
                }
                Msg::Rebuild => self.rebuild(),
                Msg::Refresh(reply) => {
                    self.read_pending();
                    self.stat_pass();
                    let _ = reply.send(());
                }
                Msg::Stop => break,
            }
            if self.shared.overflow.swap(false, Ordering::AcqRel) {
                debug!("file events were dropped; running the stat pass again");
                self.stat_pass();
            }
        }
        debug!("history indexer stopped");
    }

    /// How long to wait for more file events before reading the pending files; `None` when
    /// nothing is pending (sleep until something arrives).
    fn next_deadline(&self) -> Option<Duration> {
        let (first, last) = (self.first_pending?, self.last_event?);
        let due = (last + DEBOUNCE).min(first + MAX_DELAY);
        Some(due.saturating_duration_since(Instant::now()))
    }

    fn cutoff_ms(&self) -> u64 {
        if self.range_days == 0 {
            0
        } else {
            now_ms().saturating_sub(u64::from(self.range_days) * DAY_MS)
        }
    }

    fn conn(&mut self) -> Result<&mut Connection> {
        if self.conn.is_none() {
            self.conn = Some(store::open(&self.db_path)?);
        }
        self.conn.as_mut().ok_or(IndexError::Stopped)
    }

    fn startup(&mut self) {
        let cutoff = self.cutoff_ms();
        if cutoff > 0 {
            match self
                .conn()
                .and_then(|conn| store::prune_before(conn, cutoff))
            {
                Ok(0) => {}
                Ok(removed) => info!(
                    removed,
                    "dropped history rows older than the configured range"
                ),
                Err(error) => warn!(%error, "pruning the history index failed"),
            }
        }
        self.stat_pass();
    }

    /// The indexer is being stopped: long loops end instead of finishing their pass.
    fn stopping(&self) -> bool {
        self.shared.stopping.load(Ordering::Acquire)
    }

    fn update_scan(&self, update: impl FnOnce(&mut ScanInfo)) {
        if let Ok(mut scan) = self.shared.scan.lock() {
            update(&mut scan);
        }
    }

    /// Reads Codex's thread titles when `session_index.jsonl` changed since the last look.
    fn refresh_titles(&mut self) {
        let Some(file) = self.titles_file.clone() else {
            return;
        };
        let stamp = std::fs::metadata(&file)
            .ok()
            .map(|meta| (meta.len(), scan::mtime_ns(&meta)));
        if self.titles_stamp == Some(stamp) {
            return;
        }
        let titles = if stamp.is_some() {
            let Some(titles) = read_codex_titles(&file) else {
                return;
            };
            titles
        } else {
            Vec::new()
        };
        let result = self
            .conn()
            .and_then(|conn| store::replace_provider_titles(conn, Source::Codex, &titles));
        match result {
            Ok(()) => self.titles_stamp = Some(stamp),
            Err(error) => warn!(%error, "storing the Codex thread titles failed"),
        }
    }

    /// One `stat` pass over every root; reads only the files whose stamp changed.
    fn stat_pass(&mut self) {
        let started = Instant::now();
        let cutoff_ms = self.cutoff_ms();
        self.update_scan(|scan| scan.scanning = true);
        self.refresh_titles();
        let mut changed: Vec<(usize, PathBuf)> = Vec::new();
        let mut per_source: Vec<(Source, u64)> = Vec::new();
        for (index, root) in self.roots.iter().enumerate() {
            let Some(conn) = self.conn.as_ref() else {
                break;
            };
            match scan::list_files(conn, root.adapter.as_ref(), &root.path, cutoff_ms) {
                Ok(listing) => {
                    per_source.push((root.adapter.source(), listing.total));
                    changed.extend(listing.changed.into_iter().map(|file| (index, file)));
                }
                Err(error) => {
                    warn!(root = %root.path.display(), %error, "scanning a history source failed")
                }
            }
        }
        let files: u64 = per_source.iter().map(|(_, count)| count).sum();
        let total = changed.len();
        self.update_scan(|scan| {
            scan.files = files;
            scan.done = files.saturating_sub(total as u64);
            scan.per_source = per_source;
        });
        let mut rows = 0;
        for (index, path) in changed {
            // A stop does not wait for the whole pass: the files not read yet keep their old
            // stamps and are read by the next pass.
            if self.stopping() {
                return;
            }
            rows += self.process(index, &path);
            self.update_scan(|scan| scan.done = (scan.done + 1).min(scan.files));
            std::thread::yield_now();
        }
        self.update_scan(|scan| {
            scan.scanning = false;
            scan.done = scan.files;
            scan.finished_ms = Some(now_ms());
        });
        if total > 0 {
            info!(
                files = total,
                rows,
                elapsed_ms = started.elapsed().as_millis(),
                "history stat pass finished"
            );
        }
    }

    fn read_pending(&mut self) {
        self.first_pending = None;
        self.last_event = None;
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        for path in pending {
            if self.stopping() {
                return;
            }
            let Some(index) = self.root_of(&path) else {
                continue;
            };
            let is_dir = std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
            if is_dir {
                let mut files = Vec::new();
                scan::for_each_file(&path, &mut |file, _| files.push(file.to_owned()));
                for file in files {
                    if self.stopping() {
                        return;
                    }
                    self.process(index, &file);
                    std::thread::yield_now();
                }
            } else {
                self.process(index, &path);
            }
        }
        self.refresh_titles();
        self.count_files();
    }

    /// After files were read because they changed (not in a stat pass): the files known per
    /// source are the `sources` rows, and the catalog is as current as now.
    fn count_files(&mut self) {
        let sources: Vec<Source> = self
            .roots
            .iter()
            .map(|root| root.adapter.source())
            .collect();
        let counts = self
            .conn()
            .and_then(|conn| store::file_counts(conn))
            .unwrap_or_default();
        self.update_scan(|scan| {
            scan.per_source = sources
                .iter()
                .map(|source| (*source, counts.get(source).copied().unwrap_or(0)))
                .collect();
            scan.files = scan.per_source.iter().map(|(_, count)| count).sum();
            scan.done = scan.files;
            scan.finished_ms = Some(now_ms());
        });
    }

    fn root_of(&self, path: &Path) -> Option<usize> {
        self.roots
            .iter()
            .position(|root| path.starts_with(&root.path))
    }

    /// Indexes one file; returns the rows added. Failures are logged, not fatal.
    fn process(&mut self, root: usize, path: &Path) -> u64 {
        let adapter = Arc::clone(&self.roots[root].adapter);
        if !adapter.accepts(path) {
            return 0;
        }
        let metrics = Arc::clone(&self.shared.metrics);
        let ctx = ScanContext {
            cutoff_ms: self.cutoff_ms(),
            metrics: &metrics,
        };
        let conn = match self.conn() {
            Ok(conn) => conn,
            Err(error) => {
                error!(%error, "the history database is not available");
                return 0;
            }
        };
        // A bug on one transcript must not end indexing for all of them.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan::process_file(conn, adapter.as_ref(), path, ctx)
        }))
        .unwrap_or_else(|_| {
            error!(path = %path.display(), "the history indexer panicked on a transcript; skipping it");
            Ok(FileOutcome::Skipped)
        });
        match outcome {
            Ok(FileOutcome::Indexed {
                rows,
                errors,
                reindexed,
            }) => {
                debug!(path = %path.display(), rows, errors, reindexed, "indexed a transcript");
                rows
            }
            Ok(FileOutcome::Skipped | FileOutcome::Unchanged) => 0,
            Err(error) => {
                warn!(path = %path.display(), %error, "indexing a transcript failed");
                0
            }
        }
    }

    fn index_hutan(&mut self, record: &HutanRecord) {
        if !self.hutan {
            return;
        }
        let cutoff = self.cutoff_ms();
        if cutoff > 0 && record.ts_ms < cutoff {
            return;
        }
        let result = (|| -> Result<()> {
            let conn = self.conn()?;
            let tx = conn
                .transaction()
                .map_err(IndexError::sqlite("starting a Hutan write"))?;
            let session = store::ensure_session(
                &tx,
                Source::Hutan,
                &record.session_key,
                None,
                record.title.as_deref(),
                record.cwd.as_deref(),
            )?;
            store::delete_ext(&tx, session, &record.ext_key)?;
            let text = redact(&record.text);
            for (index, piece) in chunks(&text).into_iter().take(MAX_TOOL_CHUNKS).enumerate() {
                let key = if index == 0 {
                    record.ext_key.clone()
                } else {
                    format!("{}#{index}", record.ext_key)
                };
                // The command line is the identifier of a command row.
                let idents = if record.role == Role::Command {
                    crate::adapters::truncate(piece, 1024).to_owned()
                } else {
                    idents(piece)
                };
                store::insert_entry(
                    &tx,
                    session,
                    record.role,
                    Some(record.ts_ms),
                    Some(&key),
                    record.block,
                    piece,
                    &idents,
                )?;
            }
            // The catalog's view of the session: what was said last, and how often.
            let mut summary = store::load_summary(&tx, session)?;
            match record.role {
                Role::User | Role::Assistant | Role::Command => {
                    summary.note_turn(record.role, Some(record.ts_ms), &record.text);
                }
                Role::Tool | Role::Output => summary.note_time(Some(record.ts_ms)),
            }
            summary.messages = store::count_hutan_messages(&tx, session)?;
            store::save_summary(&tx, session, &summary, Some(now_ms()))?;
            store::set_meta(&tx, "hutan_last_indexed_ms", &now_ms().to_string())?;
            tx.commit()
                .map_err(IndexError::sqlite("committing a Hutan write"))
        })();
        if let Err(error) = result {
            warn!(%error, "indexing a Hutan record failed");
        }
    }

    fn clear(&mut self) -> Result<()> {
        self.pending.clear();
        self.first_pending = None;
        self.last_event = None;
        // Close first: the file is deleted below.
        self.conn = None;
        store::remove_database(&self.db_path)?;
        self.conn = Some(store::open(&self.db_path)?);
        // The titles file is read again; nothing in the new database knows about it.
        self.titles_stamp = None;
        self.update_scan(|scan| {
            scan.files = 0;
            scan.done = 0;
            scan.per_source.clear();
        });
        info!("history index cleared");
        Ok(())
    }

    fn rebuild(&mut self) {
        let result = self.conn().and_then(|conn| {
            let tx = conn
                .transaction()
                .map_err(IndexError::sqlite("starting a rebuild"))?;
            store::delete_external(&tx)?;
            tx.commit()
                .map_err(IndexError::sqlite("committing a rebuild"))
        });
        match result {
            Ok(()) => {
                info!("rebuilding the history index");
                self.stat_pass();
            }
            Err(error) => warn!(%error, "rebuilding the history index failed"),
        }
    }
}
