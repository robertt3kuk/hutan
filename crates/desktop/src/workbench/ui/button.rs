//! The button system (`docs/design.md` § Buttons, `docs/spikes/2026-10-22-ui-polish/buttons.md`):
//! [`Button`], a text button with an optional leading icon and shortcut hint, and [`IconButton`],
//! an icon alone that must carry a tooltip. Every button of the desktop is one of the two; the
//! grep test at the end of this file keeps ad-hoc button styling from coming back.
//!
//! Kinds: [`ButtonKind::Primary`] (the one default commit of a surface), `Secondary` (a hairline
//! outline), `Ghost` (text, hover fill), `Destructive` (`state.error` text and outline, never
//! default, never accent), `Link` (navigation, `markdown.link` text). Sizes: default (28 px,
//! `control.height`) and compact (24 px, `button.height-compact`); an icon button's hit region is
//! 28 px (`button.icon.hit`) around a 24 px hover fill and a 16 px glyph.
//!
//! States: hover and pressed fills (`button.*` tokens), the keyboard focus ring (only while focus
//! came from the keyboard and the button has a focus handle), disabled (opacity
//! `button.disabled-opacity`, default cursor, no hover, no click), loading (a spinner takes the
//! leading icon's place, the width stays, no hover, no click).

use std::f32::consts::TAU;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, ClickEvent, ElementId,
    FocusHandle, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement,
    PathBuilder, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    canvas, div, point, prelude::FluentBuilder as _, px,
};

use super::{FocusRingExt as _, StyledExt as _};
use crate::icons::Icon;
use crate::theme::{Theme, Tokens, metrics, pill_radius};
use crate::workbench::style::TextTooltip;

/// What a button is for; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// `accent` fill, `on_accent` (dark ink) text: the one default commit of a surface.
    Primary,
    /// `surface` with a `border` hairline.
    Secondary,
    /// Text only, hover fill.
    Ghost,
    /// `state.error` text and hairline, no fill: a commit that destroys something. Never the
    /// default button of a surface.
    Destructive,
    /// Navigation: `markdown.link` text, underlined on hover, no box.
    Link,
}

/// A button's height and padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    /// `control.height`: toolbars, dialogs, approvals.
    #[default]
    Default,
    /// `button.height-compact`: inline, card footers.
    Compact,
}

/// How an approval option's button looks, wherever a request is answered (the chat card, the
/// inspector's Requests): only *allow once* is primary, the one default commit of the decision;
/// rejecting, "Always", cancel and the agent's own kinds are secondary. The harness's option
/// order never makes a standing rule the easy click, and nothing is activated by Return or by
/// a click beside the buttons.
pub fn approval_button_kind(kind: hutan_core::agent::PermissionOptionKind) -> ButtonKind {
    use hutan_core::agent::PermissionOptionKind as Kind;
    match kind {
        Kind::AllowOnce => ButtonKind::Primary,
        Kind::RejectOnce | Kind::AllowAlways | Kind::RejectAlways | Kind::Cancel | Kind::Other => {
            ButtonKind::Secondary
        }
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A spinner: a 270 degree arc, `size` px square, in `color`, turning once per 0.9 s (still
/// under Reduce Motion, which GPUI applies to every animation).
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    const STROKE: f32 = 1.5;
    const SEGMENTS: usize = 24;
    let id = id.into();
    div().flex_none().size(px(size)).with_animation(
        id,
        Animation::new(Duration::from_millis(900))
            .repeat()
            .with_max_fps(30.0),
        move |element, delta| {
            element.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let center = bounds.center();
                        let radius = size / 2.0 - STROKE;
                        let start = delta * TAU;
                        let sweep = TAU * 0.75;
                        let mut builder = PathBuilder::stroke(px(STROKE));
                        for step in 0..=SEGMENTS {
                            let angle = start + sweep * step as f32 / SEGMENTS as f32;
                            let at = point(
                                center.x + px(radius * angle.cos()),
                                center.y + px(radius * angle.sin()),
                            );
                            if step == 0 {
                                builder.move_to(at);
                            } else {
                                builder.line_to(at);
                            }
                        }
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, color);
                        }
                    },
                )
                .size_full(),
            )
        },
    )
}

