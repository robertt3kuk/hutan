//! Resolution: declarations (a pack, the built-in it extends, the user's overrides) become one
//! immutable [`ResolvedTheme`] with every color, number and font filled in. Renderers read the
//! typed result; they never parse, look strings up or touch the disk.
//!
//! Order (ADR 0018, spike §3.2): the base's declared values, then the pack's, then the user's
//! overrides; then each color the pack leaves open is derived from the colors above it in the
//! registry, so changing `state.success` also recolors the default `diff.added`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::color::{Rgb, Rgba};
use super::manifest::{Declaration, appearance_name, density_index};
use super::tokens::{Color, DensityKey, FontRole, Metric, valid_token_id};
use crate::appearance::Density;
use crate::error::SettingsError;
use crate::themes::SystemAppearance;

/// Where a resolved color came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Written in the pack, its base or an override.
    Declared,
    /// Computed by the token's fallback.
    Derived,
    /// Not drawn (only `terminal.selection.foreground` can be).
    Absent,
}

/// Ordered family lists of the three font roles; each is never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fonts {
    pub ui: Vec<String>,
    pub prose: Vec<String>,
    pub mono: Vec<String>,
}

impl Fonts {
    pub fn role(&self, role: FontRole) -> &[String] {
        match role {
            FontRole::Ui => &self.ui,
            FontRole::Prose => &self.prose,
            FontRole::Mono => &self.mono,
        }
    }
}

/// The per-density numbers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DensityValues([f32; DensityKey::COUNT]);

impl DensityValues {
    pub fn get(&self, key: DensityKey) -> f32 {
        self.0[key.index()]
    }
}

/// A fully resolved color pack.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    pub id: String,
    pub name: String,
    pub appearance: SystemAppearance,
    colors: [Rgba; Color::COUNT],
    provenance: [Provenance; Color::COUNT],
    density: [DensityValues; 2],
    metrics: [f32; Metric::COUNT],
    fonts: Fonts,
}

impl ResolvedTheme {
    pub fn color(&self, token: Color) -> Rgba {
        self.colors[token.index()]
    }

    pub fn provenance(&self, token: Color) -> Provenance {
        self.provenance[token.index()]
    }

    /// The color when it is drawn; `None` for an absent optional role.
    pub fn color_if_set(&self, token: Color) -> Option<Rgba> {
        (self.provenance(token) != Provenance::Absent).then(|| self.color(token))
    }

    /// The color when a pack, its base or an override wrote it; `None` when it is derived, so a
    /// consumer with live information (an application's OSC color) can derive it itself.
    pub fn declared_color(&self, token: Color) -> Option<Rgba> {
        (self.provenance(token) == Provenance::Declared).then(|| self.color(token))
    }

    pub fn density(&self, density: Density) -> &DensityValues {
        &self.density[density_index(density)]
    }

    pub fn metric(&self, metric: Metric) -> f32 {
        self.metrics[metric.index()]
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    /// The deterministic text form the golden snapshots compare: every token in registry order
    /// with its value and whether it was declared or derived.
    pub fn snapshot(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "id = {}", self.id);
        let _ = writeln!(out, "appearance = {}", appearance_name(self.appearance));
        out.push_str("\n[colors]\n");
        for token in Color::ALL {
            let how = match self.provenance(*token) {
                Provenance::Declared => "",
                Provenance::Derived => "  (derived)",
                Provenance::Absent => "  (absent)",
            };
            let value = if self.provenance(*token) == Provenance::Absent {
                "none".to_owned()
            } else {
                self.color(*token).to_string()
            };
            let _ = writeln!(out, "{} = {value}{how}", token.id());
        }
        for (name, density) in [
            ("compact", Density::Compact),
            ("comfortable", Density::Comfortable),
        ] {
            let _ = writeln!(out, "\n[density.{name}]");
            for key in DensityKey::ALL {
                let _ = writeln!(out, "{} = {}", key.id(), self.density(density).get(*key));
            }
        }
        out.push_str("\n[metrics]\n");
        for metric in Metric::ALL {
            let _ = writeln!(out, "{} = {}", metric.id(), self.metric(*metric));
        }
        out.push_str("\n[fonts]\n");
        for role in FontRole::ALL {
            let _ = writeln!(out, "{} = {:?}", role.id(), self.fonts.role(role));
        }
        out
    }

