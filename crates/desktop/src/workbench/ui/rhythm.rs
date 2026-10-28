//! The spacing scale and the shared pieces built on it (UI-RHYTHM, `docs/design.md` § Spacing
//! scale): sheets and dialogs, their action rows. Every gap, padding and margin of a sheet comes
//! from [`space`]; a sheet never invents a ninth number.

use gpui::{Div, Styled, div, px};

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
}