/// A text button; see the module docs. The click handler runs only while the button is neither
/// disabled nor loading.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    kind: ButtonKind,
    size: ButtonSize,
    icon: Option<Icon>,
    shortcut: Option<SharedString>,
    tooltip: Option<SharedString>,
    selected: bool,
    disabled: bool,
    loading: bool,
    block: bool,
    focus: Option<FocusHandle>,
    ring: bool,
    max_width: Option<f32>,
    on_click: Option<ClickHandler>,
    children: Vec<AnyElement>,
    theme: Theme,
    tokens: Tokens,
}

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Button")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("kind", &self.kind)
            .field("disabled", &self.disabled)
            .field("loading", &self.loading)
            .finish_non_exhaustive()
    }
}

impl Button {
    /// A secondary, default-size button.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &Theme,
        tokens: &Tokens,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind: ButtonKind::Secondary,
            size: ButtonSize::Default,
            icon: None,
            shortcut: None,
            tooltip: None,
            selected: false,
            disabled: false,
            loading: false,
            block: false,
            focus: None,
            ring: false,
            max_width: None,
            on_click: None,
            children: Vec::new(),
            theme: *theme,
            tokens: *tokens,
        }
    }

    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    pub fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    pub fn destructive(self) -> Self {
        self.kind(ButtonKind::Destructive)
    }

    pub fn link(self) -> Self {
        self.kind(ButtonKind::Link)
    }

    /// The compact height and padding.
    pub fn compact(mut self) -> Self {
        self.size = ButtonSize::Compact;
        self
    }

    /// A leading icon; a spinner takes its place while loading.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The key that activates the button (`1`, `⏎`), drawn after the label.
    pub fn shortcut(mut self, key: impl Into<SharedString>) -> Self {
        self.shortcut = Some(key.into());
        self
    }

    /// Hover text; optional for a text button (an icon button requires one).
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// The chosen one of a group of buttons (a density, a filter): the selected `accent_soft`
    /// fill instead of the outline. Secondary and ghost buttons only.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Dim, default cursor, no hover, no click.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A spinner in place of the leading icon; the width stays and the button is not clickable.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Fill the width of the parent (a form's action row, a card's single action).
    pub fn block(mut self) -> Self {
        self.block = true;
        self
    }

    /// Makes the button a keyboard focus stop (Tab, then Return or Space clicks it) that shows
    /// the focus ring while focus came from the keyboard.
    pub fn focus_handle(mut self, handle: &FocusHandle) -> Self {
        self.focus = Some(handle.clone());
        self
    }

    /// Draws the keyboard focus ring while `ring` holds and focus came from the keyboard: for a
    /// sheet that keeps its own keyboard position among its buttons instead of GPUI focus.
    pub fn ring(mut self, ring: bool) -> Self {
        self.ring = ring;
        self
    }

    /// The widest the button gets, in px; a longer label is cut with "…" (a workspace name).
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = Some(width);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            label,
            kind,
            size,
            icon,
            shortcut,
            tooltip,
            selected,
            disabled,
            loading,
            block,
            focus,
            ring,
            on_click,
            max_width,
            children,
            theme,
            tokens,
        } = self;
        let m = metrics();
        let (height, pad) = match size {
            ButtonSize::Default => (tokens.control_height, m.button_padding_x),
            ButtonSize::Compact => (tokens.button_height_compact, m.button_padding_x_compact),
        };
        let inert = disabled || loading;
        let focused = ring || focus.as_ref().is_some_and(|h| h.is_focused(window));
        let (text, hover, pressed) = match kind {
            ButtonKind::Primary => (
                theme.on_accent(),
                theme.button_primary_hover,
                theme.button_primary_pressed,
            ),
            ButtonKind::Secondary | ButtonKind::Ghost => {
                (theme.fg, theme.button_hover, theme.button_pressed)
            }
            ButtonKind::Destructive => (
                theme.danger,
                theme.button_danger_hover,
                theme.button_danger_pressed,
            ),
            ButtonKind::Link => (theme.link, theme.link, theme.link),
        };
        // A loading button without an icon has no slot for the spinner: it sits in the left
        // padding, so the width never changes.
        let spinner_size = if icon.is_some() {
            14.0
        } else {
            (pad - 2.0).clamp(6.0, 10.0)
        };
        let spinner_id = (id.clone(), "spinner");

        let mut button = div()
            .id(id)
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing))
            .text_size(px(tokens.font_size))
            .medium()
            .text_color(text)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        button = match kind {
            ButtonKind::Link => {
                button.when(!inert, |s| s.cursor_pointer().hover(|s| s.underline()))
            }
            _ => button
                .h(px(height))
                .px(px(pad))
                .rounded(px(tokens.radius))
                .when(kind == ButtonKind::Primary, |s| s.bg(theme.accent))
                .when(kind == ButtonKind::Secondary, |s| {
                    s.outlined(&theme, &tokens)
                })
                .when(selected && kind != ButtonKind::Primary, |s| {
                    s.border_color(gpui::transparent_black())
                        .bg(theme.accent_soft)
                })
                .when(kind == ButtonKind::Destructive, |s| {
                    s.border_1().border_color(theme.danger)
                })
                .when(!inert, |s| {
                    s.cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .active(move |s| s.bg(pressed))
                }),
        };
        if block {
            button = button.w_full();
        }
        if let Some(width) = max_width {
            button = button.max_w(px(width)).min_w_0();
        }
        if inert {
            button = button.cursor_default();
        }
        if disabled {
            button = button.opacity(m.button_disabled_opacity);
        }
        if let Some(handle) = &focus {
            button = button.track_focus(handle);
        }
        if let Some(text) = tooltip {
            button = button.tooltip(move |_, cx| cx.new(|_| TextTooltip(text.clone())).into());
        }
        if !inert && let Some(handler) = on_click {
            button = button.on_click(handler);
        }
        let leading_icon = icon.is_some();
        button
            .when(loading && leading_icon, |s| {
                s.child(spinner(spinner_id.clone(), 14.0, text))
            })
            .when(loading && !leading_icon, |s| {
                s.child(
                    div()
                        .absolute()
                        .left(px((pad - spinner_size) / 2.0))
                        .child(spinner(spinner_id.clone(), spinner_size, text)),
                )
            })
            .when(!loading, |s| {
                s.when_some(icon, |s, icon| s.child(crate::icons::glyph(icon)))
            })
            .child(
                div()
                    .when(max_width.is_some(), |s| s.min_w_0().truncate())
                    .when(loading && !leading_icon, |s| s.opacity(0.6))
                    .child(label),
            )
            .when_some(shortcut, |s, key| {
                s.child(
                    div()
                        .flex_none()
                        .text_size(px(tokens.section_font_size))
                        .font_weight(gpui::FontWeight::NORMAL)
                        .text_color(match kind {
                            ButtonKind::Primary => theme.on_accent(),
                            _ => theme.muted,
                        })
                        .child(key),
                )
            })
            .children(children)
            .focus_ring(focused && kind != ButtonKind::Link, &theme, &tokens)
    }
}

