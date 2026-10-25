//! The registry of managed accounts (protocol 0.50, capability `accounts.multi`; ADR 0015 §
//! Amendment): named configuration homes of the harnesses, and which account a new session takes.
//!
//! An account is `(harness, name)` and a folder, `<data dir>/accounts/<harness>/<name>/`, that
//! Hutan creates empty and the harness fills when the user logs in with the harness's own
//! command. **Hutan never opens anything inside a home**: this module creates the folder, checks
//! that it is one, and deletes the tree when the account is removed. It holds no secret: the
//! registry is `accounts/registry.json` (names, labels, creation times, defaults), written
//! atomically.
//!
//! The environment a harness runs in as an account is [`AccountEnv`]: the harness's variable for
//! its configuration home (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`), see
//! `docs/agents.md` § Several accounts per harness for where each comes from.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_protocol::account::{
    ACCOUNT_NAME_MAX, AccountDefault, DefaultSource, HARNESS_CLAUDE, HARNESS_CODEX, HARNESS_OMP,
    ManagedAccount, SYSTEM_ACCOUNT,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// The longest label.
pub const LABEL_MAX_CHARS: usize = 64;

/// Credential variables that outrank a stored Claude Code login
/// (<https://code.claude.com/docs/en/authentication#authentication-precedence>): a Claude Code
/// that runs as an account must not be given the user's own, or the account would never be used.
const CLAUDE_CREDENTIAL_ENV: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// The environment of a harness process that runs as an account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountEnv {
    /// Variables to set (the home's, last: they win over the harness's `env` table).
    pub set: Vec<(String, String)>,
    /// Variables of the user's login environment to drop.
    pub remove: Vec<&'static str>,
}

impl AccountEnv {
    /// Applies it to `env` (removals first, so a variable of the harness's own `env` table that
    /// the caller adds afterwards is kept).
    pub fn strip(&self, env: &mut BTreeMap<String, String>) {
        for name in &self.remove {
            env.remove(*name);
        }
    }

    /// Sets the account's variables over a list of pairs (a process configuration's `env`).
    pub fn apply_to_pairs(&self, env: &mut Vec<(String, String)>) {
        for (name, value) in &self.set {
            env.retain(|(k, _)| k != name);
            env.push((name.clone(), value.clone()));
        }
    }

    /// Sets the account's variables over `env`.
    pub fn apply(&self, env: &mut BTreeMap<String, String>) {
        for (name, value) in &self.set {
            env.insert(name.clone(), value.clone());
        }
    }
}

/// One account as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub harness: String,
    pub name: String,
    pub label: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct State {
    #[serde(default)]
    accounts: Vec<Record>,
    /// Harness → account name (or `system`).
    #[serde(default)]
    defaults: BTreeMap<String, String>,
    /// Harness → project (workspace id) → account name (or `system`).
    #[serde(default)]
    projects: BTreeMap<String, BTreeMap<String, String>>,
}

