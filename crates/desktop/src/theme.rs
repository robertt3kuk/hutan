//! The chrome's colors, density tokens and shared metrics, as the GPUI types the views draw
//! with. Every value comes from the active color pack (`hutan_settings::themes`, docs/themes.md):
//! this module holds no palette. A [`ResolvedTheme`] becomes a [`Theme`] (colors) and, through
//! [`install`], the density [`Tokens`] and shared [`Metrics`] that `Density::ui()` and
//! [`metrics`] read, once per committed theme generation.

use std::cell::RefCell;
use std::sync::{Arc, LazyLock};

use gpui::{BoxShadow, Hsla, Rgba as GpuiRgba, WindowAppearance, point, px};
use hutan_settings::themes::{
    Builtin, Color, DensityKey, Metric, ResolvedTheme, SelectionRequest, TerminalTheme,
    ThemeCatalogSnapshot, ThemeRegistry, builtin_for, builtin_theme,
};
use hutan_settings::{AppearanceMode, AppearanceSettings, SettingsError, SystemAppearance};

pub use hutan_settings::Density;

/// Sizes in logical pixels: the active pack's density table plus the type scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub sidebar_row: f32,
    pub tab_height: f32,
    pub pane_header: f32,
    pub status_bar: f32,
    pub spacing: f32,
    /// Corner radius of controls (buttons, rows, inputs): 6 px in both densities.
    pub radius: f32,
    /// Corner radius of cards, the composer, popovers and the palette: 10 px in both densities.
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

impl Tokens {
    /// The pack's numbers for `density`.
    pub fn of(theme: &ResolvedTheme, density: Density) -> Self {
        let d = theme.density(density);
        let get = |key: DensityKey| d.get(key);
        Self {
            sidebar_row: get(DensityKey::SidebarRowHeight),
            tab_height: get(DensityKey::TabHeight),
            pane_header: get(DensityKey::PaneHeaderHeight),
            status_bar: get(DensityKey::StatusBarHeight),
            spacing: get(DensityKey::Spacing),
            radius: get(DensityKey::RadiusControl),
            card_radius: get(DensityKey::RadiusCard),
            font_size: get(DensityKey::TextBodySize),
            secondary_font_size: get(DensityKey::TextSecondarySize),
            section_font_size: get(DensityKey::TextSectionSize),
            prose_font_size: get(DensityKey::TextProseSize),
            prose_line_height: get(DensityKey::TextProseLineHeight),
            control_height: get(DensityKey::ControlHeight),
            terminal_padding: get(DensityKey::TerminalPadding),
            block_gutter: get(DensityKey::TerminalBlockGutter),
        }
    }
}

/// The pack's numbers that do not change with density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Height of the title bar and of the right sidebar's icon strip.
    pub titlebar_height: f32,
    /// Corner radius of pills, badges and round buttons.
    pub pill_radius: f32,
    /// Width of the chat's reading column.
    pub reading_width: f32,
    /// Size of the mascot in empty states.
    pub mascot_size: f32,
    pub nav_text_size: f32,
    pub nav_icon_size: f32,
    pub nav_line_height: f32,
    pub nav_padding_x: f32,
    pub nav_padding_y: f32,
    pub nav_gap: f32,
    pub strip_button_size: f32,
    pub strip_gap: f32,
    /// The pack's terminal font size and line height; the user's `terminal.*` settings win.
    pub terminal_font_size: f32,
    pub terminal_line_height: f32,
    shadow_offset: (f32, f32),
    shadow_blur: f32,
    shadow_spread: f32,
}

