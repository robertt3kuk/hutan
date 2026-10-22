//! Motion: the duration and easing tokens, the one interruptible transition element, and the
//! helpers the views call (`docs/design.md` § Motion; evidence in
//! `docs/spikes/2026-10-22-ui-polish/motion.md`).
//!
//! The rules the module enforces:
//! - Four durations ([`FAST`] 100 ms, [`BASE`] 150 ms, [`SLOW`] 200 ms, [`MAX`] 250 ms) and
//!   [`INSTANT`]; nothing is longer. Exit is shorter than enter. Every curve is an ease-out; there
//!   is no ease-in.
//! - Motion explains a change of state; it never delays input. A transition is a pure function of
//!   the clock: nothing waits for it, and a view that changes its mind mid-flight continues from
//!   the value on screen ([`Transition`] retargets; GPUI's keyed `with_animation` restarts).
//! - A settled transition requests no frames. The idle window stays idle.
//! - Never animated: keyboard-initiated actions, typing, scrolling content, terminal output,
//!   theme switches, the focus ring, context menus opening.
//! - GPUI has no transform: a slide is a margin offset of a floating surface, a collapse is a
//!   clipped height, a fade is `opacity`.
//! - Reduce Motion ([`reduce_motion`], live from macOS and `appearance.reduce_motion`): no
//!   movement and no height animation; opacity changes stay, at most [`BASE`] long.
//!
//! The helpers read `cx.reduce_motion()`, which [`apply`] keeps equal to [`reduce_motion`].

use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Context, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton, ParentElement,
    Pixels, Rgba, StatefulInteractiveElement, Styled, Window, canvas, div, px,
};
use hutan_settings::ReduceMotionSetting;

// ---- durations -------------------------------------------------------------------------------

/// Keyboard actions, the focus ring, context menus opening: no motion.
pub const INSTANT: Duration = Duration::ZERO;
/// Hover and press colour, popover close, tab close.
pub const FAST: Duration = Duration::from_millis(100);
/// Popover and menu open, collapse and expand, chips: the cap of the clean aesthetic.
pub const BASE: Duration = Duration::from_millis(150);
/// Sheets and toasts entering.
pub const SLOW: Duration = Duration::from_millis(200);
/// The hard ceiling; nothing uses it without a design review.
pub const MAX: Duration = Duration::from_millis(250);

// ---- easing ----------------------------------------------------------------------------------

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function: GPUI has only `ease_out_quint`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bezier {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Bezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coordinate(a: f32, b: f32, s: f32) -> f32 {
        // The cubic Bézier with end points 0 and 1 and control values `a`, `b`.
        let inv = 1.0 - s;
        3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
    }

    fn slope(a: f32, b: f32, s: f32) -> f32 {
        let inv = 1.0 - s;
        3.0 * inv * inv * a + 6.0 * inv * s * (b - a) + 3.0 * s * s * (1.0 - b)
    }

    /// The eased value at progress `t` (clamped to `0..=1`): solves `x(s) = t` for the curve's
    /// parameter by Newton's method, falling back to bisection where the slope is flat.
    pub fn at(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let mut s = t;
        for _ in 0..8 {
            let error = Self::coordinate(self.x1, self.x2, s) - t;
            if error.abs() < 1e-6 {
                return Self::coordinate(self.y1, self.y2, s);
            }
            let slope = Self::slope(self.x1, self.x2, s);
            if slope.abs() < 1e-6 {
                break;
            }
            s -= error / slope;
        }
        let (mut low, mut high) = (0.0_f32, 1.0_f32);
        s = t;
        for _ in 0..24 {
            let x = Self::coordinate(self.x1, self.x2, s);
            if (x - t).abs() < 1e-6 {
                break;
            }
            if x < t {
                low = s;
            } else {
                high = s;
            }
            s = (low + high) / 2.0;
        }
        Self::coordinate(self.y1, self.y2, s)
    }
}

/// The curves. Never `ease-in` on interface motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ease {
    /// Pulses only.
    Linear,
    /// CSS `ease`: hover and colour.
    Ease,
    /// `cubic-bezier(0.23, 1, 0.32, 1)`: enter and exit of everything that appears.
    EaseOut,
    /// `cubic-bezier(0.77, 0, 0.175, 1)`: movement of something already on screen (rare).
    EaseInOut,
    /// `cubic-bezier(0.32, 0.72, 0, 1)`: sheets.
    Drawer,
}

