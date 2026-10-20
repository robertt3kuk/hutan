//! The one theme registry of an application: owns the catalog snapshot, rescans when the
//! themes folder changes and publishes generations.
//!
//! One `notify` watcher watches `<data dir>/themes` (creation, removal, single-file packs) and
//! each pack folder (edits in place); until the folder exists it watches the data dir to see it
//! appear. Events go through a bounded channel into one worker that debounces (100 ms quiet,
//! 500 ms at most) and rescans; a full channel already has a rescan pending, so overflow only
//! coalesces. A rescan that finds nothing new publishes nothing.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::{Mutex, RwLock};
use tokio::sync::watch;

use super::catalog::{OverrideMemory, Selected, SelectionRequest, ThemeCatalogSnapshot, scan};
use super::resolve::{ResolvedTheme, TerminalTheme};
use crate::appearance::ThemeOverrides;
use crate::error::SettingsError;

/// Quiet period after the last event before rescanning.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// A steady stream of events still rescans at least this often.
const MAX_DELAY: Duration = Duration::from_millis(500);
/// How long the platform watcher needs to be live after the watched set changed.
const SETTLE: Duration = Duration::from_millis(150);
const MAX_SETTLE_ROUNDS: usize = 4;
/// Pending filesystem events; a full queue already has a rescan coming.
const QUEUE: usize = 64;

struct Shared {
    /// `false` for a registry without a data folder: the built-ins only, nothing to scan.
    folder: bool,
    themes_dir: PathBuf,
    data_dir: PathBuf,
    snapshot: RwLock<Arc<ThemeCatalogSnapshot>>,
    icons: RwLock<Arc<super::icons::IconCatalog>>,
    active_icons: Mutex<Option<Arc<super::icons::ResolvedIcons>>>,
    tx: watch::Sender<u64>,
    memory: Mutex<OverrideMemory>,
    /// Serializes scans so two callers never publish out of order.
    scanning: Mutex<()>,
    watching: Mutex<Option<WatchState>>,
}

struct WatchState {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

/// The theme registry; cheap to clone.
#[derive(Clone)]
pub struct ThemeRegistry {
    shared: Arc<Shared>,
}

impl fmt::Debug for ThemeRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThemeRegistry")
            .field("themes_dir", &self.shared.themes_dir)
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl ThemeRegistry {
    /// A registry for `<data_dir>/themes`, scanned once now. The data dir is resolved once (it
    /// may sit behind symlinks such as macOS's `/var`); nothing below it is followed.
    pub fn new(data_dir: &Path) -> Self {
        let data_dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
        let themes_dir = data_dir.join("themes");
        let snapshot = Arc::new(scan(&themes_dir, None));
        let (tx, _) = watch::channel(snapshot.generation);
        Self {
            shared: Arc::new(Shared {
                folder: true,
                themes_dir,
                data_dir: data_dir.clone(),
                snapshot: RwLock::new(snapshot),
                icons: RwLock::new(Arc::new(super::icons::IconCatalog::scan(
                    Some(&data_dir),
                    None,
                ))),
                active_icons: Mutex::new(None),
                tx,
                memory: Mutex::new(OverrideMemory::default()),
                scanning: Mutex::new(()),
                watching: Mutex::new(None),
            }),
        }
    }

    /// A registry that has no data folder (an in-memory settings store): the built-ins only.
    pub fn builtin_only() -> Self {
        let snapshot = Arc::new(ThemeCatalogSnapshot::builtin_only());
        let (tx, _) = watch::channel(snapshot.generation);
        Self {
            shared: Arc::new(Shared {
                folder: false,
                themes_dir: PathBuf::new(),
                data_dir: PathBuf::new(),
                snapshot: RwLock::new(snapshot),
                tx,
                icons: RwLock::new(Arc::new(super::icons::IconCatalog::builtin_only())),
                active_icons: Mutex::new(None),
                memory: Mutex::new(OverrideMemory::default()),
                scanning: Mutex::new(()),
                watching: Mutex::new(None),
            }),
        }
    }

    /// Where user packs live; `None` without a data folder.
    pub fn themes_dir(&self) -> Option<&Path> {
        self.shared
            .folder
            .then_some(self.shared.themes_dir.as_path())
    }

    /// The current catalog. Cheap; hold it for as long as a frame or a style pass needs.
    pub fn snapshot(&self) -> Arc<ThemeCatalogSnapshot> {
        Arc::clone(&self.shared.snapshot.read())
    }

    pub fn icon_snapshot(&self) -> Arc<super::icons::IconCatalog> {
        Arc::clone(&self.shared.icons.read())
    }

    pub fn select_icons(
        &self,
        selector: &str,
        appearance: super::SystemAppearance,
    ) -> (Arc<super::icons::ResolvedIcons>, Vec<SettingsError>) {
        let catalog = self.icon_snapshot();
        let mut previous = self.shared.active_icons.lock();
        let (resolved, diagnostics) = catalog.select(selector, appearance, previous.as_ref());
        *previous = Some(Arc::clone(&resolved));
        (resolved, diagnostics)
    }

    pub fn generation(&self) -> u64 {
        self.shared.snapshot.read().generation
    }

    /// Receiver of the generation; it only holds the latest value.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.shared.tx.subscribe()
    }