impl Metrics {
    pub fn of(theme: &ResolvedTheme) -> Self {
        let m = |metric: Metric| theme.metric(metric);
        Self {
            titlebar_height: m(Metric::TitlebarHeight),
            pill_radius: m(Metric::RadiusPill),
            reading_width: m(Metric::ReadingWidth),
            mascot_size: m(Metric::MascotSize),
            nav_text_size: m(Metric::NavTextSize),
            nav_icon_size: m(Metric::NavIconSize),
            nav_line_height: m(Metric::NavLineHeight),
            nav_padding_x: m(Metric::NavPaddingX),
            nav_padding_y: m(Metric::NavPaddingY),
            nav_gap: m(Metric::NavGap),
            strip_button_size: m(Metric::StripButtonSize),
            strip_gap: m(Metric::StripGap),
            terminal_font_size: m(Metric::TerminalFontSize),
            terminal_line_height: m(Metric::TerminalLineHeight),
            shadow_offset: (m(Metric::ShadowOffsetX), m(Metric::ShadowOffsetY)),
            shadow_blur: m(Metric::ShadowBlur),
            shadow_spread: m(Metric::ShadowSpread),
        }
    }
}

/// What the GPUI thread draws with: both densities' tokens and the metrics of the committed pack.
#[derive(Clone, Copy, Debug)]
struct Active {
    tokens: [Tokens; 2],
    metrics: Metrics,
}

impl Active {
    fn of(theme: &ResolvedTheme) -> Self {
        Self {
            tokens: [
                Tokens::of(theme, Density::Compact),
                Tokens::of(theme, Density::Comfortable),
            ],
            metrics: Metrics::of(theme),
        }
    }
}

thread_local! {
    /// Views read density tokens and metrics without a `cx` (`Density::ui()`); the workbench
    /// installs the committed pack here, on the UI thread, whenever the theme generation
    /// changes. Before any workbench exists it holds the built-in dark pack.
    static ACTIVE: RefCell<Active> = RefCell::new(Active::of(&builtin_theme(Builtin::HutanDark)));
}

/// Makes `theme`'s density tokens and metrics the ones `Density::ui()` and [`metrics`] return on
/// this thread.
pub fn install(theme: &ResolvedTheme) {
    ACTIVE.with(|active| *active.borrow_mut() = Active::of(theme));
}

/// The committed pack's shared metrics.
pub fn metrics() -> Metrics {
    ACTIVE.with(|active| active.borrow().metrics)
}

/// Corner radius of pills, badges and round buttons.
pub fn pill_radius() -> f32 {
    metrics().pill_radius
}

/// Width of the chat's reading column.
pub fn reading_width() -> f32 {
    metrics().reading_width
}

/// Size of the mascot in empty states.
pub fn mascot_size() -> f32 {
    metrics().mascot_size
}

/// Desktop-side additions to [`Density`] (which lives in `hutan-settings`).
pub trait DensityExt: Sized {
    /// Layout tokens and font sizes of the committed pack for this density.
    fn ui(self) -> Tokens;
    fn label(self) -> &'static str;
    fn toggled(self) -> Self;
}

