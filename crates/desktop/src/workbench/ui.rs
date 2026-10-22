//! The component kit of the visual system (design.md § Visual system, v2; the quiet workbench of
//! `docs/spikes/2026-10-21-clean-aesthetic.md` §3): buttons, pills, state labels, popover and card
//! surfaces, the keyboard focus ring, prose text, section labels and the empty state. Views call
//! these instead of spelling out colors, radii and fonts, so the look stays in one place.
//!
//! Drawing rules the kit enforces: a card is a fill, never a border; a hairline outlines only
//! text inputs and secondary buttons; a selected chip or row is an `accent_soft` fill without an
//! outline; the accent is never a border (the drag-over targets are the one exception, guarded by
//! the test at the end of this file); state is a glyph and a word, never a tinted pill.

pub mod motion;

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::sync::{Arc, LazyLock};

use gpui::{
    AnyElement, App, DispatchPhase, FontWeight, Hsla, Image, ImageFormat, InteractiveElement,
    IntoElement, Keystroke, MouseButton, MouseDownEvent, ObjectFit, ParentElement, SharedString,
    Stateful, Styled, StyledImage, canvas, div, img, prelude::FluentBuilder as _, px, relative,
    transparent_black,
};
use hutan_settings::themes::Fonts;

use crate::theme::{Theme, Tokens, mascot_size, pill_radius};

mod button;
pub use button::{Button, ButtonKind, ButtonSize, IconButton, approval_button_kind, spinner};

/// The font files macOS ships New York in. GPUI cannot select the system serif by name (CoreText
/// lists it as `.New York`, hidden from family lookups), so the files are registered with the
/// text system and found under that family name. A pack lists `.New York` (or `New York`) in its
/// `[fonts] prose` like any other family; it is no file path a pack can name.
const NEW_YORK_FILES: [&str; 2] = [
    "/System/Library/Fonts/NewYork.ttf",
    "/System/Library/Fonts/NewYorkItalic.ttf",
];
/// The system UI family: always available, and what GPUI falls back to for an unknown name.
const SYSTEM_UI: &str = ".SystemUIFont";

/// The families the committed pack's `[fonts]` resolved to on this machine.
#[derive(Clone)]
struct Families {
    ui: SharedString,
    prose: SharedString,
}

thread_local! {
    /// Read by views without a `cx`; written by [`apply_fonts`] on the UI thread. Before any
    /// pack is applied: the system UI font, and Georgia for prose.
    static FAMILIES: RefCell<Families> = RefCell::new(Families {
        ui: SYSTEM_UI.into(),
        prose: "Georgia".into(),
    });
}

/// Registers the New York font files with the text system, then applies the built-in pack's
/// fonts. Call at startup, before the first window.
pub fn init_prose_font(cx: &App) {
    let files: Vec<Cow<'static, [u8]>> = NEW_YORK_FILES
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(Cow::Owned)
        .collect();
    if !files.is_empty()
        && let Err(error) = cx.text_system().add_fonts(files)
    {
        tracing::warn!("cannot load New York: {error}");
    }
    let builtin = hutan_settings::themes::builtin_theme(hutan_settings::themes::Builtin::HutanDark);
    apply_fonts(cx, builtin.fonts());
}

/// The first of `candidates` that is installed (the system UI font always is); the last one when
/// none is, so a pack that lists only missing families still names something. A family GPUI
/// cannot find resolves to the UI face, which is how "missing" is detected.
pub fn pick_family(cx: &App, candidates: &[String]) -> SharedString {
    let text_system = cx.text_system();
    let ui_face = text_system.resolve_font(&gpui::font(SYSTEM_UI));
    candidates
        .iter()
        .find(|name| {
            name.as_str() == SYSTEM_UI
                || text_system.resolve_font(&gpui::font(name.as_str())) != ui_face
        })
        .or_else(|| candidates.last())
        .map_or_else(|| SYSTEM_UI.into(), |name| SharedString::from(name.clone()))
}

