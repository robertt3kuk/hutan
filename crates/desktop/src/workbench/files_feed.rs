//! What the workbench tells the Files panel's sections (EX-5): the files open in tabs (with
//! their unsaved state) and the files of the agent ledger. The panel observes the
//! [`ExplorerFeed`](super::panels::ExplorerFeed) and keeps the rows below its root; this side
//! only reads the workbench's own state, pushes it when it changed and closes tabs on request.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::{Context, Window};
use hutan_code_view::agent;

use super::agent_edits::{AgentLedger, LedgerEntry};
use super::panels::{AgentFile, OpenEditor};
use super::view::Workbench;

/// The places an agent changed in the file as it is on disk now, and the first one's line
/// (zero-based); `None` when nothing is left of its changes.
fn summarize(entry: &LedgerEntry) -> Option<(usize, u32)> {
    let hunks = agent::agent_hunks(&entry.base, &entry.agent, &entry.disk);
    let first = hunks.first()?;
    let line = entry.disk[..first.bytes.start.min(entry.disk.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    Some((hunks.len(), u32::try_from(line).unwrap_or(u32::MAX)))
}

impl Workbench {
    /// How many files the Files panel's feed knows are open (tests).
    pub fn files_feed_len(&self, cx: &gpui::App) -> usize {
        self.files_feed.read(cx).open().len()
    }

    /// The files of the center's tabs, tab order, each once, with their unsaved state.
    pub(crate) fn sync_explorer_open(&mut self, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        let open: Vec<OpenEditor> = self
            .open_file_items()
            .into_iter()
            .filter(|(_, path)| seen.insert(path.clone()))
            .map(|(_, path)| {
                let dirty = self
                    .buffers
                    .get(&path)
                    .is_some_and(|buffer| buffer.read(cx).is_dirty());
                OpenEditor {
                    preview: self.is_preview(&path),
                    path: PathBuf::from(path),
                    dirty,
                }
            })
            .collect();
        let active = self.active_center_file().map(PathBuf::from);
        self.files_feed
            .update(cx, |feed, cx| feed.set_open(open, active, cx));
    }

    /// The ledger's files, newest first, with what changed in each (a diff only for an entry
    /// that changed since the last time).
    pub(crate) fn sync_explorer_agents(&mut self, cx: &mut Context<Self>) {
        let AgentLedger {
            ledger, summaries, ..
        } = &mut self.agent_ledger;
        let mut files = Vec::new();
        for (path, entry) in ledger.entries() {
            let key = (entry.updated_ms, entry.disk.len());
            let summary = match summaries.get(path) {
                Some((known, summary)) if *known == key => *summary,
                _ => {
                    let summary = summarize(entry);
                    summaries.insert(path.clone(), (key, summary));
                    summary
                }
            };
            let Some((hunks, line)) = summary else {
                continue;
            };
            let who = entry
                .harness
                .clone()
                .or_else(|| entry.title.clone())
                .unwrap_or_else(|| "An agent".to_owned());
            files.push(AgentFile {
                path: PathBuf::from(path),
                session: entry.session_id,
                who,
                hunks,
                line,
                updated_ms: entry.updated_ms,
            });
        }
        summaries.retain(|path, _| ledger.entry(path).is_some());
        files.sort_by(|a, b| {
            b.updated_ms
                .cmp(&a.updated_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        self.files_feed
            .update(cx, |feed, cx| feed.set_agents(files, cx));
    }

    /// Closes every tab that shows `path`, last position first (the last one of a file with
    /// unsaved changes asks).
    pub(crate) fn close_file_tabs(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut places: Vec<(super::model::PaneId, usize)> = self
            .state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.as_file() == Some(path))
                    .map(move |(index, _)| (pane.id, index))
            })
            .collect();
        places.reverse();
        for (pane, index) in places {
            self.close_item_checked(pane, index, window, cx);
        }
    }
}
