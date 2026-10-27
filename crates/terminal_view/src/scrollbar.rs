//! The terminal's scrollbar (SCROLLBARS): a thumb over the scrollback, in lines. The bottom of the
//! track is the live screen, the top is the oldest line the daemon still holds; dragging scrolls
//! the same viewport the wheel and the keys do, which asks the daemon for the history pages it
//! shows. The alternate screen (vim, htop, a pager) has no scrollback: no bar there.

use gpui::{App, WeakEntity, Window};
use hutan_ui::{Metrics, Scroller};

use crate::view::TerminalView;

/// The scrollback of one view as a [`Scroller`].
pub(crate) struct Scrollback(pub(crate) WeakEntity<TerminalView>);

impl Scroller for Scrollback {
    fn metrics(&self, cx: &App) -> Metrics {
        let Some(view) = self.0.upgrade() else {
            return Metrics::FITS;
        };
        view.read(cx).scrollback_metrics()
    }

    fn set_offset(&self, offset: f32, _window: &mut Window, cx: &mut App) {
        if let Some(view) = self.0.upgrade() {
            view.update(cx, |view, cx| view.scroll_to_line_from_top(offset, cx));
        }
    }
}

impl TerminalView {
    /// Where the viewport is in the scrollback, in lines from the oldest one; nothing to scroll
    /// on the alternate screen, before the first render, or with no history.
    pub(crate) fn scrollback_metrics(&self) -> Metrics {
        let model = self.model.get();
        let Some(layout) = self.layout else {
            return Metrics::FITS;
        };
        if !model.is_ready() || model.modes().alt_screen {
            return Metrics::FITS;
        }
        let history = model.history_len() as f32;
        Metrics {
            viewport: f32::from(layout.rows),
            max: history,
            offset: (history - self.scroll_offset as f32).clamp(0.0, history),
        }
    }

    /// Scrolls so that the line `from_top` lines below the oldest one is the first in view (the
    /// scrollbar's drag); the end of the track is the live screen.
    pub fn scroll_to_line_from_top(&mut self, from_top: f32, cx: &mut gpui::Context<Self>) {
        let history = self.model.get().history_len();
        let from_top = from_top.round().clamp(0.0, history as f32) as u32;
        let wanted = history - from_top;
        let delta = i64::from(wanted) - i64::from(self.scroll_offset);
        if let Ok(delta) = i32::try_from(delta) {
            self.scroll_lines(delta, cx);
        }
    }

    /// The scrollbar over the scrollback (tests and probes read where its thumb is).
    pub fn scrollbar(&self) -> &hutan_ui::Scrollbar {
        &self.bar
    }
}