/// Resolves the pack's UI and prose family lists against the installed fonts and makes them the
/// ones [`ui_family`] and [`prose_family`] return.
pub fn apply_fonts(cx: &App, fonts: &Fonts) {
    let families = Families {
        ui: pick_family(cx, &fonts.ui),
        prose: pick_family(cx, &fonts.prose),
    };
    if families.prose.as_ref() != SYSTEM_UI && families.prose.as_ref() != ".New York" {
        tracing::info!("prose font: {}", families.prose);
    }
    FAMILIES.with(|f| *f.borrow_mut() = families);
}

/// The chrome's text family.
pub fn ui_family() -> SharedString {
    FAMILIES.with(|f| f.borrow().ui.clone())
}

/// The prose family ([`apply_fonts`]); Georgia until it has run.
pub fn prose_family() -> SharedString {
    FAMILIES.with(|f| f.borrow().prose.clone())
}

/// A GPUI `Font` for prose text.
pub fn prose_font() -> gpui::Font {
    gpui::font(prose_family())
}

/// Style helpers on any GPUI element.
pub trait StyledExt: Styled + Sized {
    /// Prose type: serif, 15 px, line height 1.6.
    fn prose(self, tokens: &Tokens) -> Self {
        self.font_family(prose_family())
            .text_size(px(tokens.prose_font_size))
            .line_height(relative(tokens.prose_line_height))
    }

    /// Medium weight: the heaviest weight the UI uses besides markdown strong text.
    fn medium(self) -> Self {
        self.font_weight(FontWeight::MEDIUM)
    }

    /// At most `lines` lines of text, the cut one ending in "…".
    fn clamp_lines(self, lines: usize) -> Self {
        self.line_clamp(lines).text_ellipsis()
    }

    /// A card: a `surface` fill with the card radius. No shadow: cards are fills. On a light
    /// pack the fill is 1.05:1 against `bg`, so it carries a 6 % hairline ([`Theme::card_edge`]);
    /// on a dark pack the fill alone is the card.
    fn card(self, theme: &Theme, tokens: &Tokens) -> Self {
        self.bg(theme.surface)
            .rounded(px(tokens.card_radius))
            .light_edge(theme)
    }

    /// The light packs' 6 % hairline around a `surface` fill that would otherwise melt into
    /// `bg`; nothing on a dark pack.
    fn light_edge(self, theme: &Theme) -> Self {
        if theme.dark {
            self
        } else {
            self.border_1().border_color(theme.card_edge())
        }
    }

    /// A text input or a secondary button: `surface` with a 1 px `border` hairline and the
    /// control radius. The one shape that keeps a hairline of its own.
    fn outlined(self, theme: &Theme, tokens: &Tokens) -> Self {
        self.bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(tokens.radius))
    }

    /// A floating surface (popover, menu, palette, modal): a card with the one soft shadow and
    /// 6 px inner padding.
    fn popover(mut self, theme: &Theme, tokens: &Tokens) -> Self {
        self.style().box_shadow = Some(theme.shadow());
        self.bg(theme.surface)
            .rounded(px(tokens.card_radius))
            .p(px(6.0))
    }

    /// A toast: a popover with the light packs' hairline and a deeper shadow
    /// ([`Theme::toast_shadow`]), because it floats over content of every tone.
    fn toast(mut self, theme: &Theme, tokens: &Tokens) -> Self {
        self.style().box_shadow = Some(theme.toast_shadow());
        self.popover(theme, tokens).light_edge(theme)
    }

    /// The shared geometry of a row in a list or menu; the caller adds the hover and selected
    /// fills (`surface_hover`, `accent_soft`).
    fn row(self, height: f32, tokens: &Tokens) -> Self {
        self.flex()
            .items_center()
            .h(px(height))
            .px(px(tokens.spacing + 2.0))
            .rounded(px(tokens.radius))
    }
}