    /// The terminal section as the terminal view consumes it.
    pub fn terminal(&self) -> TerminalTheme {
        let rgb = |token: Color| self.color(token).rgb();
        let mut ansi = [Rgb::new(0, 0, 0); 16];
        for (slot, token) in ansi.iter_mut().zip(Color::ANSI) {
            *slot = rgb(token);
        }
        TerminalTheme {
            name: self.id.clone(),
            foreground: rgb(Color::TerminalForeground),
            background: rgb(Color::TerminalBackground),
            cursor: rgb(Color::TerminalCursor),
            cursor_foreground: self
                .declared_color(Color::TerminalCursorForeground)
                .map(Rgba::rgb),
            selection: rgb(Color::TerminalSelectionBackground),
            selection_foreground: self
                .color_if_set(Color::TerminalSelectionForeground)
                .map(Rgba::rgb),
            ansi,
            blocks: BlockColors {
                success: rgb(Color::BlockSuccess),
                error: rgb(Color::BlockError),
                neutral: rgb(Color::BlockNeutral),
                accent: rgb(Color::BlockAccent),
                separator: self.declared_color(Color::BlockSeparator),
                highlight: self.declared_color(Color::BlockHighlight),
                hover: self.declared_color(Color::BlockHover),
                folded: self.declared_color(Color::BlockFolded),
            },
        }
    }

    /// The editor section as the code view consumes it (E-5).
    pub fn editor(&self) -> EditorTheme {
        let at = |token: Color| self.color(token);
        EditorTheme {
            name: self.id.clone(),
            background: at(Color::EditorBackground),
            foreground: at(Color::EditorForeground),
            gutter_background: at(Color::EditorGutterBackground),
            gutter_foreground: at(Color::EditorGutterForeground),
            line_number_active: at(Color::EditorLineNumberActive),
            current_line: at(Color::EditorCurrentLine),
            selection: at(Color::EditorSelection),
            selection_inactive: at(Color::EditorSelectionInactive),
            cursor: at(Color::EditorCursor),
            whitespace: at(Color::EditorWhitespace),
            indent_guide: at(Color::EditorIndentGuide),
            bracket_match: at(Color::EditorBracketMatch),
            search_match: at(Color::EditorSearchMatch),
            search_match_active: at(Color::EditorSearchMatchActive),
            diagnostic_error: at(Color::EditorDiagnosticError),
            diagnostic_warning: at(Color::EditorDiagnosticWarning),
            diagnostic_info: at(Color::EditorDiagnosticInfo),
            diagnostic_hint: at(Color::EditorDiagnosticHint),
            diff_added: at(Color::EditorDiffGutterAdded),
            diff_modified: at(Color::EditorDiffGutterModified),
            diff_removed: at(Color::EditorDiffGutterRemoved),
            agent_changed: at(Color::EditorAgentChanged),
            syntax: Color::SYNTAX.map(|token| self.color(token).rgb()),
        }
    }
}

/// The code editor's colors: the surface roles and one opaque color per `syntax.*` token.
/// Translucent roles are meant to be drawn over the editor's own background.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorTheme {
    /// Id of the pack this comes from.
    pub name: String,
    pub background: Rgba,
    pub foreground: Rgba,
    pub gutter_background: Rgba,
    pub gutter_foreground: Rgba,
    pub line_number_active: Rgba,
    pub current_line: Rgba,
    pub selection: Rgba,
    pub selection_inactive: Rgba,
    pub cursor: Rgba,
    pub whitespace: Rgba,
    pub indent_guide: Rgba,
    pub bracket_match: Rgba,
    pub search_match: Rgba,
    pub search_match_active: Rgba,
    pub diagnostic_error: Rgba,
    pub diagnostic_warning: Rgba,
    pub diagnostic_info: Rgba,
    pub diagnostic_hint: Rgba,
    pub diff_added: Rgba,
    pub diff_modified: Rgba,
    pub diff_removed: Rgba,
    pub agent_changed: Rgba,
    /// The `syntax.*` colors in the order of `hutan_syntax::Token::ALL`: index with
    /// `token as usize`.
    pub syntax: [Rgb; 22],
}