impl Ease {
    pub const fn bezier(self) -> Option<Bezier> {
        match self {
            Self::Linear => None,
            Self::Ease => Some(Bezier::new(0.25, 0.1, 0.25, 1.0)),
            Self::EaseOut => Some(Bezier::new(0.23, 1.0, 0.32, 1.0)),
            Self::EaseInOut => Some(Bezier::new(0.77, 0.0, 0.175, 1.0)),
            Self::Drawer => Some(Bezier::new(0.32, 0.72, 0.0, 1.0)),
        }
    }

    /// The eased value at progress `t` in `0..=1`.
    pub fn apply(self, t: f32) -> f32 {
        match self.bezier() {
            None => t.clamp(0.0, 1.0),
            Some(curve) => curve.at(t),
        }
    }
}

// ---- what moves ------------------------------------------------------------------------------

/// What kind of thing moves; each has its own durations, curves and travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Hover and pressed colour of buttons and rows.
    Hover,
    /// Popovers, menus, pickers: fade and a short slide.
    Popover,
    /// Sheets and modals: fade and a slide on the drawer curve.
    Sheet,
    /// Toasts.
    Toast,
    /// Collapse and expand of a group, a card or the Finished group: clipped height.
    Collapse,
    /// Chips and cards that appear and leave (queue chips, an approval card): fade.
    Chip,
    /// A tab closing: fade.
    TabClose,
}

/// The numbers of one [`Kind`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub enter: Duration,
    pub exit: Duration,
    pub enter_ease: Ease,
    pub exit_ease: Ease,
    /// Pixels a slide covers (0: the kind does not move).
    pub travel: f32,
}

impl Kind {
    /// The spec; under Reduce Motion nothing moves, opacity changes are at most [`BASE`], and a
    /// collapse is instant.
    pub const fn spec(self, reduce: bool) -> Spec {
        let full = match self {
            Self::Hover => Spec {
                enter: FAST,
                exit: FAST,
                enter_ease: Ease::Ease,
                exit_ease: Ease::Ease,
                travel: 0.0,
            },
            Self::Popover => Spec {
                enter: BASE,
                exit: FAST,
                enter_ease: Ease::EaseOut,
                exit_ease: Ease::EaseOut,
                travel: 4.0,
            },
            Self::Sheet => Spec {
                enter: SLOW,
                exit: BASE,
                enter_ease: Ease::Drawer,
                exit_ease: Ease::EaseOut,
                travel: 8.0,
            },
            Self::Toast => Spec {
                enter: SLOW,
                exit: BASE,
                enter_ease: Ease::EaseOut,
                exit_ease: Ease::EaseOut,
                travel: 8.0,
            },
            Self::Collapse => Spec {
                enter: BASE,
                exit: BASE,
                enter_ease: Ease::EaseOut,
                exit_ease: Ease::EaseOut,
                travel: 0.0,
            },
            Self::Chip => Spec {
                enter: BASE,
                exit: FAST,
                enter_ease: Ease::EaseOut,
                exit_ease: Ease::EaseOut,
                travel: 0.0,
            },
            Self::TabClose => Spec {
                enter: FAST,
                exit: FAST,
                enter_ease: Ease::EaseOut,
                exit_ease: Ease::EaseOut,
                travel: 0.0,
            },
        };
        if !reduce {
            return full;
        }
        if matches!(self, Self::Collapse) {
            return Spec {
                enter: INSTANT,
                exit: INSTANT,
                travel: 0.0,
                ..full
            };
        }
        Spec {
            enter: min_duration(full.enter, BASE),
            exit: min_duration(full.exit, BASE),
            travel: 0.0,
            ..full
        }
    }

    /// Whether a keyboard-initiated change of this kind skips the motion (a shortcut that opens a
    /// sheet, Escape that closes it, Cmd-arrow that folds a group): everything but the hover
    /// colour, which a key cannot cause, and toasts, which report something that already happened.
    pub const fn follows_keyboard(self) -> bool {
        !matches!(self, Self::Hover | Self::Toast)
    }

    /// How long the exit of this kind takes now: how long an owner keeps drawing something that
    /// left (see [`Presence`]).
    pub fn exit_duration(self, reduce: bool) -> Duration {
        self.spec(reduce).exit
    }
}

impl Spec {
    /// The same kind with no duration and no travel.
    pub const fn instant(self) -> Self {
        Self {
            enter: INSTANT,
            exit: INSTANT,
            travel: 0.0,
            ..self
        }
    }
}

const fn min_duration(a: Duration, b: Duration) -> Duration {
    if a.as_nanos() <= b.as_nanos() { a } else { b }
}

// ---- keyboard-initiated changes ---------------------------------------------------------------

static KEYBOARD: AtomicBool = AtomicBool::new(false);

