//! Workbench behavior around the session history index (M17-B): focusing the History panel,
//! the ⌘J `?` search, and what a hit does — Resume (a new terminal session running the
//! agent's resume command), Copy command, and Open block / Open session for Hutan hits.
//!
//! Resume starts in the active workspace when one of its folders holds the hit's directory;
//! otherwise `workspace.open` finds (or creates) the workspace that contains it and the window
//! switches there first. Home does not count as holding a directory unless it is that
//! directory: it is a catch-all, and a project under `~` belongs in its own workspace. A
//! recorded directory that is gone falls back to the active workspace's first folder (and says
//! so).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{ClipboardItem, Context, Entity, Window};
use hutan_client::ClientError;
use hutan_core::SessionId;
use hutan_protocol::ErrorCode;
use hutan_protocol::history::{HistoryHit, HistoryHits, HistorySource};
use hutan_protocol::messages::{TerminalSpec, WorkspaceInfo, WorkspaceKind};

use super::model::PanelId;
use super::panels::{
    HISTORY_DEBOUNCE, HISTORY_PICKER_LIMIT, HistoryFilters, HistoryPanel, HistoryResume,
    HutanTarget, age_label, hit_title, parse_snippet, short_path, source_icon, source_label,
};
use super::view::Workbench;
use super::workspaces::{HistoryState, PickerAction, PickerEntry};
use crate::sessions::SessionsStore;
use crate::settings::{HistorySettings, TerminalSettings};

/// How often (and how many times) Open block waits for the terminal view of a session it has
/// just shown before it gives up on scrolling to the block.
const JUMP_RETRY: Duration = Duration::from_millis(100);
const JUMP_TRIES: u32 = 50;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// True when `dir` is at or below one of `workspace`'s folders (Home only for its own folder).
fn holds(workspace: &WorkspaceInfo, dir: &Path) -> bool {
    workspace.roots.iter().any(|root| {
        if workspace.kind == WorkspaceKind::Home {
            dir == root
        } else {
            dir.starts_with(root)
        }
    })
}

/// A ⌘J row for `hit`: Enter resumes it when it can be, else shows it in the History panel.
fn picker_entry(hit: &HistoryHit, query: &str, home: Option<&Path>) -> PickerEntry {
    let mut detail = vec![source_label(hit.source).to_owned()];
    if let Some(cwd) = hit.cwd.as_deref().filter(|c| !c.is_empty()) {
        detail.push(short_path(cwd, home));
    }
    if let Some(ts) = hit.ts_ms {
        detail.push(age_label(ts));
    }
    let mut detail = detail.join(" · ");
    let snippet = parse_snippet(&hit.snippet).text;
    if !snippet.is_empty() {
        detail.push_str(" — ");
        detail.push_str(&snippet);
    }
    let action = match HistoryResume::of(hit) {
        Some(resume) => PickerAction::ResumeHit(resume),
        None => PickerAction::ShowHit {
            query: query.to_owned(),
            source: hit.source,
            key: hit.session_key.clone(),
        },
    };
    PickerEntry {
        icon: source_icon(hit.source).into(),
        title: hit_title(hit).into(),
        detail: detail.into(),
        action,
        fixed: false,
    }
}

impl Workbench {
    // ---- the History panel -------------------------------------------------------------------

    /// Reveals the History panel (in its default dock when it is closed) and returns it.
    pub(crate) fn history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<HistoryPanel>> {
        self.open_panel("history", cx);
        let view = self.panel_view(&PanelId::new("history"), window, cx)?;
        view.downcast::<HistoryPanel>().ok()
    }

