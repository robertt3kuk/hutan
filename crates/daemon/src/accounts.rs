//! Accounts (protocol 0.17, capability `accounts`; ADR 0015): the logins of the harnesses the
//! user chats through, listed from what each harness itself reports, and logged in or out by the
//! harness's own flow in a terminal session.
//!
//! Hutan never performs a login, never reads a harness's credentials and never shows an e-mail
//! address: provider, label and plan are all there is.
//!
//! - **OMP**: the providers `omp models --json` lists (a provider with usable models has a
//!   login), their plan from `omp usage --json` when OMP reports one. Login is `omp login
//!   [provider]`; OMP has no logout command, so logout opens `omp` with the instruction to type
//!   `/logout`.
//! - **Claude Code**: `claude auth status --json`; login is `claude auth login`, logout
//!   `claude auth logout`.
//! - **Codex**: the app server's `account/read`; login is `codex login`, logout `codex logout`.
//!
//! A login or logout session is an ordinary terminal session in the Home workspace; when it ends
//! the daemon publishes `account.changed` and clients list the accounts again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_agents::{AgentError, CodexConfig};
use hutan_core::{ModelInfo, SessionId};
use hutan_protocol::account::{
    AccountInfo, AccountList, AccountLogin, AccountLogout, AccountSession, AccountStatus,
    HARNESS_CLAUDE, HARNESS_CODEX, HARNESS_OMP,
};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    Event, SessionCreate, SessionListFilter, SessionSpec, TerminalSpec,
};
use serde_json::Value;
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::bus::EventBus;
use crate::harness::Harnesses;
use crate::registry::RegistryHandle;

/// How long a harness may take to report its accounts.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a harness may take to say whether the assistant can run on it.
const READINESS_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a readiness answer is reused: opening an assistant must not start every harness's
/// CLI each time.
const READINESS_TTL: Duration = Duration::from_secs(30);
/// Longest output read from a harness's status command.
const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
/// How often a login session is looked at to see whether it ended.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

/// Whether the assistant can run on one harness right now (`AM-1`): the harness is found and
/// logged in, and for Claude Code its ACP adapter is found too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub harness: &'static str,
    /// `Err` holds one short reason.
    pub verdict: Result<(), String>,
}

type ReadinessCache = Arc<tokio::sync::Mutex<Option<(Instant, Vec<Readiness>)>>>;

/// The accounts of one daemon.
#[derive(Debug)]
pub struct AccountService {
    registry: RegistryHandle,
    harnesses: Arc<Harnesses>,
    codex: CodexConfig,
    bus: Arc<EventBus>,
    shutdown: CancellationToken,
    /// The last readiness answer; dropped when a login or logout ends.
    readiness: ReadinessCache,
}

