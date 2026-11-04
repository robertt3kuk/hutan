//! Switching on the agent's own OpenTelemetry for Hutan's loopback receiver (TEL-2, ADR 0026
//! decision 8; `docs/integrations.md` § L3).
//!
//! Two routes, both **launch-scoped** and neither one overrides anything:
//!
//! - **A shell function** (`claude`, `codex`) of Hutan's shell integration asks
//!   `hutan telemetry env <agent>` (this module's [`launch`]) what to put in front of *that one
//!   command*. The answer is nothing as soon as the agent is not enabled, the terminal has no
//!   `HUTAN_OTLP_*`, or any OpenTelemetry setting of the user, a managed policy or the agent's own
//!   configuration is already there ([`conflicts`]).
//! - **`hutan integration install claude|codex --telemetry`** for an agent started where no
//!   function is (a script, `npx`, an editor's task): Claude Code gets `env` and
//!   `otelHeadersHelper` in the user's `settings.json`, Codex an `[otel]` block with a log
//!   exporter only in `config.toml`. What is Hutan's is marked, an uninstall removes exactly it.
//!
//! The token of a session never reaches a file: Claude's helper script reads `HUTAN_OTLP_TOKEN`
//! from the process that runs it; Codex's configuration cannot hold a per-session value (its
//! `headers` are literal), so the token comes from `OTEL_EXPORTER_OTLP_LOGS_HEADERS`, which the
//! shell function sets for that one command.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};
use toml_edit::{DocumentMut, Item};

use super::hooks_json::EditError;
use super::{Status, is_managed, read_optional, write_atomic, write_managed};
use crate::args::IntegrationTarget;
use crate::error::CliError;