    /// Rescans the folder now. `true` when the catalog changed and a new generation was
    /// published.
    pub fn rescan(&self) -> bool {
        self.shared.rescan()
    }

    /// [`ThemeCatalogSnapshot::select`] with this registry's memory of accepted overrides.
    pub fn select(
        &self,
        snapshot: &ThemeCatalogSnapshot,
        request: SelectionRequest<'_>,
    ) -> Selected {
        snapshot.select(request, &mut self.shared.memory.lock())
    }

    /// [`ThemeCatalogSnapshot::terminal`] with this registry's memory of accepted overrides.
    pub fn terminal(
        &self,
        snapshot: &ThemeCatalogSnapshot,
        selector: &str,
        active: &ResolvedTheme,
        overrides: &std::collections::BTreeMap<String, ThemeOverrides>,
    ) -> (TerminalTheme, Vec<SettingsError>) {
        snapshot.terminal(selector, active, overrides, &mut self.shared.memory.lock())
    }

    /// Starts the watcher and its worker; dropping the guard stops hot reload.
    pub fn start_watching(&self) -> Result<ThemeWatchGuard, SettingsError> {
        let fail = |message: String| SettingsError::error(None, "", message);
        if !self.shared.folder {
            return Err(fail("there is no themes folder to watch".to_owned()));
        }
        if self.shared.watching.lock().is_some() {
            return Err(fail("themes are already being watched".to_owned()));
        }
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
        let event_tx = tx.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            match event {
                // A full queue already has a rescan pending; dropping is correct.
                Ok(_) => {
                    let _ = event_tx.try_send(Msg::Changed);
                }
                Err(e) => tracing::warn!("theme watcher error: {e}"),
            }
        })
        .map_err(|e| fail(format!("cannot create the theme watcher: {e}")))?;
        *self.shared.watching.lock() = Some(WatchState {
            watcher,
            dirs: BTreeSet::new(),
        });
        self.shared.rewatch();
        let weak = Arc::downgrade(&self.shared);
        let thread = std::thread::Builder::new()
            .name("hutan-theme-watch".to_owned())
            .spawn(move || run(&rx, &weak))
            .map_err(|e| fail(format!("cannot spawn the theme watcher thread: {e}")))?;
        // Anything edited between `new` and the watcher starting; the worker looks once more
        // after the first debounce, for what the stream's startup missed.
        self.shared.rescan();
        let _ = tx.try_send(Msg::Changed);
        Ok(ThemeWatchGuard {
            shared: Arc::clone(&self.shared),
            tx,
            thread: Some(thread),
        })
    }
}

enum Msg {
    Changed,
    Stop,
}

