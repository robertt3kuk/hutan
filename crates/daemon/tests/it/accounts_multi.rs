//! Several accounts per harness (protocol 0.50, ADR 0015 § Amendment), end to end: a real daemon
//! on a temporary `HUTAN_HOME`, fake harness programs (shell scripts that record the environment
//! they were started with, then become the replaying fixture agents), no real login anywhere.
//!
//! What the tests pin: the registry (add, list, defaults, persistence), the variable each
//! harness is started with, the precedence session > project > harness, that a restart keeps a
//! chat's account, that login and remove run the harness's own commands with the account's home
//! set, and that Hutan never reads or follows anything inside a home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, stop, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::SessionId;
use hutan_protocol::ErrorCode;
use hutan_protocol::account::{
    AccountList, AccountStatus, DefaultSource, ManagedAccount, RemoveLogout,
};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{
    Event, Request, SessionInfo, SessionList, SessionListFilter, SessionResult, WorkspaceCreate,
    WorkspaceInfo,
};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

fn bin_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().to_owned()
}

fn acp_bin() -> PathBuf {
    let bin = bin_dir().join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

fn codex_fixture() -> PathBuf {
    acp_bin().with_file_name("hutan-codex-fixture")
}

fn acp_transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn codex_transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162")
        .join(format!("{name}.jsonl"))
}

/// A fake program at a stable path named by its content (see `harness.rs`: macOS vets every new
/// executable once, so scripts are written once, not once per test).
fn executable(name: &str, body: &str) -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let text = format!("#!/bin/sh\n{body}\n");
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{:016x}-{name}", hasher.finish()));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-{}", std::process::id(), name));
    std::fs::write(&staging, &text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// An agent that records which of the account variables it was started with (one line per
/// start: `<claude>|<omp dir>|<omp profile>|<codex>|<api key>`), then becomes `real`. The log is
/// `$HUTAN_TEST_ENVLOG` when set; a Codex, which has no `env` table, writes next to its home.
fn recording_agent(name: &str, real: &Path) -> PathBuf {
    executable(
        name,
        &format!(
            r#"log="${{HUTAN_TEST_ENVLOG:-/dev/null}}"
if [ -n "$CODEX_HOME" ]; then log="$CODEX_HOME/../../../envlog"; fi
printf '%s|%s|%s|%s\n' "$CLAUDE_CONFIG_DIR" "$PI_CODING_AGENT_DIR" "[$OMP_PROFILE]" "$CODEX_HOME" >> "$log"
exec {real:?} "$@""#
        ),
    )
}

/// `claude`: `auth status` says logged in (with a plan) whenever an account's home is set, logged
/// out for the harness's own login and for the account named `idle`; `auth login` and `auth
/// logout` record the home they ran with next to the data directory.
fn fake_claude() -> PathBuf {
    executable(
        "claude",
        r#"case "$1 $2" in
  "auth status")
    case "$CLAUDE_CONFIG_DIR" in
      "") echo '{"loggedIn":false,"authMethod":"none"}' ;;
      */idle) echo '{"loggedIn":false,"authMethod":"none"}' ;;
      *) echo '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}' ;;
    esac ;;
  "auth login") printf '%s\n' "$CLAUDE_CONFIG_DIR" >> "$CLAUDE_CONFIG_DIR/../../../claude-login.log" ;;
  "auth logout")
    printf '%s\n' "$CLAUDE_CONFIG_DIR" >> "$CLAUDE_CONFIG_DIR/../../../claude-logout.log"
    case "$CLAUDE_CONFIG_DIR" in */stubborn) echo "keychain locked" >&2; exit 1 ;; esac ;;
  *) echo "fake claude $*" ;;
esac"#,
    )
}

