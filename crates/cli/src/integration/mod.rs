//! `hutan integration install | uninstall | status <codex|claude|omp>`: hooks and extensions
//! that make the coding agents report to the session they run in (`docs/integrations.md`).
//!
//! An install touches only what is Hutan's: its own script or extension file, and (Codex,
//! Claude Code) its own marked hook entries inside the agent's configuration. That file is
//! copied to `<name>.hutan-<unix ms>.bak` before it changes, entries of other tools stay
//! as they are, and an uninstall removes exactly what the install added.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::args::IntegrationTarget;
use crate::error::CliError;

mod hooks_json;
pub mod mcp;
mod statusline;

/// The script both hook-based integrations run (`integrations/hooks/`).
const HOOK_SCRIPT: &str = include_str!("../../../../integrations/hooks/hutan-agent-state.sh");
/// The OMP extension (`integrations/omp/`).
const OMP_EXTENSION: &str = include_str!("../../../../integrations/omp/hutan-agent-state.ts");
/// Claude Code's status-line tee (`integrations/hooks/`), opt-in.
const STATUSLINE_SCRIPT: &str = include_str!("../../../../integrations/hooks/hutan-statusline.sh");
/// Present in everything Hutan writes as a whole file; files without it are never overwritten.
const FILE_MARKER: &str = "hutan-managed";

const SCRIPT_NAME: &str = "hutan-agent-state.sh";
const EXTENSION_NAME: &str = "hutan-agent-state.ts";

/// Events Codex reports through (its permission prompts are `PermissionRequest`).
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
];
/// Events Claude Code reports through; subagents are nodes of the agent tree (`SubagentStart` /
/// `SubagentStop`, tool and permission hooks carrying an `agent_id`), `PostModelSwitch` updates
/// the model, `SessionEnd` releases.
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "PostModelSwitch",
    "Stop",
    "StopFailure",
    "SessionEnd",
    "SubagentStart",
    "SubagentStop",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Installed,
    /// Some hooks or the script are missing, or the script is outdated.
    Partial,
    NotInstalled,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub target: &'static str,
    /// The agent's configuration directory that was used.
    pub dir: PathBuf,
    pub status: Status,
    /// The operation changed something on disk.
    pub changed: bool,
    /// Files created, rewritten, or removed.
    pub files: Vec<PathBuf>,
    /// Copies of the agent's configuration made before editing it.
    pub backups: Vec<PathBuf>,
    /// What is missing when `status` is `partial`; also what the status-line tee lacks.
    pub missing: Vec<String>,
    /// Claude Code only: the opt-in status-line tee (`install claude --statusline`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statusline: Option<Status>,
    /// What the installed integration tells Hutan about the agent's insight (empty while it is
    /// not installed).
    pub reports: Vec<String>,
    /// What it cannot tell, and what would.
    pub not_reported: Vec<String>,
    /// What the agent's transcript adds and how Hutan finds it (`claude`, `codex`; empty for
    /// OMP): independent of the integration, which only makes the match certain.
    pub transcript: Vec<String>,
}

impl Report {
    fn new(target: IntegrationTarget, dir: &Path) -> Self {
        Self {
            target: name(target),
            dir: dir.to_owned(),
            status: Status::NotInstalled,
            changed: false,
            files: Vec::new(),
            backups: Vec::new(),
            missing: Vec::new(),
            statusline: None,
            reports: Vec::new(),
            not_reported: Vec::new(),
            transcript: Vec::new(),
        }
    }
}

pub fn name(target: IntegrationTarget) -> &'static str {
    match target {
        IntegrationTarget::Codex => "codex",
        IntegrationTarget::Claude => "claude",
        IntegrationTarget::Omp => "omp",
    }
}

/// The agent's configuration directory: `--dir`, else the agent's own environment variable,
/// else its default under `$HOME`.
pub fn config_dir(target: IntegrationTarget, explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(dir) = explicit {
        return Ok(std::path::absolute(dir)?);
    }
    let (variable, default): (&str, &[&str]) = match target {
        IntegrationTarget::Codex => ("CODEX_HOME", &[".codex"]),
        IntegrationTarget::Claude => ("CLAUDE_CONFIG_DIR", &[".claude"]),
        IntegrationTarget::Omp => ("PI_CODING_AGENT_DIR", &[".omp", "agent"]),
    };
    if let Some(dir) = std::env::var_os(variable).filter(|dir| !dir.is_empty()) {
        return Ok(std::path::absolute(PathBuf::from(dir))?);
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| {
            CliError::Usage(format!(
                "cannot tell where {} keeps its configuration: set {variable} or pass --dir",
                name(target)
            ))
        })?;
    Ok(default
        .iter()
        .fold(PathBuf::from(home), |dir, part| dir.join(part)))
}

