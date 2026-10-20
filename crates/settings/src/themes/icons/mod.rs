//! GPUI-free semantic icon packs, validated on the shared theme worker. Inactive packs retain
//! only hashes/paths, not raw SVG. Active snapshots own bounded bytes and have no retired list.
mod ids;
mod manifest;
#[cfg(test)]
mod tests;

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use super::{DisabledPack, PackSource, SystemAppearance, confine, svg};
use crate::error::SettingsError;
pub use ids::IconId;
pub use manifest::{Declaration, parse};

pub const DEFAULT: &str = "phosphor";
const MAX_PACK_BYTES: usize = 8 * 1024 * 1024;
pub const LICENSE: &str = include_str!("../../../../../assets/icon-themes/phosphor/LICENSE");
const BUILTIN: &str = include_str!("../../../../../assets/icon-themes/phosphor/icon-theme.toml");
/// The upstream revision and what was changed, shipped beside the embedded artwork.
pub const PROVENANCE: &str = include_str!("../../../../../assets/icon-themes/phosphor/PROVENANCE");

/// The embedded `icon-theme.toml` of the built-in `phosphor` pack.
pub const fn builtin_manifest() -> &'static str {
    BUILTIN
}

/// The embedded artwork `rel` (a path of [`builtin_manifest`]'s `[icons]` table).
pub fn builtin_asset(rel: &str) -> Option<&'static [u8]> {
    ids::embedded(rel)
}

#[derive(Debug)]
pub struct IconAsset {
    pub key: Arc<str>,
    pub bytes: Arc<[u8]>,
    hash: [u8; 32],
}
impl IconAsset {
    fn new(pack: &str, id: IconId, bytes: Vec<u8>) -> Self {
        let hash = Sha256::digest(&bytes).into();
        Self {
            key: format!(
                "theme-icons/{pack}/{:x}/{}.svg",
                Sha256::digest(&bytes),
                id.id()
            )
            .into(),
            hash,
            bytes: bytes.into(),
        }
    }
}

#[derive(Debug)]
pub struct ResolvedIcons {
    pub id: String,
    pub generation: u64,
    assets: Vec<Arc<IconAsset>>,
}
impl ResolvedIcons {
    pub fn get(&self, id: IconId) -> &Arc<IconAsset> {
        &self.assets[id.index()]
    }
    pub fn assets(&self) -> impl Iterator<Item = &Arc<IconAsset>> {
        self.assets.iter()
    }
}

#[derive(Debug, Clone)]
pub struct IconPack {
    pub declaration: Declaration,
    pub source: PackSource,
    pub kept_previous: bool,
    hashes: BTreeMap<IconId, [u8; 32]>,
    source_hashes: BTreeMap<IconId, [u8; 32]>,
    manifest_notes: Vec<SettingsError>,
}
#[derive(Debug, Clone)]
pub struct IconCatalog {
    pub generation: u64,
    pub packs: Vec<IconPack>,
    pub disabled: Vec<DisabledPack>,
    pub diagnostics: Vec<SettingsError>,
    data_dir: Option<PathBuf>,
    fingerprint: u64,
}

static BUILTIN_ICONS: LazyLock<Arc<ResolvedIcons>> = LazyLock::new(|| {
    // Embedded assets are validated by the same manifest/SVG loaders, with exhaustive tests.
    #[allow(clippy::expect_used)]
    let declaration = parse(BUILTIN, None)
        .0
        .expect("built-in icon manifest is valid");
    let assets = IconId::ALL
        .iter()
        .map(|&id| {
            #[allow(clippy::expect_used)]
            let bytes =
                ids::embedded(&declaration.icons[&id]).expect("registered built-in SVG exists");
            #[allow(clippy::expect_used)]
            let bytes =
                svg::sanitize(bytes).expect("built-in SVG passes the untrusted SVG sanitizer");
            Arc::new(IconAsset::new(DEFAULT, id, bytes))
        })
        .collect();
    Arc::new(ResolvedIcons {
        id: DEFAULT.into(),
        generation: 0,
        assets,
    })
});

pub fn builtin() -> Arc<ResolvedIcons> {
    Arc::clone(&BUILTIN_ICONS)
}