    /// "Search history…": shows the panel and puts the cursor in its search field.
    pub fn search_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.history_panel(window, cx) else {
            self.set_notice("The History panel is not available", cx);
            return;
        };
        // The panel is drawn (and its field exists) only after this frame.
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| panel.focus_search(window, cx));
        });
    }

    /// Shows the History panel searching for `query`, with the hit of `source` / `key`
    /// selected once the answer is in.
    pub fn show_history_hit(
        &mut self,
        query: &str,
        source: HistorySource,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.history_panel(window, cx) else {
            return;
        };
        let (query, key) = (query.to_owned(), key.to_owned());
        cx.defer_in(window, move |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.show_hit(&query, source, &key, cx);
                panel.focus_search(window, cx);
            });
        });
    }

    // ---- the ⌘J `?` search ---------------------------------------------------------------------

    /// Searches the history for what follows the `?` of the open ⌘J palette (after the
    /// debounce) and fills its rows with the top hits.
    pub(crate) fn search_history_in_picker(&mut self, cx: &mut Context<Self>) {
        self.picker_search = None;
        let Some(picker) = &mut self.picker else {
            return;
        };
        let Some(query) = picker.history_query().map(str::to_owned) else {
            return;
        };
        if query.is_empty() {
            picker.history.clear();
            picker.history_state = HistoryState::Idle;
            picker.refilter();
            return;
        }
        let store = self.sessions.read(cx);
        let connection = store.connection().clone();
        if !connection.supports_history() {
            picker.history.clear();
            picker.history_state =
                if store.connection_state() == hutan_client::ConnectionState::Connected {
                    HistoryState::Unsupported
                } else {
                    HistoryState::Failed("the daemon is not connected".to_owned())
                };
            picker.refilter();
            return;
        }
        picker.history_state = HistoryState::Searching;
        let project = store
            .active_workspace()
            .or_else(|| store.home_workspace())
            .and_then(|w| w.roots.first().cloned());
        let filters = HistoryFilters::from_settings(&self.settings.get::<HistorySettings>());
        let request = filters.request(&query, HISTORY_PICKER_LIMIT, now_ms(), project.as_deref());
        self.picker_search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HISTORY_DEBOUNCE).await;
            let result = cx
                .background_executor()
                .spawn(async move { connection.history_search(request).await })
                .await;
            this.update(cx, |this, cx| this.fill_history_picker(&query, result, cx))
                .ok();
        }));
    }

    fn fill_history_picker(
        &mut self,
        query: &str,
        result: Result<HistoryHits, ClientError>,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        // The query moved on (or the palette changed mode) while the search was out.
        if picker.history_query() != Some(query) {
            return;
        }
        match result {
            Ok(found) => {
                let home = std::env::home_dir();
                picker.history = found
                    .hits
                    .iter()
                    .map(|hit| picker_entry(hit, query, home.as_deref()))
                    .collect();
                picker.history_state = HistoryState::Done;
            }
            Err(error) => {
                picker.history.clear();
                picker.history_state = match error {
                    ClientError::Remote(body) if body.code == ErrorCode::InvalidState => {
                        HistoryState::Disabled
                    }
                    other => HistoryState::Failed(other.to_string()),
                };
            }
        }
        picker.selected = 0;
        picker.refilter();
        cx.notify();
    }

    // ---- hit actions ---------------------------------------------------------------------------

    pub fn copy_history_command(&mut self, line: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(line.to_owned()));
        self.set_notice("Copied the resume command", cx);
    }

    /// Starts `resume.argv` in a new terminal session and shows it (see the module docs for the
    /// workspace and directory it starts in).
    pub fn resume_history_hit(
        &mut self,
        resume: HistoryResume,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let HistoryResume { argv, cwd, title } = resume;
        if argv.is_empty() {
            self.set_notice("This session has no resume command", cx);
            return;
        }
        let recorded = cwd.map(PathBuf::from);
        let dir = recorded.clone().filter(|dir| dir.is_dir());
        let (target, held) = {
            let store = self.sessions.read(cx);
            let target = self.target_workspace(store).cloned();
            let held = dir
                .as_deref()
                .is_some_and(|dir| target.as_ref().is_some_and(|w| holds(w, dir)));
            (target, held)
        };
        // Where the session starts, and whether `workspace.open` must find its workspace first.
        let (open, start_dir) = match (&dir, held) {
            (Some(dir), false) => (Some(dir.clone()), Some(dir.clone())),
            (Some(dir), true) => (None, Some(dir.clone())),
            (None, _) => (
                None,
                target
                    .as_ref()
                    .and_then(|w| w.roots.first().cloned())
                    .or_else(std::env::home_dir),
            ),
        };
        let gone = match (&recorded, &dir) {
            (Some(recorded), None) => Some(recorded.clone()),
            _ => None,
        };
        let mut spec = TerminalSpec {
            title: Some(match title {
                Some(title) => format!("resume: {title}"),
                None => argv.join(" "),
            }),
            command: argv,
            cwd: start_dir.clone(),
            ..TerminalSpec::default()
        };
        // The settings of the project the session will belong to: the folder's own when the
        // daemon has to find (or make) its project first.
        let (workspace_root, project_root) = match (&open, &target) {
            (Some(dir), _) => (None, Some(dir.clone())),
            (None, Some(target)) => {
                super::workspaces::layer_roots_of(self.sessions.read(cx), target.id)
            }
            (None, None) => (None, None),
        };
        self.settings
            .get_for_roots::<TerminalSettings>(workspace_root.as_deref(), project_root.as_deref())
            .apply_to_spec(&mut spec);
        let opening = open.map(|dir| {
            self.sessions
                .update(cx, |store, cx| store.open_workspace(dir, cx))
        });
        let target_id = target.map(|w| w.id);
        cx.spawn_in(window, async move |this, cx| {
            let workspace = match opening {
                Some(task) => match task.await {
                    Ok(opened) => {
                        let id = opened.workspace.id;
                        this.update_in(cx, |this, window, cx| {
                            // Its `workspace.updated` may still be on its way.
                            this.pending_workspace = Some(id);
                            this.switch_workspace(id, window, cx);
                        })
                        .ok();
                        Some(id)
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| {
                            this.set_notice(format!("Cannot resume: {error}"), cx);
                        })
                        .ok();
                        return;
                    }
                },
                None => target_id,
            };
            let created = this.update(cx, |this, cx| {
                this.sessions
                    .update(cx, |store, cx| store.create_session(workspace, spec, cx))
            });
            let Ok(created) = created else {
                return;
            };
            match created.await {
                Ok(info) => {
                    this.update_in(cx, |this, window, cx| {
                        this.open_session(info.id, window, cx);
                        if let Some(gone) = gone {
                            let at = start_dir.map_or_else(
                                || "the default directory".to_owned(),
                                |dir| dir.display().to_string(),
                            );
                            this.set_notice(
                                format!("{} no longer exists; resumed in {at}", gone.display()),
                                cx,
                            );
                        }
                    })
                    .ok();
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_notice(format!("Cannot resume: {error}"), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Reveals the transcript file of a hit of another tool in Finder.
    pub fn reveal_history_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = Path::new(path);
        if path.is_file() {
            (self.daemon.reveal)(path, cx);
        } else {
            self.set_notice(format!("{} no longer exists", path.display()), cx);
        }
    }

    /// Opens the transcript file of a session with the system (its default app).
    pub fn open_history_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = Path::new(path);
        if path.is_file() {
            (self.daemon.open)(path, cx);
        } else {
            self.set_notice(format!("{} no longer exists", path.display()), cx);
        }
    }

    /// Shows the workspace that holds `dir` (the most specific one when several do); when none
    /// does, `workspace.open` finds or creates one for the folder first.
    pub fn open_history_directory(
        &mut self,
        dir: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = PathBuf::from(dir);
        if !dir.is_dir() {
            self.set_notice(format!("{} no longer exists", dir.display()), cx);
            return;
        }
        let holder = self
            .sessions
            .read(cx)
            .workspaces()
            .iter()
            .filter(|w| holds(w, &dir))
            .max_by_key(|w| {
                w.roots
                    .iter()
                    .filter(|root| dir.starts_with(root))
                    .map(|root| root.components().count())
                    .max()
            })
            .map(|w| w.id);
        if let Some(id) = holder {
            self.switch_workspace(id, window, cx);
            return;
        }
        let opening = self
            .sessions
            .update(cx, |store, cx| store.open_workspace(dir, cx));
        cx.spawn_in(window, async move |this, cx| match opening.await {
            Ok(opened) => {
                let id = opened.workspace.id;
                this.update_in(cx, |this, window, cx| {
                    // Its `workspace.updated` may still be on its way.
                    this.pending_workspace = Some(id);
                    this.switch_workspace(id, window, cx);
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot open the folder: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Shows the Hutan session a hit points at and, for a block, scrolls its terminal to the
    /// block: the block the index recorded, by its id, in the terminal that ran it. A session
    /// whose terminal is another one (the daemon started it again since) has lost the block,
    /// and a block the terminal no longer keeps cannot be shown; both say so.
    pub fn open_history_hit(
        &mut self,
        target: HutanTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(session) = target.session.parse::<SessionId>() else {
            self.set_notice("That is not a session of this daemon", cx);
            return;
        };
        // A conversation with the assistant opens in the Assistant panel, not as a tab.
        if self.show_assistant_session(session, window, cx) {
            return;
        }
        let terminal = {
            let store: &SessionsStore = self.sessions.read(cx);
            match store.session(session) {
                Some(info) => info.terminal_id,
                None => {
                    self.set_notice("That terminal session no longer exists", cx);
                    return;
                }
            }
        };
        self.open_session(session, window, cx);
        let Some(block) = target.block else {
            return;
        };
        if terminal != Some(block.terminal_id) {
            self.set_notice("That block is no longer in the terminal", cx);
            return;
        }
        let block = block.block_id;
        cx.spawn_in(window, async move |this, cx| {
            // The terminal view exists once the session has been drawn, and knows its blocks a
            // moment after that.
            for _ in 0..JUMP_TRIES {
                let done = this.update(cx, |this, cx| {
                    this.terminal_view_of(session).is_some_and(|view| {
                        view.update(cx, |view, cx| view.jump_to_block(block, cx))
                    })
                });
                match done {
                    Ok(true) => return,
                    Ok(false) => cx.background_executor().timer(JUMP_RETRY).await,
                    Err(_) => return,
                }
            }
            this.update(cx, |this, cx| {
                this.set_notice("That block is no longer in the terminal", cx);
            })
            .ok();
        })
        .detach();
    }
}