/// Installs the hooks or the extension of `target`. With `statusline` (Claude Code only, opt-in)
/// also puts the status-line tee in place of the user's `statusLine`, keeping their command.
pub fn install(
    target: IntegrationTarget,
    dir: &Path,
    statusline: bool,
) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    if statusline && layout.statusline.is_none() {
        return Err(CliError::Usage(format!(
            "--statusline is for Claude Code's status line; {} has none",
            name(target)
        )));
    }
    let mut report = Report::new(target, dir);
    match &layout.config {
        Some(config) => {
            // Nothing is written before the configuration is known to be editable, and the
            // script goes in before any hook points at it.
            let command = hook_command(target, &layout.script);
            let events = layout.events;
            let tee = layout.statusline.as_ref().filter(|_| statusline);
            let tee_command = tee.map(|tee| statusline::command(&tee.script));
            let mut kept = None;
            let edit = plan_config(config, |doc| {
                let mut changed = hooks_json::install(doc, events, &command)?;
                if let Some(tee_command) = &tee_command {
                    let found = statusline::install(doc, tee_command)?;
                    changed |= found.changed;
                    kept = found.original;
                }
                Ok(changed)
            })?;
            write_managed(&layout.script, layout.script_text, 0o755, &mut report)?;
            if let Some(tee) = tee {
                // The user's own status line is on disk before the settings point at the tee.
                if let Some(original) = &kept {
                    write_kept(
                        &tee.command_file,
                        &statusline::command_text(original),
                        &mut report,
                    )?;
                    write_kept(
                        &tee.original_file,
                        &statusline::original_text(original),
                        &mut report,
                    )?;
                }
                write_managed(&tee.script, STATUSLINE_SCRIPT, 0o755, &mut report)?;
            }
            edit.apply(&mut report)?;
        }
        None => write_managed(&layout.script, layout.script_text, 0o644, &mut report)?,
    }
    finish_status(&layout, &mut report)?;
    Ok(report)
}

/// Removes what [`install`] added, the status-line tee among it (always, flag or not): the
/// user's own `statusLine` is put back as it was while the tee is still in place.
pub fn uninstall(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    let mut report = Report::new(target, dir);
    if let Some(config) = &layout.config {
        let tee = layout.statusline.as_ref();
        let original = match tee {
            Some(tee) => read_optional(&tee.original_file)?
                .as_deref()
                .and_then(statusline::read_original),
            None => None,
        };
        plan_config(config, |doc| {
            let mut changed = hooks_json::uninstall(doc)?;
            if tee.is_some() {
                changed |= statusline::uninstall(doc, original)?;
            }
            Ok(changed)
        })?
        .apply(&mut report)?;
    }
    remove_managed(&layout.script, &mut report)?;
    if let Some(tee) = &layout.statusline {
        remove_managed(&tee.script, &mut report)?;
        remove_kept(&tee.command_file, &mut report)?;
        remove_kept(&tee.original_file, &mut report)?;
    }
    finish_status(&layout, &mut report)?;
    Ok(report)
}

pub fn status(target: IntegrationTarget, dir: &Path) -> Result<Report, CliError> {
    let layout = Layout::of(target, dir);
    let mut report = Report::new(target, dir);
    finish_status(&layout, &mut report)?;
    Ok(report)
}

/// Where an integration puts things.
struct Layout {
    /// The agent's hooks file (Codex, Claude Code); `None` for OMP, which only loads a file.
    config: Option<PathBuf>,
    /// The script or extension Hutan owns.
    script: PathBuf,
    script_text: &'static str,
    events: &'static [&'static str],
    /// Claude Code's status-line tee (opt-in), beside the hook script.
    statusline: Option<Tee>,
}

