//! Mawas, the assistant that acts, and its safeguards (ADR 0017 decision 4, protocol 0.31,
//! `docs/control.md` § Mawas): a real daemon on a temporary `HUTAN_HOME`, real client
//! connections that say they run inside a Mawas chat (`ClientInfo.origin`), and
//! `hutan-acp-fixture replay` standing in for `omp acp`. Nothing here starts a real harness or
//! calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use common::{TestDaemon, WAIT, create, sh, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::{AgentEventKind, ApprovalId, Seq, SessionId, WorkspaceId};
use hutan_daemon::GatewayConfig;
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatHistory,
    ChatHistoryRequest,
};
use hutan_protocol::grants::{ControlGrant, ControlGrantRevoke, ControlGrants, ControlGrantsList};
use hutan_protocol::messages::{
    Empty, Event, Origin, Request, SessionInfo, SessionRef, SessionRename, SessionResult,
    WorkspaceCreate,
};
use hutan_storage::{ApprovalRow, GrantRow, Storage};
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};
use tracing_subscriber::fmt::MakeWriter;

/// The model the fixtures start on, so that the configured assistant needs no model switch.
const FIXTURE_MODEL: &str = "gitlab-duo/duo-chat-opus-4-6";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
}

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(
        bin.is_file(),
        "hutan-acp-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// A wrapper that records the arguments it was started with in `$ARGV_LOG` and becomes the fake
/// agent (`$FIXTURE`).
fn argv_logging_wrapper() -> PathBuf {
    let text = "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ARGV_LOG\"\nexec \"$FIXTURE\" \"$@\"\n";
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mawas-argv-wrapper");
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-mawas-argv-wrapper", std::process::id()));
    std::fs::write(&staging, text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// A directory of transcripts, used one per spawn of the agent, in this order.
fn script(dir: &Path, names: &[&str]) -> PathBuf {
    let scripted = dir.join("script");
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            fixtures().join(format!("{name}.jsonl")),
            scripted.join(format!("{:02}-{name}.jsonl", index + 1)),
        )
        .unwrap();
    }
    scripted
}

fn tweak(config: &mut hutan_daemon::DaemonConfig) {
    config.gateway = GatewayConfig {
        credentials: Some(Arc::new(MemoryStore::new()) as Arc<dyn CredentialStore>),
        http: HttpConfig {
            no_proxy: true,
            connect_retries: 1,
            retry_backoff: Duration::from_millis(5),
            ..HttpConfig::default()
        },
        discovery: Some(Vec::new()),
    };
}

// ---- the audit log, captured ----------------------------------------------------------------

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

static LOG: std::sync::LazyLock<Captured> = std::sync::LazyLock::new(Captured::default);
static LOG_INIT: Once = Once::new();

/// Routes the daemon's `hutan::audit` events (this process's only subscriber) into [`LOG`].
fn capture_audit_log() {
    LOG_INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_writer(LOG.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .try_init();
    });
}

fn audit_lines() -> Vec<String> {
    String::from_utf8_lossy(&LOG.0.lock())
        .lines()
        .filter(|line| line.contains("hutan::audit"))
        .map(str::to_owned)
        .collect()
}

// ---- the fixture world ----------------------------------------------------------------------

struct M {
    daemon: TestDaemon,
    /// Holds the scripted transcripts, the received log and the projects.
    work: tempfile::TempDir,
    user: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

impl M {
    /// A daemon with `[control] agents = "allow"` (the gate of Mawas must not follow it), the
    /// default assistant backend OMP (the fake agent replaying `names`, one transcript per
    /// start of it) and Mawas in its default mode.
    async fn start(extra: &str, names: &[&str]) -> Self {
        capture_audit_log();
        let work = tempfile::tempdir().unwrap();
        let scripted = script(work.path(), names);
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n\
             [control]\nagents = \"allow\"\napproval_timeout_secs = 20\n\n\
             [harness.omp]\nbin = {wrapper:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n\
             env = {{ ARGV_LOG = {argv:?}, FIXTURE = {bin:?} }}\n\n\
             [assistant]\nbackend = \"omp\"\nmodel = \"{FIXTURE_MODEL}\"\n{extra}\n",
            wrapper = argv_logging_wrapper(),
            bin = acp_bin(),
            received = work.path().join("received.jsonl"),
            argv = work.path().join("argv.log"),
        );
        let daemon = TestDaemon::start_with(&toml, tweak).await;
        Self::around(daemon, work).await
    }

