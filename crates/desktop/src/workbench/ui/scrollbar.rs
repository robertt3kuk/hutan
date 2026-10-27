//! A scrollbar for a scrolled element that GPUI draws none for: a thin thumb along one edge of
//! the element, sized by how much of the content is in view, shown only while there is more
//! content than room. Dragging the thumb scrolls, a click on the track beside it scrolls a page
//! towards the click, and the wheel and the keys keep working as they do without it.
//!
//! The element it belongs to is wrapped in a `relative` parent; the scrollbar is one more child
//! of that parent, after the element, and paints over its edge. It reads the element's
//! [`ScrollHandle`] while it is painted (the handle's maximum offset and bounds are set when the
//! element is laid out, which happens before a later sibling is painted), so it is never one
//! frame behind the content.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, DispatchPhase, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, ScrollHandle, Styled, Window, canvas, fill, point, px, size,
};

use crate::theme::Theme;

/// The thumb's thickness; the hit area is a little wider.
const THICKNESS: f32 = 7.0;
/// Space between the thumb and the element's edge.
const INSET: f32 = 2.0;
/// The shortest the thumb gets, so it can always be grabbed.
const MIN_THUMB: f32 = 28.0;

/// Which way the scrollbar scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

type OnPaint = Rc<dyn Fn(Bounds<Pixels>, &mut Window)>;

/// One scrollbar: the handle it follows and the drag in progress. Cheap to clone; the view keeps
/// one per scrolled element so a drag survives the frames it spans.
#[derive(Clone)]
pub struct Scrollbar {
    handle: ScrollHandle,
    axis: Axis,
    /// Where on the thumb the drag took hold, along the axis, while the thumb is held.
    grab: Rc<Cell<Option<Pixels>>>,
    on_paint: Option<OnPaint>,
    /// An element that scrolls the scrolled one the other way (a block inside a pane): the bar
    /// sits at the edge of what is visible of both, so it can be reached however far the pane
    /// is scrolled.
    within: Option<ScrollHandle>,
}

impl std::fmt::Debug for Scrollbar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scrollbar")
            .field("axis", &self.axis)
            .finish_non_exhaustive()
    }
}

/// The track and thumb of a scrollbar for given geometry; `None` while everything fits.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Geometry {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// How far the content can scroll along the axis (positive).
    max: Pixels,
    /// How far the thumb can travel along the track.
    travel: Pixels,
}

impl Scrollbar {
    pub fn new(handle: ScrollHandle, axis: Axis) -> Self {
        Self {
            handle,
            axis,
            grab: Rc::default(),
            on_paint: None,
            within: None,
        }
    }

    /// Keeps the bar inside the visible part of `outer`, the element that scrolls this one.
    pub fn within(mut self, outer: ScrollHandle) -> Self {
        self.within = Some(outer);
        self
    }

    /// Calls `f` with the thumb's bounds each time the scrollbar is painted with a thumb (tests
    /// and probes read where it is).
    pub fn on_paint(mut self, f: impl Fn(Bounds<Pixels>, &mut Window) + 'static) -> Self {
        self.on_paint = Some(Rc::new(f));
        self
    }

    /// The scrolled element's handle.
    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

    fn geometry(&self) -> Option<Geometry> {
        let view = self.handle.bounds();
        let max = self.handle.max_offset();
        // Where the cross-axis edge is: of the element, or of what shows of it.
        let edge = match &self.within {
            Some(outer) => view.intersect(&outer.bounds()),
            None => view,
        };
        let (max, viewport, origin, extent, cross) = match self.axis {
            Axis::Vertical => (
                max.y,
                view.size.height,
                view.origin.y,
                view.size.height,
                edge,
            ),
            Axis::Horizontal => (max.x, view.size.width, view.origin.x, view.size.width, edge),
        };
        if max <= px(0.5) || viewport <= px(0.0) {
            return None;
        }
        let inset = px(INSET);
        let track_len = (extent - inset * 2.0).max(px(MIN_THUMB));
        let content = viewport + max;
        let thumb_len = (track_len * (viewport / content)).clamp(px(MIN_THUMB), track_len);
        let travel = track_len - thumb_len;
        let offset = match self.axis {
            Axis::Vertical => -self.handle.offset().y,
            Axis::Horizontal => -self.handle.offset().x,
        };
        let at = travel * (offset / max).clamp(0.0, 1.0);
        let thick = px(THICKNESS);
        let (track, thumb) = match self.axis {
            Axis::Vertical => {
                let x = cross.right() - thick - inset;
                (
                    Bounds::new(point(x, origin + inset), size(thick, track_len)),
                    Bounds::new(point(x, origin + inset + at), size(thick, thumb_len)),
                )
            }
            Axis::Horizontal => {
                let y = cross.bottom() - thick - inset;
                (
                    Bounds::new(point(origin + inset, y), size(track_len, thick)),
                    Bounds::new(point(origin + inset + at, y), size(thumb_len, thick)),
                )
            }
        };
        Some(Geometry {
            track,
            thumb,
            max,
            travel,
        })
    }

