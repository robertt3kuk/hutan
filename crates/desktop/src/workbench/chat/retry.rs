//! A turn that failed for a reason that may pass (`UX-RETRY`): a rate limit with its
//! `retry-after`, a provider that failed, a connection that did not open, a timeout, a stream
//! that broke. The failed footer says what to do next, counts down a known `retry-after` (and
//! stops once the window has passed), and has *Retry now*, which sends the last message again.
//!
//! The words are the daemon's (`GatewayError`'s `Display`, carried by `turn_failed`), so the
//! classification reads them; GPUI-free parts first, unit tested.

use crate::workbench::ui::Button;
use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px};
use hutan_chat_view::Body;

use super::markdown_view::Look;
use super::view::ChatView;
use crate::workbench::render::probe;
use crate::workbench::view::Region;

/// Whether the same message may succeed later without a change.
pub fn is_transient(message: &str) -> bool {
    [
        "rate limited",
        "the provider failed",
        "could not connect to the provider",
        "the provider did not answer in time",
        "the stream failed",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

/// The seconds of `(retry after 5s)`, when the provider sent a `Retry-After`.
pub fn retry_after_secs(message: &str) -> Option<u64> {
    let rest = message.split_once("(retry after ")?.1;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    rest[digits.len()..].starts_with("s)").then_some(())?;
    digits.parse().ok()
}

/// What to do next, in a sentence; `None` for an error Retry cannot help with.
pub fn next_action(message: &str) -> Option<String> {
    let text = if message.starts_with("rate limited") {
        match retry_after_secs(message) {
            Some(secs) => format!(
                "The provider is limiting requests. It asks for {secs} s; Retry now sends your last message again."
            ),
            None => "The provider is limiting requests and did not say for how long. Wait a little, then Retry now, or switch model.".to_owned(),
        }
    } else if message.starts_with("the provider failed") {
        "The provider had a problem on its side. Retry now sends your last message again."
            .to_owned()
    } else if message.starts_with("could not connect") {
        "Hutan could not reach the provider: check the network and the base URL, then Retry now."
            .to_owned()
    } else if message.starts_with("the provider did not answer") {
        "The provider took too long. Retry now sends your last message again.".to_owned()
    } else if message.starts_with("the stream failed") {
        "The reply broke off. Retry now sends your last message again.".to_owned()
    } else {
        return None;
    };
    Some(text)
}

/// Whole seconds still to wait at `now_ms` when a failure at `failed_ms` asked for `after_secs`;
/// `None` once the window has passed (the countdown is no longer offered).
pub fn remaining_secs(failed_ms: u64, after_secs: u64, now_ms: u64) -> Option<u64> {
    let ready = failed_ms.saturating_add(after_secs.saturating_mul(1000));
    let left = ready.saturating_sub(now_ms);
    (left > 0).then(|| left.div_ceil(1000))
}

impl ChatView {
    /// The recovery row of the failed footer at `index`: what to do next, the countdown while a
    /// `retry-after` runs, and *Retry now*. Only the newest failure of a conversation in which
    /// nothing has been sent since, and not while a turn runs.
    pub(super) fn footer_retry(
        &mut self,
        index: usize,
        message: &str,
        failed_ms: u64,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !is_transient(message) {
            return None;
        }
        let model = self.model.read(cx);
        let entries = model.transcript().entries();
        let last_failure = entries
            .iter()
            .rposition(|e| matches!(e.body, Body::Footer(_)))
            == Some(index);
        let spoken_since = entries[index + 1..]
            .iter()
            .any(|e| matches!(e.body, Body::User(_)));
        let busy = model.sending()
            || self
                .sessions
                .read(cx)
                .session(self.session)
                .and_then(|s| s.chat.as_ref())
                .is_none_or(|chat| chat.turn_active);
        if !last_failure || spoken_since || busy {
            return None;
        }
        let (theme, tokens) = (&look.theme, &look.tokens);
        let now = hutan_client::time::now_ms();
        let left =
            retry_after_secs(message).and_then(|after| remaining_secs(failed_ms, after, now));
        if left.is_some() && self.retry_tick.is_none() {
            // Draw this card again in a second; the list keeps cards until told they changed.
            self.retry_tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                this.update(cx, |this, cx| {
                    this.retry_tick = None;
                    this.list.splice(index..index + 1, 1);
                    cx.notify();
                })
                .ok();
            }));
        }
        let guidance = next_action(message)?;
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .mt(px(tokens.spacing))
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(SharedString::from(guidance)),
                )
                .children(left.map(|secs| {
                    div()
                        .flex_none()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.warning)
                        .child(SharedString::from(format!("Retry available in {secs} s")))
                        .child(probe(&self.regions, Region::Chat("retry-countdown")))
                }))
                .child(
                    Button::new("chat-retry-now", "Retry now", &look.theme, &look.tokens)
                        .child(probe(&self.regions, Region::Chat("retry-now")))
                        .on_click(cx.listener(|this, _, _, cx| this.retry_last_message(cx))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_failures_that_may_pass_offer_a_retry() {
        for message in [
            "rate limited (retry after 5s): slow down",
            "rate limited: slow down",
            "the provider failed (HTTP 503): overloaded",
            "could not connect to the provider after 3 attempt(s): refused",
            "the provider did not answer in time",
            "the stream failed: reset",
        ] {
            assert!(is_transient(message), "{message}");
            assert!(next_action(message).is_some(), "{message}");
        }
        for message in [
            "the provider refused the credentials (HTTP 401): no",
            "not found: gone; check the model name and the provider's base URL",
            "the provider rejected the request (HTTP 400): bad",
            "cancelled",
        ] {
            assert!(!is_transient(message), "{message}");
            assert_eq!(next_action(message), None);
        }
    }

    #[test]
    fn a_retry_after_is_read_from_the_error_text() {
        assert_eq!(
            retry_after_secs("rate limited (retry after 5s): x"),
            Some(5)
        );
        assert_eq!(
            retry_after_secs("rate limited (retry after 120s): x"),
            Some(120)
        );
        assert_eq!(retry_after_secs("rate limited: x"), None);
        assert_eq!(retry_after_secs("rate limited (retry after soon): x"), None);
    }

    #[test]
    fn the_countdown_runs_down_and_is_gone_once_the_window_passed() {
        assert_eq!(remaining_secs(10_000, 5, 10_000), Some(5));
        assert_eq!(remaining_secs(10_000, 5, 12_001), Some(3));
        assert_eq!(remaining_secs(10_000, 5, 14_999), Some(1));
        assert_eq!(remaining_secs(10_000, 5, 15_000), None);
        assert_eq!(remaining_secs(10_000, 5, 99_000), None);
        // A clock behind the failure does not underflow.
        assert_eq!(remaining_secs(10_000, 5, 0), Some(15));
    }
}
