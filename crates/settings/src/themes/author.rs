//! Authoring helpers for color and icon packs, shared by `hutan theme ls|validate|new|export`
//! and the Settings pickers (ADR 0018, `docs/themes.md`): listings, validation by path or id,
//! scaffolds, exports and duplicates. Pure file work on the user's pack folders: no GPUI, no
//! daemon, no network.
//!
//! Every pack is judged by the loaders the desktop uses (`load_pack`, the icon catalog's folder
//! loader), so the CLI and Settings never disagree. Every write creates a new folder and
//! refuses to replace one: files are staged in a hidden sibling and renamed into place with
//! `RENAME_NOREPLACE`, so a reader (or the hot-reload watcher) sees a whole pack or none.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::catalog::{
    self, OverrideMemory, PackSource, SelectionRequest, ThemeCatalogSnapshot, builtin_source,
};
use super::icons::{self, IconCatalog, IconId};
use super::manifest::{self, Builtin, appearance_name};
use super::resolve::{Provenance, ResolvedTheme};
use super::tokens::{Color, DensityKey, FontRole, Metric, valid_pack_id};
use super::{AUTO, ThemeRegistry};
use crate::appearance::{Density, ThemeOverrides};
use crate::error::SettingsError;
use crate::themes::{SystemAppearance, confine, svg};

/// What a command did not manage to do: `Usage` is a wrong invocation (exit status 2 of the
/// CLI), `Failed` a problem with the files (status 1).
#[derive(Debug, thiserror::Error)]
pub enum AuthorError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Failed(String),
}

fn failed(message: impl Into<String>) -> AuthorError {
    AuthorError::Failed(message.into())
}

fn usage(message: impl Into<String>) -> AuthorError {
    AuthorError::Usage(message.into())
}

/// The two kinds of pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackKind {
    Color,
    Icon,
}

impl PackKind {
    /// `color` or `icon`, as `--kind` spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Icon => "icon",
        }
    }

    /// The folder of the data folder that holds packs of this kind.
    pub const fn folder(self) -> &'static str {
        match self {
            Self::Color => "themes",
            Self::Icon => "icon-themes",
        }
    }

    /// The manifest file of a pack folder.
    pub const fn manifest(self) -> &'static str {
        match self {
            Self::Color => "theme.toml",
            Self::Icon => "icon-theme.toml",
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Color => Self::Icon,
            Self::Icon => Self::Color,
        }
    }
}

/// Where a listed pack comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Embedded in Hutan.
    Embedded,
    /// A pack of the user's folder.
    User,
    /// A pack of the user's folder that takes the place of an embedded one of the same id.
    UserOverride,
    /// A pack of the user's folder converted from another tool's file (`[provenance]` names a
    /// `source_format`).
    Imported,
}

impl Origin {
    /// `embedded`, `user`, `user-override` or `imported`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::User => "user",
            Self::UserOverride => "user-override",
            Self::Imported => "imported",
        }
    }
}

/// One pack as a picker or `hutan theme ls` shows it: valid, or disabled with its errors.
#[derive(Debug, Clone)]
pub struct PackInfo {
    pub kind: PackKind,
    /// The pack's id; `None` for a pack whose manifest never got far enough to have one.
    pub id: Option<String>,
    /// The display name; the folder or file name for a pack that has none.
    pub name: String,
    /// The polarity a color pack fills, or an icon pack declares (`None`: either).
    pub appearance: Option<SystemAppearance>,
    pub authors: Vec<String>,
    pub license: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub origin: Origin,
    /// The manifest file; `None` for an embedded pack.
    pub manifest: Option<PathBuf>,
    /// `false`: the pack cannot be selected; `diagnostics` say why.
    pub valid: bool,
    /// The file has an invalid edit and this is the last good version.
    pub kept_previous: bool,
    /// How many semantic ids an icon pack replaces; `None` for a color pack.
    pub icons: Option<usize>,
    /// What is wrong with this pack, errors and warnings.
    pub diagnostics: Vec<SettingsError>,
}

impl PackInfo {
    pub fn errors(&self) -> impl Iterator<Item = &SettingsError> {
        self.diagnostics.iter().filter(|d| d.is_error())
    }

    pub fn warnings(&self) -> impl Iterator<Item = &SettingsError> {
        self.diagnostics.iter().filter(|d| !d.is_error())
    }
}

fn diagnostics_for(all: &[SettingsError], manifest: Option<&Path>) -> Vec<SettingsError> {
    match manifest {
        Some(path) => all
            .iter()
            .filter(|d| d.file.as_deref() == Some(path))
            .cloned()
            .collect(),
        None => Vec::new(),
    }
}

/// The label of a pack that has no name: its folder, or its file without `.toml`.
fn label_of(manifest: &Path) -> String {
    let file = manifest.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let named = if file == "theme.toml" || file == "icon-theme.toml" {
        manifest.parent().and_then(Path::file_name)
    } else {
        manifest.file_stem()
    };
    named
        .and_then(|n| n.to_str())
        .unwrap_or("(unnamed)")
        .to_owned()
}

