//! Closing sessions and reopening them (protocol 0.30, capability `session.close`): the × of
//! the sidebar and the Agent Dashboard, "Close session…" in the menus, the confirmation when
//! something runs, the "Closed … · Undo" toast, and History's Reopen.

use std::time::Duration;

use crate::theme::DensityExt as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use hutan_core::SessionId;
use hutan_protocol::messages::SessionBusy;

use super::render::probe;
use super::ui::{self, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::CloseFailure;

/// How long the Undo toast stays.
pub const TOAST_TIME: Duration = Duration::from_secs(5);

/// "Closed <title> · Undo".
#[derive(Debug, Clone)]
pub struct CloseToast {
    pub session: SessionId,
    pub title: SharedString,
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

impl Workbench {
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
        let was_focused = self.focused_session() == Some(session);
        let task = self
            .sessions
            .update(cx, |store, cx| store.close(session, stop_running, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.on_session_closed(session, window, cx);
                    this.show_close_toast(session, title, cx);
                    if was_focused
                        && this.focused_session().is_none()
                        && let Some(next) = neighbour
                    {
                        this.open_session(next, window, cx);
                    }
                }
                Err(CloseFailure::Busy(busy)) => {
                    let what = busy_text(&busy);
                    this.open_confirm(
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

    fn show_close_toast(&mut self, session: SessionId, title: String, cx: &mut Context<Self>) {
        self.toast_rev += 1;
        let rev = self.toast_rev;
        self.close_toast = Some(CloseToast {
            session,
            title: title.into(),
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
            self.reopen_session(toast.session, window, cx);
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
        let task = self
            .sessions
            .update(cx, |store, cx| store.reopen(session, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(info) => this.open_session(info.id, window, cx),
                Err(error) => this.set_notice(format!("Cannot reopen the session: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn render_close_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.close_toast.clone()?;
        let (theme, tokens) = (self.theme, self.density.ui());
        Some(
            div()
                .id("close-toast")
                .absolute()
                .bottom(px(tokens.status_bar + tokens.spacing * 3.0))
                .left(px(tokens.spacing * 4.0))
                .flex()
                .items_center()
                .gap(px(tokens.spacing * 3.0))
                .popover(&theme, &tokens)
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing * 2.0))
                .child(SharedString::from(format!("Closed {}", toast.title)))
                .child(
                    ui::button(
                        "close-undo",
                        "Undo",
                        ui::ButtonKind::Ghost,
                        true,
                        &theme,
                        &tokens,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.undo_close(window, cx)))
                    .child(probe(&self.regions, Region::CloseToast("undo"))),
                )
                .into_any_element(),
        )
    }
}