impl IconCatalog {
    pub fn builtin_only() -> Self {
        Self::scan(None, None)
    }
    pub(super) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub(super) fn scan(data_dir: Option<&Path>, previous: Option<&Self>) -> Self {
        use std::hash::{Hash, Hasher};
        let builtin = parse(BUILTIN, None).0;
        let mut out = Self {
            generation: previous.map_or(0, |p| p.generation + 1),
            packs: Vec::new(),
            disabled: Vec::new(),
            diagnostics: Vec::new(),
            data_dir: data_dir.map(Path::to_path_buf),
            fingerprint: 0,
        };
        if let Some(declaration) = builtin {
            out.packs.push(IconPack {
                declaration,
                source: PackSource::Builtin,
                kept_previous: false,
                hashes: BTreeMap::new(),
                source_hashes: BTreeMap::new(),
                manifest_notes: Vec::new(),
            });
        }
        let mut candidates = Vec::new();
        if let Some(data_dir) = data_dir {
            let dir = data_dir.join("icon-themes");
            match std::fs::symlink_metadata(&dir) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                    match std::fs::read_dir(&dir) {
                        Ok(entries) => {
                            for entry in entries.take(super::MAX_CANDIDATES + 1) {
                                match entry {
                                    Ok(e) => candidates.push(e.path()),
                                    Err(e) => out.diagnostics.push(SettingsError::error(
                                        Some(&dir),
                                        "",
                                        format!("cannot enumerate icon packs: {e}"),
                                    )),
                                }
                            }
                            candidates.sort();
                            if candidates.len() > super::MAX_CANDIDATES {
                                candidates.truncate(super::MAX_CANDIDATES);
                                out.diagnostics.push(SettingsError::warning(
                                    Some(&dir),
                                    "",
                                    "icon pack candidate limit is 256",
                                ));
                            }
                        }
                        Err(e) => out.diagnostics.push(SettingsError::error(
                            Some(&dir),
                            "",
                            format!("cannot enumerate icon packs: {e}"),
                        )),
                    }
                }
                _ => out.diagnostics.push(SettingsError::error(
                    Some(&dir),
                    "",
                    "icon-themes must be a real directory, not a symlink",
                )),
            }
            let mut total = 0;
            for path in candidates {
                // Staging folders of a pack being written (`.<id>.hutan-new-…`) are not packs.
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    continue;
                }
                let file = path.join("icon-theme.toml");
                let old = previous.and_then(|p| {
                    p.packs
                        .iter()
                        .find(|p| p.source == PackSource::User(file.clone()))
                });
                match load_dir(data_dir, &path, old, &mut total, &mut out.diagnostics) {
                    Load::Budget => break,
                    Load::Pack(pack) | Load::Kept(pack) => out.packs.push(pack),
                    Load::Disabled(disabled) => out.disabled.push(disabled),
                }
            }
        }
        let mut counts = BTreeMap::new();
        for pack in &out.packs {
            if matches!(pack.source, PackSource::User(_)) {
                *counts.entry(pack.declaration.meta.id.clone()).or_insert(0) += 1;
            }
        }
        out.packs.retain(|pack| {
            if matches!(pack.source, PackSource::User(_)) && counts[&pack.declaration.meta.id] > 1 {
                let PackSource::User(path) = &pack.source else {
                    return true;
                };
                out.diagnostics.push(SettingsError::error(
                    Some(path),
                    "id",
                    "duplicate user icon pack id; every duplicate is disabled",
                ));
                out.disabled.push(DisabledPack {
                    path: path.clone(),
                    id: Some(pack.declaration.meta.id.clone()),
                });
                false
            } else {
                true
            }
        });
        if out.diagnostics.len() > super::MAX_DIAGNOSTICS {
            let more = out.diagnostics.len() - super::MAX_DIAGNOSTICS;
            out.diagnostics.truncate(super::MAX_DIAGNOSTICS);
            out.diagnostics.push(SettingsError::warning(
                None,
                "",
                format!("{more} more icon diagnostics are not shown"),
            ));
        }
        let mut hash = std::hash::DefaultHasher::new();
        for pack in &out.packs {
            pack.declaration.source_hash.hash(&mut hash);
            pack.hashes.hash(&mut hash);
            pack.kept_previous.hash(&mut hash);
            if let PackSource::User(path) = &pack.source {
                path.hash(&mut hash);
            }
        }
        for d in &out.diagnostics {
            (&d.file, &d.key_path, &d.message).hash(&mut hash);
        }
        out.fingerprint = hash.finish();
        out
    }

    /// Resolve from immutable built-ins plus explicitly validated per-id entries. No other pack
    /// can be inherited. Only one selected snapshot is remembered by the registry.
    pub fn select(
        &self,
        selector: &str,
        appearance: SystemAppearance,
        previous: Option<&Arc<ResolvedIcons>>,
    ) -> (Arc<ResolvedIcons>, Vec<SettingsError>) {
        let mut diagnostics = Vec::new();
        let pack = self.packs.iter().rev().find(|p| {
            p.declaration.meta.id == selector
                && p.declaration.appearance.is_none_or(|a| a == appearance)
        });
        let Some(pack) = pack else {
            diagnostics.push(SettingsError::warning(None,"appearance.icon_theme",format!("icon pack `{selector}` is missing, disabled or has the wrong appearance; using embedded phosphor")));
            return (builtin(), diagnostics);
        };
        if matches!(pack.source, PackSource::Builtin) {
            return (builtin(), diagnostics);
        }
        let mut assets: Vec<_> = builtin().assets.clone();
        let mut bytes = 0;
        for (&id, rel) in &pack.declaration.icons {
            if let Some(asset) = previous
                .filter(|p| p.id == selector)
                .map(|p| p.get(id))
                .filter(|a| pack.hashes.get(&id) == Some(&a.hash))
            {
                assets[id.index()] = Arc::clone(asset);
                continue;
            }
            let raw = self
                .data_dir
                .as_ref()
                .zip(match &pack.source {
                    PackSource::User(file) => Some(file),
                    _ => None,
                })
                .ok_or_else(|| "pack source missing".to_owned())
                .and_then(|(root, file)| read_asset(root, file, rel))
                .and_then(|raw| svg::sanitize(&raw));
            let asset = match raw {
                Ok(raw) if pack.hashes.get(&id) == Some(&Sha256::digest(&raw).into()) => {
                    bytes += raw.len();
                    if bytes <= MAX_PACK_BYTES {
                        Some(Arc::new(IconAsset::new(selector, id, raw)))
                    } else {
                        None
                    }
                }
                _ => None,
            };
            if let Some(asset) = asset {
                assets[id.index()] = asset;
            } else {
                if let Some(old) = previous.filter(|p| p.id == selector) {
                    assets[id.index()] = Arc::clone(old.get(id));
                }
                diagnostics.push(SettingsError::warning(
                    None,
                    format!("icons.{}", id.id()),
                    "invalid SVG edit; kept last-good entry or default icon",
                ));
            }
        }
        if let Some(previous) = previous.filter(|p| {
            p.id == selector && p.assets.iter().zip(&assets).all(|(a, b)| a.key == b.key)
        }) {
            return (Arc::clone(previous), diagnostics);
        }
        (
            Arc::new(ResolvedIcons {
                id: selector.into(),
                generation: self.generation,
                assets,
            }),
            diagnostics,
        )
    }
}