/// The agents this slice can switch on (the others are TEL-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn of(target: IntegrationTarget) -> Option<Self> {
        match target {
            IntegrationTarget::Claude => Some(Self::Claude),
            IntegrationTarget::Codex => Some(Self::Codex),
            IntegrationTarget::Omp => None,
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn target(self) -> IntegrationTarget {
        match self {
            Self::Claude => IntegrationTarget::Claude,
            Self::Codex => IntegrationTarget::Codex,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// What `hutan integration status` says of the telemetry of one agent.
#[derive(Debug, Serialize)]
pub struct TelemetryReport {
    /// Hutan's own configuration in the agent's files: `installed`, `partial` (something of it is
    /// missing or names another port) or `not_installed`.
    pub status: Status,
    /// Settings that already name an OpenTelemetry destination or exporter and that Hutan
    /// therefore leaves alone, each with where it is. While there is one, neither the install nor
    /// the shell function changes anything.
    pub conflicts: Vec<String>,
    /// What is missing when `status` is `partial`, and what to do.
    pub missing: Vec<String>,
    /// How the agent reports in a Hutan terminal now.
    pub notes: Vec<String>,
}

// ---- where things are --------------------------------------------------------------------------

/// The folders managed policies live in; `HUTAN_MANAGED_CONFIG_ROOT` replaces the system ones for
/// tests (`<root>/ClaudeCode`, `<root>/codex`).
#[derive(Debug, Clone)]
pub struct System {
    claude: PathBuf,
    codex: PathBuf,
}

impl System {
    pub fn from_env() -> Self {
        match std::env::var_os("HUTAN_MANAGED_CONFIG_ROOT").filter(|root| !root.is_empty()) {
            Some(root) => {
                let root = PathBuf::from(root);
                Self {
                    claude: root.join("ClaudeCode"),
                    codex: root.join("codex"),
                }
            }
            None => Self {
                claude: PathBuf::from("/Library/Application Support/ClaudeCode"),
                codex: PathBuf::from("/etc/codex"),
            },
        }
    }

    #[cfg(test)]
    pub fn in_root(root: &Path) -> Self {
        Self {
            claude: root.join("ClaudeCode"),
            codex: root.join("codex"),
        }
    }
}

/// Where the receiver listens, from `daemon.toml`: only the port is persisted (ADR 0026), the
/// token never is.
pub fn persisted_port() -> Option<u16> {
    let paths = hutan_settings::SettingsPaths::from_env().ok()?;
    let file = paths.user.parent()?.join("daemon.toml");
    let text = fs::read_to_string(file).ok()?;
    let doc = text.parse::<DocumentMut>().ok()?;
    let port = doc.get("telemetry")?.get("port")?.as_integer()?;
    u16::try_from(port).ok().filter(|port| *port != 0)
}

/// Whether `[telemetry.agents] <agent>` of `daemon.toml` is on.
pub fn persisted_enabled(agent: Agent) -> bool {
    let Ok(paths) = hutan_settings::SettingsPaths::from_env() else {
        return false;
    };
    let Some(file) = paths.user.parent().map(|dir| dir.join("daemon.toml")) else {
        return false;
    };
    let Ok(text) = fs::read_to_string(file) else {
        return false;
    };
    text.parse::<DocumentMut>().ok().is_some_and(|doc| {
        doc.get("telemetry")
            .and_then(|telemetry| telemetry.get("agents"))
            .and_then(|agents| agents.get(agent.id()))
            .and_then(Item::as_bool)
            == Some(true)
    })
}

/// `http://127.0.0.1:<port>`: what an agent's configuration file names (no token in it).
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

// ---- conflicts ---------------------------------------------------------------------------------

/// What Claude Code reads that decides its OpenTelemetry. Hutan writes four of them and no other.
const CLAUDE_VARIABLES: &[&str] = &[
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "OTEL_LOGS_EXPORTER",
    "OTEL_METRICS_EXPORTER",
    "OTEL_TRACES_EXPORTER",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
];

/// What Codex honours from the environment: the headers a shell function would set.
const CODEX_VARIABLES: &[&str] = &[
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
];

/// The `env` keys of Claude's settings that Hutan writes, in this order.
const CLAUDE_OWN_KEYS: &[(&str, &str)] = &[
    ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
    ("OTEL_LOGS_EXPORTER", "otlp"),
    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
    // The endpoint is written from the port.
];
/// Claude Code's content gates, set off in everything Hutan writes or injects (defence in depth:
/// the receiver reads an allowlist of attributes whatever the agent sends).
const CLAUDE_GATES: &[&str] = &[
    "OTEL_LOG_USER_PROMPTS",
    "OTEL_LOG_TOOL_DETAILS",
    "OTEL_LOG_TOOL_CONTENT",
    "OTEL_LOG_RAW_API_BODIES",
    "OTEL_LOG_ASSISTANT_RESPONSES",
];
/// Marks the `env` of a settings file as carrying Hutan's keys.
const CLAUDE_MARKER_KEY: &str = "HUTAN_TELEMETRY_ENTRY";
const CLAUDE_MARKER: &str = "hutan-managed";
/// The logs of Claude Code are exported every two seconds: live enough, a request a few seconds.
const CLAUDE_LOGS_INTERVAL_MS: &str = "2000";

pub const HELPER_NAME: &str = "hutan-otel-headers.sh";
const HELPER_SCRIPT: &str = include_str!("../../../../integrations/hooks/hutan-otel-headers.sh");

/// Where Claude's header helper goes.
pub fn helper_path(dir: &Path) -> PathBuf {
    dir.join("hooks").join(HELPER_NAME)
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

fn codex_config_path(dir: &Path) -> PathBuf {
    dir.join("config.toml")
}

/// Everything that stops Hutan from touching the agent's OpenTelemetry. Each entry says what and
/// where, never a value (a header may hold a secret).
#[derive(Debug, Default)]
pub struct Conflicts {
    pub items: Vec<String>,
    /// Hutan's own configuration is in the agent's files (never a conflict).
    pub installed: bool,
}

fn json_settings(path: &Path) -> Result<Option<Value>, CliError> {
    let Some(text) = read_optional(path)? else {
        return Ok(None);
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| CliError::Failed(format!("{}: {e}; not touching it", path.display())))
}

fn is_ours_env(env: &Map<String, Value>) -> bool {
    env.get(CLAUDE_MARKER_KEY).and_then(Value::as_str) == Some(CLAUDE_MARKER)
}

/// The settings files of Claude's managed policy: `managed-settings.json` and the drop-ins of
/// `managed-settings.d/`.
fn claude_managed_files(system: &System) -> Vec<PathBuf> {
    let mut files = vec![system.claude.join("managed-settings.json")];
    if let Ok(entries) = fs::read_dir(system.claude.join("managed-settings.d")) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        found.sort();
        files.extend(found);
    }
    files
}

/// Every variable that decides Claude Code's OpenTelemetry or is one Hutan sets.
fn claude_keys() -> impl Iterator<Item = &'static str> {
    CLAUDE_VARIABLES
        .iter()
        .chain(CLAUDE_GATES)
        .chain(&["OTEL_LOGS_EXPORT_INTERVAL"])
        .copied()
}

/// Whether `value` of `key` is in the way of what Hutan would set. A content gate that is off is
/// what Hutan wants too.
fn is_in_the_way(key: &str, value: &str) -> bool {
    !(CLAUDE_GATES.contains(&key) && matches!(value.trim(), "" | "0" | "false"))
}

/// Whether Hutan writes `key` into the `env` it owns.
fn written_by_hutan(key: &str) -> bool {
    CLAUDE_OWN_KEYS.iter().any(|(own, _)| *own == key)
        || CLAUDE_GATES.contains(&key)
        || matches!(
            key,
            "OTEL_EXPORTER_OTLP_ENDPOINT" | "OTEL_LOGS_EXPORT_INTERVAL"
        )
}

/// What in a Claude settings document decides OpenTelemetry (not Hutan's own).
fn claude_settings_conflicts(doc: &Value, where_: &str, ours_allowed: bool, out: &mut Vec<String>) {
    let env = doc.get("env").and_then(Value::as_object);
    let ours = ours_allowed && env.is_some_and(is_ours_env);
    if let Some(env) = env {
        for key in claude_keys() {
            let Some(value) = env.get(key) else {
                continue;
            };
            let text = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            if !(ours && written_by_hutan(key)) && is_in_the_way(key, &text) {
                out.push(format!("{where_} sets {key} in `env`"));
            }
        }
    }
    if let Some(helper) = doc.get("otelHeadersHelper") {
        let ours_helper = ours
            && helper.as_str().is_some_and(|text| {
                Path::new(text).ends_with(Path::new("hooks").join(HELPER_NAME))
            });
        if !ours_helper {
            out.push(format!("{where_} sets `otelHeadersHelper`"));
        }
    }
}

/// The conflicts of `agent` with the files under `dir`, the managed policy in `system` and the
/// environment `env` (the shell's, as the agent would inherit it).
pub fn conflicts(
    agent: Agent,
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    system: &System,
) -> Result<Conflicts, CliError> {
    let mut found = Conflicts::default();
    let present = |name: &str| env(name).is_some_and(|value| !value.is_empty());
    match agent {
        Agent::Claude => {
            for key in claude_keys() {
                if let Some(value) = env(key)
                    && is_in_the_way(key, &value)
                    && !value.is_empty()
                {
                    found
                        .items
                        .push(format!("{key} is set in the environment of this shell"));
                }
            }
            let user = settings_path(dir);
            if let Some(doc) = json_settings(&user)? {
                found.installed = doc
                    .get("env")
                    .and_then(Value::as_object)
                    .is_some_and(is_ours_env);
                claude_settings_conflicts(
                    &doc,
                    &user.display().to_string(),
                    true,
                    &mut found.items,
                );
            }
            for file in claude_managed_files(system) {
                if let Some(doc) = json_settings(&file)? {
                    claude_settings_conflicts(
                        &doc,
                        &format!("the managed policy {}", file.display()),
                        false,
                        &mut found.items,
                    );
                }
            }
        }
        Agent::Codex => {
            for key in CODEX_VARIABLES {
                if present(key) {
                    found
                        .items
                        .push(format!("{key} is set in the environment of this shell"));
                }
            }
            let user = codex_config_path(dir);
            if let Some(text) = read_optional(&user)? {
                found.installed = find_block(&text).is_some();
                let outside = remove_block(&text).map_or_else(|| text.clone(), |(rest, _)| rest);
                if let Ok(doc) = outside.parse::<DocumentMut>()
                    && doc.contains_key("otel")
                {
                    found
                        .items
                        .push(format!("{} already has an [otel] table", user.display()));
                }
            }
            for name in ["config.toml", "managed_config.toml", "requirements.toml"] {
                let file = system.codex.join(name);
                if let Some(text) = read_optional(&file)?
                    && text
                        .parse::<DocumentMut>()
                        .is_ok_and(|doc| doc.contains_key("otel"))
                {
                    found.items.push(format!(
                        "the managed policy {} has an [otel] table",
                        file.display()
                    ));
                }
            }
        }
    }
    Ok(found)
}

// ---- what a shell function launches with ---------------------------------------------------------

/// What goes in front of one launch of an agent.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Launch {
    /// `NAME=value` for `env`.
    pub env: Vec<(String, String)>,
    /// Arguments in front of the user's.
    pub args: Vec<String>,
}

/// `http://127.0.0.1:<port>`, from a session's `HUTAN_OTLP_ENDPOINT` (`…/s/<token>`).
fn base_of(endpoint: &str) -> &str {
    endpoint.find("/s/").map_or(endpoint, |at| &endpoint[..at])
}

/// How to launch `agent` so that it reports to this terminal's session, or `None`: nothing to add
/// (a conflict, or Hutan's own configuration in the agent's files already does it).
///
/// `endpoint` and `token` are `HUTAN_OTLP_ENDPOINT` and `HUTAN_OTLP_TOKEN` of the terminal.
pub fn launch(agent: Agent, found: &Conflicts, endpoint: &str, token: &str) -> Option<Launch> {
    if !found.items.is_empty() {
        return None;
    }
    let mut launch = Launch::default();
    match agent {
        Agent::Claude => {
            if found.installed {
                // The settings carry the endpoint, the helper the token: nothing to add.
                return None;
            }
            for (key, value) in CLAUDE_OWN_KEYS {
                launch.env.push(((*key).to_owned(), (*value).to_owned()));
            }
            launch.env.push((
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(),
                endpoint.to_owned(),
            ));
            launch.env.push((
                "OTEL_LOGS_EXPORT_INTERVAL".to_owned(),
                CLAUDE_LOGS_INTERVAL_MS.to_owned(),
            ));
            for gate in CLAUDE_GATES {
                launch.env.push(((*gate).to_owned(), "0".to_owned()));
            }
        }
        Agent::Codex => {
            // The token is a header from the environment: a path or an argument would show it
            // in `ps`, and the configuration file cannot hold a per-session value.
            launch.env.push((
                "OTEL_EXPORTER_OTLP_LOGS_HEADERS".to_owned(),
                format!("x-hutan-token={token}"),
            ));
            if !found.installed {
                let logs = format!("{}/v1/logs", base_of(endpoint));
                for setting in [
                    format!(
                        "otel.exporter={{otlp-http={{endpoint=\"{logs}\",protocol=\"binary\"}}}}"
                    ),
                    "otel.trace_exporter=\"none\"".to_owned(),
                    "otel.metrics_exporter=\"none\"".to_owned(),
                    "otel.log_user_prompt=false".to_owned(),
                ] {
                    launch.args.push("-c".to_owned());
                    launch.args.push(setting);
                }
            }
        }
    }
    Some(launch)
}

// ---- Claude Code: settings.json ---------------------------------------------------------------

/// Puts Hutan's keys into `doc`: `env` (with the marker) and `otelHeadersHelper`. The caller has
/// checked that there are no conflicts; what is Hutan's is brought up to date.
pub fn claude_set(doc: &mut Value, port: u16, helper: &Path) -> Result<bool, EditError> {
    let root = doc.as_object_mut().ok_or(EditError::NotAnObject)?;
    let before = root.clone();
    let env = root
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| EditError::UnexpectedShape("env".to_owned()))?;
    let mut wanted: Vec<(&str, String)> = vec![(CLAUDE_MARKER_KEY, CLAUDE_MARKER.to_owned())];
    for (key, value) in CLAUDE_OWN_KEYS {
        wanted.push((key, (*value).to_owned()));
    }
    wanted.push(("OTEL_EXPORTER_OTLP_ENDPOINT", base_url(port)));
    wanted.push((
        "OTEL_LOGS_EXPORT_INTERVAL",
        CLAUDE_LOGS_INTERVAL_MS.to_owned(),
    ));
    for gate in CLAUDE_GATES {
        wanted.push((gate, "0".to_owned()));
    }
    for (key, value) in wanted {
        if env.get(key).and_then(Value::as_str) != Some(value.as_str()) {
            env.insert(key.to_owned(), json!(value));
        }
    }
    let helper = helper.to_string_lossy();
    if root.get("otelHeadersHelper").and_then(Value::as_str) != Some(helper.as_ref()) {
        root.insert("otelHeadersHelper".to_owned(), json!(helper));
    }
    Ok(*root != before)
}