/// Records whether the last input was a key (`true`) or a pointer press (`false`): while it was
/// a key, the kinds that [`follow the keyboard`](Kind::follows_keyboard) change instantly. The
/// workbench calls it from its keystroke interceptor and its mouse-down observer.
pub fn note_input(from_keyboard: bool) {
    KEYBOARD.store(from_keyboard, Ordering::Relaxed);
}

/// Whether the last input was a key (see [`note_input`]).
pub fn keyboard_input() -> bool {
    KEYBOARD.load(Ordering::Relaxed)
}

fn spec_now(kind: Kind, reduce: bool) -> Spec {
    let spec = kind.spec(reduce);
    if kind.follows_keyboard() && KEYBOARD.load(Ordering::Relaxed) {
        spec.instant()
    } else {
        spec
    }
}

// ---- Reduce Motion ---------------------------------------------------------------------------

static REDUCE: AtomicBool = AtomicBool::new(false);

/// Whether motion is reduced now: `appearance.reduce_motion` (`on` / `off`), or, at `system`,
/// macOS's Reduce Motion (read live; the `HUTAN_REDUCE_MOTION` variable, `0` / `1`, stands in
/// for it where set).
pub fn reduce_motion() -> bool {
    REDUCE.load(Ordering::Relaxed)
}

/// The inputs of [`reduce_motion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReduceMotion {
    pub system: bool,
    pub setting: ReduceMotionSetting,
}

impl ReduceMotion {
    pub fn effective(self) -> bool {
        match self.setting {
            ReduceMotionSetting::System => self.system,
            ReduceMotionSetting::On => true,
            ReduceMotionSetting::Off => false,
        }
    }
}

/// The global that holds [`ReduceMotion`]; observe it to react to a change.
#[derive(Debug)]
pub struct Inputs(ReduceMotion);

impl gpui::Global for Inputs {}

/// Makes `inputs` the Reduce Motion state of the process and of GPUI (`cx.reduce_motion()`, which
/// its own animations and every helper here read). Windows redraw when the value changed.
pub fn apply(cx: &mut App, inputs: ReduceMotion) {
    if cx.try_global::<Inputs>().is_some_and(|old| old.0 == inputs) {
        return;
    }
    cx.set_global(Inputs(inputs));
    let reduce = inputs.effective();
    REDUCE.store(reduce, Ordering::Relaxed);
    cx.set_reduce_motion(reduce);
}

/// The inputs last given to [`apply`] (macOS off, setting `system` before any).
pub fn inputs(cx: &App) -> ReduceMotion {
    cx.try_global::<Inputs>().map_or(
        ReduceMotion {
            system: false,
            setting: ReduceMotionSetting::System,
        },
        |inputs| inputs.0,
    )
}

/// macOS's Reduce Motion changed (or was read for the first time).
pub fn set_system(cx: &mut App, system: bool) {
    apply(
        cx,
        ReduceMotion {
            system,
            ..inputs(cx)
        },
    );
}

/// `appearance.reduce_motion` changed.
pub fn set_setting(cx: &mut App, setting: ReduceMotionSetting) {
    apply(
        cx,
        ReduceMotion {
            setting,
            ..inputs(cx)
        },
    );
}

// ---- the tween -------------------------------------------------------------------------------

/// A value on its way to a target, started at a time. Pure: the clock is an argument, so the
/// transitions are tested with the test clock and never wait for wall time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
    ease: Ease,
}

impl Tween {
    /// At rest at `value`.
    pub fn at(value: f32, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
            duration: INSTANT,
            ease: Ease::EaseOut,
        }
    }

    /// The value at `now`.
    pub fn value(&self, now: Instant) -> f32 {
        if self.settled(now) {
            return self.to;
        }
        let progress =
            now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * self.ease.apply(progress)
    }

    pub fn settled(&self, now: Instant) -> bool {
        self.duration.is_zero() || now.saturating_duration_since(self.start) >= self.duration
    }

    /// The tween toward `target`: unchanged while it already heads there; otherwise it starts
    /// from the value on screen at `now`, with the duration scaled by the distance left (a
    /// reversal after half a way takes half the time), the enter numbers when `target` is
    /// above the current value and the exit numbers when below.
    pub fn retarget(&mut self, target: f32, spec: Spec, now: Instant) {
        if self.to == target {
            return;
        }
        let current = self.value(now);
        let entering = target > current;
        let (full, ease) = if entering {
            (spec.enter, spec.enter_ease)
        } else {
            (spec.exit, spec.exit_ease)
        };
        *self = Self {
            from: current,
            to: target,
            start: now,
            duration: full.mul_f32((target - current).abs().min(1.0)),
            ease,
        };
    }

    /// Holds the tween at its start until `now` (the content to move was not measured yet).
    fn hold(&mut self, now: Instant) {
        self.start = now;
    }

    pub fn target(&self) -> f32 {
        self.to
    }
}

