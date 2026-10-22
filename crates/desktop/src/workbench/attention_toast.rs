//! The window's own call for attention (UX-ATTN-3): while the window is in front, a request that
//! comes for a session that is not on screen shows a toast at the bottom right (`Claude Code asks
//! to edit src/lib.rs · Review ⌘⇧R`) and puts a dot on the session's tab until it is seen.
//!
//! The daemon decides *that* a request calls (`notify.intent`: never for a session some client
//! looks at, never for a muted one); the rules of the toasts themselves (two at most, the oldest
//! replaced, five seconds, hover pauses) are `sessions::calls`. Nothing blinks and nothing
//! repeats: one toast per request, one dot per session.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, div, px,
};
use hutan_core::SessionId;
use hutan_protocol::notify::{IntentKind, NotificationIntent};

use super::model::LayoutPreset;
use super::render::{key_hint, probe};
use super::terminals::session_of_item;
use super::ui::{self, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::RequestCalled;
use crate::sessions::calls::{self, Toast, ToastStack};
use crate::theme::DensityExt as _;

/// The key context the root has while a toast shows: Escape dismisses the toasts then.
pub const TOAST_CONTEXT: &str = "AttentionToast";

/// The toasts on screen and the timer of the next one to go.
#[derive(Default)]
pub struct AttentionToasts {
    stack: ToastStack,
    timer: Option<Task<()>>,
    /// Focus of the toasts' container; created with the first frame that shows one.
    focus: Option<gpui::FocusHandle>,
}

impl Workbench {
    /// The toasts on screen, oldest first.
    pub fn attention_toasts(&self) -> Vec<Toast> {
        self.attention_toasts.stack.toasts().cloned().collect()
    }

    /// The focus of the toasts' container (`None` before one was shown): Escape dismisses the
    /// toasts only while it is focused.
    pub fn attention_toast_focus(&self) -> Option<gpui::FocusHandle> {
        self.attention_toasts.focus.clone()
    }

    /// Whether `session` is the active item of some pane: its card is there to see.
    fn session_on_screen(&self, session: SessionId) -> bool {
        self.state.preset != LayoutPreset::OVERVIEW
            && self
                .state
                .panes()
                .into_iter()
                .filter_map(|pane| pane.active_item())
                .any(|item| session_of_item(item) == Some(session))
    }

    /// A request called for a session this window is not looking at.
    pub(crate) fn on_request_called(
        &mut self,
        event: &RequestCalled,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let intent = &event.intent;
        let session = intent.session_id;
        // Shown in a pane the user is not typing in: it is seen, and needs no toast or dot.
        if self.session_on_screen(session) {
            self.sessions
                .update(cx, |store, cx| store.see_call(session, cx));
            return;
        }
        // A window behind others says nothing: the system's notification is for that, and the
        // dot waits on the tab.
        if !window.is_window_active() {
            return;
        }
        let text = self.call_text(intent, cx);
        let now = cx.background_executor().now();
        self.attention_toasts.stack.push(now, session, text);
        self.arm_toast_timer(cx);
        cx.notify();
    }

    /// What the toast for `intent` says: the agent and what it asks, from the request itself.
    fn call_text(&self, intent: &NotificationIntent, cx: &Context<Self>) -> String {
        let session = intent.session_id;
        let store = self.sessions.read(cx);
        let who = self
            .agent_name_of(session, cx)
            .or_else(|| {
                store
                    .session(session)
                    .map(|info| info.title.clone())
                    .filter(|title| !title.is_empty())
            })
            .unwrap_or_else(|| "An agent".to_owned());
        let what = match intent.kind {
            IntentKind::ApprovalRequired => store
                .approvals()
                .iter()
                .rev()
                .find(|a| a.session_id == session)
                .map(|a| a.title.clone())
                // The state does not show it yet: the intent's body is `<title>: <detail>`.
                .unwrap_or_else(|| first_part(&intent.body)),
            IntentKind::QuestionRequired => store
                .questions()
                .iter()
                .rev()
                .find(|q| q.session_id == session)
                .map(|q| q.spec.title.clone())
                .unwrap_or_default(),
            // A blocked agent: the reason its row gives (what it waits for), else nothing.
            _ => store
                .session(session)
                .map(|info| store.presentation(info).detail)
                .unwrap_or_default(),
        };
        calls::describe(&who, intent.kind, &what)
    }

    /// Wakes the window when the next toast is due: one timer, started from the stack's earliest
    /// deadline, so nothing polls.
    fn arm_toast_timer(&mut self, cx: &mut Context<Self>) {
        self.attention_toasts.timer = None;
        let Some(deadline) = self.attention_toasts.stack.next_deadline() else {
            return;
        };
        let wait = deadline.saturating_duration_since(cx.background_executor().now());
        self.attention_toasts.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                let now = cx.background_executor().now();
                this.attention_toasts.stack.expire(now);
                this.arm_toast_timer(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The pointer came over toast `id`, or left it: a toast under the pointer stays.
    fn toast_hover(&mut self, id: u64, hovered: bool, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.attention_toasts
            .stack
            .hover(now, hovered.then_some(id));
        self.arm_toast_timer(cx);
        cx.notify();
    }

    /// Takes toast `id` off.
    fn dismiss_attention_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.attention_toasts.stack.dismiss(id) {
            self.arm_toast_timer(cx);
            cx.notify();
        }
    }

    /// Escape while a toast shows: every toast goes. Their dots stay on the tabs.
    pub fn dismiss_attention_toasts(&mut self, cx: &mut Context<Self>) {
        if self.attention_toasts.stack.dismiss_all() > 0 {
            self.arm_toast_timer(cx);
            cx.notify();
        }
    }

    /// Takes off the toasts whose request is answered or whose session was looked at.
    pub(crate) fn prune_attention_toasts(&mut self, cx: &mut Context<Self>) {
        let store = self.sessions.read(cx);
        let gone: Vec<SessionId> = self
            .attention_toasts
            .stack
            .toasts()
            .map(|toast| toast.session)
            .filter(|session| !store.has_call(*session))
            .collect();
        let mut any = false;
        for session in gone {
            any |= self.attention_toasts.stack.dismiss_session(session);
        }
        if any {
            self.arm_toast_timer(cx);
            cx.notify();
        }
    }

    /// *Review Request* (⌘⇧R): shows the session of the newest toast, else the one whose request
    /// has waited longest; the toasts go (their dots go with the looking).
    pub fn review_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self
            .attention_toasts
            .stack
            .newest()
            .map(|toast| toast.session)
            .or_else(|| self.sessions.read(cx).requests().first().copied());
        let Some(session) = session else {
            self.set_notice("Nothing is waiting for you", cx);
            return;
        };
        self.attention_toasts.stack.dismiss_session(session);
        self.arm_toast_timer(cx);
        self.open_session(session, window, cx);
    }

    /// The toasts, bottom right above the status bar. The newest is lowest.
    pub(crate) fn render_attention_toasts(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.attention_toasts.stack.is_empty() {
            return None;
        }
        let (theme, tokens) = (self.theme, self.density.ui());
        let focus = self
            .attention_toasts
            .focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let review_key: SharedString =
            key_hint(window, &super::actions::ReviewRequest).unwrap_or_else(|| "".into());
        let toasts: Vec<Toast> = self.attention_toasts.stack.toasts().cloned().collect();
        let rows: Vec<AnyElement> = toasts
            .into_iter()
            .enumerate()
            .map(|(index, toast)| {
                let id = toast.id;
                let mut review = ui::button(
                    format!("attn-toast-review-{id}"),
                    "Review",
                    ui::ButtonKind::Ghost,
                    true,
                    &theme,
                    &tokens,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    let session = this
                        .attention_toasts
                        .stack
                        .toasts()
                        .find(|t| t.id == id)
                        .map(|t| t.session);
                    if let Some(session) = session {
                        this.attention_toasts.stack.dismiss_session(session);
                        this.arm_toast_timer(cx);
                        this.open_session(session, window, cx);
                    }
                }))
                .child(probe(
                    &self.regions,
                    Region::AttentionToast(index, "review"),
                ));
                if !review_key.is_empty() {
                    review = review.child(ui::key_hint(
                        review_key.clone(),
                        ui::ButtonKind::Ghost,
                        &theme,
                        &tokens,
                    ));
                }
                div()
                    .id(("attn-toast", id as usize))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .popover(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        this.toast_hover(id, *hovered, cx);
                    }))
                    .child(SharedString::from(toast.text))
                    .child(review)
                    .child(
                        div()
                            .id(("attn-toast-dismiss", id as usize))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(tokens.font_size + 4.0))
                            .rounded(px(tokens.radius))
                            .cursor_pointer()
                            .text_color(theme.muted)
                            .hover(move |s| s.bg(theme.surface_hover).text_color(theme.fg))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.dismiss_attention_toast(id, cx);
                            }))
                            .child(crate::icons::glyph(crate::icons::Icon::ActionClose))
                            .child(probe(
                                &self.regions,
                                Region::AttentionToast(index, "dismiss"),
                            )),
                    )
                    .child(probe(&self.regions, Region::AttentionToast(index, "toast")))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .id("attn-toasts")
                // Escape dismisses only while keyboard focus is on the toasts (Tab or a click
                // puts it there): never taken from a terminal, the composer or a text field.
                .key_context(TOAST_CONTEXT)
                .track_focus(&focus)
                .tab_stop(true)
                .absolute()
                .bottom(px(tokens.status_bar + tokens.spacing * 3.0))
                .right(px(tokens.spacing * 4.0))
                .flex()
                .flex_col()
                .items_end()
                .gap(px(tokens.spacing * 2.0))
                .children(rows)
                .into_any_element(),
        )
    }
}

/// The part of an intent body before its first `: `: an approval's title, without its detail.
fn first_part(body: &str) -> String {
    body.split_once(": ")
        .map_or(body, |(title, _)| title)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::first_part;

    #[test]
    fn the_title_of_an_approval_is_the_body_before_its_detail() {
        assert_eq!(
            first_part("Edit src/lib.rs: adds a test"),
            "Edit src/lib.rs"
        );
        assert_eq!(first_part("Edit src/lib.rs"), "Edit src/lib.rs");
    }
}
