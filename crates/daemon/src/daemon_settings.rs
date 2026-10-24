//! `daemon.toml` as a list of settings a client can show and change (protocol 0.28, capability
//! `daemon.settings`; `docs/protocol.md` § Daemon settings).
//!
//! One table in this file ([`SPECS`]) describes every option a user may set: its kind, default,
//! range or choices, and whether the running daemon takes a change at once or only after a
//! restart. `get` reads `daemon.toml` fresh and compares it with what the running daemon uses;
//! `set` validates against the table, edits the file with `toml_edit` (comments, order and
//! unknown keys stay), writes it atomically, and applies the change to the services that can take
//! it live. A test pins that every field of the config structs is in the table or deliberately
//! left out, so a new option cannot miss the settings surface.
//!
//! Values are never secrets: credentials stay in the Keychain. A key that is not in the table is
//! refused, and so is a write to a file this daemon cannot parse.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::daemon_settings::{
    Applies, DaemonSetting, DaemonSettingKind, DaemonSettingSet, DaemonSettings as Listing,
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use toml_edit::{DocumentMut, Item, TableLike};

use crate::assistant::AssistantService;
use crate::config::{
    self, AgentPolicy, ContextOverride, ContextPermissions, ControlSettings,
    DaemonSettings as Config, RestoreMode,
};
use crate::control::ControlService;
use crate::history::HistoryService;
use crate::memory::MemoryService;
use crate::notify::NotifyService;
use crate::registry::RegistryHandle;
use crate::tasks::TaskService;
use crate::worktrees::WorktreeService;
use hutan_terminal::EnvInherit;

/// The `[notify] kinds` names, as `IntentKind::as_str` gives them (a test keeps them equal).
const INTENT_KINDS: [&str; 8] = [
    "approval_required",
    "agent_blocked",
    "agent_finished",
    "session_exited_with_error",
    "chat_turn_finished",
    "task_done",
    "task_failed",
    "question_required",
];

/// What a path setting must point at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathRule {
    /// An absolute path to an existing executable file.
    Executable,
    /// An existing directory (`~` stands for the user's home directory).
    Directory,
}

/// What a setting holds, with what the daemon accepts for it.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Bool,
    Int {
        min: Option<i64>,
        max: Option<i64>,
        /// When not empty, the only values accepted.
        only: &'static [i64],
    },
    String,
    Path(PathRule),
    Enum(&'static [&'static str]),
    StringList(Option<&'static [&'static str]>),
    StringMap,
}

impl Kind {
    fn wire(self) -> DaemonSettingKind {
        match self {
            Self::Bool => DaemonSettingKind::Bool,
            Self::Int { min, max, .. } => DaemonSettingKind::Int { min, max },
            Self::String => DaemonSettingKind::String,
            Self::Path(_) => DaemonSettingKind::Path,
            Self::Enum(choices) => DaemonSettingKind::Enum {
                choices: choices.iter().map(|c| (*c).to_owned()).collect(),
            },
            Self::StringList(choices) => DaemonSettingKind::StringList {
                choices: choices.map(|c| c.iter().map(|c| (*c).to_owned()).collect()),
            },
            Self::StringMap => DaemonSettingKind::StringMap,
        }
    }
}

/// The service that takes a change of a `Live` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Notify,
    History,
    Assistant,
    Control,
    Tasks,
    Gateway,
    Finish,
    Checkpoints,
}

struct Spec {
    key: &'static str,
    kind: Kind,
    applies: Applies,
    group: Option<Group>,
    /// The value of the setting in a parsed config (its default when the file has none).
    get: fn(&Config) -> Value,
}

const fn live(key: &'static str, kind: Kind, group: Group, get: fn(&Config) -> Value) -> Spec {
    Spec {
        key,
        kind,
        applies: Applies::Live,
        group: Some(group),
        get,
    }
}

const fn restart(key: &'static str, kind: Kind, get: fn(&Config) -> Value) -> Spec {
    Spec {
        key,
        kind,
        applies: Applies::Restart,
        group: None,
        get,
    }
}

const POSITIVE: Kind = Kind::Int {
    min: Some(1),
    max: None,
    only: &[],
};

fn path_value(path: Option<&PathBuf>) -> Value {
    Value::String(path.map_or_else(String::new, |p| p.to_string_lossy().into_owned()))
}

fn text_value(text: Option<&String>) -> Value {
    Value::String(text.cloned().unwrap_or_default())
}

fn list_value(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

fn int_value(n: u64) -> Value {
    json!(i64::try_from(n).unwrap_or(i64::MAX))
}

fn agent_policy_name(policy: AgentPolicy) -> &'static str {
    match policy {
        AgentPolicy::Ask => "ask",
        AgentPolicy::Allow => "allow",
        AgentPolicy::ReadOnly => "read_only",
    }
}

fn restore_mode_name(mode: RestoreMode) -> &'static str {
    match mode {
        RestoreMode::Respawn => "respawn",
        RestoreMode::Stopped => "stopped",
    }
}

fn env_inherit_name(inherit: EnvInherit) -> &'static str {
    match inherit {
        EnvInherit::Login => "login",
        EnvInherit::Daemon => "daemon",
    }
}

/// `[harness.<id>]` as the config has it; a harness without a table has the defaults.
fn harness(config: &Config, id: &str) -> config::HarnessSettings {
    config.harness.get(id).cloned().unwrap_or_default()
}

macro_rules! harness_specs {
    ($specs:ident, $id:literal) => {
        $specs.push(restart(
            concat!("harness.", $id, ".bin"),
            Kind::Path(PathRule::Executable),
            |c| path_value(harness(c, $id).bin.as_ref()),
        ));
        $specs.push(restart(
            concat!("harness.", $id, ".args"),
            Kind::StringList(None),
            |c| list_value(&harness(c, $id).args),
        ));
        $specs.push(restart(
            concat!("harness.", $id, ".env"),
            Kind::StringMap,
            |c| {
                Value::Object(
                    harness(c, $id)
                        .env
                        .into_iter()
                        .map(|(k, v)| (k, Value::String(v)))
                        .collect(),
                )
            },
        ));
        $specs.push(restart(
            concat!("harness.", $id, ".cli"),
            Kind::Path(PathRule::Executable),
            |c| path_value(harness(c, $id).cli.as_ref()),
        ));
    };
}

macro_rules! permission_specs {
    ($specs:ident, $table:ident) => {
        $specs.push(live(
            concat!("assistant.", stringify!($table), ".shell"),
            Kind::Bool,
            Group::Assistant,
            |c| json!(c.assistant.$table.shell),
        ));
        $specs.push(live(
            concat!("assistant.", stringify!($table), ".agents"),
            Kind::Bool,
            Group::Assistant,
            |c| json!(c.assistant.$table.agents),
        ));
        $specs.push(live(
            concat!("assistant.", stringify!($table), ".files"),
            Kind::Bool,
            Group::Assistant,
            |c| json!(c.assistant.$table.files),
        ));
        $specs.push(live(
            concat!("assistant.", stringify!($table), ".search"),
            Kind::Bool,
            Group::Assistant,
            |c| json!(c.assistant.$table.search),
        ));
    };
}

/// Every setting a user may change, in the order clients list them.
static SPECS: LazyLock<Vec<Spec>> = LazyLock::new(build_specs);

