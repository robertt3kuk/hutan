//! The catalog: the embedded built-ins plus the packs found in `<data dir>/themes/`, validated,
//! resolved and published as one immutable snapshot. Scanning is bounded and deterministic
//! (sorted, capped); an invalid edit of a pack that was good keeps the last good version.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use super::confine;
use super::contrast;
use super::manifest::{self, Builtin, Declaration, MAX_MANIFEST_BYTES};
use super::resolve::{self, ColorOverrides, ResolvedTheme, TerminalTheme};
use crate::appearance::ThemeOverrides;
use crate::error::SettingsError;
use crate::themes::{AUTO, SystemAppearance};

/// Candidate entries (directories and `.toml` files) examined per scan.
pub const MAX_CANDIDATES: usize = 256;
/// Manifest bytes read per scan.
pub const MAX_TOTAL_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
/// Diagnostics kept per snapshot; one more summarizes the rest.
pub const MAX_DIAGNOSTICS: usize = 512;

const DARK_TOML: &str = include_str!("../../../../assets/themes/hutan-dark/theme.toml");
const LIGHT_TOML: &str = include_str!("../../../../assets/themes/hutan-light/theme.toml");

/// Where a pack comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackSource {
    /// Embedded in the binary.
    Builtin,
    /// A user pack: its manifest file.
    User(PathBuf),
}

/// What a picker or `hutan theme ls` shows about a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeDescriptor {
    pub id: String,
    pub name: String,
    pub appearance: SystemAppearance,
    pub authors: Vec<String>,
    pub license: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub source: PackSource,
    /// A user pack that shadows the built-in of the same id (shown as "user override").
    pub overrides_builtin: bool,
    /// Converted from another tool's file (its `[provenance]` names a `source_format`).
    pub imported: bool,
    /// The file has an invalid edit; this is the last good version.
    pub kept_previous: bool,
}

/// A valid pack: its declaration (kept for overrides) and the resolved theme.
#[derive(Debug, Clone)]
pub struct ThemePack {
    pub descriptor: ThemeDescriptor,
    declaration: Arc<Declaration>,
    resolved: Arc<ResolvedTheme>,
}

impl ThemePack {
    pub fn resolved(&self) -> &Arc<ResolvedTheme> {
        &self.resolved
    }

    pub fn declaration(&self) -> &Declaration {
        &self.declaration
    }
}

/// A pack that could not be loaded; listed disabled with its errors in the diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisabledPack {
    pub path: PathBuf,
    /// The pack's id when its manifest got far enough to have one.
    pub id: Option<String>,
}

/// An immutable view of every available color pack at one generation.
#[derive(Debug)]
pub struct ThemeCatalogSnapshot {
    /// Increases on every published change; 0 is the first scan.
    pub generation: u64,
    packs: Vec<Arc<ThemePack>>,
    disabled: Vec<DisabledPack>,
    diagnostics: Vec<SettingsError>,
    fingerprint: u64,
}

struct Builtins {
    dark: Arc<ThemePack>,
    light: Arc<ThemePack>,
}

impl Builtins {
    fn get(&self, builtin: Builtin) -> &Arc<ThemePack> {
        match builtin {
            Builtin::HutanDark => &self.dark,
            Builtin::HutanLight => &self.light,
        }
    }
}

/// Parses an embedded manifest with the loader every user pack goes through.
#[allow(clippy::expect_used)] // the embedded manifests are asserted valid by `builtins_are_valid_roots`
fn load_builtin(text: &str) -> Arc<ThemePack> {
    let parsed = manifest::parse(text, None);
    let declaration = Arc::new(
        parsed
            .declaration
            .expect("embedded built-in manifests are valid"),
    );
    let resolved = resolve::resolve(&declaration, None, &ColorOverrides::default())
        .expect("embedded built-in manifests resolve");
    Arc::new(ThemePack {
        descriptor: descriptor(&declaration, PackSource::Builtin, false, false),
        declaration,
        resolved: Arc::new(resolved),
    })
}