/// Every color pack of `snapshot`: valid ones in catalog order (built-ins first), then the
/// disabled ones by path.
pub fn list_colors(snapshot: &ThemeCatalogSnapshot) -> Vec<PackInfo> {
    let mut out = Vec::new();
    for pack in snapshot.packs() {
        let d = &pack.descriptor;
        let (origin, manifest) = match &d.source {
            PackSource::Builtin => (Origin::Embedded, None),
            PackSource::User(path) => (
                if d.overrides_builtin {
                    Origin::UserOverride
                } else if d.imported {
                    Origin::Imported
                } else {
                    Origin::User
                },
                Some(path.clone()),
            ),
        };
        out.push(PackInfo {
            kind: PackKind::Color,
            id: Some(d.id.clone()),
            name: d.name.clone(),
            appearance: Some(d.appearance),
            authors: d.authors.clone(),
            license: d.license.clone(),
            version: d.version.clone(),
            description: d.description.clone(),
            origin,
            diagnostics: diagnostics_for(snapshot.diagnostics(), manifest.as_deref()),
            manifest,
            valid: true,
            kept_previous: d.kept_previous,
            icons: None,
        });
    }
    for disabled in snapshot.disabled() {
        out.push(PackInfo {
            kind: PackKind::Color,
            id: disabled.id.clone(),
            name: label_of(&disabled.path),
            appearance: None,
            authors: Vec::new(),
            license: String::new(),
            version: None,
            description: None,
            origin: Origin::User,
            manifest: Some(disabled.path.clone()),
            valid: false,
            kept_previous: false,
            icons: None,
            diagnostics: diagnostics_for(snapshot.diagnostics(), Some(&disabled.path)),
        });
    }
    out
}

/// Every icon pack of `catalog`, in the same order as [`list_colors`].
pub fn list_icons(catalog: &IconCatalog) -> Vec<PackInfo> {
    let mut out = Vec::new();
    for pack in &catalog.packs {
        let meta = &pack.declaration.meta;
        let (origin, manifest) = match &pack.source {
            PackSource::Builtin => (Origin::Embedded, None),
            PackSource::User(path) => (
                if meta.id == icons::DEFAULT {
                    Origin::UserOverride
                } else if pack.declaration.imported {
                    Origin::Imported
                } else {
                    Origin::User
                },
                Some(path.clone()),
            ),
        };
        out.push(PackInfo {
            kind: PackKind::Icon,
            id: Some(meta.id.clone()),
            name: meta.name.clone(),
            appearance: pack.declaration.appearance,
            authors: meta.authors.clone(),
            license: meta.license.clone(),
            version: meta.version.clone(),
            description: meta.description.clone(),
            origin,
            diagnostics: diagnostics_for(&catalog.diagnostics, manifest.as_deref()),
            manifest,
            valid: true,
            kept_previous: pack.kept_previous,
            icons: Some(pack.declaration.icons.len()),
        });
    }
    for disabled in &catalog.disabled {
        out.push(PackInfo {
            kind: PackKind::Icon,
            id: disabled.id.clone(),
            name: label_of(&disabled.path),
            appearance: None,
            authors: Vec::new(),
            license: String::new(),
            version: None,
            description: None,
            origin: Origin::User,
            manifest: Some(disabled.path.clone()),
            valid: false,
            kept_previous: false,
            icons: None,
            diagnostics: diagnostics_for(&catalog.diagnostics, Some(&disabled.path)),
        });
    }
    out
}

impl ThemeRegistry {
    /// The packs of `kind` as the catalog holds them right now, and every diagnostic of that
    /// catalog (including the ones no listed pack owns, such as a refused symlink).
    pub fn listing(&self, kind: PackKind) -> (Vec<PackInfo>, Vec<SettingsError>) {
        match kind {
            PackKind::Color => {
                let snapshot = self.snapshot();
                (list_colors(&snapshot), snapshot.diagnostics().to_vec())
            }
            PackKind::Icon => {
                let catalog = self.icon_snapshot();
                (list_icons(&catalog), catalog.diagnostics.clone())
            }
        }
    }
}

// ---- validation ------------------------------------------------------------------------------

/// The verdict on one pack.
#[derive(Debug, Clone)]
pub struct Validation {
    pub kind: PackKind,
    /// What was asked for: the path as given, or the id.
    pub target: String,
    pub id: Option<String>,
    pub manifest: Option<PathBuf>,
    pub diagnostics: Vec<SettingsError>,
}

impl Validation {
    pub fn errors(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.is_error()).count()
    }

    pub fn warnings(&self) -> usize {
        self.diagnostics.len() - self.errors()
    }
}

/// Whether `target` names a place on disk rather than an installed id: it has a `/`, starts
/// with `.` or `~`, or ends in `.toml`. (An installed id wins over a folder of the same name in
/// the current directory; write `./name` to mean the folder.)
pub fn looks_like_path(target: &str) -> bool {
    target.contains('/')
        || target.starts_with('.')
        || target.starts_with('~')
        || target.ends_with(".toml")
}

