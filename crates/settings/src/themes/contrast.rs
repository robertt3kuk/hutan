//! Contrast warnings (WCAG 2.2 relative luminance, spike §3.8): warning-only, never repaired.
//!
//! Every pair composes translucent colors over the background it is drawn on first. Pairs the
//! application does not control (arbitrary ANSI output on arbitrary backgrounds) are not
//! checked; the palette's own colors against the terminal background are, as a sample.

use super::color::Rgb;
use super::resolve::ResolvedTheme;
use super::tokens::Color;
use crate::themes::SystemAppearance;

/// One pair below its threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastIssue {
    pub foreground: &'static str,
    pub background: &'static str,
    pub ratio: f32,
    pub threshold: f32,
}

impl ContrastIssue {
    pub fn message(&self) -> String {
        format!(
            "{} on {}: contrast {:.2}:1, below {:.1}:1",
            self.foreground, self.background, self.ratio, self.threshold
        )
    }
}

fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// `(lighter + .05) / (darker + .05)`.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Text on a surface needs 4.5, large text and icons 3.
const TEXT: f32 = 4.5;
const GRAPHIC: f32 = 3.0;

/// Every checked pair of `theme` that falls short, in a stable order.
pub fn contrast_issues(theme: &ResolvedTheme) -> Vec<ContrastIssue> {
    let mut out = Vec::new();
    // `bg` is composed over the window background, `fg` over that.
    let mut check = |fg: Color, bg: Color, threshold: f32| {
        let background = theme.color(bg).over(theme.color(Color::Background).rgb());
        let foreground = theme.color(fg).over(background);
        let ratio = contrast_ratio(foreground, background);
        if ratio < threshold {
            out.push(ContrastIssue {
                foreground: fg.id(),
                background: bg.id(),
                ratio,
                threshold,
            });
        }
    };

    let grounds = [
        Color::Background,
        Color::SidebarBackground,
        Color::SidebarRightBackground,
        Color::Surface,
        Color::SurfaceHover,
    ];
    for bg in grounds {
        check(Color::Foreground, bg, TEXT);
        check(Color::TextMuted, bg, TEXT);
        check(Color::TextFaint, bg, GRAPHIC);
    }
    for bg in [
        Color::SidebarSelected,
        Color::SidebarRightSelected,
        Color::AccentSoft,
    ] {
        check(Color::Foreground, bg, TEXT);
        check(Color::TextMuted, bg, TEXT);
    }
    check(Color::OnAccent, Color::Accent, TEXT);
    for state in [
        Color::StateSuccess,
        Color::StateWarning,
        Color::StateError,
        Color::Accent,
    ] {
        check(state, Color::Background, GRAPHIC);
        check(state, Color::Surface, GRAPHIC);
    }
    for (fg, bg) in [
        (Color::Foreground, Color::DiffAddedBackground),
        (Color::Foreground, Color::DiffRemovedBackground),
        (Color::DiffHunkForeground, Color::DiffHunkBackground),
    ] {
        check(fg, bg, TEXT);
    }

    // Terminal: the default text, the selection and the sampled palette.
    check(Color::TerminalForeground, Color::TerminalBackground, TEXT);
    check(
        Color::TerminalForeground,
        Color::TerminalSelectionBackground,
        TEXT,
    );
    check(Color::TerminalCursor, Color::TerminalBackground, GRAPHIC);
    let cursor_glyph = if theme
        .color_if_set(Color::TerminalCursorForeground)
        .is_some()
    {
        Color::TerminalCursorForeground
    } else {
        Color::TerminalBackground
    };
    check(cursor_glyph, Color::TerminalCursor, TEXT);
    // The neutral that is paper on this polarity (black on dark, white on light) is not text.
    let paper = match theme.appearance {
        SystemAppearance::Dark => Color::AnsiBlack,
        SystemAppearance::Light => Color::AnsiWhite,
    };
    for (i, ansi) in Color::ANSI.into_iter().enumerate() {
        if ansi == paper {
            continue;
        }
        let threshold = if i < 8 { TEXT } else { GRAPHIC };
        check(ansi, Color::TerminalBackground, threshold);
    }

    // Editor: the default text and every `syntax.*` color on the editor background.
    check(Color::EditorForeground, Color::EditorBackground, TEXT);
    for token in Color::SYNTAX {
        check(token, Color::EditorBackground, TEXT);
    }
    out
}

/// What a pack adds on top of the contrast problems its base already has: an issue counts when
/// the base does not have the pair or the pair got worse.
pub fn new_issues(theme: &ResolvedTheme, base: Option<&ResolvedTheme>) -> Vec<ContrastIssue> {
    let inherited = base.map(contrast_issues).unwrap_or_default();
    contrast_issues(theme)
        .into_iter()
        .filter(|issue| {
            !inherited.iter().any(|old| {
                old.foreground == issue.foreground
                    && old.background == issue.background
                    && issue.ratio >= old.ratio - 0.05
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ratio_matches_the_wcag_definition() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-6);
        // #767676 on white is the classic 4.54:1.
        let gray = Rgb::new(0x76, 0x76, 0x76);
        assert!((contrast_ratio(gray, white) - 4.54).abs() < 0.01);
    }
}