/// Removes what [`claude_set`] added, and `env` itself when that leaves it empty.
pub fn claude_remove(doc: &mut Value) -> Result<bool, EditError> {
    let Some(root) = doc.as_object_mut() else {
        return Ok(false);
    };
    let ours = root
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(is_ours_env);
    if !ours {
        return Ok(false);
    }
    if let Some(env) = root.get_mut("env").and_then(Value::as_object_mut) {
        env.remove(CLAUDE_MARKER_KEY);
        for (key, _) in CLAUDE_OWN_KEYS {
            env.remove(*key);
        }
        env.remove("OTEL_EXPORTER_OTLP_ENDPOINT");
        env.remove("OTEL_LOGS_EXPORT_INTERVAL");
        for gate in CLAUDE_GATES {
            env.remove(*gate);
        }
        if env.is_empty() {
            root.remove("env");
        }
    }
    if root
        .get("otelHeadersHelper")
        .and_then(Value::as_str)
        .is_some_and(|text| Path::new(text).ends_with(Path::new("hooks").join(HELPER_NAME)))
    {
        root.remove("otelHeadersHelper");
    }
    Ok(true)
}

/// The helper script, written before `settings.json` points at it.
pub fn write_helper(dir: &Path, report: &mut super::Report) -> Result<(), CliError> {
    write_managed(&helper_path(dir), HELPER_SCRIPT, 0o755, report)
}

