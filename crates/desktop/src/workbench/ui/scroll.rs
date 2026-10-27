//! The two wheel rules every surface follows (FX-WORK-1, `docs/design.md` § Scrolling):
//!
//! - **An overlay keeps the wheel.** A popover, sheet, palette or dialog covers the window; its
//!   backdrop takes the wheel and the pointer ([`ScrollExt::shield_behind`]), so a wheel notch
//!   over it never scrolls the editor, terminal or list under it.
//! - **A scroll area keeps its wheel.** A scrollable list inside another scrollable surface
//!   ([`ScrollExt::scroll_y_contained`]) takes every vertical wheel event while it has room to
//!   scroll, also at its first and last row: the notch that arrives at the end stops there and
//!   never scrolls the surface around it (no scroll chaining). A list that fits (nothing to
//!   scroll) lets the wheel through, so the page around it still moves.
//!
//! GPUI delivers a wheel event to every scrollable under the pointer, innermost first, and stops
//! only where a handler says so; a handler registered on an element runs after the element's own
//! scrolling, so "scroll, then stop" is one listener.

use gpui::{
    InteractiveElement, Pixels, ScrollDelta, ScrollHandle, ScrollWheelEvent,
    StatefulInteractiveElement, px,
};

/// Whether `event` asks to move along the vertical axis.
fn vertical(event: &ScrollWheelEvent) -> bool {
    match event.delta {
        ScrollDelta::Pixels(delta) => delta.y != Pixels::ZERO,
        ScrollDelta::Lines(delta) => delta.y != 0.0,
    }
}

/// Whether a scroll area with `max_offset` of room can take a vertical `event`.
pub fn takes_vertical_wheel(event: &ScrollWheelEvent, max_offset: Pixels) -> bool {
    vertical(event) && max_offset > px(0.5)
}

pub trait ScrollExt: InteractiveElement + Sized {
    /// The backdrop of an overlay: occludes everything under it (clicks, hover, wheel) and ends
    /// the wheel event here. The overlay's own scroll areas are painted after it and have already
    /// scrolled by then.
    fn shield_behind(self) -> Self {
        self.occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
    }
}

impl<T: InteractiveElement + Sized> ScrollExt for T {}

pub trait ScrollAreaExt: StatefulInteractiveElement + Sized {
    /// A vertically scrolling area that does not chain: while its content is taller than it
    /// (`handle` knows after the first frame), a vertical wheel event stops here, at either end.
    fn scroll_y_contained(self, handle: &ScrollHandle) -> Self {
        let tracked = handle.clone();
        self.track_scroll(handle)
            .overflow_y_scroll()
            .on_scroll_wheel(move |event, _, cx| {
                if takes_vertical_wheel(event, tracked.max_offset().y) {
                    cx.stop_propagation();
                }
            })
    }
}

impl<T: StatefulInteractiveElement + Sized> ScrollAreaExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TouchPhase, point};

    fn wheel(dy: f32) -> ScrollWheelEvent {
        ScrollWheelEvent {
            position: point(px(0.0), px(0.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        }
    }

    #[test]
    fn a_list_with_room_takes_the_wheel_and_one_that_fits_lets_it_through() {
        assert!(takes_vertical_wheel(&wheel(-10.0), px(120.0)));
        assert!(takes_vertical_wheel(&wheel(10.0), px(120.0)));
        assert!(!takes_vertical_wheel(&wheel(-10.0), px(0.0)));
        assert!(
            !takes_vertical_wheel(&wheel(0.0), px(120.0)),
            "no vertical move"
        );
    }
}
