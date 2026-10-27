use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    App, Bounds, DispatchPhase, Hitbox, HitboxBehavior, Hsla, IntoElement, ListOffset, ListState,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollHandle, Styled, Task, Window, canvas, fill, point, px, size,
};

use super::geometry::{
    self, INSET, MARK_WIDTH, MIN_THUMB, Metrics, SLOP, THICKNESS, ZONE, mark_span, offset_for,
    page_target,
};
use super::policy::{HOLD, Policy, Visibility, fade_opacity};

/// Which way the scrollbar scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// A colored tick on the track: where in the content (`at`, `0..=1`) and how much of it (`len`,
/// a share of the content; ticks are never thinner than a couple of pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mark {
    pub at: f32,
    pub len: f32,
    pub color: Hsla,
}

/// Where a scrollbar sits: along its axis the `extent` of what scrolls, across it the `edge` the
/// bar lies against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub extent: Bounds<Pixels>,
    pub edge: Bounds<Pixels>,
}

impl Region {
    /// Extent and edge are the same bounds.
    pub fn of(bounds: Bounds<Pixels>) -> Self {
        Self {
            extent: bounds,
            edge: bounds,
        }
    }
}

/// What a scrollbar follows and moves.
pub trait Scroller {
    /// The position now along the bar's axis (`Metrics::FITS` hides the bar).
    fn metrics(&self, cx: &App) -> Metrics;

    /// Where the bar sits; `None`: the bounds of the element the bar was added to.
    fn region(&self, _cx: &App) -> Option<Region> {
        None
    }

    /// Scrolls to `offset` (from the start, within `0..=max` of [`metrics`](Self::metrics)).
    fn set_offset(&self, offset: f32, window: &mut Window, cx: &mut App);

    /// The thumb was grabbed: a scroller that measures its content lazily (a list) keeps the
    /// extent it had until [`drag_ended`](Self::drag_ended), so the thumb does not run away
    /// from the pointer.
    fn drag_started(&self) {}

    /// The thumb was let go.
    fn drag_ended(&self) {}
}

/// A GPUI list's [`ListState`] as a [`Scroller`], in pixels (the chat, the sessions and agents
/// lists, history).
///
/// A list measures the rows it draws and counts the others as nothing, which makes a thumb of
/// the measured part and a drag to the top land on the first measured row. So the source gives
/// every row nobody has drawn yet an estimated height (`estimate`, in pixels) whenever the list
/// has laid itself out for a new width, and the two ends of the track are the ends of the list:
/// the first row, and the tail.
#[derive(Clone, Debug)]
pub struct ListSource {
    list: ListState,
    estimate: Pixels,
    /// The width the estimate was given for: a new width makes the list forget its heights.
    width: Cell<Pixels>,
}

impl ListSource {
    pub fn new(list: ListState, estimate: f32) -> Self {
        Self {
            list,
            estimate: px(estimate),
            width: Cell::new(px(-1.0)),
        }
    }
}

impl Scroller for ListSource {
    fn metrics(&self, _cx: &App) -> Metrics {
        let viewport = self.list.viewport_bounds();
        if self.width.replace(viewport.size.width) != viewport.size.width {
            // The list has just laid out for this width and counts its rows as unmeasured.
            let _ = self.list.clone().with_uniform_item_height(self.estimate);
        }
        Metrics {
            viewport: f32::from(viewport.size.height),
            max: f32::from(self.list.max_offset_for_scrollbar().y),
            offset: f32::from(-self.list.scroll_px_offset_for_scrollbar().y),
        }
    }

    fn region(&self, _cx: &App) -> Option<Region> {
        Some(Region::of(self.list.viewport_bounds()))
    }

    fn set_offset(&self, offset: f32, _window: &mut Window, _cx: &mut App) {
        if offset <= 0.5 {
            // The first row, whatever the rows above the measured ones add up to.
            self.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.0),
            });
        } else {
            self.list
                .set_offset_from_scrollbar(point(px(0.0), -px(offset)));
        }
    }

    fn drag_started(&self) {
        self.list.scrollbar_drag_started();
    }

    fn drag_ended(&self) {
        self.list.scrollbar_drag_ended();
    }
}

/// A GPUI [`ScrollHandle`] as a [`Scroller`], in pixels.
#[derive(Clone, Debug)]
pub struct ScrollHandleSource {
    handle: ScrollHandle,
    axis: Axis,
    /// An element that scrolls the scrolled one the other way (a block inside a pane): the bar
    /// sits at the edge of what is visible of both, so it can be reached however far the pane
    /// is scrolled.
    within: Option<ScrollHandle>,
}

