//! The color pack manifest (`theme.toml`, schema v1): parsing and validation into a
//! [`Declaration`]. Pure: no I/O, no GPUI. Everything the file declares is kept as declared (no
//! inheritance, no fallbacks); [`super::resolve`] turns declarations into a resolved theme.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use super::color::Rgba;
use super::tokens::{
    Color, DensityKey, FontRole, Metric, NumberSpec, valid_pack_id, valid_token_id,
};
use crate::appearance::Density;
use crate::error::SettingsError;
use crate::themes::SystemAppearance;

/// The one schema version this Hutan reads.
pub const SCHEMA_VERSION: i64 = 1;
/// A manifest larger than this is refused unread.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Color, density and metric entries per manifest.
pub const MAX_ENTRIES: usize = 512;
/// Deepest nesting of tables and arrays a manifest may have (the schema needs 3).
pub const MAX_DEPTH: usize = 5;
/// Families per `[fonts]` list.
pub const MAX_FAMILIES: usize = 8;

/// The immutable embedded pack a sparse pack extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    HutanDark,
    HutanLight,
}

impl Builtin {
    pub const ALL: [Builtin; 2] = [Builtin::HutanDark, Builtin::HutanLight];

    pub const fn id(self) -> &'static str {
        match self {
            Self::HutanDark => "hutan-dark",
            Self::HutanLight => "hutan-light",
        }
    }

    pub const fn appearance(self) -> SystemAppearance {
        match self {
            Self::HutanDark => SystemAppearance::Dark,
            Self::HutanLight => SystemAppearance::Light,
        }
    }

    /// `builtin:hutan-dark`.
    pub fn reference(self) -> String {
        format!("builtin:{}", self.id())
    }

    fn from_reference(text: &str) -> Option<Self> {
        let id = text.strip_prefix("builtin:")?;
        Self::ALL.into_iter().find(|b| b.id() == id)
    }
}

/// The identity and provenance block at the manifest's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub id: String,
    pub name: String,
    pub authors: Vec<String>,
    pub license: String,
    pub license_file: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    pub appearance: SystemAppearance,
    pub root: bool,
    pub extends: Option<Builtin>,
}

/// What a pack declares: only the tokens it names. A sparse pack leaves most entries `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub meta: Meta,
    pub(super) colors: [Option<Rgba>; Color::COUNT],
    /// Indexed by [`Density`] (`compact`, `comfortable`).
    pub(super) density: [[Option<f32>; DensityKey::COUNT]; 2],
    pub(super) metrics: [Option<f32>; Metric::COUNT],
    pub(super) fonts: [Option<Vec<String>>; 3],
    /// Hash of the manifest text; unchanged text means an unchanged pack.
    pub source_hash: u64,
}

impl Declaration {
    pub fn color(&self, token: Color) -> Option<Rgba> {
        self.colors[token.index()]
    }

    pub fn density(&self, density: Density, key: DensityKey) -> Option<f32> {
        self.density[density_index(density)][key.index()]
    }

    pub fn metric(&self, metric: Metric) -> Option<f32> {
        self.metrics[metric.index()]
    }

    pub fn fonts(&self, role: FontRole) -> Option<&[String]> {
        self.fonts[role.index()].as_deref()
    }

    /// How many colors, density values and metrics the manifest sets.
    pub fn entry_count(&self) -> usize {
        self.colors.iter().flatten().count()
            + self.density.iter().flatten().flatten().count()
            + self.metrics.iter().flatten().count()
    }
}

pub(super) const fn density_index(density: Density) -> usize {
    match density {
        Density::Compact => 0,
        Density::Comfortable => 1,
    }
}

/// The result of parsing one manifest: the declaration when it has no error, and every
/// diagnostic (warnings included).
#[derive(Debug)]
pub struct Parsed {
    pub declaration: Option<Declaration>,
    pub diagnostics: Vec<SettingsError>,
}

pub(super) fn hash_text(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

struct Sink<'a> {
    file: Option<&'a Path>,
    out: Vec<SettingsError>,
    errors: usize,
}

impl Sink<'_> {
    fn error(&mut self, key: impl Into<String>, message: impl Into<String>) {
        self.errors += 1;
        self.out.push(SettingsError::error(self.file, key, message));
    }

    fn warn(&mut self, key: impl Into<String>, message: impl Into<String>) {
        self.out
            .push(SettingsError::warning(self.file, key, message));
    }
}