/// What the animator of a [`Transition`] is given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The eased value, `0` hidden to `1` shown (or whatever range the targets span).
    pub value: f32,
    /// The transition has arrived: nothing more is requested.
    pub settled: bool,
    /// Pixels the surface is still away from its place (`0` under Reduce Motion): `travel × (1 -
    /// value)`.
    pub shift: f32,
    /// Reduce Motion is on.
    pub reduced: bool,
}

impl Frame {
    /// Opacity only.
    pub fn fade<E: Styled>(self, element: E) -> E {
        element.opacity(self.value.clamp(0.0, 1.0))
    }

    /// Opacity and a slide: the surface starts `shift` below its place (above with a negative
    /// travel) and rises into it. A margin, so it moves the surface and not the layout around:
    /// use it on floating (absolute) surfaces.
    pub fn slide_fade<E: Styled>(self, element: E) -> E {
        self.fade(element).mt(px(self.shift))
    }

    /// Invisible and not hit-testable once it arrived at hidden.
    pub fn visible(&self) -> bool {
        !(self.settled && self.value <= 0.0)
    }
}

type Animator<E> = Box<dyn FnOnce(E, Frame) -> E>;

/// An element drawn at a value that moves to a target when the target changes, from wherever it
/// is. Mounting starts at `initial` (the target unless given), so an element that appears for the
/// first time enters; one whose target flips mid-flight reverses without a jump. The state lives
/// in GPUI's element state under `id`: it survives as long as the element is drawn each frame.
///
/// The animator runs on every frame the transition is unsettled, and once more when it settles.
pub struct Transition<E> {
    id: ElementId,
    element: Option<E>,
    kind: Kind,
    target: f32,
    initial: Option<f32>,
    animator: Option<Animator<E>>,
}

impl<E> std::fmt::Debug for Transition<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transition")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl<E> Transition<E> {
    /// Starts from `initial` at the first frame.
    #[must_use]
    pub fn from(mut self, initial: f32) -> Self {
        self.initial = Some(initial);
        self
    }
}

/// A transition toward `target` whose `animate` draws the element at each [`Frame`].
pub fn transition<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    kind: Kind,
    target: f32,
    element: E,
    animate: impl FnOnce(E, Frame) -> E + 'static,
) -> Transition<E> {
    Transition {
        id: id.into(),
        element: Some(element),
        kind,
        target,
        initial: None,
        animator: Some(Box::new(animate)),
    }
}

/// Which way a surface travels while it enters ([`Kind`]'s `travel` pixels; it leaves the way it
/// came).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slide {
    /// Fade only.
    None,
    /// Starts below its place and rises into it: toasts, popovers that open above their trigger.
    Rise,
    /// Starts above its place and settles down: menus that hang from their trigger, sheets.
    Drop,
}

/// `element` between hidden and shown as `wanted` flips: it enters when it first appears (opacity
/// `0 → 1`, plus the slide of `kind` for a floating surface), leaves when `wanted` turns false,
/// and a flip in either direction continues from the value on screen. Settled hidden it is
/// invisible and takes no input. The owner keeps building it while it leaves ([`Ghost`],
/// [`Presence`]); a surface that never leaves passes `true`.
///
/// The slide is a margin, so use `Slide::Rise`/`Slide::Drop` on floating (absolute) surfaces only.
pub fn present<E: IntoElement + Styled + 'static>(
    id: impl Into<ElementId>,
    kind: Kind,
    wanted: bool,
    slide: Slide,
    element: E,
) -> Transition<E> {
    transition(
        id,
        kind,
        if wanted { 1.0 } else { 0.0 },
        element,
        move |element, frame| {
            let element = match slide {
                Slide::None => frame.fade(element),
                Slide::Rise => frame.slide_fade(element),
                Slide::Drop => Frame {
                    shift: -frame.shift,
                    ..frame
                }
                .slide_fade(element),
            };
            if frame.visible() {
                element
            } else {
                element.invisible()
            }
        },
    )
    .from(0.0)
}

/// [`present`] as the last link of an element chain, so a surface built as one long expression
/// gets its motion by appending a line instead of wrapping the expression.
pub trait PresentExt: IntoElement + Styled + Sized + 'static {
    /// `element.presented(id, kind, wanted, slide)` is [`present`]`(id, kind, wanted, slide, element)`.
    fn presented(
        self,
        id: impl Into<ElementId>,
        kind: Kind,
        wanted: bool,
        slide: Slide,
    ) -> Transition<Self> {
        present(id, kind, wanted, slide, self)
    }
}