/// Validates the pack at `path` (a pack folder, or a single-file color pack) the way the
/// desktop loads it.
pub fn validate_path(kind: PackKind, path: &Path) -> Result<Validation, AuthorError> {
    let meta =
        fs::metadata(path).map_err(|e| failed(format!("cannot read {}: {e}", path.display())))?;
    let target = path.display().to_string();
    match kind {
        PackKind::Color => {
            let (root, rel, pack_dir) = if meta.is_dir() {
                let root = fs::canonicalize(path)
                    .map_err(|e| failed(format!("cannot read {}: {e}", path.display())))?;
                if !root.join("theme.toml").exists() && root.join("icon-theme.toml").exists() {
                    return Err(failed(format!(
                        "{} holds an icon pack (icon-theme.toml); use --kind icon",
                        path.display()
                    )));
                }
                (root.clone(), "theme.toml".to_owned(), Some(root))
            } else {
                let file = fs::canonicalize(path)
                    .map_err(|e| failed(format!("cannot read {}: {e}", path.display())))?;
                let name = file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| failed("the file name is not UTF-8"))?
                    .to_owned();
                let root = file
                    .parent()
                    .ok_or_else(|| failed("the file has no folder"))?
                    .to_path_buf();
                (root, name, None)
            };
            let manifest_path = root.join(&rel);
            let text = confine::read_text(&root, &rel, manifest::MAX_MANIFEST_BYTES as u64)
                .map_err(|why| failed(format!("{}: {why}", manifest_path.display())))?;
            let (pack, diagnostics) =
                catalog::load_pack(&text, &manifest_path, pack_dir.as_deref());
            Ok(Validation {
                kind,
                target,
                id: pack.map(|p| p.descriptor.id.clone()),
                manifest: Some(manifest_path),
                diagnostics,
            })
        }
        PackKind::Icon => {
            if !meta.is_dir() {
                return Err(failed(format!(
                    "{} is not a folder: an icon pack is a folder with an icon-theme.toml",
                    path.display()
                )));
            }
            if !path.join("icon-theme.toml").exists() && path.join("theme.toml").exists() {
                return Err(failed(format!(
                    "{} holds a color pack (theme.toml); use --kind color",
                    path.display()
                )));
            }
            let verdict = icons::validate_dir(path).map_err(failed)?;
            Ok(Validation {
                kind,
                target,
                id: verdict.declaration.map(|d| d.meta.id),
                manifest: Some(path.join("icon-theme.toml")),
                diagnostics: verdict.diagnostics,
            })
        }
    }
}

/// Validates the installed pack `id` of the data folder (an embedded one is valid by
/// construction): what the desktop would say about it, duplicate ids included.
pub fn validate_installed(
    kind: PackKind,
    registry: &ThemeRegistry,
    id: &str,
) -> Result<Validation, AuthorError> {
    let (packs, _) = registry.listing(kind);
    let found = packs
        .iter()
        .rev()
        .find(|p| p.id.as_deref() == Some(id))
        // A pack that never loaded has no id: it is found by its folder or file name.
        .or_else(|| packs.iter().find(|p| !p.valid && p.name == id));
    let Some(pack) = found else {
        let hint = if registry
            .listing(kind.other())
            .0
            .iter()
            .any(|p| p.id.as_deref() == Some(id))
        {
            format!(
                "; there is a {} pack with that id: use --kind {}",
                kind.other().name(),
                kind.other().name()
            )
        } else {
            "; `hutan theme ls` lists the installed ones".to_owned()
        };
        return Err(failed(format!("no {} theme `{id}`{hint}", kind.name())));
    };
    let diagnostics = pack.diagnostics.clone();
    Ok(Validation {
        kind,
        target: id.to_owned(),
        id: pack.id.clone(),
        manifest: pack.manifest.clone(),
        diagnostics,
    })
}

// ---- staged, no-clobber writes ------------------------------------------------------------------

static STAGING: AtomicU64 = AtomicU64::new(0);

/// One file of a pack being written: a path relative to the pack folder and its bytes.
type PackFile = (String, Vec<u8>);

/// Creates `<parent>/<name>/` holding `files`. Nothing is visible until every file is written;
/// an existing `<parent>/<name>` is never touched.
fn write_pack_dir(parent: &Path, name: &str, files: &[PackFile]) -> Result<PathBuf, AuthorError> {
    fs::create_dir_all(parent)
        .map_err(|e| failed(format!("cannot create {}: {e}", parent.display())))?;
    let parent = fs::canonicalize(parent)
        .map_err(|e| failed(format!("cannot resolve {}: {e}", parent.display())))?;
    let target = parent.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(failed(format!(
            "{} already exists; nothing was written (remove it or choose another id or --output)",
            target.display()
        )));
    }
    let staging = parent.join(format!(
        ".{name}.hutan-new-{}-{}",
        std::process::id(),
        STAGING.fetch_add(1, Ordering::Relaxed)
    ));
    let written = stage(&staging, files).and_then(|()| {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &staging,
            rustix::fs::CWD,
            &target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|e| {
            if e == rustix::io::Errno::EXIST || e == rustix::io::Errno::NOTEMPTY {
                format!(
                    "{} already exists; nothing was written (remove it or choose another id or --output)",
                    target.display()
                )
            } else {
                format!("cannot move the new pack into {}: {e}", target.display())
            }
        })
    });
    match written {
        Ok(()) => Ok(target),
        Err(message) => {
            let _ = fs::remove_dir_all(&staging);
            Err(failed(message))
        }
    }
}

