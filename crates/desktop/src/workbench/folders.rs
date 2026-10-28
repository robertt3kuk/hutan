//! Session folders in the window (SESSION-DIRS-B; protocol 0.57, capability `session.dirs`,
//! ADR 0023 decision 4; the spike's `mock-folders.svg`): the chip of a chat header and of a
//! terminal agent, the popover it opens (the working folder, each extra folder with how far
//! the agent has taken it, *Add Folder…*, remove, *Restart with this folder*), the ＋ menu's
//! *Add Folder to the Focused Session…* and the session menu's *Folders…*.
//!
//! The daemon decides and says what happened (`ChatDir.applied`); the window shows it in words
//! (`sessions::folders`) and never pretends: a folder a running Claude Code chat cannot take
//! says *needs a restart* until the user restarts it, a terminal agent that was typed
//! `/add-dir <path>` says *press Return there to confirm*. Adding and removing are user-only
//! on the daemon; this window is the user.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window,
    div, prelude::*, px,
};
use hutan_core::SessionId;

use super::actions;
use super::regions::Regions;
use super::render::probe;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::ui::{self, Button, ButtonKind, IconButton, StyledExt as _};
use super::view::{MenuEntry, Region, Workbench};
use super::workspaces::tilde;
use crate::icons::{Icon, icon};
use crate::sessions::folders::{Folder, SessionFolders, name_of};
use crate::theme::{DensityExt as _, Theme, Tokens};

const WIDTH: f32 = 440.0;

/// The folders popover: which session, where it hangs, what the last action said.
#[derive(Debug)]
pub(crate) struct FoldersControl {
    pub(crate) focus: FocusHandle,
    open: Option<Open>,
}

#[derive(Debug, Clone)]
struct Open {
    session: SessionId,
    /// Where the chip was clicked: the popover hangs below it.
    at: Point<Pixels>,
    /// False while the popover fades out (its state stays for the exit).
    shown: bool,
    /// The last thing that went wrong, said in the popover.
    problem: Option<String>,
    /// A request is in flight: its buttons wait.
    busy: bool,
}

impl FoldersControl {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self { focus, open: None }
    }
}

/// The chip of a session's folders: a ghost button with the folder glyph, the working folder's
/// name and, for the extra ones, a `+N` counter; a click opens the popover under it. `region`
/// is where tests find it; `compact` (a narrow header) leaves the name to the tooltip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn chip(
    id: &'static str,
    folders: &SessionFolders,
    session: SessionId,
    region: Region,
    regions: &Regions,
    compact: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> AnyElement {
    let label = if compact {
        folders.chip_count().unwrap_or_else(|| "Folders".to_owned())
    } else {
        folders.chip_name()
    };
    Button::new(id, label, theme, tokens)
        .ghost()
        .compact()
        .icon(Icon::ItemFolder)
        .max_width(200.0)
        .tooltip(folders.chip_tooltip())
        .when(!compact, |button| {
            button.children(
                folders
                    .chip_count()
                    .map(|count| ui::counter(count, theme, tokens).into_any_element()),
            )
        })
        .on_click(move |event, window, cx| {
            window.dispatch_action(
                Box::new(actions::ShowSessionFolders(actions::SessionMenuAt {
                    session: session.to_string(),
                    position: event.position(),
                })),
                cx,
            );
        })
        .child(probe(regions, region))
        .into_any_element()
}

impl Workbench {
    // ---- state ---------------------------------------------------------------------------------

    /// The session of the active pane's active tab (a chat or a terminal), whether or not the
    /// pane has the keyboard: the one *Add Folder to the Focused Session…* means from a menu
    /// that took the focus.
    pub fn shown_session(&self) -> Option<SessionId> {
        self.state
            .active_pane()
            .and_then(|pane| pane.active_item())
            .and_then(super::terminals::session_of_item)
    }

    /// The session whose folders popover is open.
    pub fn folders_popover_session(&self) -> Option<SessionId> {
        self.folders
            .open
            .as_ref()
            .filter(|open| open.shown)
            .map(|open| open.session)
    }