fn has_control(text: &str) -> bool {
    text.chars().any(char::is_control)
}

fn depth(value: &toml::Value, limit: usize) -> usize {
    if limit == 0 {
        return 1;
    }
    match value {
        toml::Value::Table(table) => {
            1 + table
                .values()
                .map(|v| depth(v, limit - 1))
                .max()
                .unwrap_or(0)
        }
        toml::Value::Array(items) => {
            1 + items.iter().map(|v| depth(v, limit - 1)).max().unwrap_or(0)
        }
        _ => 0,
    }
}

/// Parses and validates a manifest. `file` only labels the diagnostics.
pub fn parse(text: &str, file: Option<&Path>) -> Parsed {
    let mut sink = Sink {
        file,
        out: Vec::new(),
        errors: 0,
    };
    let declaration = parse_into(text, &mut sink);
    Parsed {
        declaration: if sink.errors == 0 { declaration } else { None },
        diagnostics: sink.out,
    }
}

fn parse_into(text: &str, sink: &mut Sink<'_>) -> Option<Declaration> {
    if text.len() > MAX_MANIFEST_BYTES {
        sink.error(
            "",
            format!(
                "manifest is {} bytes; the limit is {MAX_MANIFEST_BYTES}",
                text.len()
            ),
        );
        return None;
    }
    let table: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(err) => {
            let line = err.span().map(|span| {
                text.as_bytes()[..span.start.min(text.len())]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count()
                    + 1
            });
            sink.error(
                "",
                match line {
                    Some(line) => format!("invalid TOML (line {line}): {}", err.message()),
                    None => format!("invalid TOML: {}", err.message()),
                },
            );
            return None;
        }
    };
    if depth(&toml::Value::Table(table.clone()), MAX_DEPTH + 1) > MAX_DEPTH {
        sink.error(
            "",
            format!("tables are nested deeper than {MAX_DEPTH} levels"),
        );
        return None;
    }

    // The version first: a future schema cannot be read best-effort.
    match table.get("schema_version") {
        Some(toml::Value::Integer(v)) if *v == SCHEMA_VERSION => {}
        Some(toml::Value::Integer(v)) => {
            sink.error(
                "schema_version",
                format!("unsupported schema_version {v}; this Hutan reads schema_version {SCHEMA_VERSION}"),
            );
            return None;
        }
        Some(_) => {
            sink.error("schema_version", "expected the integer 1");
            return None;
        }
        None => {
            sink.error("schema_version", "required; write schema_version = 1");
            return None;
        }
    }

    const KNOWN: [&str; 16] = [
        "schema_version",
        "id",
        "name",
        "authors",
        "license",
        "license_file",
        "version",
        "description",
        "repository",
        "appearance",
        "root",
        "extends",
        "colors",
        "fonts",
        "density",
        "metrics",
    ];
    for key in table.keys() {
        if key != "provenance" && !KNOWN.contains(&key.as_str()) {
            sink.warn(key.clone(), "unknown key ignored");
        }
    }
    if let Some(value) = table.get("provenance")
        && !value.is_table()
    {
        sink.error("provenance", "expected a table");
    }

    let meta = parse_meta(&table, sink);
    let mut declaration = Declaration {
        meta: meta.clone().unwrap_or_else(placeholder_meta),
        colors: [None; Color::COUNT],
        density: [[None; DensityKey::COUNT]; 2],
        metrics: [None; Metric::COUNT],
        fonts: [None, None, None],
        source_hash: hash_text(text),
    };
    parse_colors(&table, &mut declaration, sink);
    parse_density(&table, &mut declaration, sink);
    parse_metrics(&table, &mut declaration, sink);
    parse_fonts(&table, &mut declaration, sink);
    if declaration.entry_count() > MAX_ENTRIES {
        sink.error(
            "",
            format!(
                "{} color, density and metric entries; the limit is {MAX_ENTRIES}",
                declaration.entry_count()
            ),
        );
    }
    let meta = meta?;
    if meta.root {
        check_complete(&declaration, sink);
    }
    Some(declaration)
}

fn placeholder_meta() -> Meta {
    Meta {
        id: String::new(),
        name: String::new(),
        authors: Vec::new(),
        license: String::new(),
        license_file: None,
        version: None,
        description: None,
        repository: None,
        appearance: SystemAppearance::Dark,
        root: false,
        extends: None,
    }
}

