//! Closing sessions and reopening them (protocol 0.30, capability `session.close`): the × of
//! the sidebar and the Agent Dashboard, "Close session…" in the menus, the confirmation when
//! something runs, the "Closed … · Undo" toast, and History's Reopen.

use std::rc::Rc;
use std::time::Duration;

use crate::theme::DensityExt as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
};
use hutan_core::SessionId;
use hutan_protocol::messages::SessionBusy;

use super::modal::{Modal, ModalChoice};
use super::model::{ItemRef, TermSplit};
use super::render::probe;
use super::ui::motion;
use super::ui::{Button, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::CloseFailure;
use crate::settings::CloseSessionMode;

/// How long the Undo toast stays.
pub const TOAST_TIME: Duration = Duration::from_secs(5);

/// The widest an Undo toast is: a long label is cut, never wrapped to a second line.
pub const UNDO_TOAST_MAX_WIDTH: f32 = 360.0;

/// "Closed <title> · Undo".
#[derive(Debug, Clone)]
pub struct CloseToast {
    pub session: SessionId,
    pub title: SharedString,
    /// The tab of terminal splits the session was in, as it stood: Undo puts the session back
    /// into it, in its place, instead of opening a tab of its own.
    split: Option<TermSplit>,
    rev: u64,
}

/// What the layout looked like when a session was closed, for what follows it.
struct ClosedFrom {
    /// The tab of terminal splits the session was in, as it stood.
    split: Option<TermSplit>,
    /// The session had the keyboard.
    was_focused: bool,
    /// The session the sidebar selects when it goes.
    neighbour: Option<SessionId>,
}

/// What the confirmation says runs in the session.
pub fn busy_text(busy: &SessionBusy) -> String {
    match (busy.state.as_str(), &busy.agent, &busy.command) {
        ("turn", ..) => "a reply is being written".to_owned(),
        ("approval", ..) => "a request is waiting for your answer".to_owned(),
        (state, Some(agent), _) => format!(
            "{agent} is {}",
            if state == "blocked" {
                "waiting for you"
            } else {
                "working"
            }
        ),
        (_, None, Some(command)) => format!("`{command}` is still running"),
        (_, None, None) => "something is running".to_owned(),
    }
}

/// A tab that the × just closed, drawn while it fades (100 ms): the tab is gone from the layout
/// at once, so the strip keeps this stand-in at its place until the fade is over.
#[derive(Debug)]
pub(crate) struct ClosingTab {
    pane: super::model::PaneId,
    index: usize,
    icon: crate::icons::Icon,
    title: SharedString,
    until: std::time::Instant,
    _timer: gpui::Task<()>,
}

impl Workbench {
    /// The × of a tab: closes it, and when the tab went away at once, lets a ghost of it fade.
    pub(crate) fn close_tab_with_fade(
        &mut self,
        pane: super::model::PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = |this: &Self| this.state.pane(pane).map_or(0, |p| p.items.len());
        let before = items(self);
        let ghost = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .map(|item| self.item_icon_and_title(item, cx));
        self.close_item_checked(pane, index, window, cx);
        let (Some((icon, title)), true) = (ghost, items(self) < before) else {
            return;
        };
        let exit = motion::Kind::TabClose.exit_duration(cx.reduce_motion());
        if exit.is_zero() || motion::keyboard_input() {
            return;
        }
        self.closing_tab = Some(ClosingTab {
            pane,
            index,
            icon,
            title,
            until: cx.background_executor().now() + exit,
            _timer: cx.spawn(async move |this, cx| {
                cx.background_executor().timer(exit).await;
                this.update(cx, |this, cx| {
                    this.closing_tab = None;
                    cx.notify();
                })
                .ok();
            }),
        });
        cx.notify();
    }

    /// The stand-in of the tab that was just closed in `pane`, with the index it sat at.
    pub(crate) fn closing_tab_ghost(
        &self,
        pane: super::model::PaneId,
        cx: &mut Context<Self>,
    ) -> Option<(usize, AnyElement)> {
        let ghost = self.closing_tab.as_ref().filter(|g| g.pane == pane)?;
        if cx.background_executor().now() >= ghost.until {
            return None;
        }
        let (theme, tokens) = (self.theme, self.density.ui());
        let tab = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(tokens.spacing * 2.0))
            .pr(px(tokens.spacing))
            .h(px(tokens.tab_height - 6.0))
            .rounded_t(px(tokens.radius))
            .text_color(theme.muted)
            .child(crate::icons::glyph(ghost.icon))
            .child(div().max_w(px(180.0)).truncate().child(ghost.title.clone()))
            .child(div().size(px(tokens.font_size + 4.0)));
        let faded = motion::transition(
            "closing-tab",
            motion::Kind::TabClose,
            0.0,
            tab,
            |tab, frame| frame.fade(tab),
        )
        .from(1.0);
        Some((ghost.index, faded.into_any_element()))
    }

    /// The Undo toast, if one is showing.
    pub fn close_toast(&self) -> Option<&CloseToast> {
        self.close_toast.as_ref()
    }

    /// Whether this daemon can close sessions (else no × anywhere).
    pub fn can_close_sessions(&self, cx: &gpui::App) -> bool {
        self.sessions.read(cx).supports_close()
    }

    /// Closes `session`: at once when nothing runs in it (with the Undo toast), else after the
    /// user confirmed stopping what runs.
    pub fn close_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_session_with(session, false, window, cx);
    }

    fn close_session_with(
        &mut self,
        session: SessionId,
        stop_running: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_close_sessions(cx) {
            self.set_notice("This hutand cannot close sessions", cx);
            return;
        }
        let Some(info) = self.sessions.read(cx).session(session).cloned() else {
            return;
        };
        let title = info.title.clone();
        let neighbour = self.neighbour_of(session, &[], cx);
        let split_before = self.split_of_session(session);
        let was_focused = self.focused_session() == Some(session);
        let task = self
            .sessions
            .update(cx, |store, cx| store.close(session, stop_running, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.session_closed_here(
                    session,
                    title,
                    ClosedFrom {
                        split: split_before,
                        was_focused,
                        neighbour,
                    },
                    window,
                    cx,
                ),
                Err(CloseFailure::Busy(busy)) => {
                    let what = busy_text(&busy);
                    this.open_destructive(
                        format!("Close {title}?"),
                        format!("{what}. Closing stops it."),
                        "Stop and close",
                        move |this, window, cx| this.close_session_with(session, true, window, cx),
                        window,
                        cx,
                    );
                }
                Err(CloseFailure::Failed(error)) => {
                    this.set_notice(format!("Cannot close the session: {error}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// What `tabs.close_session` says now (the older `terminal.tab_close` where that is the
    /// only one set).
    pub(crate) fn close_session_mode(&self) -> CloseSessionMode {
        CloseSessionMode::resolve(&self.settings)
    }

    /// Closes the tab at `index` of `pane` (the ×, ⌘W, a middle click, *Close* in its menu): a
    /// tab of a session closes the session with it as `tabs.close_session` says, any other tab
    /// (a file with unsaved changes asks first) just goes. A tab of splits closes the session
    /// of every split.
    pub(crate) fn close_tab(
        &mut self,
        pane: super::model::PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.close_session_mode();
        let sessions: Vec<SessionId> = self
            .state
            .pane(pane)
            .and_then(|p| p.items.get(index))
            .map(|item| {
                item.sessions()
                    .into_iter()
                    .filter_map(|s| s.parse().ok())
                    .filter(|s| self.sessions.read(cx).session(*s).is_some())
                    .collect()
            })
            .unwrap_or_default();
        if sessions.is_empty() || mode == CloseSessionMode::Never || !self.can_close_sessions(cx) {
            self.close_tab_with_fade(pane, index, window, cx);
            return;
        }
        self.close_tab_sessions(sessions, mode == CloseSessionMode::Always, window, cx);
    }

    /// Closes the sessions of a tab (or one split). With `stop_running` what runs in them is
    /// stopped; without, the sessions where something runs are asked about together, in one
    /// question: *Keep running in background* closes their tabs only, *Close* stops them.
    pub(crate) fn close_tab_sessions(
        &mut self,
        sessions: Vec<SessionId>,
        stop_running: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let together = sessions.clone();
        let started: Vec<(SessionId, SharedString, ClosedFrom, _)> = sessions
            .into_iter()
            .filter_map(|session| {
                let title = self.sessions.read(cx).session(session)?.title.clone();
                let from = ClosedFrom {
                    split: self.split_of_session(session),
                    was_focused: self.focused_session() == Some(session),
                    neighbour: self.neighbour_of(session, &together, cx),
                };
                let task = self
                    .sessions
                    .update(cx, |store, cx| store.close(session, stop_running, cx));
                Some((session, SharedString::from(title), from, task))
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut busy: Vec<(SessionId, SharedString, String)> = Vec::new();
            for (session, title, from, task) in started {
                let result = task.await;
                let asked = this
                    .update_in(cx, |this, window, cx| match result {
                        Ok(()) => {
                            this.session_closed_here(session, title.to_string(), from, window, cx);
                            None
                        }
                        Err(CloseFailure::Busy(what)) => Some((session, title, busy_text(&what))),
                        Err(CloseFailure::Failed(error)) => {
                            this.set_notice(format!("Cannot close the session: {error}"), cx);
                            None
                        }
                    })
                    .ok()
                    .flatten();
                busy.extend(asked);
            }
            if !busy.is_empty() {
                this.update_in(cx, |this, window, cx| {
                    this.ask_keep_or_close(busy, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// "Close X? … Closing stops it.": the tab was closed while something runs in its session.
    /// *Keep running in background* (the default) closes the tab only; *Close* stops and closes
    /// the session; Escape leaves everything as it was.
    fn ask_keep_or_close(
        &mut self,
        busy: Vec<(SessionId, SharedString, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<SessionId> = busy.iter().map(|(id, ..)| *id).collect();
        let (title, message) = match busy.as_slice() {
            [(_, title, what)] => (
                format!("Close {title}?"),
                format!("{what}. Closing stops it."),
            ),
            many => (
                format!("Close {} sessions?", many.len()),
                format!(
                    "{}. Closing stops them.",
                    many.iter()
                        .map(|(_, title, what)| format!("{title}: {what}"))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        };
        let keep = ids.clone();
        self.open_modal(
            Modal {
                title: title.into(),
                message: message.into(),
                rows: Vec::new(),
                confirm_label: "Keep running in background".into(),
                input: None,
                secure_input: false,
                destructive: false,
                choice: Some(ModalChoice {
                    label: "Close".into(),
                    destructive: true,
                    run: Rc::new(move |this, _, window, cx| {
                        this.close_tab_sessions(ids.clone(), true, window, cx);
                    }),
                }),
                checkbox: None,
                on_confirm: Rc::new(move |this, _, _, cx| {
                    for session in &keep {
                        this.close_session_tabs(*session, cx);
                    }
                    cx.notify();
                }),
            },
            window,
            cx,
        );
    }

    /// A session this window closed: its tabs go, the Undo toast shows, and the keyboard moves
    /// to the session next to it when this was the one in focus.
    fn session_closed_here(
        &mut self,
        session: SessionId,
        title: String,
        from: ClosedFrom,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_session_closed(session, window, cx);
        self.show_close_toast(session, title, from.split, cx);
        if from.was_focused
            && self.focused_session().is_none()
            && let Some(next) = from.neighbour
        {
            self.open_session(next, window, cx);
        }
    }

    /// The tab of terminal splits `session` is one of, as it stands now.
    fn split_of_session(&self, session: SessionId) -> Option<TermSplit> {
        let (pane, index) = self.state.find_session_tab(&session.to_string())?;
        match self.state.pane(pane)?.items.get(index)? {
            ItemRef::TerminalSplit { split } => Some((**split).clone()),
            _ => None,
        }
    }

    /// The session the sidebar selects when `session` goes: the next one of its project, else
    /// the one before it. A session that closes together with it (`closing`) never is it.
    fn neighbour_of(
        &self,
        session: SessionId,
        closing: &[SessionId],
        cx: &gpui::App,
    ) -> Option<SessionId> {
        let store = self.sessions.read(cx);
        let workspace = store.session(session)?.workspace_id;
        let same: Vec<SessionId> = store
            .sessions()
            .iter()
            .filter(|s| s.workspace_id == workspace)
            .map(|s| s.id)
            .filter(|id| *id == session || !closing.contains(id))
            .collect();
        let at = same.iter().position(|id| *id == session)?;
        same.get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|before| same.get(before)))
            .copied()
    }

    /// A session was closed (by this window or another client): its tabs go.
    pub(crate) fn on_session_closed(
        &mut self,
        session: SessionId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_session_tabs(session, cx);
        cx.notify();
    }

    fn show_close_toast(
        &mut self,
        session: SessionId,
        title: String,
        split: Option<TermSplit>,
        cx: &mut Context<Self>,
    ) {
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.close_toast = Some(CloseToast {
            session,
            title: title.into(),
            split,
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.close_toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.close_toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Undo of the toast: reopens the session.
    pub fn undo_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(toast) = self.close_toast.take() {
            self.reopen_session_into(toast.session, toast.split, window, cx);
        }
        cx.notify();
    }

    /// `session.reopen`, then shows the session in its workspace.
    pub fn reopen_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reopen_session_into(session, None, window, cx);
    }

    /// [`reopen_session`](Self::reopen_session); a session that was a split of a tab goes back
    /// into that tab (same place, same share) when the tab still has the splits it had.
    fn reopen_session_into(
        &mut self,
        session: SessionId,
        split: Option<TermSplit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.reopen(session, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(info) => {
                    if let Some(before) = &split {
                        this.show_workspace_of(info.id, cx);
                        let id = info.id.to_string();
                        this.mutate(cx, |s| {
                            s.restore_term_split(&id, before);
                        });
                    }
                    this.open_session(info.id, window, cx)
                }
                Err(error) => this.set_notice(format!("Cannot reopen the session: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_close_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.close_toast_exit
                .follow(self.close_toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        Some(
            motion::present(
                "close-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("close-toast")
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .toast(&theme, &tokens)
                    .undo_toast(&tokens)
                    .max_w(px(UNDO_TOAST_MAX_WIDTH))
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(SharedString::from(format!("Closed {}", toast.title))),
                    )
                    .child(
                        Button::new("close-undo", "Undo", &theme, &tokens)
                            .link()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.undo_close(window, cx)),
                            )
                            .child(probe(&self.regions, Region::CloseToast("undo"))),
                    ),
            )
            .into_any_element(),
        )
    }
}