impl AccountService {
    pub fn new(
        registry: RegistryHandle,
        harnesses: Arc<Harnesses>,
        codex: CodexConfig,
        bus: Arc<EventBus>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            harnesses,
            codex,
            bus,
            shutdown,
            readiness: Arc::default(),
        })
    }

    /// The CLI of `harness` and its environment (the lookup may run a login shell).
    async fn cli(&self, harness: &str) -> Result<(PathBuf, BTreeMap<String, String>), ErrorBody> {
        let harnesses = self.harnesses.clone();
        let name = harness.to_owned();
        let codex = self.codex.program.clone();
        tokio::task::spawn_blocking(move || harnesses.cli(&name, &codex))
            .await
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("locating {harness}: {e}")))?
    }

    // Listing ---------------------------------------------------------------------------------

    /// The accounts every harness reports, omp first, then claude, then codex.
    pub async fn list(&self) -> AccountList {
        let (omp, claude, codex) = tokio::join!(
            self.omp_accounts(),
            self.claude_accounts(),
            self.codex_accounts()
        );
        let accounts = omp.into_iter().chain(claude).chain(codex).collect();
        AccountList { accounts }
    }

    // Readiness -------------------------------------------------------------------------------

    /// Whether the assistant can run on OMP, Claude Code and Codex (in that order), kept for
    /// [`READINESS_TTL`] and dropped when a login or logout ends. Concurrent callers wait for
    /// the one probe instead of starting their own. Unlike [`list`](Self::list) this never asks
    /// OMP for usage.
    pub async fn harness_readiness(&self) -> Vec<Readiness> {
        let mut cache = self.readiness.lock().await;
        if let Some((at, answer)) = cache.as_ref()
            && at.elapsed() < READINESS_TTL
        {
            return answer.clone();
        }
        let (omp, claude, codex) = tokio::join!(
            Self::probe(self.omp_ready()),
            Self::probe(self.claude_ready()),
            Self::probe(self.codex_ready())
        );
        let answer = vec![
            Readiness {
                harness: HARNESS_OMP,
                verdict: omp,
            },
            Readiness {
                harness: HARNESS_CLAUDE,
                verdict: claude,
            },
            Readiness {
                harness: HARNESS_CODEX,
                verdict: codex,
            },
        ];
        *cache = Some((Instant::now(), answer.clone()));
        answer
    }

    async fn probe(check: impl Future<Output = Result<(), String>>) -> Result<(), String> {
        timeout(READINESS_TIMEOUT, check)
            .await
            .unwrap_or_else(|_| Err("did not answer in time".to_owned()))
    }

    async fn omp_ready(&self) -> Result<(), String> {
        let (program, env) = self.cli(HARNESS_OMP).await.map_err(short_reason)?;
        let entries = omp_catalog(&program, &env).await.map_err(short_reason)?;
        if entries.is_empty() {
            return Err("no provider has a login (`hutan account login omp`)".to_owned());
        }
        Ok(())
    }

    async fn claude_ready(&self) -> Result<(), String> {
        logged_in(self.claude_accounts().await)?;
        let harnesses = self.harnesses.clone();
        tokio::task::spawn_blocking(move || harnesses.acp_config(HARNESS_CLAUDE, None))
            .await
            .map_err(|e| format!("locating the adapter: {e}"))?
            .map(drop)
            .map_err(short_reason)
    }

    async fn codex_ready(&self) -> Result<(), String> {
        logged_in(self.codex_accounts().await)
    }

    fn unknown(harness: &str, provider: &str, detail: String) -> Vec<AccountInfo> {
        vec![AccountInfo {
            harness: harness.to_owned(),
            provider: provider.to_owned(),
            label: harness.to_owned(),
            plan: None,
            status: AccountStatus::Unknown,
            detail: Some(detail),
        }]
    }

    async fn omp_accounts(&self) -> Vec<AccountInfo> {
        let (program, env) = match self.cli(HARNESS_OMP).await {
            Ok(found) => found,
            Err(error) => return Self::unknown(HARNESS_OMP, "omp", error.message),
        };
        let entries = match omp_catalog(&program, &env).await {
            Ok(entries) => entries,
            Err(error) => return Self::unknown(HARNESS_OMP, "omp", error.message),
        };
        let mut providers: Vec<String> = Vec::new();
        for entry in &entries {
            if !providers.contains(&entry.provider) {
                providers.push(entry.provider.clone());
            }
        }
        if providers.is_empty() {
            return vec![AccountInfo {
                harness: HARNESS_OMP.to_owned(),
                provider: "omp".to_owned(),
                label: "no provider has a login".to_owned(),
                plan: None,
                status: AccountStatus::LoggedOut,
                detail: Some("run `hutan account login omp`".to_owned()),
            }];
        }
        let plans = omp_plans(&program, &env).await;
        providers
            .into_iter()
            .map(|provider| AccountInfo {
                harness: HARNESS_OMP.to_owned(),
                plan: plans.get(&provider).cloned(),
                label: provider.clone(),
                provider,
                status: AccountStatus::LoggedIn,
                detail: None,
            })
            .collect()
    }

    async fn claude_accounts(&self) -> Vec<AccountInfo> {
        let (program, env) = match self.cli(HARNESS_CLAUDE).await {
            Ok(found) => found,
            Err(error) => return Self::unknown(HARNESS_CLAUDE, "anthropic", error.message),
        };
        match run(&program, &["auth", "status", "--json"], &env).await {
            Ok(text) => vec![parse_claude_status(&text)],
            Err(error) => Self::unknown(HARNESS_CLAUDE, "anthropic", error.message),
        }
    }

    async fn codex_accounts(&self) -> Vec<AccountInfo> {
        // Found as the other CLIs are (the login environment's PATH), so a GUI-launched
        // daemon starts the same Codex a shell would.
        let (program, env) = match self.cli(HARNESS_CODEX).await {
            Ok(found) => found,
            Err(error) => return Self::unknown(HARNESS_CODEX, "openai", error.message),
        };
        let mut config = self.codex.clone();
        config.program = program;
        config.env.extend(env);
        match timeout(STATUS_TIMEOUT, hutan_agents::codex::read_account(&config)).await {
            Err(_) => Self::unknown(
                HARNESS_CODEX,
                "openai",
                "Codex did not answer in time".to_owned(),
            ),
            Ok(Err(error)) => Self::unknown(HARNESS_CODEX, "openai", agent_message(&error)),
            Ok(Ok(account)) => {
                let (status, label) = match account.kind.as_deref() {
                    None => (AccountStatus::LoggedOut, "Codex".to_owned()),
                    Some("chatgpt") => (AccountStatus::LoggedIn, "ChatGPT".to_owned()),
                    Some("apiKey") => (AccountStatus::LoggedIn, "API key".to_owned()),
                    Some("amazonBedrock") => (AccountStatus::LoggedIn, "Amazon Bedrock".to_owned()),
                    Some(other) => (AccountStatus::LoggedIn, other.to_owned()),
                };
                vec![AccountInfo {
                    harness: HARNESS_CODEX.to_owned(),
                    provider: "openai".to_owned(),
                    label,
                    plan: account.plan,
                    status,
                    detail: None,
                }]
            }
        }
    }

    // Login and logout --------------------------------------------------------------------------

    /// Starts the harness's own login in a terminal session.
    pub async fn login(&self, req: AccountLogin) -> Result<AccountSession, ErrorBody> {
        self.flow(&req.harness, req.provider.as_deref(), Flow::Login)
            .await
    }

    /// Starts the harness's own logout in a terminal session.
    pub async fn logout(&self, req: AccountLogout) -> Result<AccountSession, ErrorBody> {
        self.flow(&req.harness, req.provider.as_deref(), Flow::Logout)
            .await
    }

    async fn flow(
        &self,
        harness: &str,
        provider: Option<&str>,
        flow: Flow,
    ) -> Result<AccountSession, ErrorBody> {
        let provider = provider.map(str::trim).filter(|p| !p.is_empty());
        if let Some(provider) = provider {
            check_provider(harness, provider)?;
        } else if !matches!(harness, HARNESS_OMP | HARNESS_CLAUDE | HARNESS_CODEX) {
            return Err(bad_request(format!(
                "unknown harness `{harness}` (omp, claude, codex)"
            )));
        }
        let (program, _) = self.cli(harness).await?;
        let program = program.to_string_lossy().into_owned();
        let (args, instructions, what): (Vec<String>, Option<String>, String) =
            match (harness, flow) {
                (HARNESS_OMP, Flow::Login) => (
                    std::iter::once("login".to_owned())
                        .chain(provider.map(str::to_owned))
                        .collect(),
                    None,
                    provider.map_or_else(|| "omp".to_owned(), |p| format!("omp ({p})")),
                ),
                (HARNESS_OMP, Flow::Logout) => (
                    Vec::new(),
                    Some(format!(
                        "OMP logs out inside its own interface: type `/logout{}` here",
                        provider.map(|p| format!(" {p}")).unwrap_or_default()
                    )),
                    "omp".to_owned(),
                ),
                (HARNESS_CLAUDE, Flow::Login) => (
                    vec!["auth".to_owned(), "login".to_owned()],
                    None,
                    "Claude Code".to_owned(),
                ),
                (HARNESS_CLAUDE, Flow::Logout) => (
                    vec!["auth".to_owned(), "logout".to_owned()],
                    None,
                    "Claude Code".to_owned(),
                ),
                (_, Flow::Login) => (vec!["login".to_owned()], None, "Codex".to_owned()),
                (_, Flow::Logout) => (vec!["logout".to_owned()], None, "Codex".to_owned()),
            };
        let argv: Vec<String> = std::iter::once(program).chain(args).collect();
        let verb = match flow {
            Flow::Login => "Log in to",
            Flow::Logout => "Log out of",
        };
        let session = self
            .registry
            .create_session(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: argv,
                    title: Some(format!("{verb} {what}")),
                    ..TerminalSpec::default()
                }),
            })
            .await?;
        self.watch(harness.to_owned(), session.id);
        Ok(AccountSession {
            session,
            instructions,
        })
    }

    /// Publishes `account.changed` when the session ends (or is gone), after dropping the
    /// readiness answer the login or logout made stale.
    fn watch(&self, harness: String, session: SessionId) {
        let registry = self.registry.clone();
        let bus = self.bus.clone();
        let shutdown = self.shutdown.clone();
        let readiness = self.readiness.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => return,
                    () = tokio::time::sleep(WATCH_INTERVAL) => {}
                }
                let live = match registry.list_sessions(SessionListFilter::default()).await {
                    Ok(list) => list
                        .sessions
                        .iter()
                        .find(|s| s.id == session)
                        .is_some_and(|s| s.state.is_live()),
                    // The registry is gone: the daemon is stopping.
                    Err(_) => return,
                };
                if !live {
                    break;
                }
            }
            readiness.lock().await.take();
            if let Err(error) = bus.publish(&Event::AccountChanged { harness }) {
                warn!(%error, "failed to publish account.changed");
            }
        });
    }
}

