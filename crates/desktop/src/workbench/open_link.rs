//! ⌘-click on a link in a terminal: the one place that decides what opens it.
//!
//! A terminal view finds the link (an OSC 8 hyperlink, a URL or an existing `path[:line[:col]]`
//! in the text) and emits [`TerminalViewEvent::OpenLink`]; the workbench hands it to
//! [`open_link`]. When the built-in editor arrives (`ItemRef::File`), this is the function that
//! opens a file at its line in a tab instead of the system.

use std::path::Path;
use std::rc::Rc;

use gpui::{App, Context, Global};
use hutan_terminal_view::LinkTarget;

use super::view::Workbench;
use crate::settings::OpenFilesWith;

/// How a file is handed to the system.
pub type OpenFile = Rc<dyn Fn(&Path, OpenFilesWith)>;

/// Replaces how files are handed to the system (tests: nothing must launch a real application).
pub struct FileOpener(pub OpenFile);

impl std::fmt::Debug for FileOpener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileOpener")
    }
}

impl Global for FileOpener {}

/// Opens `target`: URLs in the browser, files as `with` says. The line and column a file link
/// carries are for the built-in editor; the system cannot be told a line.
pub fn open_link(target: &LinkTarget, with: OpenFilesWith, cx: &mut App) {
    match target {
        LinkTarget::Url(url) => cx.open_url(url),
        LinkTarget::File { path, .. } => {
            if let Some(opener) = cx.try_global::<FileOpener>() {
                let opener = opener.0.clone();
                opener(path, with);
                return;
            }
            match with {
                OpenFilesWith::System => cx.open_with_system(path),
                OpenFilesWith::Editor => open_in_text_editor(path),
            }
        }
    }
}

/// `open -t`: the system's default text editor. The result is not waited for on this thread.
fn open_in_text_editor(path: &Path) {
    match std::process::Command::new("open")
        .arg("-t")
        .arg("--")
        .arg(path)
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait().ok());
        }
        Err(error) => tracing::warn!("cannot open {}: {error}", path.display()),
    }
}

impl Workbench {
    /// A ⌘-click in a terminal asked to open `target`.
    pub fn open_terminal_link(&mut self, target: &LinkTarget, cx: &mut Context<Self>) {
        if let LinkTarget::File { path, .. } = target
            && !path.exists()
        {
            self.set_notice(format!("{} no longer exists", path.display()), cx);
            return;
        }
        open_link(target, self.terminal_settings.open_files_with, cx);
    }
}