impl<T: Styled + Sized> StyledExt for T {}

thread_local! {
    /// Whether the last input that could move focus was a key (a Tab or arrow, or a keystroke
    /// that moved focus) rather than a mouse press: the keyboard focus ring shows only then.
    /// GPUI has no focus-visible notion, so the workbench keeps it: set by its keystroke
    /// observer, cleared by [`keyboard_focus_observer`] on any mouse press.
    static KEYBOARD_FOCUS: Cell<bool> = const { Cell::new(false) };
}

/// Whether focus came from the keyboard (see [`KEYBOARD_FOCUS`]).
pub fn keyboard_focus() -> bool {
    KEYBOARD_FOCUS.with(Cell::get)
}

/// Records where focus came from; `true` when that changed, so the caller repaints.
pub fn set_keyboard_focus(from_keyboard: bool) -> bool {
    KEYBOARD_FOCUS.with(|cell| cell.replace(from_keyboard) != from_keyboard)
}

/// A key that walks focus or a selection (Tab, the arrows, Home, End, Page Up, Page Down).
/// Typing text does not count: a field clicked into keeps no ring while the user types.
pub fn navigates(keystroke: &Keystroke) -> bool {
    matches!(
        keystroke.key.as_str(),
        "tab" | "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown"
    )
}