static BUILTINS: LazyLock<Builtins> = LazyLock::new(|| Builtins {
    dark: load_builtin(DARK_TOML),
    light: load_builtin(LIGHT_TOML),
});

fn descriptor(
    declaration: &Declaration,
    source: PackSource,
    overrides_builtin: bool,
    kept_previous: bool,
) -> ThemeDescriptor {
    let meta = &declaration.meta;
    ThemeDescriptor {
        id: meta.id.clone(),
        name: meta.name.clone(),
        appearance: meta.appearance,
        authors: meta.authors.clone(),
        license: meta.license.clone(),
        version: meta.version.clone(),
        description: meta.description.clone(),
        source,
        overrides_builtin,
        imported: declaration.imported,
        kept_previous,
    }
}

/// The ids of the built-in packs, dark first.
pub fn builtin_ids() -> [&'static str; 2] {
    [Builtin::HutanDark.id(), Builtin::HutanLight.id()]
}

/// The embedded manifest text of `builtin`: what `hutan theme export` writes for it.
pub const fn builtin_source(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::HutanDark => DARK_TOML,
        Builtin::HutanLight => LIGHT_TOML,
    }
}

/// The immutable embedded pack for `builtin` (never a user's shadow of it).
pub fn builtin_theme(builtin: Builtin) -> Arc<ResolvedTheme> {
    Arc::clone(&BUILTINS.get(builtin).resolved)
}

/// The built-in of a polarity.
pub fn builtin_for(appearance: SystemAppearance) -> Builtin {
    match appearance {
        SystemAppearance::Dark => Builtin::HutanDark,
        SystemAppearance::Light => Builtin::HutanLight,
    }
}

impl ThemeCatalogSnapshot {
    /// Only the built-ins.
    pub fn builtin_only() -> Self {
        Self::assemble(0, Vec::new(), Vec::new(), Vec::new())
    }

    fn assemble(
        generation: u64,
        user: Vec<Arc<ThemePack>>,
        disabled: Vec<DisabledPack>,
        mut diagnostics: Vec<SettingsError>,
    ) -> Self {
        let mut packs: Vec<Arc<ThemePack>> = Builtin::ALL
            .iter()
            .map(|b| BUILTINS.get(*b))
            .filter(|b| !user.iter().any(|u| u.descriptor.id == b.descriptor.id))
            .cloned()
            .collect();
        packs.extend(user);
        let rank = |id: &str| Builtin::ALL.iter().position(|b| b.id() == id);
        packs.sort_by(|a, b| {
            let (a, b) = (&a.descriptor.id, &b.descriptor.id);
            (rank(a).is_none(), rank(a), a).cmp(&(rank(b).is_none(), rank(b), b))
        });
        if diagnostics.len() > MAX_DIAGNOSTICS {
            let more = diagnostics.len() - MAX_DIAGNOSTICS;
            diagnostics.truncate(MAX_DIAGNOSTICS);
            diagnostics.push(SettingsError::warning(
                None,
                "",
                format!("{more} more theme diagnostics are not shown"),
            ));
        }
        let mut hasher = DefaultHasher::new();
        for pack in &packs {
            pack.descriptor.id.hash(&mut hasher);
            pack.declaration.source_hash.hash(&mut hasher);
            pack.descriptor.kept_previous.hash(&mut hasher);
            if let PackSource::User(path) = &pack.descriptor.source {
                path.hash(&mut hasher);
            }
        }
        for pack in &disabled {
            pack.path.hash(&mut hasher);
        }
        for d in &diagnostics {
            (d.severity, &d.file, &d.key_path, &d.message).hash(&mut hasher);
        }
        Self {
            generation,
            packs,
            disabled,
            diagnostics,
            fingerprint: hasher.finish(),
        }
    }

    pub(super) fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    pub(super) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Valid packs: the built-ins first, then the rest by id.
    pub fn packs(&self) -> &[Arc<ThemePack>] {
        &self.packs
    }