impl<E: IntoElement + Styled + 'static> PresentExt for E {}

impl<E: IntoElement + 'static> IntoElement for Transition<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: ParentElement> ParentElement for Transition<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        if let Some(element) = &mut self.element {
            element.extend(elements);
        }
    }
}

impl<E: IntoElement + 'static> Element for Transition<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let (Some(element), Some(animator)) = (self.element.take(), self.animator.take()) else {
            let mut empty = div().into_any_element();
            return (empty.request_layout(window, cx), empty);
        };
        let now = cx.background_executor().now();
        let reduce = cx.reduce_motion();
        let spec = spec_now(self.kind, reduce);
        let (target, initial) = (self.target, self.initial);
        let draw = |tween: &mut Tween, window: &mut Window| {
            tween.retarget(target, spec, now);
            let value = tween.value(now);
            let settled = tween.settled(now);
            let frame = Frame {
                value,
                settled,
                shift: if reduce {
                    0.0
                } else {
                    spec.travel * (1.0 - value)
                },
                reduced: reduce,
            };
            if !settled {
                window.request_animation_frame();
            }
            animator(element, frame).into_any_element()
        };
        match global_id {
            Some(global_id) => window.with_element_state(global_id, |state, window| {
                let mut tween = state.unwrap_or_else(|| Tween::at(initial.unwrap_or(target), now));
                let mut element = draw(&mut tween, window);
                ((element.request_layout(window, cx), element), tween)
            }),
            None => {
                let mut tween = Tween::at(target, now);
                let mut element = draw(&mut tween, window);
                (element.request_layout(window, cx), element)
            }
        }
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

// ---- collapse --------------------------------------------------------------------------------

struct CollapseState {
    tween: Tween,
    /// The content's own height, measured while it was drawn at full size.
    natural: Rc<Cell<Option<f32>>>,
}

/// The height of a group that opens and closes: [`expand`].
pub struct Collapse<E> {
    id: ElementId,
    content: Option<E>,
    open: bool,
    grow: bool,
}

impl<E> std::fmt::Debug for Collapse<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Collapse")
            .field("id", &self.id)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

/// `content` below a header that expands and collapses: its height runs `0 ↔ natural` over 150 ms
/// (instant under Reduce Motion), clipped while it moves, plain once it arrived. Mounting open
/// shows it at once; mounting closed draws nothing. The owner builds the content while it is
/// open and while it leaves ([`Presence`]); when it is closed and arrived the content is dropped
/// unread.
pub fn expand<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    open: bool,
    content: E,
) -> Collapse<E> {
    Collapse {
        id: id.into(),
        content: Some(content),
        open,
        grow: false,
    }
}

impl<E> Collapse<E> {
    /// Mounted open, it grows from nothing instead of showing at once: for content the owner
    /// builds only while it is open (and while it leaves, [`Presence`]), so the first frame of
    /// the open state is the first frame of the content.
    #[must_use]
    pub fn from_closed(mut self) -> Self {
        self.grow = true;
        self
    }
}