    fn along(&self, position: Point<Pixels>) -> Pixels {
        match self.axis {
            Axis::Vertical => position.y,
            Axis::Horizontal => position.x,
        }
    }

    fn start_of(&self, bounds: Bounds<Pixels>) -> Pixels {
        self.along(bounds.origin)
    }

    /// Scrolls the content to `offset` (positive, from the start) along the axis.
    fn scroll_to(&self, offset: Pixels, max: Pixels) {
        let mut at = self.handle.offset();
        let offset = offset.clamp(px(0.0), max);
        match self.axis {
            Axis::Vertical => at.y = -offset,
            Axis::Horizontal => at.x = -offset,
        }
        self.handle.set_offset(at);
    }

    /// The element to add after the scrolled one.
    pub fn element(&self, theme: &Theme) -> impl gpui::IntoElement + use<> {
        let bar = self.clone();
        let (idle, held): (Hsla, Hsla) = (theme.fg.opacity(0.28), theme.fg.opacity(0.5));
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let Some(geometry) = bar.geometry() else {
                    bar.grab.set(None);
                    return;
                };
                if let Some(on_paint) = &bar.on_paint {
                    on_paint(geometry.thumb, window);
                }
                let color = if bar.grab.get().is_some() { held } else { idle };
                window.paint_quad(fill(geometry.thumb, color).corner_radii(px(THICKNESS / 2.0)));
                let down = bar.clone();
                window.on_mouse_event(
                    move |event: &MouseDownEvent, phase, window, cx: &mut App| {
                        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                            return;
                        }
                        let Some(g) = down.geometry() else { return };
                        // The hit area is a little wider than the drawn thumb.
                        let hit = g.track.dilate(px(2.0));
                        if !hit.contains(&event.position) {
                            return;
                        }
                        if g.thumb.dilate(px(2.0)).contains(&event.position) {
                            down.grab
                                .set(Some(down.along(event.position) - down.start_of(g.thumb)));
                        } else {
                            // A page towards the click.
                            let page = match down.axis {
                                Axis::Vertical => down.handle.bounds().size.height,
                                Axis::Horizontal => down.handle.bounds().size.width,
                            };
                            let current = match down.axis {
                                Axis::Vertical => -down.handle.offset().y,
                                Axis::Horizontal => -down.handle.offset().x,
                            };
                            let towards_end =
                                down.along(event.position) > down.start_of(g.thumb) + px(0.0);
                            let to = if towards_end {
                                current + page
                            } else {
                                current - page
                            };
                            down.scroll_to(to, g.max);
                        }
                        window.refresh();
                        cx.stop_propagation();
                    },
                );
                let drag = bar.clone();
                window.on_mouse_event(
                    move |event: &MouseMoveEvent, phase, window, cx: &mut App| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        let Some(grab) = drag.grab.get() else { return };
                        if event.pressed_button != Some(MouseButton::Left) {
                            drag.grab.set(None);
                            window.refresh();
                            return;
                        }
                        let Some(g) = drag.geometry() else { return };
                        if g.travel > px(0.0) {
                            let thumb_start =
                                drag.along(event.position) - grab - drag.start_of(g.track);
                            let share = (thumb_start / g.travel).clamp(0.0, 1.0);
                            drag.scroll_to(g.max * share, g.max);
                        }
                        window.refresh();
                        cx.stop_propagation();
                    },
                );
                let up = bar.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, _: &mut App| {
                    if phase == DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && up.grab.take().is_some()
                    {
                        window.refresh();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_drawn_while_the_content_fits() {
        let bar = Scrollbar::new(ScrollHandle::new(), Axis::Vertical);
        assert!(bar.geometry().is_none());
    }
}