impl DensityExt for Density {
    fn ui(self) -> Tokens {
        ACTIVE.with(|active| {
            let tokens = &active.borrow().tokens;
            match self {
                Density::Compact => tokens[0],
                Density::Comfortable => tokens[1],
            }
        })
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

fn hsla(color: hutan_settings::Rgba) -> Hsla {
    Hsla::from(GpuiRgba {
        r: f32::from(color.r) / 255.0,
        g: f32::from(color.g) / 255.0,
        b: f32::from(color.b) / 255.0,
        a: color.a,
    })
}

/// An opaque pack color (a terminal palette entry) as a GPUI color.
pub fn rgb_color(color: hutan_settings::Rgb) -> Hsla {
    hsla(hutan_settings::Rgba::opaque(color))
}

/// Chrome colors: the pack's tokens as GPUI colors (docs/themes.md has the full list). These
/// are the only color values in the desktop: every view reads them from here.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// Window and center panes (`ui.background`).
    pub bg: Hsla,
    /// Left dock and the title bar strip (`ui.sidebar.background`).
    pub sidebar: Hsla,
    /// Selected nav row and active project card of the left sidebar (`ui.sidebar.selected`).
    pub sidebar_accent: Hsla,
    /// The right sidebar (`ui.sidebar-right.background`).
    pub right_sidebar: Hsla,
    /// Hover and selected rows of the right sidebar (`ui.sidebar-right.selected`).
    pub right_sidebar_accent: Hsla,
    /// Cards, composer, popovers, palette, modals (`ui.surface`).
    pub surface: Hsla,
    /// Row and button hover (`ui.surface.hover`).
    pub surface_hover: Hsla,
    /// Hairlines (1 px) (`ui.border`).
    pub border: Hsla,
    /// Primary text (`ui.foreground`).
    pub fg: Hsla,
    /// Secondary text and icons (`ui.text.muted`).
    pub muted: Hsla,
    /// Placeholders and disabled text (`ui.text.faint`).
    pub faint: Hsla,
    /// Primary buttons, selection, focus, links: the brand color (`ui.accent`).
    pub accent: Hsla,
    /// Selected row, chips, the user message bubble (`ui.accent.soft`).
    pub accent_soft: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub danger: Hsla,
    /// Hover fill of the left nav rows (`ui.sidebar.hover`).
    pub sidebar_hover: Hsla,
    /// File status and diff counts: `diff.added`, `diff.removed`, `diff.modified`,
    /// `diff.renamed`.
    pub diff_added: Hsla,
    pub diff_removed: Hsla,
    pub diff_modified: Hsla,
    pub diff_renamed: Hsla,
    /// Row tints of a diff: `diff.added.background`, `diff.removed.background`,
    /// `diff.hunk.background`, and the hunk header's text (`diff.hunk.foreground`).
    pub diff_added_bg: Hsla,
    pub diff_removed_bg: Hsla,
    pub diff_hunk_bg: Hsla,
    pub diff_hunk_fg: Hsla,
    /// Links and inline code in chat messages: `markdown.link`,
    /// `markdown.inline-code.background`.
    pub link: Hsla,
    pub code_bg: Hsla,
    on_accent: Hsla,
    shadow_color: Hsla,
    scrim_color: Hsla,
    drop_target: Hsla,
    shadow_offset: (f32, f32),
    shadow_blur: f32,
    shadow_spread: f32,
}

impl Theme {
    /// The chrome of a resolved pack.
    pub fn from_resolved(theme: &ResolvedTheme) -> Self {
        let color = |token: Color| hsla(theme.color(token));
        let metrics = Metrics::of(theme);
        Self {
            dark: theme.appearance == SystemAppearance::Dark,
            bg: color(Color::Background),
            sidebar: color(Color::SidebarBackground),
            sidebar_accent: color(Color::SidebarSelected),
            right_sidebar: color(Color::SidebarRightBackground),
            right_sidebar_accent: color(Color::SidebarRightSelected),
            surface: color(Color::Surface),
            surface_hover: color(Color::SurfaceHover),
            border: color(Color::Border),
            fg: color(Color::Foreground),
            muted: color(Color::TextMuted),
            faint: color(Color::TextFaint),
            accent: color(Color::Accent),
            accent_soft: color(Color::AccentSoft),
            success: color(Color::StateSuccess),
            warning: color(Color::StateWarning),
            danger: color(Color::StateError),
            sidebar_hover: color(Color::SidebarHover),
            diff_added: color(Color::DiffAdded),
            diff_removed: color(Color::DiffRemoved),
            diff_modified: color(Color::DiffModified),
            diff_renamed: color(Color::DiffRenamed),
            diff_added_bg: color(Color::DiffAddedBackground),
            diff_removed_bg: color(Color::DiffRemovedBackground),
            diff_hunk_bg: color(Color::DiffHunkBackground),
            diff_hunk_fg: color(Color::DiffHunkForeground),
            link: color(Color::MarkdownLink),
            code_bg: color(Color::MarkdownInlineCodeBackground),
            on_accent: color(Color::OnAccent),
            shadow_color: color(Color::Shadow),
            scrim_color: color(Color::Scrim),
            drop_target: color(Color::DropTarget),
            shadow_offset: metrics.shadow_offset,
            shadow_blur: metrics.shadow_blur,
            shadow_spread: metrics.shadow_spread,
        }
    }