    async fn around(daemon: TestDaemon, work: tempfile::TempDir) -> Self {
        let (user, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            user,
            events,
        }
    }

    async fn restarted(self) -> Self {
        let Self { daemon, work, .. } = self;
        let daemon = daemon.restart_with(tweak).await;
        Self::around(daemon, work).await
    }

    /// The same daemon after a restart in which `grants` were written to its database first
    /// (what an earlier version, or a hand-edit, could leave there).
    async fn restarted_with_grants(self, grants: Vec<GrantRow>) -> Self {
        let Self { daemon, work, .. } = self;
        let TestDaemon {
            _dir,
            paths,
            daemon,
        } = daemon;
        daemon.shutdown();
        timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
        let storage = Storage::open(&paths.database).unwrap();
        for grant in grants {
            storage.control_grants().insert(grant).unwrap();
        }
        storage.close();
        let mut config = hutan_daemon::DaemonConfig::new(paths.clone());
        tweak(&mut config);
        let daemon = hutan_daemon::start(config).await.unwrap();
        let daemon = TestDaemon {
            _dir,
            paths,
            daemon,
        };
        Self::around(daemon, work).await
    }

    /// A project folder of its own and a workspace holding it.
    async fn workspace(&self, name: &str) -> WorkspaceId {
        let folder = self.work.path().canonicalize().unwrap().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        self.user
            .call::<hutan_protocol::messages::WorkspaceInfo>(Request::WorkspaceCreate(
                WorkspaceCreate {
                    name: name.into(),
                    paths: vec![folder],
                    project_ids: Vec::new(),
                    created_by: None,
                },
            ))
            .await
            .unwrap()
            .id
    }