/// What loading one pack folder came to.
enum Load {
    /// The catalog's manifest budget is spent: stop scanning.
    Budget,
    Pack(IconPack),
    /// The manifest edit is invalid; the previous version stays.
    Kept(IconPack),
    Disabled(DisabledPack),
}

/// Loads the pack folder `path` (below `root`, the confinement root): manifest, folder
/// inventory, every referenced asset and the license file. The one implementation behind both
/// the catalog scan and `hutan theme validate <path>`, so a pack is judged the same way in both.
fn load_dir(
    root: &Path,
    path: &Path,
    old: Option<&IconPack>,
    total: &mut usize,
    diagnostics: &mut Vec<SettingsError>,
) -> Load {
    let file = path.join("icon-theme.toml");
    let mut manifest_notes = Vec::new();
    let rel = file.strip_prefix(root).ok().and_then(Path::to_str);
    let loaded = rel
        .ok_or_else(|| "non-UTF-8 icon pack path".to_owned())
        .and_then(|rel| confine::read_text(root, rel, super::MAX_MANIFEST_BYTES as u64));
    let declaration = match loaded {
        Ok(text) => {
            *total += text.len();
            if *total > super::MAX_TOTAL_MANIFEST_BYTES {
                diagnostics.push(SettingsError::error(
                    Some(&file),
                    "",
                    "catalog manifest limit is 16 MiB",
                ));
                return Load::Budget;
            }
            let declaration = if let Some(old) =
                old.filter(|p| p.declaration.source_hash == super::manifest::hash_text(&text))
            {
                manifest_notes = old.manifest_notes.clone();
                Some(old.declaration.clone())
            } else {
                let (declaration, mut notes) = parse(&text, Some(&file));
                if let Some(note) = declaration
                    .as_ref()
                    .and_then(|d| super::manifest::license_note(&d.meta.license))
                {
                    notes.push(SettingsError::warning(Some(&file), "license", note));
                }
                manifest_notes = notes;
                declaration
            };
            diagnostics.extend(manifest_notes.iter().cloned());
            declaration
        }
        Err(why) => {
            diagnostics.push(SettingsError::error(Some(&file), "", why));
            None
        }
    };
    let Some(declaration) = declaration else {
        return if let Some(old) = old {
            let mut old = old.clone();
            old.kept_previous = true;
            diagnostics.push(SettingsError::warning(
                Some(&file),
                "",
                "kept previous icon pack; invalid manifest edit",
            ));
            Load::Kept(old)
        } else {
            Load::Disabled(DisabledPack {
                path: file,
                id: None,
            })
        };
    };
    if let Err(why) = inventory(path) {
        diagnostics.push(SettingsError::error(Some(&file), "", why));
        return Load::Disabled(DisabledPack {
            path: file,
            id: Some(declaration.meta.id),
        });
    }
    let mut pack = IconPack {
        declaration,
        source: PackSource::User(file.clone()),
        kept_previous: false,
        hashes: BTreeMap::new(),
        source_hashes: BTreeMap::new(),
        manifest_notes,
    };
    let mut bytes = 0;
    let mut files = BTreeSet::new();
    // Validate one pack at a time; discard raw bytes immediately for inactive packs.
    for (&id, rel) in &pack.declaration.icons {
        let result = read_asset(root, &file, rel).and_then(|raw| {
            if files.insert(rel) {
                bytes += raw.len();
            }
            if bytes > MAX_PACK_BYTES || files.len() > 512 {
                return Err("pack exceeds 8 MiB / 512 files".into());
            }
            let source_hash = Sha256::digest(&raw).into();
            pack.source_hashes.insert(id, source_hash);
            if let Some(hash) = old
                .filter(|old| old.source_hashes.get(&id) == Some(&source_hash))
                .and_then(|old| old.hashes.get(&id))
            {
                Ok(*hash)
            } else {
                svg::sanitize(&raw).map(|raw| Sha256::digest(&raw).into())
            }
        });
        match result {
            Ok(hash) => {
                pack.hashes.insert(id, hash);
            }
            Err(why) => diagnostics.push(SettingsError::warning(
                Some(&file),
                format!("icons.{}", id.id()),
                format!(
                    "{why}; kept last-good entry when active, otherwise using default icon for this id"
                ),
            )),
        }
    }
    if let Some(license) = &pack.declaration.meta.license_file {
        let license_path = path.join(license);
        let result = license_path
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| "invalid license path".to_owned())
            .and_then(|rel| confine::read_text(root, rel, super::MAX_MANIFEST_BYTES as u64));
        if let Err(why) = result {
            diagnostics.push(SettingsError::error(Some(&file), "license_file", why));
            return Load::Disabled(DisabledPack {
                path: file,
                id: Some(pack.declaration.meta.id),
            });
        }
    }
    Load::Pack(pack)
}