/// An invisible element whose paint registers a window-level mouse-down observer for the frame:
/// any press, inside an overlay or not, ends keyboard focus and hides the ring. The workbench
/// root carries one.
pub fn keyboard_focus_observer() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            window.on_mouse_event(|_: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Capture {
                    motion::note_input(false);
                }
                if phase == DispatchPhase::Capture && set_keyboard_focus(false) {
                    window.refresh();
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// Whether the ring draws: the element has focus and focus came from the keyboard, never from
/// a mouse press.
pub fn ring_shown(focused: bool, from_keyboard: bool) -> bool {
    focused && from_keyboard
}

/// The keyboard focus ring on any element with children.
pub trait FocusRingExt: ParentElement + Styled + Sized {
    /// While `focused` and focus came from the keyboard: a 2 px ring in `focus` (`ui.focus`, or
    /// the macOS keyboard focus color) 1 px outside the element, rounded to the control radius
    /// plus that gap. Drawn as an absolute child, so it changes no layout; nothing at all when
    /// focus came from the mouse.
    fn focus_ring(self, focused: bool, theme: &Theme, tokens: &Tokens) -> Self {
        if !ring_shown(focused, keyboard_focus()) {
            return self;
        }
        // 1 px gap + 2 px ring outside the element's edge.
        const OUTSET: f32 = 3.0;
        self.relative().child(
            div()
                .absolute()
                .top(px(-OUTSET))
                .left(px(-OUTSET))
                .right(px(-OUTSET))
                .bottom(px(-OUTSET))
                .border_2()
                .border_color(theme.focus)
                .rounded(px(tokens.radius + OUTSET)),
        )
    }
}

impl<T: ParentElement + Styled + Sized> FocusRingExt for T {}

/// The action row at the foot of a dialog or sheet: right-aligned, 12 px between buttons.
/// Cancel goes first and the one primary (or the destructive confirm) last.
pub fn dialog_footer() -> gpui::Div {
    div().flex().justify_end().items_center().gap(px(12.0))
}

/// The widest a [`pill`] gets before its text is cut with "…".
const PILL_MAX_WIDTH: f32 = 140.0;

/// A small neutral label ("exit 0", a branch, "inferred"): `fg` on `surface_hover`, fully
/// rounded. Never in a state's color: a state is a [`state_label`] (a glyph and a word).
pub fn pill(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        // A long branch or label is cut with "…" instead of pushing its row wider.
        .max_w(px(PILL_MAX_WIDTH))
        .truncate()
        .px(px(tokens.spacing * 2.0))
        .rounded(px(pill_radius()))
        .bg(theme.surface_hover)
        .text_color(theme.fg)
        .text_size(px(tokens.secondary_font_size))
        .child(text.into())
}

/// A state: `glyph` in `tone` (a state color, `accent` for working, `muted` for idle) and `word`
/// beside it at the secondary size. The word takes the tone too, except on a selected row
/// (`on_selected`), where state colors are not AA in light and the word is `fg`, and for the
/// accent, which is spent on the glyph only. No fill, no border.
pub fn state_label(
    glyph: crate::icons::Icon,
    word: impl Into<SharedString>,
    tone: Hsla,
    on_selected: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    let word_color = if on_selected || tone == theme.accent {
        theme.fg
    } else {
        tone
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(tokens.spacing))
        .max_w(px(PILL_MAX_WIDTH))
        .text_size(px(tokens.secondary_font_size))
        .child(
            div()
                .flex_none()
                .text_color(tone)
                .child(crate::icons::glyph(glyph)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(word_color)
                .child(word.into()),
        )
}

/// The glyph of a state drawn in `tone`: done, a request, failed, working, or idle.
pub fn state_glyph(tone: Hsla, theme: &Theme) -> crate::icons::Icon {
    use crate::icons::Icon;
    if tone == theme.success {
        Icon::StateCompleted
    } else if tone == theme.warning {
        Icon::StateWarning
    } else if tone == theme.danger {
        Icon::StateFailed
    } else if tone == theme.accent {
        Icon::StateReview
    } else {
        Icon::StateIdle
    }
}

/// A counter pill: tabular figures are the platform default for SF digits in this size.
pub fn counter(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        .px(px(tokens.spacing + 2.0))
        .rounded(px(pill_radius()))
        .bg(theme.surface_hover)
        .text_color(theme.muted)
        .text_size(px(tokens.secondary_font_size))
        .child(text.into())
}

/// A section label: 11 px medium, `muted`, sentence case.
pub fn section_label(text: impl Into<SharedString>, theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .text_size(px(tokens.section_font_size))
        .medium()
        .text_color(theme.muted)
        .child(text.into())
}

/// The leading mark of a picker row: a check for the one in use (the current model), blank
/// otherwise, so the names of all rows line up.
pub fn current_mark(current: bool, theme: &Theme) -> gpui::Div {
    div()
        .flex_none()
        .w(px(14.0))
        .text_color(theme.accent)
        .when(current, |s| {
            s.child(crate::icons::glyph(crate::icons::Icon::StateCompleted))
        })
}

/// Which mascot image a view shows (`assets/brand/05-mascot/png`): the states of the empty
/// states, and the poses the first-run setup uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mascot {
    /// Nothing here yet.
    Empty,
    /// Loading, or an agent is working.
    Thinking,
    /// All clear, finished.
    Success,
    /// Something failed.
    Error,
    /// The daemon is not running: idle.
    Sleep,
    /// Looking straight at you: a welcome.
    AngleFront,
    /// Three-quarter view from the left.
    Angle34Left,
    /// Looking down, at the prompt.
    AngleLookDown,
    /// Concentrating.
    Focus,
}

impl Mascot {
    pub const ALL: [Mascot; 9] = [
        Mascot::Empty,
        Mascot::Thinking,
        Mascot::Success,
        Mascot::Error,
        Mascot::Sleep,
        Mascot::AngleFront,
        Mascot::Angle34Left,
        Mascot::AngleLookDown,
        Mascot::Focus,
    ];

    /// The file name under `assets/brand/05-mascot/png`.
    pub fn file(self) -> &'static str {
        match self {
            Mascot::Empty => "state-empty@2x.png",
            Mascot::Thinking => "state-thinking@2x.png",
            Mascot::Success => "state-success@2x.png",
            Mascot::Error => "state-error@2x.png",
            Mascot::Sleep => "state-sleep@2x.png",
            Mascot::AngleFront => "angle-front@2x.png",
            Mascot::Angle34Left => "angle-34-left@2x.png",
            Mascot::AngleLookDown => "angle-look-down@2x.png",
            Mascot::Focus => "state-focus@2x.png",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Mascot::Empty => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-empty@2x.png")
            }
            Mascot::Thinking => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-thinking@2x.png")
            }
            Mascot::Success => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-success@2x.png")
            }
            Mascot::Error => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-error@2x.png")
            }
            Mascot::Sleep => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-sleep@2x.png")
            }
            Mascot::AngleFront => {
                include_bytes!("../../../../assets/brand/05-mascot/png/angle-front@2x.png")
            }
            Mascot::Angle34Left => {
                include_bytes!("../../../../assets/brand/05-mascot/png/angle-34-left@2x.png")
            }
            Mascot::AngleLookDown => {
                include_bytes!("../../../../assets/brand/05-mascot/png/angle-look-down@2x.png")
            }
            Mascot::Focus => {
                include_bytes!("../../../../assets/brand/05-mascot/png/state-focus@2x.png")
            }
        }
    }

    fn image(self) -> Arc<Image> {
        static IMAGES: LazyLock<[Arc<Image>; 9]> = LazyLock::new(|| {
            Mascot::ALL.map(|m| Arc::new(Image::from_bytes(ImageFormat::Png, m.bytes().to_vec())))
        });
        let index = Mascot::ALL.iter().position(|m| *m == self).unwrap_or(0);
        IMAGES[index].clone()
    }
}