fn build_specs() -> Vec<Spec> {
    let mut specs = vec![
        restart(
            "daemon.restore_sessions",
            Kind::Enum(&["respawn", "stopped"]),
            |c| json!(restore_mode_name(c.restore_sessions)),
        ),
        restart(
            "workspaces.home_folder",
            Kind::Path(PathRule::Directory),
            |c| path_value(c.home_folder.as_ref()),
        ),
        live("history.enabled", Kind::Bool, Group::History, |c| {
            json!(c.history.enabled)
        }),
        live(
            "history.range_days",
            Kind::Int {
                min: Some(0),
                max: Some(90),
                only: &[0, 30, 90],
            },
            Group::History,
            |c| json!(c.history.range_days),
        ),
        live("history.hutan", Kind::Bool, Group::History, |c| {
            json!(c.history.hutan)
        }),
        live("history.claude", Kind::Bool, Group::History, |c| {
            json!(c.history.claude)
        }),
        live("history.codex", Kind::Bool, Group::History, |c| {
            json!(c.history.codex)
        }),
        live("history.omp", Kind::Bool, Group::History, |c| {
            json!(c.history.omp)
        }),
        live(
            "history.roots.claude",
            Kind::Path(PathRule::Directory),
            Group::History,
            |c| path_value(c.history.roots.claude.as_ref()),
        ),
        live(
            "history.roots.codex",
            Kind::Path(PathRule::Directory),
            Group::History,
            |c| path_value(c.history.roots.codex.as_ref()),
        ),
        live(
            "history.roots.omp",
            Kind::Path(PathRule::Directory),
            Group::History,
            |c| path_value(c.history.roots.omp.as_ref()),
        ),
        restart("codex.bin", Kind::Path(PathRule::Executable), |c| {
            path_value(c.codex.bin.as_ref())
        }),
        restart("codex.args", Kind::StringList(None), |c| {
            list_value(&c.codex.args)
        }),
    ];
    harness_specs!(specs, "omp");
    harness_specs!(specs, "claude");
    specs.extend([
        live("gateway.max_context_chars", POSITIVE, Group::Gateway, |c| {
            int_value(c.gateway.max_context_chars as u64)
        }),
        live(
            "assistant.backend",
            Kind::Enum(&["omp", "claude", "codex", "provider"]),
            Group::Assistant,
            |c| json!(c.assistant.backend.map_or("", |b| b.as_str())),
        ),
        live("assistant.provider", Kind::String, Group::Assistant, |c| {
            text_value(c.assistant.provider.as_ref())
        }),
        live("assistant.model", Kind::String, Group::Assistant, |c| {
            text_value(c.assistant.model.as_ref())
        }),
        live(
            "assistant.context_budget_chars",
            POSITIVE,
            Group::Assistant,
            |c| int_value(c.assistant.context_budget_chars as u64),
        ),
        live(
            "assistant.idle_stop_secs",
            Kind::Int {
                min: Some(0),
                max: None,
                only: &[],
            },
            Group::Assistant,
            |c| int_value(c.assistant.idle_stop_secs),
        ),
        live(
            "assistant.mode",
            Kind::Enum(&["ask", "advisor"]),
            Group::Assistant,
            |c| json!(c.assistant.mode.as_str()),
        ),
        live(
            "assistant.max_actions_per_turn",
            Kind::Int {
                min: Some(1),
                max: Some(1000),
                only: &[],
            },
            Group::Assistant,
            |c| int_value(u64::from(c.assistant.max_actions_per_turn)),
        ),
        live("assistant.memory", Kind::Bool, Group::Assistant, |c| {
            json!(c.assistant.memory)
        }),
        live(
            "assistant.memory_max_entries",
            Kind::Int {
                min: Some(1),
                max: Some(10_000),
                only: &[],
            },
            Group::Assistant,
            |c| int_value(u64::from(c.assistant.memory_max_entries)),
        ),
        live(
            "assistant.memory_files_in_project",
            Kind::Bool,
            Group::Assistant,
            |c| json!(c.assistant.memory_files_in_project),
        ),
    ]);
    permission_specs!(specs, context);
    permission_specs!(specs, global_context);
    specs.extend([
        live("notify.enabled", Kind::Bool, Group::Notify, |c| {
            json!(c.notify.enabled)
        }),
        live(
            "notify.min_turn_secs",
            Kind::Int {
                min: Some(0),
                max: None,
                only: &[],
            },
            Group::Notify,
            |c| int_value(c.notify.min_turn_secs),
        ),
        live(
            "notify.kinds",
            Kind::StringList(Some(&INTENT_KINDS)),
            Group::Notify,
            |c| {
                Value::Array(
                    c.notify
                        .kinds
                        .iter()
                        .map(|k| Value::String(k.as_str().to_owned()))
                        .collect(),
                )
            },
        ),
        live("notify.expire_secs", POSITIVE, Group::Notify, |c| {
            int_value(c.notify.expire_secs)
        }),
        live(
            "notify.sound",
            Kind::Enum(&["default", "none"]),
            Group::Notify,
            |c| json!(c.notify.sound.as_str()),
        ),
        live("notify.finished", Kind::Bool, Group::Notify, |c| {
            json!(c.notify.finished)
        }),
        restart("notify.native", Kind::Bool, |c| json!(c.notify.native)),
        restart(
            "terminal.env_inherit",
            Kind::Enum(&["login", "daemon"]),
            |c| json!(env_inherit_name(c.terminal.env_inherit)),
        ),
        live("tasks.max_parallel", POSITIVE, Group::Tasks, |c| {
            int_value(c.tasks.max_parallel as u64)
        }),
        restart("projects.auto", Kind::Bool, |c| json!(c.projects.auto)),
        live(
            "control.agents",
            Kind::Enum(&["ask", "allow", "read_only"]),
            Group::Control,
            |c| json!(agent_policy_name(c.control.agents)),
        ),
        restart("control.mcp", Kind::Bool, |c| json!(c.control.mcp)),
        live(
            "control.approval_timeout_secs",
            POSITIVE,
            Group::Control,
            |c| int_value(c.control.approval_timeout_secs),
        ),
        live("finish.push", Kind::Bool, Group::Finish, |c| {
            json!(c.finish.push)
        }),
        live(
            "checkpoints.mode",
            Kind::Enum(&CHECKPOINT_MODES),
            Group::Checkpoints,
            |c| json!(c.checkpoints.mode.as_str()),
        ),
        live(
            "checkpoints.max_file_bytes",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.max_file_bytes),
        ),
        live(
            "checkpoints.snapshot_budget_bytes",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.snapshot_budget_bytes),
        ),
        live("checkpoints.max_files", POSITIVE, Group::Checkpoints, |c| {
            int_value(c.checkpoints.max_files)
        }),
        live(
            "checkpoints.max_untracked_files",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.max_untracked_files),
        ),
        live(
            "checkpoints.exclude",
            Kind::StringList(None),
            Group::Checkpoints,
            |c| list_value(&c.checkpoints.exclude),
        ),
        live(
            "checkpoints.keep_turns",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.keep_turns),
        ),
        live("checkpoints.keep_days", POSITIVE, Group::Checkpoints, |c| {
            int_value(c.checkpoints.keep_days)
        }),
        live(
            "checkpoints.max_repo_bytes",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.max_repo_bytes),
        ),
        live(
            "checkpoints.max_repo_refs",
            POSITIVE,
            Group::Checkpoints,
            |c| int_value(c.checkpoints.max_repo_refs),
        ),
    ]);
    specs
}

/// `checkpoints.mode` and its per-project override.
const CHECKPOINT_MODES: [&str; 2] = ["on", "off"];

/// What `[checkpoints.projects.<project>]` may set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointField {
    Mode,
    MaxFileBytes,
    SnapshotBudgetBytes,
    MaxFiles,
    MaxUntrackedFiles,
}

impl CheckpointField {
    const ALL: [Self; 5] = [
        Self::Mode,
        Self::MaxFileBytes,
        Self::SnapshotBudgetBytes,
        Self::MaxFiles,
        Self::MaxUntrackedFiles,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::MaxFileBytes => "max_file_bytes",
            Self::SnapshotBudgetBytes => "snapshot_budget_bytes",
            Self::MaxFiles => "max_files",
            Self::MaxUntrackedFiles => "max_untracked_files",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == name)
    }

    fn kind(self) -> Kind {
        match self {
            Self::Mode => Kind::Enum(&CHECKPOINT_MODES),
            _ => POSITIVE,
        }
    }

    fn value(self, project: &config::ProjectCheckpoints) -> Value {
        match self {
            Self::Mode => json!(project.mode.as_str()),
            Self::MaxFileBytes => int_value(project.max_file_bytes),
            Self::SnapshotBudgetBytes => int_value(project.snapshot_budget_bytes),
            Self::MaxFiles => int_value(project.max_files),
            Self::MaxUntrackedFiles => int_value(project.max_untracked_files),
        }
    }
}

/// The four permissions of `[assistant.context]`, `[assistant.global_context]` and
/// `[assistant.workspaces.<workspace>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Permission {
    Shell,
    Agents,
    Files,
    Search,
}

impl Permission {
    const ALL: [Self; 4] = [Self::Shell, Self::Agents, Self::Files, Self::Search];

    fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agents => "agents",
            Self::Files => "files",
            Self::Search => "search",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    fn of(self, permissions: &ContextPermissions) -> bool {
        match self {
            Self::Shell => permissions.shell,
            Self::Agents => permissions.agents,
            Self::Files => permissions.files,
            Self::Search => permissions.search,
        }
    }

    fn of_override(self, over: &ContextOverride) -> Option<bool> {
        match self {
            Self::Shell => over.shell,
            Self::Agents => over.agents,
            Self::Files => over.files,
            Self::Search => over.search,
        }
    }
}