pub fn remove_helper(dir: &Path, report: &mut super::Report) -> Result<(), CliError> {
    super::remove_managed(&helper_path(dir), report)
}

// ---- Codex: config.toml ------------------------------------------------------------------------

const BLOCK_BEGIN: &str = "# >>> hutan telemetry";
const BLOCK_END: &str = "# <<< hutan telemetry";
/// On the begin line when Hutan had to add the final newline of the file.
const NO_EOL: &str = " (no-eol)";

fn block(port: u16, no_eol: bool) -> String {
    format!(
        "{BLOCK_BEGIN} (hutan-managed; `hutan integration uninstall codex` removes this block){}\n\
         [otel]\n\
         exporter = {{ otlp-http = {{ endpoint = \"{}/v1/logs\", protocol = \"binary\" }} }}\n\
         trace_exporter = \"none\"\n\
         metrics_exporter = \"none\"\n\
         log_user_prompt = false\n\
         {BLOCK_END}\n",
        if no_eol { NO_EOL } else { "" },
        base_url(port)
    )
}

/// Where Hutan's block starts and ends (end exclusive), as line offsets into `text`.
fn find_block(text: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut begin = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if begin.is_none() && trimmed.starts_with(BLOCK_BEGIN) {
            begin = Some(offset);
        } else if begin.is_some() && trimmed == BLOCK_END {
            return begin.map(|start| (start, offset + line.len()));
        }
        offset += line.len();
    }
    None
}