fn string_field(
    table: &toml::Table,
    key: &str,
    required: bool,
    max_chars: usize,
    sink: &mut Sink<'_>,
) -> Option<String> {
    match table.get(key) {
        None => {
            if required {
                sink.error(key, "required");
            }
            None
        }
        Some(toml::Value::String(s)) => {
            let chars = s.chars().count();
            if chars == 0 || chars > max_chars {
                sink.error(key, format!("must be 1 to {max_chars} characters"));
                None
            } else if has_control(s) {
                sink.error(key, "must not contain control characters");
                None
            } else {
                Some(s.clone())
            }
        }
        Some(_) => {
            sink.error(key, "expected a string");
            None
        }
    }
}

fn parse_meta(table: &toml::Table, sink: &mut Sink<'_>) -> Option<Meta> {
    let before = sink.errors;
    let id = match table.get("id") {
        Some(toml::Value::String(id)) if valid_pack_id(id) && id != super::AUTO => Some(id.clone()),
        Some(toml::Value::String(id)) if id == super::AUTO => {
            sink.error("id", "`auto` is reserved for the terminal.theme selector");
            None
        }
        Some(toml::Value::String(id)) => {
            sink.error(
                "id",
                format!(
                    "`{id}` is not a pack id: use lower-case words joined by `-` (1-64 characters, e.g. `canopy-night`)"
                ),
            );
            None
        }
        Some(_) => {
            sink.error("id", "expected a string");
            None
        }
        None => {
            sink.error("id", "required");
            None
        }
    };
    let name = string_field(table, "name", true, 120, sink);
    let authors = match table.get("authors") {
        Some(toml::Value::Array(items)) if !items.is_empty() => {
            let mut authors = Vec::new();
            for (i, item) in items.iter().enumerate() {
                match item {
                    toml::Value::String(s)
                        if (1..=200).contains(&s.chars().count()) && !has_control(s) =>
                    {
                        authors.push(s.clone());
                    }
                    _ => sink.error(
                        format!("authors[{i}]"),
                        "expected a string of 1 to 200 characters without control characters",
                    ),
                }
            }
            Some(authors)
        }
        Some(_) => {
            sink.error("authors", "expected a non-empty array of strings");
            None
        }
        None => {
            sink.error("authors", "required");
            None
        }
    };
    let license = string_field(table, "license", true, 200, sink);
    let license_file = string_field(table, "license_file", false, 200, sink);
    if let Some(path) = &license_file
        && let Err(why) = super::confine::check_relative(path)
    {
        sink.error("license_file", why);
    }
    let version = string_field(table, "version", false, 64, sink);
    let description = string_field(table, "description", false, 1024, sink);
    let repository = string_field(table, "repository", false, 512, sink);
    if let Some(url) = &repository
        && (!url.starts_with("https://") || url.chars().any(char::is_whitespace))
    {
        sink.error("repository", "expected an https:// URL without spaces");
    }

    let appearance = match table.get("appearance") {
        Some(toml::Value::String(a)) if a == "dark" => Some(SystemAppearance::Dark),
        Some(toml::Value::String(a)) if a == "light" => Some(SystemAppearance::Light),
        Some(_) => {
            sink.error("appearance", "expected \"dark\" or \"light\"");
            None
        }
        None => {
            sink.error("appearance", "required: \"dark\" or \"light\"");
            None
        }
    };
    let root = match table.get("root") {
        None => false,
        Some(toml::Value::Boolean(b)) => *b,
        Some(_) => {
            sink.error("root", "expected true or false");
            false
        }
    };
    let extends = match table.get("extends") {
        None => None,
        Some(toml::Value::String(reference)) => match Builtin::from_reference(reference) {
            Some(base) => Some(base),
            None => {
                sink.error(
                    "extends",
                    format!(
                        "unknown base `{reference}`; use builtin:hutan-dark or builtin:hutan-light"
                    ),
                );
                None
            }
        },
        Some(_) => {
            sink.error(
                "extends",
                "expected a string such as \"builtin:hutan-dark\"",
            );
            None
        }
    };
    match (root, table.contains_key("extends"), appearance, extends) {
        (true, true, ..) => sink.error("extends", "a root pack is complete and cannot extend"),
        (false, false, ..) => sink.error(
            "extends",
            "required unless root = true; use extends = \"builtin:hutan-dark\" (or hutan-light)",
        ),
        (false, true, Some(appearance), Some(base)) if base.appearance() != appearance => sink
            .error(
                "extends",
                format!(
                    "{} is a {} pack but this one declares appearance = \"{}\"",
                    base.reference(),
                    appearance_name(base.appearance()),
                    appearance_name(appearance)
                ),
            ),
        _ => {}
    }
    if sink.errors > before {
        return None;
    }
    Some(Meta {
        id: id?,
        name: name?,
        authors: authors?,
        license: license?,
        license_file,
        version,
        description,
        repository,
        appearance: appearance?,
        root,
        extends,
    })
}