fn stage(staging: &Path, files: &[PackFile]) -> Result<(), String> {
    fs::create_dir(staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
    for (rel, bytes) in files {
        confine::check_relative(rel).map_err(|why| format!("{rel}: {why}"))?;
        let path = staging.join(rel);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)
                .map_err(|e| format!("cannot create {}: {e}", folder.display()))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// A TOML basic string.
fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn string_array(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|i| quoted(i)).collect();
    format!("[{}]", items.join(", "))
}

// ---- new --------------------------------------------------------------------------------------------

/// What `hutan theme new` is asked for.
#[derive(Debug, Clone, Copy)]
pub struct NewPack<'a> {
    pub kind: PackKind,
    pub id: &'a str,
    pub name: &'a str,
    pub author: &'a str,
    pub license: &'a str,
    /// `builtin:hutan-dark`, `builtin:hutan-light` or `builtin:phosphor`; the default of the kind
    /// when `None`.
    pub extends: Option<&'a str>,
}

/// A pack that was written.
#[derive(Debug, Clone)]
pub struct Written {
    /// The new pack folder.
    pub dir: PathBuf,
    /// The manifest to edit.
    pub manifest: PathBuf,
    /// Every file of the pack, relative to `dir`.
    pub files: Vec<String>,
    /// What the user should know (flattening, notices).
    pub notes: Vec<String>,
}

/// An example icon that passes the sanitizer: a close cross.
const EXAMPLE_ICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.75\" stroke-linecap=\"round\">\n  <path d=\"M6 6 18 18M18 6 6 18\"/>\n</svg>\n";

fn check_id(id: &str) -> Result<(), AuthorError> {
    if id == AUTO {
        return Err(usage("`auto` is reserved for the terminal.theme selector"));
    }
    if !valid_pack_id(id) {
        return Err(usage(format!(
            "`{id}` is not a pack id: use lower-case words joined by `-` (1-64 characters, e.g. `canopy-night`)"
        )));
    }
    Ok(())
}

/// Writes a complete sparse pack `<parent>/<id>/` with an explicit `extends` and real metadata;
/// an icon pack also gets one safe example SVG. The manifest is checked by the loader before
/// anything is written, and an existing folder is never replaced.
pub fn new_pack(spec: &NewPack<'_>, parent: &Path) -> Result<Written, AuthorError> {
    check_id(spec.id)?;
    let (manifest_text, mut files) = match spec.kind {
        PackKind::Color => {
            let base = match spec.extends {
                None => Builtin::HutanDark,
                Some(reference) => Builtin::from_reference(reference).ok_or_else(|| {
                    usage(format!(
                        "unknown base `{reference}` for a color theme; use builtin:hutan-dark or builtin:hutan-light"
                    ))
                })?,
            };
            let accent = catalog::builtin_theme(base).color(Color::Accent);
            let mut text = String::new();
            let _ = writeln!(
                text,
                "# {}: a color theme for Hutan (docs/themes.md).",
                spec.name.replace('\n', " ")
            );
            let _ = writeln!(text, "schema_version = 1");
            let _ = writeln!(text, "id = {}", quoted(spec.id));
            let _ = writeln!(text, "name = {}", quoted(spec.name));
            let _ = writeln!(text, "authors = [{}]", quoted(spec.author));
            let _ = writeln!(text, "license = {}", quoted(spec.license));
            let _ = writeln!(text, "version = \"0.1.0\"");
            let _ = writeln!(
                text,
                "description = {}",
                quoted(&format!(
                    "A color theme based on {}.",
                    catalog::builtin_theme(base).name
                ))
            );
            let _ = writeln!(
                text,
                "appearance = \"{}\"",
                appearance_name(base.appearance())
            );
            let _ = writeln!(text, "extends = {}", quoted(&base.reference()));
            let _ = writeln!(
                text,
                "\n# Only what differs from the base is written here; everything else is inherited.\n# Every token is listed in docs/themes.md.\n[colors]\n\"ui.accent\" = {}",
                quoted(&accent.to_string())
            );
            (text, Vec::new())
        }
        PackKind::Icon => {
            if let Some(reference) = spec.extends
                && reference != "builtin:phosphor"
            {
                return Err(usage(format!(
                    "unknown base `{reference}` for an icon theme; use builtin:phosphor"
                )));
            }
            let mut text = String::new();
            let _ = writeln!(
                text,
                "# {}: an icon theme for Hutan (docs/themes.md).",
                spec.name.replace('\n', " ")
            );
            let _ = writeln!(text, "schema_version = 1");
            let _ = writeln!(text, "id = {}", quoted(spec.id));
            let _ = writeln!(text, "name = {}", quoted(spec.name));
            let _ = writeln!(text, "authors = [{}]", quoted(spec.author));
            let _ = writeln!(text, "license = {}", quoted(spec.license));
            let _ = writeln!(text, "version = \"0.1.0\"");
            let _ = writeln!(
                text,
                "description = \"An icon theme based on Phosphor; only the icons listed below are replaced.\""
            );
            let _ = writeln!(text, "extends = \"builtin:phosphor\"");
            let _ = writeln!(
                text,
                "\n# Semantic ids -> SVG files of this folder; every other icon is Phosphor's.\n# The ids are listed in docs/themes.md.\n[icons]\n\"action.close\" = \"icons/close.svg\""
            );
            (
                text,
                vec![(
                    "icons/close.svg".to_owned(),
                    EXAMPLE_ICON.as_bytes().to_vec(),
                )],
            )
        }
    };
    check_new_manifest(spec.kind, &manifest_text)?;
    files.insert(
        0,
        (spec.kind.manifest().to_owned(), manifest_text.into_bytes()),
    );
    let dir = write_pack_dir(parent, spec.id, &files)?;
    Ok(Written {
        manifest: dir.join(spec.kind.manifest()),
        files: files.into_iter().map(|(rel, _)| rel).collect(),
        dir,
        notes: Vec::new(),
    })
}

