//! The component kit of the visual system (design.md § Visual system, v2): buttons, pills,
//! popover and card surfaces, prose text, section labels and the mascot empty state. Views call
//! these instead of spelling out colors, radii and fonts, so the look stays in one place.

use std::borrow::Cow;
use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use gpui::{
    AnyElement, App, FontWeight, Hsla, Image, ImageFormat, InteractiveElement, MouseButton,
    ObjectFit, ParentElement, SharedString, Stateful, Styled, StyledImage, div, img,
    prelude::FluentBuilder as _, px, relative,
};
use hutan_settings::themes::Fonts;

use crate::theme::{Theme, Tokens, mascot_size, pill_radius};

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

    /// A card: `surface`, hairline border, card radius. No shadow.
    fn card(self, theme: &Theme, tokens: &Tokens) -> Self {
        self.bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(tokens.card_radius))
    }

    /// A floating surface (popover, menu, palette, modal): a card with the one soft shadow and
    /// 6 px inner padding.
    fn popover(mut self, theme: &Theme, tokens: &Tokens) -> Self {
        self.style().box_shadow = Some(theme.shadow());
        self.card(theme, tokens).p(px(6.0))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// `accent` fill, white text.
    Primary,
    /// `surface` with a `border`.
    Secondary,
    /// Text only, hover fill.
    Ghost,
}

/// How an approval option's button looks, wherever a request is answered (the chat card, the
/// inspector's Requests): only the choices that touch this one request (allow once, reject once)
/// are primary; "Always", cancel and the agent's own kinds are secondary. The harness's option
/// order never makes a standing rule the easy click, and nothing is activated by Return or by
/// a click beside the buttons.
pub fn approval_button_kind(kind: hutan_core::agent::PermissionOptionKind) -> ButtonKind {
    use hutan_core::agent::PermissionOptionKind as Kind;
    match kind {
        Kind::AllowOnce | Kind::RejectOnce => ButtonKind::Primary,
        Kind::AllowAlways | Kind::RejectAlways | Kind::Cancel | Kind::Other => {
            ButtonKind::Secondary
        }
    }
}

/// A 28 px text button with an 8 px radius. A disabled one is drawn dim and ignores the mouse
/// (the caller adds the click handler only when enabled).
pub fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<gpui::Div> {
    let hover = match kind {
        ButtonKind::Primary => theme.accent.opacity(0.88),
        ButtonKind::Secondary | ButtonKind::Ghost => theme.surface_hover,
    };
    let mut button = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(tokens.spacing))
        .h(px(tokens.control_height))
        .px(px(12.0))
        .rounded(px(tokens.radius))
        .text_size(px(tokens.font_size))
        .medium()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.into());
    button = match kind {
        ButtonKind::Primary => button.bg(theme.accent).text_color(theme.on_accent()),
        ButtonKind::Secondary => button
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .text_color(theme.fg),
        ButtonKind::Ghost => button.text_color(theme.fg),
    };
    if enabled {
        button.cursor_pointer().hover(move |s| s.bg(hover))
    } else {
        button.text_color(theme.faint).opacity(0.7)
    }
}

/// A round icon button (the composer's send / stop and attach): `primary` fills with `accent`,
/// otherwise a hairline-bordered `surface` circle. A disabled one is dim and inert.
pub fn round_button(
    id: impl Into<gpui::ElementId>,
    glyph: crate::icons::Icon,
    primary: bool,
    enabled: bool,
    size: f32,
    theme: &Theme,
) -> Stateful<gpui::Div> {
    let hover = if primary {
        theme.accent.opacity(0.88)
    } else {
        theme.surface_hover
    };
    let base = div()
        .id(id.into())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(pill_radius()))
        .medium()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(crate::icons::glyph(glyph));
    let base = if primary {
        base.bg(theme.accent).text_color(theme.on_accent())
    } else {
        base.border_1()
            .border_color(theme.border)
            .text_color(theme.muted)
    };
    if enabled {
        base.cursor_pointer().hover(move |s| s.bg(hover))
    } else {
        base.opacity(0.45)
    }
}

/// The widest a [`pill`] gets before its text is cut with "…".
const PILL_MAX_WIDTH: f32 = 140.0;

/// A small label in a tone's color on a faint tint of it ("exit 0", "running"). Fully rounded.
pub fn pill(text: impl Into<SharedString>, color: Hsla, tokens: &Tokens) -> gpui::Div {
    div()
        .flex_none()
        // A long branch or label is cut with "…" instead of pushing its row wider.
        .max_w(px(PILL_MAX_WIDTH))
        .truncate()
        .px(px(tokens.spacing * 2.0))
        .rounded(px(pill_radius()))
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_size(px(tokens.secondary_font_size))
        .child(text.into())
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

/// An empty state: the mascot at 96 px, one serif sentence, optionally one action button.
pub fn empty_state(
    state: Mascot,
    text: impl Into<SharedString>,
    action: Option<AnyElement>,
    theme: &Theme,
    tokens: &Tokens,
) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(tokens.spacing * 3.0))
        .p(px(tokens.spacing * 4.0))
        .child(mascot(state, mascot_size()))
        .child(
            div()
                .w_full()
                .min_w_0()
                .max_w(px(420.0))
                .text_center()
                .text_color(theme.muted)
                .prose(tokens)
                .child(text.into()),
        )
        .when_some(action, |s, action| s.child(action))
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

/// One option of a choice (appearance, density, the assistant's backend): a hairline `surface`
/// button, with an `accent` border and a soft fill while it is the selected one.
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
        .rounded(px(tokens.radius))
        .border_1()
        .text_size(px(tokens.font_size))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(selected, |s| {
            s.border_color(theme.accent)
                .bg(theme.accent_soft)
                .text_color(theme.fg)
                .medium()
        })
        .when(!selected, |s| {
            s.border_color(theme.border)
                .bg(theme.surface)
                .text_color(theme.muted)
                .hover(move |s| s.bg(hover))
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
            s.bg(theme.accent).border_color(theme.accent).justify_end()
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