    pub fn pack(&self, id: &str) -> Option<&Arc<ThemePack>> {
        self.packs.iter().find(|p| p.descriptor.id == id)
    }

    /// Packs that could not be loaded (and have no last good version).
    pub fn disabled(&self) -> &[DisabledPack] {
        &self.disabled
    }

    /// Everything wrong with the packs, in scan order, capped at [`MAX_DIAGNOSTICS`] plus a
    /// summary line.
    pub fn diagnostics(&self) -> &[SettingsError] {
        &self.diagnostics
    }

    /// Directories of user packs worth watching (their manifests change in place).
    pub(super) fn pack_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .packs
            .iter()
            .filter_map(|p| match &p.descriptor.source {
                PackSource::User(path) => path.parent().map(Path::to_path_buf),
                PackSource::Builtin => None,
            })
            .chain(
                self.disabled
                    .iter()
                    .filter_map(|d| d.path.parent().map(Path::to_path_buf)),
            )
            .collect();
        dirs.sort();
        dirs.dedup();
        dirs
    }
}

// ---- scanning -----------------------------------------------------------------------------

struct Candidate {
    /// The manifest file.
    path: PathBuf,
    outcome: Result<Arc<ThemePack>, ()>,
    diagnostics: Vec<SettingsError>,
}

fn base_of(declaration: &Declaration) -> Option<&Arc<Declaration>> {
    declaration
        .meta
        .extends
        .map(|b| &BUILTINS.get(b).declaration)
}

/// Loads one manifest's text into a pack, with the diagnostics of the attempt.
pub fn load_pack(
    text: &str,
    file: &Path,
    pack_dir: Option<&Path>,
) -> (Option<Arc<ThemePack>>, Vec<SettingsError>) {
    let parsed = manifest::parse(text, Some(file));
    let mut diagnostics = parsed.diagnostics;
    let Some(declaration) = parsed.declaration else {
        return (None, diagnostics);
    };
    let declaration = Arc::new(declaration);
    let base = base_of(&declaration).map(AsRef::as_ref);
    match resolve::resolve(&declaration, base, &ColorOverrides::default()) {
        Err(problems) => {
            diagnostics.extend(
                problems
                    .into_iter()
                    .map(|(key, message)| SettingsError::error(Some(file), key, message)),
            );
            (None, diagnostics)
        }
        Ok(resolved) => {
            if let Some(note) = manifest::license_note(&declaration.meta.license) {
                diagnostics.push(SettingsError::warning(Some(file), "license", note));
            }
            // A root pack has no base to be worse than: it is judged against the built-in of its
            // polarity, so an exported (flattened) copy reports what its source reported.
            let base_theme = builtin_theme(
                declaration
                    .meta
                    .extends
                    .unwrap_or_else(|| builtin_for(declaration.meta.appearance)),
            );
            for issue in contrast::new_issues(&resolved, Some(&base_theme)) {
                diagnostics.push(SettingsError::warning(
                    Some(file),
                    "contrast",
                    issue.message(),
                ));
            }
            if let Some(license_file) = &declaration.meta.license_file {
                match pack_dir {
                    Some(dir) => {
                        if let Err(why) = confine::read_confined(dir, license_file, 256 * 1024) {
                            let severity_error = why.contains("symlink") || why.contains("changed");
                            let key = "license_file";
                            let message = format!("{license_file}: {why}");
                            diagnostics.push(if severity_error {
                                SettingsError::error(Some(file), key, message)
                            } else {
                                SettingsError::warning(Some(file), key, message)
                            });
                            if severity_error {
                                return (None, diagnostics);
                            }
                        }
                    }
                    None => diagnostics.push(SettingsError::warning(
                        Some(file),
                        "license_file",
                        "ignored: a single-file pack has no files next to it",
                    )),
                }
            }
            let overrides_builtin = Builtin::ALL.iter().any(|b| b.id() == declaration.meta.id);
            let pack = ThemePack {
                descriptor: descriptor(
                    &declaration,
                    PackSource::User(file.to_path_buf()),
                    overrides_builtin,
                    false,
                ),
                declaration,
                resolved: Arc::new(resolved),
            };
            (Some(Arc::new(pack)), diagnostics)
        }
    }
}

