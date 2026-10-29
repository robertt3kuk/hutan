//! The spacing scale and the shared pieces built on it (UI-RHYTHM, `docs/design.md` § Spacing
//! scale): sheets and dialogs, their action rows. Every gap, padding and margin of a sheet comes
//! from [`space`]; a sheet never invents a ninth number.

use gpui::{
    AnyElement, Div, ElementId, InteractiveElement, IntoElement, ParentElement, SharedString,
    Stateful, Styled, div, prelude::FluentBuilder as _, px,
};

use super::StyledExt as _;
use crate::theme::{Theme, Tokens};

/// The spacing scale in points: 4 · 8 · 12 · 16 · 24 · 32, nothing else.
pub mod space {
    /// Between an icon and its label, inside a chip.
    pub const XS: f32 = 4.0;
    /// Between lines of one block, between buttons of a stack.
    pub const SM: f32 = 8.0;
    /// Between buttons of a row, between a label and its control.
    pub const MD: f32 = 12.0;
    /// A card's inner padding, between blocks of a dialog, a window margin.
    pub const LG: f32 = 16.0;
    /// A dialog's padding, the space above a section label.
    pub const XL: f32 = 24.0;
    /// A page's side padding.
    pub const XXL: f32 = 32.0;

    /// The whole scale, for tests that check nothing else is used.
    pub const ALL: [f32; 6] = [XS, SM, MD, LG, XL, XXL];
}

/// The width of a dialog card in points.
pub const SHEET_WIDTH: f32 = 480.0;

/// The space a dialog keeps from every edge of the window.
pub const SHEET_MARGIN: f32 = space::LG;

/// The width a dialog of `wanted` points gets in a window `viewport` points wide: `wanted`,
/// but never wider than the window less its margins.
pub fn sheet_width(viewport: f32, wanted: f32) -> f32 {
    wanted.min((viewport - 2.0 * SHEET_MARGIN).max(0.0))
}

/// Whether the buttons of a dialog stack one under the other (the primary on top, as macOS
/// sheets do) instead of standing in one right-aligned row: the card is narrower than
/// [`SHEET_WIDTH`].
pub fn sheet_stacks(viewport: f32) -> bool {
    sheet_width(viewport, SHEET_WIDTH) < SHEET_WIDTH
}

/// A dialog card: a popover surface `width` points wide, padded [`space::XL`], its blocks
/// [`space::LG`] apart. The caller adds the blocks and, last, the [`dialog_footer`].
pub fn sheet(width: f32, theme: &Theme, tokens: &Tokens) -> Div {
    div()
        .w(px(width))
        .flex()
        .flex_col()
        .gap(px(space::LG))
        .popover(theme, tokens)
        .p(px(space::XL))
}

/// The action row at the foot of a dialog or sheet: right-aligned, [`space::MD`] between buttons.
/// Cancel goes first and the one primary (or the destructive confirm) last. A row that does not
/// fit wraps (right-aligned, [`space::SM`] between lines) instead of overflowing its card.
pub fn dialog_footer() -> Div {
    div()
        .flex()
        .flex_wrap()
        .justify_end()
        .items_center()
        .gap_x(px(space::MD))
        .gap_y(px(space::SM))
}

/// The action column of a dialog in a narrow window: every button as wide as the card,
/// [`space::SM`] apart. The caller adds them primary first.
pub fn dialog_footer_stacked() -> Div {
    div().flex().flex_col().gap(px(space::SM))
}

/// The height of a settings row: a label (13) over one line of help (12).
pub const ROW_HEIGHT: f32 = 52.0;

/// The width of the control column of a settings row: dropdowns and boxes are this wide, so the
/// controls of a page share one left edge and one right edge.
pub const CONTROL_WIDTH: f32 = 168.0;

/// The element id of a section label, which [`group_sections`] finds again.
const SECTION_HEAD: &str = "settings-section-head";

/// A fixed-height settings row: `label` fills the left, `controls` stand in the right column
/// ([`CONTROL_WIDTH`] at least, right-aligned, [`space::SM`] apart), a hairline under it. A row
/// whose controls are taller than a line grows with `.h_auto().min_h(px(ROW_HEIGHT))`.
pub fn row_fixed(
    id: impl Into<ElementId>,
    label: impl IntoElement,
    controls: Vec<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(space::LG))
        .h(px(ROW_HEIGHT))
        .border_b_1()
        .border_color(theme.border)
        .child(div().flex().flex_col().flex_1().min_w_0().child(label))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_end()
                .min_w(px(CONTROL_WIDTH))
                .gap(px(space::SM))
                .children(controls),
        )
}

/// The label over a card: `title` (12 semibold, muted) with an optional line of `help`,
/// [`space::XL`] above and [`space::SM`] below.
pub fn section_head(
    title: impl Into<SharedString>,
    help: impl Into<SharedString>,
    theme: &Theme,
    tokens: &Tokens,
) -> AnyElement {
    let help = help.into();
    div()
        .id(SECTION_HEAD)
        .flex()
        .flex_col()
        .gap(px(space::XS))
        .pt(px(space::XL))
        .pb(px(space::SM))
        .child(
            div()
                .text_size(px(tokens.secondary_font_size + 1.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.muted)
                .child(title.into()),
        )
        .when(!help.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(help),
            )
        })
        .into_any_element()
}