pub(super) fn appearance_name(appearance: SystemAppearance) -> &'static str {
    match appearance {
        SystemAppearance::Dark => "dark",
        SystemAppearance::Light => "light",
    }
}

fn parse_colors(table: &toml::Table, declaration: &mut Declaration, sink: &mut Sink<'_>) {
    let Some(value) = table.get("colors") else {
        return;
    };
    let Some(colors) = value.as_table() else {
        sink.error("colors", "expected a table");
        return;
    };
    for (id, value) in colors {
        let key = format!("colors.{id}");
        if value.is_table() {
            sink.error(
                &key,
                "nested tables are not allowed; write the whole id as one quoted key, e.g. \"ui.border\" = \"#e6e3d9\"",
            );
            continue;
        }
        let Some(token) = Color::from_id(id) else {
            warn_unknown(sink, &key, id, "color");
            continue;
        };
        let Some(text) = value.as_str() else {
            sink.error(&key, "expected a color string \"#rrggbb\" or \"#rrggbbaa\"");
            continue;
        };
        match text.parse::<Rgba>() {
            Err(_) => sink.error(
                &key,
                format!("expected #rrggbb[aa], found \"{}\"", clip(text)),
            ),
            Ok(color) if token.spec().opaque && !color.is_opaque() => sink.error(
                &key,
                format!("must be opaque (#rrggbb); \"{}\" has an alpha", clip(text)),
            ),
            Ok(color) => declaration.colors[token.index()] = Some(color),
        }
    }
}

fn clip(text: &str) -> String {
    text.chars().take(24).collect()
}

fn warn_unknown(sink: &mut Sink<'_>, key: &str, id: &str, what: &str) {
    if id == "syntax" || id.starts_with("syntax.") {
        sink.warn(
            key,
            "unsupported: the syntax.* namespace is reserved until the editor ships; ignored",
        );
    } else if !valid_token_id(id) {
        sink.warn(
            key,
            format!(
                "`{id}` is not a valid {what} id (lower-case words joined by `.` and `-`); ignored"
            ),
        );
    } else {
        sink.warn(key, format!("unknown {what} token ignored"));
    }
}

fn number(value: &toml::Value) -> Option<f32> {
    match value {
        toml::Value::Integer(i) => Some(*i as f32),
        toml::Value::Float(f) if f.is_finite() => Some(*f as f32),
        _ => None,
    }
}

fn read_number(
    value: &toml::Value,
    spec: &NumberSpec,
    key: &str,
    sink: &mut Sink<'_>,
) -> Option<f32> {
    let Some(n) = number(value) else {
        sink.error(key, "expected a finite number");
        return None;
    };
    if n < spec.min || n > spec.max {
        sink.error(key, format!("{n} is outside {} to {}", spec.min, spec.max));
        return None;
    }
    Some(n)
}

fn parse_density(table: &toml::Table, declaration: &mut Declaration, sink: &mut Sink<'_>) {
    let Some(value) = table.get("density") else {
        return;
    };
    let Some(densities) = value.as_table() else {
        sink.error("density", "expected a table");
        return;
    };
    for (name, value) in densities {
        let which = match name.as_str() {
            "compact" => Density::Compact,
            "comfortable" => Density::Comfortable,
            _ => {
                sink.warn(format!("density.{name}"), "unknown density ignored");
                continue;
            }
        };
        let Some(entries) = value.as_table() else {
            sink.error(format!("density.{name}"), "expected a table");
            continue;
        };
        for (id, value) in entries {
            let key = format!("density.{name}.{id}");
            let Some(token) = DensityKey::from_id(id) else {
                warn_unknown(sink, &key, id, "density");
                continue;
            };
            if let Some(n) = read_number(value, token.spec(), &key, sink) {
                declaration.density[density_index(which)][token.index()] = Some(n);
            }
        }
    }
}