impl ScrollHandleSource {
    pub fn new(handle: ScrollHandle, axis: Axis) -> Self {
        Self {
            handle,
            axis,
            within: None,
        }
    }

    /// Keeps the bar inside the visible part of `outer`, the element that scrolls this one.
    pub fn within(mut self, outer: ScrollHandle) -> Self {
        self.within = Some(outer);
        self
    }
}

impl Scroller for ScrollHandleSource {
    fn metrics(&self, _cx: &App) -> Metrics {
        let view = self.handle.bounds();
        let max = self.handle.max_offset();
        let at = self.handle.offset();
        let (viewport, max, offset) = match self.axis {
            Axis::Vertical => (view.size.height, max.y, -at.y),
            Axis::Horizontal => (view.size.width, max.x, -at.x),
        };
        Metrics {
            viewport: f32::from(viewport),
            max: f32::from(max),
            offset: f32::from(offset),
        }
    }

    fn region(&self, _cx: &App) -> Option<Region> {
        let view = self.handle.bounds();
        let edge = match &self.within {
            Some(outer) => view.intersect(&outer.bounds()),
            None => view,
        };
        Some(Region { extent: view, edge })
    }

    fn set_offset(&self, offset: f32, _window: &mut Window, _cx: &mut App) {
        let mut at = self.handle.offset();
        match self.axis {
            Axis::Vertical => at.y = -px(offset),
            Axis::Horizontal => at.x = -px(offset),
        }
        self.handle.set_offset(at);
    }
}

/// What the bar remembers between frames, shared by its clones.
#[derive(Default)]
pub(super) struct State {
    /// Where on the thumb (pixels from its start) the drag took hold, while it is held.
    grab: Cell<Option<f32>>,
    /// The scroll position the last frame saw: a change is scrolling.
    seen: Cell<Option<f32>>,
    /// The last moment the bar was scrolled, held or approached.
    touched: Cell<Option<Instant>>,
    /// The pointer is in the strip along the edge.
    near: Cell<bool>,
    over_thumb: Cell<bool>,
    /// How opaque the last frame drew the thumb.
    opacity: Cell<f32>,
    /// Where the last frame drew the thumb; `None` while none was.
    thumb: Cell<Option<Bounds<Pixels>>>,
    /// The timer that wakes the view when the hold is over, with the moment it is set for.
    wake: RefCell<Option<(Instant, Task<()>)>>,
}

/// One scrollbar: the scroller it follows and the interaction in progress. Cheap to clone; the
/// view keeps one per scrolled element so a drag and a fade survive the frames they span.
#[derive(Clone)]
pub struct Scrollbar {
    source: Rc<dyn Scroller>,
    axis: Axis,
    /// Leaves room at the end for the bar of the other axis.
    corner: bool,
    on_paint: Option<OnPaint>,
    state: Rc<State>,
}

type OnPaint = Rc<dyn Fn(Bounds<Pixels>, &mut Window)>;

impl std::fmt::Debug for Scrollbar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scrollbar")
            .field("axis", &self.axis)
            .finish_non_exhaustive()
    }
}

/// Where the bar's parts are for one scroll position.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placed {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// The strip along the edge in which the pointer is "near" the bar.
    zone: Bounds<Pixels>,
    metrics: Metrics,
    travel: f32,
}

/// What the prepaint of the element hands its paint.
struct Prepared {
    placed: Option<Placed>,
    zone: Option<Hitbox>,
}

fn place(axis: Axis, metrics: Metrics, region: Region, corner: bool) -> Option<Placed> {
    let (origin, mut extent) = match axis {
        Axis::Vertical => (region.extent.origin.y, region.extent.size.height),
        Axis::Horizontal => (region.extent.origin.x, region.extent.size.width),
    };
    if corner {
        extent -= px(THICKNESS + INSET);
    }
    let track_len = (f32::from(extent) - INSET * 2.0).max(MIN_THUMB);
    let thumb = geometry::thumb(metrics, track_len)?;
    let (inset, thick) = (px(INSET), px(THICKNESS));
    let (track, thumb_bounds, zone) = match axis {
        Axis::Vertical => {
            let x = region.edge.right() - thick - inset;
            (
                Bounds::new(point(x, origin + inset), size(thick, px(track_len))),
                Bounds::new(
                    point(x, origin + inset + px(thumb.start)),
                    size(thick, px(thumb.len)),
                ),
                Bounds::new(
                    point(region.edge.right() - px(ZONE), origin),
                    size(px(ZONE), extent),
                ),
            )
        }
        Axis::Horizontal => {
            let y = region.edge.bottom() - thick - inset;
            (
                Bounds::new(point(origin + inset, y), size(px(track_len), thick)),
                Bounds::new(
                    point(origin + inset + px(thumb.start), y),
                    size(px(thumb.len), thick),
                ),
                Bounds::new(
                    point(origin, region.edge.bottom() - px(ZONE)),
                    size(extent, px(ZONE)),
                ),
            )
        }
    };
    Some(Placed {
        track,
        thumb: thumb_bounds,
        zone,
        metrics,
        travel: thumb.travel,
    })
}