/// The verdict of validating one icon pack folder outside a catalog.
#[derive(Debug)]
pub struct DirVerdict {
    /// The pack, when it can be used (assets with problems fall back per id and are warnings).
    pub declaration: Option<Declaration>,
    pub diagnostics: Vec<SettingsError>,
}

/// Validates the icon pack folder `dir` exactly as the catalog scan would. `dir` is resolved
/// once (it may be a symlink the user named); nothing below it is followed.
pub fn validate_dir(dir: &Path) -> Result<DirVerdict, String> {
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let root = dir
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", dir.display()))?;
    let mut diagnostics = Vec::new();
    let mut total = 0;
    let declaration = match load_dir(root, &dir, None, &mut total, &mut diagnostics) {
        Load::Pack(pack) | Load::Kept(pack) => Some(pack.declaration),
        Load::Budget | Load::Disabled(_) => None,
    };
    Ok(DirVerdict {
        declaration,
        diagnostics,
    })
}

fn read_asset(data_dir: &Path, manifest: &Path, rel: &str) -> Result<Vec<u8>, String> {
    if rel.len() > 1024 {
        return Err("asset path exceeds 1024 bytes".into());
    }
    confine::check_relative(rel)?;
    if rel.split('/').count() > 5 {
        return Err("asset directory depth exceeds 4".into());
    }
    let path = manifest.parent().ok_or("pack directory missing")?.join(rel);
    let rel = path
        .strip_prefix(data_dir)
        .ok()
        .and_then(Path::to_str)
        .ok_or("asset outside data directory")?;
    confine::read_confined(data_dir, rel, svg::MAX_SVG_BYTES as u64)
}