fn parse_metrics(table: &toml::Table, declaration: &mut Declaration, sink: &mut Sink<'_>) {
    let Some(value) = table.get("metrics") else {
        return;
    };
    let Some(metrics) = value.as_table() else {
        sink.error("metrics", "expected a table");
        return;
    };
    for (id, value) in metrics {
        let key = format!("metrics.{id}");
        let Some(token) = Metric::from_id(id) else {
            warn_unknown(sink, &key, id, "metric");
            continue;
        };
        if let Some(n) = read_number(value, token.spec(), &key, sink) {
            declaration.metrics[token.index()] = Some(n);
        }
    }
}

fn parse_fonts(table: &toml::Table, declaration: &mut Declaration, sink: &mut Sink<'_>) {
    let Some(value) = table.get("fonts") else {
        return;
    };
    let Some(fonts) = value.as_table() else {
        sink.error("fonts", "expected a table");
        return;
    };
    for (id, value) in fonts {
        let key = format!("fonts.{id}");
        let Some(role) = FontRole::from_id(id) else {
            sink.warn(key, "unknown font role ignored");
            continue;
        };
        let Some(items) = value.as_array() else {
            sink.error(key, "expected an array of installed family names");
            continue;
        };
        if items.is_empty() || items.len() > MAX_FAMILIES {
            sink.error(key, format!("expected 1 to {MAX_FAMILIES} family names"));
            continue;
        }
        let mut families = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match item {
                toml::Value::String(s)
                    if (1..=120).contains(&s.chars().count()) && !has_control(s) =>
                {
                    families.push(s.clone());
                }
                _ => sink.error(
                    format!("{key}[{i}]"),
                    "expected a family name of 1 to 120 characters",
                ),
            }
        }
        if families.len() == items.len() {
            declaration.fonts[role.index()] = Some(families);
        }
    }
}

/// A root pack has no base to inherit from, so it must declare everything.
fn check_complete(declaration: &Declaration, sink: &mut Sink<'_>) {
    let missing_colors: Vec<&str> = Color::ALL
        .iter()
        .filter(|c| c.is_required() && declaration.colors[c.index()].is_none())
        .map(|c| c.id())
        .collect();
    report_missing(sink, "colors", &missing_colors);
    for density in [Density::Compact, Density::Comfortable] {
        let missing: Vec<&str> = DensityKey::ALL
            .iter()
            .filter(|k| declaration.density[density_index(density)][k.index()].is_none())
            .map(|k| k.id())
            .collect();
        let name = match density {
            Density::Compact => "density.compact",
            Density::Comfortable => "density.comfortable",
        };
        report_missing(sink, name, &missing);
    }
    let missing_metrics: Vec<&str> = Metric::ALL
        .iter()
        .filter(|m| declaration.metrics[m.index()].is_none())
        .map(|m| m.id())
        .collect();
    report_missing(sink, "metrics", &missing_metrics);
    let missing_fonts: Vec<&str> = FontRole::ALL
        .iter()
        .filter(|r| declaration.fonts[r.index()].is_none())
        .map(|r| r.id())
        .collect();
    report_missing(sink, "fonts", &missing_fonts);
}

