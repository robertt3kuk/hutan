//! The restart summary: a card that comes up once per daemon restart that left something to
//! say about the sessions (`session.recovery`, protocol 0.26), "hutand restarted 3 min ago (it was
//! killed or crashed). 5 sessions: …". Each line opens the session and offers Retry last message
//! or Restart where they apply; Dismiss all acknowledges everything. Closing the card keeps the
//! chips of the rows and the terminal and chat headers until they are acknowledged.

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use hutan_core::SessionId;

use super::panels::{time_tooltip, tone_color};
use super::render::probe;
use super::ui::motion::{self, PresentExt as _};
use super::ui::{self, Button, StyledExt as _};
use super::view::{Region, Workbench};
use crate::sessions::Tone;
use crate::theme::DensityExt as _;
use hutan_client::recovery::{ChipTone, RowAction};

const WIDTH: f32 = 520.0;
const ROWS_MAX_HEIGHT: f32 = 300.0;

impl Workbench {
    /// Whether the restart summary is on screen.
    pub fn recovery_summary_open(&self) -> bool {
        self.recovery_open
    }

    /// The store says a restart left something unacknowledged. After a clean stop that cut
    /// nothing off, started by the user (they stopped the daemon and started it again), there
    /// is nothing to decide: it is acknowledged without a card, chips or banners. Anything
    /// else is shown.
    pub(crate) fn on_recovery_due(&mut self, cx: &mut Context<Self>) {
        let own_start = self.daemon_started_by_user();
        let calm = own_start
            && self
                .sessions
                .read(cx)
                .recovery_summary()
                .is_some_and(|summary| summary.is_calm());
        if calm {
            self.ack_recovery(None, cx);
        } else {
            self.show_recovery_summary(cx);
        }
    }

    /// Puts the summary up (the store says a restart left something unacknowledged).
    pub(crate) fn show_recovery_summary(&mut self, cx: &mut Context<Self>) {
        self.recovery_open = true;
        self.recovery_review = false;
        cx.notify();
    }

    /// Takes the summary down; the chips stay until they are acknowledged.
    pub(crate) fn close_recovery_summary(&mut self, cx: &mut Context<Self>) {
        if self.recovery_open {
            self.recovery_open = false;
            self.recovery_review = false;
            cx.notify();
        }
    }