/// Scans `themes_dir` (a directory inside the data dir) for user packs. A pack whose manifest
/// is now invalid but was valid in `previous` keeps its last good version.
pub(super) fn scan(
    themes_dir: &Path,
    previous: Option<&ThemeCatalogSnapshot>,
) -> ThemeCatalogSnapshot {
    let mut diagnostics = Vec::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut disabled = Vec::new();

    match fs::symlink_metadata(themes_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => diagnostics.push(SettingsError::error(
            Some(themes_dir),
            "",
            format!("cannot read the themes folder: {e}"),
        )),
        Ok(meta) if meta.file_type().is_symlink() => diagnostics.push(SettingsError::error(
            Some(themes_dir),
            "",
            "the themes folder is a symlink; refused (packs must live inside the data folder)",
        )),
        Ok(meta) if !meta.is_dir() => diagnostics.push(SettingsError::error(
            Some(themes_dir),
            "",
            "the themes path is not a folder",
        )),
        Ok(_) => scan_dir(themes_dir, &mut candidates, &mut diagnostics),
    }

    // Duplicate ids within the user tier disable every candidate with that id.
    let mut by_id: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, candidate) in candidates.iter().enumerate() {
        if let Ok(pack) = &candidate.outcome {
            by_id.entry(pack.descriptor.id.clone()).or_default().push(i);
        }
    }
    for (id, indices) in by_id.iter().filter(|(_, v)| v.len() > 1) {
        let paths: Vec<String> = indices
            .iter()
            .map(|i| candidates[*i].path.display().to_string())
            .collect();
        for i in indices {
            let message = format!(
                "theme id `{id}` is used by more than one pack ({}); none of them is loaded",
                paths.join(", ")
            );
            let candidate = &mut candidates[*i];
            candidate
                .diagnostics
                .push(SettingsError::error(Some(&candidate.path), "id", message));
            candidate.outcome = Err(());
        }
    }

    let mut user = Vec::new();
    for candidate in candidates {
        match candidate.outcome {
            Ok(pack) => user.push(pack),
            Err(()) => {
                let kept = previous.and_then(|p| {
                    p.packs.iter().find(|old| {
                        matches!(&old.descriptor.source, PackSource::User(path) if *path == candidate.path)
                    })
                });
                // A clash of ids is not an edit of a good pack: nothing is kept then.
                let clash = candidate
                    .diagnostics
                    .iter()
                    .any(|d| d.key_path == "id" && d.message.contains("more than one pack"));
                match kept.filter(|_| !clash) {
                    Some(old) => {
                        let mut pack = (**old).clone();
                        pack.descriptor.kept_previous = true;
                        diagnostics.extend(candidate.diagnostics);
                        diagnostics.push(SettingsError::warning(
                            Some(&candidate.path),
                            "",
                            format!(
                                "kept the previous version of theme `{}`: this edit has errors",
                                pack.descriptor.id
                            ),
                        ));
                        user.push(Arc::new(pack));
                        continue;
                    }
                    None => disabled.push(DisabledPack {
                        id: None,
                        path: candidate.path.clone(),
                    }),
                }
            }
        }
        diagnostics.extend(candidate.diagnostics);
    }
    user.sort_by(|a, b| a.descriptor.id.cmp(&b.descriptor.id));
    ThemeCatalogSnapshot::assemble(0, user, disabled, diagnostics)
}