/// The mascot image at `size` px.
pub fn mascot(state: Mascot, size: f32) -> gpui::Img {
    img(state.image())
        .size(px(size))
        .flex_none()
        .object_fit(ObjectFit::Contain)
}

/// An empty state (`docs/design.md` § Visual system): one sentence that teaches, at most one
/// action, optionally one muted hint (the shortcut). A center surface leads with the mascot and
/// says its sentence in serif prose; a sidebar or panel says it as one muted line of text, with
/// a ghost button for its action ([`EmptyState::new`] without [`EmptyState::mascot`]).
pub struct EmptyState {
    mascot: Option<Mascot>,
    text: SharedString,
    action: Option<AnyElement>,
    hint: Option<SharedString>,
}

impl std::fmt::Debug for EmptyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmptyState")
            .field("mascot", &self.mascot)
            .field("text", &self.text)
            .field("hint", &self.hint)
            .finish_non_exhaustive()
    }
}

impl EmptyState {
    /// A text-only empty state (sidebars, panels).
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            mascot: None,
            text: text.into(),
            action: None,
            hint: None,
        }
    }

    /// A center surface's: the mascot at the pack's mascot size above a serif sentence.
    pub fn mascot(mut self, state: Mascot) -> Self {
        self.mascot = Some(state);
        self
    }

    /// The one action (a secondary button, primary only when it is the surface's default
    /// commit; a ghost button in a sidebar or panel).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// One muted line under the action, e.g. the shortcut that does the same.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn render(self, theme: &Theme, tokens: &Tokens) -> gpui::Div {
        let center = self.mascot.is_some();
        let sentence = div()
            .w_full()
            .min_w_0()
            .max_w(px(420.0))
            .text_center()
            .text_color(theme.muted);
        let sentence = if center {
            sentence.prose(tokens)
        } else {
            sentence.text_size(px(tokens.secondary_font_size))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing * if center { 3.0 } else { 2.0 }))
            .p(px(tokens.spacing * 4.0))
            .when_some(self.mascot, |s, state| {
                s.child(mascot(state, mascot_size()))
            })
            .child(sentence.child(self.text))
            .when_some(self.action, |s, action| s.child(action))
            .when_some(self.hint, |s, hint| {
                s.child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.muted)
                        .child(hint),
                )
            })
    }
}