/// The files of the status-line tee.
struct Tee {
    script: PathBuf,
    /// The user's own command, as text, which the script runs after Hutan's part.
    command_file: PathBuf,
    /// The user's own `statusLine` entry, whole, which an uninstall puts back.
    original_file: PathBuf,
}

impl Layout {
    fn of(target: IntegrationTarget, dir: &Path) -> Self {
        match target {
            IntegrationTarget::Codex => Self {
                config: Some(dir.join("hooks.json")),
                script: dir.join(SCRIPT_NAME),
                script_text: HOOK_SCRIPT,
                events: CODEX_EVENTS,
                statusline: None,
            },
            IntegrationTarget::Claude => Self {
                config: Some(dir.join("settings.json")),
                script: dir.join("hooks").join(SCRIPT_NAME),
                script_text: HOOK_SCRIPT,
                events: CLAUDE_EVENTS,
                statusline: Some(Tee {
                    script: dir.join("hooks").join(statusline::SCRIPT_NAME),
                    command_file: dir.join("hooks").join(statusline::COMMAND_FILE),
                    original_file: dir.join("hooks").join(statusline::ORIGINAL_FILE),
                }),
            },
            IntegrationTarget::Omp => Self {
                config: None,
                script: dir.join("extensions").join(EXTENSION_NAME),
                script_text: OMP_EXTENSION,
                events: &[],
                statusline: None,
            },
        }
    }
}