/// The head of a section of a page that has several (a page that used to be a page of its own):
/// `title` (15 semibold, in the text color) over a line of `help` (12, muted), [`space::XL`]
/// above and [`space::SM`] below, a hairline under it. It is a bigger label than
/// [`section_head`]: the cards below it keep their own.
pub fn page_section_head(
    title: impl Into<SharedString>,
    help: impl Into<SharedString>,
    theme: &Theme,
    tokens: &Tokens,
) -> Stateful<Div> {
    let help = help.into();
    div()
        .id("page-section-head")
        .flex()
        .flex_col()
        .gap(px(space::XS))
        .pt(px(space::XL))
        .pb(px(space::SM))
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(tokens.font_size + 2.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme.fg)
                .child(title.into()),
        )
        .when(!help.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(tokens.secondary_font_size))
                    .text_color(theme.muted)
                    .child(help),
            )
        })
}

/// Whether `element` was made by [`section_head`].
fn is_section_head(element: &mut AnyElement) -> bool {
    element
        .downcast_mut::<Stateful<Div>>()
        .is_some_and(|div| div.interactivity().element_id == Some(ElementId::from(SECTION_HEAD)))
}

/// A card of rows: the page's `bg` on the sheet's surface, rounded like every card, no border.
/// Every child is inset [`space::LG`] from both edges, so its hairline is too; the last row's
/// hairline is cut off by the card's edge.
pub fn card_section(children: Vec<AnyElement>, theme: &Theme, tokens: &Tokens) -> AnyElement {
    div()
        .overflow_hidden()
        .rounded(px(tokens.card_radius))
        .bg(theme.bg)
        .child(
            div()
                .flex()
                .flex_col()
                // The last row's 1 px hairline falls outside the card and is clipped.
                .mb(px(-1.0))
                .children(
                    children
                        .into_iter()
                        .map(|child| div().flex().flex_col().px(px(space::LG)).child(child)),
                ),
        )
        .into_any_element()
}

/// The rows of a page in cards: a [`section_head`] ends the card before it and the elements
/// after it, up to the next head, are the next card; the elements before the first head are a
/// card without a label. A head with nothing after it stands alone.
pub fn group_sections(rows: Vec<AnyElement>, theme: &Theme, tokens: &Tokens) -> Vec<AnyElement> {
    let mut out = Vec::new();
    let mut card = Vec::new();
    let flush = |out: &mut Vec<AnyElement>, card: &mut Vec<AnyElement>| {
        if !card.is_empty() {
            out.push(card_section(std::mem::take(card), theme, tokens));
        }
    };
    for mut row in rows {
        if is_section_head(&mut row) {
            flush(&mut out, &mut card);
            out.push(row);
        } else {
            card.push(row);
        }
    }
    flush(&mut out, &mut card);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_is_480_wide_with_a_margin_and_stacks_when_it_cannot_be() {
        assert_eq!(sheet_width(900.0, SHEET_WIDTH), 480.0);
        assert_eq!(sheet_width(512.0, SHEET_WIDTH), 480.0);
        assert_eq!(sheet_width(400.0, SHEET_WIDTH), 368.0);
        assert_eq!(sheet_width(20.0, SHEET_WIDTH), 0.0);
        assert!(!sheet_stacks(600.0) && !sheet_stacks(512.0));
        assert!(sheet_stacks(511.0) && sheet_stacks(400.0));
    }

    #[test]
    fn the_scale_is_four_eight_twelve_sixteen_twenty_four_thirty_two() {
        assert_eq!(space::ALL, [4.0, 8.0, 12.0, 16.0, 24.0, 32.0]);
    }

    #[test]
    fn a_section_head_ends_the_card_before_it_and_the_rows_after_it_are_the_next_card() {
        use crate::theme::{Density, DensityExt as _};
        let (theme, tokens) = (Theme::dark(), Density::Compact.ui());
        let row = || div().into_any_element();
        let head = || section_head("Theme", "", &theme, &tokens);
        // rows, head, rows, head, head, rows → card, head, card, head, head, card.
        let rows = vec![row(), row(), head(), row(), head(), head(), row(), row()];
        let mut out = group_sections(rows, &theme, &tokens);
        let kinds: Vec<&str> = out
            .iter_mut()
            .map(|element| {
                if is_section_head(element) {
                    "head"
                } else if element.downcast_mut::<Div>().is_some() {
                    "card"
                } else {
                    "other"
                }
            })
            .collect();
        assert_eq!(kinds, ["card", "head", "card", "head", "head", "card"]);
        assert!(group_sections(Vec::new(), &theme, &tokens).is_empty());
    }
}