/// Add bounded real directories to the existing TH-1 watcher, including nested SVG folders.
pub(super) fn watch_dirs(data_dir: &Path, out: &mut BTreeSet<PathBuf>) {
    let root = data_dir.join("icon-themes");
    let real = |p: &Path| {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    };
    if !real(&root) {
        return;
    }
    out.insert(root.clone());
    if let Ok(packs) = std::fs::read_dir(root) {
        for entry in packs.flatten().take(super::MAX_CANDIDATES) {
            let path = entry.path();
            if !real(&path) {
                continue;
            }
            let mut pending = vec![(path, 0)];
            let mut examined = 0;
            while let Some((dir, depth)) = pending.pop() {
                out.insert(dir.clone());
                if depth >= 4 {
                    continue;
                }
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten().take(513 - examined) {
                        examined += 1;
                        if examined > 512 {
                            break;
                        }
                        let path = entry.path();
                        if real(&path) {
                            pending.push((path, depth + 1));
                        }
                    }
                }
                if examined > 512 {
                    break;
                }
            }
        }
    }
}

/// Offline author-tool projection of the native icon contract. Runtime checks additionally
/// enforce metadata, paths, aggregate limits and the SVG subset.
pub fn schema() -> serde_json::Value {
    let properties: serde_json::Map<String, serde_json::Value> = IconId::ALL.iter().map(|id| {
        (id.id().to_owned(), serde_json::json!({"type":"string","description":"Pack-root-relative monochrome SVG path","maxLength":1024}))
    }).collect();
    serde_json::json!({
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"Hutan icon pack, schema v1",
        "type":"object",
        "required":["schema_version","id","name","authors","license","icons"],
        "properties":{
            "schema_version":{"const":1},
            "id":{"type":"string","pattern":"^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$","maxLength":64,"not":{"const":"auto"}},
            "name":{"type":"string","minLength":1,"maxLength":120},
            "authors":{"type":"array","minItems":1,"items":{"type":"string","minLength":1,"maxLength":200}},
            "license":{"type":"string","minLength":1,"maxLength":200},
            "license_file":{"type":"string","maxLength":200},
            "description":{"type":"string","maxLength":1024},
            "repository":{"type":"string","pattern":"^https://","maxLength":512},
            "version":{"type":"string","maxLength":64},
            "appearance":{"enum":["dark","light"]},
            "root":{"type":"boolean"},
            "extends":{"const":"builtin:phosphor"},
            "icons":{"type":"object","maxProperties":512,"properties":properties,"additionalProperties":true}
        },
        "oneOf":[
            {"properties":{"root":{"const":true}},"required":["root"],"not":{"required":["extends"]}},
            {"properties":{"root":{"const":false}},"required":["extends"]}
        ],
        "additionalProperties":true
    })
}

/// Count all entries, not only referenced assets. No recursive symlink traversal.
fn inventory(root: &Path) -> Result<(), String> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut count = 0;
    while let Some((dir, depth)) = pending.pop() {
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("cannot enumerate icon assets: {e}"))?;
        for entry in entries {
            count += 1;
            if count > 512 {
                return Err("icon pack exceeds 512 files/directories".into());
            }
            let entry = entry.map_err(|e| format!("cannot enumerate icon asset: {e}"))?;
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot inspect icon asset: {e}"))?;
            if kind.is_dir() {
                if depth >= 4 {
                    return Err("icon asset directory depth exceeds 4".into());
                }
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    Ok(())
}