#[derive(Debug, Clone, Copy)]
enum Flow {
    Login,
    Logout,
}

/// `Ok` when the harness's account is logged in, else one short reason.
fn logged_in(accounts: Vec<AccountInfo>) -> Result<(), String> {
    let Some(account) = accounts.into_iter().next() else {
        return Err("reports no account".to_owned());
    };
    match account.status {
        AccountStatus::LoggedIn => Ok(()),
        AccountStatus::LoggedOut => Err("not logged in".to_owned()),
        _ => Err(account
            .detail
            .map_or_else(|| "its login cannot be read".to_owned(), |d| short(&d))),
    }
}

/// The first sentence of an error: the rest says how to configure what is missing.
fn short(message: &str) -> String {
    message
        .split(';')
        .next()
        .unwrap_or(message)
        .trim()
        .to_owned()
}

fn short_reason(error: ErrorBody) -> String {
    short(&error.message)
}

/// A provider id as OMP names them (`anthropic`, `openai-codex`): safe to pass as an argument.
fn check_provider(harness: &str, provider: &str) -> Result<(), ErrorBody> {
    match harness {
        HARNESS_OMP => {
            let ok = !provider.starts_with('-')
                && provider
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if ok {
                Ok(())
            } else {
                Err(bad_request(format!("`{provider}` is not a provider id")))
            }
        }
        HARNESS_CLAUDE if provider == "anthropic" => Ok(()),
        HARNESS_CODEX if matches!(provider, "openai" | "openai-codex") => Ok(()),
        HARNESS_CLAUDE | HARNESS_CODEX => Err(bad_request(format!(
            "{harness} has one login of its own: it takes no provider `{provider}`"
        ))),
        other => Err(bad_request(format!(
            "unknown harness `{other}` (omp, claude, codex)"
        ))),
    }
}