impl Scrollbar {
    /// A bar for a scrolled element's `ScrollHandle` along `axis`.
    pub fn new(handle: ScrollHandle, axis: Axis) -> Self {
        Self::of(ScrollHandleSource::new(handle, axis), axis)
    }

    /// A bar over `source` that continues the interaction `state` of an earlier frame's bar: for
    /// elements that rebuild their bar each frame (`scroll_y`).
    pub(super) fn with_state(
        source: impl Scroller + 'static,
        axis: Axis,
        state: Rc<State>,
    ) -> Self {
        Self {
            source: Rc::new(source),
            axis,
            corner: false,
            on_paint: None,
            state,
        }
    }

    /// A bar for a `uniform_list`'s scroll handle.
    pub fn for_uniform_list(handle: &gpui::UniformListScrollHandle) -> Self {
        Self::new(handle.0.borrow().base_handle.clone(), Axis::Vertical)
    }

    /// A bar for a `list`'s state whose rows are about `estimate` pixels tall until drawn.
    pub fn for_list(state: &ListState, estimate: f32) -> Self {
        Self::of(ListSource::new(state.clone(), estimate), Axis::Vertical)
    }

    /// A bar for a scrolled element inside `outer`, the element that scrolls it the other way:
    /// it sits at the edge of what is visible of both.
    pub fn nested(handle: ScrollHandle, axis: Axis, outer: ScrollHandle) -> Self {
        Self::of(ScrollHandleSource::new(handle, axis).within(outer), axis)
    }

    /// A bar for any [`Scroller`] along `axis`.
    pub fn of(source: impl Scroller + 'static, axis: Axis) -> Self {
        Self {
            source: Rc::new(source),
            axis,
            corner: false,
            on_paint: None,
            state: Rc::default(),
        }
    }

    /// Leaves room at the end of the track for the bar of the other axis.
    pub fn leave_corner(mut self) -> Self {
        self.corner = true;
        self
    }

    /// Calls `f` with the thumb's bounds each time a frame draws a thumb (the regions of the
    /// desktop's tests read where it is).
    pub fn on_paint(mut self, f: impl Fn(Bounds<Pixels>, &mut Window) + 'static) -> Self {
        self.on_paint = Some(Rc::new(f));
        self
    }

    pub fn axis(&self) -> Axis {
        self.axis
    }

    /// Where the last frame drew the thumb; `None` while the content fits or the bar has faded
    /// out (tests and probes read where it is).
    pub fn thumb(&self) -> Option<Bounds<Pixels>> {
        self.state.thumb.get()
    }

    /// How opaque the last frame drew the thumb, `0` to `1`.
    pub fn opacity(&self) -> f32 {
        self.state.opacity.get()
    }

    /// Whether the thumb is being dragged.
    pub fn held(&self) -> bool {
        self.state.grab.get().is_some()
    }

    fn along(&self, position: Point<Pixels>) -> f32 {
        f32::from(match self.axis {
            Axis::Vertical => position.y,
            Axis::Horizontal => position.x,
        })
    }

    /// The element to add after the scrolled one.
    pub fn element(&self) -> impl IntoElement + use<> {
        self.marked(Rc::from(Vec::new()))
    }