fn report_missing(sink: &mut Sink<'_>, section: &str, missing: &[&str]) {
    if missing.is_empty() {
        return;
    }
    let shown = missing
        .iter()
        .take(6)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let more = missing.len().saturating_sub(6);
    sink.error(
        section,
        if more == 0 {
            format!("a root pack must declare {shown}")
        } else {
            format!(
                "a root pack must declare {shown} and {more} more ({} missing)",
                missing.len()
            )
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPARSE: &str = r##"
schema_version = 1
id = "canopy-night"
name = "Canopy Night"
authors = ["Someone"]
license = "MIT"
appearance = "dark"
extends = "builtin:hutan-dark"

[colors]
"ui.accent" = "#d99a58"
"ui.border" = "#ffffff20"
"##;

    fn errors(parsed: &Parsed) -> Vec<String> {
        parsed
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| format!("{}: {}", d.key_path, d.message))
            .collect()
    }

    #[test]
    fn a_sparse_pack_parses_with_only_what_it_declares() {
        let parsed = parse(SPARSE, None);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let decl = parsed.declaration.unwrap();
        assert_eq!(decl.meta.id, "canopy-night");
        assert_eq!(decl.meta.extends, Some(Builtin::HutanDark));
        assert_eq!(decl.entry_count(), 2);
        assert_eq!(decl.color(Color::Accent).unwrap().to_string(), "#d99a58");
        assert!(decl.color(Color::Background).is_none());
    }

    #[test]
    fn metadata_is_validated() {
        let cases = [
            ("id = \"canopy-night\"", "id = \"Canopy_Night\"", "id"),
            ("id = \"canopy-night\"", "id = \"auto\"", "id"),
            ("name = \"Canopy Night\"", "name = \"\"", "name"),
            ("authors = [\"Someone\"]", "authors = []", "authors"),
            ("license = \"MIT\"", "license = 3", "license"),
            (
                "appearance = \"dark\"",
                "appearance = \"sepia\"",
                "appearance",
            ),
            (
                "extends = \"builtin:hutan-dark\"",
                "extends = \"builtin:hutan-light\"",
                "extends",
            ),
            (
                "extends = \"builtin:hutan-dark\"",
                "extends = \"builtin:nope\"",
                "extends",
            ),
            (
                "extends = \"builtin:hutan-dark\"",
                "extends = \"builtin:hutan-dark\"\nrepository = \"http://x.example\"",
                "repository",
            ),
            (
                "extends = \"builtin:hutan-dark\"",
                "extends = \"builtin:hutan-dark\"\nroot = true",
                "extends",
            ),
        ];
        for (from, to, key) in cases {
            let parsed = parse(&SPARSE.replace(from, to), None);
            assert!(parsed.declaration.is_none(), "{to}");
            assert!(
                parsed
                    .diagnostics
                    .iter()
                    .any(|d| d.is_error() && d.key_path == key),
                "{to}: {:?}",
                parsed.diagnostics
            );
        }
        let no_extends = SPARSE.replace("extends = \"builtin:hutan-dark\"\n", "");
        assert!(parse(&no_extends, None).declaration.is_none());
    }

    #[test]
    fn colors_are_checked_by_type_grammar_and_opacity() {
        let text = SPARSE.replace(
            "\"ui.border\" = \"#ffffff20\"",
            "\"ui.border\" = \"red\"\n\"ui.foreground\" = \"#ffffff80\"\n\"ui.surface\" = 3\n\"ui.text\" = \"#fff\"\n\"ui.background\" = { x = 1 }",
        );
        let parsed = parse(&text, None);
        assert!(parsed.declaration.is_none());
        let errors = errors(&parsed);
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("colors.ui.border: expected #rrggbb[aa]")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("colors.ui.foreground: must be opaque")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("colors.ui.surface: expected a color string")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("colors.ui.background: nested tables")),
            "{errors:?}"
        );
        assert_eq!(errors.len(), 4, "{errors:?}");
    }

    #[test]
    fn unknown_tokens_and_keys_warn_and_syntax_is_reported_unsupported() {
        let text = format!(
            "{SPARSE}\"ui.nope\" = \"#000000\"\n\"syntax.keyword\" = \"#ff0000\"\n\"UI.Bad\" = \"#000000\"\n"
        )
        .replace("[colors]", "mood = \"calm\"\n[colors]");
        let parsed = parse(&text, None);
        assert!(parsed.declaration.is_some(), "{:?}", parsed.diagnostics);
        let warnings: Vec<_> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.is_error(), d.key_path.as_str(), d.message.as_str()))
            .collect();
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert!(warnings.iter().all(|(error, ..)| !error));
        assert!(
            warnings
                .iter()
                .any(|(_, key, m)| *key == "mood" && m.contains("unknown key"))
        );
        assert!(
            warnings
                .iter()
                .any(|(_, key, m)| *key == "colors.syntax.keyword" && m.contains("unsupported"))
        );
        assert!(
            warnings
                .iter()
                .any(|(_, key, m)| *key == "colors.UI.Bad" && m.contains("not a valid"))
        );
    }

    #[test]
    fn an_unsupported_schema_version_is_refused_with_a_diagnostic() {
        for (text, needle) in [
            (
                "schema_version = 2\nid = \"x\"",
                "unsupported schema_version 2",
            ),
            ("schema_version = \"1\"", "expected the integer 1"),
            ("id = \"x\"", "required"),
        ] {
            let parsed = parse(text, None);
            assert!(parsed.declaration.is_none());
            assert_eq!(parsed.diagnostics.len(), 1, "{text}");
            assert!(parsed.diagnostics[0].message.contains(needle), "{text}");
            assert_eq!(parsed.diagnostics[0].key_path, "schema_version");
        }
    }

    #[test]
    fn malformed_toml_and_duplicate_keys_are_errors_with_a_line() {
        let parsed = parse("schema_version = 1\nid = \"a\"\nid = \"b\"\n", None);
        assert!(parsed.declaration.is_none());
        assert!(
            parsed.diagnostics[0]
                .message
                .starts_with("invalid TOML (line 3)"),
            "{:?}",
            parsed.diagnostics
        );
        assert!(parse("schema_version = [", None).declaration.is_none());
    }

    #[test]
    fn caps_refuse_oversized_deep_and_crowded_manifests() {
        let huge = format!("{SPARSE}\n# {}\n", "x".repeat(MAX_MANIFEST_BYTES));
        let parsed = parse(&huge, None);
        assert!(parsed.declaration.is_none());
        assert!(parsed.diagnostics[0].message.contains("the limit is"));

        let deep = format!(
            "{SPARSE}\n[provenance]\ndeep = {{ a = {{ b = {{ c = {{ d = {{ e = 1 }} }} }} }} }}\n"
        );
        let parsed = parse(&deep, None);
        assert!(parsed.declaration.is_none());
        assert!(parsed.diagnostics[0].message.contains("nested deeper"));

        // A flat manifest of many valid entries is not nested; entries are capped separately.
        let mut crowded = String::from(SPARSE);
        for i in 0..MAX_ENTRIES {
            crowded.push_str(&format!("\"ui.t{i}\" = \"#000000\"\n"));
        }
        // Unknown tokens are not entries.
        assert!(parse(&crowded, None).declaration.is_some());
    }

    #[test]
    fn numbers_are_bounded_and_typed() {
        let text = format!(
            "{SPARSE}\n[density.compact]\nspacing = 4\n\"text.body.size\" = 90\n\"tab.height\" = \"tall\"\n\"text.prose.line-height\" = 0.1\n\n[metrics]\n\"shadow.blur\" = -3\n\"reading.width\" = 720.5\n"
        );
        let parsed = parse(&text, None);
        let errors = errors(&parsed);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("density.compact.text.body.size: 90 is outside 8 to 40"))
        );
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("density.compact.tab.height: expected a finite number"))
        );
        assert!(errors.iter().any(|e| e.starts_with("metrics.shadow.blur")));
    }

    #[test]
    fn fonts_are_lists_of_bounded_names() {
        let ok = format!(
            "{SPARSE}\n[fonts]\nui = [\".SystemUIFont\"]\nmono = [\"Menlo\", \"monospace\"]\n"
        );
        let decl = parse(&ok, None).declaration.unwrap();
        assert_eq!(decl.fonts(FontRole::Mono).unwrap(), ["Menlo", "monospace"]);
        assert!(decl.fonts(FontRole::Prose).is_none());
        for bad in ["ui = []", "ui = \"Menlo\"", "ui = [3]", "ui = [\"\"]"] {
            let text = format!("{SPARSE}\n[fonts]\n{bad}\n");
            assert!(parse(&text, None).declaration.is_none(), "{bad}");
        }
    }

    #[test]
    fn a_root_pack_must_be_complete() {
        let text = r##"
schema_version = 1
id = "bare"
name = "Bare"
authors = ["a"]
license = "MIT"
appearance = "light"
root = true

[colors]
"ui.background" = "#ffffff"
"##;
        let parsed = parse(text, None);
        assert!(parsed.declaration.is_none());
        let errors = errors(&parsed);
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("colors: a root pack must declare ui.sidebar.background")),
            "{errors:?}"
        );
        assert!(errors.iter().any(|e| e.starts_with("density.compact:")));
        assert!(errors.iter().any(|e| e.starts_with("metrics:")));
        assert!(
            errors
                .iter()
                .any(|e| e.starts_with("fonts: a root pack must declare ui, prose, mono"))
        );
    }
}
