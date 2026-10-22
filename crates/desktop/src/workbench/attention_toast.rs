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
use super::ui::motion::{self, PresentExt as _};
use super::ui::{Button, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::RequestCalled;
use crate::sessions::calls::{self, Toast, ToastStack};
use crate::theme::DensityExt as _;

/// The key context the root has while a toast shows: Escape dismisses the toasts then.
pub const TOAST_CONTEXT: &str = "AttentionToast";

/// The toasts on screen, the timer of the next one to go, and the ones on their way out.
#[derive(Default)]
pub struct AttentionToasts {
    stack: ToastStack,
    timer: Option<Task<()>>,
    /// Focus of the toasts' container; created with the first frame that shows one.
    focus: Option<gpui::FocusHandle>,
    /// The toasts of the last frame: one missing from the next has left and is drawn for its
    /// exit.
    drawn: Vec<Toast>,
    leaving: Vec<(Toast, motion::Presence)>,
}

impl Workbench {
    /// The toasts on screen, oldest first.
    pub fn attention_toasts(&self) -> Vec<Toast> {
        self.attention_toasts.stack.toasts().cloned().collect()
    }

    /// How many toasts the last frame drew: the ones on screen and the ones on their way out.
    pub fn attention_toasts_drawn(&self) -> usize {
        self.attention_toasts.drawn.len() + self.attention_toasts.leaving.len()
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

    /// The toasts, bottom right above the status bar. The newest is lowest. A toast that went
    /// (dismissed, answered, expired, replaced) is drawn for its exit, without input.
    pub(crate) fn render_attention_toasts(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let current: Vec<Toast> = self.attention_toasts.stack.toasts().cloned().collect();
        let now = cx.background_executor().now();
        for toast in std::mem::take(&mut self.attention_toasts.drawn) {
            let gone = !current.iter().any(|t| t.id == toast.id);
            let leaving = self
                .attention_toasts
                .leaving
                .iter()
                .any(|(t, _)| t.id == toast.id);
            if gone && !leaving {
                let mut presence = motion::Presence::new(true);
                presence.set(false, motion::Kind::Toast, cx);
                self.attention_toasts.leaving.push((toast, presence));
            }
        }
        self.attention_toasts.leaving.retain(|(toast, presence)| {
            presence.draws(now) && !current.iter().any(|t| t.id == toast.id)
        });
        self.attention_toasts.drawn = current.clone();
        if current.is_empty() && self.attention_toasts.leaving.is_empty() {
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
        let regions = self.regions.clone();
        // Live toasts first by their place in the stack (the probes index them), then the ones
        // that leave; every row keeps its own element id, so a row's motion survives the others.
        let mut shown: Vec<(Toast, Option<usize>)> = current
            .into_iter()
            .enumerate()
            .map(|(index, toast)| (toast, Some(index)))
            .collect();
        shown.extend(
            self.attention_toasts
                .leaving
                .iter()
                .map(|(toast, _)| (toast.clone(), None)),
        );
        shown.sort_by_key(|(toast, _)| toast.id);
        let rows: Vec<AnyElement> = shown
            .into_iter()
            .map(|(toast, live)| {
                let id = toast.id;
                let mut review =
                    Button::new(format!("attn-toast-review-{id}"), "Review", &theme, &tokens)
                        .ghost()
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
                        }));
                if !review_key.is_empty() {
                    review = review.shortcut(review_key.clone());
                }
                let mut dismiss = IconButton::new(
                    format!("attn-toast-dismiss-{id}"),
                    crate::icons::Icon::ActionClose,
                    "Dismiss",
                    &theme,
                    &tokens,
                )
                .compact()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dismiss_attention_toast(id, cx);
                }));
                let mut row = div()
                    .id(("attn-toast", id as usize))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .popover(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(SharedString::from(toast.text));
                if let Some(index) = live {
                    review = review.child(probe(&regions, Region::AttentionToast(index, "review")));
                    dismiss =
                        dismiss.child(probe(&regions, Region::AttentionToast(index, "dismiss")));
                    row = row
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.toast_hover(id, *hovered, cx);
                        }))
                        .child(probe(&regions, Region::AttentionToast(index, "toast")));
                }
                row.child(review)
                    .child(dismiss)
                    .presented(
                        ("attn-toast-motion", id as usize),
                        motion::Kind::Toast,
                        live.is_some(),
                        motion::Slide::Rise,
                    )
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