    /// [`element`](Self::element) with `marks` on the track (a vertical bar's).
    pub fn marked(&self, marks: Rc<[Mark]>) -> impl IntoElement + use<> {
        let (prepare, paint) = (self.clone(), self.clone());
        canvas(
            move |bounds, window, cx| prepare.prepare(bounds, window, cx),
            move |bounds, prepared, window, cx| paint.paint(bounds, prepared, &marks, window, cx),
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn prepare(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Prepared {
        let metrics = self.source.metrics(cx);
        let region = self.source.region(cx).unwrap_or(Region::of(bounds));
        let placed = place(self.axis, metrics, region, self.corner);
        let zone = placed.map(|p| window.insert_hitbox(p.zone, HitboxBehavior::Normal));
        Prepared { placed, zone }
    }

    fn reset(&self) {
        let state = &self.state;
        if state.grab.take().is_some() {
            self.source.drag_ended();
        }
        state.seen.set(None);
        state.near.set(false);
        state.over_thumb.set(false);
        state.opacity.set(0.0);
        state.thumb.set(None);
        *state.wake.borrow_mut() = None;
    }

    fn paint(
        &self,
        _bounds: Bounds<Pixels>,
        prepared: Prepared,
        marks: &[Mark],
        window: &mut Window,
        cx: &mut App,
    ) {
        let (Some(placed), Some(zone)) = (prepared.placed, prepared.zone) else {
            self.reset();
            return;
        };
        let state = &self.state;
        let now = cx.background_executor().now();
        let policy = Policy::of(cx);
        let view = window.current_view();

        let scrolled = state
            .seen
            .replace(Some(placed.metrics.offset))
            .is_some_and(|old| (old - placed.metrics.offset).abs() > 0.001);
        let engaged = state.grab.get().is_some() || state.near.get();
        if scrolled || engaged {
            state.touched.set(Some(now));
        }
        let opacity = match policy.visibility() {
            Visibility::Always => 1.0,
            Visibility::Scrolling if engaged => 1.0,
            Visibility::Scrolling => state.touched.get().map_or(0.0, |at| {
                fade_opacity(now.saturating_duration_since(at), cx.reduce_motion())
            }),
        };
        state.opacity.set(opacity);
        if policy.visibility() == Visibility::Scrolling && !engaged && opacity > 0.0 {
            self.keep_awake(now, view, window, cx);
        }

        if opacity <= 0.0 {
            state.thumb.set(None);
        } else {
            state.thumb.set(Some(placed.thumb));
            if let Some(on_paint) = &self.on_paint {
                on_paint(placed.thumb, window);
            }
            let style = policy.style;
            let held = state.grab.get().is_some() || state.over_thumb.get();
            let thumb: Hsla = if held {
                style.thumb_active
            } else {
                style.thumb
            };
            let radius = px(THICKNESS / 2.0);
            if engaged || policy.visibility() == Visibility::Always {
                window.paint_quad(
                    fill(placed.track, style.track.opacity(opacity)).corner_radii(radius),
                );
            }
            if self.axis == Axis::Vertical {
                let track_len = f32::from(placed.track.size.height);
                for mark in marks {
                    let (start, height) = mark_span(mark.at, mark.len, track_len);
                    let x = placed.track.origin.x + px((THICKNESS - MARK_WIDTH) / 2.0);
                    let bounds = Bounds::new(
                        point(x, placed.track.origin.y + px(start)),
                        size(px(MARK_WIDTH), px(height)),
                    );
                    window.paint_quad(fill(bounds, mark.color.opacity(opacity)));
                }
            }
            window.paint_quad(fill(placed.thumb, thumb.opacity(opacity)).corner_radii(radius));
        }
        self.listen(placed, zone, view, window);
    }

    /// Asks for the frames that fade the thumb: a timer for the end of the hold, then a frame
    /// after frame until it is gone.
    fn keep_awake(&self, now: Instant, view: gpui::EntityId, window: &mut Window, cx: &mut App) {
        let Some(touched) = self.state.touched.get() else {
            return;
        };
        let due = touched + HOLD;
        if now >= due {
            window.request_animation_frame();
            return;
        }
        let pending = self
            .state
            .wake
            .borrow()
            .as_ref()
            .is_some_and(|(at, _)| *at == due);
        if pending {
            return;
        }
        let delay = due.saturating_duration_since(now);
        let task = window.spawn(cx, async move |cx| {
            cx.background_executor().timer(delay).await;
            cx.update(|_, cx| cx.notify(view)).ok();
        });
        *self.state.wake.borrow_mut() = Some((due, task));
    }

    fn listen(&self, placed: Placed, zone: Hitbox, view: gpui::EntityId, window: &mut Window) {
        let down = self.clone();
        let down_zone = zone.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx: &mut App| {
            if phase != DispatchPhase::Capture
                || event.button != MouseButton::Left
                || down.state.opacity.get() <= 0.0
                || !down_zone.should_handle_scroll(window)
                || !placed.track.dilate(px(SLOP)).contains(&event.position)
            {
                return;
            }
            let from = down.along(placed.thumb.origin);
            if placed.thumb.dilate(px(SLOP)).contains(&event.position) {
                down.state.grab.set(Some(down.along(event.position) - from));
                down.source.drag_started();
            } else {
                let towards_end = down.along(event.position) > from;
                down.source
                    .set_offset(page_target(placed.metrics, towards_end), window, cx);
            }
            cx.notify(view);
            cx.stop_propagation();
        });

        let drag = self.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx: &mut App| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let state = &drag.state;
            if let Some(grab) = state.grab.get() {
                if event.pressed_button != Some(MouseButton::Left) {
                    state.grab.set(None);
                    drag.source.drag_ended();
                    cx.notify(view);
                    return;
                }
                let start = drag.along(event.position) - grab - drag.along(placed.track.origin);
                let offset = offset_for(placed.metrics.max, placed.travel, start);
                drag.source.set_offset(offset, window, cx);
                cx.notify(view);
                cx.stop_propagation();
                return;
            }
            let near = zone.should_handle_scroll(window);
            let over = near && placed.thumb.dilate(px(SLOP)).contains(&event.position);
            if near != state.near.replace(near) || over != state.over_thumb.replace(over) {
                cx.notify(view);
            }
        });

        let leave = self.clone();
        window.on_mouse_event(move |_: &MouseExitEvent, _, _, cx: &mut App| {
            let state = &leave.state;
            if state.grab.get().is_none()
                && (state.near.replace(false) | state.over_thumb.replace(false))
            {
                cx.notify(view);
            }
        });

        let up = self.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx: &mut App| {
            if phase == DispatchPhase::Capture
                && event.button == MouseButton::Left
                && up.state.grab.take().is_some()
            {
                up.source.drag_ended();
                cx.notify(view);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region() -> Region {
        Region::of(Bounds::new(
            point(px(10.0), px(20.0)),
            size(px(300.0), px(400.0)),
        ))
    }

    #[test]
    fn a_vertical_bar_lies_against_the_right_edge_and_its_thumb_tracks_the_offset() {
        let metrics = Metrics {
            viewport: 400.0,
            max: 400.0,
            offset: 200.0,
        };
        let placed = place(Axis::Vertical, metrics, region(), false).expect("scrollable");
        assert_eq!(placed.track.right(), px(310.0 - INSET));
        assert_eq!(placed.track.size.height, px(396.0));
        // Half in view: half the track; halfway: the middle of the travel.
        assert_eq!(placed.thumb.size.height, px(198.0));
        assert_eq!(placed.thumb.origin.y, px(20.0 + INSET + 99.0));
        assert!(placed.zone.contains(&point(px(305.0), px(100.0))));
        assert!(!placed.zone.contains(&point(px(200.0), px(100.0))));
    }

    #[test]
    fn the_end_of_the_content_puts_the_thumb_at_the_end_of_the_track() {
        let metrics = Metrics {
            viewport: 400.0,
            max: 1200.0,
            offset: 1200.0,
        };
        let placed = place(Axis::Vertical, metrics, region(), false).expect("scrollable");
        assert_eq!(placed.thumb.bottom(), placed.track.bottom());
    }

    #[test]
    fn a_corner_leaves_room_for_the_other_bar() {
        let metrics = Metrics {
            viewport: 400.0,
            max: 400.0,
            offset: 0.0,
        };
        let full = place(Axis::Vertical, metrics, region(), false).expect("scrollable");
        let cornered = place(Axis::Vertical, metrics, region(), true).expect("scrollable");
        assert_eq!(
            full.track.size.height - cornered.track.size.height,
            px(THICKNESS + INSET)
        );
    }

    #[test]
    fn a_horizontal_bar_lies_along_the_bottom_edge() {
        let metrics = Metrics {
            viewport: 300.0,
            max: 300.0,
            offset: 0.0,
        };
        let placed = place(Axis::Horizontal, metrics, region(), false).expect("scrollable");
        assert_eq!(placed.track.bottom(), px(420.0 - INSET));
        assert_eq!(placed.thumb.origin.x, px(10.0 + INSET));
    }

    #[test]
    fn nothing_is_placed_while_the_content_fits() {
        assert!(place(Axis::Vertical, Metrics::FITS, region(), false).is_none());
    }
}