/// The terminal view's colors: a complete palette plus the block overlay roles.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTheme {
    /// Id of the pack this comes from.
    pub name: String,
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// The glyph under a block cursor; `None` draws it in the terminal background.
    pub cursor_foreground: Option<Rgb>,
    /// Background of selected cells.
    pub selection: Rgb,
    /// Text of selected cells; `None` keeps each cell's own foreground.
    pub selection_foreground: Option<Rgb>,
    /// ANSI colors 0-7 (normal) then 8-15 (bright).
    pub ansi: [Rgb; 16],
    pub blocks: BlockColors,
}

/// Colors of the block overlay. The four fills are `Some` only when a pack declares them; when
/// they are `None` the view derives them from the terminal colors in effect (the application's
/// OSC 10/11 overrides included), with the algorithms of the token fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockColors {
    pub success: Rgb,
    pub error: Rgb,
    pub neutral: Rgb,
    pub accent: Rgb,
    pub separator: Option<Rgba>,
    pub highlight: Option<Rgba>,
    pub hover: Option<Rgba>,
    pub folded: Option<Rgba>,
}

/// The four block fills a terminal derives from the colors it is actually showing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockFills {
    pub separator: Rgba,
    pub highlight: Rgba,
    pub hover: Rgba,
    pub folded: Rgba,
}

/// The fallbacks of `terminal.block.separator`, `.highlight`, `.hover` and `.folded` evaluated
/// on live colors: a terminal whose application changed its background (OSC 11) keeps tints
/// that match, and the algorithms stay the registry's (not a second copy in the view).
pub fn derive_block_fills(
    background: Rgb,
    foreground: Rgb,
    accent: Rgb,
    neutral: Rgb,
) -> BlockFills {
    let mut values = [Rgba::opaque(Rgb::new(0, 0, 0)); Color::COUNT];
    values[Color::TerminalBackground.index()] = Rgba::opaque(background);
    values[Color::TerminalForeground.index()] = Rgba::opaque(foreground);
    values[Color::BlockAccent.index()] = Rgba::opaque(accent);
    values[Color::BlockNeutral.index()] = Rgba::opaque(neutral);
    let mut fill = |token: Color| {
        let color = token.derive(&values, true).unwrap_or(values[token.index()]);
        values[token.index()] = color;
        color
    };
    BlockFills {
        separator: fill(Color::BlockSeparator),
        highlight: fill(Color::BlockHighlight),
        hover: fill(Color::BlockHover),
        folded: fill(Color::BlockFolded),
    }
}

/// The user's color overrides of one pack, validated.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColorOverrides(pub(super) Vec<(Color, Rgba)>);

impl ColorOverrides {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Validates `appearance.theme_overrides.<id>.colors`: an unknown token is a warning and
    /// skipped; a malformed or translucent-where-opaque value is an error, and then none of the
    /// overrides must be applied.
    pub fn parse(
        colors: &BTreeMap<String, String>,
        key_prefix: &str,
    ) -> (Self, Vec<SettingsError>) {
        let mut out = Vec::new();
        let mut diagnostics = Vec::new();
        for (id, text) in colors {
            let key = format!("{key_prefix}.{id}");
            let Some(token) = Color::from_id(id) else {
                let why = if valid_token_id(id) {
                    "unknown color token ignored"
                } else {
                    "not a valid color token id; ignored"
                };
                diagnostics.push(SettingsError::warning(None, key, why));
                continue;
            };
            match text.parse::<Rgba>() {
                Err(_) => diagnostics.push(SettingsError::error(
                    None,
                    key,
                    format!(
                        "expected #rrggbb[aa], found \"{}\"",
                        text.chars().take(24).collect::<String>()
                    ),
                )),
                Ok(color) if token.spec().opaque && !color.is_opaque() => {
                    diagnostics.push(SettingsError::error(
                        None,
                        key,
                        "must be opaque (#rrggbb)".to_owned(),
                    ));
                }
                Ok(color) => out.push((token, color)),
            }
        }
        (Self(out), diagnostics)
    }
}

/// One thing wrong with a resolved pack: `(key path, message)`.
pub type Problem = (String, String);

