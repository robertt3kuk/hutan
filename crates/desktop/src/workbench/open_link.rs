//! ⌘-click on a link in a terminal: the one place that decides what opens it.
//!
//! A terminal view finds the link (an OSC 8 hyperlink, a URL or an existing `path[:line[:col]]`
//! in the text) and emits [`TerminalViewEvent::OpenLink`]; the workbench hands it to
//! [`Workbench::open_terminal_link`]. A URL opens in the browser; a file opens in Hutan's own
//! code view at its line (`terminal.open_files_with = "editor"`) or with its default application
//! (`system`, the default).

use std::path::Path;
use std::rc::Rc;

use gpui::{App, Context, Global, Window};
use hutan_terminal_view::LinkTarget;

use super::files::FileOpen;
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

/// Hands `path` to its default application (through the [`FileOpener`] when one is set).
pub fn open_with_system(path: &Path, cx: &mut App) {
    if let Some(opener) = cx.try_global::<FileOpener>() {
        let opener = opener.0.clone();
        opener(path, OpenFilesWith::System);
        return;
    }
    cx.open_with_system(path);
}

impl Workbench {
    /// A ⌘-click in a terminal asked to open `target`.
    pub fn open_terminal_link(
        &mut self,
        target: &LinkTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            LinkTarget::Url(url) => cx.open_url(url),
            LinkTarget::File { path, .. } if !path.exists() => {
                self.set_notice(format!("{} no longer exists", path.display()), cx);
            }
            LinkTarget::File { path, line, column } => {
                match self.terminal_settings.open_files_with {
                    OpenFilesWith::System => open_with_system(path, cx),
                    // The user clicked it: read it wherever it is.
                    OpenFilesWith::Editor => self.open_file(
                        FileOpen {
                            path: path.clone(),
                            line: *line,
                            column: *column,
                            picked: true,
                        },
                        window,
                        cx,
                    ),
                }
            }
        }
    }
}
