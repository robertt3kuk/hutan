# ST-1 — daemon settings over the protocol; management fixes in the daemon

Owner: implementer. Crates: `protocol` (authorized for exactly the DTOs below), `daemon`, `client`, `cli`, docs. No desktop changes (ST-3 renders this).

Source: settings audit 2026-10-18 (`docs/spikes/2026-10-18-settings-audit.md`). Today every `daemon.toml` option is file-only and needs a hand edit plus a restart; the desktop cannot show or change them. The daemon owns `daemon.toml`, so the daemon must offer it.

## Contract (protocol 0.28, capability `daemon.settings`)

New module `crates/protocol/src/daemon_settings.rs`:

```rust
/// `daemon.settings.get` (params `{}`) and the answer of `daemon.settings.set`.
pub struct DaemonSettings {
    /// The daemon.toml path (shown to the user, "Reveal in Finder").
    pub file: String,
    pub entries: Vec<DaemonSetting>,
    /// True when some entry's file value is not what the running daemon uses.
    pub restart_pending: bool,
}
pub struct DaemonSetting {
    /// Dotted daemon.toml path: "notify.enabled", "assistant.context.files",
    /// "assistant.workspaces.<id>.files", "harness.omp.bin", "mcp.<name>" …
    pub key: String,
    pub kind: DaemonSettingKind,
    /// The value in daemon.toml, else the default (JSON of the TOML value).
    pub value: Value,
    pub default: Value,
    /// Present in daemon.toml (a reset removes it).
    pub set_in_file: bool,
    pub applies: Applies,
    /// The running daemon uses something else (an edit that needs a restart, or a file
    /// edited by hand since start).
    pub pending_restart: bool,
}
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonSettingKind {
    Bool,
    Int { min: Option<i64>, max: Option<i64> },
    String,
    Path,                       // a file or executable path; empty = default lookup
    Enum { choices: Vec<String> },
    StringList { choices: Option<Vec<String>> },
    StringMap,                  // env maps
    Table,                      // read-only here (mcp servers): shown, edited in the file
}
#[serde(rename_all = "snake_case")]
pub enum Applies { Live, Restart }
/// `daemon.settings.set`: `value: null` (absent) removes the key from daemon.toml (back to
/// the default). Answers `DaemonSettings`.
pub struct DaemonSettingSet { pub key: String, pub value: Option<Value> }
```
Plus `assistant.reset` (`AssistantReset { scope, workspace_id }`, answers `AssistantConfig`-less `Empty`): removes the stored `assistant.configure` choice of that scope so it inherits again (workspace → parent → global → daemon.toml → automatic).

Additive: minor bump to 0.28, capability `daemon.settings` in `conn.rs`, golden fixtures (`just update-golden`), `docs/protocol.md` catalog rows. `Value` is `serde_json::Value` as in `ui.rs`.

## Daemon

1. **Settings service** (`crates/daemon/src/daemon_settings.rs`): one table in code describing every supported key (kind, default, min/max/choices, live or restart) — every key of `config.rs` that a user may set: `[daemon] restore_sessions`, `[workspaces] home_folder`, `[history]` (enabled, range_days, hutan/claude/codex/omp, roots.*), `[codex] bin,args`, `[harness.omp|claude] bin,args,env,cli`, `[mcp.*]` (Table, read-only), `[gateway] max_context_chars`, `[assistant] backend,provider,model,context_budget_chars`, `[assistant.context]` and `[assistant.global_context]` shell/agents/files/search, `[assistant.workspaces.<id>]` (entries for the workspaces that have one; setting a new `assistant.workspaces.<workspace-id>.files` creates it), `[notify] enabled,min_turn_secs,kinds,expire_secs,native`, `[terminal] env_inherit`, `[tasks] max_parallel`, `[projects] auto`, `[control] agents,mcp,approval_timeout_secs`. A test pins that every field of the config structs is either in the table or listed as deliberately excluded (so new options cannot silently miss the UI).
2. `get` reads daemon.toml fresh, compares with the config the daemon started with (`pending_restart`). `set` validates against the table (type, range, choices; paths: must exist and be executable for `bin`, a directory for roots/home_folder; empty string = remove), writes with `toml_edit` **preserving comments and order**, atomically (temp + rename, mode 0600), then applies live where the table says `Live` and the service supports it. At least: `notify.*`, `history.*` (via the existing reload in `history.set_enabled`), `assistant.context*`/`assistant.global_context`/`assistant.workspaces.*` (permissions are read per request: make them read from a shared, swappable config), `control.agents`/`control.approval_timeout_secs`, `tasks.max_parallel`, `gateway.max_context_chars`. Everything else is `Restart` and reports `pending_restart` until the daemon restarts. A broken daemon.toml (hand-edited) → `get` still answers with the parse error in an `invalid_state` error that names line/column; `set` refuses to overwrite a file it cannot parse.
3. **Security:** `daemon.settings.set` and `assistant.reset` from an agent-originated connection (`ControlService::is_agent_origin` / the connection's origin, ADR 0016) → `forbidden`, always, whatever `[control] agents` says: an agent must never widen its own permissions. Not exposed through `hutan mcp`. Values are never secrets (keys stay in the Keychain); refuse keys that are not in the table.
4. **Provider persistence errors** (`crates/daemon/src/providers.rs:303-381`): add/update/remove must return an `internal` error (and roll back the in-memory change) when SQLite fails, instead of logging and answering success. Test with a storage that fails.
5. `assistant.reset` in `assistant.rs`: delete the scope's `app_meta` key; `assistant.list`/`open` then follow inheritance. Test.

## Client / CLI

- `hutan_client`: `daemon_settings()`, `daemon_setting_set(key, Option<Value>)`, `assistant_reset(scope, workspace)`, `supports_daemon_settings()`.
- `hutan config ls [--json]` (key, value, default, applies, pending), `hutan config get <key>`, `hutan config set <key> <value>` (value parsed as TOML scalar/array: `true`, `30`, `"x"`, `["a","b"]`), `hutan config reset <key>`; prints "restart the daemon to apply (`hutan daemon restart`)" when pending. `hutan assistant reset [--workspace ID]`.

## Docs

`docs/protocol.md` (catalog + capability), `docs/architecture.md` one paragraph (daemon owns daemon.toml; edits via `daemon.settings`), `docs/assistant.md` (`assistant.reset`), CLI help.

## Tests

- Unit: table validation (each kind; ranges; choices; path checks), toml_edit round-trip keeps comments/unknown keys, atomic write, every-config-field-covered test.
- Daemon integration (`crates/daemon/tests/daemon_settings.rs`): get on empty home lists defaults; set a live key (notify.enabled=false) → effective immediately (e.g. an intent is no longer raised / `notify` reports disabled) and persisted; set a restart key (daemon.restore_sessions) → `pending_restart` true, after daemon restart false and the value in effect; reset removes the key; invalid values refused with a clear message; agent-originated connection refused (`forbidden`); hand-broken daemon.toml → error with line; assistant.reset makes a workspace inherit the global choice.
- CLI test for `hutan config set/get/reset`.
- Golden fixtures updated.

## Acceptance

`just check` green; protocol 0.28 with capability `daemon.settings`; nothing in desktop. Do not edit `docs/status.md`/`docs/roadmap.md`.