/// A scaffold with bad metadata (a name with a control character, a 300-character author) is an
/// invalid invocation, reported with the loader's own words.
fn check_new_manifest(kind: PackKind, text: &str) -> Result<(), AuthorError> {
    let diagnostics = match kind {
        PackKind::Color => catalog::load_pack(text, Path::new(kind.manifest()), None).1,
        PackKind::Icon => icons::parse(text, Some(Path::new(kind.manifest()))).1,
    };
    match diagnostics.iter().find(|d| d.is_error()) {
        Some(error) => Err(usage(format!("{}: {}", error.key_path, error.message))),
        None => Ok(()),
    }
}

// ---- export -----------------------------------------------------------------------------------------

/// What `hutan theme export` (and "Duplicate as new pack") is asked for.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions<'a> {
    /// Write a self-contained `root = true` pack with every fallback flattened in.
    pub resolved: bool,
    /// The id and display name of the written pack when they differ from the source's.
    pub rename: Option<(&'a str, &'a str)>,
    /// The user's `appearance.theme_overrides`; a resolved color export lays the pack's own
    /// overrides over it, so the file is what Settings shows.
    pub overrides: Option<&'a BTreeMap<String, ThemeOverrides>>,
}

/// Exports the installed pack `id` into `<parent>/<id or new id>/`.
pub fn export(
    kind: PackKind,
    registry: &ThemeRegistry,
    id: &str,
    parent: &Path,
    options: &ExportOptions<'_>,
) -> Result<Written, AuthorError> {
    if let Some((new_id, _)) = options.rename {
        check_id(new_id)?;
    }
    let (files, name, notes) = match kind {
        PackKind::Color => export_color(registry, id, options)?,
        PackKind::Icon => export_icon(registry, id, options)?,
    };
    let dir = write_pack_dir(parent, name.as_str(), &files)?;
    Ok(Written {
        manifest: dir.join(kind.manifest()),
        files: files.into_iter().map(|(rel, _)| rel).collect(),
        dir,
        notes,
    })
}

type Exported = (Vec<PackFile>, String, Vec<String>);

/// A pack that is installed but cannot be exported as it is.
fn not_exportable(kind: PackKind, id: &str, registry: &ThemeRegistry) -> AuthorError {
    let invalid = registry
        .listing(kind)
        .0
        .iter()
        .any(|p| !p.valid && (p.id.as_deref() == Some(id) || p.name == id));
    if invalid {
        failed(format!(
            "{} theme `{id}` is invalid; fix it first (`hutan theme validate {id} --kind {}`)",
            kind.name(),
            kind.name()
        ))
    } else {
        failed(format!(
            "no {} theme `{id}`; `hutan theme ls --kind {}` lists the installed ones",
            kind.name(),
            kind.name()
        ))
    }
}

/// Sets the pack's `id` and `name` in manifest text, keeping its comments and layout.
fn rename_manifest(text: &str, id: &str, name: &str) -> Result<String, AuthorError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| failed(format!("the manifest is not valid TOML: {e}")))?;
    doc["id"] = toml_edit::value(id);
    doc["name"] = toml_edit::value(name);
    Ok(doc.to_string())
}

/// Reads `rel` of the pack folder that holds `manifest`, confined to it.
fn read_pack_file(manifest: &Path, rel: &str, max: u64) -> Result<Vec<u8>, AuthorError> {
    let root = manifest
        .parent()
        .ok_or_else(|| failed("the manifest has no folder"))?;
    confine::read_confined(root, rel, max)
        .map_err(|why| failed(format!("{}: {why}", root.join(rel).display())))
}

/// Reads the manifest file itself.
fn read_manifest(manifest: &Path) -> Result<String, AuthorError> {
    let name = manifest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| failed("the manifest name is not UTF-8"))?;
    let bytes = read_pack_file(manifest, name, manifest::MAX_MANIFEST_BYTES as u64)?;
    String::from_utf8(bytes).map_err(|_| failed(format!("{}: not valid UTF-8", manifest.display())))
}

