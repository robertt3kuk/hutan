//! Density tokens and the chrome palette (design.md § Visual system, v2).

use gpui::{BoxShadow, Hsla, Rgba, WindowAppearance, hsla, point, px, rgb};
use hutan_settings::{AppearanceMode, SystemAppearance};

pub use hutan_settings::Density;

/// Sizes in logical pixels: the settings crate's density token table plus the type scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub sidebar_row: f32,
    pub tab_height: f32,
    pub pane_header: f32,
    pub status_bar: f32,
    pub spacing: f32,
    /// Corner radius of controls (buttons, rows, inputs): 6 px compact, 8 px comfortable.
    pub radius: f32,
    /// Corner radius of cards, the composer, popovers and the palette: 10 px / 12 px.
    pub card_radius: f32,
    /// UI body text: 12 pt compact, 13 pt comfortable.
    pub font_size: f32,
    /// Secondary UI text (subtitles, chips, hints): 11 pt compact, 12 pt comfortable.
    pub secondary_font_size: f32,
    /// Section labels: 11 pt medium, sentence case.
    pub section_font_size: f32,
    /// Prose in the chat view and empty-state copy.
    pub prose_font_size: f32,
    /// Prose line height as a multiple of its size.
    pub prose_line_height: f32,
    /// Height of buttons and single-line fields.
    pub control_height: f32,
    /// Space between a terminal view's edge and its text on every side: 8 px compact, 12 px
    /// comfortable.
    pub terminal_padding: f32,
    /// Width of the block gutter bar at a terminal's left edge, inside the terminal padding:
    /// 3 px compact, 4 px comfortable.
    pub block_gutter: f32,
}

/// Height of the status bar, in both densities (Orca's 24 px).
pub const STATUS_BAR_HEIGHT: f32 = 24.0;
/// Height of the title bar and of the right sidebar's icon strip (Orca's 36 px).
pub const TITLEBAR_HEIGHT: f32 = 36.0;
/// Corner radius of pills, badges and round buttons.
pub const PILL_RADIUS: f32 = 999.0;
/// Width of the chat's reading column.
pub const READING_WIDTH: f32 = 720.0;
/// Size of the mascot in empty states.
pub const MASCOT_SIZE: f32 = 96.0;

/// Desktop-side additions to [`Density`] (which lives in `hutan-settings`).
pub trait DensityExt: Sized {
    /// Layout tokens and font sizes for this density.
    fn ui(self) -> Tokens;
    fn label(self) -> &'static str;
    fn toggled(self) -> Self;
}

impl DensityExt for Density {
    fn ui(self) -> Tokens {
        let t = self.tokens();
        let compact = self == Density::Compact;
        Tokens {
            sidebar_row: t.sidebar_row,
            tab_height: t.tab_height,
            pane_header: t.pane_header,
            status_bar: STATUS_BAR_HEIGHT,
            spacing: t.spacing,
            radius: t.radius,
            card_radius: if compact { 10.0 } else { 12.0 },
            font_size: if compact { 12.0 } else { 13.0 },
            secondary_font_size: if compact { 11.0 } else { 12.0 },
            section_font_size: 11.0,
            prose_font_size: 15.0,
            prose_line_height: 1.6,
            control_height: 28.0,
            terminal_padding: if compact { 8.0 } else { 12.0 },
            block_gutter: if compact { 3.0 } else { 4.0 },
        }
    }

    fn label(self) -> &'static str {
        match self {
            Density::Compact => "Compact",
            Density::Comfortable => "Comfortable",
        }
    }

    fn toggled(self) -> Self {
        match self {
            Density::Compact => Density::Comfortable,
            Density::Comfortable => Density::Compact,
        }
    }
}

