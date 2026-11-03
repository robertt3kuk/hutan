//! File watching: `notify` events -> bounded channel -> debounce thread -> `reload_from_disk`.
//!
//! Directories are watched (not files) so atomic-rename saves and files created later are seen.
//! The reload re-reads all three files and compares contents, so event paths never need to be
//! matched (FSEvents reports canonical paths; temp dirs live behind symlinks).

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::SettingsError;
use crate::store::{Shared, create_private_dir};

/// Quiet period after the last event before reloading.
const DEBOUNCE: Duration = Duration::from_millis(100);
/// A steady stream of events (noisy workspace root) still reloads at least this often.
const MAX_DELAY: Duration = Duration::from_millis(500);
const QUEUE: usize = 64;
/// With no event at all, the files are still read again this often. The platform's stream
/// (macOS FSEvents) starts asynchronously, and under load reports late or never: a write right
/// after the watch, or minutes into the session, may produce no event, and the edit would not
/// apply until something else touched the folder. Reading the (at most four, small) files
/// again is a string comparison per file; nothing is parsed or published unless one changed.
const SAFETY_POLL: Duration = Duration::from_secs(5);

enum Msg {
    Changed,
    Stop,
}

pub(crate) struct WatchState {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

/// Keeps the watcher and its debounce thread alive; dropping it stops hot reload.
pub struct WatchGuard {
    shared: Arc<Shared>,
    tx: SyncSender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for WatchGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatchGuard").finish_non_exhaustive()
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        *self.shared.watch.lock() = None;
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch_error(message: String) -> SettingsError {
    SettingsError::error(None, "", message)
}

pub(crate) fn start(shared: &Arc<Shared>) -> Result<WatchGuard, SettingsError> {
    if shared.watch.lock().is_some() {
        return Err(watch_error("settings are already being watched".to_owned()));
    }
    if let Some(dir) = shared.user_dir() {
        create_private_dir(&dir)
            .map_err(|e| watch_error(format!("cannot create {}: {e}", dir.display())))?;
    }

    let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
    let event_tx = tx.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            // A full queue already has a reload pending; dropping is correct.
            Ok(_) => {
                let _ = event_tx.try_send(Msg::Changed);
            }
            Err(e) => tracing::warn!("settings watcher error: {e}"),
        }
    })
    .map_err(|e| watch_error(format!("cannot create file watcher: {e}")))?;
    *shared.watch.lock() = Some(WatchState {
        watcher,
        dirs: BTreeSet::new(),
    });
    rewatch(shared);

    let weak = Arc::downgrade(shared);
    let thread = std::thread::Builder::new()
        .name("hutan-settings-watch".to_owned())
        .spawn(move || run(&rx, &weak, SAFETY_POLL))
        .map_err(|e| watch_error(format!("cannot spawn watcher thread: {e}")))?;

    // Anything edited between `SettingsStore::new` and now.
    shared.reload_from_disk();
    Ok(WatchGuard {
        shared: Arc::clone(shared),
        tx,
        thread: Some(thread),
    })
}

fn run(rx: &mpsc::Receiver<Msg>, store: &Weak<Shared>, poll: Duration) {
    loop {
        match rx.recv_timeout(poll) {
            Ok(Msg::Changed) => {}
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                let Some(shared) = store.upgrade() else {
                    return;
                };
                let mut changed = shared.reload_from_disk();
                if rewatch(&shared) {
                    changed |= shared.reload_from_disk();
                }
                if changed {
                    tracing::debug!("settings changed on disk and the platform did not report it");
                }
                continue;
            }
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
        let Some(shared) = store.upgrade() else {
            return;
        };
        shared.reload_from_disk();
        if rewatch(&shared) {
            // Edits made before the new directory was watched produced no events.
            shared.reload_from_disk();
        }
    }
}

/// Brings the watched directory set in line with the current paths (workspace root changes,
/// `.hutan` appearing). No-op when not watching. Returns whether a directory was newly watched.
pub(crate) fn rewatch(shared: &Shared) -> bool {
    let wanted = shared.watch_dirs();
    let mut guard = shared.watch.lock();
    let Some(state) = guard.as_mut() else {
        return false;
    };
    let mut added = false;
    for dir in state.dirs.difference(&wanted).cloned().collect::<Vec<_>>() {
        // The directory may already be gone; nothing to unwatch then.
        let _ = state.watcher.unwatch(&dir);
        state.dirs.remove(&dir);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SettingsStore;

    /// The platform may report nothing (FSEvents under load, or just after the watch started):
    /// a file that changes with no event at all is still read by the safety poll.
    #[test]
    fn a_change_the_platform_never_reports_is_found_by_the_safety_poll() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::SettingsPaths::in_dir(dir.path());
        let store = SettingsStore::new(paths.clone());
        assert!(store.errors().is_empty());
        // The worker as `start` runs it, but with no watcher behind the channel: no event can
        // ever arrive, and the poll is short.
        let (tx, rx) = mpsc::sync_channel::<Msg>(QUEUE);
        let weak = Arc::downgrade(&store.shared_for_tests());
        let worker = std::thread::spawn(move || run(&rx, &weak, Duration::from_millis(20)));

        std::fs::write(&paths.user, "[no_such_namespace]\nx = 1\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while store.errors().is_empty() {
            assert!(Instant::now() < deadline, "the write was never read");
            std::thread::sleep(Duration::from_millis(10));
        }
        tx.send(Msg::Stop).unwrap();
        worker.join().unwrap();
    }
}