/// `text` without Hutan's block, and the blank line and final newline Hutan added around it:
/// byte for byte what the file was. `None` when there is no block.
fn remove_block(text: &str) -> Option<(String, bool)> {
    let (start, end) = find_block(text)?;
    let begin_line = text[start..].lines().next().unwrap_or_default();
    let no_eol = begin_line.ends_with(NO_EOL);
    let mut head = &text[..start];
    // The one blank line that came with the block (only when the file had content).
    if head.ends_with("\n\n") {
        head = &head[..head.len() - 1];
    }
    let mut rest = format!("{head}{}", &text[end..]);
    if no_eol && rest.ends_with('\n') {
        rest.pop();
    }
    Some((rest, no_eol))
}

/// Writes (or brings up to date) Hutan's `[otel]` block in `config.toml`. The caller has checked
/// that no `[otel]` table of the user's is in the way.
pub fn codex_install(dir: &Path, port: u16, report: &mut super::Report) -> Result<(), CliError> {
    let path = codex_config_path(dir);
    let original = read_optional(&path)?;
    let base = match &original {
        Some(text) => remove_block(text).map_or_else(|| text.clone(), |(rest, _)| rest),
        None => String::new(),
    };
    let no_eol = !base.is_empty() && !base.ends_with('\n');
    let mut text = base.clone();
    if no_eol {
        text.push('\n');
    }
    if !base.is_empty() {
        text.push('\n');
    }
    text.push_str(&block(port, no_eol));
    text.parse::<DocumentMut>().map_err(|e| {
        CliError::Failed(format!(
            "{}: the result would not be valid TOML ({e}); not touching it",
            path.display()
        ))
    })?;
    if original.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    if let Some(original) = &original {
        report.backups.push(super::backup(&path, original)?);
    }
    write_text(&path, &text, report)
}