/// The hook command: a no-op builtin carrying the marker, then the script.
fn hook_command(target: IntegrationTarget, script: &Path) -> String {
    format!(
        ": {}{}; sh {} {}",
        hooks_json::MARKER,
        name(target),
        shell_quote(&script.to_string_lossy()),
        name(target)
    )
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Reads back what is on disk and sets `report.status` and `report.missing`.
fn finish_status(layout: &Layout, report: &mut Report) -> Result<(), CliError> {
    let mut missing = Vec::new();
    let mut present = 0usize;

    match read_optional(&layout.script)? {
        Some(text) if text == layout.script_text => present += 1,
        Some(text) if is_managed(&text) => {
            present += 1;
            missing.push(format!("{} is outdated", layout.script.display()));
        }
        Some(_) => missing.push(format!(
            "{} exists but is not Hutan's",
            layout.script.display()
        )),
        None => missing.push(format!("{} is missing", layout.script.display())),
    }
    let mut config_doc = None;
    if let Some(config) = &layout.config {
        let doc = read_json(config)?.unwrap_or_else(|| Value::Object(Default::default()));
        let installed = hooks_json::installed_events(&doc, layout.events);
        present += installed.len();
        for event in layout.events {
            if !installed.contains(event) {
                missing.push(format!("hook {event} is missing in {}", config.display()));
            }
        }
        config_doc = Some(doc);
    }
    report.status = if missing.is_empty() {
        Status::Installed
    } else if present == 0 {
        Status::NotInstalled
    } else {
        Status::Partial
    };
    report.statusline = None;
    let mut statusline_missing = Vec::new();
    if let (Some(tee), Some(doc)) = (&layout.statusline, &config_doc) {
        let entry = doc.get("statusLine").is_some_and(statusline::is_ours);
        let script = read_optional(&tee.script)?;
        let kept = tee.command_file.exists() && tee.original_file.exists();
        if script.is_some() || entry || kept {
            if script.as_deref() != Some(STATUSLINE_SCRIPT) {
                statusline_missing.push(format!(
                    "the status-line script {} is {}",
                    tee.script.display(),
                    if script.is_some() {
                        "outdated"
                    } else {
                        "missing"
                    }
                ));
            }
            if !entry {
                statusline_missing.push(format!(
                    "`statusLine` of {} is not Hutan's",
                    layout.config.as_deref().unwrap_or(&layout.script).display()
                ));
            }
            if !kept {
                statusline_missing.push(format!(
                    "the copy of your status line ({}) is missing",
                    tee.original_file.display()
                ));
            }
            report.statusline = Some(if statusline_missing.is_empty() {
                Status::Installed
            } else {
                Status::Partial
            });
        } else {
            report.statusline = Some(Status::NotInstalled);
        }
    }
    report.missing = if report.status == Status::Partial {
        missing
    } else {
        Vec::new()
    };
    report.missing.extend(statusline_missing);
    (report.reports, report.not_reported) = what_is_reported(
        report.target,
        report.status != Status::NotInstalled,
        report.statusline == Some(Status::Installed),
    );
    report.transcript = transcript_route(report.target, report.status != Status::NotInstalled);
    Ok(())
}

/// What the agent's transcript tells Hutan about its insight and how Hutan finds it (INSIGHT-C):
/// the route does not need an installed integration, but without its hooks Hutan can only guess by
/// folder. Empty for an agent that reports through an extension.
fn transcript_route(target: &str, hooks: bool) -> Vec<String> {
    let (agent, route, root) = match target {
        "claude" => (
            "Claude Code",
            if hooks {
                "the transcript path its hooks name (certain)"
            } else {
                "the only transcript of its folder written since it started, when no other session runs it there (likely: install its hooks to be certain)"
            },
            "~/.claude/projects",
        ),
        "codex" => (
            "Codex",
            if hooks {
                "the session id its hooks report, found in the history catalog (certain)"
            } else {
                "the only rollout of its folder written since it started, when no other session runs it there (likely: install its hooks to be certain)"
            },
            "~/.codex/sessions",
        ),
        _ => return Vec::new(),
    };
    vec![
        format!(
            "tokens per model call (input, output, cache), the context, the cost where {agent} writes one, tools with their times and failures, files changed, the model — read from its transcript under {root}"
        ),
        format!("route: {route}"),
        format!(
            "only when [history] has {agent} on (Settings → History, or `hutan index enable`), for the session you look at (one read a second, ending 30 s after you look away) or when a check asks"
        ),
    ]
}

/// What an installed integration tells Hutan about the agent's insight, and what it cannot
/// (`hutan integration status`): the one place that says so for every agent.
fn what_is_reported(target: &str, installed: bool, statusline: bool) -> (Vec<String>, Vec<String>) {
    if !installed {
        return (Vec::new(), Vec::new());
    }
    let words = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
    match target {
        "omp" => (
            words(&[
                "tokens, cost and the context after every model call (extension)",
                "the context split in fixed and messages (extension)",
                "tools with their times (extension)",
                "files the edit and write tools changed, from their arguments (extension)",
                "turns, from prompt to idle (extension)",
            ]),
            words(&["the context by category (OMP's /context stays in its own screen)"]),
        ),
        "claude" if statusline => (
            words(&[
                "tools with their times (hooks)",
                "files changed by Edit, Write, MultiEdit and NotebookEdit (hooks)",
                "turns, from prompt to stop (hooks)",
                "the context size, the cost and the API time (status line)",
            ]),
            words(&[
                "tokens per turn (hooks and the status line carry none; its transcript does)",
                "the context by category (only after you ran /context in it: its transcript records that)",
            ]),
        ),
        "claude" => (
            words(&[
                "tools with their times (hooks)",
                "files changed by Edit, Write, MultiEdit and NotebookEdit (hooks)",
                "turns, from prompt to stop (hooks)",
            ]),
            words(&[
                "the context size, the cost and the API time (install with --statusline, or turn on Claude Code history)",
                "tokens per turn (hooks carry none; its transcript does)",
                "the context by category (only after you ran /context in it: its transcript records that)",
            ]),
        ),
        "codex" => (
            words(&[
                "tools (hooks)",
                "files changed by apply_patch (hooks)",
                "turns, from prompt to stop (hooks)",
            ]),
            words(&[
                "tool times, tokens, the context size and the cost (Codex's hooks carry none; its transcript has all but the cost)",
            ]),
        ),
        _ => (Vec::new(), Vec::new()),
    }
}

// ---- files ------------------------------------------------------------------------------------

fn is_managed(text: &str) -> bool {
    text.lines().take(3).any(|line| line.contains(FILE_MARKER))
}

fn io_error(path: &Path, error: io::Error) -> CliError {
    CliError::Failed(format!("{}: {error}", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<String>, CliError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(path, e)),
    }
}

fn read_json(path: &Path) -> Result<Option<Value>, CliError> {
    read_optional(path)?
        .map(|text| parse_json(path, &text))
        .transpose()
}

/// An empty file counts as an empty document.
fn parse_json(path: &Path, text: &str) -> Result<Value, CliError> {
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(text).map_err(|e| {
        CliError::Failed(format!(
            "{} is not valid JSON ({e}); not touching it",
            path.display()
        ))
    })
}

/// Writes a whole file Hutan owns (script, extension). Refuses to replace one that is not.
fn write_managed(path: &Path, text: &str, mode: u32, report: &mut Report) -> Result<(), CliError> {
    if let Some(existing) = read_optional(path)? {
        if existing == text {
            return Ok(());
        }
        if !is_managed(&existing) {
            return Err(CliError::Failed(format!(
                "{} exists and was not written by Hutan; move it away first",
                path.display()
            )));
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    write_atomic(path, text.as_bytes(), Some(mode))?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

fn remove_managed(path: &Path, report: &mut Report) -> Result<(), CliError> {
    let Some(existing) = read_optional(path)? else {
        return Ok(());
    };
    if !is_managed(&existing) {
        return Ok(());
    }
    fs::remove_file(path).map_err(|e| io_error(path, e))?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

/// Writes a file that holds something of the user's (their status-line command), replacing what
/// an earlier install kept there.
fn write_kept(path: &Path, text: &str, report: &mut Report) -> Result<(), CliError> {
    if read_optional(path)?.as_deref() == Some(text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
    }
    write_atomic(path, text.as_bytes(), Some(0o600))?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

/// Removes a file [`write_kept`] wrote (after the uninstall put its content back).
fn remove_kept(path: &Path, report: &mut Report) -> Result<(), CliError> {
    match fs::remove_file(path) {
        Ok(()) => {
            report.changed = true;
            report.files.push(path.to_owned());
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, error)),
    }
}

/// An edit of an agent's JSON configuration, worked out but not yet written.
struct ConfigEdit {
    path: PathBuf,
    /// The file as it is now; `None` if it does not exist.
    original: Option<String>,
    doc: Value,
    changed: bool,
}

/// Applies `edit` to the JSON document at `path` (an empty one if the file is missing)
/// without writing anything.
fn plan_config(
    path: &Path,
    edit: impl FnOnce(&mut Value) -> Result<bool, hooks_json::EditError>,
) -> Result<ConfigEdit, CliError> {
    let original = read_optional(path)?;
    let mut doc = match &original {
        Some(text) => parse_json(path, text)?,
        None => Value::Object(serde_json::Map::new()),
    };
    let changed = edit(&mut doc)
        .map_err(|e| CliError::Failed(format!("{}: {e}; not touching it", path.display())))?;
    Ok(ConfigEdit {
        path: path.to_owned(),
        original,
        doc,
        changed,
    })
}

impl ConfigEdit {
    /// Backs the file up and writes the document with the file's own indentation. An edit that
    /// changed nothing writes nothing; a document an uninstall leaves empty removes the file.
    fn apply(self, report: &mut Report) -> Result<(), CliError> {
        let Self {
            path,
            original,
            doc,
            changed,
        } = self;
        if !changed {
            return Ok(());
        }
        if let Some(text) = &original {
            report.backups.push(backup(&path, text)?);
        }
        if doc.as_object().is_some_and(serde_json::Map::is_empty) && original.is_some() {
            fs::remove_file(&path).map_err(|e| io_error(&path, e))?;
        } else {
            let text = render_json(&doc, original.as_deref())?;
            let mode = fs::metadata(&path).ok().map(|meta| {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o777
            });
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
            }
            write_atomic(&path, text.as_bytes(), mode)?;
        }
        report.changed = true;
        report.files.push(path);
        Ok(())
    }
}

/// The document as text, indented like `original` (two spaces for a new file) with its trailing
/// newline.
fn render_json(doc: &Value, original: Option<&str>) -> Result<String, CliError> {
    use serde::Serialize as _;
    let (indent, newline) = match original {
        Some(text) if !text.trim().is_empty() => (detect_indent(text), text.ends_with('\n')),
        _ => (Some("  ".to_owned()), true),
    };
    let mut out = Vec::new();
    match indent {
        Some(indent) => {
            let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
            let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
            doc.serialize(&mut serializer)
                .map_err(|e| CliError::Failed(e.to_string()))?;
        }
        None => {
            serde_json::to_writer(&mut out, doc).map_err(|e| CliError::Failed(e.to_string()))?
        }
    }
    if newline {
        out.push(b'\n');
    }
    String::from_utf8(out).map_err(|e| CliError::Failed(e.to_string()))
}

/// The indentation of the first indented line; `None` for a document on a single line.
fn detect_indent(text: &str) -> Option<String> {
    let mut lines = text.trim_end().lines();
    lines.next();
    let mut any = false;
    for line in lines {
        any = true;
        let indent = &line[..line.len() - line.trim_start().len()];
        if !indent.is_empty() {
            return Some(indent.to_owned());
        }
    }
    // Several lines but no indentation: keep the default; one line: stay compact.
    any.then(|| "  ".to_owned())
}

/// Copies `path` to a fresh `<name>.hutan-<unix ms>.bak` next to it.
fn backup(path: &Path, contents: &str) -> Result<PathBuf, CliError> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for attempt in 0u32..1000 {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = path.with_file_name(format!("{file_name}.hutan-{stamp}{suffix}.bak"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(contents.as_bytes())
                    .map_err(|e| io_error(&candidate, e))?;
                if let Ok(meta) = fs::metadata(path) {
                    let _ = fs::set_permissions(&candidate, meta.permissions());
                }
                return Ok(candidate);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_error(&candidate, e)),
        }
    }
    Err(CliError::Failed(format!(
        "no free backup name next to {}",
        path.display()
    )))
}

/// Replaces `path` through a temporary file in the same directory, so the agent never reads a
/// half-written configuration.
fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), CliError> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{file_name}.hutan-tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Some(mode) = mode {
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| io_error(path, e))
}

/// Human text for a report.
pub fn describe(action: &str, report: &Report) -> String {
    let mut text = match (action, report.status) {
        ("install", _) if report.changed => {
            format!("{}: installed in {}", report.target, report.dir.display())
        }
        ("install", _) => format!(
            "{}: already installed in {}",
            report.target,
            report.dir.display()
        ),
        ("uninstall", _) if report.changed => {
            format!("{}: removed from {}", report.target, report.dir.display())
        }
        ("uninstall", _) => format!(
            "{}: nothing to remove in {}",
            report.target,
            report.dir.display()
        ),
        (_, Status::Installed) => {
            format!("{}: installed ({})", report.target, report.dir.display())
        }
        (_, Status::NotInstalled) => {
            format!(
                "{}: not installed ({})",
                report.target,
                report.dir.display()
            )
        }
        (_, Status::Partial) => format!(
            "{}: partially installed ({})",
            report.target,
            report.dir.display()
        ),
    };
    for backup in &report.backups {
        text.push_str(&format!("\n  backup: {}", backup.display()));
    }
    for item in &report.missing {
        text.push_str(&format!("\n  {item}"));
    }
    if action == "status" || action == "install" {
        for item in &report.reports {
            text.push_str(&format!("\n  reports: {item}"));
        }
        for item in &report.not_reported {
            text.push_str(&format!("\n  not reported: {item}"));
        }
        for item in &report.transcript {
            text.push_str(&format!("\n  transcript: {item}"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indentation_and_trailing_newline_follow_the_original() {
        assert_eq!(detect_indent("{\n  \"a\": 1\n}\n").as_deref(), Some("  "));
        assert_eq!(detect_indent("{\n\t\"a\": 1\n}").as_deref(), Some("\t"));
        assert_eq!(
            detect_indent("{\n    \"a\": {\n      \"b\": 1\n    }\n}").as_deref(),
            Some("    ")
        );
        assert_eq!(detect_indent("{\"a\":1}\n"), None);
        assert_eq!(detect_indent("{\n\"a\": 1\n}").as_deref(), Some("  "));
    }

    #[test]
    fn script_paths_are_quoted_for_sh() {
        assert_eq!(
            shell_quote("/home/me/.codex/x.sh"),
            "'/home/me/.codex/x.sh'"
        );
        assert_eq!(shell_quote("/home/o'neil/x.sh"), "'/home/o'\\''neil/x.sh'");
    }

    #[test]
    fn embedded_files_carry_the_ownership_marker() {
        assert!(is_managed(HOOK_SCRIPT));
        assert!(is_managed(OMP_EXTENSION));
        assert!(!is_managed("#!/bin/sh\necho hi\n"));
    }
}