fn scan_dir(
    themes_dir: &Path,
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<SettingsError>,
) {
    let entries = match fs::read_dir(themes_dir) {
        Ok(entries) => entries,
        Err(e) => {
            diagnostics.push(SettingsError::error(
                Some(themes_dir),
                "",
                format!("cannot list the themes folder: {e}"),
            ));
            return;
        }
    };
    let mut names: Vec<(String, fs::FileType)> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        names.push((name, file_type));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));

    let mut examined = 0usize;
    let mut skipped = 0usize;
    let mut bytes = 0usize;
    for (name, file_type) in names {
        let path = themes_dir.join(&name);
        let single_file = !file_type.is_dir() && name.ends_with(".toml");
        if !(file_type.is_dir() || single_file || file_type.is_symlink()) {
            continue;
        }
        if examined == MAX_CANDIDATES {
            skipped += 1;
            continue;
        }
        examined += 1;
        if file_type.is_symlink() {
            diagnostics.push(SettingsError::error(
                Some(&path),
                "",
                "symlink refused: packs must be real folders or files inside the themes folder",
            ));
            continue;
        }
        let (root, rel, manifest_path) = if single_file {
            (themes_dir.to_path_buf(), name.clone(), path.clone())
        } else {
            (
                path.clone(),
                "theme.toml".to_owned(),
                path.join("theme.toml"),
            )
        };
        if bytes >= MAX_TOTAL_MANIFEST_BYTES {
            skipped += 1;
            continue;
        }
        let text = match confine::read_text(&root, &rel, MAX_MANIFEST_BYTES as u64) {
            Ok(text) => text,
            Err(why) => {
                candidates.push(Candidate {
                    path: manifest_path.clone(),
                    outcome: Err(()),
                    diagnostics: vec![SettingsError::error(Some(&manifest_path), "", why)],
                });
                continue;
            }
        };
        bytes += text.len();
        let (pack, pack_diagnostics) = load_pack(
            &text,
            &manifest_path,
            (!single_file).then_some(root.as_path()),
        );
        candidates.push(Candidate {
            path: manifest_path,
            outcome: pack.ok_or(()),
            diagnostics: pack_diagnostics,
        });
    }
    if skipped > 0 {
        diagnostics.push(SettingsError::warning(
            Some(themes_dir),
            "",
            format!(
                "stopped at {MAX_CANDIDATES} entries or {} MiB of manifests; {skipped} more were not read",
                MAX_TOTAL_MANIFEST_BYTES / (1024 * 1024)
            ),
        ));
    }
}

// ---- selection ----------------------------------------------------------------------------

/// The user's last accepted color overrides per pack, so a bad edit of them keeps what worked.
#[derive(Debug, Default)]
pub struct OverrideMemory(BTreeMap<String, ColorOverrides>);

/// The inputs of [`ThemeCatalogSnapshot::select`].
#[derive(Debug, Clone, Copy)]
pub struct SelectionRequest<'a> {
    /// What the window is: the polarity `appearance.appearance` resolved to.
    pub polarity: SystemAppearance,
    pub light_theme: &'a str,
    pub dark_theme: &'a str,
    pub overrides: &'a BTreeMap<String, ThemeOverrides>,
}

/// The theme in effect and what was wrong with the request.
#[derive(Debug, Clone)]
pub struct Selected {
    pub theme: Arc<ResolvedTheme>,
    /// The id the user asked for; kept even when it fell back, so installing it recovers.
    pub requested: String,
    pub diagnostics: Vec<SettingsError>,
}

impl ThemeCatalogSnapshot {
    /// The pack for the polarity's slot, with the user's overrides applied. A missing or
    /// wrong-polarity pack falls back to the embedded built-in of that polarity.
    pub fn select(&self, request: SelectionRequest<'_>, memory: &mut OverrideMemory) -> Selected {
        let (key, requested) = match request.polarity {
            SystemAppearance::Light => ("appearance.light_theme", request.light_theme),
            SystemAppearance::Dark => ("appearance.dark_theme", request.dark_theme),
        };
        let builtin = builtin_for(request.polarity);
        let mut diagnostics = Vec::new();
        let pack = match self.pack(requested) {
            Some(pack) if pack.descriptor.appearance == request.polarity => Some(pack),
            Some(pack) => {
                diagnostics.push(SettingsError::warning(
                    None,
                    key,
                    format!(
                        "`{requested}` is a {} theme; this slot needs a {} one, so {} is used",
                        super::manifest::appearance_name(pack.descriptor.appearance),
                        super::manifest::appearance_name(request.polarity),
                        builtin.id()
                    ),
                ));
                None
            }
            None => {
                diagnostics.push(SettingsError::warning(
                    None,
                    key,
                    format!(
                        "theme `{requested}` is not installed; {} is used until it appears",
                        builtin.id()
                    ),
                ));
                None
            }
        };
        let theme = match pack {
            Some(pack) => self.apply_overrides(pack, request.overrides, memory, &mut diagnostics),
            None => builtin_theme(builtin),
        };
        Selected {
            theme,
            requested: requested.to_owned(),
            diagnostics,
        }
    }

