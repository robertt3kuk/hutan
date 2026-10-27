//! The Trash behind `fs.trash`, and the undo tickets `fs.restore` takes (protocol 0.54).
//!
//! [`SystemTrash`] is `NSFileManager trashItemAtURL:resultingItemURL:error:` (ADR 0012
//! amendment 2026-10-27, decision 8): the item goes where Finder's Trash is, per volume, and the
//! call says where it went, which is what a ticket needs. [`FolderTrash`] is a Trash that is a
//! plain folder, for tests. A backend never deletes.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::confine::FsError;

/// Somewhere to move files to instead of deleting them.
pub trait Trash: std::fmt::Debug + Send + Sync {
    /// Moves `path` (canonical, it exists) to the Trash and returns where it is now.
    /// `unsupported` with `data.reason` `no_trash` when this volume has none.
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError>;
}

fn no_trash(path: &Path, why: &str) -> FsError {
    FsError::unsupported(
        "no_trash",
        format!("{} cannot be moved to the Trash: {why}", path.display()),
    )
}

/// The user's Trash, through `NSFileManager`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemTrash;

#[cfg(target_os = "macos")]
impl Trash for SystemTrash {
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError> {
        use objc2_foundation::{NSFileManager, NSString, NSURL};

        /// `NSFeatureUnsupportedError`, `NSFileWriteVolumeReadOnlyError`: no Trash to move to.
        const NO_TRASH_CODES: [isize; 2] = [3328, 642];

        let shown = path.display().to_string();
        let Some(text) = path.to_str() else {
            return Err(FsError::bad(format!("{shown} is not a UTF-8 path")));
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        let mut resulting = None;
        match NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
        {
            Ok(()) => resulting
                .and_then(|url| url.path())
                .map(|trashed| PathBuf::from(trashed.to_string()))
                .ok_or_else(|| FsError::Io {
                    context: shown,
                    source: std::io::Error::other("the Trash did not say where the item went"),
                }),
            Err(error) => {
                let why = error.localizedDescription().to_string();
                if NO_TRASH_CODES.contains(&error.code()) {
                    Err(no_trash(path, &why))
                } else {
                    Err(FsError::Io {
                        context: shown,
                        source: std::io::Error::other(why),
                    })
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Trash for SystemTrash {
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError> {
        Err(no_trash(path, "this platform has no system Trash"))
    }
}

/// A Trash that is a folder: every item goes into a folder of its own below `dir`. What tests
/// inject, so that none of them touches the user's Trash.
#[derive(Debug)]
pub struct FolderTrash {
    dir: PathBuf,
    count: AtomicU64,
}

impl FolderTrash {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            count: AtomicU64::new(0),
        }
    }
}

impl Trash for FolderTrash {
    fn trash(&self, path: &Path) -> Result<PathBuf, FsError> {
        let shown = path.display().to_string();
        let name = path
            .file_name()
            .ok_or_else(|| FsError::bad(format!("{shown} has no name")))?;
        let slot = self
            .dir
            .join(self.count.fetch_add(1, Ordering::Relaxed).to_string());
        std::fs::create_dir_all(&slot).map_err(|e| FsError::io(shown.clone(), e))?;
        let trashed = slot.join(name);
        std::fs::rename(path, &trashed).map_err(|e| FsError::io(shown, e))?;
        Ok(trashed)
    }
}

/// One item of a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketItem {
    /// Canonical, where it was.
    pub original: PathBuf,
    /// Where the Trash put it.
    pub trashed: PathBuf,
}

#[derive(Debug)]
struct Ticket {
    id: String,
    items: Vec<TicketItem>,
    issued: Instant,
}

/// The undo tickets of `fs.trash`, in memory: the newest `max`, each for `ttl`.
#[derive(Debug)]
pub struct Tickets {
    list: VecDeque<Ticket>,
    max: usize,
    ttl: Duration,
}

impl Tickets {
    pub fn new(max: usize, ttl: Duration) -> Self {
        Self {
            list: VecDeque::new(),
            max,
            ttl,
        }
    }

    fn sweep(&mut self) {
        let ttl = self.ttl;
        self.list.retain(|ticket| ticket.issued.elapsed() < ttl);
    }

    /// Remembers `items` and returns their ticket; the oldest ticket goes when there are too
    /// many.
    pub fn issue(&mut self, items: Vec<TicketItem>) -> String {
        self.sweep();
        while self.list.len() >= self.max.max(1) {
            self.list.pop_front();
        }
        let id = ulid::Ulid::generate().to_string();
        self.list.push_back(Ticket {
            id: id.clone(),
            items,
            issued: Instant::now(),
        });
        id
    }

    /// The items still to restore of ticket `id`; `None` when it is unknown, used up or
    /// expired.
    pub fn items(&mut self, id: &str) -> Option<Vec<TicketItem>> {
        self.sweep();
        self.list
            .iter()
            .find(|ticket| ticket.id == id)
            .map(|ticket| ticket.items.clone())
    }

    /// `restored` came back: they leave the ticket, and a ticket with nothing left goes.
    pub fn settle(&mut self, id: &str, restored: &[PathBuf]) {
        if let Some(ticket) = self.list.iter_mut().find(|ticket| ticket.id == id) {
            ticket
                .items
                .retain(|item| !restored.contains(&item.original));
        }
        self.list.retain(|ticket| !ticket.items.is_empty());
    }
}
