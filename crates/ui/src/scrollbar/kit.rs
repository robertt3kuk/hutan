//! [`scroll_y`]: a vertically scrolled element with its scrollbar, for the many places that build
//! a scrolled `div` in a render helper with no field to keep a handle and a bar in.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Refineable as _, ScrollHandle, StatefulInteractiveElement, StyleRefinement,
    Styled, Window,
};

use super::bar::{Axis, ScrollHandleSource, Scrollbar, State};

/// What `scroll_y` keeps between frames under its element id.
struct Kept {
    handle: ScrollHandle,
    state: Rc<State>,
}

type OnPaint = Rc<dyn Fn(Bounds<Pixels>, &mut Window)>;

/// `area`, a styled `div` with an id and its children, scrolled vertically, with a scrollbar
/// as its last child (painted over the content at the edge of what is visible). The element
/// keeps the handle and the bar's interaction (a drag, a fade) in the window's element state
/// under `id`, which must be unique among the scrolled elements of the view being rendered, and
/// not the id of `area` itself.
///
/// ```ignore
/// hutan_ui::scroll_y(
///     "settings-page-scroll",
///     div().id("settings-page").flex().flex_col().children(rows),
/// )
/// ```
pub fn scroll_y<E>(id: impl Into<ElementId>, area: E) -> ScrollY<E>
where
    E: StatefulInteractiveElement + gpui::ParentElement + Styled + IntoElement + 'static,
{
    ScrollY {
        id: id.into(),
        area: Some(area),
        handle: None,
        on_paint: None,
        style: StyleRefinement::default(),
    }
}

/// See [`scroll_y`].
pub struct ScrollY<E> {
    id: ElementId,
    area: Option<E>,
    handle: Option<ScrollHandle>,
    on_paint: Option<OnPaint>,
    /// What was asked of the element after it was wrapped (`presented`, `.w()`): laid over the
    /// area's own style.
    style: StyleRefinement,
}

/// Styling the wrapper styles the scrolled element.
impl<E> Styled for ScrollY<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl<E> std::fmt::Debug for ScrollY<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScrollY")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<E> ScrollY<E> {
    /// Scrolls with `handle`, one the owner scrolls programmatically, instead of one of its own.
    #[must_use]
    pub fn tracking(mut self, handle: &ScrollHandle) -> Self {
        self.handle = Some(handle.clone());
        self
    }

    /// Calls `f` with the thumb's bounds each time a frame draws a thumb.
    #[must_use]
    pub fn on_paint(mut self, f: impl Fn(Bounds<Pixels>, &mut Window) + 'static) -> Self {
        self.on_paint = Some(Rc::new(f));
        self
    }
}

impl<E> IntoElement for ScrollY<E>
where
    E: StatefulInteractiveElement + gpui::ParentElement + Styled + IntoElement + 'static,
{
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E> Element for ScrollY<E>
where
    E: StatefulInteractiveElement + gpui::ParentElement + Styled + IntoElement + 'static,
{
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let Some(mut area) = self.area.take() else {
            let mut empty = gpui::div().into_any_element();
            return (empty.request_layout(window, cx), empty);
        };
        area.style().refine(&self.style);
        let own = self.handle.clone();
        let on_paint = self.on_paint.clone();
        let build = move |kept: Option<Kept>| -> (AnyElement, Kept) {
            let mut kept = kept.unwrap_or_else(|| Kept {
                handle: ScrollHandle::new(),
                state: Rc::default(),
            });
            if let Some(handle) = own {
                kept.handle = handle;
            }
            let mut bar = Scrollbar::with_state(
                ScrollHandleSource::new(kept.handle.clone(), Axis::Vertical),
                Axis::Vertical,
                Rc::clone(&kept.state),
            );
            if let Some(f) = on_paint {
                bar = bar.on_paint(move |bounds, window| f(bounds, window));
            }
            let element = area
                .track_scroll(&kept.handle)
                .overflow_y_scroll()
                .child(bar.element())
                .into_any_element();
            (element, kept)
        };
        let mut element = match global_id {
            Some(global_id) => window.with_element_state(global_id, |kept: Option<Kept>, _| {
                let (element, kept) = build(kept);
                (element, kept)
            }),
            None => build(None).0,
        };
        let layout = element.request_layout(window, cx);
        (layout, element)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut AnyElement,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}
