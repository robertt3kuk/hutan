//! The daemon's file service (protocol 0.33, capabilities `fs` and `fs.watch`): confined reads
//! and listings, atomic chunked writes, a fuzzy finder, text search and change watching, so
//! that clients, the CLI and agents reach files through the one place permissions are enforced
//! (ADR 0012, amendment 2026-10-21; `docs/protocol.md` § Files).
//!
//! - [`confine`]: which paths a request may name, and how they are opened.
//! - [`read`]: `fs.read`, `fs.stat`, `fs.list`.
//! - [`write`]: `fs.write.*`: a temporary file, a flush, one rename.
//! - [`find`] and [`search`]: `fs.find` over a bounded index, `fs.search`.
//! - [`watch`]: `fs.watch`, `fs.changed`, `fs.generation` and who made a change.
//!
//! Blocking I/O runs on the blocking pool. Everything that waits or queues is bounded: eight
//! uploads (each discarded after 30 s without a chunk), four searches per connection, 256
//! watched paths per connection, indexes of 200 000 paths, event windows of 500 changes.

pub mod confine;
pub mod find;
pub mod read;
pub mod search;
pub mod watch;
pub mod write;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use hutan_core::SessionId;
use hutan_protocol::control::ErrorBody;
use hutan_protocol::fs::{
    CHANGES_MAX, CHUNK_MAX_BYTES, FIND_MAX, FsEntry, FsFile, FsFind, FsFound, FsList, FsListing,
    FsRead, FsSearch, FsSearchCancel, FsSearchResult, FsStat, FsUnwatch, FsUpload, FsWatch,
    FsWatching, FsWriteAbort, FsWriteBegin, FsWriteChunk, FsWriteCommit, FsWritten, LIST_MAX,
    SEARCH_MAX, SEARCHES_MAX, UPLOAD_IDLE_SECS, UPLOADS_MAX,
};
use hutan_protocol::messages::{Empty, Event, Request, SessionListFilter};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use self::confine::{FsError, canonical_roots, fstat, kind_of, open, resolve};
use self::find::Finder;
use self::watch::{RootKey, WatchHub};
use self::write::Upload;
use crate::bus::EventBus;
use crate::conn::{ConnHandle, ConnId};
use crate::registry::RegistryHandle;

/// Events of the bus the service watches for files agents report; the queue is bounded and a
/// full one drops the event.
const OBSERVER_QUEUE: usize = 256;

/// The bounds a test may lower; [`Limits::default`] is the protocol's.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Most changes of one `fs.changed` event.
    pub changes_max: usize,
    /// How long an upload may go without a chunk.
    pub upload_idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            changes_max: CHANGES_MAX,
            upload_idle: Duration::from_secs(UPLOAD_IDLE_SECS),
        }
    }
}

/// An upload that a blocking job works on while the table holds it too; the commit takes it.
type SharedUpload = Arc<Mutex<Option<Upload>>>;

/// An upload between its begin and its commit.
#[derive(Debug)]
struct Slot {
    conn: ConnId,
    agent: Option<SessionId>,
    last_activity: Instant,
    upload: SharedUpload,
}

#[derive(Debug)]
pub struct FileService {
    registry: RegistryHandle,
    hub: Arc<WatchHub>,
    finder: Arc<Finder>,
    uploads: Mutex<HashMap<String, Slot>>,
    searches: Mutex<HashMap<(ConnId, String), Arc<AtomicBool>>>,
    upload_idle: Duration,
}