    /// The built-in pack for the `appearance` setting; `system` follows the window appearance.
    /// (The workbench resolves the user's chosen packs through [`resolve_frame`]; this is the
    /// built-in look, for previews and tests.)
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
        Self::from_resolved(&builtin_theme(Builtin::HutanDark))
    }

    pub fn light() -> Self {
        Self::from_resolved(&builtin_theme(Builtin::HutanLight))
    }

    /// Text on an `accent` fill (primary buttons, the send button): `ui.on-accent`.
    pub fn on_accent(&self) -> Hsla {
        self.on_accent
    }

    /// The one soft shadow of popovers, the palette and modals: `ui.shadow` with the pack's
    /// `shadow.*` geometry.
    pub fn shadow(&self) -> Vec<BoxShadow> {
        vec![BoxShadow {
            color: self.shadow_color,
            offset: point(px(self.shadow_offset.0), px(self.shadow_offset.1)),
            blur_radius: px(self.shadow_blur),
            spread_radius: px(self.shadow_spread),
            inset: false,
        }]
    }

    /// Dimming layer behind modals and sheets: `ui.scrim`.
    pub fn scrim(&self) -> Hsla {
        self.scrim_color
    }

    /// Drag-and-drop target highlight: `ui.drop-target`.
    pub fn drop_zone(&self) -> Hsla {
        self.drop_target
    }
}

/// The block overlay's settings for a terminal theme: its `terminal.block.*` colors, the
/// density's gutter and the motion preference. `animate` is false under Reduce Motion.
pub fn block_settings(
    terminal: &TerminalTheme,
    density: Density,
    enabled: bool,
    animate: bool,
) -> hutan_terminal_view::BlockSettings {
    hutan_terminal_view::BlockSettings {
        enabled,
        gutter_width: density.ui().block_gutter,
        animate,
        ..hutan_terminal_view::BlockSettings::default()
    }
    .with_colors(&terminal.blocks)
}

/// Everything one resolution of the user's theme settings produces.
#[derive(Debug, Clone)]
pub struct ThemeFrame {
    /// The chrome pack in effect (the slot of the current polarity, overrides applied).
    pub resolved: Arc<ResolvedTheme>,
    /// The terminal palette `terminal.theme` selects.
    pub terminal: TerminalTheme,
    /// Problems with the packs and with the selection, for the Problems list.
    pub diagnostics: Vec<SettingsError>,
}

/// The polarity a window shows: `appearance.appearance`, or the window's own under `system`.
pub fn polarity(mode: AppearanceMode, window: WindowAppearance) -> SystemAppearance {
    match mode {
        AppearanceMode::Light => SystemAppearance::Light,
        AppearanceMode::Dark => SystemAppearance::Dark,
        AppearanceMode::System => match window {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => SystemAppearance::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => SystemAppearance::Light,
        },
    }
}

/// Resolves the theme settings against a catalog snapshot.
pub fn resolve_frame(
    registry: &ThemeRegistry,
    snapshot: &ThemeCatalogSnapshot,
    appearance: &AppearanceSettings,
    terminal_theme: &str,
    polarity: SystemAppearance,
) -> ThemeFrame {
    let selected = registry.select(
        snapshot,
        SelectionRequest {
            polarity,
            light_theme: &appearance.light_theme,
            dark_theme: &appearance.dark_theme,
            overrides: &appearance.theme_overrides,
        },
    );
    let (terminal, terminal_diagnostics) = registry.terminal(
        snapshot,
        terminal_theme,
        &selected.theme,
        &appearance.theme_overrides,
    );
    let mut diagnostics = snapshot.diagnostics().to_vec();
    diagnostics.extend(selected.diagnostics);
    diagnostics.extend(terminal_diagnostics);
    ThemeFrame {
        resolved: selected.theme,
        terminal,
        diagnostics,
    }
}

/// The built-in pack of a polarity, for code that runs before the user's selection is known.
pub fn builtin_of(polarity: SystemAppearance) -> Arc<ResolvedTheme> {
    builtin_theme(builtin_for(polarity))
}

