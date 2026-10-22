//! The Open panels of *File: Open File…* (E-13a), which starts in the active project's folder,
//! and of the first run's *Open a folder* (FX-E2E2-C), which starts at `~`. GPUI's own prompt
//! cannot be told where to start (it opens wherever the last one was), so on macOS the panel is
//! `NSOpenPanel` with its `directoryURL` set.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tokio::sync::oneshot;

/// The answer of an Open panel: the file chosen, `None` when it was cancelled.
pub type Chosen = oneshot::Receiver<Option<PathBuf>>;

/// Asks the user for one file.
pub trait OpenPanel {
    /// Shows a panel for one file that starts in `directory`. `None`: this host has no panel of
    /// its own, and the caller uses GPUI's prompt (which starts where the last one was).
    fn choose_file(&self, directory: &Path) -> Option<Chosen>;

    /// Shows a panel for one folder that starts in `directory`; `None` as for
    /// [`choose_file`](Self::choose_file).
    fn choose_folder(&self, directory: &Path) -> Option<Chosen>;
}

/// No panel of its own: GPUI's prompt is used (tests answer it with
/// `simulate_path_prompt_response`).
#[derive(Debug, Default, Clone, Copy)]
pub struct GpuiPrompt;

impl OpenPanel for GpuiPrompt {
    fn choose_file(&self, _directory: &Path) -> Option<Chosen> {
        None
    }

    fn choose_folder(&self, _directory: &Path) -> Option<Chosen> {
        None
    }
}

/// Records the folder every panel was asked to start in; the test answers the open one.
#[derive(Debug, Clone, Default)]
pub struct FakeOpenPanel {
    asked: Rc<RefCell<Vec<PathBuf>>>,
    asked_folders: Rc<RefCell<Vec<PathBuf>>>,
    pending: Rc<RefCell<Option<oneshot::Sender<Option<PathBuf>>>>>,
}

impl FakeOpenPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The folders the panels started in, oldest first.
    pub fn asked(&self) -> Vec<PathBuf> {
        self.asked.borrow().clone()
    }

    /// The folders the *folder* panels started in, oldest first.
    pub fn asked_folders(&self) -> Vec<PathBuf> {
        self.asked_folders.borrow().clone()
    }

    /// Answers the open panel with `file` (`None`: Cancel); false when none is open.
    pub fn answer(&self, file: Option<PathBuf>) -> bool {
        match self.pending.borrow_mut().take() {
            Some(sender) => sender.send(file).is_ok(),
            None => false,
        }
    }
}

impl OpenPanel for FakeOpenPanel {
    fn choose_file(&self, directory: &Path) -> Option<Chosen> {
        self.asked.borrow_mut().push(directory.to_owned());
        let (sender, receiver) = oneshot::channel();
        *self.pending.borrow_mut() = Some(sender);
        Some(receiver)
    }

    fn choose_folder(&self, directory: &Path) -> Option<Chosen> {
        self.asked_folders.borrow_mut().push(directory.to_owned());
        let (sender, receiver) = oneshot::channel();
        *self.pending.borrow_mut() = Some(sender);
        Some(receiver)
    }
}

/// `NSOpenPanel`, one file or one folder, starting in the folder.
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemOpenPanel;

#[cfg(target_os = "macos")]
impl OpenPanel for SystemOpenPanel {
    fn choose_file(&self, directory: &Path) -> Option<Chosen> {
        Self::show(directory, false)
    }

    fn choose_folder(&self, directory: &Path) -> Option<Chosen> {
        Self::show(directory, true)
    }
}

#[cfg(target_os = "macos")]
impl SystemOpenPanel {
    fn show(directory: &Path, folders: bool) -> Option<Chosen> {
        use std::cell::Cell;

        use block2::RcBlock;
        use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel};
        use objc2_foundation::{MainThreadMarker, NSString, NSURL};

        let Some(mtm) = MainThreadMarker::new() else {
            tracing::warn!("the Open panel was asked for off the main thread; GPUI's is used");
            return None;
        };
        let (sender, receiver) = oneshot::channel();
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(!folders);
        panel.setCanChooseDirectories(folders);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(false);
        panel.setPrompt(Some(&NSString::from_str("Open")));
        let start = NSURL::from_directory_path(directory);
        panel.setDirectoryURL(start.as_deref());
        let sender = Cell::new(Some(sender));
        let handler = RcBlock::new({
            let panel = panel.clone();
            move |response: NSModalResponse| {
                let Some(sender) = sender.take() else {
                    return;
                };
                let chosen = (response == NSModalResponseOK)
                    .then(|| {
                        panel
                            .URLs()
                            .iter()
                            .filter(|url| url.isFileURL())
                            .find_map(|url| url.to_file_path())
                    })
                    .flatten();
                // The window may be gone; nobody waits then.
                let _ = sender.send(chosen);
            }
        });
        panel.beginWithCompletionHandler(&handler);
        Some(receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_records_where_it_started_and_hands_over_the_answer() {
        let panel = FakeOpenPanel::new();
        assert!(!panel.answer(None), "nothing is open yet");
        let mut chosen = panel
            .choose_file(Path::new("/repo"))
            .expect("the fake has a panel");
        assert_eq!(panel.asked(), [PathBuf::from("/repo")]);
        assert!(panel.answer(Some(PathBuf::from("/repo/src/main.rs"))));
        assert_eq!(
            chosen.try_recv().ok(),
            Some(Some(PathBuf::from("/repo/src/main.rs")))
        );
        assert!(GpuiPrompt.choose_file(Path::new("/repo")).is_none());
    }
}