impl FileService {
    /// Starts the service: it listens to the bus for the files agents report, and discards
    /// uploads that stop receiving chunks after `upload_idle`.
    pub fn new(
        registry: RegistryHandle,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
        limits: Limits,
    ) -> Arc<Self> {
        let hub = WatchHub::new(
            registry.clone(),
            bus.clone(),
            shutdown.clone(),
            limits.changes_max,
        );
        let upload_idle = limits.upload_idle;
        let service = Arc::new(Self {
            registry,
            hub: hub.clone(),
            finder: Arc::new(Finder::default()),
            uploads: Mutex::new(HashMap::new()),
            searches: Mutex::new(HashMap::new()),
            upload_idle,
        });
        let (observed, mut events) = mpsc::channel::<Event>(OBSERVER_QUEUE);
        bus.set_fs_observer(Some(observed));
        let stop = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = stop.cancelled() => break,
                    event = events.recv() => match event {
                        Some(event) => hub.observe(&event),
                        None => break,
                    },
                }
            }
        });
        tokio::spawn(reap_uploads(Arc::downgrade(&service), shutdown));
        service
    }

    /// The folders the daemon tracks: those of its workspaces and the checkouts its sessions
    /// work in. Canonicalised by the blocking side.
    async fn roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Ok(list) = self.registry.list_workspaces().await {
            roots.extend(list.workspaces.into_iter().flat_map(|w| w.roots));
        }
        if let Ok(list) = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        {
            roots.extend(
                list.sessions
                    .into_iter()
                    .filter_map(|s| s.placement.map(|p| p.root)),
            );
        }
        roots
    }

    /// Runs `job` with the canonical roots on the blocking pool.
    async fn blocking<T: Send + 'static>(
        &self,
        job: impl FnOnce(Vec<PathBuf>) -> Result<T, FsError> + Send + 'static,
    ) -> Result<T, ErrorBody> {
        let roots = self.roots().await;
        spawn_blocking(move || job(canonical_roots(&roots)))
            .await
            .map_err(|e| ErrorBody::new(hutan_protocol::ErrorCode::Internal, e.to_string()))?
            .map_err(ErrorBody::from)
    }

    // Reading ----------------------------------------------------------------------------------

    /// `fs.read` of connection `conn`. `trusted`: the request is not an agent's, so `picked`
    /// counts. The version read is remembered for the connection's watch of the file.
    pub async fn read(
        &self,
        conn: ConnId,
        trusted: bool,
        req: FsRead,
    ) -> Result<FsFile, ErrorBody> {
        let hub = self.hub.clone();
        self.blocking(move |roots| {
            let resolved = resolve(&req.path, &roots, req.picked && trusted)?;
            let max = req
                .max_bytes
                .map_or(CHUNK_MAX_BYTES, |max| (max as usize).min(CHUNK_MAX_BYTES));
            let file = read::read(&resolved, req.offset.unwrap_or(0), max)?;
            hub.served(conn, &file.path, file.version);
            Ok(file)
        })
        .await
    }

    pub async fn stat(&self, trusted: bool, req: FsStat) -> Result<FsEntry, ErrorBody> {
        self.blocking(move |roots| read::stat(&resolve(&req.path, &roots, req.picked && trusted)?))
            .await
    }

    pub async fn list(&self, trusted: bool, req: FsList) -> Result<FsListing, ErrorBody> {
        self.blocking(move |roots| {
            let resolved = resolve(&req.dir, &roots, req.picked && trusted)?;
            read::list(&resolved, req.ignore, req.limit.unwrap_or(LIST_MAX))
        })
        .await
    }

    /// `fs.find`.
    pub async fn find(&self, req: FsFind) -> Result<FsFound, ErrorBody> {
        let asked = req.root.clone();
        let root = self
            .blocking(move |roots| {
                let resolved = resolve(&asked, &roots, false)?;
                require_directory(&resolved)?;
                Ok(resolved.canonical)
            })
            .await?;
        // The index of a watched root is valid while its watch generation holds.
        let generation = self.hub.generation_of(&root);
        let limit = req.limit.unwrap_or(50).clamp(1, FIND_MAX);
        let finder = self.finder.clone();
        let (indexed_root, query) = (root.clone(), req.query);
        let (matches, indexed, truncated) = spawn_blocking(move || {
            let index = finder.index(&indexed_root, generation);
            (index.find(&query, limit), index.len(), index.truncated())
        })
        .await
        .map_err(join_error)?;
        Ok(FsFound {
            root,
            matches,
            indexed: u32::try_from(indexed).unwrap_or(u32::MAX),
            index_truncated: truncated,
        })
    }

    /// `fs.search`: runs until it ends or is cancelled.
    pub async fn search(
        &self,
        conn: &ConnHandle,
        req: FsSearch,
    ) -> Result<FsSearchResult, ErrorBody> {
        if req.search_id.is_empty() || req.search_id.len() > 128 {
            return Err(FsError::bad("search_id must be 1 to 128 characters").into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut searches = self.searches.lock();
            if searches.contains_key(&(conn.id(), req.search_id.clone())) {
                return Err(
                    FsError::bad(format!("a search named `{}` is running", req.search_id)).into(),
                );
            }
            if searches.keys().filter(|(id, _)| *id == conn.id()).count() >= SEARCHES_MAX {
                return Err(FsError::InvalidState(format!(
                    "a connection runs at most {SEARCHES_MAX} searches at once"
                ))
                .into());
            }
            searches.insert((conn.id(), req.search_id.clone()), cancel.clone());
        }
        let outcome = self.run_search(conn, &req, cancel).await;
        self.searches
            .lock()
            .remove(&(conn.id(), req.search_id.clone()));
        let outcome = outcome?;
        Ok(FsSearchResult {
            search_id: req.search_id,
            hits: outcome.hits,
            files_searched: outcome.files_searched,
            truncated: outcome.truncated,
            cancelled: outcome.cancelled,
        })
    }

    async fn run_search(
        &self,
        conn: &ConnHandle,
        req: &FsSearch,
        cancel: Arc<AtomicBool>,
    ) -> Result<search::Outcome, ErrorBody> {
        let hub = self.hub.clone();
        let conn = conn.clone();
        let search_id = req.search_id.clone();
        let asked = req.root.clone();
        let (pattern, regex, case_sensitive, globs) = (
            req.pattern.clone(),
            req.regex,
            req.case_sensitive,
            req.globs.clone(),
        );
        let limit = req.limit.unwrap_or(200).clamp(1, SEARCH_MAX);
        self.blocking(move |roots| {
            let resolved = resolve(&asked, &roots, false)?;
            require_directory(&resolved)?;
            let params = search::Params {
                root: resolved.canonical,
                pattern,
                regex,
                case_sensitive,
                globs,
                limit,
            };
            search::search(&params, &cancel, &mut |files_searched, hits| {
                let event = Event::FsSearchProgress {
                    search_id: search_id.clone(),
                    files_searched,
                    hits,
                };
                if let Some(frame) = hub.frame(&event) {
                    // A progress report that does not fit the queue is dropped.
                    let _ = conn.enqueue_local(frame);
                }
            })
        })
        .await
    }

    /// `fs.search.cancel`.
    pub fn search_cancel(&self, conn: ConnId, req: &FsSearchCancel) -> Empty {
        if let Some(cancel) = self.searches.lock().get(&(conn, req.search_id.clone())) {
            cancel.store(true, Ordering::Release);
        }
        Empty {}
    }

    // Writing ----------------------------------------------------------------------------------

    /// `fs.write.begin`. `agent`: the session of the agent that asks, if it is one.
    pub async fn write_begin(
        &self,
        conn: &ConnHandle,
        agent: Option<SessionId>,
        req: FsWriteBegin,
    ) -> Result<FsUpload, ErrorBody> {
        {
            let mut uploads = self.uploads.lock();
            self.sweep(&mut uploads);
            if uploads.len() >= UPLOADS_MAX {
                return Err(FsError::InvalidState(format!(
                    "{UPLOADS_MAX} uploads are open already; finish or abort one first"
                ))
                .into());
            }
        }
        let id = ulid::Ulid::generate().to_string();
        let upload_id = id.clone();
        let trusted = agent.is_none();
        let upload = self
            .blocking(move |roots| Upload::begin(&roots, &req, trusted, &upload_id))
            .await?;
        let mut uploads = self.uploads.lock();
        if uploads.len() >= UPLOADS_MAX {
            return Err(FsError::InvalidState(format!(
                "{UPLOADS_MAX} uploads are open already; finish or abort one first"
            ))
            .into());
        }
        uploads.insert(
            id.clone(),
            Slot {
                conn: conn.id(),
                agent,
                last_activity: Instant::now(),
                upload: Arc::new(Mutex::new(Some(upload))),
            },
        );
        Ok(FsUpload { upload_id: id })
    }

    /// Whether `upload_id` is an upload of this connection: what a chunk, a commit or an abort
    /// continues, after the begin was allowed.
    pub fn owns_upload(&self, conn: ConnId, upload_id: &str) -> bool {
        self.uploads
            .lock()
            .get(upload_id)
            .is_some_and(|slot| slot.conn == conn)
    }

    fn slot_of(
        &self,
        conn: ConnId,
        upload_id: &str,
    ) -> Result<(SharedUpload, Option<SessionId>), ErrorBody> {
        let mut uploads = self.uploads.lock();
        self.sweep(&mut uploads);
        match uploads.get_mut(upload_id) {
            Some(slot) if slot.conn == conn => {
                slot.last_activity = Instant::now();
                Ok((slot.upload.clone(), slot.agent))
            }
            _ => Err(FsError::InvalidState(format!(
                "no upload {upload_id}: it was never begun, finished, or expired after {} s without a chunk",
                self.upload_idle.as_secs()
            ))
            .into()),
        }
    }

    /// `fs.write.chunk`. A chunk that is refused ends the upload.
    pub async fn write_chunk(&self, conn: ConnId, req: FsWriteChunk) -> Result<Empty, ErrorBody> {
        let (upload, _) = self.slot_of(conn, &req.upload_id)?;
        let appended = spawn_blocking(move || {
            let mut upload = upload.lock();
            match upload.as_mut() {
                Some(upload) => upload.append(req.seq, &req.text),
                None => Err(FsError::InvalidState("the upload is over".into())),
            }
        })
        .await
        .map_err(join_error)?;
        if let Err(error) = appended {
            self.uploads.lock().remove(&req.upload_id);
            return Err(error.into());
        }
        Ok(Empty {})
    }

    /// `fs.write.commit`.
    pub async fn write_commit(
        &self,
        conn: &ConnHandle,
        req: FsWriteCommit,
    ) -> Result<FsWritten, ErrorBody> {
        let (upload, agent) = self.slot_of(conn.id(), &req.upload_id)?;
        // The upload ends here whatever happens: a refused commit has to be started again.
        self.uploads.lock().remove(&req.upload_id);
        let write_id = ulid::Ulid::generate().to_string();
        let hub = self.hub.clone();
        let (writer, id) = (conn.id(), write_id.clone());
        let total = req.total_bytes;
        let committed = spawn_blocking(move || {
            let Some(upload) = upload.lock().take() else {
                return Err(FsError::InvalidState("the upload is over".into()));
            };
            // Known before the rename, so that the change it causes is recognised as ours.
            hub.write_started(upload.target(), writer, agent, &id);
            let committed = upload.commit(total);
            hub.write_done(&id, committed.as_ref().ok().map(|(_, version)| *version));
            committed
        })
        .await
        .map_err(join_error)??;
        // The writer knows the file as it wrote it, for its watch of the file.
        self.hub.served(writer, &committed.0, committed.1);
        Ok(FsWritten {
            path: committed.0,
            version: committed.1,
            write_id,
        })
    }

    /// `fs.write.abort`.
    pub fn write_abort(&self, conn: ConnId, req: &FsWriteAbort) -> Empty {
        let mut uploads = self.uploads.lock();
        if uploads.get(&req.upload_id).is_some_and(|s| s.conn == conn) {
            uploads.remove(&req.upload_id);
        }
        Empty {}
    }

    /// Drops the uploads that were idle too long (their temporary files go with them).
    fn sweep(&self, uploads: &mut HashMap<String, Slot>) {
        let idle = self.upload_idle;
        uploads.retain(|id, slot| {
            let keep = slot.last_activity.elapsed() < idle;
            if !keep {
                debug!(upload = %id, "discarding an idle upload");
            }
            keep
        });
    }

    // Watching ---------------------------------------------------------------------------------

    /// `fs.watch`.
    pub async fn watch(
        &self,
        conn: &ConnHandle,
        agent: Option<SessionId>,
        req: FsWatch,
    ) -> Result<FsWatching, ErrorBody> {
        let hub = self.hub.clone();
        let conn = conn.clone();
        let trusted = agent.is_none();
        self.blocking(move |roots| {
            let mut specs = Vec::with_capacity(req.paths.len());
            for watched in &req.paths {
                let resolved = resolve(&watched.path, &roots, req.picked && trusted)?;
                let is_dir = kind_of(&fstat(&open(&resolved)?, &resolved.canonical)?)
                    == hutan_protocol::fs::FsKind::Dir;
                let key = if resolved.confined {
                    RootKey {
                        path: resolved.root.clone(),
                        recursive: true,
                    }
                } else if is_dir {
                    RootKey {
                        path: resolved.canonical.clone(),
                        recursive: watched.recursive,
                    }
                } else {
                    RootKey {
                        path: resolved
                            .canonical
                            .parent()
                            .map_or_else(|| PathBuf::from("/"), ToOwned::to_owned),
                        recursive: false,
                    }
                };
                specs.push((resolved.canonical, watched.recursive && is_dir, key));
            }
            hub.watch(&conn, specs)
        })
        .await
    }

    /// `fs.unwatch`.
    pub async fn unwatch(&self, conn: ConnId, req: FsUnwatch) -> Result<FsWatching, ErrorBody> {
        let hub = self.hub.clone();
        spawn_blocking(move || {
            let paths: Vec<PathBuf> = req
                .paths
                .iter()
                .map(|p| watch::canonical_lenient(p))
                .collect();
            hub.unwatch(conn, &paths)
        })
        .await
        .map_err(join_error)
    }

    /// Drops everything the closed connection held: uploads, searches, watched paths.
    pub fn connection_closed(&self, conn: ConnId) {
        self.uploads.lock().retain(|_, slot| slot.conn != conn);
        for ((id, _), cancel) in self.searches.lock().iter() {
            if *id == conn {
                cancel.store(true, Ordering::Release);
            }
        }
        self.hub.connection_closed(conn);
    }

    /// The request when it is one of the file service's that continues an upload this
    /// connection began.
    pub fn continues_upload(&self, conn: ConnId, request: &Request) -> bool {
        match request {
            Request::FsWriteChunk(r) => self.owns_upload(conn, &r.upload_id),
            Request::FsWriteCommit(r) => self.owns_upload(conn, &r.upload_id),
            Request::FsWriteAbort(r) => self.owns_upload(conn, &r.upload_id),
            _ => false,
        }
    }
}