    /// Mawas of a workspace: its default chat, or a new one.
    async fn mawas(&self, workspace: WorkspaceId, new: bool) -> SessionInfo {
        self.user
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(workspace),
                new,
            }))
            .await
            .unwrap()
            .session
    }

    /// A connection that says it runs inside `session` (what `hutan mcp` of that chat does).
    async fn from(&self, session: SessionId) -> Client {
        let mut options = self.daemon.options();
        options.client.origin = Some(Origin {
            session_id: session,
        });
        Client::connect(options).await.unwrap()
    }

    async fn shell(&self) -> SessionInfo {
        create(&self.user, sh("sleep 120")).await
    }

    async fn pending(&self) -> Vec<ApprovalInfo> {
        self.user
            .call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest::default()))
            .await
            .unwrap()
            .approvals
    }

    /// Waits until a control approval is pending and returns it.
    async fn next_approval(&self) -> ApprovalInfo {
        timeout(WAIT, async {
            loop {
                if let Some(info) = self.pending().await.into_iter().next() {
                    return info;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a control approval never appeared")
    }

    async fn answer(&self, approval: ApprovalId, option: &str) {
        self.user
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: approval,
                option_id: option.to_owned(),
            }))
            .await
            .unwrap();
    }

    async fn title_of(&self, session: SessionId) -> String {
        let list: hutan_protocol::messages::SessionList = self
            .user
            .call(Request::SessionList(Default::default()))
            .await
            .unwrap();
        list.sessions
            .into_iter()
            .find(|s| s.id == session)
            .unwrap()
            .title
    }

    async fn set_setting(&self, key: &str, value: serde_json::Value) {
        self.user
            .daemon_setting_set(key, Some(value))
            .await
            .unwrap();
    }

    async fn grants(&self) -> Vec<ControlGrant> {
        self.user.control_grants(None).await.unwrap()
    }

    /// The audit rows of `kind = control`, once `count` of them are resolved.
    async fn audit(&self, count: usize) -> Vec<ApprovalRow> {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let storage = Storage::open(&self.daemon.paths.database).unwrap();
            let rows: Vec<ApprovalRow> = storage
                .approvals()
                .list(None)
                .unwrap()
                .into_iter()
                .filter(|r| r.kind == "control")
                .collect();
            storage.close();
            if rows.len() >= count && rows.iter().all(|r| r.outcome.is_some()) {
                return rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected {count} resolved control rows, have {rows:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// What the fake agent received, by method.
    fn received_of(&self, method: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.work.path().join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"] == method)
            .collect()
    }

    /// The arguments the wrapper started the agent with, one per line, all starts.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.work.path().join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

async fn rename(
    client: &Client,
    session: SessionId,
    title: &str,
) -> Result<SessionInfo, ClientError> {
    client
        .call::<SessionResult>(Request::SessionRename(SessionRename {
            session_id: session,
            title: title.to_owned(),
        }))
        .await
        .map(|r| r.session)
}

fn options_of(approval: &ApprovalInfo) -> Vec<&str> {
    approval.options.iter().map(|o| o.id.as_str()).collect()
}

fn forbidden(error: ClientError) -> hutan_protocol::ErrorBody {
    match error {
        ClientError::Remote(body) if body.code == ErrorCode::Forbidden => body,
        other => panic!("expected forbidden, got {other:?}"),
    }
}

// ---- the gate -------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_mawas_change_asks_even_when_the_policy_lets_agents_act() {
    let m = M::start("", &["plain-turn"; 2]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    assert!(mawas.chat.as_ref().unwrap().assistant);
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;

    // Reading is free.
    let list: hutan_protocol::messages::SessionList = agent
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    assert!(list.sessions.iter().any(|s| s.id == target.id));

    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "from mawas").await })
    };
    let approval = m.next_approval().await;
    assert_eq!(approval.session_id, mawas.id, "it names the Mawas chat");
    assert!(
        approval.title.starts_with("Mawas asks Hutan to rename"),
        "{}",
        approval.title
    );
    assert_eq!(
        options_of(&approval),
        ["allow_once", "allow_chat", "allow_workspace", "deny"]
    );
    assert_eq!(
        m.title_of(target.id).await,
        target.title,
        "nothing changed before the answer"
    );
    m.answer(approval.id, "allow_once").await;
    assert_eq!(asking.await.unwrap().unwrap().title, "from mawas");
    assert_eq!(m.title_of(target.id).await, "from mawas");

    // "Allow once" is once: the next one asks again, and a denial runs nothing.
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "second").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "deny").await;
    let denied = forbidden(asking.await.unwrap().unwrap_err());
    assert_eq!(denied.data.unwrap()["control"]["decision"], "deny");
    assert_eq!(m.title_of(target.id).await, "from mawas");

    // Any other agent still follows `[control] agents = "allow"`: no approval.
    let client = m.user.clone();
    let shell = m.shell().await;
    let reported: hutan_protocol::messages::AgentApplied = client
        .call(Request::AgentReport(
            hutan_protocol::messages::AgentReportRequest {
                session_id: shell.id,
                source: "test-hooks".into(),
                agent: "fake".into(),
                state: Some(hutan_protocol::messages::AgentState::Working),
                message: None,
                seq: 1,
                agent_session_id: None,
                resume_argv: None,
                pid: None,
                node: None,
            },
        ))
        .await
        .unwrap();
    assert!(reported.applied);
    let other = m.from(shell.id).await;
    rename(&other, target.id, "by another agent").await.unwrap();
    assert!(m.pending().await.is_empty());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_for_this_chat_covers_that_chat_only() {
    let m = M::start("", &["plain-turn"; 3]).await;
    let ws = m.workspace("alpha").await;
    let first = m.mawas(ws, false).await;
    let second = m.mawas(ws, true).await;
    assert_ne!(first.id, second.id);
    let target = m.shell().await;
    let one = m.from(first.id).await;
    let two = m.from(second.id).await;

    let asking = {
        let one = one.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&one, id, "a").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_chat").await;
    asking.await.unwrap().unwrap();
    let grants = m.grants().await;
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(grants[0].tool, "session.rename");
    assert_eq!(grants[0].chat_session_id, Some(first.id));
    assert_eq!(grants[0].workspace_id, None);

    // The next same tool in that chat runs without asking …
    rename(&one, target.id, "b").await.unwrap();
    assert_eq!(m.title_of(target.id).await, "b");
    assert!(m.pending().await.is_empty());
    // … the grant is for that tool: another one still asks …
    let asking = {
        let one = one.clone();
        let id = target.id;
        tokio::spawn(async move {
            one.call::<SessionResult>(Request::SessionStop(SessionRef { session_id: id }))
                .await
        })
    };
    let approval = m.next_approval().await;
    assert!(
        approval.title.starts_with("Mawas asks Hutan to stop"),
        "{}",
        approval.title
    );
    m.answer(approval.id, "deny").await;
    asking.await.unwrap().unwrap_err();
    // … and another chat, even in the same workspace, asks again.
    let asking = {
        let two = two.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&two, id, "c").await })
    };
    let approval = m.next_approval().await;
    assert_eq!(approval.session_id, second.id);
    m.answer(approval.id, "deny").await;
    forbidden(asking.await.unwrap().unwrap_err());
    assert_eq!(m.title_of(target.id).await, "b");
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn always_allow_here_covers_the_workspace_and_not_another_one() {
    let m = M::start("", &["plain-turn"; 4]).await;
    let alpha = m.workspace("alpha").await;
    let beta = m.workspace("beta").await;
    let in_alpha = m.mawas(alpha, false).await;
    let also_alpha = m.mawas(alpha, true).await;
    let in_beta = m.mawas(beta, false).await;
    let target = m.shell().await;
    let a1 = m.from(in_alpha.id).await;
    let a2 = m.from(also_alpha.id).await;
    let b = m.from(in_beta.id).await;

    let asking = {
        let a1 = a1.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&a1, id, "x").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_workspace").await;
    asking.await.unwrap().unwrap();
    let grants = m.grants().await;
    assert_eq!(grants[0].workspace_id, Some(alpha));
    assert_eq!(grants[0].chat_session_id, None);

    // Every chat of that workspace: no prompt.
    rename(&a1, target.id, "y").await.unwrap();
    rename(&a2, target.id, "z").await.unwrap();
    assert_eq!(m.title_of(target.id).await, "z");
    assert!(m.pending().await.is_empty());
    // The other workspace asks.
    let asking = {
        let b = b.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&b, id, "w").await })
    };
    let approval = m.next_approval().await;
    assert_eq!(approval.session_id, in_beta.id);
    m.answer(approval.id, "deny").await;
    forbidden(asking.await.unwrap().unwrap_err());

    // The list filters by workspace; the audit rows say who decided.
    assert_eq!(m.user.control_grants(Some(alpha)).await.unwrap().len(), 1);
    assert!(m.user.control_grants(Some(beta)).await.unwrap().is_empty());
    let rows = m.audit(4).await;
    let decisions: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| {
            (
                r.option_id.as_deref().unwrap(),
                r.decided_by.as_deref().unwrap(),
            )
        })
        .collect();
    assert!(
        decisions.contains(&("allow_workspace", "cli")),
        "{decisions:?}"
    );
    assert!(
        decisions
            .iter()
            .any(|(option, by)| *option == "allow_grant" && by.starts_with("grant:")),
        "{decisions:?}"
    );
    assert!(decisions.contains(&("deny", "cli")), "{decisions:?}");
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_destructive_request_always_asks_even_with_a_grant_in_the_database() {
    let m = M::start("", &["plain-turn"; 3]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    // A grant for `session.kill`, which no approval offers (the database is all there is), and
    // for `session.rename`, which does.
    let grant = |id, tool: &str| GrantRow {
        id,
        tool: tool.into(),
        workspace_id: Some(ws),
        chat_session_id: None,
        created_ms: 1,
        created_by: "test".into(),
    };
    let m = m
        .restarted_with_grants(vec![
            grant(101, "session.kill"),
            grant(102, "session.rename"),
        ])
        .await;
    assert_eq!(m.grants().await.len(), 2);
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;

    // The granted tool runs unasked …
    rename(&agent, target.id, "granted").await.unwrap();
    assert!(m.pending().await.is_empty());
    // … the destructive one asks anyway, and offers no grant.
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move {
            agent
                .call::<SessionResult>(Request::SessionKill(SessionRef { session_id: id }))
                .await
        })
    };
    let approval = m.next_approval().await;
    assert!(
        approval.title.starts_with("Mawas asks Hutan to kill"),
        "{}",
        approval.title
    );
    assert_eq!(options_of(&approval), ["allow_once", "deny"]);
    assert!(
        approval.detail.as_deref().unwrap().contains("always asks"),
        "{approval:?}"
    );
    // A grant option it did not offer is refused.
    let refused = m
        .user
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: approval.id,
            option_id: "allow_workspace".into(),
        }))
        .await;
    assert_eq!(common::remote_code(refused), ErrorCode::OptionNotOffered);
    m.answer(approval.id, "deny").await;
    forbidden(asking.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_can_never_list_revoke_or_answer_grants_and_approvals() {
    let m = M::start("", &["plain-turn"; 2]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;
    // One grant exists, and one approval waits.
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "x").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_workspace").await;
    asking.await.unwrap().unwrap();
    let grant = m.grants().await.remove(0);
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move {
            agent
                .call::<SessionResult>(Request::SessionStop(SessionRef { session_id: id }))
                .await
        })
    };
    let waiting = m.next_approval().await;

    // Mawas: every one of these is forbidden, the grants stay, the approval waits. (Another
    // connection of the same chat: one connection answers its requests in order.)
    let probe = m.from(mawas.id).await;
    let body = forbidden(probe.control_grants(None).await.unwrap_err());
    assert_eq!(body.data.unwrap()["control"]["decision"], "user_only");
    forbidden(probe.control_grant_revoke(grant.id).await.unwrap_err());
    forbidden(
        probe
            .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
                approval_id: waiting.id,
                option_id: "allow_once".into(),
            }))
            .await
            .unwrap_err(),
    );
    forbidden(
        probe
            .call::<ControlGrants>(Request::ControlGrantsList(ControlGrantsList {
                workspace_id: Some(ws),
            }))
            .await
            .unwrap_err(),
    );
    forbidden(
        probe
            .call::<Empty>(Request::ControlGrantRevoke(ControlGrantRevoke {
                id: grant.id,
            }))
            .await
            .unwrap_err(),
    );
    assert_eq!(m.grants().await.len(), 1);
    assert_eq!(m.pending().await.len(), 1, "nobody answered it");
    m.answer(waiting.id, "deny").await;
    asking.await.unwrap().unwrap_err();

    // Any other agent is as shut out, whatever `[control] agents` says.
    let shell = m.shell().await;
    let reported: hutan_protocol::messages::AgentApplied = m
        .user
        .call(Request::AgentReport(
            hutan_protocol::messages::AgentReportRequest {
                session_id: shell.id,
                source: "test-hooks".into(),
                agent: "fake".into(),
                state: Some(hutan_protocol::messages::AgentState::Working),
                message: None,
                seq: 1,
                agent_session_id: None,
                resume_argv: None,
                pid: None,
                node: None,
            },
        ))
        .await
        .unwrap();
    assert!(reported.applied);
    let other = m.from(shell.id).await;
    forbidden(other.control_grants(None).await.unwrap_err());
    forbidden(other.control_grant_revoke(grant.id).await.unwrap_err());

    // The user can.
    m.user.control_grant_revoke(grant.id).await.unwrap();
    assert!(m.grants().await.is_empty());
    assert_eq!(
        common::remote_code(m.user.control_grant_revoke(grant.id).await),
        ErrorCode::NotFound
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn advisor_refuses_every_change_with_the_reason_and_reads_still_work() {
    let m = M::start("mode = \"advisor\"", &["plain-turn"; 2]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;

    let error = forbidden(rename(&agent, target.id, "nope").await.unwrap_err());
    assert!(error.message.contains("Advisor mode"), "{}", error.message);
    assert!(
        error.message.contains("session.rename"),
        "{}",
        error.message
    );
    assert_eq!(error.data.unwrap()["control"]["decision"], "advisor");
    assert_eq!(m.title_of(target.id).await, target.title);
    assert!(m.pending().await.is_empty(), "nothing was even asked");
    let list: hutan_protocol::messages::SessionList = agent
        .call(Request::SessionList(Default::default()))
        .await
        .unwrap();
    assert!(!list.sessions.is_empty());
    let rows = m.audit(1).await;
    assert_eq!(rows[0].option_id.as_deref(), Some("deny_advisor"));

    // The mode is a live setting: back to Ask before acting, the same request asks.
    m.set_setting("assistant.mode", serde_json::json!("ask"))
        .await;
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "now").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_once").await;
    asking.await.unwrap().unwrap();
    // … and Advisor again refuses at once, grants or not.
    m.set_setting("assistant.mode", serde_json::json!("advisor"))
        .await;
    forbidden(rename(&agent, target.id, "again").await.unwrap_err());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_limit_makes_the_next_change_ask_even_with_a_grant() {
    let m = M::start("max_actions_per_turn = 2", &["plain-turn"; 2]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;

    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "one").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_chat").await;
    asking.await.unwrap().unwrap();
    rename(&agent, target.id, "two").await.unwrap();

    // Two changes made: the third asks, though the chat holds a grant for it.
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "three").await })
    };
    let approval = m.next_approval().await;
    assert!(
        approval
            .title
            .starts_with("Mawas has made 2 changes this turn."),
        "{}",
        approval.title
    );
    assert_eq!(
        options_of(&approval),
        ["allow_once", "deny"],
        "a grant cannot lift the limit"
    );
    m.answer(approval.id, "allow_once").await;
    asking.await.unwrap().unwrap();
    assert_eq!(m.title_of(target.id).await, "three");

    // The limit is a live setting.
    m.set_setting("assistant.max_actions_per_turn", serde_json::json!(100))
        .await;
    rename(&agent, target.id, "four").await.unwrap();
    assert!(m.pending().await.is_empty());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_decision_is_audited_in_the_log_and_the_table() {
    let m = M::start("", &["plain-turn"; 2]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;

    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "x").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_chat").await;
    asking.await.unwrap().unwrap();
    rename(&agent, target.id, "y").await.unwrap();
    // A user-only request, refused.
    forbidden(agent.control_grants(None).await.unwrap_err());

    let lines: Vec<String> = audit_lines()
        .into_iter()
        .filter(|line| line.contains(&mawas.id.to_string()))
        .collect();
    let has = |needle: &str| lines.iter().any(|line| line.contains(needle));
    assert!(has("mawas session.rename: allow_chat (cli)"), "{lines:#?}");
    assert!(
        has("mawas session.rename: allow_grant (grant)"),
        "{lines:#?}"
    );
    assert!(
        has("mawas control.grants.list: deny_user_only (policy)"),
        "{lines:#?}"
    );
    assert!(has("actor=\"mawas\"") || has("actor=mawas"), "{lines:#?}");
    let rows = m.audit(3).await;
    let options: Vec<&str> = rows.iter().filter_map(|r| r.option_id.as_deref()).collect();
    for expected in ["allow_chat", "allow_grant", "deny_user_only"] {
        assert!(options.contains(&expected), "{options:?}");
    }
    m.user
        .control_grant_revoke(m.grants().await[0].id)
        .await
        .unwrap();
    assert!(
        audit_lines()
            .iter()
            .any(|line| line.contains("revoked") && line.contains("session.rename")),
        "revoking is audited too"
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_survive_a_daemon_restart_and_revoking_makes_it_ask_again() {
    let m = M::start("", &["plain-turn"; 5]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target = m.shell().await;
    let agent = m.from(mawas.id).await;
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "x").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "allow_workspace").await;
    asking.await.unwrap().unwrap();
    let before = m.grants().await;
    assert_eq!(before.len(), 1);

    let m = m.restarted().await;
    assert_eq!(m.grants().await, before, "stored, not remembered");
    // A new chat of that workspace after the restart: covered, no prompt (the shell session of
    // the target is restored by the daemon too).
    let again = m.mawas(ws, true).await;
    let agent = m.from(again.id).await;
    let target = m
        .user
        .call::<hutan_protocol::messages::SessionList>(Request::SessionList(Default::default()))
        .await
        .unwrap()
        .sessions
        .into_iter()
        .find(|s| s.id == target.id)
        .unwrap_or_else(|| panic!("the target was restored"));
    rename(&agent, target.id, "after restart").await.unwrap();
    assert!(m.pending().await.is_empty());

    // Revoke: the same request asks again.
    m.user.control_grant_revoke(before[0].id).await.unwrap();
    assert!(m.grants().await.is_empty());
    let asking = {
        let agent = agent.clone();
        let id = target.id;
        tokio::spawn(async move { rename(&agent, id, "asks").await })
    };
    let approval = m.next_approval().await;
    m.answer(approval.id, "deny").await;
    forbidden(asking.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}

// ---- the mode of a running assistant -----------------------------------------------------------

fn answers(history: &ChatHistory) -> usize {
    history
        .events
        .iter()
        .filter(|e| matches!(&e.kind, AgentEventKind::MessageEnd { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_of_mode_restarts_the_agent_before_the_next_turn_and_says_so() {
    let mut m = M::start("", &["plain-turn", "load"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    // Ask before acting: everything the agent can do, asking Hutan.
    let first = m.argv();
    assert!(first.iter().any(|a| a == "--approval-mode"), "{first:?}");
    assert!(!first.iter().any(|a| a == "--tools"), "{first:?}");
    m.user.chat_send(mawas.id, "Say hello.").await.unwrap();
    wait_turn_end(&mut m.events).await;

    // Switch to Advisor: the setting is live, the agent restarts when the next message comes.
    m.set_setting("assistant.mode", serde_json::json!("advisor"))
        .await;
    assert_eq!(
        m.received_of("initialize").len(),
        1,
        "nothing restarts before the next turn"
    );
    m.user.chat_send(mawas.id, "again").await.unwrap();
    wait_turn_end(&mut m.events).await;
    assert_eq!(m.received_of("initialize").len(), 2, "a second agent");
    assert_eq!(
        m.received_of("session/load").len(),
        1,
        "it continued the stored conversation"
    );
    let argv = m.argv();
    let tools = argv.iter().position(|a| a == "--tools").expect("--tools");
    assert_eq!(argv[tools + 1], "read,grep,glob", "{argv:?}");
    // The journal tells the user.
    let history: ChatHistory = m
        .user
        .call(Request::ChatHistory(ChatHistoryRequest {
            session_id: mawas.id,
            before_seq: None,
            limit: 500,
        }))
        .await
        .unwrap();
    let notices: Vec<&str> = history
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            AgentEventKind::Notice { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        notices
            .iter()
            .any(|n| n.contains("Advisor") && n.contains("restarted")),
        "{notices:?}"
    );
    assert!(answers(&history) >= 2);
    m.daemon.stop().await;
}

async fn wait_turn_end(events: &mut broadcast::Receiver<(Seq, Event)>) {
    wait_event(events, "the turn to end", |event| match event {
        Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
        _ => None,
    })
    .await;
}