/// An icon alone. The tooltip names the action and is required. The hit region is
/// `button.icon.hit` (28 px) around a hover fill 4 px smaller and a `button.icon.size` (16 px)
/// glyph; [`IconButton::round`] fills the whole region as a circle (the composer's send).
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: Icon,
    tooltip: SharedString,
    kind: ButtonKind,
    size: ButtonSize,
    side: Option<f32>,
    round: bool,
    toggled: bool,
    disabled: bool,
    loading: bool,
    focus: Option<FocusHandle>,
    on_click: Option<ClickHandler>,
    children: Vec<AnyElement>,
    theme: Theme,
    tokens: Tokens,
}

impl std::fmt::Debug for IconButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IconButton")
            .field("id", &self.id)
            .field("tooltip", &self.tooltip)
            .field("kind", &self.kind)
            .field("disabled", &self.disabled)
            .field("loading", &self.loading)
            .finish_non_exhaustive()
    }
}

impl IconButton {
    /// A ghost icon button.
    pub fn new(
        id: impl Into<ElementId>,
        icon: Icon,
        tooltip: impl Into<SharedString>,
        theme: &Theme,
        tokens: &Tokens,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            tooltip: tooltip.into(),
            kind: ButtonKind::Ghost,
            size: ButtonSize::Default,
            side: None,
            round: false,
            toggled: false,
            disabled: false,
            loading: false,
            focus: None,
            on_click: None,
            children: Vec::new(),
            theme: *theme,
            tokens: *tokens,
        }
    }

    pub fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn primary(self) -> Self {
        self.kind(ButtonKind::Primary)
    }

    /// The compact hit region (`button.height-compact`).
    pub fn compact(mut self) -> Self {
        self.size = ButtonSize::Compact;
        self
    }

    /// A circle that fills the whole hit region (`radius.pill`).
    pub fn round(mut self) -> Self {
        self.round = true;
        self
    }

    /// An explicit side for the hit region, in px (a strip button, the composer's send).
    pub fn side(mut self, side: f32) -> Self {
        self.side = Some(side);
        self
    }

    /// The button's state is on (a dock shown, a filter active): the selected `accent_soft` fill.
    pub fn toggled(mut self, toggled: bool) -> Self {
        self.toggled = toggled;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A spinner in place of the icon; not clickable.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn focus_handle(mut self, handle: &FocusHandle) -> Self {
        self.focus = Some(handle.clone());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl ParentElement for IconButton {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            icon,
            tooltip,
            kind,
            size,
            side,
            round,
            toggled,
            disabled,
            loading,
            focus,
            on_click,
            children,
            theme,
            tokens,
        } = self;
        let m = metrics();
        let hit = side.unwrap_or(match size {
            ButtonSize::Default => m.button_icon_hit,
            ButtonSize::Compact => tokens.button_height_compact,
        });
        let glyph = match size {
            ButtonSize::Default => m.button_icon_size,
            ButtonSize::Compact => (m.button_icon_size - 2.0).max(8.0),
        };
        // The hover fill sits 2 px inside the hit region on every side; a round button is all fill.
        let inset = if round { 0.0 } else { 2.0 };
        let inert = disabled || loading;
        let focused = focus.as_ref().is_some_and(|h| h.is_focused(window));
        let (rest, hover_text, hover, pressed) = match kind {
            ButtonKind::Primary => (
                theme.on_accent(),
                theme.on_accent(),
                theme.button_primary_hover,
                theme.button_primary_pressed,
            ),
            ButtonKind::Destructive => (
                theme.danger,
                theme.danger,
                theme.button_danger_hover,
                theme.button_danger_pressed,
            ),
            ButtonKind::Secondary | ButtonKind::Ghost | ButtonKind::Link => (
                theme.muted,
                theme.fg,
                theme.button_hover,
                theme.button_pressed,
            ),
        };
        let rest = if toggled { theme.fg } else { rest };
        let group: SharedString = format!("icon-button-{id}").into();
        let spinner_id = (id.clone(), "spinner");
        let radius = if round { pill_radius() } else { tokens.radius };

        let mut face = div()
            .id((id.clone(), "face"))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(hit - 2.0 * inset))
            .rounded(px(radius))
            .text_color(rest)
            .when(kind == ButtonKind::Primary, |s| s.bg(theme.accent))
            .when(kind == ButtonKind::Secondary && !toggled, |s| {
                s.outlined(&theme, &tokens).rounded(px(radius))
            })
            .when(toggled && kind != ButtonKind::Primary, |s| {
                s.bg(theme.accent_soft)
            })
            .when(!inert, |s| {
                s.group_hover(group.clone(), move |s| s.bg(hover).text_color(hover_text))
                    .group_active(group.clone(), move |s| s.bg(pressed))
            });
        if kind == ButtonKind::Destructive {
            face = face.border_1().border_color(theme.danger);
        }
        let face = face
            .when(loading, |s| s.child(spinner(spinner_id, glyph, rest)))
            .when(!loading, |s| s.child(crate::icons::sized(icon, glyph)))
            .children(children);

        let mut button = div()
            .id(id)
            .group(group)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(hit))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(tooltip.clone())).into());
        if disabled {
            button = button.opacity(m.button_disabled_opacity);
        }
        button = if inert {
            button.cursor_default()
        } else {
            button.cursor_pointer()
        };
        if let Some(handle) = &focus {
            button = button.track_focus(handle);
        }
        if !inert && let Some(handler) = on_click {
            button = button.on_click(handler);
        }
        button.child(face).focus_ring(focused, &theme, &tokens)
    }
}