fn require_directory(resolved: &confine::Resolved) -> Result<(), FsError> {
    let stat = fstat(&open(resolved)?, &resolved.canonical)?;
    if kind_of(&stat) == hutan_protocol::fs::FsKind::Dir {
        Ok(())
    } else {
        Err(FsError::unsupported(
            "not_a_file",
            format!("{} is not a directory", resolved.canonical.display()),
        ))
    }
}

fn join_error(error: tokio::task::JoinError) -> ErrorBody {
    ErrorBody::new(hutan_protocol::ErrorCode::Internal, error.to_string())
}

/// Discards uploads that went quiet, once a second (or sooner for short timeouts).
async fn reap_uploads(service: Weak<FileService>, shutdown: CancellationToken) {
    loop {
        let Some(idle) = service.upgrade().map(|s| s.upload_idle) else {
            return;
        };
        let every = (idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(1));
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(every) => {}
        }
        let Some(service) = service.upgrade() else {
            return;
        };
        let mut uploads = service.uploads.lock();
        service.sweep(&mut uploads);
    }
}

#[cfg(test)]
mod tests {
    /// The queues of this module and its children are bounded: no unbounded channel anywhere
    /// in the service (`AGENTS.md`: bounded everything).
    #[test]
    fn no_channel_of_the_file_service_is_unbounded() {
        let needle = ["unbounded", "_channel"].concat();
        let needle_type = ["Unbounded", "Sender"].concat();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/files");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let source = std::fs::read_to_string(&path).unwrap();
            // The test itself spells the needles out of pieces.
            assert!(!source.contains(&needle), "{}", path.display());
            assert!(!source.contains(&needle_type), "{}", path.display());
            checked += 1;
        }
        assert!(checked >= 7);
    }
}