/// A center surface's empty state: the mascot, one serif sentence, optionally one action
/// ([`EmptyState`] with a mascot).
pub fn empty_state(
    state: Mascot,
    text: impl Into<SharedString>,
    action: Option<AnyElement>,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    let empty = EmptyState::new(text).mascot(state);
    match action {
        Some(action) => empty.action(action),
        None => empty,
    }
    .render(theme, tokens)
}

/// A serif headline (the prose family at 24 px, `fg`): the title of a first-run step.
pub fn headline(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .font_family(prose_family())
        .text_size(px(24.0))
        .line_height(relative(1.25))
        .text_color(theme.fg)
        .child(text.into())
}

/// One option of a choice (appearance, density, the assistant's backend): an outlined `surface`
/// button with `muted` text; the selected one is an `accent_soft` fill with `fg` medium text and
/// no outline (its hairline turns transparent, so selecting moves nothing).
pub fn choice(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<gpui::Div> {
    let hover = theme.surface_hover;
    div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(tokens.control_height))
        .px(px(12.0))
        .outlined(theme, tokens)
        .text_size(px(tokens.font_size))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(selected, |s| {
            s.border_color(transparent_black())
                .bg(theme.accent_soft)
                .text_color(theme.fg)
                .medium()
        })
        .when(!selected, |s| {
            s.text_color(theme.muted).hover(move |s| s.bg(hover))
        })
        .child(label.into())
}

/// The one silhouette of a segmented control: an outlined `surface` strip with 2 px inside it,
/// holding [`segment`]s.
pub fn segmented(theme: &Theme, tokens: &Tokens) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .p(px(2.0))
        .outlined(theme, tokens)
}

