//! Harness accounts (protocol 0.17, capability `accounts`): DTOs of `account.*` and the
//! `account.changed` event (`docs/protocol.md` § Accounts, ADR 0015).
//!
//! An account is a login the user made *in a harness* (OMP, Codex, Claude Code): Hutan lists
//! what each harness reports about it, and sends the user to the harness's own login flow in a
//! terminal session. Hutan never performs the login, never sees a token and never shows an
//! e-mail address: a provider, a label and the plan are all there is.
//!
//! Protocol 0.50 (capability `accounts.multi`, ADR 0015 § Amendment): a harness can also have
//! **managed accounts**: named configuration homes Hutan creates and starts the harness with
//! (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`). Hutan never opens a home's
//! contents; what an account is logged in as is what the harness reports with its home set.

use serde::{Deserialize, Serialize};

use crate::messages::SessionInfo;

/// Harness ids the daemon knows for accounts.
pub const HARNESS_OMP: &str = "omp";
pub const HARNESS_CLAUDE: &str = "claude";
pub const HARNESS_CODEX: &str = "codex";

/// The account name that stands for the harness's own login: no variable is set, the harness
/// uses the configuration home it finds when started in a terminal (what the only account was
/// before protocol 0.50). Not a name a managed account may take.
pub const SYSTEM_ACCOUNT: &str = "system";
/// Longest name of a managed account.
pub const ACCOUNT_NAME_MAX: usize = 32;

/// Whether a harness has a usable login for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    LoggedIn,
    LoggedOut,
    /// The harness could not say (not installed, did not answer); see `detail`.
    Unknown,
}

/// One login of a harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// `omp`, `claude` or `codex`.
    pub harness: String,
    /// The model provider the login is for (`anthropic`, `openai-codex`, ...).
    pub provider: String,
    /// What to call it: the harness's own name for the account kind (`ChatGPT`, `API key`,
    /// `Claude subscription`), never an e-mail address.
    pub label: String,
    /// The subscription plan when the harness reports one (`plus`, `max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub status: AccountStatus,
    /// Why the status is `unknown`, or what the user can do (`install ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of `account.list`: per harness the accounts it reports, in the order omp, claude,
/// codex. A harness that is not installed, or that did not answer, is one entry with status
/// `unknown` and a `detail`.
///
/// Protocol 0.50 (capability `accounts.multi`): `accounts` are the harness's **own** logins
/// (the `system` account); `managed` lists the accounts Hutan created, each with what the
/// harness reports for its home; `defaults` says which account a new session takes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AccountList {
    pub accounts: Vec<AccountInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed: Vec<ManagedAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defaults: Vec<AccountDefault>,
}

/// An account Hutan created for a harness (protocol 0.50): a name, a label and a folder. No
/// secret is in the record; the login lives in the harness's own storage for that folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedAccount {
    pub harness: String,
    /// 1 to [`ACCOUNT_NAME_MAX`] lowercase letters, digits, `-` or `_`; unique per harness.
    pub name: String,
    /// What the user calls it (`Work`); the name when none was given.
    pub label: String,
    /// The configuration home the harness runs with: `<data dir>/accounts/<harness>/<name>`.
    pub home: String,
    pub created_at_ms: u64,
    /// What the harness reports for this home (what `AccountInfo` is for the system account),
    /// empty in the result of `account.add`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logins: Vec<AccountInfo>,
}

/// Where a default account was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// `account.default` (Settings → Accounts, `hutan account default`).
    Registry,
    /// `[harness.<id>] account` / `[codex] account` in `daemon.toml`.
    Config,
}

/// The account a new session of `harness` takes when its creator names none: the project's
/// (when `project` is set), else the harness's. Precedence for a session: its own choice >
/// the project's default > the harness's default > the harness's own login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountDefault {
    pub harness: String,
    /// The id of the project (workspace) the default is for; absent: the harness's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// A managed account name, or [`SYSTEM_ACCOUNT`]: the harness's own login.
    pub account: String,
    pub source: DefaultSource,
}

/// `account.add` (protocol 0.50): creates the account's empty home and records it. Does not
/// log in: `account.login` with the same `account` does, in a terminal session. Errors:
/// `bad_request` (unknown harness, a bad or reserved name, a label over 64 characters),
/// `conflict` (the name is taken for that harness). Answered with the new [`ManagedAccount`];
/// `account.changed` follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAdd {
    pub harness: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `account.remove` (protocol 0.50): the harness's own logout with the account's home set
/// (where it has one), then the home is deleted and the account forgotten, defaults that named
/// it included. Clients ask the user to confirm first. Errors: `not_found`, `invalid_state`
/// while a live session runs on the account (its title is in the message). Answered with
/// [`AccountRemoved`]; `account.changed` follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRemove {
    pub harness: String,
    pub name: String,
}

/// What `account.remove` did about the harness's own login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveLogout {
    /// The harness's logout ran and succeeded.
    Done,
    /// The harness has no logout command (OMP keeps its login in the home that was deleted),
    /// or its program could not be found.
    Skipped,
    /// The logout failed or timed out: a secret the harness keeps outside the home (a Keychain
    /// item) may remain; `detail` says what to run.
    Failed,
}

/// Result of `account.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRemoved {
    pub logout: RemoveLogout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `account.default` (protocol 0.50): sets the account a new session of `harness` takes, for
/// the harness or, with `project` (a project's name or id), for that project; `account: null`
/// clears it. `account` may be [`SYSTEM_ACCOUNT`] (a project that uses the harness's own login
/// although the harness default is an account). Errors: `not_found` (the account or the
/// project), `bad_request`. Answered with `Empty`; `account.changed` follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountDefaultSet {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// The account a chat runs as (protocol 0.50): `ChatInfo.account`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatAccount {
    pub name: String,
    pub label: String,
}

/// `account.login`: start the harness's own login in an interactive terminal session. The
/// result carries that session; the user completes the login in it. `account.changed` follows
/// when the session ends.
///
/// `provider` is the provider to log in to (`omp login anthropic`); absent lets the harness
/// ask. Errors: `bad_request` (unknown harness, a provider the harness cannot take),
/// `provider_error` when the harness's program cannot be found (the message says what to
/// install or which `daemon.toml` key to set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLogin {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Protocol 0.50: the managed account to log in (the harness's login command runs with its
    /// home); absent or [`SYSTEM_ACCOUNT`]: the harness's own login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// `account.logout`: like `account.login`, for the harness's own logout. A harness that has no
/// logout command (OMP: `/logout` inside its interface) gets a terminal session running the
/// harness, and `instructions` says what to type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLogout {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Protocol 0.50: as for [`AccountLogin`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

/// Result of `account.login` and `account.logout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSession {
    /// The terminal session running the harness's flow.
    pub session: SessionInfo,
    /// What the user still has to do in it, when the harness does not do it by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}
