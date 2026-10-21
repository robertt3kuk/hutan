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

/// The ratio of `fg` drawn on `bg`, both composed the way the application draws them: `bg` over
/// the window background, `fg` over that.
fn ratio(theme: &ResolvedTheme, fg: Color, bg: Color) -> f32 {
    let background = theme.color(bg).over(theme.color(Color::Background).rgb());
    let foreground = theme.color(fg).over(background);
    contrast_ratio(foreground, background)
}

/// The workbench pairs of the design direction (`docs/spikes/2026-10-21-clean-aesthetic.md`
/// §3.2.3), as `(foreground, background, threshold)`: the text of the chrome, the dark ink on the
/// accent, the state words, links, and the accent and state glyphs as components. A built-in pack
/// that stops passing one of them fails `the_builtin_packs_pass_the_workbench_pairs`.
///
/// `ui.text.faint` is not here: it is disabled text only (WCAG exempts it), and placeholders use
/// `ui.text.muted`. A state word on a selected row is `ui.foreground` (the glyph keeps the
/// color), so only the glyph is held to 3:1 there.
fn workbench_pairs() -> Vec<(Color, Color, f32)> {
    let mut pairs = Vec::new();
    for bg in [
        Color::Background,
        Color::SidebarBackground,
        Color::SidebarRightBackground,
        Color::Surface,
        Color::SurfaceHover,
        Color::SidebarSelected,
        Color::SidebarRightSelected,
        Color::AccentSoft,
    ] {
        pairs.push((Color::Foreground, bg, TEXT));
        pairs.push((Color::TextMuted, bg, TEXT));
    }
    pairs.push((Color::OnAccent, Color::Accent, TEXT));
    // State words are text on the grounds a row or a card is drawn on.
    for state in [Color::StateSuccess, Color::StateWarning, Color::StateError] {
        for bg in [
            Color::Background,
            Color::Surface,
            Color::SidebarBackground,
            Color::SurfaceHover,
        ] {
            pairs.push((state, bg, TEXT));
        }
        pairs.push((state, Color::SidebarSelected, GRAPHIC));
    }
    // Links are the only accent text.
    for bg in [Color::Background, Color::Surface, Color::SidebarBackground] {
        pairs.push((Color::MarkdownLink, bg, TEXT));
    }
    // The accent as a fill (primary button, send button, working glyph) is a component.
    for bg in [Color::Background, Color::Surface] {
        pairs.push((Color::Accent, bg, GRAPHIC));
    }
    pairs
}

/// Every checked pair of `theme` that falls short, in a stable order.
pub fn contrast_issues(theme: &ResolvedTheme) -> Vec<ContrastIssue> {
    let mut out = Vec::new();
    let mut check = |fg: Color, bg: Color, threshold: f32| {
        let ratio = ratio(theme, fg, bg);
        if ratio < threshold {
            out.push(ContrastIssue {
                foreground: fg.id(),
                background: bg.id(),
                ratio,
                threshold,
            });
        }
    };

    for (fg, bg, threshold) in workbench_pairs() {
        check(fg, bg, threshold);
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
    use crate::themes::{Builtin, builtin_theme};

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

    /// Spike §3.2.3: every pair, with its ratio, in both built-ins. Run with `--nocapture` to read
    /// the table; a built-in that stops passing a pair fails here, and the message names it.
    #[test]
    fn the_builtin_packs_pass_the_workbench_pairs() {
        let mut failures = Vec::new();
        for builtin in Builtin::ALL {
            let theme = builtin_theme(builtin);
            println!("{}:", builtin.id());
            for (fg, bg, threshold) in workbench_pairs() {
                let value = ratio(&theme, fg, bg);
                let verdict = if value >= threshold { "pass" } else { "FAIL" };
                println!(
                    "  {:<24} on {:<28} {value:5.2}:1 (>= {threshold:.1}) {verdict}",
                    fg.id(),
                    bg.id()
                );
                if value < threshold {
                    failures.push(format!(
                        "{}: {} on {} is {value:.2}:1, below {threshold:.1}:1",
                        builtin.id(),
                        fg.id(),
                        bg.id()
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_dark_ink_on_the_accent_and_the_state_words_have_the_documented_margins() {
        let dark = builtin_theme(Builtin::HutanDark);
        let light = builtin_theme(Builtin::HutanLight);
        // The spike's computed values (§3.2.3), to two places.
        let at = |theme: &ResolvedTheme, fg, bg| (ratio(theme, fg, bg) * 100.0).round() / 100.0;
        assert_eq!(at(&light, Color::OnAccent, Color::Accent), 4.94);
        assert_eq!(at(&dark, Color::OnAccent, Color::Accent), 5.84);
        assert_eq!(at(&light, Color::StateWarning, Color::Background), 5.16);
        assert_eq!(at(&light, Color::MarkdownLink, Color::Background), 5.26);
        assert_eq!(at(&dark, Color::StateError, Color::SurfaceHover), 4.80);
    }
}
