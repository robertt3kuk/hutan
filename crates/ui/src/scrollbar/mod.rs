//! The scrollbar of every scroll surface (`docs/design.md` § Scrolling): one thin thumb along the
//! right (or bottom) edge of what scrolls, drawn by [`Scrollbar::element`] as one more child of
//! the scrolled element's `relative` parent, after it.
//!
//! - What it follows is a [`Scroller`]: a GPUI `ScrollHandle` ([`Scrollbar::new`]), a list's
//!   state, an editor's pixel offset, a terminal's scrollback in lines. The bar never scrolls on
//!   its own; the wheel and the keys keep working as they do without it.
//! - [`Visibility::Always`] draws a thumb on a faint track wherever there is more to scroll;
//!   [`Visibility::Scrolling`] (an overlay, as on a trackpad) draws it while the content scrolls
//!   and while the pointer is near the edge, and fades it out a second after both stop. The
//!   policy, `appearance.scrollbars` and the colors are [`Policy`].
//! - Dragging the thumb scrolls; a click on the track beside it scrolls a page towards the
//!   click. A bar that is not drawn (faded, or content that fits) takes no clicks, and one under
//!   an overlay that occludes the pointer is not reached.
//! - [`Mark`]s are colored ticks on the track (the editor's diagnostics, find matches and git
//!   changes), drawn while the bar is.
//!
//! The bar reads the scroller while it is painted, after the scrolled element laid itself out, so
//! it is never one frame behind the content.

mod bar;
pub mod geometry;
mod kit;
mod policy;

pub use bar::{Axis, ListSource, Mark, Region, ScrollHandleSource, Scrollbar, Scroller};
pub use geometry::Metrics;
pub use kit::{ScrollY, scroll_y};
pub use policy::{
    FADE, FADE_REDUCED, HOLD, Policy, Style, Visibility, fade_opacity, set_setting, set_style,
    set_system_always,
};