    /// What the popover says went wrong, if it does.
    pub fn folders_popover_problem(&self) -> Option<&str> {
        self.folders
            .open
            .as_ref()
            .filter(|open| open.shown)
            .and_then(|open| open.problem.as_deref())
    }

    /// What the open popover says, line by line: each folder with its words (`name · words`),
    /// the label of the restart button when it is offered, and the harness line.
    pub fn folders_popover_lines(&self, cx: &gpui::App) -> Vec<String> {
        let Some(open) = self.folders.open.as_ref().filter(|open| open.shown) else {
            return Vec::new();
        };
        let Some(folders) = self.sessions.read(cx).session_folders(open.session) else {
            return Vec::new();
        };
        let mut lines: Vec<String> = folders
            .primary
            .iter()
            .map(|path| format!("{} · working directory", name_of(path)))
            .collect();
        lines.extend(
            folders
                .extras
                .iter()
                .map(|folder| format!("{} · {}", folder.name(), folders.words(folder))),
        );
        if let Some(label) = restart_label(&folders) {
            lines.push(label.to_owned());
        }
        lines.push(folders.how());
        lines
    }

    /// Opens the popover of `session`'s folders, hanging below `at`.
    pub fn show_session_folders(
        &mut self,
        session: SessionId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.read(cx).session_folders(session).is_none() {
            self.set_notice("This session has no tools to give folders to", cx);
            return;
        }
        self.palette = None;
        self.context_menu = None;
        self.folders.open = Some(Open {
            session,
            at,
            shown: true,
            problem: None,
            busy: false,
        });
        window.focus(&self.folders.focus, cx);
        cx.notify();
    }