fn agent_message(error: &AgentError) -> String {
    match error {
        AgentError::Spawn { program, .. } => format!("could not start `{program}`"),
        other => other.to_string(),
    }
}

// Running a harness's CLI ---------------------------------------------------------------------

/// stdout of `program args`, run in `env` with no input; fails on a non-zero exit, a timeout or
/// output past [`MAX_OUTPUT`]. Never logs the output (it may name accounts).
async fn run(
    program: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
) -> Result<String, ErrorBody> {
    let name = program.file_name().map_or_else(
        || program.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let fail = |message: String| ErrorBody::new(ErrorCode::ProviderError, message);
    let mut child = Command::new(program)
        .args(args)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| fail(format!("could not start `{name}`: {e}")))?;
    let Some(stdout) = child.stdout.take() else {
        return Err(fail(format!("`{name}` gave no output pipe")));
    };
    let exchange = async {
        let mut out = Vec::new();
        stdout.take(MAX_OUTPUT).read_to_end(&mut out).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((out, status))
    };
    match timeout(STATUS_TIMEOUT, exchange).await {
        Err(_) => Err(fail(format!(
            "`{name} {}` did not answer in time",
            args.join(" ")
        ))),
        Ok(Err(error)) => Err(fail(format!("`{name}` failed: {error}"))),
        Ok(Ok((out, status))) if status.success() => String::from_utf8(out)
            .map_err(|_| fail(format!("`{name}` printed text that is not UTF-8"))),
        Ok(Ok((_, status))) => Err(fail(format!(
            "`{name} {}` exited with {status}",
            args.join(" ")
        ))),
    }
}