/// `omp`: lists a provider that depends on whether an agent directory is set.
fn fake_omp() -> PathBuf {
    executable(
        "omp",
        r#"case "$1" in
  models)
    if [ -n "$PI_CODING_AGENT_DIR" ]; then
      echo '{"models":[{"provider":"openai-codex","kind":"chat","id":"gpt-5","selector":"openai-codex/gpt-5","name":"GPT-5"}]}'
    else
      echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Opus"}]}'
    fi ;;
  usage) echo '{"reports":[],"accountsWithoutUsage":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    )
}

struct M {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

use hutan_core::Seq;

impl M {
    async fn start() -> Self {
        Self::start_with("").await
    }

    /// `extra` is appended to `daemon.toml`.
    async fn start_with(extra: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let toml = Self::toml(work.path(), extra);
        let codex = Self::codex(work.path());
        let daemon = TestDaemon::start_with(&toml, move |c| c.codex = Some(codex)).await;
        Self::around(daemon, work).await
    }

    async fn around(daemon: TestDaemon, work: tempfile::TempDir) -> Self {
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    fn envlog(work: &Path) -> PathBuf {
        work.join("envlog")
    }

    fn toml(work: &Path, extra: &str) -> String {
        let claude_agent = recording_agent("claude-agent", &acp_bin());
        let omp_agent = recording_agent("omp-agent", &acp_bin());
        let received = work.join("received.jsonl");
        let envlog = Self::envlog(work);
        let plain = acp_transcript("plain-turn");
        format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n\
             [harness.omp]\nbin = {omp_agent:?}\ncli = {omp:?}\n\
             args = [\"replay\", \"--received\", {received:?}, {plain:?}, \"acp\"]\n\
             env = {{ HUTAN_TEST_ENVLOG = {envlog:?} }}\n\n\
             [harness.claude]\nbin = {claude_agent:?}\ncli = {claude:?}\n\
             args = [\"replay\", \"--received\", {received:?}, {plain:?}, \"acp\"]\n\
             env = {{ HUTAN_TEST_ENVLOG = {envlog:?} }}\n\n{extra}",
            omp = fake_omp(),
            claude = fake_claude(),
        )
    }

    /// A Codex that records its environment, then replays the plain turn (for chats).
    fn codex(_work: &Path) -> CodexConfig {
        CodexConfig {
            program: recording_agent("codex-agent", &codex_fixture()),
            args: vec![
                "replay".to_owned(),
                codex_transcript("plain-turn").display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        }
    }

    fn base(&self) -> &Path {
        &self.daemon.paths.base
    }

    fn envlog_lines(&self) -> Vec<String> {
        // The Codex records next to the data directory, the others where the toml says.
        let mut lines = Vec::new();
        for path in [Self::envlog(self.work.path()), self.base().join("envlog")] {
            if let Ok(text) = std::fs::read_to_string(path) {
                lines.extend(text.lines().map(str::to_owned));
            }
        }
        lines
    }

    async fn list(&self) -> AccountList {
        self.client.account_overview().await.unwrap()
    }

    async fn add(&self, harness: &str, name: &str) -> ManagedAccount {
        self.client
            .account_add(harness, name, Some(name.to_uppercase()))
            .await
            .unwrap()
    }

    fn request(&self, agent: &str, account: Option<&str>) -> ChatCreate {
        ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: None,
            cwd: self.work.path().to_owned(),
            agent: agent.to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: account.map(str::to_owned),
            safety: None,
        }
    }

    async fn create(&self, agent: &str, account: Option<&str>) -> SessionInfo {
        self.try_create(self.request(agent, account)).await.unwrap()
    }

    async fn try_create(&self, req: ChatCreate) -> Result<SessionInfo, hutan_client::ClientError> {
        self.client
            .call::<SessionResult>(Request::ChatCreate(req))
            .await
            .map(|r| r.session)
    }

    async fn session(&self, id: SessionId) -> SessionInfo {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
            .into_iter()
            .find(|s| s.id == id)
            .expect("session listed")
    }

    /// The log lines of one variable: the starts so far, the last one first.
    async fn wait_lines(&self, count: usize) -> Vec<String> {
        timeout(WAIT, async {
            loop {
                let lines = self.envlog_lines();
                if lines.len() >= count {
                    return lines;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the agent never started {count} times"))
    }

    async fn stop_session(&self, id: SessionId) {
        stop(&self.client, id).await.unwrap();
        timeout(WAIT, async {
            while self.session(id).await.state.is_live() {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the session stops");
    }

    async fn project(&self, name: &str) -> WorkspaceInfo {
        let root = self.work.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.client
            .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
                name: name.into(),
                paths: vec![root],
                project_ids: Vec::new(),
                created_by: None,
            }))
            .await
            .unwrap()
    }
}

fn home_of(account: &ManagedAccount) -> PathBuf {
    PathBuf::from(&account.home)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_is_an_empty_private_home_that_the_harness_reports_on() {
    let mut m = M::start().await;
    let work = m.add("claude", "work").await;
    assert_eq!(work.label, "WORK");
    assert_eq!(
        home_of(&work),
        m.base().join("accounts/claude/work"),
        "under the data directory"
    );
    let home = home_of(&work);
    assert_eq!(
        std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0, "empty");
    wait_event(&mut m.events, "account.changed", |e| match e {
        Event::AccountChanged { harness } if harness == "claude" => Some(()),
        _ => None,
    })
    .await;

    // The harness's own login is not an account of Hutan's; each account is asked on its own.
    m.add("claude", "idle").await;
    let list = m.list().await;
    let own = list
        .accounts
        .iter()
        .find(|a| a.harness == "claude")
        .unwrap();
    assert_eq!(own.status, AccountStatus::LoggedOut);
    let names: Vec<_> = list.managed.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["idle", "work"]);
    let work = list.managed.iter().find(|a| a.name == "work").unwrap();
    assert_eq!(work.logins.len(), 1);
    assert_eq!(work.logins[0].status, AccountStatus::LoggedIn);
    assert_eq!(work.logins[0].plan.as_deref(), Some("max"));
    let idle = list.managed.iter().find(|a| a.name == "idle").unwrap();
    assert_eq!(idle.logins[0].status, AccountStatus::LoggedOut);

    // OMP and Codex homes go through their own variables too.
    m.add("omp", "second").await;
    m.add("codex", "personal").await;
    let list = m.list().await;
    let omp = list.managed.iter().find(|a| a.harness == "omp").unwrap();
    assert_eq!(
        omp.logins[0].provider, "openai-codex",
        "its own agent directory"
    );
    assert_eq!(
        list.accounts
            .iter()
            .find(|a| a.harness == "omp")
            .unwrap()
            .provider,
        "anthropic"
    );
    let codex = list.managed.iter().find(|a| a.harness == "codex").unwrap();
    // Codex is asked for this account too (its answer here is the chat transcript's, so the
    // fixture has nothing to say about the login).
    assert_eq!(codex.logins.len(), 1);
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn names_conflicts_and_unknown_things_are_refused() {
    let m = M::start().await;
    m.add("claude", "work").await;
    for (harness, name) in [
        ("claude", "work"),
        ("claude", "Work"),
        ("claude", "system"),
        ("claude", ""),
        ("claude", "a/b"),
        ("claude", "-x"),
        ("gemini", "x"),
    ] {
        let result = m.client.account_add(harness, name, None).await;
        let code = remote_code(result);
        let expected = if (harness, name) == ("claude", "work") {
            ErrorCode::Conflict
        } else {
            ErrorCode::BadRequest
        };
        assert_eq!(code, expected, "{harness} {name:?}");
    }
    let long = "x".repeat(65);
    assert_eq!(
        remote_code(m.client.account_add("omp", "ok", Some(long)).await),
        ErrorCode::BadRequest
    );
    // A folder that is not Hutan's is never taken over.
    std::fs::create_dir_all(m.base().join("accounts/omp/foreign")).unwrap();
    assert_eq!(
        remote_code(m.client.account_add("omp", "foreign", None).await),
        ErrorCode::Conflict
    );
    assert_eq!(
        remote_code(
            m.client
                .account_default("claude", Some("ghost".into()), None)
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            m.client
                .account_default(
                    "claude",
                    Some("work".into()),
                    Some("no-such-project".into())
                )
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(m.client.account_remove("claude", "ghost").await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(m.try_create(m.request("claude", Some("ghost"))).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(m.try_create(m.request("direct", Some("work"))).await),
        ErrorCode::BadRequest
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_and_its_defaults_survive_a_daemon_restart() {
    let m = M::start().await;
    m.add("claude", "work").await;
    m.add("codex", "personal").await;
    let project = m.project("web").await;
    m.client
        .account_default("claude", Some("work".into()), None)
        .await
        .unwrap();
    m.client
        .account_default("codex", Some("personal".into()), Some("web".into()))
        .await
        .unwrap();
    let before = m.list().await;
    assert_eq!(before.defaults.len(), 2);
    let project_default = before
        .defaults
        .iter()
        .find(|d| d.project.is_some())
        .unwrap();
    assert_eq!(
        project_default.project.as_deref(),
        Some(project.id.to_string().as_str())
    );
    assert_eq!(project_default.source, DefaultSource::Registry);

    let work = m.work;
    let daemon = m
        .daemon
        .restart_with({
            let codex = M::codex(work.path());
            move |c| c.codex = Some(codex)
        })
        .await;
    let m = M::around(daemon, work).await;
    let after = m.list().await;
    assert_eq!(after.defaults, before.defaults);
    assert_eq!(
        after
            .managed
            .iter()
            .map(|a| (&a.harness, &a.name, &a.label, a.created_at_ms))
            .collect::<Vec<_>>(),
        before
            .managed
            .iter()
            .map(|a| (&a.harness, &a.name, &a.label, a.created_at_ms))
            .collect::<Vec<_>>()
    );
    // A default can be cleared again.
    m.client
        .account_default("claude", None, None)
        .await
        .unwrap();
    assert_eq!(m.list().await.defaults.len(), 1);
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_harness_starts_with_its_own_variable_and_nothing_else_of_the_account() {
    let m = M::start().await;
    let claude = m.add("claude", "work").await;
    let omp = m.add("omp", "second").await;
    let codex = m.add("codex", "personal").await;

    let c = m.create("claude", Some("work")).await;
    let chat = c.chat.unwrap();
    let account = chat.account.expect("the header knows the account");
    assert_eq!(
        (account.name.as_str(), account.label.as_str()),
        ("work", "WORK")
    );
    let o = m.create("omp", Some("second")).await;
    assert_eq!(o.chat.unwrap().account.unwrap().name, "second");
    let x = m.create("codex", Some("personal")).await;
    assert_eq!(x.chat.unwrap().account.unwrap().label, "PERSONAL");

    let lines = m.wait_lines(3).await;
    let fields = |line: &str| line.split('|').map(str::to_owned).collect::<Vec<_>>();
    let claude_line = lines
        .iter()
        .map(|l| fields(l))
        .find(|f| !f[0].is_empty())
        .expect("a claude start");
    assert_eq!(claude_line[0], claude.home);
    assert_eq!(claude_line[1], "", "no OMP variable for Claude Code");
    let omp_line = lines
        .iter()
        .map(|l| fields(l))
        .find(|f| !f[1].is_empty())
        .expect("an omp start");
    assert_eq!(omp_line[1], omp.home);
    assert_eq!(
        omp_line[2], "[]",
        "OMP_PROFILE is set empty so that a profile cannot shadow the agent directory"
    );
    assert_eq!(omp_line[0], "");
    let codex_line = lines
        .iter()
        .map(|l| fields(l))
        .find(|f| !f[3].is_empty())
        .expect("a codex start");
    assert_eq!(codex_line[3], codex.home);
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_takes_its_own_choice_then_the_projects_default_then_the_harnesss() {
    let m = M::start().await;
    let work = m.add("claude", "work").await;
    let side = m.add("claude", "side").await;
    let proj = m.add("claude", "proj").await;
    let project = m.project("web").await;
    let in_project = |account: Option<&str>| {
        let mut req = m.request("claude", account);
        req.workspace_id = Some(project.id);
        req.cwd = project_root(&m, "web");
        req
    };

    // Nothing set: the harness's own login (no variable).
    let own = m.try_create(m.request("claude", None)).await.unwrap();
    assert!(own.chat.unwrap().account.is_none());
    // The harness default.
    m.client
        .account_default("claude", Some("work".into()), None)
        .await
        .unwrap();
    let a = m.try_create(m.request("claude", None)).await.unwrap();
    assert_eq!(a.chat.unwrap().account.unwrap().name, "work");
    // The project's default beats it, in that project only.
    m.client
        .account_default("claude", Some("proj".into()), Some("web".into()))
        .await
        .unwrap();
    let b = m.try_create(in_project(None)).await.unwrap();
    assert_eq!(b.chat.unwrap().account.unwrap().name, "proj");
    let elsewhere = m.try_create(m.request("claude", None)).await.unwrap();
    assert_eq!(elsewhere.chat.unwrap().account.unwrap().name, "work");
    // The session's own choice beats both, and `system` is the harness's own login.
    let c = m.try_create(in_project(Some("side"))).await.unwrap();
    assert_eq!(c.chat.unwrap().account.unwrap().name, "side");
    let d = m.try_create(in_project(Some("system"))).await.unwrap();
    assert!(d.chat.unwrap().account.is_none());
    // A project may use the harness's own login although the harness default is an account.
    m.client
        .account_default("claude", Some("system".into()), Some("web".into()))
        .await
        .unwrap();
    let e = m.try_create(in_project(None)).await.unwrap();
    assert!(e.chat.unwrap().account.is_none());

    let homes: Vec<String> = m
        .wait_lines(7)
        .await
        .iter()
        .map(|l| l.split('|').next().unwrap().to_owned())
        .collect();
    let expected = [
        String::new(),
        work.home.clone(),
        proj.home.clone(),
        work.home.clone(),
        side.home.clone(),
        String::new(),
        String::new(),
    ];
    assert_eq!(homes, expected);
    m.daemon.stop().await;
}

fn project_root(m: &M, name: &str) -> PathBuf {
    m.work.path().join(name).canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_account_the_chat_was_created_with() {
    let m = M::start().await;
    let work = m.add("claude", "work").await;
    m.client
        .account_default("claude", Some("work".into()), None)
        .await
        .unwrap();
    let created = m.create("claude", None).await;
    m.wait_lines(1).await;
    // The default changes afterwards; the chat is still `work`'s.
    m.client
        .account_default("claude", None, None)
        .await
        .unwrap();

    let id = created.id;
    let work_dir = m.work;
    let daemon = m
        .daemon
        .restart_with({
            let codex = M::codex(work_dir.path());
            move |c| c.codex = Some(codex)
        })
        .await;
    let m = M::around(daemon, work_dir).await;
    timeout(WAIT, async {
        loop {
            if m.envlog_lines().len() >= 2 {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the agent started again");
    let lines = m.envlog_lines();
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with(&format!("{}|", work.home))),
        "{lines:?}"
    );
    let chat = m.session(id).await.chat.unwrap();
    assert_eq!(chat.account.unwrap().name, "work");
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_runs_the_harnesss_command_with_the_accounts_home_in_a_terminal() {
    let mut m = M::start().await;
    let work = m.add("claude", "work").await;
    let started = m
        .client
        .account_login_as("claude", None, Some("work".into()))
        .await
        .unwrap();
    assert_eq!(started.session.title, "Log in to Claude Code (work)");
    assert_eq!(&started.session.command[1..], ["auth", "login"]);
    let log = m.base().join("claude-login.log");
    timeout(WAIT, async {
        loop {
            if let Ok(text) = std::fs::read_to_string(&log)
                && !text.is_empty()
            {
                assert_eq!(text.trim(), work.home);
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the login command ran with the account's home");
    wait_event(
        &mut m.events,
        "account.changed after the login",
        |e| match e {
            Event::AccountChanged { harness } if harness == "claude" => Some(()),
            _ => None,
        },
    )
    .await;
    // An account that does not exist is refused, not logged in as the harness's own.
    assert_eq!(
        remote_code(
            m.client
                .account_login_as("claude", None, Some("ghost".into()))
                .await
        ),
        ErrorCode::NotFound
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_logs_out_with_the_home_set_then_deletes_it_and_what_named_it() {
    let m = M::start().await;
    let work = m.add("claude", "work").await;
    m.add("claude", "other").await;
    m.client
        .account_default("claude", Some("work".into()), None)
        .await
        .unwrap();

    // A live chat on the account keeps it.
    let chat = m.create("claude", Some("work")).await;
    let refused = m.client.account_remove("claude", "work").await;
    let error = match refused {
        Err(hutan_client::ClientError::Remote(error)) => error,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("work"), "{}", error.message);
    assert!(home_of(&work).is_dir());
    m.stop_session(chat.id).await;

    // What is inside a home is never read or followed: an unreadable credential file, and a
    // link out of the home to a folder that must survive.
    let home = home_of(&work);
    let secret = home.join(".credentials.json");
    std::fs::write(&secret, "{}").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
    let outside = m.work.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "keep").unwrap();
    std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
    // Listing and starting do not trip over it either.
    assert_eq!(m.list().await.managed.len(), 2);
    let again = m.create("claude", Some("work")).await;
    m.stop_session(again.id).await;

    let removed = m.client.account_remove("claude", "work").await.unwrap();
    assert_eq!(removed.logout, RemoveLogout::Done);
    let logged_out = std::fs::read_to_string(m.base().join("claude-logout.log")).unwrap();
    assert_eq!(
        logged_out.trim(),
        work.home,
        "the harness's own logout, with the home set"
    );
    assert!(!home.exists(), "the home is gone");
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep",
        "a link inside the home is removed, never followed"
    );
    let list = m.list().await;
    assert_eq!(list.managed.len(), 1);
    assert!(
        list.defaults.is_empty(),
        "the default that named it went with it"
    );
    assert_eq!(
        remote_code(m.try_create(m.request("claude", Some("work"))).await),
        ErrorCode::NotFound
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removal_says_when_the_harness_could_not_log_out_or_had_nothing_to_log_out_of() {
    let m = M::start().await;
    m.add("claude", "stubborn").await;
    m.add("claude", "idle").await;
    m.add("omp", "second").await;
    let stubborn = m.client.account_remove("claude", "stubborn").await.unwrap();
    assert_eq!(stubborn.logout, RemoveLogout::Failed);
    let detail = stubborn.detail.unwrap();
    assert!(detail.contains("Keychain"), "{detail}");
    // Not logged in: nothing to log out of, so nothing to fail.
    let idle = m.client.account_remove("claude", "idle").await.unwrap();
    assert_eq!((idle.logout, idle.detail), (RemoveLogout::Skipped, None));
    // OMP has no logout command: its login is in the home that is deleted.
    let omp = m.client.account_remove("omp", "second").await.unwrap();
    assert_eq!(omp.logout, RemoveLogout::Skipped);
    assert!(m.list().await.managed.is_empty());
    m.daemon.stop().await;
}