/// Resolves `pack` over `base` (the built-in it extends; `None` for a root pack) with the user's
/// `overrides`. Errors name the token; nothing is clamped.
pub fn resolve(
    pack: &Declaration,
    base: Option<&Declaration>,
    overrides: &ColorOverrides,
) -> Result<ResolvedTheme, Vec<Problem>> {
    let mut problems: Vec<Problem> = Vec::new();
    let dark = pack.meta.appearance == SystemAppearance::Dark;

    let mut declared: Vec<Option<Rgba>> = Color::ALL
        .iter()
        .map(|token| {
            pack.color(*token)
                .or_else(|| base.and_then(|b| b.color(*token)))
        })
        .collect();
    for (token, color) in &overrides.0 {
        declared[token.index()] = Some(*color);
    }
    let placeholder = Rgba::opaque(Rgb::new(0, 0, 0));
    let mut colors = vec![placeholder; Color::COUNT];
    let mut provenance = [Provenance::Derived; Color::COUNT];
    for token in Color::ALL {
        let i = token.index();
        match declared[i] {
            Some(color) => {
                colors[i] = color;
                provenance[i] = Provenance::Declared;
            }
            None => match token.derive(&colors, dark) {
                Some(color) => colors[i] = color,
                None if token.is_required() => problems.push((
                    format!("colors.{}", token.id()),
                    "required, but neither the pack nor its base declares it".to_owned(),
                )),
                None => provenance[i] = Provenance::Absent,
            },
        }
    }

    let mut density = [DensityValues([0.0; DensityKey::COUNT]); 2];
    for which in [Density::Compact, Density::Comfortable] {
        let name = match which {
            Density::Compact => "compact",
            Density::Comfortable => "comfortable",
        };
        for key in DensityKey::ALL {
            match pack
                .density(which, *key)
                .or_else(|| base.and_then(|b| b.density(which, *key)))
            {
                Some(v) => density[density_index(which)].0[key.index()] = v,
                None => problems.push((
                    format!("density.{name}.{}", key.id()),
                    "required, but neither the pack nor its base declares it".to_owned(),
                )),
            }
        }
    }
    let mut metrics = [0.0; Metric::COUNT];
    for metric in Metric::ALL {
        match pack
            .metric(*metric)
            .or_else(|| base.and_then(|b| b.metric(*metric)))
        {
            Some(v) => metrics[metric.index()] = v,
            None => problems.push((
                format!("metrics.{}", metric.id()),
                "required, but neither the pack nor its base declares it".to_owned(),
            )),
        }
    }
    let family = |role: FontRole| {
        pack.fonts(role)
            .or_else(|| base.and_then(|b| b.fonts(role)))
            .map(<[String]>::to_vec)
    };
    let (Some(ui), Some(prose), Some(mono)) = (
        family(FontRole::Ui),
        family(FontRole::Prose),
        family(FontRole::Mono),
    ) else {
        problems.push((
            "fonts".to_owned(),
            "required, but neither the pack nor its base declares every role".to_owned(),
        ));
        return Err(problems);
    };

    if problems.is_empty() {
        check_coupled(&density, &metrics, &mut problems);
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let Ok(colors) = <[Rgba; Color::COUNT]>::try_from(colors) else {
        return Err(vec![(
            "colors".to_owned(),
            "internal color table size".to_owned(),
        )]);
    };
    Ok(ResolvedTheme {
        id: pack.meta.id.clone(),
        name: pack.meta.name.clone(),
        appearance: pack.meta.appearance,
        colors,
        provenance,
        density,
        metrics,
        fonts: Fonts { ui, prose, mono },
    })
}

/// Cross-field geometry: a row, tab, header or control must be tall enough for its text, and the
/// right strip and title bar for their buttons. The defaults leave a few pixels of margin.
fn check_coupled(
    density: &[DensityValues; 2],
    metrics: &[f32; Metric::COUNT],
    out: &mut Vec<Problem>,
) {
    for (name, which) in [
        ("compact", Density::Compact),
        ("comfortable", Density::Comfortable),
    ] {
        let d = &density[density_index(which)];
        let body = d.get(DensityKey::TextBodySize);
        let secondary = d.get(DensityKey::TextSecondarySize);
        for (key, text, label) in [
            (DensityKey::SidebarRowHeight, body, "text.body.size"),
            (DensityKey::TabHeight, body, "text.body.size"),
            (DensityKey::PaneHeaderHeight, body, "text.body.size"),
            (DensityKey::ControlHeight, body, "text.body.size"),
            (
                DensityKey::StatusBarHeight,
                secondary,
                "text.secondary.size",
            ),
        ] {
            let height = d.get(key);
            if height < text + 2.0 {
                out.push((
                    format!("density.{name}.{}", key.id()),
                    format!("{height} is too small for {label} = {text}; it must be at least {} (text size + 2)", text + 2.0),
                ));
            }
        }
    }
    let m = |metric: Metric| metrics[metric.index()];
    for (key, value, floor, label) in [
        (
            Metric::NavLineHeight,
            m(Metric::NavLineHeight),
            m(Metric::NavTextSize),
            "nav.text.size",
        ),
        (
            Metric::StripButtonSize,
            m(Metric::StripButtonSize),
            m(Metric::NavIconSize),
            "nav.icon.size",
        ),
        (
            Metric::TitlebarHeight,
            m(Metric::TitlebarHeight),
            m(Metric::StripButtonSize),
            "strip.button.size",
        ),
    ] {
        if value < floor {
            out.push((
                format!("metrics.{}", key.id()),
                format!("{value} is smaller than {label} = {floor}"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::manifest::parse;
    use super::*;

    fn declaration(extra: &str) -> Declaration {
        let text = format!(
            "schema_version = 1\nid = \"t\"\nname = \"T\"\nauthors = [\"a\"]\nlicense = \"MIT\"\nappearance = \"dark\"\nextends = \"builtin:hutan-dark\"\n{extra}"
        );
        let parsed = parse(&text, None);
        assert!(
            parsed.diagnostics.iter().all(|d| !d.is_error()),
            "{:?}",
            parsed.diagnostics
        );
        parsed.declaration.unwrap()
    }

    #[test]
    fn overrides_validate_token_and_value() {
        let mut colors = BTreeMap::new();
        colors.insert("ui.accent".to_owned(), "#d99a58".to_owned());
        colors.insert("ui.nope".to_owned(), "#000000".to_owned());
        let (overrides, diagnostics) =
            ColorOverrides::parse(&colors, "appearance.theme_overrides.x.colors");
        assert_eq!(overrides.0.len(), 1);
        assert_eq!(diagnostics.len(), 1);
        assert!(!diagnostics[0].is_error());
        colors.insert("ui.foreground".to_owned(), "#ffffff10".to_owned());
        colors.insert("ui.border".to_owned(), "blue".to_owned());
        let (_, diagnostics) = ColorOverrides::parse(&colors, "k");
        assert_eq!(diagnostics.iter().filter(|d| d.is_error()).count(), 2);
    }

    #[test]
    fn live_block_fills_use_the_registry_fallbacks() {
        let fills = derive_block_fills(
            Rgb::new(0, 0, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(10, 20, 30),
            Rgb::new(200, 200, 200),
        );
        assert_eq!(fills.separator.rgb(), Rgb::new(41, 41, 41));
        assert_eq!(fills.folded.rgb(), Rgb::new(18, 18, 18));
        assert_eq!(
            (fills.highlight.rgb(), fills.highlight.a),
            (Rgb::new(10, 20, 30), 0.13)
        );
        assert_eq!(
            (fills.hover.rgb(), fills.hover.a),
            (Rgb::new(200, 200, 200), 0.07)
        );
    }

    #[test]
    fn coupled_geometry_is_checked() {
        let mut density = [DensityValues([20.0; DensityKey::COUNT]); 2];
        let mut metrics = [20.0; Metric::COUNT];
        for d in &mut density {
            d.0[DensityKey::TextBodySize.index()] = 12.0;
            d.0[DensityKey::TextSecondarySize.index()] = 11.0;
        }
        density[0].0[DensityKey::SidebarRowHeight.index()] = 13.0;
        metrics[Metric::NavLineHeight.index()] = 10.0;
        metrics[Metric::NavTextSize.index()] = 13.0;
        let mut problems = Vec::new();
        check_coupled(&density, &metrics, &mut problems);
        let keys: Vec<&str> = problems.iter().map(|(k, _)| k.as_str()).collect();
        assert!(
            keys.contains(&"density.compact.sidebar.row-height"),
            "{keys:?}"
        );
        assert!(keys.contains(&"metrics.nav.line-height"), "{keys:?}");
        assert!(!keys.iter().any(|k| k.contains("tab.height")));
        let _ = declaration("");
    }
}
