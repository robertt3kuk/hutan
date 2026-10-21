//! File tabs and the disk (slice E-6): saving through the daemon (`fs.write` with the version
//! the buffer was read as, so a file that changed meanwhile is refused with `conflict` and never
//! overwritten without being asked), autosave, watching the open files (`fs.watch`) so a clean
//! buffer takes a change on disk and a changed one shows the conflict bar, the bar's answers
//! (*Reload*, *Keep mine*), and *Save before closing?*.
//!
//! Our own save comes back as an `fs.changed` with origin `self`; it is not somebody else's
//! change and reloads nothing. A change with origin `agent` also enters the pending ledger of
//! agents' edits (E-10, `agent_edits.rs`), compared with the text the file had before.

use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};
use hutan_client::{ClientError, Notice};
use hutan_code_view::{BufferEdited, Conflict, EditKind, FileBuffer};
use hutan_core::SessionId;
use hutan_protocol::ErrorCode;
use hutan_protocol::fs::{ChangeKind, FileVersion, FsChange, FsOrigin, FsWriteBegin};
use tokio::sync::broadcast::error::RecvError;

use super::modal::{Modal, ModalChoice};
use super::model::{ItemRef, PaneId};
use super::view::Workbench;
use crate::settings::{Autosave, EditorSettings};

/// The version a `conflict` error says the file has now (`data.current`), and whether the file
/// is gone (`data.reason = "missing"`).
fn conflict_of(error: &ClientError) -> Option<(Option<FileVersion>, bool)> {
    let ClientError::Remote(body) = error else {
        return None;
    };
    if body.code != ErrorCode::Conflict {
        return None;
    }
    let data = body.data.as_ref();
    let current = data
        .and_then(|d| d.get("current"))
        .and_then(|v| serde_json::from_value::<FileVersion>(v.clone()).ok());
    let missing = data
        .and_then(|d| d.get("reason"))
        .and_then(serde_json::Value::as_str)
        == Some("missing");
    Some((current, missing))
}

fn is_missing(error: &ClientError) -> bool {
    matches!(error, ClientError::Remote(body) if body.code == ErrorCode::NotFound)
}

impl Workbench {
    /// What the store keeps an eye on for a new buffer: a flip of its dirty state redraws the
    /// tabs (the dot), and an edit schedules `editor.autosave = "after_delay"`.
    pub(crate) fn watch_buffer(
        &mut self,
        path: &str,
        buffer: &Entity<FileBuffer>,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let key = path.to_owned();
        let dirty = cx.observe(buffer, move |this, buffer, cx| {
            let dirty = buffer.read(cx).is_dirty();
            if let Some(stored) = this.buffers.buffers.get_mut(&key)
                && stored.dirty != dirty
            {
                stored.dirty = dirty;
                cx.notify();
            }
        });
        let key = path.to_owned();
        let edits = cx.subscribe(buffer, move |this, _, event: &BufferEdited, cx| {
            if event.kind != EditKind::Reload {
                this.schedule_autosave(&key, cx);
            }
        });
        vec![dirty, edits]
    }