/// What a key names.
enum Target {
    Spec(&'static Spec),
    /// `assistant.workspaces.<workspace>.<permission>`: `workspace` is an id or a name.
    Workspace {
        workspace: String,
        permission: Permission,
    },
    /// `checkpoints.projects.<project>.<field>`: `project` is an id or a name.
    CheckpointProject {
        project: String,
        field: CheckpointField,
    },
    /// `mcp.<name>`: one `[mcp.<name>]` table, set as a whole from an object.
    Mcp(String),
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn internal(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

fn resolve_key(key: &str) -> Result<Target, ErrorBody> {
    if let Some(spec) = SPECS.iter().find(|s| s.key == key) {
        return Ok(Target::Spec(spec));
    }
    if let Some(rest) = key.strip_prefix("assistant.workspaces.")
        && let Some((workspace, permission)) = rest.rsplit_once('.')
        && let Some(permission) = Permission::parse(permission)
        && !workspace.is_empty()
    {
        return Ok(Target::Workspace {
            workspace: workspace.to_owned(),
            permission,
        });
    }
    if let Some(rest) = key.strip_prefix("checkpoints.projects.")
        && let Some((project, field)) = rest.rsplit_once('.')
        && let Some(field) = CheckpointField::parse(field)
        && !project.is_empty()
    {
        return Ok(Target::CheckpointProject {
            project: project.to_owned(),
            field,
        });
    }
    if let Some(name) = key.strip_prefix("mcp.")
        && !name.is_empty()
    {
        return Ok(Target::Mcp(name.to_owned()));
    }
    Err(bad_request(format!(
        "`{key}` is not a daemon setting (`hutan config ls` lists them)"
    )))
}

impl Target {
    /// The TOML path of the key.
    fn segments(&self, key: &str) -> Vec<String> {
        match self {
            Self::Spec(_) => key.split('.').map(str::to_owned).collect(),
            Self::Mcp(name) => vec!["mcp".to_owned(), name.clone()],
            Self::Workspace {
                workspace,
                permission,
            } => vec![
                "assistant".to_owned(),
                "workspaces".to_owned(),
                workspace.clone(),
                permission.name().to_owned(),
            ],
            Self::CheckpointProject { project, field } => vec![
                "checkpoints".to_owned(),
                "projects".to_owned(),
                project.clone(),
                field.name().to_owned(),
            ],
        }
    }
}

/// What a `set` does to the file.
#[derive(Debug)]
enum Change {
    Remove,
    Set(toml_edit::Value),
    /// The keys of one `[mcp.<name>]` table: the listed ones are written, the other keys of
    /// the table (a server that changes from `command` to `url`) are dropped.
    McpServer(Vec<(&'static str, toml_edit::Value)>),
}

/// The keys of an `[mcp.<name>]` table, in the order they are written.
const MCP_FIELDS: [&str; 4] = ["command", "args", "url", "transport"];

/// The longest name of an MCP server `set` accepts.
const MCP_NAME_MAX: usize = 32;

/// Checks `value` against the kind of `key` and turns it into the TOML value to write; `None`
/// (or `null`, or an empty string for text, paths and choices) removes the key.
fn validate(
    key: &str,
    kind: Kind,
    value: Option<Value>,
    user_home: Option<&Path>,
) -> Result<Change, ErrorBody> {
    let wrong = |what: &str| bad_request(format!("`{key}` {what}"));
    let value = match value {
        None | Some(Value::Null) => return Ok(Change::Remove),
        Some(value) => value,
    };
    match kind {
        Kind::Bool => match value {
            Value::Bool(b) => Ok(Change::Set(b.into())),
            _ => Err(wrong("is true or false")),
        },
        Kind::Int { min, max, only } => {
            let Some(n) = value.as_i64() else {
                return Err(wrong("is a whole number"));
            };
            if !only.is_empty() && !only.contains(&n) {
                let list: Vec<String> = only.iter().map(i64::to_string).collect();
                return Err(wrong(&format!(
                    "must be one of {}, not {n}",
                    list.join(", ")
                )));
            }
            if let Some(min) = min
                && n < min
            {
                return Err(wrong(&format!("must be at least {min}, not {n}")));
            }
            if let Some(max) = max
                && n > max
            {
                return Err(wrong(&format!("must be at most {max}, not {n}")));
            }
            Ok(Change::Set(n.into()))
        }
        Kind::String | Kind::Path(_) | Kind::Enum(_) => {
            let Some(text) = value.as_str() else {
                return Err(wrong("is text"));
            };
            let text = text.trim();
            if text.is_empty() {
                return Ok(Change::Remove);
            }
            if text.chars().any(char::is_control) {
                return Err(wrong("must not contain control characters"));
            }
            match kind {
                Kind::Enum(choices) if !choices.contains(&text) => Err(wrong(&format!(
                    "must be one of {}, not `{text}`",
                    choices.join(", ")
                ))),
                Kind::Path(rule) => {
                    check_path(key, rule, text, user_home)?;
                    Ok(Change::Set(text.into()))
                }
                _ => Ok(Change::Set(text.into())),
            }
        }
        Kind::StringList(choices) => {
            let Some(items) = value.as_array() else {
                return Err(wrong("is a list of text"));
            };
            let mut array = toml_edit::Array::new();
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err(wrong("is a list of text"));
                };
                if let Some(choices) = choices
                    && !choices.contains(&text)
                {
                    return Err(wrong(&format!(
                        "has no choice `{text}` (one of {})",
                        choices.join(", ")
                    )));
                }
                array.push(text);
            }
            Ok(Change::Set(array.into()))
        }
        Kind::StringMap => {
            let Some(map) = value.as_object() else {
                return Err(wrong("is a table of text (NAME = \"value\")"));
            };
            let mut table = toml_edit::InlineTable::new();
            for (name, item) in map {
                if name.is_empty() || name.contains(['=', '\0']) {
                    return Err(wrong(&format!("has the invalid name `{name}`")));
                }
                let Some(text) = item.as_str() else {
                    return Err(wrong(&format!("needs text for `{name}`")));
                };
                table.insert(name, text.into());
            }
            Ok(Change::Set(table.into()))
        }
    }
}

/// An optional text field of an MCP server: absent, `null` and blank are all "not set".
fn mcp_text(
    key: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, ErrorBody> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.chars().any(char::is_control) {
                return Err(bad_request(format!(
                    "`{key}`: `{field}` must not contain control characters"
                )));
            }
            Ok((!text.is_empty()).then(|| text.to_owned()))
        }
        Some(_) => Err(bad_request(format!("`{key}`: `{field}` is text"))),
    }
}

/// Checks the object `{command, args, url, transport}` of `mcp.<name>` with the rules the
/// config loader applies to `[mcp.<name>]`, and turns it into the keys to write. `None` and
/// `null` remove the server (whatever its name: a server with an old-style name can go).
fn validate_mcp(name: &str, value: Option<Value>) -> Result<Change, ErrorBody> {
    let key = format!("mcp.{name}");
    let object = match value {
        None | Some(Value::Null) => return Ok(Change::Remove),
        Some(Value::Object(object)) => object,
        Some(_) => {
            return Err(bad_request(format!(
                "`{key}` is a table with `command` (and `args`) or `url` (and `transport`)"
            )));
        }
    };
    if name.is_empty()
        || name.len() > MCP_NAME_MAX
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
    {
        return Err(bad_request(format!(
            "the name of an MCP server is 1 to {MCP_NAME_MAX} lowercase letters, digits, `-` \
             or `_`, not `{name}`"
        )));
    }
    if let Some(unknown) = object.keys().find(|k| !MCP_FIELDS.contains(&k.as_str())) {
        return Err(bad_request(format!(
            "`{key}` has no field `{unknown}` (`command`, `args`, `url`, `transport`)"
        )));
    }
    let command = mcp_text(&key, &object, "command")?;
    let url = mcp_text(&key, &object, "url")?;
    let transport = mcp_text(&key, &object, "transport")?;
    let args = match object.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| bad_request(format!("`{key}`: `args` is a list of text")))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(bad_request(format!("`{key}`: `args` is a list of text"))),
    };
    let mut fields: Vec<(&'static str, toml_edit::Value)> = Vec::new();
    match (command, url) {
        (Some(_), Some(_)) => {
            return Err(bad_request(format!(
                "`{key}`: set either `command` (a program on this Mac) or `url` (a remote \
                 server), not both"
            )));
        }
        (None, None) => {
            return Err(bad_request(format!(
                "`{key}`: set `command` (a program on this Mac) or `url` (a remote server)"
            )));
        }
        (Some(command), None) => {
            if transport.is_some() {
                return Err(bad_request(format!(
                    "`{key}`: `transport` is for a server with a `url`"
                )));
            }
            fields.push(("command", command.into()));
            if !args.is_empty() {
                fields.push((
                    "args",
                    args.iter()
                        .map(String::as_str)
                        .collect::<toml_edit::Array>()
                        .into(),
                ));
            }
        }
        (None, Some(url)) => {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(bad_request(format!(
                    "`{key}`: `url` must start with http:// or https://"
                )));
            }
            if !args.is_empty() {
                return Err(bad_request(format!(
                    "`{key}`: `args` is for a server with a `command`"
                )));
            }
            if let Some(transport) = &transport
                && !matches!(transport.as_str(), "http" | "sse")
            {
                return Err(bad_request(format!(
                    "`{key}`: `transport` is `http` or `sse`, not `{transport}`"
                )));
            }
            fields.push(("url", url.into()));
            if let Some(transport) = transport {
                fields.push(("transport", transport.into()));
            }
        }
    }
    Ok(Change::McpServer(fields))
}