// OMP -----------------------------------------------------------------------------------------

/// One chat model of `omp models --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OmpModel {
    provider: String,
    /// `provider/id`, what ACP and `--model` take.
    selector: String,
    name: String,
    thinking: Vec<String>,
}

async fn omp_catalog(
    program: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Vec<OmpModel>, ErrorBody> {
    let text = run(program, &["models", "--json"], env).await?;
    parse_omp_models(&text).map_err(|message| {
        ErrorBody::new(
            ErrorCode::ProviderError,
            format!("`omp models --json` answered with an unexpected shape: {message}"),
        )
    })
}

fn parse_omp_models(text: &str) -> Result<Vec<OmpModel>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or("no `models` list")?;
    Ok(models
        .iter()
        .filter(|m| {
            m.get("kind")
                .and_then(Value::as_str)
                .is_none_or(|k| k == "chat")
        })
        .filter_map(|m| {
            let provider = m.get("provider").and_then(Value::as_str)?;
            let id = m.get("id").and_then(Value::as_str)?;
            let selector = m
                .get("selector")
                .and_then(Value::as_str)
                .map_or_else(|| format!("{provider}/{id}"), str::to_owned);
            Some(OmpModel {
                provider: provider.to_owned(),
                name: m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_owned(),
                selector,
                thinking: m
                    .get("thinking")
                    .and_then(Value::as_array)
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// The models OMP offers (`omp models --json`), as the ids ACP takes.
pub async fn omp_models(
    program: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Vec<ModelInfo>, ErrorBody> {
    Ok(omp_catalog(program, env)
        .await?
        .into_iter()
        .map(|m| ModelInfo {
            id: m.selector,
            display_name: m.name,
            description: m.provider,
            is_default: false,
            hidden: false,
            default_reasoning_effort: None,
            reasoning_efforts: m.thinking,
        })
        .collect())
}

/// Plans per provider from `omp usage --json`'s reports (`metadata.planType`), best effort:
/// OMP asks the providers' usage endpoints, which can fail or take long.
async fn omp_plans(program: &Path, env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let text = match run(program, &["usage", "--json"], env).await {
        Ok(text) => text,
        Err(error) => {
            debug!(message = %error.message, "no usage report from omp");
            return BTreeMap::new();
        }
    };
    parse_omp_plans(&text)
}

fn parse_omp_plans(text: &str) -> BTreeMap<String, String> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return BTreeMap::new();
    };
    let mut plans = BTreeMap::new();
    for report in value
        .get("reports")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let (Some(provider), Some(plan)) = (
            report.get("provider").and_then(Value::as_str),
            report.pointer("/metadata/planType").and_then(Value::as_str),
        ) else {
            continue;
        };
        if plan != "unknown" {
            plans
                .entry(provider.to_owned())
                .or_insert_with(|| plan.to_owned());
        }
    }
    plans
}

// Claude Code ---------------------------------------------------------------------------------

/// `claude auth status --json`: `{loggedIn, authMethod, ...}`. Only the method and a plan are
/// read; the rest (directories, and an e-mail address when logged in) is not looked at.
fn parse_claude_status(text: &str) -> AccountInfo {
    let account = |status, label: &str, plan, detail| AccountInfo {
        harness: HARNESS_CLAUDE.to_owned(),
        provider: "anthropic".to_owned(),
        label: label.to_owned(),
        plan,
        status,
        detail,
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return account(
            AccountStatus::Unknown,
            "Claude Code",
            None,
            Some("`claude auth status` did not print JSON".to_owned()),
        );
    };
    let Some(logged_in) = value.get("loggedIn").and_then(Value::as_bool) else {
        return account(
            AccountStatus::Unknown,
            "Claude Code",
            None,
            Some("`claude auth status` did not say whether it is logged in".to_owned()),
        );
    };
    if !logged_in {
        return account(AccountStatus::LoggedOut, "Claude Code", None, None);
    }
    let label = match value.get("authMethod").and_then(Value::as_str) {
        Some("claude.ai") => "Claude subscription",
        Some("api_key" | "apiKey" | "console") => "API key",
        Some("none") | None => "Claude Code",
        Some(other) => other,
    };
    let plan = ["subscriptionType", "plan", "planType"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned);
    account(AccountStatus::LoggedIn, label, plan, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_models_are_grouped_by_provider_and_keep_acp_selectors() {
        let text = r#"{"models":[
            {"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5","thinking":["low","high"]},
            {"provider":"anthropic","kind":"chat","id":"claude-haiku-4-5","name":"Claude Haiku 4.5"},
            {"provider":"voice","kind":"tts","id":"v1","name":"Voice"},
            {"provider":"xai-oauth","kind":"chat","id":"grok-4","selector":"xai-oauth/grok-4","name":"Grok 4"}
        ]}"#;
        let models = parse_omp_models(text).unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.selector.as_str()).collect();
        assert_eq!(
            ids,
            [
                "anthropic/claude-opus-4-5",
                "anthropic/claude-haiku-4-5",
                "xai-oauth/grok-4"
            ],
            "a missing selector is provider/id; other catalog kinds are left out"
        );
        assert_eq!(models[0].thinking, ["low", "high"]);
        assert!(parse_omp_models("{}").is_err());
        assert!(parse_omp_models("nope").is_err());
    }

    #[test]
    fn plans_come_from_usage_metadata_and_never_from_identities() {
        let text = r#"{"reports":[
            {"provider":"openai-codex","metadata":{"planType":"plus","email":"me@example.com","accountId":"acct_1"},"limits":[]},
            {"provider":"anthropic","metadata":{"email":"me@example.com"},"limits":[]},
            {"provider":"xai-oauth","metadata":{"planType":"unknown"},"limits":[]}
        ],"accountsWithoutUsage":[]}"#;
        let plans = parse_omp_plans(text);
        assert_eq!(plans.len(), 1);
        assert_eq!(plans.get("openai-codex").map(String::as_str), Some("plus"));
        assert!(parse_omp_plans("not json").is_empty());
    }

    #[test]
    fn claude_status_is_a_method_and_a_plan() {
        let out = parse_claude_status(
            r#"{"loggedIn":false,"authMethod":"none","analyticsDisabled":false}"#,
        );
        assert_eq!(out.status, AccountStatus::LoggedOut);
        let on = parse_claude_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max","email":"me@example.com","orgName":"Acme"}"#,
        );
        assert_eq!(on.status, AccountStatus::LoggedIn);
        assert_eq!(on.label, "Claude subscription");
        assert_eq!(on.plan.as_deref(), Some("max"));
        let json = serde_json::to_string(&on).unwrap();
        assert!(
            !json.contains("example.com") && !json.contains("Acme"),
            "{json}"
        );
        assert_eq!(parse_claude_status("hello").status, AccountStatus::Unknown);
        assert_eq!(parse_claude_status("{}").status, AccountStatus::Unknown);
    }

    #[test]
    fn providers_must_be_ids_the_harness_takes() {
        assert!(check_provider("omp", "openai-codex").is_ok());
        assert!(check_provider("omp", "--help").is_err());
        assert!(check_provider("omp", "a b").is_err());
        assert!(check_provider("claude", "anthropic").is_ok());
        assert!(check_provider("claude", "openai").is_err());
        assert!(check_provider("codex", "openai").is_ok());
        assert!(check_provider("codex", "anthropic").is_err());
        assert!(check_provider("gemini", "x").is_err());
    }
}