/// The accounts of one daemon.
#[derive(Debug)]
pub struct AccountRegistry {
    /// `<data dir>/accounts`; `None`: a registry that is not kept (tests, no data dir).
    dir: Option<PathBuf>,
    /// `[harness.<id>] account` / `[codex] account` of `daemon.toml`.
    config: BTreeMap<String, String>,
    state: Mutex<State>,
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn internal(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

/// Whether `harness` is one accounts exist for.
pub fn is_account_harness(harness: &str) -> bool {
    matches!(harness, HARNESS_OMP | HARNESS_CLAUDE | HARNESS_CODEX)
}

fn check_harness(harness: &str) -> Result<(), ErrorBody> {
    if is_account_harness(harness) {
        Ok(())
    } else {
        Err(bad_request(format!(
            "unknown harness `{harness}` (omp, claude, codex)"
        )))
    }
}

/// An account name: 1 to [`ACCOUNT_NAME_MAX`] lowercase letters, digits, `-`, `_`, starting with
/// a letter or a digit, and not the reserved [`SYSTEM_ACCOUNT`].
pub fn check_name(name: &str) -> Result<(), ErrorBody> {
    let ok = !name.is_empty()
        && name.len() <= ACCOUNT_NAME_MAX
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !ok {
        return Err(bad_request(format!(
            "`{name}` is not an account name: 1 to {ACCOUNT_NAME_MAX} lowercase letters, digits, `-` or `_`, starting with a letter or digit"
        )));
    }
    if name == SYSTEM_ACCOUNT {
        return Err(bad_request(format!(
            "`{SYSTEM_ACCOUNT}` stands for the harness's own login; pick another name"
        )));
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl AccountRegistry {
    /// The registry kept in `<base>/accounts`, with the `daemon.toml` defaults (harness →
    /// account). A registry file that cannot be parsed is moved aside (`registry.json.broken`)
    /// and a warning logged: the homes stay on disk, only the names are lost.
    pub fn open(base: &Path, config: BTreeMap<String, String>) -> Self {
        let dir = base.join("accounts");
        let file = dir.join("registry.json");
        let state = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                let aside = dir.join("registry.json.broken");
                warn!(%error, file = %file.display(), "the account registry is not valid; moved aside");
                let _ = fs::rename(&file, &aside);
                State::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(error) => {
                warn!(%error, file = %file.display(), "the account registry could not be read");
                State::default()
            }
        };
        Self {
            dir: Some(dir),
            config,
            state: Mutex::new(state),
        }
    }

    /// A registry nothing is kept for: it has no accounts and cannot add one.
    pub fn detached() -> Self {
        Self {
            dir: None,
            config: BTreeMap::new(),
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn dir(&self) -> Result<&Path, ErrorBody> {
        self.dir
            .as_deref()
            .ok_or_else(|| internal("this daemon keeps no accounts"))
    }

    /// The folder of an account (derived, never stored).
    pub fn home(&self, record: &Record) -> PathBuf {
        self.dir
            .as_deref()
            .unwrap_or(Path::new(""))
            .join(&record.harness)
            .join(&record.name)
    }

    fn save(&self, state: &State) -> Result<(), ErrorBody> {
        let dir = self.dir()?;
        fs::create_dir_all(dir)
            .map_err(|e| internal(format!("creating {}: {e}", dir.display())))?;
        let json = serde_json::to_vec_pretty(state).map_err(|e| internal(e.to_string()))?;
        let tmp = dir.join("registry.json.tmp");
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&tmp, dir.join("registry.json"))
        };
        write().map_err(|e| internal(format!("saving the account registry: {e}")))
    }

    // Reading ---------------------------------------------------------------------------------

    /// Every account, by harness and name.
    pub fn list(&self) -> Vec<Record> {
        let mut records = self.state().accounts.clone();
        records.sort_by(|a, b| (&a.harness, &a.name).cmp(&(&b.harness, &b.name)));
        records
    }

    /// The account `name` of `harness`.
    pub fn get(&self, harness: &str, name: &str) -> Option<Record> {
        self.state()
            .accounts
            .iter()
            .find(|r| r.harness == harness && r.name == name)
            .cloned()
    }

    /// `record` as the protocol shows it (without what the harness reports).
    pub fn managed(&self, record: &Record) -> ManagedAccount {
        ManagedAccount {
            harness: record.harness.clone(),
            name: record.name.clone(),
            label: record.label.clone(),
            home: self.home(record).to_string_lossy().into_owned(),
            created_at_ms: record.created_at_ms,
            logins: Vec::new(),
        }
    }

    /// The defaults set so far, for `account.list`: the registry's (harness, then project) and
    /// the `daemon.toml` ones of the harnesses the registry has none for.
    pub fn defaults(&self) -> Vec<AccountDefault> {
        let state = self.state();
        let mut out = Vec::new();
        for harness in [HARNESS_OMP, HARNESS_CLAUDE, HARNESS_CODEX] {
            if let Some(account) = state.defaults.get(harness) {
                out.push(AccountDefault {
                    harness: harness.to_owned(),
                    project: None,
                    account: account.clone(),
                    source: DefaultSource::Registry,
                });
            } else if let Some(account) = self.config.get(harness) {
                out.push(AccountDefault {
                    harness: harness.to_owned(),
                    project: None,
                    account: account.clone(),
                    source: DefaultSource::Config,
                });
            }
            for (project, account) in state.projects.get(harness).into_iter().flatten() {
                out.push(AccountDefault {
                    harness: harness.to_owned(),
                    project: Some(project.clone()),
                    account: account.clone(),
                    source: DefaultSource::Registry,
                });
            }
        }
        out
    }

    // Changing --------------------------------------------------------------------------------

    /// Creates the account's empty home (0700, canonical path) and records it. Never writes
    /// into the home.
    pub fn add(&self, harness: &str, name: &str, label: Option<&str>) -> Result<Record, ErrorBody> {
        check_harness(harness)?;
        check_name(name)?;
        let label = match label.map(str::trim).filter(|l| !l.is_empty()) {
            Some(label) => {
                if label.chars().count() > LABEL_MAX_CHARS || label.chars().any(char::is_control) {
                    return Err(bad_request(format!(
                        "a label is at most {LABEL_MAX_CHARS} characters without control characters"
                    )));
                }
                label.to_owned()
            }
            None => name.to_owned(),
        };
        let dir = self.dir()?;
        let mut state = self.state();
        if state
            .accounts
            .iter()
            .any(|r| r.harness == harness && r.name == name)
        {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                format!("{harness} already has an account named `{name}`"),
            ));
        }
        let parent = dir.join(harness);
        fs::create_dir_all(&parent)
            .map_err(|e| internal(format!("creating {}: {e}", parent.display())))?;
        let home = parent.join(name);
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&home).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                ErrorBody::new(
                    ErrorCode::Conflict,
                    format!(
                        "{} exists but is not an account of Hutan; move or remove it first",
                        home.display()
                    ),
                )
            } else {
                internal(format!("creating {}: {e}", home.display()))
            }
        })?;
        let record = Record {
            harness: harness.to_owned(),
            name: name.to_owned(),
            label,
            created_at_ms: now_ms(),
        };
        let mut next = state.clone();
        next.accounts.push(record.clone());
        if let Err(error) = self.save(&next) {
            let _ = fs::remove_dir(&home);
            return Err(error);
        }
        *state = next;
        Ok(record)
    }

    /// Forgets the account and every default that names it. The caller deletes the home first
    /// ([`delete_home`](Self::delete_home)).
    pub fn forget(&self, harness: &str, name: &str) -> Result<(), ErrorBody> {
        let mut state = self.state();
        let mut next = state.clone();
        next.accounts
            .retain(|r| !(r.harness == harness && r.name == name));
        if next.defaults.get(harness).is_some_and(|a| a == name) {
            next.defaults.remove(harness);
        }
        if let Some(projects) = next.projects.get_mut(harness) {
            projects.retain(|_, account| account != name);
        }
        next.projects.retain(|_, projects| !projects.is_empty());
        self.save(&next)?;
        *state = next;
        Ok(())
    }

    /// Deletes the account's home tree. Symbolic links inside are removed, never followed.
    pub fn delete_home(&self, record: &Record) -> Result<(), ErrorBody> {
        let home = self.home(record);
        match fs::remove_dir_all(&home) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(internal(format!("deleting {}: {e}", home.display()))),
        }
    }

    /// Sets or, with `None`, clears a default: of the harness, or of the project (workspace id)
    /// `project`. `account` is a managed account or [`SYSTEM_ACCOUNT`].
    pub fn set_default(
        &self,
        harness: &str,
        account: Option<&str>,
        project: Option<&str>,
    ) -> Result<(), ErrorBody> {
        check_harness(harness)?;
        let mut state = self.state();
        if let Some(account) = account
            && account != SYSTEM_ACCOUNT
            && !state
                .accounts
                .iter()
                .any(|r| r.harness == harness && r.name == account)
        {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("{harness} has no account named `{account}` (`hutan account list`)"),
            ));
        }
        let mut next = state.clone();
        match (project, account) {
            (None, Some(account)) => {
                next.defaults.insert(harness.to_owned(), account.to_owned());
            }
            (None, None) => {
                next.defaults.remove(harness);
            }
            (Some(project), Some(account)) => {
                next.projects
                    .entry(harness.to_owned())
                    .or_default()
                    .insert(project.to_owned(), account.to_owned());
            }
            (Some(project), None) => {
                if let Some(projects) = next.projects.get_mut(harness) {
                    projects.remove(project);
                }
                next.projects.retain(|_, projects| !projects.is_empty());
            }
        }
        if next == *state {
            return Ok(());
        }
        self.save(&next)?;
        *state = next;
        Ok(())
    }

    // Resolving -------------------------------------------------------------------------------

    /// The account a new session runs as: `explicit` (the session's own choice;
    /// [`SYSTEM_ACCOUNT`] is the harness's own login), else the default of the project
    /// (workspace id) `project`, else the harness's default, else `daemon.toml`'s, else none.
    /// `not_found` when the one that wins does not exist.
    pub fn resolve(
        &self,
        harness: &str,
        explicit: Option<&str>,
        project: Option<&str>,
    ) -> Result<Option<Record>, ErrorBody> {
        check_harness(harness)?;
        let state = self.state();
        let named = explicit
            .map(|a| (a, "named"))
            .or_else(|| {
                project
                    .and_then(|p| state.projects.get(harness)?.get(p))
                    .map(|a| (a.as_str(), "the project's default"))
            })
            .or_else(|| {
                state
                    .defaults
                    .get(harness)
                    .map(|a| (a.as_str(), "the default"))
            })
            .or_else(|| {
                self.config
                    .get(harness)
                    .map(|a| (a.as_str(), "the default in daemon.toml"))
            });
        let Some((name, source)) = named else {
            return Ok(None);
        };
        if name == SYSTEM_ACCOUNT {
            return Ok(None);
        }
        state
            .accounts
            .iter()
            .find(|r| r.harness == harness && r.name == name)
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!(
                        "the account {source} for {harness}, `{name}`, does not exist (`hutan account list`)"
                    ),
                )
            })
    }

    /// The environment for a harness that runs as `name`: the harness's variable for its
    /// configuration home. The home is created again when it is missing (deleted by hand);
    /// nothing in it is read. `not_found` for an account that does not exist.
    pub fn launch_env(&self, harness: &str, name: &str) -> Result<AccountEnv, ErrorBody> {
        check_harness(harness)?;
        let record = self.get(harness, name).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::NotFound,
                format!(
                    "{harness} has no account named `{name}`: it was removed (`hutan account list`)"
                ),
            )
        })?;
        let home = self.home(&record);
        if !home.is_dir() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(&home)
                .map_err(|e| internal(format!("creating {}: {e}", home.display())))?;
        }
        let home = home.to_string_lossy().into_owned();
        Ok(match harness {
            HARNESS_CODEX => AccountEnv {
                set: vec![("CODEX_HOME".to_owned(), home)],
                remove: Vec::new(),
            },
            HARNESS_CLAUDE => AccountEnv {
                set: vec![("CLAUDE_CONFIG_DIR".to_owned(), home)],
                remove: CLAUDE_CREDENTIAL_ENV.to_vec(),
            },
            // The agent directory is the default profile's; an explicitly empty `OMP_PROFILE`
            // selects the default profile even when the user's environment names another.
            _ => AccountEnv {
                set: vec![
                    ("PI_CODING_AGENT_DIR".to_owned(), home),
                    ("OMP_PROFILE".to_owned(), String::new()),
                ],
                remove: Vec::new(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(dir: &Path) -> AccountRegistry {
        AccountRegistry::open(dir, BTreeMap::new())
    }

    #[test]
    fn names_are_checked() {
        for good in ["work", "a", "client-1", "x_y", "0day"] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "Work",
            "-x",
            "_x",
            "a b",
            "a/b",
            "system",
            &"x".repeat(33),
            "é",
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_account_is_created_listed_and_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = registry(tmp.path());
        let record = reg.add("claude", "work", Some("  Work  ")).unwrap();
        assert_eq!(record.label, "Work");
        let home = reg.home(&record);
        assert!(home.is_dir());
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&home).unwrap().permissions())
                & 0o777,
            0o700
        );
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0, "the home is empty");
        assert_eq!(
            reg.add("claude", "work", None).unwrap_err().code,
            ErrorCode::Conflict
        );
        reg.set_default("claude", Some("work"), None).unwrap();

        let again = registry(tmp.path());
        assert_eq!(again.list(), vec![record.clone()]);
        assert_eq!(
            again.resolve("claude", None, None).unwrap(),
            Some(record.clone())
        );
        assert_eq!(again.defaults().len(), 1);
    }

    #[test]
    fn a_foreign_folder_is_not_taken_over() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("accounts/omp/old")).unwrap();
        let error = registry(tmp.path()).add("omp", "old", None).unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
    }

    #[test]
    fn precedence_is_session_then_project_then_harness_then_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config = BTreeMap::from([("codex".to_owned(), "cfg".to_owned())]);
        let reg = AccountRegistry::open(tmp.path(), config);
        for name in ["a", "b", "c", "cfg"] {
            reg.add("codex", name, None).unwrap();
        }
        let name = |r: Result<Option<Record>, ErrorBody>| r.unwrap().map(|r| r.name);
        assert_eq!(
            name(reg.resolve("codex", None, Some("p1"))),
            Some("cfg".into())
        );
        reg.set_default("codex", Some("a"), None).unwrap();
        assert_eq!(
            name(reg.resolve("codex", None, Some("p1"))),
            Some("a".into())
        );
        reg.set_default("codex", Some("b"), Some("p1")).unwrap();
        assert_eq!(
            name(reg.resolve("codex", None, Some("p1"))),
            Some("b".into())
        );
        assert_eq!(
            name(reg.resolve("codex", None, Some("p2"))),
            Some("a".into())
        );
        assert_eq!(
            name(reg.resolve("codex", Some("c"), Some("p1"))),
            Some("c".into())
        );
        // The harness's own login can be asked for, per session and per project.
        assert_eq!(name(reg.resolve("codex", Some("system"), Some("p1"))), None);
        reg.set_default("codex", Some("system"), Some("p1"))
            .unwrap();
        assert_eq!(name(reg.resolve("codex", None, Some("p1"))), None);
        reg.set_default("codex", None, Some("p1")).unwrap();
        assert_eq!(
            name(reg.resolve("codex", None, Some("p1"))),
            Some("a".into())
        );
        assert_eq!(
            reg.resolve("codex", Some("nope"), None).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn removing_an_account_clears_the_defaults_that_name_it() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = registry(tmp.path());
        let work = reg.add("omp", "work", None).unwrap();
        reg.add("omp", "home", None).unwrap();
        reg.set_default("omp", Some("work"), None).unwrap();
        reg.set_default("omp", Some("work"), Some("p")).unwrap();
        reg.set_default("omp", Some("home"), Some("q")).unwrap();
        reg.delete_home(&work).unwrap();
        reg.forget("omp", "work").unwrap();
        assert!(!reg.home(&work).exists());
        let defaults = reg.defaults();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].account, "home");
        assert_eq!(reg.resolve("omp", None, Some("p")).unwrap(), None);
        assert_eq!(
            reg.set_default("omp", Some("work"), None).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_default_in_the_config_that_names_nothing_fails_clearly() {
        let tmp = tempfile::tempdir().unwrap();
        let config = BTreeMap::from([("claude".to_owned(), "ghost".to_owned())]);
        let reg = AccountRegistry::open(tmp.path(), config);
        let error = reg.resolve("claude", None, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.contains("daemon.toml"), "{}", error.message);
    }

    #[test]
    fn the_environment_names_the_homes_variable_and_drops_competing_credentials() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = registry(tmp.path());
        for harness in ["codex", "claude", "omp"] {
            reg.add(harness, "w", None).unwrap();
        }
        let home = |h: &str| {
            reg.home(&reg.get(h, "w").unwrap())
                .to_string_lossy()
                .into_owned()
        };
        let codex = reg.launch_env("codex", "w").unwrap();
        assert_eq!(codex.set, vec![("CODEX_HOME".to_owned(), home("codex"))]);
        let claude = reg.launch_env("claude", "w").unwrap();
        assert_eq!(
            claude.set,
            vec![("CLAUDE_CONFIG_DIR".to_owned(), home("claude"))]
        );
        assert!(claude.remove.contains(&"ANTHROPIC_API_KEY"));
        let omp = reg.launch_env("omp", "w").unwrap();
        assert_eq!(
            omp.set,
            vec![
                ("PI_CODING_AGENT_DIR".to_owned(), home("omp")),
                ("OMP_PROFILE".to_owned(), String::new())
            ]
        );
        let mut env = BTreeMap::from([
            ("ANTHROPIC_API_KEY".to_owned(), "x".to_owned()),
            ("OMP_PROFILE".to_owned(), "work".to_owned()),
        ]);
        claude.strip(&mut env);
        omp.apply(&mut env);
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(env["OMP_PROFILE"], "");
    }

    #[test]
    fn a_home_deleted_by_hand_is_created_again_and_nothing_inside_is_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let reg = registry(tmp.path());
        let record = reg.add("claude", "w", None).unwrap();
        let home = reg.home(&record);
        // A credential file the daemon could not read even if it tried.
        let secret = home.join(".credentials.json");
        fs::write(&secret, "{}").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
        reg.launch_env("claude", "w").unwrap();
        reg.managed(&record);
        reg.delete_home(&record).unwrap();
        assert!(!home.exists());
        reg.launch_env("claude", "w").unwrap();
        assert!(home.is_dir());
    }
}
