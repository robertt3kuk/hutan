//! `hutan theme ls|validate|new|export`: color and icon packs as local files (docs/themes.md).
//! Nothing here starts, dials or even resolves the daemon: the commands read and write the data
//! folder (`$HUTAN_HOME`, else `~/Library/Application Support/Hutan`) with the same loaders the
//! desktop uses (`hutan_settings::themes::author`), so a pack is judged the same way in both.
//!
//! Exit status: 0 ok (warnings included), 1 errors in a pack or a file that cannot be read or
//! written, 2 invalid invocation; `validate --strict` makes warnings status 1.

use std::path::{Path, PathBuf};

use hutan_settings::themes::ThemeRegistry;
use hutan_settings::themes::author::{
    AuthorError, ExportOptions, NewPack, PackInfo, PackKind, Validation, export, looks_like_path,
    new_pack, user_appearance, validate_installed, validate_path,
};
use hutan_settings::{SettingsError, SettingsPaths, SystemAppearance};
use serde_json::{Value, json};

use super::print_line;
use crate::args::{
    ThemeCommand, ThemeExportArgs, ThemeKind, ThemeLsArgs, ThemeNewArgs, ThemeValidateArgs,
};
use crate::error::CliError;

/// Version of every `--json` document of `hutan theme`.
const JSON_VERSION: u32 = 1;

pub(super) fn run(json: bool, command: ThemeCommand) -> Result<(), CliError> {
    match command {
        ThemeCommand::Ls(args) => ls(json, &args),
        ThemeCommand::Validate(args) => validate(json, &args),
        ThemeCommand::New(args) => new(json, &args),
        ThemeCommand::Export(args) => export_pack(json, &args),
    }
}

fn kind_of(kind: ThemeKind) -> PackKind {
    match kind {
        ThemeKind::Color => PackKind::Color,
        ThemeKind::Icon => PackKind::Icon,
    }
}

impl From<AuthorError> for CliError {
    fn from(error: AuthorError) -> Self {
        match error {
            AuthorError::Usage(message) => CliError::Usage(message),
            AuthorError::Failed(message) => CliError::Failed(message),
        }
    }
}

/// The data folder: where `settings.toml`, `themes/` and `icon-themes/` live.
fn data_dir() -> Result<PathBuf, CliError> {
    let paths = SettingsPaths::from_env().map_err(|e| CliError::Failed(e.message))?;
    paths
        .user
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CliError::Failed("cannot determine the data folder".to_owned()))
}

fn diagnostic_json(d: &SettingsError) -> Value {
    json!({
        "severity": d.severity.to_string(),
        "file": d.file.as_ref().map(|f| f.display().to_string()),
        "key": d.key_path,
        "message": d.message,
    })
}

fn appearance_name(appearance: SystemAppearance) -> &'static str {
    match appearance {
        SystemAppearance::Dark => "dark",
        SystemAppearance::Light => "light",
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

// ---- ls ---------------------------------------------------------------------------------------

/// The slots of the settings a pack is selected in.
fn selected_as(
    kind: PackKind,
    id: &str,
    appearance: &hutan_settings::AppearanceSettings,
) -> Vec<&'static str> {
    let mut slots = Vec::new();
    match kind {
        PackKind::Color => {
            if appearance.light_theme == id {
                slots.push("light");
            }
            if appearance.dark_theme == id {
                slots.push("dark");
            }
        }
        PackKind::Icon => {
            if appearance.icon_theme == id {
                slots.push("icon");
            }
        }
    }
    slots
}