/// Keeps the watcher and its worker alive.
pub struct ThemeWatchGuard {
    shared: Arc<Shared>,
    tx: SyncSender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for ThemeWatchGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThemeWatchGuard").finish_non_exhaustive()
    }
}

impl Drop for ThemeWatchGuard {
    fn drop(&mut self) {
        *self.shared.watching.lock() = None;
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Shared {
    fn rescan(&self) -> bool {
        if !self.folder {
            return false;
        }
        let _guard = self.scanning.lock();
        let previous = Arc::clone(&self.snapshot.read());
        let next = scan(&self.themes_dir, Some(&previous));
        let previous_icons = Arc::clone(&self.icons.read());
        let mut icons =
            super::icons::IconCatalog::scan(Some(&self.data_dir), Some(&previous_icons));
        if next.fingerprint() == previous.fingerprint()
            && icons.fingerprint() == previous_icons.fingerprint()
        {
            return false;
        }
        let generation = previous.generation + 1;
        *self.snapshot.write() = Arc::new(next.with_generation(generation));
        icons.generation = generation;
        *self.icons.write() = Arc::new(icons);
        self.tx.send_replace(generation);
        true
    }

    /// The directories to watch now: the themes folder and each pack folder in it; the data
    /// dir while the themes folder does not exist yet.
    fn wanted_dirs(&self) -> BTreeSet<PathBuf> {
        let is_real_dir = |path: &Path| {
            std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        };
        let mut wanted = BTreeSet::new();
        wanted.insert(self.data_dir.clone());
        super::icons::watch_dirs(&self.data_dir, &mut wanted);
        if is_real_dir(&self.themes_dir) {
            wanted.insert(self.themes_dir.clone());
            let snapshot = Arc::clone(&self.snapshot.read());
            for dir in snapshot.pack_dirs() {
                if dir != self.themes_dir && dir.starts_with(&self.themes_dir) && is_real_dir(&dir)
                {
                    wanted.insert(dir);
                }
            }
            // A folder that holds a broken pack is not in the snapshot's valid list; add every
            // real subfolder, bounded by the scan's own candidate cap.
            if let Ok(entries) = std::fs::read_dir(&self.themes_dir) {
                for entry in entries.flatten().take(super::catalog::MAX_CANDIDATES) {
                    let path = entry.path();
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        wanted.insert(path);
                    }
                }
            }
        } else if is_real_dir(&self.data_dir) {
            wanted.insert(self.data_dir.clone());
        }
        wanted
    }

    /// Brings the watched set in line with [`Self::wanted_dirs`]. Returns whether the watched
    /// set changed.
    fn rewatch(&self) -> bool {
        let wanted = self.wanted_dirs();
        let mut guard = self.watching.lock();
        let Some(state) = guard.as_mut() else {
            return false;
        };
        let mut added = false;
        for dir in state.dirs.difference(&wanted).cloned().collect::<Vec<_>>() {
            // The directory may already be gone; nothing to unwatch then.
            let _ = state.watcher.unwatch(&dir);
            state.dirs.remove(&dir);
            added = true;
        }
        for dir in wanted.difference(&state.dirs.clone()) {
            match state.watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    state.dirs.insert(dir.clone());
                    added = true;
                }
                Err(e) => tracing::warn!("cannot watch {}: {e}", dir.display()),
            }
        }
        added
    }
}

fn run(rx: &mpsc::Receiver<Msg>, shared: &Weak<Shared>) {
    loop {
        match rx.recv() {
            Ok(Msg::Changed) => {}
            Ok(Msg::Stop) | Err(_) => return,
        }
        let started = Instant::now();
        loop {
            let left = MAX_DELAY.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(DEBOUNCE.min(left)) {
                Ok(Msg::Changed) => {}
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        // Starting or stopping a watch restarts the platform's event stream (FSEvents), and
        // what happens in that gap produces no event: look again once it has settled, until
        // the watched set stops changing.
        for _ in 0..MAX_SETTLE_ROUNDS {
            shared.rescan();
            if !shared.rewatch() {
                break;
            }
            std::thread::sleep(SETTLE);
        }
    }
}
