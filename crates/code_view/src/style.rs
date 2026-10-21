//! How code is drawn: the editor colors of the color pack (E-4) and the font.

use gpui::{Font, FontFeatures, FontStyle, FontWeight, Hsla, SharedString};
use hutan_settings::themes::EditorTheme;
use hutan_settings::{Rgb, Rgba};
use hutan_syntax::Token;

/// Colors and font of a code view. Changing it restyles what is on screen; the text and its
/// syntax tree are untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeStyle {
    pub theme: EditorTheme,
    /// Monospace family.
    pub font_family: SharedString,
    /// Points.
    pub font_size: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
}

impl CodeStyle {
    pub fn new(
        theme: EditorTheme,
        font_family: impl Into<SharedString>,
        font_size: f32,
        line_height: f32,
    ) -> Self {
        Self {
            theme,
            font_family: font_family.into(),
            font_size: font_size.clamp(4.0, 72.0),
            line_height: line_height.clamp(0.8, 3.0),
        }
    }

    /// The font of `token` (bold and italic markup), ligatures off so every cell is one glyph.
    pub fn font(&self, token: Option<Token>) -> Font {
        Font {
            family: self.font_family.clone(),
            features: FontFeatures(std::sync::Arc::new(vec![
                ("calt".to_owned(), 0),
                ("liga".to_owned(), 0),
            ])),
            fallbacks: None,
            weight: if token == Some(Token::MarkupBold) || token == Some(Token::MarkupHeading) {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if token == Some(Token::MarkupItalic) {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        }
    }

    /// The text color of `token` (the editor foreground for text without one).
    pub fn token_color(&self, token: Option<Token>) -> Hsla {
        match token {
            Some(token) => rgb(self.theme.syntax[token as usize]),
            None => rgba(self.theme.foreground),
        }
    }
}

/// A pack color as GPUI's.
pub fn rgba(color: Rgba) -> Hsla {
    gpui::Rgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: color.a,
    }
    .into()
}

/// An opaque pack color as GPUI's.
pub fn rgb(color: Rgb) -> Hsla {
    rgba(Rgba::opaque(color))
}