impl<E: IntoElement + 'static> IntoElement for Collapse<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: IntoElement + 'static> Element for Collapse<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let Some(content) = self.content.take() else {
            let mut empty = div().into_any_element();
            return (empty.request_layout(window, cx), empty);
        };
        let Some(global_id) = global_id else {
            let mut shown = if self.open {
                div().child(content).into_any_element()
            } else {
                div().into_any_element()
            };
            return (shown.request_layout(window, cx), shown);
        };
        let now = cx.background_executor().now();
        let spec = spec_now(Kind::Collapse, cx.reduce_motion());
        let target = if self.open { 1.0 } else { 0.0 };
        window.with_element_state(global_id, |state, window| {
            let mut state = state.unwrap_or_else(|| CollapseState {
                tween: Tween::at(if self.grow { 0.0 } else { target }, now),
                natural: Rc::new(Cell::new(None)),
            });
            let natural = state.natural.get();
            // Opening needs the content's height; the first frame of an expansion draws the
            // content invisibly to measure it and starts the clock the frame after.
            let measuring = target > state.tween.value(now) && natural.is_none();
            if measuring {
                state.tween.hold(now);
            } else {
                state.tween.retarget(target, spec, now);
            }
            let value = state.tween.value(now);
            let settled = state.tween.settled(now) && !measuring;
            if !settled {
                window.request_animation_frame();
            }
            let sink = Rc::clone(&state.natural);
            let measure = canvas(
                move |bounds: Bounds<Pixels>, _, _| sink.set(Some(f32::from(bounds.size.height))),
                |_, (), _, _| {},
            )
            .absolute()
            .size_full();
            let body = div().relative().flex_none().child(content).child(measure);
            let mut element = if settled && value >= 1.0 {
                div().child(body).into_any_element()
            } else if settled {
                div().into_any_element()
            } else if measuring {
                div()
                    .h(px(0.0))
                    .overflow_hidden()
                    .child(body)
                    .into_any_element()
            } else {
                let visible = natural.unwrap_or(0.0) * value;
                div()
                    .flex()
                    .flex_col()
                    .h(px(visible))
                    .overflow_hidden()
                    .opacity(value.clamp(0.0, 1.0))
                    .child(body)
                    .into_any_element()
            };
            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

// ---- leaving ---------------------------------------------------------------------------------

/// What an owner needs to let something leave: an element that stops being wanted disappears from
/// the next render, so the owner keeps building it for the exit time. Call [`Presence::set`] when
/// the wanted state changes and build while [`Presence::draws`] is true.
///
/// ```ignore
/// self.chip_presence.set(self.queue.is_some(), Kind::Chip, cx);
/// if self.chip_presence.draws() { /* build the chip with its Transition */ }
/// ```
#[derive(Debug, Default)]
pub struct Presence {
    wanted: bool,
    /// Until when the exit runs, once the thing is no longer wanted.
    until: Option<Instant>,
    timer: Option<gpui::Task<()>>,
}

impl Presence {
    pub fn new(wanted: bool) -> Self {
        Self {
            wanted,
            until: None,
            timer: None,
        }
    }

    /// The owner wants the thing drawn (`true`) or gone (`false`). Going gone keeps
    /// [`draws`](Self::draws) true for the exit of `kind` and re-renders `V` when it is over.
    pub fn set<V: 'static>(&mut self, wanted: bool, kind: Kind, cx: &mut Context<V>) {
        if wanted == self.wanted {
            return;
        }
        self.wanted = wanted;
        self.timer = None;
        if wanted {
            self.until = None;
            return;
        }
        let exit = kind.exit_duration(cx.reduce_motion());
        if exit.is_zero() {
            self.until = None;
            return;
        }
        self.until = Some(cx.background_executor().now() + exit);
        self.timer = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(exit).await;
            view.update(cx, |_, cx| cx.notify()).ok();
        }));
    }

    /// Whether the owner builds the thing this frame: wanted, or still leaving.
    pub fn draws(&self, now: Instant) -> bool {
        self.wanted || self.until.is_some_and(|until| now < until)
    }

    /// Still wanted, as last [`set`](Self::set).
    pub fn wanted(&self) -> bool {
        self.wanted
    }
}

/// The last value of something that went away, kept for its exit: for owners whose state is
/// cleared when the thing is dismissed (a toast), where [`Presence`] alone has nothing to build.
///
/// ```ignore
/// let toast = self.toast_ghost.follow(self.toast.as_ref(), Kind::Toast, cx)?;
/// // `toast.value` to build from, `toast.wanted` to hand to `motion::present`.
/// ```
#[derive(Debug)]
pub struct Ghost<T> {
    last: Option<T>,
    presence: Presence,
}

/// What [`Ghost::follow`] says to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drawn<T> {
    pub value: T,
    /// `false` while it leaves.
    pub wanted: bool,
}

impl<T> Default for Ghost<T> {
    fn default() -> Self {
        Self {
            last: None,
            presence: Presence::new(false),
        }
    }
}

impl<T: Clone> Ghost<T> {
    /// Call once per render with the current value (`None`: nothing is wanted). `Some` while it
    /// is wanted and while it leaves; `None` once the exit is over.
    pub fn follow<V: 'static>(
        &mut self,
        current: Option<&T>,
        kind: Kind,
        cx: &mut Context<V>,
    ) -> Option<Drawn<T>> {
        if let Some(value) = current {
            self.last = Some(value.clone());
            self.presence.set(true, kind, cx);
            return Some(Drawn {
                value: value.clone(),
                wanted: true,
            });
        }
        self.presence.set(false, kind, cx);
        let now = cx.background_executor().now();
        if !self.presence.draws(now) {
            self.last = None;
        }
        self.last.clone().map(|value| Drawn {
            value,
            wanted: false,
        })
    }
}

/// The [`Presence`] of every overlay a view owns, by name: one field instead of one per overlay.
#[derive(Debug, Default)]
pub struct Presences {
    by_name: std::collections::HashMap<&'static str, Presence>,
}