fn export_color(
    registry: &ThemeRegistry,
    id: &str,
    options: &ExportOptions<'_>,
) -> Result<Exported, AuthorError> {
    let snapshot = registry.snapshot();
    let pack = snapshot
        .pack(id)
        .ok_or_else(|| not_exportable(PackKind::Color, id, registry))?;
    let declaration = pack.declaration();
    let (text, license_file): (String, Option<(String, Vec<u8>)>) = match &pack.descriptor.source {
        PackSource::Builtin => {
            let builtin = Builtin::ALL
                .into_iter()
                .find(|b| b.id() == id)
                .ok_or_else(|| failed("an embedded pack without a manifest"))?;
            (builtin_source(builtin).to_owned(), None)
        }
        PackSource::User(manifest) => {
            let text = read_manifest(manifest)?;
            let license = match &declaration.meta.license_file {
                // A single-file pack has no files next to it; the loader already warned.
                Some(rel) if manifest.file_name().is_some_and(|n| n == "theme.toml") => {
                    Some((rel.clone(), read_pack_file(manifest, rel, 256 * 1024)?))
                }
                _ => None,
            };
            (text, license)
        }
    };
    let mut notes = Vec::new();
    let (new_id, new_name) = options
        .rename
        .unwrap_or((declaration.meta.id.as_str(), declaration.meta.name.as_str()));
    let manifest_text = if options.resolved {
        let mut resolved = std::sync::Arc::clone(pack.resolved());
        if let Some(all) = options.overrides
            && let Some(own) = all.get(id).filter(|o| !o.colors.is_empty())
        {
            // The same path the desktop takes, so the file is what Settings shows.
            let selected = snapshot.select(
                SelectionRequest {
                    polarity: declaration.meta.appearance,
                    light_theme: id,
                    dark_theme: id,
                    overrides: all,
                },
                &mut OverrideMemory::default(),
            );
            if let Some(error) = selected.diagnostics.iter().find(|d| d.is_error()) {
                return Err(failed(format!(
                    "the overrides of `{id}` in settings.toml are invalid ({}: {}); fix them or export without --resolved",
                    error.key_path, error.message
                )));
            }
            resolved = selected.theme;
            notes.push(format!(
                "applied the {} color override(s) of `{id}` from settings.toml",
                own.colors.len()
            ));
        }
        flatten_color(&resolved, &declaration.meta, &text, new_id, new_name)?
    } else if options.rename.is_some() {
        rename_manifest(&text, new_id, new_name)?
    } else {
        text
    };
    let mut files = vec![("theme.toml".to_owned(), manifest_text.into_bytes())];
    if let Some((rel, bytes)) = license_file {
        files.push((rel, bytes));
    }
    Ok((files, new_id.to_owned(), notes))
}

/// A self-contained `root = true` manifest of `theme`: every required token, both density
/// tables, every metric and font role, plus the optional colors that were declared (the ones
/// that were derived stay derived, so the file still follows the colors it derives from). The
/// source's `[provenance]` is kept and the flattening recorded.
fn flatten_color(
    theme: &ResolvedTheme,
    meta: &manifest::Meta,
    source_text: &str,
    id: &str,
    name: &str,
) -> Result<String, AuthorError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {name}: exported from `{}` with every fallback written out (hutan theme export --resolved).",
        meta.id
    );
    let _ = writeln!(out, "schema_version = 1");
    let _ = writeln!(out, "id = {}", quoted(id));
    let _ = writeln!(out, "name = {}", quoted(name));
    let _ = writeln!(out, "authors = {}", string_array(&meta.authors));
    let _ = writeln!(out, "license = {}", quoted(&meta.license));
    if let Some(file) = &meta.license_file {
        let _ = writeln!(out, "license_file = {}", quoted(file));
    }
    if let Some(version) = &meta.version {
        let _ = writeln!(out, "version = {}", quoted(version));
    }
    if let Some(description) = &meta.description {
        let _ = writeln!(out, "description = {}", quoted(description));
    }
    if let Some(repository) = &meta.repository {
        let _ = writeln!(out, "repository = {}", quoted(repository));
    }
    let _ = writeln!(out, "appearance = \"{}\"", appearance_name(meta.appearance));
    let _ = writeln!(out, "root = true");

    let _ = writeln!(out, "\n[colors]");
    for token in Color::ALL {
        let write = token.is_required() || theme.provenance(*token) == Provenance::Declared;
        if write {
            let _ = writeln!(
                out,
                "{} = {}",
                quoted(token.id()),
                quoted(&theme.color(*token).to_string())
            );
        }
    }
    let _ = writeln!(out, "\n[fonts]");
    for role in FontRole::ALL {
        let _ = writeln!(
            out,
            "{} = {}",
            role.id(),
            string_array(theme.fonts().role(role))
        );
    }
    for (table, density) in [
        ("compact", Density::Compact),
        ("comfortable", Density::Comfortable),
    ] {
        let _ = writeln!(out, "\n[density.{table}]");
        for key in DensityKey::ALL {
            let _ = writeln!(
                out,
                "{} = {}",
                quoted(key.id()),
                theme.density(density).get(*key)
            );
        }
    }
    let _ = writeln!(out, "\n[metrics]");
    for metric in Metric::ALL {
        let _ = writeln!(out, "{} = {}", quoted(metric.id()), theme.metric(*metric));
    }

    // `[provenance]` is inert metadata: what the source recorded stays, and the flattening is
    // added to it.
    let mut provenance = toml::Table::new();
    if let Ok(source) = source_text.parse::<toml::Table>()
        && let Some(toml::Value::Table(existing)) = source.get("provenance")
    {
        provenance = existing.clone();
    }
    provenance
        .entry("exported_from")
        .or_insert_with(|| toml::Value::String(meta.id.clone()));
    if let Some(base) = meta.extends {
        provenance
            .entry("flattened_from_base")
            .or_insert_with(|| toml::Value::String(base.reference()));
    }
    let mut wrapper = toml::Table::new();
    wrapper.insert("provenance".to_owned(), toml::Value::Table(provenance));
    let rendered = toml::to_string(&wrapper)
        .map_err(|e| failed(format!("cannot write the provenance table: {e}")))?;
    let _ = write!(out, "\n{rendered}");
    Ok(out)
}

