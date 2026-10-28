//! The preview tab (FX-WORK-2, N-03; `editor.preview_tabs`).
//!
//! A single click on a file in the Files tree (and Space on its row) shows it in a *preview
//! tab*: its title is in italics and the next file shown the same way takes its place, so
//! browsing a tree does not pile up tabs (or *Open Editors* rows) one per click. A double
//! click on the row or on the tab, an edit, *Keep Open* in the tab's menu, and opening the file
//! any other way (Return, ⌘P, an agent's card, a split) make it a regular tab. There is at
//! most one preview tab in a window; it is view state — not saved with the layout, so after a
//! restart every tab is a regular one.

use gpui::Context;

use super::model::ItemRef;
use super::view::Workbench;
use crate::settings::EditorSettings;

impl Workbench {
    /// The file of the preview tab, if one is open.
    pub fn preview_file(&self) -> Option<&str> {
        self.preview_tab
            .as_deref()
            .filter(|path| self.state.find_item(&ItemRef::file(*path)).is_some())
    }

    /// Whether the tab of `path` is the preview tab.
    pub fn is_preview(&self, path: &str) -> bool {
        self.preview_file() == Some(path)
    }

    /// The tab of `path` is a regular one from now on (no-op when it is not the preview).
    pub fn keep_open(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.preview_tab.as_deref() == Some(path) {
            self.preview_tab = None;
            self.sync_explorer_open(cx);
            cx.notify();
        }
    }

    /// Whether files shown by a single click go to the preview tab.
    pub(crate) fn preview_tabs_on(&self) -> bool {
        self.settings.get::<EditorSettings>().preview_tabs
    }

    /// The preview tab's file follows a rename or a move.
    pub(crate) fn rekey_preview(&mut self, from: &str, to: &str) {
        if self.preview_tab.as_deref() == Some(from) {
            self.preview_tab = Some(to.to_owned());
        }
    }

    /// Forgets a preview whose tab is gone.
    pub(crate) fn prune_preview(&mut self) {
        if self.preview_tab.is_some() && self.preview_file().is_none() {
            self.preview_tab = None;
        }
    }
}