impl Presences {
    /// Call once per render of the overlay `name` with whether it is wanted. `Some(wanted)` while
    /// it is drawn (`false`: it is leaving), `None` when nothing is drawn.
    pub fn track<V: 'static>(
        &mut self,
        name: &'static str,
        wanted: bool,
        kind: Kind,
        cx: &mut Context<V>,
    ) -> Option<bool> {
        let presence = self.by_name.entry(name).or_default();
        presence.set(wanted, kind, cx);
        presence
            .draws(cx.background_executor().now())
            .then_some(wanted)
    }
}

// ---- hover and pressed colour ----------------------------------------------------------------

/// The colour `t` of the way from `from` to `to` (in RGB; hue does not wrap).
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (from.into(), to.into());
    let t = t.clamp(0.0, 1.0);
    Hsla::from(Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    })
}

/// The background at transition value `value` of [`hover_fade`]: `0` at rest, `1` hovered, `2`
/// pressed.
pub fn tint(value: f32, rest: Hsla, hover: Hsla, pressed: Option<Hsla>) -> Hsla {
    if value <= 1.0 {
        mix(rest, hover, value)
    } else {
        mix(hover, pressed.unwrap_or(hover), value - 1.0)
    }
}

#[derive(Default)]
struct Pointer {
    hovered: Cell<bool>,
    pressed: Cell<bool>,
}