    /// Takes the connection's `fs.changed` notices while some file is open.
    pub(crate) fn listen_for_file_changes(&mut self, cx: &mut Context<Self>) {
        if self.buffers.listener.is_some() {
            return;
        }
        let mut notices = self.sessions.read(cx).connection().notices();
        self.buffers.listener = Some(cx.spawn(async move |this, cx| {
            loop {
                let (changes, overflow) = match notices.recv().await {
                    Ok(Notice::FsChanged { changes, overflow }) => (changes, overflow),
                    Ok(_) => continue,
                    // Some were missed: look at every file again.
                    Err(RecvError::Lagged(_)) => (Vec::new(), true),
                    Err(RecvError::Closed) => break,
                };
                if this
                    .update(cx, |this, cx| this.on_files_changed(changes, overflow, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// The file of `path` was read (or reloaded) as `version`, with `text` when the caller has
    /// it: the next save expects that version, an agent's next write is compared with that
    /// text, the ledger's changes of the file reach its buffer, and the file is watched from
    /// now on.
    pub(crate) fn loaded(
        &mut self,
        path: &str,
        version: FileVersion,
        text: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = text {
            self.agent_ledger.ledger.disk_is(path, text);
        }
        let changes = self.ledger_changes(path);
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        stored.disk = Some(version);
        stored.conflict_disk = None;
        if let Some(text) = text {
            stored.disk_text =
                (text.len() <= super::agent_edits::MAX_TEXT_BYTES).then(|| Arc::from(text));
        }
        if let Some(changes) = changes {
            stored
                .buffer
                .update(cx, |b, cx| b.set_agent_changes(Some(changes), cx));
        }
        if stored.watched {
            return;
        }
        stored.watched = true;
        let picked = stored.picked;
        let connection = self.sessions.read(cx).connection().clone();
        let file = std::path::PathBuf::from(path);
        cx.background_spawn(async move {
            if let Err(error) = connection.fs_watch(&file, picked).await {
                tracing::debug!(path = %file.display(), %error, "fs.watch failed");
            }
        })
        .detach();
    }

    pub(crate) fn unwatch_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let connection = self.sessions.read(cx).connection().clone();
        let file = std::path::PathBuf::from(path);
        cx.background_spawn(async move { connection.fs_unwatch(&file).await })
            .detach();
    }

    // ---- saving ----------------------------------------------------------------------------

    /// ⌘S: writes the buffer of `path` through the daemon, expecting the version it was read
    /// as. A file that changed meanwhile is refused and the conflict bar shows; nothing is
    /// overwritten. `then_close`: the tab to close once the file is saved.
    pub(crate) fn save_file(
        &mut self,
        path: &str,
        then_close: Option<ItemRef>,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        let (editable, saving, conflict) = {
            let b = buffer.read(cx);
            (b.is_editable(), b.is_saving(), b.conflict().cloned())
        };
        if !editable || saving {
            return;
        }
        if conflict.is_some() {
            // The bar asks; ⌘S does not decide for the user.
            self.set_notice(
                "The file changed on disk: choose Reload or Keep Mine in the bar above it",
                cx,
            );
            return;
        }
        let expect = stored.disk;
        self.write_file(path, expect, false, then_close, cx);
    }

    /// *Keep mine*: writes the buffer over what the file became (the version seen when the
    /// conflict was found, so a newer change is still refused); a deleted file is created again.
    pub(crate) fn overwrite_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(stored) = self.buffers.buffers.get(path) else {
            return;
        };
        let deleted = matches!(stored.buffer.read(cx).conflict(), Some(Conflict::Deleted));
        let expect = if deleted { None } else { stored.conflict_disk };
        stored.buffer.update(cx, |b, cx| b.set_conflict(None, cx));
        self.write_file(path, expect, deleted, None, cx);
    }

    fn write_file(
        &mut self,
        path: &str,
        expect: Option<FileVersion>,
        create: bool,
        then_close: Option<ItemRef>,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        let picked = stored.picked;
        stored.autosave = None;
        let (version, bytes) = buffer.update(cx, |b, cx| {
            b.set_saving(true, cx);
            b.bytes_to_save()
        });
        let connection = self.sessions.read(cx).connection().clone();
        let file = std::path::PathBuf::from(path);
        let request = FsWriteBegin {
            path: file.clone(),
            expect,
            create,
            size_hint: None,
            picked,
        };
        let size = bytes.len() as u64;
        let text: Option<Arc<str>> =
            (bytes.len() <= super::agent_edits::MAX_TEXT_BYTES).then(|| Arc::from(bytes.as_str()));
        let write = cx.background_spawn(async move { connection.fs_write(request, &bytes).await });
        let key = path.to_owned();
        stored.save = Some(cx.spawn(async move |this, cx| {
            let written = write.await;
            this.update(cx, |this, cx| {
                this.saved(&key, version, size, text, written, then_close, cx);
            })
            .ok();
        }));
    }

    #[allow(clippy::too_many_arguments)]
    fn saved(
        &mut self,
        path: &str,
        version: u64,
        size: u64,
        text: Option<Arc<str>>,
        written: Result<hutan_protocol::fs::FsWritten, ClientError>,
        then_close: Option<ItemRef>,
        cx: &mut Context<Self>,
    ) {
        if written.is_ok()
            && let Some(text) = &text
        {
            self.agent_ledger.ledger.disk_is(path, text);
        }
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        stored.save = None;
        match written {
            Ok(written) => {
                stored.disk = Some(written.version);
                stored.conflict_disk = None;
                stored.disk_text = text;
                buffer.update(cx, |b, cx| b.saved(version, size, cx));
                if let Some(item) = then_close
                    && let Some((pane, index)) = self.state.find_item(&item)
                {
                    self.try_mutate(cx, |s| s.close_item(pane, index));
                }
            }
            Err(error) => {
                buffer.update(cx, |b, cx| b.set_saving(false, cx));
                match conflict_of(&error) {
                    Some((current, missing)) => {
                        tracing::info!(path, "save refused: the file changed on disk");
                        self.conflicted(path, current, missing, cx);
                    }
                    None => {
                        let name = buffer.read(cx).file_name();
                        self.set_notice(
                            format!("Cannot save {name}: {}", super::files::error_text(&error)),
                            cx,
                        );
                    }
                }
            }
        }
    }

    /// The file on disk is not what the buffer was read from and the buffer has changes of its
    /// own: reads what the file is now (for *Show diff* and *Reload*) and shows the bar.
    fn conflicted(
        &mut self,
        path: &str,
        current: Option<FileVersion>,
        missing: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        stored.conflict_disk = current;
        if missing {
            buffer.update(cx, |b, cx| b.set_conflict(Some(Conflict::Deleted), cx));
            return;
        }
        buffer.update(cx, |b, cx| {
            b.set_conflict(Some(Conflict::Changed { disk: None }), cx);
        });
        let picked = stored.picked;
        let connection = self.sessions.read(cx).connection().clone();
        let file = std::path::PathBuf::from(path);
        let read = cx.background_spawn(async move { connection.fs_read_all(&file, picked).await });
        let key = path.to_owned();
        stored.check = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| {
                let Some(stored) = this.buffers.buffers.get_mut(&key) else {
                    return;
                };
                let buffer = stored.buffer.clone();
                match read {
                    Ok(read) => {
                        stored.conflict_disk = Some(read.version);
                        buffer.update(cx, |b, cx| {
                            if b.conflict().is_some() {
                                let disk: Arc<str> = read.text.into();
                                b.set_conflict(Some(Conflict::Changed { disk: Some(disk) }), cx);
                            }
                        });
                    }
                    Err(error) if is_missing(&error) => {
                        stored.conflict_disk = None;
                        buffer.update(cx, |b, cx| b.set_conflict(Some(Conflict::Deleted), cx));
                    }
                    Err(error) => {
                        tracing::debug!(path = key, %error, "reading the changed file failed");
                    }
                }
            })
            .ok();
        }));
    }

    /// *Reload*: the buffer takes the file as it is on disk now (one undo step: ⌘Z brings the
    /// buffer's own text back).
    pub(crate) fn reload_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(stored) = self.buffers.buffers.get(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        if let Some(Conflict::Changed { disk: Some(text) }) = buffer.read(cx).conflict().cloned()
            && let Some(version) = stored.conflict_disk
        {
            buffer.update(cx, |b, cx| {
                b.reload(&text, cx);
            });
            self.loaded(path, version, Some(&text), cx);
            return;
        }
        self.read_into(path, true, None, cx);
    }

    /// Reads the file of `path` and gives it to the buffer when the buffer is clean (or
    /// `force`); a changed buffer gets the conflict bar instead. `agent`: an agent's session
    /// wrote it (E-10: the write enters the ledger).
    fn read_into(
        &mut self,
        path: &str,
        force: bool,
        agent: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let picked = stored.picked;
        let connection = self.sessions.read(cx).connection().clone();
        let file = std::path::PathBuf::from(path);
        let read = cx.background_spawn(async move { connection.fs_read_all(&file, picked).await });
        let key = path.to_owned();
        stored.check = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| this.took_disk(&key, read, force, agent, cx))
                .ok();
        }));
    }

    fn took_disk(
        &mut self,
        path: &str,
        read: Result<hutan_client::FsText, ClientError>,
        force: bool,
        agent: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let buffer = stored.buffer.clone();
        stored.check = None;
        let read = match read {
            Ok(read) => read,
            Err(error) if is_missing(&error) => {
                stored.conflict_disk = None;
                buffer.update(cx, |b, cx| b.set_conflict(Some(Conflict::Deleted), cx));
                return;
            }
            Err(error) => {
                tracing::debug!(path, %error, "reading the changed file failed");
                return;
            }
        };
        if stored.disk == Some(read.version) && !force {
            return;
        }
        // What the file was before this change (an agent's write is compared with it).
        let before = stored.disk_text.clone();
        let (dirty, saving) = {
            let b = buffer.read(cx);
            (b.is_dirty(), b.is_saving())
        };
        if (dirty || saving) && !force {
            // Never over the user's unsaved text: the bar asks. The agent's write still enters
            // the ledger; its hunks show once the buffer takes the file (*Reload*).
            stored.conflict_disk = Some(read.version);
            let disk: Arc<str> = read.text.as_str().into();
            buffer.update(cx, |b, cx| {
                b.set_conflict(Some(Conflict::Changed { disk: Some(disk) }), cx);
            });
            if let Some(session) = agent {
                let changes =
                    self.ledger_agent_wrote(path, session, before.as_deref(), &read.text, cx);
                buffer.update(cx, |b, cx| b.set_agent_changes(changes, cx));
            }
            return;
        }
        let at = buffer.update(cx, |b, cx| {
            b.reload(&read.text, cx).then(|| b.last_reload()).flatten()
        });
        if let Some(session) = agent {
            let changes = self.ledger_agent_wrote(path, session, before.as_deref(), &read.text, cx);
            buffer.update(cx, |b, cx| match changes {
                Some(changes) => b.agent_wrote(changes, at.map(|r| r.start), cx),
                None => b.set_agent_changes(None, cx),
            });
        }
        self.loaded(path, read.version, Some(&read.text), cx);
    }

    /// `fs.changed`: a change of somebody else to an open file reloads a clean buffer and puts
    /// the bar over a changed one; our own save (origin `self`) is not a change; `overflow` (or
    /// a reconnect) checks every open file. An agent's write also enters the pending ledger
    /// (E-10), for open files and for the files the ledger watches.
    pub(crate) fn on_files_changed(
        &mut self,
        changes: Vec<FsChange>,
        overflow: bool,
        cx: &mut Context<Self>,
    ) {
        self.buffers.changes_seen += 1;
        self.ledger_files_changed(&changes, cx);
        let mut touched: Vec<(String, Option<SessionId>, bool)> = Vec::new();
        for change in changes {
            let agent = match change.origin {
                Some(FsOrigin::Own { .. }) => continue,
                Some(FsOrigin::Agent { session_id, .. }) => Some(session_id),
                // A restore of files (protocol 0.38) is nobody's agent: like an outside change.
                Some(FsOrigin::External | FsOrigin::Restore { .. }) | None => None,
            };
            let key = change.path.display().to_string();
            let Some(stored) = self.buffers.buffers.get(&key) else {
                continue;
            };
            if !stored.buffer.read(cx).is_ready() {
                continue;
            }
            let removed = change.kind == ChangeKind::Removed;
            if !removed && change.version.is_some() && change.version == stored.disk {
                continue;
            }
            touched.push((key, agent, removed));
        }
        if overflow {
            for (key, stored) in &self.buffers.buffers {
                if stored.buffer.read(cx).is_ready() && !touched.iter().any(|t| &t.0 == key) {
                    touched.push((key.clone(), None, false));
                }
            }
        }
        for (path, agent, removed) in touched {
            if removed {
                if let Some(stored) = self.buffers.buffers.get_mut(&path) {
                    stored.conflict_disk = None;
                    stored
                        .buffer
                        .update(cx, |b, cx| b.set_conflict(Some(Conflict::Deleted), cx));
                }
                continue;
            }
            self.read_into(&path, false, agent, cx);
        }
    }

    // ---- autosave --------------------------------------------------------------------------

    fn schedule_autosave(&mut self, path: &str, cx: &mut Context<Self>) {
        let settings = self.settings.get::<EditorSettings>();
        if settings.autosave != Autosave::AfterDelay {
            return;
        }
        let Some(stored) = self.buffers.buffers.get_mut(path) else {
            return;
        };
        let delay = Duration::from_millis(settings.autosave_delay_ms.max(100));
        let key = path.to_owned();
        stored.autosave = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| this.autosave(&key, cx)).ok();
        }));
    }

    /// Saves by itself only what is changed, free of conflicts, and not already being saved.
    fn autosave(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(stored) = self.buffers.buffers.get(path) else {
            return;
        };
        let b = stored.buffer.read(cx);
        if b.is_dirty() && b.conflict().is_none() && !b.is_saving() {
            self.save_file(path, None, cx);
        }
    }

    pub(crate) fn autosave_on_focus_change(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.settings.get::<EditorSettings>().autosave == Autosave::OnFocusChange {
            self.autosave(path, cx);
        }
    }

    // ---- closing ---------------------------------------------------------------------------

    /// Closes the tab at `index` of `pane`; the last tab of a file with unsaved changes asks
    /// *Save before closing?* first.
    pub(crate) fn close_item_checked(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .cloned();
        if let Some(path) = item.as_ref().and_then(ItemRef::as_file)
            && self.closing_loses_changes(path, cx)
        {
            let path = path.to_owned();
            self.ask_save_before_closing(&path, window, cx);
            return;
        }
        self.try_mutate(cx, |s| s.close_item(pane, index));
    }

    /// ⌘W: as [`close_item_checked`](Self::close_item_checked) for the focused tab; a panel
    /// closes as before.
    pub(crate) fn close_active_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state.focus, super::model::FocusTarget::Pane(_)) {
            let pane = self.state.active_pane_id();
            let active = self
                .state
                .pane(pane)
                .filter(|p| p.active < p.items.len())
                .map(|p| p.active);
            if let Some(index) = active {
                self.close_item_checked(pane, index, window, cx);
                return;
            }
        }
        self.mutate(cx, |s| {
            s.close_active_item();
        });
    }

    /// Whether closing one tab of `path` drops a buffer with unsaved changes: it is the only
    /// tab of the file.
    pub(crate) fn closing_loses_changes(&self, path: &str, cx: &gpui::App) -> bool {
        let dirty = self
            .buffers
            .buffers
            .get(path)
            .is_some_and(|s| s.buffer.read(cx).is_dirty());
        if !dirty {
            return false;
        }
        let tabs = self
            .state
            .center
            .panes()
            .iter()
            .flat_map(|p| p.items.iter())
            .filter(|i| i.as_file() == Some(path))
            .count();
        tabs <= 1
    }

    fn ask_save_before_closing(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = hutan_code_view::file_name(std::path::Path::new(path));
        let item = ItemRef::file(path.to_owned());
        let save_path = path.to_owned();
        let save_item = item.clone();
        let discard_item = item;
        self.open_modal(
            Modal {
                title: format!("Save changes to {name}?").into(),
                message: "Your changes are lost if you close the file without saving them.".into(),
                confirm_label: "Save".into(),
                input: None,
                secure_input: false,
                choice: Some(ModalChoice {
                    label: "Don't Save".into(),
                    run: std::rc::Rc::new(move |this, _, _, cx| {
                        if let Some((pane, index)) = this.state.find_item(&discard_item) {
                            this.try_mutate(cx, |s| s.close_item(pane, index));
                        }
                    }),
                }),
                checkbox: None,
                on_confirm: std::rc::Rc::new(move |this, _, _, cx| {
                    this.save_file(&save_path, Some(save_item.clone()), cx);
                }),
            },
            window,
            cx,
        );
    }
}