/// One option of a [`segmented`] control: text without an outline, `muted`; the selected one is
/// an `accent_soft` fill with `fg` medium text. Its radius is the strip's minus the 3 px between
/// their edges (hairline and inset), so the two corners stay concentric.
pub fn segment(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<gpui::Div> {
    let hover = theme.surface_hover;
    div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        // The strip's 1 px hairline and 2 px inset on each side keep the control height.
        .h(px(tokens.control_height - 6.0))
        .px(px(10.0))
        .rounded(px((tokens.radius - 3.0).max(2.0)))
        .text_size(px(tokens.font_size))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(selected, |s| {
            s.bg(theme.accent_soft).text_color(theme.fg).medium()
        })
        .when(!selected, |s| {
            s.text_color(theme.muted).hover(move |s| s.bg(hover))
        })
        .child(label.into())
}

/// An on/off switch, 34 × 20 px: `accent` track with the knob at the right when on, a hairline
/// track with the knob at the left when off. Dim and inert when disabled (the caller adds the
/// click handler only when enabled).
pub fn switch(
    id: impl Into<gpui::ElementId>,
    on: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<gpui::Div> {
    let track = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .w(px(34.0))
        .h(px(20.0))
        .px(px(2.0))
        .rounded(px(pill_radius()))
        .border_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(on, |s| {
            // The fill shows through: an "on" track has no outline of its own.
            s.bg(theme.accent)
                .border_color(transparent_black())
                .justify_end()
        })
        .when(!on, |s| {
            s.bg(theme.surface_hover)
                .border_color(theme.border)
                .justify_start()
        })
        .child(div().size(px(14.0)).rounded(px(pill_radius())).bg(if on {
            theme.on_accent()
        } else {
            theme.surface
        }));
    if enabled {
        track.cursor_pointer()
    } else {
        track.opacity(0.45)
    }
}

/// The dots of a stepped flow: `total` of them, the `current` one in `accent`, the others
/// `border`.
pub fn step_dots(current: usize, total: usize, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.0))
        .children((0..total).map(|index| {
            div()
                .size(px(if index == current { 8.0 } else { 6.0 }))
                .rounded(px(pill_radius()))
                .bg(if index == current {
                    theme.accent
                } else {
                    theme.border
                })
        }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_focus_ring_shows_only_for_keyboard_focus() {
        assert!(super::ring_shown(true, true));
        assert!(
            !super::ring_shown(true, false),
            "a mouse click leaves no ring"
        );
        assert!(
            !super::ring_shown(false, true),
            "an unfocused button has none"
        );
    }

    /// The only places the accent may color a border: drag-over targets, per file (relative to
    /// `crates/desktop/src`), with how many there are. A new drop target is added here on
    /// purpose; anything else is a fill, the keyboard focus ring, or nothing.
    const DRAG_TARGETS: [(&str, usize); 4] = [
        ("workbench/chat/rich_view.rs", 1),
        ("workbench/panels/sessions.rs", 2),
        ("workbench/render.rs", 3),
        ("workbench/sidebars.rs", 1),
    ];

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// The argument of every border color call in `source` (up to its balanced closing
    /// parenthesis, over several lines if need be), with the 0-based line it starts on.
    fn border_colors(source: &str) -> Vec<(usize, &str)> {
        // Spelled out of pieces so this test does not find itself.
        let needle = ["border", "_color("].concat();
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = source[from..].find(&needle) {
            let start = from + at + needle.len();
            let mut depth = 1;
            let mut end = start;
            for (i, c) in source[start..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            found.push((source[..start].matches('\n').count(), &source[start..end]));
            from = end.max(start);
        }
        found
    }

    /// `accent` as a name of its own (`theme.accent`, `accent.opacity(…)`, `accent`), not
    /// `accent_soft`, `sidebar_accent` or `on_accent()`.
    fn names_accent(argument: &str) -> bool {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        argument.match_indices("accent").any(|(at, _)| {
            let before = argument[..at].chars().next_back();
            let after = argument[at + "accent".len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
    }

    #[test]
    fn the_accent_is_a_border_only_on_drag_targets() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let mut drag_targets: BTreeMap<String, usize> = BTreeMap::new();
        let mut offenders = Vec::new();
        let mut checked = 0;
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            let name = path
                .strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let lines: Vec<&str> = source.lines().collect();
            for (line, argument) in border_colors(&source) {
                checked += 1;
                if !names_accent(argument) {
                    continue;
                }
                // A drag-over style: the call sits in the `drag_over` closure on its own line
                // or on one of the two lines above it.
                let drag = lines[line.saturating_sub(2)..=line]
                    .iter()
                    .any(|l| l.contains("drag_over"));
                if drag {
                    *drag_targets.entry(name.clone()).or_default() += 1;
                } else {
                    offenders.push(format!("{name}:{}: {}", line + 1, lines[line].trim()));
                }
            }
        }
        assert!(
            checked > 50,
            "only {checked} border colors found under {src:?}"
        );
        assert!(
            offenders.is_empty(),
            "accent borders outside drag targets (use a fill, the focus ring or nothing):\n{}",
            offenders.join("\n")
        );
        let expected: BTreeMap<String, usize> = DRAG_TARGETS
            .iter()
            .map(|(file, count)| ((*file).to_owned(), *count))
            .collect();
        assert_eq!(drag_targets, expected, "drag targets with an accent edge");
    }

    #[test]
    fn the_guard_reads_accent_names_and_nested_calls() {
        // `BC` stands for the call's name, which this file must not spell out.
        let source = "a.BC(if on { theme.accent } else { f(theme.border) })\n\
                      b.BC(theme.accent_soft)\nc.BC(accent.opacity(0.5))"
            .replace("BC", &["border", "_color"].concat());
        let found = border_colors(&source);
        assert_eq!(
            found,
            vec![
                (0, "if on { theme.accent } else { f(theme.border) }"),
                (1, "theme.accent_soft"),
                (2, "accent.opacity(0.5)"),
            ]
        );
        let accents: Vec<bool> = found.iter().map(|(_, a)| names_accent(a)).collect();
        assert_eq!(accents, [true, false, true]);
        assert!(!names_accent("theme.sidebar_accent"));
        assert!(!names_accent("theme.on_accent()"));
    }
}