fn check_path(
    key: &str,
    rule: PathRule,
    text: &str,
    user_home: Option<&Path>,
) -> Result<(), ErrorBody> {
    let expanded = match text.strip_prefix('~') {
        Some(rest) if rule == PathRule::Directory && (rest.is_empty() || rest.starts_with('/')) => {
            let home = user_home.ok_or_else(|| {
                bad_request(format!("`{key}` uses `~` but there is no home directory"))
            })?;
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(text),
    };
    if !expanded.is_absolute() {
        let hint = match rule {
            PathRule::Executable => "an absolute path to an executable (empty looks it up on PATH)",
            PathRule::Directory => "an absolute path to a folder",
        };
        return Err(bad_request(format!("`{key}` must be {hint}, got `{text}`")));
    }
    let meta = fs::metadata(&expanded).map_err(|e| {
        bad_request(format!(
            "`{key}`: {} is not usable: {e}",
            expanded.display()
        ))
    })?;
    match rule {
        PathRule::Executable => {
            if !meta.is_file() {
                return Err(bad_request(format!(
                    "`{key}`: {} is not a file",
                    expanded.display()
                )));
            }
            if meta.permissions().mode() & 0o111 == 0 {
                return Err(bad_request(format!(
                    "`{key}`: {} is not executable",
                    expanded.display()
                )));
            }
        }
        PathRule::Directory => {
            if !meta.is_dir() {
                return Err(bad_request(format!(
                    "`{key}`: {} is not a folder",
                    expanded.display()
                )));
            }
        }
    }
    Ok(())
}

// Editing the file ---------------------------------------------------------------------------

fn invalid_state(path: &Path, error: impl std::fmt::Display) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        format!(
            "{} cannot be read, fix it by hand first: {error}",
            path.display()
        ),
    )
}

fn read_file(path: &Path) -> Result<String, ErrorBody> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(internal(format!("reading {}: {e}", path.display()))),
    }
}

fn not_a_table(segment: &str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::InvalidState,
        format!("`{segment}` in daemon.toml is not a table; fix it by hand first"),
    )
}

/// Writes `change` at `segments` into `doc`: an existing key keeps its place and the comment
/// after its value, a new key creates the tables on the way, a removal drops the tables it
/// leaves empty (unless a comment hangs on them).
fn apply_change(
    doc: &mut DocumentMut,
    segments: &[String],
    change: Change,
) -> Result<(), ErrorBody> {
    let Some((last, parents)) = segments.split_last() else {
        return Err(internal("a setting key without a name"));
    };
    match change {
        Change::Set(value) => {
            let table = walk(doc, parents)?;
            set_value(table, last, value);
            Ok(())
        }
        Change::McpServer(fields) => {
            let table = walk(doc, parents)?;
            let server = table
                .entry(last)
                .or_insert_with(|| Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or_else(|| not_a_table(last))?;
            for field in MCP_FIELDS {
                if !fields.iter().any(|(name, _)| *name == field) {
                    server.remove(field);
                }
            }
            for (field, value) in fields {
                set_value(server, field, value);
            }
            Ok(())
        }
        Change::Remove => {
            remove_path(doc.as_table_mut(), segments)?;
            Ok(())
        }
    }
}

/// The table `parents` names, created (implicit, so no header of its own) where it is missing.
fn walk<'a>(
    doc: &'a mut DocumentMut,
    parents: &[String],
) -> Result<&'a mut dyn TableLike, ErrorBody> {
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    for segment in parents {
        let item = table.entry(segment).or_insert_with(|| {
            let mut created = toml_edit::Table::new();
            created.set_implicit(true);
            Item::Table(created)
        });
        table = item
            .as_table_like_mut()
            .ok_or_else(|| not_a_table(segment))?;
    }
    Ok(table)
}