    /// Acknowledges the recovery of `session`, or of every session, and tells the user when the
    /// daemon refused.
    pub(crate) fn ack_recovery(&mut self, session: Option<SessionId>, cx: &mut Context<Self>) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.ack_recovery(session, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot dismiss: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Sends the last message of a chat again (its turn was cut off by the restart).
    pub(crate) fn retry_last_message(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.retry_last_message(session, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Cannot retry the last message: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Opens the session of a summary line, in its tab (an assistant in its panel).
    fn open_recovered(&mut self, session: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.open_session(session, window, cx);
    }

    pub(crate) fn render_recovery_summary(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let wanted = self.overlays.track(
            "recovery-summary",
            self.recovery_open,
            motion::Kind::Popover,
            cx,
        )?;
        let Some(summary) = self.sessions.read(cx).recovery_summary() else {
            // Everything was acknowledged (here or elsewhere): nothing left to show.
            self.recovery_open = false;
            return None;
        };
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let regions = self.regions.clone();

        // 30 or more sessions: one row, "39 sessions recovered — Review", until reviewed.
        if self.sessions.read(cx).large_sidebar() && !self.recovery_review {
            let warn = summary.rows.iter().any(|r| r.chip.tone == ChipTone::Warn);
            let total = summary.rows.len();
            return Some(
                div()
                    .id("recovery-summary")
                    .key_context("RecoverySummary")
                    .absolute()
                    .right(px(12.0))
                    .bottom(px(tokens.status_bar + 8.0))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .popover(&theme, &tokens)
                    .px(px(tokens.spacing * 4.0))
                    .py(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.font_size))
                    .text_color(if warn {
                        tone_color(&theme, Tone::Warning)
                    } else {
                        theme.fg
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("recovery-headline")
                            .tooltip(time_tooltip(summary.restarted_at_ms))
                            .child(SharedString::from(format!(
                                "{total} sessions recovered — {}",
                                summary.counts
                            ))),
                    )
                    .child(
                        Button::new("recovery-review", "Review", &theme, &tokens)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.recovery_review = true;
                                cx.notify();
                            }))
                            .child(probe(&regions, Region::Recovery("review"))),
                    )
                    .child(
                        Button::new("recovery-dismiss-all", "Dismiss all", &theme, &tokens)
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ack_recovery(None, cx);
                                this.close_recovery_summary(cx);
                            }))
                            .child(probe(&regions, Region::Recovery("dismiss-all"))),
                    )
                    .child(probe(&regions, Region::Recovery("sheet")))
                    .presented(
                        "recovery-motion-row",
                        motion::Kind::Popover,
                        wanted,
                        motion::Slide::Rise,
                    )
                    .into_any_element(),
            );
        }

        let rows: Vec<AnyElement> = summary
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let session = row.session;
                // The same marks as the sidebar rows: a restart that went as planned is quiet
                // (muted, `action.recover`), only one that needs a look is a warning.
                let (tone, glyph) = match row.chip.tone {
                    ChipTone::Info => (theme.muted, crate::icons::Icon::ActionRecover),
                    ChipTone::Warn => (theme.warning, crate::icons::Icon::StateWarning),
                };
                let detail = self
                    .sessions
                    .read(cx)
                    .session(session)
                    .and_then(|info| info.recovery.as_ref())
                    .map(hutan_client::recovery::detail)
                    .unwrap_or_default();
                let action = row.action.map(|action| {
                    Button::new(
                        SharedString::from(format!("recovery-action-{index}")),
                        action.label(),
                        &theme,
                        &tokens,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| match action {
                        RowAction::Retry => this.retry_last_message(session, cx),
                        RowAction::Restart => {
                            let task = this
                                .sessions
                                .update(cx, |store, cx| store.restart(session, cx));
                            cx.spawn(async move |this, cx| {
                                if let Err(error) = task.await {
                                    this.update(cx, |this, cx| {
                                        this.set_notice(
                                            format!("Cannot restart the session: {error}"),
                                            cx,
                                        );
                                    })
                                    .ok();
                                }
                            })
                            .detach();
                        }
                    }))
                    .child(probe(&regions, Region::RecoveryRow(index, "action")))
                });
                div()
                    .id(SharedString::from(format!("recovery-row-{index}")))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .child(
                        div()
                            .id(SharedString::from(format!("recovery-open-{index}")))
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .cursor_pointer()
                            .rounded(px(tokens.radius))
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_recovered(session, window, cx);
                            }))
                            .child(ui::state_label(
                                glyph,
                                row.chip.label,
                                tone,
                                false,
                                &theme,
                                &tokens,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .truncate()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(SharedString::from(row.title.clone())),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(tokens.secondary_font_size))
                                            .text_color(theme.muted)
                                            .child(SharedString::from(detail)),
                                    ),
                            )
                            .child(probe(&regions, Region::RecoveryRow(index, "open"))),
                    )
                    .children(action)
                    .child(
                        Button::new(
                            SharedString::from(format!("recovery-dismiss-{index}")),
                            "Dismiss",
                            &theme,
                            &tokens,
                        )
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ack_recovery(Some(session), cx);
                        }))
                        .child(probe(&regions, Region::RecoveryRow(index, "dismiss"))),
                    )
                    .into_any_element()
            })
            .collect();

        let warn = summary.rows.iter().any(|r| r.chip.tone == ChipTone::Warn);
        let headline_color = if warn {
            tone_color(&theme, Tone::Warning)
        } else {
            theme.fg
        };
        Some(
            div()
                .id("recovery-summary")
                .key_context("RecoverySummary")
                .absolute()
                .right(px(12.0))
                .bottom(px(tokens.status_bar + 8.0))
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
                        .items_start()
                        .gap(px(tokens.spacing * 2.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .id("recovery-headline")
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(headline_color)
                                        .tooltip(time_tooltip(summary.restarted_at_ms))
                                        .child(SharedString::from(summary.headline.clone())),
                                )
                                .child(
                                    div()
                                        .text_color(theme.muted)
                                        .child(SharedString::from(summary.counts.clone())),
                                ),
                        )
                        .child(
                            Button::new("recovery-close", "Close", &theme, &tokens)
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.close_recovery_summary(cx);
                                }))
                                .child(probe(&regions, Region::Recovery("close"))),
                        ),
                )
                .child(
                    div()
                        .id("recovery-rows")
                        .flex()
                        .flex_col()
                        .max_h(px(ROWS_MAX_HEIGHT))
                        .overflow_y_scroll()
                        .children(rows),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("recovery-dismiss-all", "Dismiss all", &theme, &tokens)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.ack_recovery(None, cx);
                                this.close_recovery_summary(cx);
                            }))
                            .child(probe(&regions, Region::Recovery("dismiss-all"))),
                    ),
                )
                .child(probe(&regions, Region::Recovery("sheet")))
                .presented(
                    "recovery-motion",
                    motion::Kind::Popover,
                    wanted,
                    motion::Slide::Rise,
                )
                .into_any_element(),
        )
    }
}