/// Chrome colors: exactly the tokens of design.md § Visual system (v2); light and dark. The
/// terminal keeps its own palette (`hutan-light` / `hutan-dark` in `hutan-settings`), whose
/// background equals `bg`. These are the only color values in the desktop: every view reads them
/// from here.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Window and center panes.
    pub bg: Hsla,
    /// Left dock and the title bar strip.
    pub sidebar: Hsla,
    /// Selected nav row and active project card of the left sidebar (Orca's
    /// `worktree-sidebar-accent`): a step away from `sidebar`, lighter in dark, darker in light.
    pub sidebar_accent: Hsla,
    /// The right sidebar: its own surface, apart from the left one (Orca's `sidebar`).
    pub right_sidebar: Hsla,
    /// Hover and selected rows of the right sidebar (Orca's `sidebar-accent`).
    pub right_sidebar_accent: Hsla,
    /// Cards, composer, popovers, palette, modals.
    pub surface: Hsla,
    /// Row and button hover.
    pub surface_hover: Hsla,
    /// Hairlines (1 px): white at 7 % in dark (Orca's `--border`), an opaque warm gray in light.
    pub border: Hsla,
    /// Primary text.
    pub fg: Hsla,
    /// Secondary text and icons.
    pub muted: Hsla,
    /// Placeholders and disabled text.
    pub faint: Hsla,
    /// Primary buttons, selection, focus, links: the brand orange.
    pub accent: Hsla,
    /// Selected row, chips, the user message bubble.
    pub accent_soft: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
}

/// The palette table as `0xRRGGBB` values, in token order: bg, sidebar, surface, surface_hover,
/// border, fg, muted, faint, accent, accent_soft, success, warning, danger.
pub const LIGHT_PALETTE: [u32; 13] = [
    0xFAF9F5, 0xF3F1EA, 0xFFFFFF, 0xF0EEE6, 0xE6E3D9, 0x1F1E1D, 0x6F6D66, 0xA8A59C, 0xE0692E,
    0xFBE9DF, 0x3F8F5A, 0xC98A12, 0xC8452F,
];
pub const DARK_PALETTE: [u32; 13] = [
    0x262624, 0x1F1E1D, 0x30302E, 0x3A3936, 0x3D3C39, 0xF5F4EF, 0xA6A49C, 0x6F6D66, 0xEA7A43,
    0x3E2C23, 0x6BBF85, 0xE2AE45, 0xE87562,
];

/// The surfaces Orca separates and Hutan did not: `sidebar_accent`, `right_sidebar`,
/// `right_sidebar_accent`, as `0xRRGGBB`. Same structure as Orca's tokens (dark: left rows step
/// up from the sidebar, the right panel is darker than the left; light: left rows step down, the
/// right panel is lighter), in Hutan's warm hues.
pub const LIGHT_SIDEBARS: [u32; 3] = [0xE9E6DC, 0xF8F7F2, 0xF0EEE6];
pub const DARK_SIDEBARS: [u32; 3] = [0x2E2D2B, 0x1A1918, 0x2A2927];

/// Dark hairlines: white at 7 % (Orca's `--border` in dark).
const DARK_BORDER_ALPHA: f32 = 0.07;

impl Theme {
    /// The theme for the `appearance` setting; `system` follows the window appearance, so a macOS
    /// light/dark switch changes it the next time the workbench re-resolves.
    pub fn for_chrome(chrome: AppearanceMode, appearance: WindowAppearance) -> Self {
        match chrome {
            AppearanceMode::System => Self::for_appearance(appearance),
            AppearanceMode::Light => Self::light(),
            AppearanceMode::Dark => Self::dark(),
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::light(),
        }
    }

    pub fn dark() -> Self {
        Self::from_palette(true, DARK_PALETTE, DARK_SIDEBARS)
    }

    pub fn light() -> Self {
        Self::from_palette(false, LIGHT_PALETTE, LIGHT_SIDEBARS)
    }

    fn from_palette(dark: bool, p: [u32; 13], sidebars: [u32; 3]) -> Self {
        let color = |hex: u32| Hsla::from(rgb(hex));
        Self {
            dark,
            bg: color(p[0]),
            sidebar: color(p[1]),
            sidebar_accent: color(sidebars[0]),
            right_sidebar: color(sidebars[1]),
            right_sidebar_accent: color(sidebars[2]),
            surface: color(p[2]),
            surface_hover: color(p[3]),
            border: if dark {
                hsla(0.0, 0.0, 1.0, DARK_BORDER_ALPHA)
            } else {
                color(p[4])
            },
            fg: color(p[5]),
            muted: color(p[6]),
            faint: color(p[7]),
            accent: color(p[8]),
            accent_soft: color(p[9]),
            success: color(p[10]),
            warning: color(p[11]),
            danger: color(p[12]),
        }
    }

