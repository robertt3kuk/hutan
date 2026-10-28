//! Icon metadata reuses color-pack metadata validation, with an independent base/polarity.
use std::collections::BTreeMap;
use std::path::Path;

use super::IconId;
use crate::error::{SettingsError, Severity};
use crate::themes::{Meta, SystemAppearance, manifest, valid_token_id};

#[derive(Debug, Clone)]
pub struct Declaration {
    pub meta: Meta,
    pub appearance: Option<SystemAppearance>,
    pub icons: BTreeMap<IconId, String>,
    /// `[small]`: the artwork for sizes up to [`super::SMALL_MAX_PX`], by id; may be partial.
    pub small: BTreeMap<IconId, String>,
    pub source_hash: u64,
    /// `[provenance]` names a `source_format`: converted from another tool's files.
    pub imported: bool,
}

pub fn parse(text: &str, file: Option<&Path>) -> (Option<Declaration>, Vec<SettingsError>) {
    let mut diagnostics = Vec::new();
    let fail = |message| (None, vec![SettingsError::error(file, "", message)]);
    if text.len() > manifest::MAX_MANIFEST_BYTES {
        return fail("icon manifest exceeds 256 KiB".to_owned());
    }
    let mut table: toml::Table = match toml::from_str(text) {
        Ok(table) => table,
        Err(e) => return fail(format!("invalid icon TOML: {}", e.message())),
    };
    let imported = manifest::provenance_marks_import(&table);
    let appearance = match table.get("appearance") {
        None => None,
        Some(toml::Value::String(s)) if s == "dark" => Some(SystemAppearance::Dark),
        Some(toml::Value::String(s)) if s == "light" => Some(SystemAppearance::Light),
        Some(_) => return fail("appearance must be dark or light".to_owned()),
    };
    let root = match table.get("root") {
        None => false,
        Some(toml::Value::Boolean(root)) => *root,
        Some(_) => return fail("root must be a boolean".to_owned()),
    };
    match (root, table.get("extends")) {
        (true, None) => {}
        (false, Some(toml::Value::String(s))) if s == "builtin:phosphor" => {}
        _ => {
            return fail(
                "root packs omit extends; sparse packs require extends = \"builtin:phosphor\""
                    .to_owned(),
            );
        }
    }
    let Some(toml::Value::Table(icons)) = table.remove("icons") else {
        return fail("icons must be a flat table of quoted semantic ids".to_owned());
    };
    if icons.len() > 512 {
        return fail("at most 512 icon entries".to_owned());
    }
    let entries = entries_of("icons", icons, file, &mut diagnostics);
    // `[small]`: optional artwork for icons drawn at SMALL_MAX_PX or less. Same grammar as
    // `[icons]`; an id without an entry draws its `[icons]` file at every size.
    let small = match table.remove("small") {
        None => BTreeMap::new(),
        Some(toml::Value::Table(small)) => {
            if small.len() > 512 {
                return fail("at most 512 [small] entries".to_owned());
            }
            entries_of("small", small, file, &mut diagnostics)
        }
        Some(_) => {
            return fail("small must be a flat table of quoted semantic ids".to_owned());
        }
    };
    if root && entries.len() != IconId::COUNT {
        return fail("root icon pack must cover every registered id".to_owned());
    }
    // Run exactly TH-1's shared metadata/version/depth/unknown-key validator. Strip only the
    // icon-specific fields and adapt the immutable base; do not invent a second metadata policy.
    table.retain(|key, _| {
        let known = matches!(
            key,
            "schema_version"
                | "id"
                | "name"
                | "authors"
                | "license"
                | "license_file"
                | "version"
                | "description"
                | "repository"
                | "appearance"
                | "root"
                | "extends"
                | "provenance"
        );
        if !known {
            diagnostics.push(SettingsError::warning(
                file,
                key,
                "unknown icon manifest key ignored",
            ));
        }
        known
    });
    table.insert("appearance".into(), toml::Value::String("dark".into()));
    table.insert("root".into(), toml::Value::Boolean(false));
    table.insert(
        "extends".into(),
        toml::Value::String("builtin:hutan-dark".into()),
    );
    let parsed = manifest::parse(&toml::to_string(&table).unwrap_or_default(), file);
    diagnostics.extend(parsed.diagnostics);
    let declaration = parsed.declaration.map(|d| {
        let mut meta = d.meta;
        meta.root = root;
        meta.extends = None;
        Declaration {
            meta,
            appearance,
            icons: entries,
            small,
            source_hash: manifest::hash_text(text),
            imported,
        }
    });
    // Per-id errors may fall back; structural errors from the shared parser reject the pack.
    if diagnostics.iter().any(|d| {
        d.severity == Severity::Error
            && !(d.key_path.starts_with("icons.") || d.key_path.starts_with("small."))
    }) {
        return (None, diagnostics);
    }
    (declaration, diagnostics)
}

/// The ids of one table (`[icons]` or `[small]`, named by `table`) with their relative SVG paths.
/// A bad entry is a diagnostic under `<table>.<id>` and an empty path (the id falls back).
fn entries_of(
    table: &str,
    values: toml::Table,
    file: Option<&Path>,
    diagnostics: &mut Vec<SettingsError>,
) -> BTreeMap<IconId, String> {
    let mut entries = BTreeMap::new();
    for (key, value) in values {
        let key_path = format!("{table}.{key}");
        if !valid_token_id(&key) {
            diagnostics.push(SettingsError::error(
                file,
                key_path,
                "invalid semantic id grammar",
            ));
            continue;
        }
        let Some(id) = IconId::from_id(&key) else {
            diagnostics.push(SettingsError::warning(
                file,
                key_path,
                if key == "brand.menu-bar" {
                    "native menu-bar identity is not themeable"
                } else {
                    "unknown icon id ignored"
                },
            ));
            continue;
        };
        match value.as_str() {
            Some(path) => {
                entries.insert(id, path.to_owned());
            }
            None => {
                entries.insert(id, String::new());
                diagnostics.push(SettingsError::error(
                    file,
                    key_path,
                    "expected a relative SVG path; kept last-good or default icon for this id",
                ));
            }
        }
    }
    entries
}