/// Whether the user asked macOS to reduce motion (System Settings ▸ Accessibility ▸ Display).
/// Read once per process through `defaults`; anything unreadable counts as "motion is fine".
/// The `HUTAN_REDUCE_MOTION` variable (`0` / `1`) overrides it.
pub fn reduce_motion() -> bool {
    static REDUCE: LazyLock<bool> = LazyLock::new(|| {
        if let Some(value) = std::env::var_os("HUTAN_REDUCE_MOTION") {
            return value == "1";
        }
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .is_some_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "1")
    });
    *REDUCE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_of(color: Hsla) -> GpuiRgba {
        color.into()
    }

    fn hex(color: Hsla) -> u32 {
        let c = rgba_of(color);
        let byte = |v: f32| (v * 255.0).round() as u32;
        (byte(c.r) << 16) | (byte(c.g) << 8) | byte(c.b)
    }

    fn luminance(c: GpuiRgba) -> f32 {
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
                0xFAF9F5, 0xF3F1EA, 0xFFFFFF, 0xF0EEE6, 0xE6E3D9, 0x1F1E1D, 0x66645D, 0xA8A59C,
                0xE0692E, 0xFBE9DF, 0x2F7A48, 0x8A6300, 0xB93A26
            ]
        );
        // The dark hairline is white at 7 % (Orca's `--border`), so its hex is the blend-less
        // white of the table; every other token is opaque.
        assert_eq!(
            table(&Theme::dark()),
            [
                0x262624, 0x1F1E1D, 0x30302E, 0x3A3936, 0xFFFFFF, 0xF5F4EF, 0xAEACA4, 0x6F6D66,
                0xEA7A43, 0x3E2C23, 0x6BBF85, 0xE2AE45, 0xF08C7A
            ]
        );
        assert!(Theme::dark().dark && !Theme::light().dark);
        assert_eq!(rgba_of(Theme::light().border).a, 1.0);
        // 18/255: the nearest 8-bit alpha to 7 %.
        assert!((rgba_of(Theme::dark().border).a - 0.07).abs() < 0.5 / 255.0 + 1e-6);
    }

    #[test]
    fn effects_and_derived_roles_keep_the_old_values() {
        let (dark, light) = (Theme::dark(), Theme::light());
        let near = |got: f32, want: f32| (got - want).abs() < 0.5 / 255.0 + 1e-6;
        // Dark ink on the brand orange in both packs (CL-TOKENS, D-C1).
        assert_eq!(
            (hex(dark.on_accent()), hex(light.on_accent())),
            (0x1F1E1D, 0x1F1E1D)
        );
        assert!(near(rgba_of(dark.scrim()).a, 0.50) && near(rgba_of(light.scrim()).a, 0.28));
        let shadow = dark.shadow();
        assert!(near(rgba_of(shadow[0].color).a, 0.40));
        assert!(near(rgba_of(light.shadow()[0].color).a, 0.12));
        assert_eq!(
            (
                shadow[0].offset,
                shadow[0].blur_radius,
                shadow[0].spread_radius
            ),
            (point(px(0.0), px(8.0)), px(24.0), px(0.0))
        );
        // Exact fractions of the derived roles.
        assert_eq!(rgba_of(dark.drop_zone()).a, 0.28);
        assert_eq!(rgba_of(light.drop_zone()).a, 0.22);
        assert_eq!(hex(dark.drop_zone()), hex(dark.accent));
        assert_eq!(rgba_of(dark.sidebar_hover).a, 0.08);
        assert_eq!(hex(dark.sidebar_hover), hex(dark.fg));
        assert_eq!(hex(dark.diff_added), hex(dark.success));
        assert_eq!(rgba_of(dark.diff_added_bg).a, 0.16);
        assert_eq!(hex(light.diff_removed_bg), hex(light.danger));
        assert_eq!(rgba_of(light.diff_hunk_bg).a, 0.10);
        assert_eq!(hex(light.diff_hunk_fg), hex(light.muted));
        assert_eq!(hex(light.diff_modified), hex(light.accent));
        assert_eq!(hex(light.diff_renamed), hex(light.warning));
        assert_eq!(
            (hex(dark.link), hex(dark.code_bg)),
            (hex(dark.accent), hex(dark.surface_hover))
        );
        // Light links are their own, darker orange: the accent is 3.2:1 on `bg` (as text, a fail).
        assert_eq!(hex(light.link), 0xB04818);
    }

    #[test]
    fn the_sidebars_share_one_chrome_tone() {
        // Two chrome tones (CL-TOKENS, D-C2): the window and one sidebar tone. The right
        // sidebar has its own tokens but the built-ins give them the left sidebar's values;
        // selected rows step away from the surface (up in dark, down in light).
        for theme in [Theme::dark(), Theme::light()] {
            let (left, accent, right, right_accent) = (
                hex(theme.sidebar),
                hex(theme.sidebar_accent),
                hex(theme.right_sidebar),
                hex(theme.right_sidebar_accent),
            );
            assert_eq!((left, accent), (right, right_accent));
            assert!(left != accent);
            assert_eq!(accent > left, theme.dark, "dark={}", theme.dark);
            for ground in [theme.sidebar, theme.sidebar_accent, theme.right_sidebar] {
                assert!(contrast(theme.fg, ground) >= 7.0);
                assert!(contrast(theme.muted, ground) >= 4.5, "{:06x}", hex(ground));
            }
            assert!(contrast(theme.muted, theme.right_sidebar_accent) >= 4.5);
        }
        // The shared metrics of the built-ins: title bar and right strip 36 px.
        assert_eq!(metrics().titlebar_height, 36.0);
        assert_eq!(Density::Compact.ui().status_bar, 24.0);
        assert_eq!(Density::Comfortable.ui().status_bar, 24.0);
    }

    #[test]
    fn terminal_background_matches_the_bg_token() {
        for (theme, builtin) in [
            (Theme::dark(), Builtin::HutanDark),
            (Theme::light(), Builtin::HutanLight),
        ] {
            let terminal = builtin_theme(builtin).terminal();
            let bg = (u32::from(terminal.background.r) << 16)
                | (u32::from(terminal.background.g) << 8)
                | u32::from(terminal.background.b);
            assert_eq!(bg, hex(theme.bg), "{}", builtin.id());
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
            assert_eq!(
                polarity(AppearanceMode::System, window) == SystemAppearance::Dark,
                dark
            );
            assert_eq!(
                polarity(AppearanceMode::Dark, window),
                SystemAppearance::Dark
            );
            assert_eq!(
                polarity(AppearanceMode::Light, window),
                SystemAppearance::Light
            );
        }
        assert_eq!(
            hex(Theme::for_chrome(AppearanceMode::System, Dark).bg),
            0x262624
        );
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
    fn block_colors_and_gutter_follow_the_terminal_theme_and_density() {
        let dark = block_settings(
            &builtin_theme(Builtin::HutanDark).terminal(),
            Density::Compact,
            true,
            true,
        );
        assert_eq!(dark.success, hutan_terminal_view::Rgb(0x6b, 0xbf, 0x85));
        assert_eq!(dark.error, hutan_terminal_view::Rgb(0xf0, 0x8c, 0x7a));
        assert_eq!(dark.accent, hutan_terminal_view::Rgb(0xea, 0x7a, 0x43));
        assert_eq!(dark.neutral, hutan_terminal_view::Rgb(0xae, 0xac, 0xa4));
        assert_eq!(dark.gutter_width, 3.0);
        assert!(dark.enabled && dark.animate);
        assert_eq!(
            (dark.separator, dark.highlight, dark.hover, dark.folded),
            (None, None, None, None)
        );
        let light = block_settings(
            &builtin_theme(Builtin::HutanLight).terminal(),
            Density::Comfortable,
            false,
            false,
        );
        assert_eq!(light.success, hutan_terminal_view::Rgb(0x2f, 0x7a, 0x48));
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
            (30.0, 34.0, 36.0, 24.0, 6.0, 6.0, 10.0)
        );
        assert_eq!((c.font_size, m.font_size), (12.0, 13.0));
        assert_eq!((c.secondary_font_size, m.secondary_font_size), (11.0, 12.0));
        assert_eq!((c.section_font_size, m.section_font_size), (11.0, 11.0));
        assert_eq!((c.prose_font_size, c.prose_line_height), (15.0, 1.6));
        assert_eq!((c.terminal_padding, m.terminal_padding), (8.0, 12.0));
        assert_eq!((c.block_gutter, m.block_gutter), (3.0, 4.0));
        assert_eq!((c.control_height, m.control_height), (28.0, 28.0));
        assert_eq!(Density::default(), Density::Compact);
        let shared = metrics();
        assert_eq!(
            (shared.pill_radius, shared.reading_width, shared.mascot_size),
            (999.0, 720.0, 96.0)
        );
        assert_eq!(
            (
                shared.nav_text_size,
                shared.nav_icon_size,
                shared.nav_line_height
            ),
            (13.0, 16.0, 18.0)
        );
        assert_eq!((shared.strip_button_size, shared.strip_gap), (28.0, 4.0));
        assert_eq!(
            (shared.terminal_font_size, shared.terminal_line_height),
            (13.0, 1.2)
        );
    }

    #[test]
    fn installing_a_pack_changes_what_ui_reads_on_this_thread_only() {
        use hutan_settings::themes::ThemeCatalogSnapshot;
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes/roomy");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(
            themes.join("theme.toml"),
            "schema_version = 1\nid = \"roomy\"\nname = \"Roomy\"\nauthors = [\"t\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n[density.compact]\n\"sidebar.row-height\" = 40\n\"text.body.size\" = 14\n[metrics]\n\"radius.pill\" = 20\n",
        )
        .unwrap();
        let registry = ThemeRegistry::new(dir.path());
        let snapshot: Arc<ThemeCatalogSnapshot> = registry.snapshot();
        let pack = snapshot.pack("roomy").unwrap();
        install(pack.resolved());
        assert_eq!(Density::Compact.ui().sidebar_row, 40.0);
        assert_eq!(Density::Compact.ui().font_size, 14.0);
        assert_eq!(
            Density::Comfortable.ui().sidebar_row,
            30.0,
            "the other density is the base's"
        );
        assert_eq!(pill_radius(), 20.0);
        std::thread::spawn(|| assert_eq!(Density::Compact.ui().sidebar_row, 26.0))
            .join()
            .unwrap();
        install(&builtin_theme(Builtin::HutanDark));
        assert_eq!(Density::Compact.ui().sidebar_row, 26.0);
    }

    #[test]
    fn a_frame_resolves_chrome_and_terminal_independently() {
        let dir = tempfile::tempdir().unwrap();
        let registry = ThemeRegistry::new(dir.path());
        let snapshot = registry.snapshot();
        let settings = AppearanceSettings::default();
        let frame = resolve_frame(
            &registry,
            &snapshot,
            &settings,
            "hutan-light",
            SystemAppearance::Dark,
        );
        assert_eq!(frame.resolved.id, "hutan-dark");
        assert_eq!(frame.terminal.name, "hutan-light");
        assert!(frame.diagnostics.is_empty());
        let frame = resolve_frame(
            &registry,
            &snapshot,
            &settings,
            "auto",
            SystemAppearance::Light,
        );
        assert_eq!(
            (frame.resolved.id.as_str(), frame.terminal.name.as_str()),
            ("hutan-light", "hutan-light")
        );
        let frame = resolve_frame(
            &registry,
            &snapshot,
            &settings,
            "nope",
            SystemAppearance::Light,
        );
        assert_eq!(frame.terminal.name, "hutan-light");
        assert_eq!(frame.diagnostics.len(), 1);
        assert_eq!(builtin_of(SystemAppearance::Dark).id, "hutan-dark");
    }
}