fn export_icon(
    registry: &ThemeRegistry,
    id: &str,
    options: &ExportOptions<'_>,
) -> Result<Exported, AuthorError> {
    let catalog = registry.icon_snapshot();
    let pack = catalog
        .packs
        .iter()
        .rev()
        .find(|p| p.declaration.meta.id == id)
        .ok_or_else(|| not_exportable(PackKind::Icon, id, registry))?;
    let meta = &pack.declaration.meta;
    let (new_id, new_name) = options
        .rename
        .unwrap_or((meta.id.as_str(), meta.name.as_str()));
    let mut notes = Vec::new();
    let mut files: Vec<PackFile> = Vec::new();

    // Own assets of the pack, by the path the manifest names (several ids may share one file).
    let mut assets: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let manifest_text = match &pack.source {
        PackSource::Builtin => {
            for rel in pack
                .declaration
                .icons
                .values()
                .chain(pack.declaration.small.values())
            {
                if !assets.contains_key(rel) {
                    let bytes = icons::builtin_asset(rel)
                        .ok_or_else(|| failed(format!("embedded artwork `{rel}` is missing")))?;
                    assets.insert(rel.clone(), bytes.to_vec());
                }
            }
            icons::builtin_manifest().to_owned()
        }
        PackSource::User(manifest) => {
            for rel in pack
                .declaration
                .icons
                .values()
                .chain(pack.declaration.small.values())
            {
                if !assets.contains_key(rel) {
                    assets.insert(
                        rel.clone(),
                        read_pack_file(manifest, rel, svg::MAX_SVG_BYTES as u64)?,
                    );
                }
            }
            read_manifest(manifest)?
        }
    };
    let own_license = match (&pack.source, &meta.license_file) {
        (PackSource::User(manifest), Some(rel)) => {
            Some((rel.clone(), read_pack_file(manifest, rel, 256 * 1024)?))
        }
        (PackSource::Builtin, Some(rel)) => Some((rel.clone(), icons::LICENSE.as_bytes().to_vec())),
        _ => None,
    };

    if options.resolved {
        // Every registered id gets a file of this folder: the pack's own, or the embedded
        // Phosphor artwork under a folder that cannot collide with the pack's paths.
        let mut fallback_dir = "phosphor".to_owned();
        while assets
            .keys()
            .any(|rel| rel.starts_with(&format!("{fallback_dir}/")))
        {
            fallback_dir.push_str("-fallback");
        }
        let embedded = icons::builtin_manifest()
            .parse::<toml::Table>()
            .map_err(|e| failed(format!("the embedded icon manifest: {e}")))?;
        let embedded_icons = embedded
            .get("icons")
            .and_then(toml::Value::as_table)
            .ok_or_else(|| failed("the embedded icon manifest has no [icons]"))?;
        let mut mapping: BTreeMap<&'static str, String> = BTreeMap::new();
        let mut used_fallback = false;
        for icon in IconId::ALL {
            if let Some(rel) = pack.declaration.icons.get(icon) {
                mapping.insert(icon.id(), rel.clone());
                continue;
            }
            let rel = embedded_icons
                .get(icon.id())
                .and_then(toml::Value::as_str)
                .ok_or_else(|| failed(format!("the embedded pack lacks `{}`", icon.id())))?;
            let target = format!("{fallback_dir}/{rel}");
            if !assets.contains_key(&target) {
                let bytes = icons::builtin_asset(rel)
                    .ok_or_else(|| failed(format!("embedded artwork `{rel}` is missing")))?;
                assets.insert(target.clone(), bytes.to_vec());
            }
            mapping.insert(icon.id(), target);
            used_fallback = true;
        }
        // `[small]`: the pack's own entries, plus Phosphor's for the ids the pack does not
        // replace (a replaced id would show Phosphor Bold at 14 px next to the pack's own icon).
        let embedded_small = embedded.get("small").and_then(toml::Value::as_table);
        let mut small_mapping: BTreeMap<&'static str, String> = BTreeMap::new();
        for icon in IconId::ALL {
            if let Some(rel) = pack.declaration.small.get(icon) {
                small_mapping.insert(icon.id(), rel.clone());
            } else if !pack.declaration.icons.contains_key(icon)
                && !pack.declaration.meta.root
                && let Some(rel) = embedded_small
                    .and_then(|t| t.get(icon.id()))
                    .and_then(toml::Value::as_str)
            {
                let target = format!("{fallback_dir}/{rel}");
                if !assets.contains_key(&target) {
                    let bytes = icons::builtin_asset(rel)
                        .ok_or_else(|| failed(format!("embedded artwork `{rel}` is missing")))?;
                    assets.insert(target.clone(), bytes.to_vec());
                }
                small_mapping.insert(icon.id(), target);
                used_fallback = true;
            }
        }
        let mut text = String::new();
        let _ = writeln!(
            text,
            "# {new_name}: exported from `{}` with every icon written out (hutan theme export --resolved).",
            meta.id
        );
        let _ = writeln!(text, "schema_version = 1");
        let _ = writeln!(text, "id = {}", quoted(new_id));
        let _ = writeln!(text, "name = {}", quoted(new_name));
        let _ = writeln!(text, "authors = {}", string_array(&meta.authors));
        let license = if used_fallback && !license_includes_mit(&meta.license) {
            format!("({}) AND MIT", meta.license)
        } else {
            meta.license.clone()
        };
        let _ = writeln!(text, "license = {}", quoted(&license));
        let license_file = match (&own_license, used_fallback) {
            (Some((rel, _)), _) => Some(rel.clone()),
            (None, true) => Some("LICENSE-phosphor".to_owned()),
            (None, false) => None,
        };
        if let Some(rel) = &license_file {
            let _ = writeln!(text, "license_file = {}", quoted(rel));
        }
        if let Some(version) = &meta.version {
            let _ = writeln!(text, "version = {}", quoted(version));
        }
        if let Some(description) = &meta.description {
            let _ = writeln!(text, "description = {}", quoted(description));
        }
        if let Some(repository) = &meta.repository {
            let _ = writeln!(text, "repository = {}", quoted(repository));
        }
        if let Some(appearance) = pack.declaration.appearance {
            let _ = writeln!(text, "appearance = \"{}\"", appearance_name(appearance));
        }
        let _ = writeln!(text, "root = true\n\n[icons]");
        for (icon, rel) in &mapping {
            let _ = writeln!(text, "{} = {}", quoted(icon), quoted(rel));
        }
        if !small_mapping.is_empty() {
            let _ = writeln!(text, "\n[small]");
            for (icon, rel) in &small_mapping {
                let _ = writeln!(text, "{} = {}", quoted(icon), quoted(rel));
            }
        }
        files.push(("icon-theme.toml".to_owned(), text.into_bytes()));
        if used_fallback {
            notes.push(
                "the Phosphor artwork the pack falls back to is included, with its MIT notice in LICENSE-phosphor and PROVENANCE-phosphor"
                    .to_owned(),
            );
            files.push((
                "LICENSE-phosphor".to_owned(),
                icons::LICENSE.as_bytes().to_vec(),
            ));
            files.push((
                "PROVENANCE-phosphor".to_owned(),
                icons::PROVENANCE.as_bytes().to_vec(),
            ));
        }
    } else {
        let text = if options.rename.is_some() {
            rename_manifest(&manifest_text, new_id, new_name)?
        } else {
            manifest_text
        };
        files.push(("icon-theme.toml".to_owned(), text.into_bytes()));
        if matches!(pack.source, PackSource::Builtin) {
            files.push((
                "PROVENANCE".to_owned(),
                icons::PROVENANCE.as_bytes().to_vec(),
            ));
        }
    }
    if let Some((rel, bytes)) = own_license
        && !files.iter().any(|(existing, _)| *existing == rel)
    {
        files.push((rel, bytes));
    }
    let mut seen = BTreeSet::new();
    for (rel, bytes) in assets {
        if seen.insert(rel.clone()) {
            files.push((rel, bytes));
        }
    }
    Ok((files, new_id.to_owned(), notes))
}

