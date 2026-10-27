//! The draggable divider between two columns of a view (the Changes view's file list and diff):
//! a thin strip that shows the column-resize cursor and takes a drag; a double click gives the
//! column its default width back. The width it sets is a wish: the view clamps it to the room it
//! has every frame, so a window or dock that shrinks never leaves a column wider than the view.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, DispatchPhase, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Styled, Window, canvas, div,
    prelude::FluentBuilder as _, px,
};

use crate::theme::Theme;

/// Width of the strip that takes the mouse.
pub const HANDLE: f32 = 7.0;

/// The width of the first column and the drag that changes it. Cheap to clone; the view keeps one
/// so the drag survives the frames it spans.
#[derive(Clone, Debug)]
pub struct Splitter {
    /// The width the user wants, or `None` for the default.
    wanted: Rc<Cell<Option<f32>>>,
    /// `(mouse x, width)` when the drag took hold.
    drag: Rc<Cell<Option<(Pixels, f32)>>>,
    /// The widest the column may be right now (set by the view every frame).
    most: Rc<Cell<f32>>,
    /// The narrowest.
    least: f32,
}

impl Splitter {
    pub fn new(least: f32) -> Self {
        Self {
            wanted: Rc::default(),
            drag: Rc::default(),
            most: Rc::new(Cell::new(f32::MAX)),
            least,
        }
    }

    /// The column's width: the wanted one (or `default`), kept within the room there is.
    pub fn width(&self, default: f32, most: f32) -> f32 {
        let most = most.max(self.least);
        self.most.set(most);
        self.wanted.get().unwrap_or(default).clamp(self.least, most)
    }

    /// The user's wish, if they dragged.
    pub fn wanted(&self) -> Option<f32> {
        self.wanted.get()
    }

    /// Sets the wished width (a drag does; tests do).
    pub fn set(&self, width: f32) {
        self.wanted
            .set(Some(width.clamp(self.least, self.most.get())));
    }

    /// Whether a drag is in progress.
    pub fn dragging(&self) -> bool {
        self.drag.get().is_some()
    }

    /// The strip, to be placed between the two columns. `current` is the column's width now;
    /// `on_paint` receives the strip's bounds.
    pub fn element<F: Fn(Bounds<Pixels>, &mut Window) + 'static>(
        &self,
        id: &'static str,
        current: f32,
        theme: &Theme,
        on_paint: F,
    ) -> impl IntoElement + use<F> {
        let hover = theme.accent.opacity(0.35);
        let held = theme.accent.opacity(0.6);
        let dragging = self.dragging();
        let down = self.clone();
        let global = self.clone();
        div()
            .id(id)
            .relative()
            .flex_none()
            .w(px(HANDLE))
            .h_full()
            .cursor_col_resize()
            .hover(move |s| s.bg(hover))
            .when(dragging, move |s| s.bg(held))
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    down.wanted.set(None);
                    down.drag.set(None);
                } else {
                    down.drag.set(Some((event.position.x, current)));
                }
                cx.stop_propagation();
            })
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        on_paint(bounds, window);
                        let moving = global.clone();
                        window.on_mouse_event(
                            move |event: &MouseMoveEvent, phase, window, cx: &mut App| {
                                if phase != DispatchPhase::Capture {
                                    return;
                                }
                                let Some((from, width)) = moving.drag.get() else {
                                    return;
                                };
                                if event.pressed_button != Some(MouseButton::Left) {
                                    moving.drag.set(None);
                                    window.refresh();
                                    return;
                                }
                                let moved = f32::from(event.position.x - from);
                                moving.set(width + moved);
                                window.refresh();
                                cx.stop_propagation();
                            },
                        );
                        let up = global.clone();
                        window.on_mouse_event(
                            move |event: &MouseUpEvent, phase, window, _: &mut App| {
                                if phase == DispatchPhase::Capture
                                    && event.button == MouseButton::Left
                                    && up.drag.take().is_some()
                                {
                                    window.refresh();
                                }
                            },
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(HANDLE / 2.0))
                    .w(px(1.0))
                    .bg(theme.border),
            )
    }
}