/// Removes Hutan's block and what came with it; a file that held nothing else goes away.
pub fn codex_uninstall(dir: &Path, report: &mut super::Report) -> Result<(), CliError> {
    let path = codex_config_path(dir);
    let Some(original) = read_optional(&path)? else {
        return Ok(());
    };
    let Some((rest, _)) = remove_block(&original) else {
        return Ok(());
    };
    report.backups.push(super::backup(&path, &original)?);
    if rest.is_empty() {
        fs::remove_file(&path).map_err(|e| super::io_error(&path, e))?;
        report.changed = true;
        report.files.push(path);
        return Ok(());
    }
    write_text(&path, &rest, report)
}

fn write_text(path: &Path, text: &str, report: &mut super::Report) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| super::io_error(parent, e))?;
    }
    let mode = fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o777);
    write_atomic(path, text.as_bytes(), mode)?;
    report.changed = true;
    report.files.push(path.to_owned());
    Ok(())
}

// ---- status ------------------------------------------------------------------------------------

/// The telemetry part of `hutan integration status` for `agent`.
pub fn report(
    agent: Agent,
    dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    system: &System,
    port: Option<u16>,
    enabled: bool,
) -> Result<TelemetryReport, CliError> {
    let found = conflicts(agent, dir, env, system)?;
    let mut missing = Vec::new();
    let mut present = 0usize;
    let mut wanted = 0usize;
    if found.installed {
        match agent {
            Agent::Claude => {
                wanted = 2;
                present = 1;
                let helper = helper_path(dir);
                match read_optional(&helper)? {
                    Some(text) if text == HELPER_SCRIPT => present += 1,
                    Some(text) if is_managed(&text) => {
                        missing.push(format!("{} is outdated", helper.display()));
                    }
                    Some(_) => {
                        missing.push(format!("{} exists but is not Hutan's", helper.display()))
                    }
                    None => missing.push(format!("{} is missing", helper.display())),
                }
                let settings = json_settings(&settings_path(dir))?.unwrap_or_default();
                let at = settings
                    .pointer("/env/OTEL_EXPORTER_OTLP_ENDPOINT")
                    .and_then(Value::as_str);
                if let (Some(at), Some(port)) = (at, port)
                    && at != base_url(port)
                {
                    present = present.saturating_sub(1);
                    missing.push(format!(
                        "settings.json sends to {at}, the receiver listens on {}: run \
                         `hutan integration install claude --telemetry` again",
                        base_url(port)
                    ));
                }
            }
            Agent::Codex => {
                wanted = 1;
                present = 1;
                let text = read_optional(&codex_config_path(dir))?.unwrap_or_default();
                if let Some(port) = port
                    && !text.contains(&format!("\"{}/v1/logs\"", base_url(port)))
                {
                    present = 0;
                    missing.push(format!(
                        "config.toml sends to another port than the receiver's {port}: run \
                         `hutan integration install codex --telemetry` again"
                    ));
                }
            }
        }
    }
    let status = if !found.installed {
        Status::NotInstalled
    } else if present == wanted && missing.is_empty() {
        Status::Installed
    } else {
        Status::Partial
    };
    let mut notes = Vec::new();
    if !found.items.is_empty() {
        notes.push(format!(
            "{} keeps its own OpenTelemetry settings and Hutan does not change them: neither \
             the shell function nor `install --telemetry` adds anything while they are there; \
             point its export at the receiver yourself or remove them to let Hutan",
            agent.name()
        ));
    } else {
        notes.push(match (found.installed, agent) {
            (true, Agent::Claude) => "reports from any terminal Hutan started (the settings carry \
                the endpoint, the header helper the session's token)"
                .to_owned(),
            (true, Agent::Codex) => "the endpoint is in config.toml; the session's token comes \
                from the shell function, so only a Codex started in a Hutan shell is attributed"
                .to_owned(),
            (false, _) => "in a Hutan terminal started while the receiver listens, the shell \
                function sets what is needed for that one command; install --telemetry is for \
                an agent started where no function is"
                .to_owned(),
        });
    }
    if !enabled {
        notes.push(format!(
            "telemetry is not enabled for {}: `hutan telemetry enable {}` (nothing listens until then)",
            agent.name(),
            agent.id()
        ));
    }
    Ok(TelemetryReport {
        status,
        conflicts: found.items,
        missing,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_block_goes_in_and_out_leaving_the_original_bytes() {
        for original in [
            "",
            "model = \"gpt-5\"\n",
            "model = \"gpt-5\"",
            "model = \"gpt-5\"\n\n",
            "\n",
            "# only a comment",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            if !original.is_empty() {
                fs::write(&path, original).unwrap();
            }
            let mut report = super::super::Report::new(IntegrationTarget::Codex, dir.path());
            codex_install(dir.path(), 50123, &mut report).unwrap();
            let installed = fs::read_to_string(&path).unwrap();
            assert!(installed.contains("[otel]"), "{installed:?}");
            // Installing again, even for another port, keeps one block.
            codex_install(dir.path(), 50124, &mut report).unwrap();
            let again = fs::read_to_string(&path).unwrap();
            assert_eq!(again.matches("[otel]").count(), 1, "{again:?}");
            assert!(again.contains("50124"));
            codex_uninstall(dir.path(), &mut report).unwrap();
            let restored = fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(restored, original, "from {original:?}");
        }
    }

    #[test]
    fn a_users_otel_table_is_a_conflict_not_something_to_merge() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "[otel]\nenvironment = \"dev\"\n",
        )
        .unwrap();
        let system = System::in_root(&dir.path().join("sys"));
        let found = conflicts(Agent::Codex, dir.path(), &no_env, &system).unwrap();
        assert_eq!(found.items.len(), 1, "{:?}", found.items);
        assert!(found.items[0].contains("[otel]"));
        assert!(!found.installed);
        assert_eq!(
            launch(Agent::Codex, &found, "http://127.0.0.1:5/s/t", "t"),
            None
        );
    }

    #[test]
    fn claude_keys_go_in_and_out_leaving_the_other_settings() {
        let mut doc = json!({"model": "opus", "env": {"FOO": "1"}, "hooks": {}});
        let before = doc.clone();
        let helper = Path::new("/h/.claude/hooks").join(HELPER_NAME);
        assert!(claude_set(&mut doc, 50123, &helper).unwrap());
        assert_eq!(
            doc["env"]["OTEL_EXPORTER_OTLP_ENDPOINT"],
            "http://127.0.0.1:50123"
        );
        assert_eq!(doc["env"]["FOO"], "1");
        assert_eq!(doc["otelHeadersHelper"], helper.to_string_lossy().as_ref());
        assert!(!claude_set(&mut doc, 50123, &helper).unwrap());
        assert!(claude_remove(&mut doc).unwrap());
        assert_eq!(doc, before);
        // Without an env of its own the whole key goes away again.
        let mut doc = json!({"model": "opus"});
        claude_set(&mut doc, 1, &helper).unwrap();
        claude_remove(&mut doc).unwrap();
        assert_eq!(doc, json!({"model": "opus"}));
    }

    #[test]
    fn claude_conflicts_name_where_and_never_the_value() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r#"{"env":{"OTEL_EXPORTER_OTLP_HEADERS":"Authorization=Bearer secret-value"},"otelHeadersHelper":"/x"}"#,
        )
        .unwrap();
        let managed = dir.path().join("sys/ClaudeCode");
        fs::create_dir_all(&managed).unwrap();
        fs::write(
            managed.join("managed-settings.json"),
            r#"{"env":{"OTEL_EXPORTER_OTLP_ENDPOINT":"https://collector.example"}}"#,
        )
        .unwrap();
        let env = |name: &str| (name == "CLAUDE_CODE_ENABLE_TELEMETRY").then(|| "1".to_owned());
        let found = conflicts(
            Agent::Claude,
            dir.path(),
            &env,
            &System::in_root(&dir.path().join("sys")),
        )
        .unwrap();
        let text = found.items.join("\n");
        assert!(
            text.contains("CLAUDE_CODE_ENABLE_TELEMETRY is set in the environment"),
            "{text}"
        );
        assert!(
            text.contains("OTEL_EXPORTER_OTLP_HEADERS in `env`"),
            "{text}"
        );
        assert!(text.contains("`otelHeadersHelper`"), "{text}");
        assert!(text.contains("the managed policy"), "{text}");
        assert!(
            !text.contains("secret-value") && !text.contains("collector.example"),
            "{text}"
        );
        assert!(launch(Agent::Claude, &found, "http://127.0.0.1:5/s/t", "t").is_none());
    }

    #[test]
    fn the_launch_never_puts_the_token_in_an_argument() {
        let found = Conflicts::default();
        let endpoint = "http://127.0.0.1:50123/s/0123456789abcdef0123456789abcdef";
        let claude = launch(
            Agent::Claude,
            &found,
            endpoint,
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert!(claude.args.is_empty());
        assert!(claude.env.contains(&(
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(),
            endpoint.to_owned()
        )));
        let codex = launch(
            Agent::Codex,
            &found,
            endpoint,
            "0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        assert!(
            codex
                .args
                .iter()
                .all(|arg| !arg.contains("0123456789abcdef")),
            "{:?}",
            codex.args
        );
        assert!(
            codex
                .args
                .iter()
                .any(|arg| arg.contains("http://127.0.0.1:50123/v1/logs"))
        );
        assert_eq!(
            codex.env,
            [(
                "OTEL_EXPORTER_OTLP_LOGS_HEADERS".to_owned(),
                "x-hutan-token=0123456789abcdef0123456789abcdef".to_owned()
            )]
        );
        // With Hutan's own settings in the agent's files, Claude needs nothing and Codex only
        // the header.
        let installed = Conflicts {
            items: Vec::new(),
            installed: true,
        };
        assert!(launch(Agent::Claude, &installed, endpoint, "t").is_none());
        assert!(
            launch(Agent::Codex, &installed, endpoint, "t")
                .unwrap()
                .args
                .is_empty()
        );
    }
}