fn license_includes_mit(license: &str) -> bool {
    license
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .any(|t| t == "MIT")
}

// ---- duplicate --------------------------------------------------------------------------------------

/// Copies the installed pack `id` to a new user pack with a fresh id (`<id>-copy`,
/// `<id>-copy-2`, ...) and the name "<name> (copy)"; the copy is a plain export, so a sparse
/// pack stays sparse and an embedded one becomes a complete editable root pack. Returns the new
/// id and what was written.
pub fn duplicate(
    kind: PackKind,
    registry: &ThemeRegistry,
    id: &str,
) -> Result<(String, Written), AuthorError> {
    let data_dir = registry
        .data_dir()
        .ok_or_else(|| failed("there is no data folder to put the copy in"))?;
    let parent = data_dir.join(kind.folder());
    let (packs, _) = registry.listing(kind);
    let source = packs
        .iter()
        .rev()
        .find(|p| p.valid && p.id.as_deref() == Some(id))
        .ok_or_else(|| not_exportable(kind, id, registry))?;
    let taken: BTreeSet<String> = packs
        .iter()
        .filter_map(|p| p.id.clone())
        .chain(packs.iter().filter(|p| !p.valid).map(|p| p.name.clone()))
        .collect();
    let mut new_id = None;
    for n in 1..1000 {
        let suffix = if n == 1 {
            "-copy".to_owned()
        } else {
            format!("-copy-{n}")
        };
        let stem: String = id.chars().take(64 - suffix.len()).collect();
        let candidate = format!("{}{suffix}", stem.trim_end_matches('-'));
        if !taken.contains(&candidate) && fs::symlink_metadata(parent.join(&candidate)).is_err() {
            new_id = Some(candidate);
            break;
        }
    }
    let new_id = new_id.ok_or_else(|| failed("too many copies of this pack already"))?;
    let name: String = {
        let base: String = source.name.chars().take(120 - " (copy)".len()).collect();
        format!("{base} (copy)")
    };
    let written = export(
        kind,
        registry,
        id,
        &parent,
        &ExportOptions {
            rename: Some((&new_id, &name)),
            ..ExportOptions::default()
        },
    )?;
    Ok((new_id, written))
}

/// The user's `[appearance]` of `<data_dir>/settings.toml` (defaults for a missing file): which
/// packs are selected and the color overrides a resolved export lays over a pack. The file is
/// only read.
pub fn user_appearance(data_dir: &Path) -> crate::AppearanceSettings {
    use crate::{AppearanceSettings, SettingsPaths, SettingsStore};
    let store = SettingsStore::new(SettingsPaths::in_dir(data_dir));
    store.register::<AppearanceSettings>();
    AppearanceSettings::clone(&store.get::<AppearanceSettings>())
}