    /// What `terminal.theme = "auto"` resolves against: the appearance this theme *is*, so a
    /// pinned `appearance` also pins the automatic terminal theme.
    pub fn system_appearance(&self) -> SystemAppearance {
        if self.dark {
            SystemAppearance::Dark
        } else {
            SystemAppearance::Light
        }
    }

    /// Text on an `accent` fill (primary buttons, the send button).
    pub fn on_accent(&self) -> Hsla {
        rgb(0xFFFFFF).into()
    }

    /// The one soft shadow of popovers, the palette and modals: 0 8 24 rgba(0,0,0,.12) light,
    /// .4 dark.
    pub fn shadow(&self) -> Vec<BoxShadow> {
        vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, if self.dark { 0.40 } else { 0.12 }),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(24.0),
            spread_radius: px(0.0),
            inset: false,
        }]
    }

    /// Dimming layer behind modals and sheets.
    pub fn scrim(&self) -> Hsla {
        hsla(0.0, 0.0, 0.0, if self.dark { 0.50 } else { 0.28 })
    }

    /// Drag-and-drop target highlight.
    pub fn drop_zone(&self) -> Hsla {
        self.accent.opacity(if self.dark { 0.28 } else { 0.22 })
    }
}

impl Theme {
    /// The block overlay's colors and gutter for `density` (design.md: success / error tones of
    /// the attention model, neutral while running). `animate` is false under Reduce Motion.
    pub fn block_settings(
        &self,
        density: Density,
        enabled: bool,
        animate: bool,
    ) -> hutan_terminal_view::BlockSettings {
        let rgb = |color: Hsla| {
            let c: Rgba = color.into();
            hutan_terminal_view::Rgb(
                (c.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (c.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (c.b * 255.0).round().clamp(0.0, 255.0) as u8,
            )
        };
        hutan_terminal_view::BlockSettings {
            enabled,
            gutter_width: density.ui().block_gutter,
            success: rgb(self.success),
            error: rgb(self.danger),
            neutral: rgb(self.muted),
            accent: rgb(self.accent),
            animate,
        }
    }
}

/// Whether the user asked macOS to reduce motion (System Settings ▸ Accessibility ▸ Display).
/// Read once per process through `defaults`; anything unreadable counts as "motion is fine".
/// The `HUTAN_REDUCE_MOTION` variable (`0` / `1`) overrides it.
pub fn reduce_motion() -> bool {
    use std::sync::OnceLock;
    static REDUCE: OnceLock<bool> = OnceLock::new();
    *REDUCE.get_or_init(|| {
        if let Some(value) = std::env::var_os("HUTAN_REDUCE_MOTION") {
            return value == "1";
        }
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_of(color: Hsla) -> Rgba {
        color.into()
    }

    fn hex(color: Hsla) -> u32 {
        let c = rgba_of(color);
        let byte = |v: f32| (v * 255.0).round() as u32;
        (byte(c.r) << 16) | (byte(c.g) << 8) | byte(c.b)
    }

    fn luminance(c: Rgba) -> f32 {
        let lin = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (la, lb) = (luminance(rgba_of(a)), luminance(rgba_of(b)));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    fn table(theme: &Theme) -> [u32; 13] {
        [
            hex(theme.bg),
            hex(theme.sidebar),
            hex(theme.surface),
            hex(theme.surface_hover),
            hex(theme.border),
            hex(theme.fg),
            hex(theme.muted),
            hex(theme.faint),
            hex(theme.accent),
            hex(theme.accent_soft),
            hex(theme.success),
            hex(theme.warning),
            hex(theme.danger),
        ]
    }

    #[test]
    fn token_table_matches_the_v2_palette() {
        // bg, sidebar, surface, surface_hover, border, fg, muted, faint, accent, accent_soft,
        // success, warning, danger: docs/design.md § Visual system.
        assert_eq!(
            table(&Theme::light()),
            [
                0xFAF9F5, 0xF3F1EA, 0xFFFFFF, 0xF0EEE6, 0xE6E3D9, 0x1F1E1D, 0x6F6D66, 0xA8A59C,
                0xE0692E, 0xFBE9DF, 0x3F8F5A, 0xC98A12, 0xC8452F
            ]
        );
        // The dark hairline is white at 7 % (Orca's `--border`), so its hex is the blend-less
        // white of the table; every other token is opaque.
        assert_eq!(
            table(&Theme::dark()),
            [
                0x262624, 0x1F1E1D, 0x30302E, 0x3A3936, 0xFFFFFF, 0xF5F4EF, 0xA6A49C, 0x6F6D66,
                0xEA7A43, 0x3E2C23, 0x6BBF85, 0xE2AE45, 0xE87562
            ]
        );
        assert!(Theme::dark().dark && !Theme::light().dark);
        assert_eq!(rgba_of(Theme::light().border).a, 1.0);
        assert!((rgba_of(Theme::dark().border).a - 0.07).abs() < 1e-6);
    }

    #[test]
    fn the_sidebars_have_their_own_surfaces() {
        // Orca's structure: the right panel is not the left one; selected left rows step away
        // from the left surface (up in dark, down in light); the right panel is darker than the
        // left in dark and lighter in light.
        for theme in [Theme::dark(), Theme::light()] {
            let (left, accent, right, right_accent) = (
                hex(theme.sidebar),
                hex(theme.sidebar_accent),
                hex(theme.right_sidebar),
                hex(theme.right_sidebar_accent),
            );
            assert!(left != right && left != accent && right != right_accent);
            assert_eq!(accent > left, theme.dark, "dark={}", theme.dark);
            assert_eq!(right < left, theme.dark, "dark={}", theme.dark);
            for ground in [theme.sidebar, theme.sidebar_accent, theme.right_sidebar] {
                assert!(contrast(theme.fg, ground) >= 7.0);
                // Selected rows: like `accent_soft`, large-text AA for secondary labels.
                let floor = if hex(ground) == hex(theme.sidebar_accent) {
                    4.0
                } else {
                    4.5
                };
                assert!(
                    contrast(theme.muted, ground) >= floor,
                    "{:06x}",
                    hex(ground)
                );
            }
            assert!(contrast(theme.muted, theme.right_sidebar_accent) >= 4.0);
        }
        assert_eq!((TITLEBAR_HEIGHT, STATUS_BAR_HEIGHT), (36.0, 24.0));
    }

    #[test]
    fn terminal_background_matches_the_bg_token() {
        use hutan_settings::themes;
        for (theme, name) in [
            (Theme::dark(), themes::DARK),
            (Theme::light(), themes::LIGHT),
        ] {
            let terminal = themes::lookup(name).expect("built-in theme");
            let bg = (u32::from(terminal.background.r) << 16)
                | (u32::from(terminal.background.g) << 8)
                | u32::from(terminal.background.b);
            assert_eq!(bg, hex(theme.bg), "{name}");
        }
    }

    #[test]
    fn appearance_resolves_system_light_and_dark() {
        use WindowAppearance::{Dark, Light, VibrantDark, VibrantLight};
        // `system` follows the window appearance, vibrant variants included.
        for (window, dark) in [
            (Light, false),
            (VibrantLight, false),
            (Dark, true),
            (VibrantDark, true),
        ] {
            assert_eq!(Theme::for_chrome(AppearanceMode::System, window).dark, dark);
            assert_eq!(Theme::for_appearance(window).dark, dark);
            // A pinned mode ignores the system.
            assert!(Theme::for_chrome(AppearanceMode::Dark, window).dark);
            assert!(!Theme::for_chrome(AppearanceMode::Light, window).dark);
        }
        assert_eq!(
            hex(Theme::for_chrome(AppearanceMode::System, Dark).bg),
            0x262624
        );
    }

    #[test]
    fn a_system_switch_changes_a_system_theme_live_and_leaves_a_pinned_one_alone() {
        let mut system = Theme::for_chrome(AppearanceMode::System, WindowAppearance::Light);
        let mut pinned = Theme::for_chrome(AppearanceMode::Light, WindowAppearance::Light);
        assert!(!system.dark && !pinned.dark);
        // macOS flips to dark: the workbench re-resolves on the appearance change.
        system = Theme::for_chrome(AppearanceMode::System, WindowAppearance::Dark);
        pinned = Theme::for_chrome(AppearanceMode::Light, WindowAppearance::Dark);
        assert!(system.dark && !pinned.dark);
        assert_eq!(system.system_appearance(), SystemAppearance::Dark);
        assert_eq!(pinned.system_appearance(), SystemAppearance::Light);
    }

    #[test]
    fn text_tokens_are_readable_on_every_surface() {
        for theme in [Theme::dark(), Theme::light()] {
            for ground in [theme.bg, theme.sidebar, theme.surface, theme.accent_soft] {
                assert!(contrast(theme.fg, ground) >= 7.0, "fg dark={}", theme.dark);
                // The spec's light `muted` on `accent_soft` is 4.4:1: large-text AA, used only
                // for secondary labels on selected rows.
                let floor = if hex(ground) == hex(theme.accent_soft) {
                    4.0
                } else {
                    4.5
                };
                assert!(
                    contrast(theme.muted, ground) >= floor,
                    "muted on {:06x} dark={}: {:.2}",
                    hex(ground),
                    theme.dark,
                    contrast(theme.muted, ground)
                );
            }
            // Hover fills must keep secondary text readable too.
            assert!(contrast(theme.muted, theme.surface_hover) >= 4.0);
            // Borders stay quieter than text but visible.
            assert!(contrast(theme.border, theme.bg) > 1.0);
        }
    }

    #[test]
    fn block_colors_and_gutter_follow_the_chrome_theme_and_density() {
        let dark = Theme::dark().block_settings(Density::Compact, true, true);
        assert_eq!(dark.success, hutan_terminal_view::Rgb(0x6b, 0xbf, 0x85));
        assert_eq!(dark.error, hutan_terminal_view::Rgb(0xe8, 0x75, 0x62));
        assert_eq!(dark.accent, hutan_terminal_view::Rgb(0xea, 0x7a, 0x43));
        assert_eq!(dark.gutter_width, 3.0);
        assert!(dark.enabled && dark.animate);
        let light = Theme::light().block_settings(Density::Comfortable, false, false);
        assert_eq!(light.success, hutan_terminal_view::Rgb(0x3f, 0x8f, 0x5a));
        assert_eq!(light.gutter_width, 4.0);
        assert!(!light.enabled && !light.animate);
    }

    #[test]
    fn density_tokens_match_the_v2_table() {
        let c = Density::Compact.ui();
        assert_eq!(
            (
                c.sidebar_row,
                c.tab_height,
                c.pane_header,
                c.status_bar,
                c.spacing,
                c.radius,
                c.card_radius
            ),
            (26.0, 30.0, 30.0, 24.0, 4.0, 6.0, 10.0)
        );
        let m = Density::Comfortable.ui();
        assert_eq!(
            (
                m.sidebar_row,
                m.tab_height,
                m.pane_header,
                m.status_bar,
                m.spacing,
                m.radius,
                m.card_radius
            ),
            (30.0, 34.0, 36.0, 24.0, 6.0, 8.0, 12.0)
        );
        assert_eq!((c.font_size, m.font_size), (12.0, 13.0));
        assert_eq!((c.secondary_font_size, m.secondary_font_size), (11.0, 12.0));
        assert_eq!((c.section_font_size, m.section_font_size), (11.0, 11.0));
        assert_eq!((c.prose_font_size, c.prose_line_height), (15.0, 1.6));
        assert_eq!((c.terminal_padding, m.terminal_padding), (8.0, 12.0));
        assert_eq!((c.block_gutter, m.block_gutter), (3.0, 4.0));
        assert_eq!((c.control_height, m.control_height), (28.0, 28.0));
        assert_eq!(Density::default(), Density::Compact);
    }
}