/// Writes `value` at `key`: an existing key keeps its place and the comment after its value.
fn set_value(table: &mut dyn TableLike, key: &str, mut value: toml_edit::Value) {
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        Some(item) => *item = Item::Value(value),
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

fn has_comment(decor: &toml_edit::Decor) -> bool {
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .filter_map(toml_edit::RawString::as_str)
        .any(|text| text.contains('#'))
}

/// Removes the key at `segments`; the tables it empties go too. `Ok(false)`: it was not there.
fn remove_path(table: &mut dyn TableLike, segments: &[String]) -> Result<bool, ErrorBody> {
    match segments {
        [] => Ok(false),
        [last] => Ok(table.remove(last).is_some()),
        [first, rest @ ..] => {
            let Some(item) = table.get_mut(first) else {
                return Ok(false);
            };
            let Some(child) = item.as_table_like_mut() else {
                return Err(not_a_table(first));
            };
            let removed = remove_path(child, rest)?;
            let empty = match table.get(first) {
                Some(Item::Table(t)) => t.is_empty() && !has_comment(t.decor()),
                Some(Item::Value(toml_edit::Value::InlineTable(t))) => {
                    t.is_empty() && !has_comment(t.decor())
                }
                _ => false,
            };
            if removed && empty {
                table.remove(first);
            }
            Ok(removed)
        }
    }
}

/// Whether the file has a key at `segments`.
fn present(doc: &DocumentMut, segments: &[String]) -> bool {
    let mut item = doc.as_item();
    for segment in segments {
        match item.get(segment) {
            Some(next) => item = next,
            None => return false,
        }
    }
    true
}

/// Reads the file, applies one change, checks that the result still loads, and writes it.
/// Returns the settings the new file holds. Runs on a blocking thread.
fn edit_file(
    path: &Path,
    user_home: Option<&Path>,
    key: &str,
    segments: &[String],
    change: Change,
) -> Result<Config, ErrorBody> {
    let text = read_file(path)?;
    // A file that cannot be read is not overwritten: the user's text would be lost.
    Config::parse(&text, user_home).map_err(|e| invalid_state(path, e))?;
    let mut doc: DocumentMut = text.parse().map_err(|e| invalid_state(path, e))?;
    apply_change(&mut doc, segments, change)?;
    let edited = doc.to_string();
    let fresh = Config::parse(&edited, user_home)
        .map_err(|e| bad_request(format!("`{key}`: daemon.toml would not load: {e}")))?;
    if edited != text {
        config::write_atomic(path, &edited)
            .map_err(|e| internal(format!("writing {}: {e}", path.display())))?;
    }
    Ok(fresh)
}

// The listing --------------------------------------------------------------------------------

/// The entries of `fresh` (what `daemon.toml` says), compared with `running` (what the daemon
/// uses), in the order of the table, then the workspace overrides, then the MCP servers.
fn entries(fresh: &Config, running: &Config, doc: &DocumentMut) -> Vec<DaemonSetting> {
    let defaults = Config::default();
    let mut list = Vec::new();
    for spec in SPECS.iter() {
        let value = (spec.get)(fresh);
        let segments: Vec<String> = spec.key.split('.').map(str::to_owned).collect();
        list.push(DaemonSetting {
            key: spec.key.to_owned(),
            kind: spec.kind.wire(),
            pending_restart: (spec.get)(running) != value,
            default: (spec.get)(&defaults),
            set_in_file: present(doc, &segments),
            applies: spec.applies,
            value,
        });
    }
    for workspace in fresh.assistant.workspaces.keys() {
        for permission in Permission::ALL {
            let effective = |config: &Config| {
                let default = permission.of(&config.assistant.context);
                config
                    .assistant
                    .workspaces
                    .get(workspace)
                    .and_then(|over| permission.of_override(over))
                    .unwrap_or(default)
            };
            let target = Target::Workspace {
                workspace: workspace.clone(),
                permission,
            };
            let key = format!("assistant.workspaces.{workspace}.{}", permission.name());
            list.push(DaemonSetting {
                pending_restart: effective(fresh) != effective(running),
                value: json!(effective(fresh)),
                default: json!(permission.of(&fresh.assistant.context)),
                set_in_file: present(doc, &target.segments(&key)),
                kind: DaemonSettingKind::Bool,
                applies: Applies::Live,
                key,
            });
        }
    }
    for project in fresh.checkpoints.projects.keys() {
        for field in CheckpointField::ALL {
            let effective =
                |config: &Config| field.value(&config.checkpoints.for_project(project, ""));
            let target = Target::CheckpointProject {
                project: project.clone(),
                field,
            };
            let key = format!("checkpoints.projects.{project}.{}", field.name());
            list.push(DaemonSetting {
                pending_restart: effective(fresh) != effective(running),
                value: effective(fresh),
                default: field.value(&fresh.checkpoints.for_project("", "")),
                set_in_file: present(doc, &target.segments(&key)),
                kind: field.kind().wire(),
                applies: Applies::Live,
                key,
            });
        }
    }
    for (name, server) in &fresh.mcp {
        let describe = |server: Option<&config::McpSettings>| {
            server.map_or(Value::Null, |server| {
                let mut table = Map::new();
                if let Some(command) = &server.command {
                    table.insert("command".into(), json!(command));
                }
                if !server.args.is_empty() {
                    table.insert("args".into(), list_value(&server.args));
                }
                if let Some(url) = &server.url {
                    table.insert("url".into(), json!(url));
                }
                if let Some(transport) = &server.transport {
                    table.insert("transport".into(), json!(transport));
                }
                Value::Object(table)
            })
        };
        list.push(DaemonSetting {
            key: format!("mcp.{name}"),
            kind: DaemonSettingKind::Table,
            value: describe(Some(server)),
            default: Value::Object(Map::new()),
            set_in_file: true,
            applies: Applies::Restart,
            pending_restart: describe(running.mcp.get(name)) != describe(Some(server)),
        });
    }
    // A server that was removed from the file still runs until the restart.
    for (name, server) in &running.mcp {
        if !fresh.mcp.contains_key(name) {
            list.push(DaemonSetting {
                key: format!("mcp.{name}"),
                kind: DaemonSettingKind::Table,
                value: Value::Null,
                default: Value::Object(Map::new()),
                set_in_file: false,
                applies: Applies::Restart,
                pending_restart: server != &config::McpSettings::default(),
            });
        }
    }
    list
}

// The service --------------------------------------------------------------------------------

/// `daemon.settings.get` / `.set`: reads and edits `daemon.toml`, and applies the live settings.
#[derive(Debug)]
pub struct SettingsService {
    file: PathBuf,
    user_home: Option<PathBuf>,
    /// What the daemon started with: the base of what it uses now (the settings that need a
    /// restart never change).
    startup: Config,
    history: Arc<HistoryService>,
    assistant: Arc<AssistantService>,
    notify: Arc<NotifyService>,
    tasks: Arc<TaskService>,
    control: Arc<ControlService>,
    memory: Arc<MemoryService>,
    registry: RegistryHandle,
    worktrees: Arc<WorktreeService>,
    /// One edit of the file at a time.
    write: Mutex<()>,
}

/// The services a [`SettingsService`] applies live settings to.
#[derive(Debug)]
pub struct Targets {
    pub history: Arc<HistoryService>,
    pub assistant: Arc<AssistantService>,
    pub notify: Arc<NotifyService>,
    pub tasks: Arc<TaskService>,
    pub control: Arc<ControlService>,
    pub memory: Arc<MemoryService>,
    pub registry: RegistryHandle,
    pub worktrees: Arc<WorktreeService>,
}

impl SettingsService {
    pub fn new(
        file: PathBuf,
        user_home: Option<PathBuf>,
        startup: Config,
        targets: Targets,
    ) -> Arc<Self> {
        Arc::new(Self {
            file,
            user_home,
            startup,
            history: targets.history,
            assistant: targets.assistant,
            notify: targets.notify,
            tasks: targets.tasks,
            control: targets.control,
            memory: targets.memory,
            registry: targets.registry,
            worktrees: targets.worktrees,
            write: Mutex::new(()),
        })
    }

    /// The settings the running daemon uses: what it started with, with the live settings as
    /// its services hold them now.
    async fn running(&self) -> Config {
        let mut running = self.startup.clone();
        running.history = self.history.settings().await;
        running.assistant = self.assistant.settings();
        running.notify = self.notify.settings();
        let control = self.control.settings();
        running.control = ControlSettings {
            mcp: self.startup.control.mcp,
            ..control
        };
        running.tasks.max_parallel = self.tasks.max_parallel();
        running.gateway.max_context_chars = self.registry.chats().max_context_chars();
        running.finish.push = self.worktrees.finish_push();
        running.checkpoints = self.registry.chats().checkpoints().settings();
        running
    }

    /// `daemon.settings.get`.
    pub async fn get(&self) -> Result<Listing, ErrorBody> {
        let path = self.file.clone();
        let user_home = self.user_home.clone();
        let (fresh, doc) = tokio::task::spawn_blocking(move || {
            let text = read_file(&path)?;
            let fresh =
                Config::parse(&text, user_home.as_deref()).map_err(|e| invalid_state(&path, e))?;
            let doc: DocumentMut = text.parse().map_err(|e| invalid_state(&path, e))?;
            Ok::<_, ErrorBody>((fresh, doc))
        })
        .await
        .map_err(|e| internal(e.to_string()))??;
        let running = self.running().await;
        let entries = entries(&fresh, &running, &doc);
        Ok(Listing {
            file: self.file.to_string_lossy().into_owned(),
            restart_pending: entries.iter().any(|e| e.pending_restart),
            entries,
        })
    }

    /// `daemon.settings.set`.
    pub async fn set(&self, req: DaemonSettingSet) -> Result<Listing, ErrorBody> {
        let target = resolve_key(&req.key)?;
        // `None`: an MCP server, checked as a whole by `validate_mcp`.
        let (kind, group) = match &target {
            Target::Spec(spec) => (Some(spec.kind), spec.group),
            Target::Workspace { .. } => (Some(Kind::Bool), Some(Group::Assistant)),
            Target::CheckpointProject { field, .. } => {
                (Some(field.kind()), Some(Group::Checkpoints))
            }
            Target::Mcp(_) => (None, None),
        };
        let named = match &target {
            Target::Workspace { workspace, .. } => Some(workspace),
            Target::CheckpointProject { project, .. } => Some(project),
            Target::Spec(_) | Target::Mcp(_) => None,
        };
        if let Some(workspace) = named
            && req.value.is_some()
        {
            self.require_workspace(workspace).await?;
        }
        let segments = target.segments(&req.key);
        let _one_edit = self.write.lock().await;
        let (path, user_home, key, value) = (
            self.file.clone(),
            self.user_home.clone(),
            req.key.clone(),
            req.value,
        );
        let fresh = tokio::task::spawn_blocking(move || {
            let change = match (kind, &target) {
                (Some(kind), _) => validate(&key, kind, value, user_home.as_deref())?,
                (None, Target::Mcp(name)) => validate_mcp(name, value)?,
                (None, _) => return Err(internal("a setting without a kind")),
            };
            edit_file(&path, user_home.as_deref(), &key, &segments, change)
        })
        .await
        .map_err(|e| internal(e.to_string()))??;
        if let Some(group) = group {
            self.apply(group, &fresh).await?;
        }
        self.get().await
    }

    /// The workspace an override names must exist, by id or by name.
    async fn require_workspace(&self, workspace: &str) -> Result<(), ErrorBody> {
        let known = self.registry.list_workspaces().await?.workspaces;
        if known
            .iter()
            .any(|w| w.id.to_string() == workspace || w.name == workspace)
        {
            Ok(())
        } else {
            Err(bad_request(format!(
                "no workspace has the id or name `{workspace}` (`hutan ws ls`)"
            )))
        }
    }

    /// Hands the settings of one group to the service that takes them live. What needs a
    /// restart keeps the value the daemon started with.
    async fn apply(&self, group: Group, fresh: &Config) -> Result<(), ErrorBody> {
        match group {
            Group::Notify => {
                let mut notify = fresh.notify.clone();
                notify.native = self.notify.settings().native;
                self.notify.apply_settings(notify);
            }
            Group::History => {
                self.history
                    .apply_settings(fresh.history.clone())
                    .await
                    .map_err(|e| {
                        internal(format!(
                            "saved to daemon.toml, but history indexing could not follow: {}",
                            e.message
                        ))
                    })?;
            }
            Group::Assistant => {
                self.registry
                    .chats()
                    .set_assistant_idle_stop(Duration::from_secs(fresh.assistant.idle_stop_secs));
                self.registry.chats().set_assistant_policy(
                    fresh.assistant.mode,
                    fresh.assistant.max_actions_per_turn,
                );
                self.assistant.apply_settings(fresh.assistant.clone());
                self.memory.apply_settings(&fresh.assistant);
            }
            Group::Control => {
                let current = self.control.settings();
                self.control.apply_settings(ControlSettings {
                    mcp: current.mcp,
                    ..fresh.control
                });
            }
            Group::Tasks => self.tasks.set_max_parallel(fresh.tasks.max_parallel),
            Group::Gateway => self
                .registry
                .chats()
                .set_max_context_chars(fresh.gateway.max_context_chars),
            Group::Finish => self.worktrees.set_finish_push(fresh.finish.push),
            Group::Checkpoints => self
                .registry
                .chats()
                .checkpoints()
                .apply_settings(fresh.checkpoints.clone()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HARNESS_IDS;
    use hutan_protocol::notify::IntentKind;

    fn doc(text: &str) -> DocumentMut {
        text.parse().unwrap()
    }

    fn change(key: &str, value: Value) -> Result<Change, ErrorBody> {
        let Target::Spec(spec) = resolve_key(key).unwrap() else {
            panic!("{key} is not a table setting");
        };
        validate(key, spec.kind, Some(value), None)
    }

    fn refused(key: &str, value: Value) -> String {
        let error = change(key, value).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        error.message
    }

    #[test]
    fn the_intent_kind_choices_are_the_real_ones() {
        let names: Vec<&str> = IntentKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names, INTENT_KINDS);
    }

    #[test]
    fn keys_are_unique_and_every_default_matches_the_kind() {
        let defaults = Config::default();
        let mut seen = std::collections::BTreeSet::new();
        for spec in SPECS.iter() {
            assert!(seen.insert(spec.key), "{} twice", spec.key);
            let default = (spec.get)(&defaults);
            let fits = match spec.kind {
                Kind::Bool => default.is_boolean(),
                Kind::Int { .. } => default.is_i64(),
                Kind::String | Kind::Path(_) | Kind::Enum(_) => default.is_string(),
                Kind::StringList(_) => default.is_array(),
                Kind::StringMap => default.is_object(),
            };
            assert!(
                fits,
                "{}: default {default} does not fit {:?}",
                spec.key, spec.kind
            );
            // The default is what the config gives for an empty file: it validates (or is the
            // empty "not set" of text).
            if let Kind::Enum(choices) = spec.kind
                && default != json!("")
            {
                assert!(choices.contains(&default.as_str().unwrap()), "{}", spec.key);
            }
        }
        // Defaults that are not the type's zero.
        let default = |key: &str| {
            let Target::Spec(spec) = resolve_key(key).unwrap() else {
                panic!()
            };
            (spec.get)(&defaults)
        };
        assert_eq!(default("assistant.global_context.files"), json!(false));
        assert_eq!(default("assistant.context.files"), json!(true));
        assert_eq!(default("tasks.max_parallel"), json!(3));
        assert_eq!(default("notify.kinds").as_array().unwrap().len(), 8);
        assert_eq!(default("control.agents"), json!("ask"));
    }

    /// Every field of the config structs is a setting in the table, or listed here as left out on
    /// purpose. The patterns have no `..`: a new field does not compile until it is listed, and
    /// then the test says whether the table has it.
    #[test]
    fn every_config_field_is_in_the_table_or_left_out_on_purpose() {
        let Config {
            restore_sessions: _,
            home_folder: _,
            history,
            codex,
            harness: _,
            mcp: _,
            gateway,
            assistant,
            notify,
            terminal,
            tasks,
            projects,
            control,
            finish,
            checkpoints,
            // Language servers are read when the daemon starts and take tables of their own
            // (`[lsp.servers.<id>]`, `[lsp.projects.<p>]`); `daemon.toml` is where they are set
            // (`docs/lsp.md`).
            lsp: _,
        } = Config::default();
        let config::HistorySettings {
            enabled: _,
            range_days: _,
            hutan: _,
            claude: _,
            codex: _,
            omp: _,
            roots:
                config::HistoryRoots {
                    claude: _,
                    codex: _,
                    omp: _,
                },
        } = history;
        let config::CodexSettings { bin: _, args: _ } = codex;
        let config::HarnessSettings {
            bin: _,
            args: _,
            env: _,
            cli: _,
        } = config::HarnessSettings::default();
        let config::McpSettings {
            command: _,
            args: _,
            url: _,
            transport: _,
        } = config::McpSettings::default();
        let config::GatewaySettings {
            max_context_chars: _,
        } = gateway;
        let config::AssistantSettings {
            backend: _,
            provider: _,
            model: _,
            context_budget_chars: _,
            idle_stop_secs: _,
            mode: _,
            max_actions_per_turn: _,
            memory: _,
            memory_max_entries: _,
            memory_files_in_project: _,
            context:
                ContextPermissions {
                    shell: _,
                    agents: _,
                    files: _,
                    search: _,
                },
            global_context: _,
            workspaces: _,
        } = assistant;
        let ContextOverride {
            shell: _,
            agents: _,
            files: _,
            search: _,
        } = ContextOverride::default();
        let config::NotifySettings {
            enabled: _,
            min_turn_secs: _,
            kinds: _,
            expire_secs: _,
            native: _,
            sound: _,
            finished: _,
        } = notify;
        let config::TerminalSettings { env_inherit: _ } = terminal;
        let config::TasksSettings { max_parallel: _ } = tasks;
        let config::ProjectsSettings { auto: _ } = projects;
        let ControlSettings {
            agents: _,
            mcp: _,
            approval_timeout_secs: _,
        } = control;
        let config::FinishSettings { push: _ } = finish;
        let config::CheckpointSettings {
            mode: _,
            max_file_bytes: _,
            snapshot_budget_bytes: _,
            max_files: _,
            max_untracked_files: _,
            exclude: _,
            keep_turns: _,
            keep_days: _,
            max_repo_bytes: _,
            max_repo_refs: _,
            projects: _,
        } = checkpoints;
        let config::CheckpointOverride {
            mode: _,
            max_file_bytes: _,
            snapshot_budget_bytes: _,
            max_files: _,
            max_untracked_files: _,
        } = config::CheckpointOverride::default();

        let in_table: std::collections::BTreeSet<&str> = SPECS.iter().map(|s| s.key).collect();
        // Every key a field maps to (the permission tables share four names; the harness
        // tables are the same four fields for each harness id).
        let mut expected: Vec<String> = [
            "daemon.restore_sessions",
            "workspaces.home_folder",
            "history.enabled",
            "history.range_days",
            "history.hutan",
            "history.claude",
            "history.codex",
            "history.omp",
            "history.roots.claude",
            "history.roots.codex",
            "history.roots.omp",
            "codex.bin",
            "codex.args",
            "gateway.max_context_chars",
            "assistant.backend",
            "assistant.provider",
            "assistant.model",
            "assistant.context_budget_chars",
            "assistant.idle_stop_secs",
            "assistant.mode",
            "assistant.max_actions_per_turn",
            "assistant.memory",
            "assistant.memory_max_entries",
            "assistant.memory_files_in_project",
            "notify.enabled",
            "notify.min_turn_secs",
            "notify.kinds",
            "notify.expire_secs",
            "notify.native",
            "notify.sound",
            "notify.finished",
            "terminal.env_inherit",
            "tasks.max_parallel",
            "projects.auto",
            "control.agents",
            "control.mcp",
            "control.approval_timeout_secs",
            "finish.push",
            "checkpoints.mode",
            "checkpoints.max_file_bytes",
            "checkpoints.snapshot_budget_bytes",
            "checkpoints.max_files",
            "checkpoints.max_untracked_files",
            "checkpoints.exclude",
            "checkpoints.keep_turns",
            "checkpoints.keep_days",
            "checkpoints.max_repo_bytes",
            "checkpoints.max_repo_refs",
        ]
        .map(str::to_owned)
        .into();
        for table in ["context", "global_context"] {
            for permission in Permission::ALL {
                expected.push(format!("assistant.{table}.{}", permission.name()));
            }
        }
        for id in HARNESS_IDS {
            for field in ["bin", "args", "env", "cli"] {
                expected.push(format!("harness.{id}.{field}"));
            }
        }
        for key in &expected {
            assert!(in_table.contains(key.as_str()), "{key} is not in the table");
        }
        assert_eq!(
            in_table.len(),
            expected.len(),
            "the table has keys no config field maps to: {:?}",
            in_table
                .iter()
                .filter(|k| !expected.iter().any(|e| e == **k))
                .collect::<Vec<_>>()
        );
        // Left out on purpose: `[mcp.<name>]` (one entry per server, set as a whole),
        // `[assistant.workspaces.<workspace>]` and `[checkpoints.projects.<project>]` (one entry
        // per override, created by `set`; `CheckpointField` lists what a project may override).
    }

    #[test]
    fn booleans_and_numbers_are_checked() {
        assert!(matches!(
            change("notify.enabled", json!(false)).unwrap(),
            Change::Set(_)
        ));
        assert!(refused("notify.enabled", json!("yes")).contains("true or false"));
        assert!(refused("tasks.max_parallel", json!(0)).contains("at least 1"));
        assert!(refused("tasks.max_parallel", json!("3")).contains("whole number"));
        assert!(refused("tasks.max_parallel", json!(2.5)).contains("whole number"));
        assert!(refused("history.range_days", json!(45)).contains("0, 30, 90"));
        assert!(matches!(
            change("history.range_days", json!(30)).unwrap(),
            Change::Set(_)
        ));
        assert!(refused("notify.min_turn_secs", json!(-1)).contains("at least 0"));
        assert!(matches!(
            change("notify.min_turn_secs", json!(0)).unwrap(),
            Change::Set(_)
        ));
    }

    #[test]
    fn choices_lists_and_maps_are_checked() {
        assert!(refused("control.agents", json!("sometimes")).contains("ask, allow, read_only"));
        assert!(matches!(
            change("control.agents", json!("read_only")).unwrap(),
            Change::Set(_)
        ));
        assert!(refused("notify.kinds", json!(["task_done", "nope"])).contains("`nope`"));
        assert!(refused("notify.kinds", json!("task_done")).contains("list of text"));
        assert!(refused("codex.args", json!([1])).contains("list of text"));
        assert!(matches!(
            change("codex.args", json!(["app-server", "-c", "k=v"])).unwrap(),
            Change::Set(_)
        ));
        assert!(refused("harness.omp.env", json!(["A=1"])).contains("table of text"));
        assert!(refused("harness.omp.env", json!({"A": 1})).contains("text for `A`"));
        assert!(refused("harness.omp.env", json!({"A=B": "1"})).contains("invalid name"));
        assert!(matches!(
            change("harness.omp.env", json!({"OMP_LOG": "debug"})).unwrap(),
            Change::Set(_)
        ));
    }

    #[test]
    fn null_and_empty_text_remove_the_key() {
        for value in [Value::Null, json!(""), json!("   ")] {
            assert!(
                matches!(change("assistant.model", value).unwrap(), Change::Remove),
                "text"
            );
        }
        assert!(matches!(
            change("harness.omp.bin", json!("")).unwrap(),
            Change::Remove
        ));
        assert!(matches!(
            change("assistant.backend", json!("")).unwrap(),
            Change::Remove
        ));
        let Target::Spec(spec) = resolve_key("notify.enabled").unwrap() else {
            panic!()
        };
        assert!(matches!(
            validate("notify.enabled", spec.kind, None, None).unwrap(),
            Change::Remove
        ));
    }

    #[test]
    fn executables_must_exist_be_absolute_and_executable() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("run");
        fs::write(&run, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("plain");
        fs::write(&plain, "x").unwrap();
        fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            change("codex.bin", json!(run.to_string_lossy())).unwrap(),
            Change::Set(_)
        ));
        assert!(refused("codex.bin", json!(plain.to_string_lossy())).contains("not executable"));
        assert!(
            refused(
                "codex.bin",
                json!(dir.path().join("missing").to_string_lossy())
            )
            .contains("not usable")
        );
        assert!(refused("codex.bin", json!(dir.path().to_string_lossy())).contains("not a file"));
        assert!(refused("codex.bin", json!("codex")).contains("absolute path"));
        assert!(refused("harness.claude.cli", json!("./claude")).contains("absolute path"));
    }

    #[test]
    fn folders_must_exist_and_a_tilde_is_the_home_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        fs::write(&file, "x").unwrap();
        let ok = |key: &str, value: &Path, home: Option<&Path>| {
            let Target::Spec(spec) = resolve_key(key).unwrap() else {
                panic!()
            };
            validate(key, spec.kind, Some(json!(value.to_string_lossy())), home)
        };
        assert!(ok("history.roots.claude", dir.path(), None).is_ok());
        let error = ok("history.roots.claude", &file, None).unwrap_err();
        assert!(error.message.contains("not a folder"), "{}", error.message);
        let error = ok("workspaces.home_folder", &dir.path().join("nope"), None).unwrap_err();
        assert!(error.message.contains("not usable"), "{}", error.message);
        let error = ok("workspaces.home_folder", Path::new("relative"), None).unwrap_err();
        assert!(error.message.contains("absolute"), "{}", error.message);

        fs::create_dir(dir.path().join("Notes")).unwrap();
        let Target::Spec(spec) = resolve_key("workspaces.home_folder").unwrap() else {
            panic!()
        };
        assert!(
            validate(
                "workspaces.home_folder",
                spec.kind,
                Some(json!("~/Notes")),
                Some(dir.path())
            )
            .is_ok()
        );
        assert!(
            validate(
                "workspaces.home_folder",
                spec.kind,
                Some(json!("~/Notes")),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn keys_outside_the_table_are_refused() {
        for key in [
            "notify",
            "notify.nope",
            "assistant.workspaces.x.nope",
            "assistant.workspaces..files",
            "mcp.",
            "history.roots",
            "",
        ] {
            let Err(error) = resolve_key(key) else {
                panic!("`{key}` resolved");
            };
            assert_eq!(error.code, ErrorCode::BadRequest, "{key}");
            assert!(error.message.contains("not a daemon setting"), "{key}");
        }
        assert!(matches!(
            resolve_key("assistant.workspaces.my.proj.files").unwrap(),
            Target::Workspace { workspace, permission: Permission::Files } if workspace == "my.proj"
        ));
        assert!(matches!(resolve_key("mcp.docs").unwrap(), Target::Mcp(n) if n == "docs"));
    }

    const COMMENTED: &str = r#"# My daemon settings
[daemon]
restore_sessions = "respawn"   # or "stopped"

# who hears what
[notify]
enabled = true  # the master switch
min_turn_secs = 20
future_option = "kept"

[unknown]
x = 1
"#;

    fn edited(text: &str, key: &str, value: Option<Value>) -> String {
        let Ok(target) = resolve_key(key) else {
            panic!("{key}")
        };
        let change = match &target {
            Target::Spec(spec) => validate(key, spec.kind, value, None).unwrap(),
            Target::Mcp(name) => validate_mcp(name, value).unwrap(),
            Target::Workspace { .. } => validate(key, Kind::Bool, value, None).unwrap(),
            Target::CheckpointProject { field, .. } => {
                validate(key, field.kind(), value, None).unwrap()
            }
        };
        let mut doc = doc(text);
        apply_change(&mut doc, &target.segments(key), change).unwrap();
        doc.to_string()
    }

    #[test]
    fn editing_keeps_comments_order_and_unknown_keys() {
        let out = edited(COMMENTED, "notify.enabled", Some(json!(false)));
        assert_eq!(
            out,
            COMMENTED.replace(
                "enabled = true  # the master switch",
                "enabled = false  # the master switch"
            )
        );
        let out = edited(COMMENTED, "daemon.restore_sessions", Some(json!("stopped")));
        assert!(
            out.contains("restore_sessions = \"stopped\"   # or \"stopped\""),
            "{out}"
        );
        assert!(out.contains("future_option = \"kept\""));
        assert!(out.contains("[unknown]\nx = 1"));
        assert!(out.starts_with("# My daemon settings\n"));

        // A removed key leaves the rest of its table (and the comment above the table) alone.
        let out = edited(COMMENTED, "notify.min_turn_secs", None);
        assert!(!out.contains("min_turn_secs"));
        assert!(out.contains("# who hears what\n[notify]"));
        assert!(out.contains("future_option"));
    }

    #[test]
    fn a_new_key_creates_its_table_and_a_removal_drops_the_empty_table() {
        let out = edited("", "tasks.max_parallel", Some(json!(2)));
        assert_eq!(out, "[tasks]\nmax_parallel = 2\n");
        let base = "# top\n[tasks]\nmax_parallel = 2\n";
        let out = edited(base, "history.roots.claude", Some(json!("/tmp")));
        assert_eq!(
            out,
            "# top\n[tasks]\nmax_parallel = 2\n\n[history.roots]\nclaude = \"/tmp\"\n"
        );
        let back = edited(&out, "history.roots.claude", None);
        assert_eq!(back, base);
        // A table with a comment on it stays when its last key goes.
        let out = edited(
            "# mine\n[tasks]\nmax_parallel = 2\n",
            "tasks.max_parallel",
            None,
        );
        assert_eq!(out, "# mine\n[tasks]\n");
        // Removing what is not there changes nothing.
        let same = "[tasks]\nmax_parallel = 2\n";
        assert_eq!(edited(same, "notify.enabled", None), same);
        assert_eq!(edited(same, "history.roots.omp", None), same);
    }

    #[test]
    fn workspace_overrides_are_created_and_removed() {
        let key = "assistant.workspaces.01K7F3B5N2Q9W8E4R6T1Y0V3JA.files";
        let out = edited("", key, Some(json!(false)));
        assert_eq!(
            out,
            "[assistant.workspaces.01K7F3B5N2Q9W8E4R6T1Y0V3JA]\nfiles = false\n"
        );
        let parsed = Config::parse(&out, None).unwrap();
        assert_eq!(
            parsed.assistant.workspaces["01K7F3B5N2Q9W8E4R6T1Y0V3JA"].files,
            Some(false)
        );
        assert_eq!(edited(&out, key, None), "");
    }

    #[test]
    fn checkpoint_project_overrides_are_created_checked_and_removed() {
        let key = "checkpoints.projects.web.mode";
        let out = edited("", key, Some(json!("off")));
        assert_eq!(out, "[checkpoints.projects.web]\nmode = \"off\"\n");
        let parsed = Config::parse(&out, None).unwrap();
        assert_eq!(
            parsed.checkpoints.for_project("01X", "web").mode,
            config::CheckpointMode::Off
        );
        assert_eq!(edited(&out, key, None), "");
        let out = edited("", "checkpoints.projects.web.max_files", Some(json!(9)));
        assert_eq!(out, "[checkpoints.projects.web]\nmax_files = 9\n");
        for (key, value) in [
            ("checkpoints.projects.web.mode", json!("sometimes")),
            ("checkpoints.projects.web.max_files", json!(0)),
        ] {
            let Target::CheckpointProject { field, .. } = resolve_key(key).unwrap() else {
                panic!("{key}");
            };
            assert!(validate(key, field.kind(), Some(value), None).is_err());
        }
        // Retention is not per project, and the project needs a name.
        assert!(resolve_key("checkpoints.projects.web.keep_turns").is_err());
        assert!(resolve_key("checkpoints.projects..mode").is_err());
        assert!(refused("checkpoints.mode", json!("maybe")).contains("on, off"));
    }

    #[test]
    fn a_table_where_a_key_is_expected_is_refused_not_overwritten() {
        let mut document = doc("notify = 3\n");
        let error = apply_change(
            &mut document,
            &["notify".to_owned(), "enabled".to_owned()],
            Change::Set(true.into()),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(error.message.contains("not a table"));
    }

    #[test]
    fn maps_and_lists_are_written_as_toml_values() {
        let out = edited(
            "[harness.omp]\nargs = [\"acp\"]\n",
            "harness.omp.env",
            Some(json!({"OMP_LOG": "debug", "A": "b c"})),
        );
        let parsed = Config::parse(&out, None).unwrap();
        assert_eq!(parsed.harness["omp"].env["OMP_LOG"], "debug");
        assert_eq!(parsed.harness["omp"].env["A"], "b c");
        assert_eq!(parsed.harness["omp"].args, ["acp"]);
        let out = edited(&out, "harness.omp.args", Some(json!(["acp", "--x"])));
        assert_eq!(
            Config::parse(&out, None).unwrap().harness["omp"].args,
            ["acp", "--x"]
        );
    }

    // ---- MCP servers ----

    fn mcp_refused(name: &str, value: Value) -> String {
        let error = validate_mcp(name, Some(value)).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        error.message
    }

    #[test]
    fn an_mcp_server_is_written_as_its_own_table_and_edited_in_place() {
        let text = "# mine\n[tasks]\nmax_parallel = 2 # two\n";
        let added = edited(
            text,
            "mcp.docs",
            Some(json!({"command": "npx", "args": ["-y", "docs"]})),
        );
        assert_eq!(
            added,
            format!("{text}\n[mcp.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs\"]\n")
        );
        let parsed = Config::parse(&added, None).unwrap();
        assert_eq!(parsed.mcp["docs"].command.as_deref(), Some("npx"));
        assert_eq!(parsed.mcp["docs"].args, ["-y", "docs"]);

        // An edit keeps the comments of the table and of the keys it still has.
        let commented = "[mcp.docs]  # the docs\ncommand = \"npx\"  # runs it\nargs = [\"-y\"]\n\n[tasks]\nmax_parallel = 4\n";
        let changed = edited(commented, "mcp.docs", Some(json!({"command": "uvx"})));
        assert_eq!(
            changed,
            "[mcp.docs]  # the docs\ncommand = \"uvx\"  # runs it\n\n[tasks]\nmax_parallel = 4\n"
        );

        // From a program to a URL: the old keys go.
        let remote = edited(
            commented,
            "mcp.docs",
            Some(json!({"url": "https://x.test/mcp", "transport": "sse"})),
        );
        let parsed = Config::parse(&remote, None).unwrap();
        let server = &parsed.mcp["docs"];
        assert_eq!(server.command, None);
        assert!(server.args.is_empty());
        assert_eq!(server.url.as_deref(), Some("https://x.test/mcp"));
        assert_eq!(server.transport.as_deref(), Some("sse"));
        assert!(remote.contains("# the docs") && remote.contains("[tasks]"));
    }

    #[test]
    fn removing_an_mcp_server_keeps_the_rest_and_null_is_a_removal() {
        let text = "# top\n[tasks]\nmax_parallel = 2\n\n# the a server\n[mcp.a]\ncommand = \"a\"\n\n[mcp.b]\ncommand = \"b\"\n";
        // The comments of a server go with it; everything else stays.
        let out = edited(text, "mcp.a", None);
        assert_eq!(
            out,
            "# top\n[tasks]\nmax_parallel = 2\n\n[mcp.b]\ncommand = \"b\"\n"
        );
        let out = edited(&out, "mcp.b", Some(Value::Null));
        assert_eq!(out, "# top\n[tasks]\nmax_parallel = 2\n");
        // Removing what is not there changes nothing.
        assert_eq!(edited(&out, "mcp.gone", None), out);
        // Blank values are "not set", not errors.
        let blank = edited(
            "",
            "mcp.x",
            Some(json!({"command": " npx ", "url": "", "transport": null, "args": null})),
        );
        assert_eq!(blank, "[mcp.x]\ncommand = \"npx\"\n");
    }

    #[test]
    fn an_mcp_server_that_the_loader_would_refuse_is_refused_first() {
        assert!(mcp_refused("x", json!({})).contains("`command`"));
        assert!(mcp_refused("x", json!({"command": "a", "url": "https://x"})).contains("not both"));
        assert!(mcp_refused("x", json!({"url": "ftp://x"})).contains("http"));
        assert!(
            mcp_refused("x", json!({"url": "https://x", "transport": "ws"}))
                .contains("`http` or `sse`")
        );
        assert!(mcp_refused("x", json!({"command": "a", "transport": "http"})).contains("`url`"));
        assert!(mcp_refused("x", json!({"url": "https://x", "args": ["a"]})).contains("`command`"));
        assert!(mcp_refused("x", json!({"command": "a", "env": {}})).contains("no field `env`"));
        assert!(mcp_refused("x", json!({"command": "a", "args": [1]})).contains("list of text"));
        assert!(mcp_refused("x", json!({"command": 3})).contains("is text"));
        assert!(mcp_refused("x", json!("npx")).contains("is a table"));
        assert!(mcp_refused("x", json!({"command": "a\nb"})).contains("control"));
        for name in ["", "Docs", "a.b", "a b", "ü", &"x".repeat(33)] {
            assert!(
                mcp_refused(name, json!({"command": "a"})).contains("name of an MCP server"),
                "{name:?}"
            );
        }
        assert!(validate_mcp(&"x".repeat(32), Some(json!({"command": "a"}))).is_ok());
        assert!(validate_mcp("a-b_9", Some(json!({"command": "a"}))).is_ok());
        // A server with an old-style name can still be removed.
        assert!(matches!(validate_mcp("Old.Name", None), Ok(Change::Remove)));
    }

    #[test]
    fn the_listing_compares_the_file_with_what_runs() {
        let running = Config::parse("[daemon]\nrestore_sessions = \"respawn\"\n", None).unwrap();
        let text = "[daemon]\nrestore_sessions = \"stopped\"\n[notify]\nenabled = false\n\
                    [assistant.workspaces.hutan]\nfiles = false\n[mcp.docs]\ncommand = \"npx\"\n";
        let fresh = Config::parse(text, None).unwrap();
        let list = entries(&fresh, &running, &doc(text));
        let find = |key: &str| {
            list.iter()
                .find(|e| e.key == key)
                .unwrap_or_else(|| panic!("{key}"))
        };

        let restore = find("daemon.restore_sessions");
        assert_eq!(restore.value, json!("stopped"));
        assert_eq!(restore.default, json!("respawn"));
        assert!(restore.set_in_file && restore.pending_restart);
        assert_eq!(restore.applies, Applies::Restart);

        let notify = find("notify.enabled");
        assert_eq!(notify.value, json!(false));
        assert!(notify.set_in_file);
        assert!(
            notify.pending_restart,
            "the file differs from the running daemon"
        );

        let untouched = find("tasks.max_parallel");
        assert_eq!(untouched.value, json!(3));
        assert!(!untouched.set_in_file && !untouched.pending_restart);

        let files = find("assistant.workspaces.hutan.files");
        assert_eq!(files.value, json!(false));
        assert_eq!(
            files.default,
            json!(true),
            "the inherited [assistant.context] value"
        );
        assert!(files.set_in_file && files.pending_restart);
        let shell = find("assistant.workspaces.hutan.shell");
        assert_eq!(shell.value, json!(true));
        assert!(!shell.set_in_file && !shell.pending_restart);

        let docs = find("mcp.docs");
        assert_eq!(docs.kind, DaemonSettingKind::Table);
        assert_eq!(docs.value, json!({"command": "npx"}));
        assert!(docs.pending_restart);
    }

    #[test]
    fn a_file_that_does_not_parse_is_not_edited() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        let broken = "[notify\nenabled = false\n";
        fs::write(&path, broken).unwrap();
        let error = edit_file(
            &path,
            None,
            "notify.enabled",
            &["notify".to_owned(), "enabled".to_owned()],
            Change::Set(true.into()),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidState);
        assert!(error.message.contains("line 1"), "{}", error.message);
        assert_eq!(fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn the_write_is_atomic_private_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        fs::write(&path, COMMENTED).unwrap();
        let fresh = edit_file(
            &path,
            None,
            "tasks.max_parallel",
            &["tasks".to_owned(), "max_parallel".to_owned()],
            Change::Set(5.into()),
        )
        .unwrap();
        assert_eq!(fresh.tasks.max_parallel, 5);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(COMMENTED), "everything before is kept");
        assert!(text.ends_with("[tasks]\nmax_parallel = 5\n"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["daemon.toml"], "no temporary file is left");
        // A missing file is created by a set and not by a removal.
        let missing = dir.path().join("sub.toml");
        edit_file(
            &missing,
            None,
            "tasks.max_parallel",
            &["tasks".to_owned(), "max_parallel".to_owned()],
            Change::Remove,
        )
        .unwrap();
        assert!(!missing.exists());
    }

    #[test]
    fn an_edit_that_would_break_the_file_is_refused_before_writing() {
        // `[mcp.x]` needs a command or a URL: nothing here may write one half-way, so a file
        // that the daemon would refuse to load never reaches the disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        let text = "[harness.omp]\nbin = \"/x\"\n";
        fs::write(&path, text).unwrap();
        // Setting a list where the config wants a number cannot happen through `validate`,
        // so force the failure with a raw change.
        let error = edit_file(
            &path,
            None,
            "tasks.max_parallel",
            &["tasks".to_owned(), "max_parallel".to_owned()],
            Change::Set("many".into()),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(
            error.message.contains("would not load"),
            "{}",
            error.message
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
}