/// Fades the background of a button or row between `rest`, `hover` and `pressed` over 100 ms
/// (the pointer state is tracked here; the element needs a stateful id). Keyboard focus is not
/// part of it: the focus ring stays instant. `pressed` is optional: without it a press keeps the
/// hover colour.
///
/// Call it last: `motion::hover_fade(id, element.on_click(..), rest, hover, None, window, cx)`.
pub fn hover_fade<E>(
    id: impl Into<ElementId>,
    element: E,
    rest: Hsla,
    hover: Hsla,
    pressed: Option<Hsla>,
    window: &mut Window,
    cx: &mut App,
) -> Transition<E>
where
    E: IntoElement + Styled + InteractiveElement + StatefulInteractiveElement + 'static,
{
    let id = id.into();
    let view = window.current_view();
    let pointer = window
        .use_keyed_state(id.clone(), cx, |_, _| Rc::new(Pointer::default()))
        .read(cx)
        .clone();
    let target = match (pointer.hovered.get(), pointer.pressed.get(), pressed) {
        (true, true, Some(_)) => 2.0,
        (true, _, _) => 1.0,
        _ => 0.0,
    };
    let (on_hover, on_down) = (Rc::clone(&pointer), Rc::clone(&pointer));
    let element = element
        .on_hover(move |hovered, _, cx| {
            on_hover.hovered.set(*hovered);
            if !*hovered {
                on_hover.pressed.set(false);
            }
            cx.notify(view);
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            on_down.pressed.set(true);
            cx.notify(view);
        })
        .on_mouse_up(MouseButton::Left, {
            let pointer = Rc::clone(&pointer);
            move |_, _, cx| {
                pointer.pressed.set(false);
                cx.notify(view);
            }
        });
    transition(
        ElementId::from((id, "hover")),
        Kind::Hover,
        target,
        element,
        move |element, frame| element.bg(tint(frame.value, rest, hover, pressed)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> Instant {
        Instant::now()
    }

    #[test]
    fn the_curves_start_at_zero_end_at_one_and_never_dip() {
        for ease in [
            Ease::Linear,
            Ease::Ease,
            Ease::EaseOut,
            Ease::EaseInOut,
            Ease::Drawer,
        ] {
            assert!(ease.apply(0.0).abs() < 1e-6);
            assert!((ease.apply(1.0) - 1.0).abs() < 1e-6);
            let mut last = 0.0;
            for step in 0..=100 {
                let value = ease.apply(step as f32 / 100.0);
                assert!(value + 1e-4 >= last, "{ease:?} dips at step {step}");
                last = value;
            }
        }
    }

    #[test]
    fn ease_out_is_quick_at_the_start() {
        // cubic-bezier(0.23, 1, 0.32, 1) is over 75 % of the way a quarter into the time.
        assert!(Ease::EaseOut.apply(0.25) > 0.75);
        assert!(Ease::EaseOut.apply(0.25) > Ease::Linear.apply(0.25));
        // The reference value of the CSS curve at t = 0.5 (solved by bisection in Python).
        assert!((Ease::EaseOut.apply(0.5) - 0.966).abs() < 0.002);
    }

    #[test]
    fn exit_is_shorter_than_enter_and_nothing_exceeds_the_ceiling() {
        for kind in [
            Kind::Hover,
            Kind::Popover,
            Kind::Sheet,
            Kind::Toast,
            Kind::Collapse,
            Kind::Chip,
            Kind::TabClose,
        ] {
            for reduce in [false, true] {
                let spec = kind.spec(reduce);
                assert!(spec.exit <= spec.enter, "{kind:?}");
                assert!(spec.enter <= MAX, "{kind:?}");
                if reduce {
                    assert!(spec.enter <= BASE && spec.travel == 0.0, "{kind:?}");
                }
            }
        }
        assert_eq!(Kind::Popover.spec(false).enter, BASE);
        assert_eq!(Kind::Popover.spec(false).exit, FAST);
        assert_eq!(Kind::Sheet.spec(false).enter, SLOW);
        assert_eq!(Kind::Sheet.spec(false).exit, BASE);
        assert_eq!(Kind::Collapse.spec(true).enter, INSTANT);
    }

    #[test]
    fn a_tween_reaches_its_target_at_the_token_duration() {
        let t0 = clock();
        let spec = Kind::Popover.spec(false);
        let mut tween = Tween::at(0.0, t0);
        tween.retarget(1.0, spec, t0);
        assert_eq!(tween.value(t0), 0.0);
        assert!(!tween.settled(t0 + BASE - Duration::from_millis(1)));
        assert!(tween.settled(t0 + BASE));
        assert_eq!(tween.value(t0 + BASE), 1.0);
        let middle = tween.value(t0 + BASE / 2);
        assert!(middle > 0.5 && middle < 1.0, "{middle}");
    }

    #[test]
    fn a_reversal_continues_from_the_value_on_screen() {
        let t0 = clock();
        let spec = Kind::Popover.spec(false);
        let mut tween = Tween::at(0.0, t0);
        tween.retarget(1.0, spec, t0);
        let at = t0 + Duration::from_millis(40);
        let on_screen = tween.value(at);
        tween.retarget(0.0, spec, at);
        assert!((tween.value(at) - on_screen).abs() < 1e-6, "no jump");
        // The exit is FAST scaled by the distance left.
        let ends = at + FAST.mul_f32(on_screen);
        assert!(tween.settled(ends));
        assert!(!tween.settled(ends - Duration::from_millis(2)));
        assert_eq!(tween.value(ends), 0.0);
    }

    #[test]
    fn retargeting_the_same_target_keeps_the_clock() {
        let t0 = clock();
        let spec = Kind::Popover.spec(false);
        let mut tween = Tween::at(0.0, t0);
        tween.retarget(1.0, spec, t0);
        let before = tween;
        tween.retarget(1.0, spec, t0 + Duration::from_millis(30));
        assert_eq!(tween, before);
    }

    #[test]
    fn reduce_motion_inputs_combine() {
        let on = ReduceMotion {
            system: false,
            setting: ReduceMotionSetting::On,
        };
        let off = ReduceMotion {
            system: true,
            setting: ReduceMotionSetting::Off,
        };
        let system = |system| ReduceMotion {
            system,
            setting: ReduceMotionSetting::System,
        };
        assert!(on.effective());
        assert!(!off.effective());
        assert!(system(true).effective());
        assert!(!system(false).effective());
    }

    #[test]
    fn the_tint_runs_rest_hover_pressed() {
        let (rest, hover, pressed) = (
            gpui::hsla(0.0, 0.0, 0.0, 1.0),
            gpui::hsla(0.0, 0.0, 0.5, 1.0),
            gpui::hsla(0.0, 0.0, 1.0, 1.0),
        );
        assert_eq!(tint(0.0, rest, hover, Some(pressed)), rest);
        assert!((tint(1.0, rest, hover, Some(pressed)).l - 0.5).abs() < 1e-3);
        assert!((tint(2.0, rest, hover, Some(pressed)).l - 1.0).abs() < 1e-3);
        assert!(
            (tint(2.0, rest, hover, None).l - 0.5).abs() < 1e-3,
            "without a pressed colour a press keeps the hover colour"
        );
    }

    #[test]
    fn mixing_colors_hits_both_ends() {
        let (a, b) = (
            gpui::hsla(0.0, 0.0, 0.0, 1.0),
            gpui::hsla(0.0, 0.0, 1.0, 1.0),
        );
        assert_eq!(mix(a, b, 0.0), a);
        assert!((mix(a, b, 1.0).l - 1.0).abs() < 1e-4);
        let middle: Rgba = mix(a, b, 0.5).into();
        assert!((middle.r - 0.5).abs() < 1e-3);
    }
}