fn ls(json: bool, args: &ThemeLsArgs) -> Result<(), CliError> {
    let kind = kind_of(args.kind);
    let data = data_dir()?;
    let registry = ThemeRegistry::new(&data);
    let (packs, diagnostics) = registry.listing(kind);
    let appearance = user_appearance(&data);
    if json {
        let themes: Vec<Value> = packs
            .iter()
            .map(|p| {
                json!({
                    "id": p.id,
                    "name": p.name,
                    "appearance": p.appearance.map(appearance_name),
                    "origin": p.origin.name(),
                    "valid": p.valid,
                    "kept_previous": p.kept_previous,
                    "selected_as": p.id.as_deref().map_or_else(Vec::new, |id| selected_as(kind, id, &appearance)),
                    "authors": p.authors,
                    "license": p.license,
                    "version": p.version,
                    "description": p.description,
                    "icons": p.icons,
                    "path": p.manifest.as_ref().map(|m| m.display().to_string()),
                    "diagnostics": p.diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let document = json!({
            "version": JSON_VERSION,
            "kind": kind.name(),
            "data_dir": data.display().to_string(),
            "themes": themes,
            "diagnostics": diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
        });
        return print_line(&document.to_string());
    }
    print_line(&listing_text(
        kind,
        &data,
        &packs,
        &diagnostics,
        &appearance,
    ))
}

fn listing_text(
    kind: PackKind,
    data: &Path,
    packs: &[PackInfo],
    diagnostics: &[SettingsError],
    appearance: &hutan_settings::AppearanceSettings,
) -> String {
    let rows: Vec<[String; 6]> = packs
        .iter()
        .map(|p| {
            let status = if !p.valid {
                format!("invalid ({})", plural(p.errors().count(), "error"))
            } else if p.kept_previous {
                "kept previous version".to_owned()
            } else {
                match p.warnings().count() {
                    0 => "ok".to_owned(),
                    n => format!("ok ({})", plural(n, "warning")),
                }
            };
            [
                p.id.clone().unwrap_or_else(|| format!("({})", p.name)),
                p.name.clone(),
                p.appearance.map_or("-", appearance_name).to_owned(),
                p.origin.name().to_owned(),
                status,
                p.id.as_deref()
                    .map(|id| selected_as(kind, id, appearance).join(","))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    let headers = ["ID", "NAME", "APPEARANCE", "ORIGIN", "STATUS", "SELECTED"];
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &[String; 6]| {
        let mut text = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                text.push_str(cell);
            } else {
                text.push_str(&format!("{cell:<width$}  ", width = widths[i]));
            }
        }
        text.trim_end().to_owned()
    };
    let mut out = format!(
        "{} themes in {} ({}/):\n",
        kind.name(),
        data.display(),
        kind.folder()
    );
    out.push_str(&line(&headers.map(str::to_owned)));
    for row in &rows {
        out.push('\n');
        out.push_str(&line(row));
    }
    if !diagnostics.is_empty() {
        out.push_str("\nproblems:");
        for d in diagnostics {
            out.push_str(&format!("\n  {d}"));
        }
    }
    out
}

// ---- validate ----------------------------------------------------------------------------------

fn validate(json: bool, args: &ThemeValidateArgs) -> Result<(), CliError> {
    let kind = kind_of(args.kind);
    let verdict = verdict_of(kind, &args.target)?;
    let (errors, warnings) = (verdict.errors(), verdict.warnings());
    let ok = errors == 0 && !(args.strict && warnings > 0);
    if json {
        let document = json!({
            "version": JSON_VERSION,
            "kind": kind.name(),
            "target": verdict.target,
            "id": verdict.id,
            "path": verdict.manifest.as_ref().map(|m| m.display().to_string()),
            "valid": errors == 0,
            "strict": args.strict,
            "ok": ok,
            "errors": errors,
            "warnings": warnings,
            "diagnostics": verdict.diagnostics.iter().map(diagnostic_json).collect::<Vec<_>>(),
        });
        print_line(&document.to_string())?;
    } else {
        print_line(&verdict_text(&verdict, args.strict))?;
    }
    if ok { Ok(()) } else { Err(CliError::Reported) }
}

/// Validates by path or by id (an id wins over a folder of that name in the current directory;
/// a target that is not an installed id but exists on disk is validated as a path).
fn verdict_of(kind: PackKind, target: &str) -> Result<Validation, CliError> {
    if looks_like_path(target) {
        return Ok(validate_path(kind, Path::new(target))?);
    }
    let registry = ThemeRegistry::new(&data_dir()?);
    match validate_installed(kind, &registry, target) {
        Err(AuthorError::Failed(_)) if Path::new(target).exists() => {
            Ok(validate_path(kind, Path::new(target))?)
        }
        other => Ok(other?),
    }
}

fn verdict_text(verdict: &Validation, strict: bool) -> String {
    let (errors, warnings) = (verdict.errors(), verdict.warnings());
    let what = match &verdict.id {
        Some(id) => format!("{} theme `{id}`", verdict.kind.name()),
        None => format!("{} theme {}", verdict.kind.name(), verdict.target),
    };
    let mut out = match (errors, warnings) {
        (0, 0) => format!("{what}: ok"),
        (0, w) if strict => format!("{what}: {} (--strict: failing)", plural(w, "warning")),
        (0, w) => format!("{what}: ok, {}", plural(w, "warning")),
        (e, w) => format!("{what}: {}, {}", plural(e, "error"), plural(w, "warning")),
    };
    for d in &verdict.diagnostics {
        out.push_str(&format!("\n  {d}"));
    }
    out
}

// ---- new ---------------------------------------------------------------------------------------

fn new(json: bool, args: &ThemeNewArgs) -> Result<(), CliError> {
    let kind = kind_of(args.kind);
    let parent = match &args.output {
        Some(output) => output.clone(),
        None => data_dir()?.join(kind.folder()),
    };
    let written = new_pack(
        &NewPack {
            kind,
            id: &args.id,
            name: &args.name,
            author: &args.author,
            license: &args.license,
            extends: args.extends.as_deref(),
        },
        &parent,
    )?;
    if json {
        let document = json!({
            "version": JSON_VERSION,
            "kind": kind.name(),
            "id": args.id,
            "dir": written.dir.display().to_string(),
            "manifest": written.manifest.display().to_string(),
            "files": written.files,
        });
        return print_line(&document.to_string());
    }
    let mut out = format!("created {}", written.dir.display());
    out.push_str(&file_lines(&written.files));
    out.push_str(&format!(
        "\nedit {}, then `hutan theme validate {}{}`; select it in Settings > Appearance",
        written.manifest.display(),
        if args.output.is_some() {
            written.dir.display().to_string()
        } else {
            args.id.clone()
        },
        if kind == PackKind::Icon {
            " --kind icon"
        } else {
            ""
        },
    ));
    print_line(&out)
}

// ---- export ------------------------------------------------------------------------------------

fn export_pack(json: bool, args: &ThemeExportArgs) -> Result<(), CliError> {
    let kind = kind_of(args.kind);
    let data = data_dir()?;
    let registry = ThemeRegistry::new(&data);
    let appearance = user_appearance(&data);
    let written = export(
        kind,
        &registry,
        &args.id,
        &args.output,
        &ExportOptions {
            resolved: args.resolved,
            rename: None,
            overrides: Some(&appearance.theme_overrides),
        },
    )?;
    if json {
        let document = json!({
            "version": JSON_VERSION,
            "kind": kind.name(),
            "id": args.id,
            "resolved": args.resolved,
            "dir": written.dir.display().to_string(),
            "manifest": written.manifest.display().to_string(),
            "files": written.files,
            "notes": written.notes,
        });
        return print_line(&document.to_string());
    }
    let mut out = format!("exported {} to {}", args.id, written.dir.display());
    out.push_str(&file_lines(&written.files));
    for note in &written.notes {
        out.push_str(&format!("\nnote: {note}"));
    }
    print_line(&out)
}

/// The files of a written pack, one per line; a long list (a flattened icon pack carries a
/// hundred SVGs) is cut after a few lines. `--json` lists them all.
fn file_lines(files: &[String]) -> String {
    const SHOWN: usize = 8;
    let mut out = String::new();
    for file in files.iter().take(SHOWN) {
        out.push_str(&format!("\n  {file}"));
    }
    if files.len() > SHOWN {
        out.push_str(&format!("\n  ... and {} more", files.len() - SHOWN));
    }
    out
}