    pub fn close_folders_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.folders.open.as_mut()
            && std::mem::take(&mut open.shown)
        {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    // ---- adding and removing ------------------------------------------------------------------

    /// *Add Folder…*: the system's folder panel, starting beside the session's own folder; the
    /// choice is added to `session`.
    pub fn add_folder_to_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(folders) = self.sessions.read(cx).session_folders(session) else {
            self.set_notice("This session has no tools to give folders to", cx);
            return;
        };
        let start = folders
            .primary
            .as_deref()
            .and_then(Path::parent)
            .or(folders.primary.as_deref())
            .map(Path::to_path_buf)
            .or_else(std::env::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let panel = self.mac.open_panel.clone();
        cx.spawn_in(window, async move |this, cx| {
            // AppKit is asked outside of any update of the window, as GPUI does with its own.
            let path = match panel.choose_folder(&start) {
                Some(chosen) => chosen.await.ok().flatten(),
                None => {
                    let Ok(chosen) = cx.update(|_, cx| {
                        cx.prompt_for_paths(PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: Some("Add".into()),
                        })
                    }) else {
                        return;
                    };
                    match chosen.await {
                        Ok(Ok(Some(paths))) => paths.into_iter().next(),
                        _ => None,
                    }
                }
            };
            if let Some(path) = path {
                this.update_in(cx, |this, window, cx| {
                    this.add_session_folder(session, path, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// *Add Folder to the Focused Session…* (the ＋ menu, ⌘K): the session in the active tab.
    pub fn add_folder_to_shown_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.shown_session() else {
            self.set_notice(
                "Open a chat or a terminal agent first: folders belong to a session",
                cx,
            );
            return;
        };
        if !self.sessions.read(cx).supports_session_dirs() {
            self.set_notice(
                "This daemon cannot give a session more folders (needs 0.57)",
                cx,
            );
            return;
        }
        if self.sessions.read(cx).session_folders(session).is_none() {
            self.set_notice("This session has no tools to give folders to", cx);
            return;
        }
        self.add_folder_to_session(session, window, cx);
    }

    /// `chat.add_dir` for `path`, then what the answer asks of the user: a Return in the
    /// terminal, a restart, or nothing.
    pub fn add_session_folder(
        &mut self,
        session: SessionId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_folders_busy(session, true);
        let added = path.clone();
        let task = self
            .sessions
            .update(cx, |store, cx| store.add_session_dir(session, path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.set_folders_busy(session, false);
                match result {
                    Ok(_) => {
                        this.set_folders_problem(session, None);
                        this.folder_added(session, &added, window, cx);
                    }
                    Err(error) => {
                        let text = format!("Cannot add {}: {error}", name_of(&added));
                        this.folders_say(session, text, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `chat.remove_dir`.
    pub fn remove_session_folder(
        &mut self,
        session: SessionId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_folders_busy(session, true);
        let removed = path.clone();
        let task = self
            .sessions
            .update(cx, |store, cx| store.remove_session_dir(session, path, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.set_folders_busy(session, false);
                match result {
                    Ok(_) => this.set_folders_problem(session, None),
                    Err(error) => {
                        let text = format!("Cannot remove {}: {error}", name_of(&removed));
                        this.folders_say(session, text, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The daemon took `added`: for a terminal agent it was typed `/add-dir <path>` and waits
    /// for Return in the terminal (the popover closes and the terminal gets the keyboard); for
    /// a running session that only a restart gives the folder to, the restart is offered.
    fn folder_added(
        &mut self,
        session: SessionId,
        added: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(folders) = self.sessions.read(cx).session_folders(session) else {
            return;
        };
        let name = name_of(added);
        let state = folders
            .extras
            .iter()
            .find(|folder| folder_is(folder, added))
            .map(|folder| folder.applied);
        use hutan_protocol::chat::ChatDirApplied::{Paste, Restart};
        match state {
            Some(Paste) => {
                self.close_folders_popover(window, cx);
                self.open_session(session, window, cx);
                self.set_notice(
                    format!(
                        "Typed /add-dir {} at the prompt: press Return in the terminal to confirm",
                        added.display()
                    ),
                    cx,
                );
            }
            Some(Restart) if folders.needs_restart() => {
                self.confirm_restart_with_folders(session, window, cx);
            }
            _ => {
                self.set_notice(format!("Added {name} to this session"), cx);
            }
        }
    }

    /// *Restart with this folder*: asks first (the agent's process stops and starts again; a
    /// turn in progress ends), then stops the session and restarts it so that the agent is given
    /// every folder at its start.
    pub fn confirm_restart_with_folders(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.sessions.read(cx).session(session).cloned() else {
            return;
        };
        let Some(folders) = self.sessions.read(cx).session_folders(session) else {
            return;
        };
        let restart = folders.restart_folders();
        if restart.is_empty() {
            self.set_notice("Nothing needs a restart", cx);
            return;
        }
        let names: Vec<String> = restart.iter().map(|folder| folder.name()).collect();
        let what = if names.len() == 1 {
            "this folder"
        } else {
            "these folders"
        };
        let message = format!(
            "“{}” is running. {} takes {what} (“{}”) only when it starts, so Hutan stops it and starts it again. {}",
            info.title,
            match &folders.holder {
                crate::sessions::folders::Holder::Chat(agent) =>
                    super::harnesses::harness_name(agent).to_owned(),
                crate::sessions::folders::Holder::Terminal(_) => "The program".to_owned(),
            },
            names.join("”, “"),
            if info.chat.is_some() {
                "The conversation stays; a turn in progress ends."
            } else {
                "What the terminal shows now is gone."
            }
        );
        self.open_confirm(
            if names.len() == 1 {
                "Restart with this folder?"
            } else {
                "Restart with these folders?"
            },
            message,
            "Restart",
            move |this, window, cx| this.restart_with_folders(session, window, cx),
            window,
            cx,
        );
    }

    /// Stops `session`, then starts it again; both answers are the daemon's.
    fn restart_with_folders(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_folders_busy(session, true);
        let stopping = self
            .sessions
            .update(cx, |store, cx| store.stop(session, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                stopping.await?;
                let restarting = this
                    .update(cx, |this, cx| {
                        this.sessions
                            .update(cx, |store, cx| store.restart(session, cx))
                    })
                    .map_err(|error| error.to_string())?;
                restarting.await
            }
            .await;
            this.update(cx, |this, cx| {
                this.set_folders_busy(session, false);
                match result {
                    Ok(_) => this.set_folders_problem(session, None),
                    Err(error) => {
                        this.folders_say(
                            session,
                            format!("Cannot restart the session: {error}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_folders_busy(&mut self, session: SessionId, busy: bool) {
        if let Some(open) = self.folders.open.as_mut().filter(|o| o.session == session) {
            open.busy = busy;
        }
    }

    fn set_folders_problem(&mut self, session: SessionId, problem: Option<String>) {
        if let Some(open) = self.folders.open.as_mut().filter(|o| o.session == session) {
            open.problem = problem;
        }
    }

    /// Says a problem in the popover while it is open, else in the status line.
    fn folders_say(&mut self, session: SessionId, text: String, cx: &mut Context<Self>) {
        match self
            .folders
            .open
            .as_mut()
            .filter(|open| open.session == session && open.shown)
        {
            Some(open) => open.problem = Some(text),
            None => self.set_notice(text, cx),
        }
    }

    // ---- menus ---------------------------------------------------------------------------------

    /// *Folders…* of a session's menu: opens the popover where the menu was.
    pub(crate) fn folders_menu_entry(
        &self,
        session: SessionId,
        position: Point<Pixels>,
        cx: &gpui::App,
    ) -> Option<MenuEntry> {
        let store = self.sessions.read(cx);
        if !store.supports_session_dirs() {
            return None;
        }
        // The same sessions that show the chip: a chat, a coding agent in a terminal, anything
        // that holds folders. A plain shell gets one from the ＋ menu first.
        let folders = store.session_folders(session)?;
        if !folders.has_chip() {
            return None;
        }
        let label = match folders.chip_count() {
            Some(count) => format!("Folders… ({count})"),
            None => "Folders…".to_owned(),
        };
        Some(Self::entry(label, move |this, window, cx| {
            this.show_session_folders(session, position, window, cx);
        }))
    }

    /// The ＋ menu's *Add Folder to the Focused Session…*: greyed with the reason when no tab
    /// shows a session that can take one.
    pub(crate) fn add_folder_to_session_entry(&self, cx: &gpui::App) -> MenuEntry {
        const LABEL: &str = "Add Folder to the Focused Session…";
        let store = self.sessions.read(cx);
        let reason = match self.shown_session() {
            None => Some("no chat or terminal in view"),
            Some(_) if !store.supports_session_dirs() => Some("the daemon is older than 0.57"),
            Some(session) if store.session_folders(session).is_none() => {
                Some("this session has no tools")
            }
            Some(_) => None,
        };
        match reason {
            Some(reason) => Self::disabled_entry(format!("{LABEL} ({reason})")),
            None => Self::entry(LABEL, |this, window, cx| {
                this.add_folder_to_shown_session(window, cx);
            }),
        }
    }

    // ---- the popover ---------------------------------------------------------------------------

    pub(crate) fn render_folders_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let wanted = self.folders.open.as_ref().is_some_and(|open| open.shown);
        let wanted = self
            .overlays
            .track("folders-popover", wanted, motion::Kind::Popover, cx)?;
        let open = self.folders.open.clone()?;
        let folders = self.sessions.read(cx).session_folders(open.session)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let session = open.session;
        let busy = open.busy;

        let viewport = window.viewport_size();
        let left = (f32::from(open.at.x) - 28.0)
            .min(f32::from(viewport.width) - WIDTH - 8.0)
            .max(8.0);
        // Under the chip of a header; above the chip of a terminal at the foot of the pane.
        let above = f32::from(open.at.y) > f32::from(viewport.height) / 2.0;
        let anchor = if above {
            (f32::from(viewport.height) - f32::from(open.at.y) + 10.0).max(8.0)
        } else {
            (f32::from(open.at.y) + 14.0).max(8.0)
        };

        let folder_row = |index: Option<usize>,
                          path: &Path,
                          subtitle: Option<(&'static str, bool)>,
                          region_row: usize| {
            let name = SharedString::from(name_of(path));
            let shown_path = SharedString::from(tilde(path));
            let trailing: AnyElement = match index {
                None => ui::pill("working directory", &theme, &tokens).into_any_element(),
                Some(index) => {
                    let target = path.to_path_buf();
                    IconButton::new(
                        SharedString::from(format!("folder-remove-{index}")),
                        Icon::ActionClose,
                        "Remove from this session",
                        &theme,
                        &tokens,
                    )
                    .compact()
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remove_session_folder(session, target.clone(), window, cx);
                    }))
                    .child(probe(&self.regions, Region::FolderRow("remove", index)))
                    .into_any_element()
                }
            };
            div()
                .id(SharedString::from(format!("folder-row-{region_row}")))
                .relative()
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 2.0))
                .py(px(tokens.spacing))
                .rounded(px(tokens.radius))
                .hover(move |s| s.bg(hover))
                .child(
                    div()
                        .flex_none()
                        .child(icon(Icon::ItemFolder, 14.0, theme.muted)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.fg)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(name),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(shown_path),
                        )
                        .children(subtitle.map(|(words, waits)| {
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(if waits { theme.warning } else { theme.muted })
                                .child(words)
                        })),
                )
                .child(div().flex_none().child(trailing))
                .child(probe(&self.regions, Region::FolderRow("row", region_row)))
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(primary) = folders.primary.as_deref() {
            rows.push(folder_row(None, primary, None, 0).into_any_element());
        }
        for (index, folder) in folders.extras.iter().enumerate() {
            rows.push(
                folder_row(
                    Some(index),
                    &folder.path,
                    Some((folders.words(folder), folders.waits(folder))),
                    index + 1,
                )
                .into_any_element(),
            );
        }

        let add = Button::new("folders-add", "Add Folder…", &theme, &tokens)
            .disabled(busy)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.add_folder_to_session(session, window, cx);
            }))
            .child(probe(&self.regions, Region::Folders("add")));
        let restart = restart_label(&folders).map(|label| {
            Button::new("folders-restart", label, &theme, &tokens)
                .kind(ButtonKind::Primary)
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.confirm_restart_with_folders(session, window, cx);
                }))
                .child(probe(&self.regions, Region::Folders("restart")))
        });
        let full = folders.extras.len() >= hutan_protocol::chat::SESSION_DIRS_MAX;
        let problem = open.problem.clone().map(|text| {
            div()
                .id("folders-problem")
                .relative()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.danger)
                .child(SharedString::from(text))
                .child(probe(&self.regions, Region::Folders("problem")))
        });
        let how = SharedString::from(folders.how());
        let regions = self.regions.clone();

        Some(
            div()
                .id("folders-backdrop")
                .absolute()
                .inset_0()
                .when(wanted, |s| s.shield_behind())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_folders_popover(window, cx)),
                )
                .child(
                    div()
                        .id("folders-popover")
                        .key_context("FoldersPopover")
                        .track_focus(&self.folders.focus)
                        .absolute()
                        .left(px(left))
                        .when(above, |s| s.bottom(px(anchor)))
                        .when(!above, |s| s.top(px(anchor)))
                        .w(px(WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child("Folders in this session"),
                                )
                                .child(
                                    div()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.muted)
                                        .child("The agent can read and edit these. Relative paths resolve in the first."),
                                ),
                        )
                        .child(div().flex().flex_col().gap(px(2.0)).children(rows))
                        .children(problem)
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(tokens.spacing * 2.0))
                                .child(add.disabled(busy || full))
                                .children(restart)
                                .when(full, |s| {
                                    s.child(
                                        div()
                                            .text_size(px(tokens.secondary_font_size))
                                            .text_color(theme.muted)
                                            .child("8 folders is the most; take one out first"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("folders-how")
                                .relative()
                                .text_size(px(tokens.secondary_font_size))
                                .text_color(theme.muted)
                                .child(how)
                                .child(probe(&regions, Region::Folders("how"))),
                        )
                        .child(probe(&regions, Region::Folders("popover")))
                        .presented(
                            "folders-popover-motion",
                            motion::Kind::Popover,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .into_any_element(),
        )
    }
}

fn folder_is(folder: &Folder, path: &Path) -> bool {
    folder.path == path || std::fs::canonicalize(path).is_ok_and(|real| real == folder.path)
}

/// The restart button's label while some folder only a restart gives: *Restart with this
/// folder* (or *these folders*).
fn restart_label(folders: &SessionFolders) -> Option<&'static str> {
    match folders.restart_folders().len() {
        0 => None,
        1 => Some("Restart with this folder"),
        _ => Some("Restart with these folders"),
    }
}