    fn apply_overrides(
        &self,
        pack: &ThemePack,
        all: &BTreeMap<String, ThemeOverrides>,
        memory: &mut OverrideMemory,
        diagnostics: &mut Vec<SettingsError>,
    ) -> Arc<ResolvedTheme> {
        let id = &pack.descriptor.id;
        let Some(wanted) = all.get(id).filter(|o| !o.colors.is_empty()) else {
            memory.0.remove(id);
            return Arc::clone(&pack.resolved);
        };
        let prefix = format!("appearance.theme_overrides.{id}.colors");
        let (parsed, problems) = ColorOverrides::parse(&wanted.colors, &prefix);
        let rejected = problems.iter().any(SettingsError::is_error);
        diagnostics.extend(problems);
        let overrides = if rejected {
            match memory.0.get(id) {
                Some(previous) => {
                    diagnostics.push(SettingsError::warning(
                        None,
                        prefix,
                        "these overrides were not applied; the previous accepted ones stay",
                    ));
                    previous.clone()
                }
                None => {
                    diagnostics.push(SettingsError::warning(
                        None,
                        prefix,
                        "these overrides were not applied",
                    ));
                    return Arc::clone(&pack.resolved);
                }
            }
        } else {
            memory.0.insert(id.clone(), parsed.clone());
            parsed
        };
        let base = base_of(&pack.declaration).map(AsRef::as_ref);
        match resolve::resolve(&pack.declaration, base, &overrides) {
            Ok(resolved) => Arc::new(resolved),
            Err(problems) => {
                diagnostics.extend(
                    problems
                        .into_iter()
                        .map(|(key, message)| SettingsError::error(None, key, message)),
                );
                Arc::clone(&pack.resolved)
            }
        }
    }

    /// The terminal palette for a `terminal.theme` value: `auto` is the active theme's own; a
    /// pack id picks that pack's terminal section only; an unknown id falls back to `auto`.
    pub fn terminal(
        &self,
        selector: &str,
        active: &ResolvedTheme,
        overrides: &BTreeMap<String, ThemeOverrides>,
        memory: &mut OverrideMemory,
    ) -> (TerminalTheme, Vec<SettingsError>) {
        if selector == AUTO || selector == active.id {
            return (active.terminal(), Vec::new());
        }
        let mut diagnostics = Vec::new();
        match self.pack(selector) {
            Some(pack) => {
                let theme = self.apply_overrides(pack, overrides, memory, &mut diagnostics);
                (theme.terminal(), diagnostics)
            }
            None => {
                diagnostics.push(SettingsError::warning(
                    None,
                    "terminal.theme",
                    format!("theme `{selector}` is not installed; the active theme's terminal colors are used"),
                ));
                (active.terminal(), diagnostics)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_valid_roots() {
        let snapshot = ThemeCatalogSnapshot::builtin_only();
        assert_eq!(
            snapshot
                .packs()
                .iter()
                .map(|p| p.descriptor.id.as_str())
                .collect::<Vec<_>>(),
            ["hutan-dark", "hutan-light"]
        );
        for pack in snapshot.packs() {
            assert!(pack.declaration().meta.root);
            assert_eq!(pack.descriptor.source, PackSource::Builtin);
        }
        assert!(snapshot.diagnostics().is_empty());
    }
}
