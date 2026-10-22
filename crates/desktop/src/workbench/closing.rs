//! Closing sessions and reopening them (protocol 0.30, capability `session.close`): the × of
//! the sidebar and the Agent Dashboard, "Close session…" in the menus, the confirmation when
//! something runs, the "Closed … · Undo" toast, and History's Reopen.

use std::time::Duration;

use crate::theme::DensityExt as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    Window, div, px,
};
use hutan_core::SessionId;
use hutan_protocol::messages::SessionBusy;

use super::model::{ItemRef, TermSplit};
use super::render::probe;
use super::ui::motion;
use super::ui::{Button, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::CloseFailure;

/// How long the Undo toast stays.
pub const TOAST_TIME: Duration = Duration::from_secs(5);

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
        let neighbour = self.neighbour_of(session, cx);
        let split_before = self.split_of_session(session);
        let was_focused = self.focused_session() == Some(session);
        let task = self
            .sessions
            .update(cx, |store, cx| store.close(session, stop_running, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.on_session_closed(session, window, cx);
                    this.show_close_toast(session, title, split_before, cx);
                    if was_focused
                        && this.focused_session().is_none()
                        && let Some(next) = neighbour
                    {
                        this.open_session(next, window, cx);
                    }
                }
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

    /// The tab of terminal splits `session` is one of, as it stands now.
    fn split_of_session(&self, session: SessionId) -> Option<TermSplit> {
        let (pane, index) = self.state.find_session_tab(&session.to_string())?;
        match self.state.pane(pane)?.items.get(index)? {
            ItemRef::TerminalSplit { split } => Some((**split).clone()),
            _ => None,
        }
    }

    /// The session the sidebar selects when `session` goes: the next one of its project, else
    /// the one before it.
    fn neighbour_of(&self, session: SessionId, cx: &gpui::App) -> Option<SessionId> {
        let store = self.sessions.read(cx);
        let workspace = store.session(session)?.workspace_id;
        let same: Vec<SessionId> = store
            .sessions()
            .iter()
            .filter(|s| s.workspace_id == workspace)
            .map(|s| s.id)
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
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(SharedString::from(format!("Closed {}", toast.title)))
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
