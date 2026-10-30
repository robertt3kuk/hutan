//! Mawas, the assistant that acts, and its safeguards (ADR 0017 decision 4, protocol 0.31,
//! `docs/control.md` § Mawas): a real daemon on a temporary `HUTAN_HOME`, real client
//! connections that say they run inside a Mawas chat (`ClientInfo.origin`), and
//! `hutan-acp-fixture replay` standing in for `omp acp`. Nothing here starts a real harness or
//! calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatCreate, ChatHistory,
    ChatHistoryRequest, ControlTarget, MessageOrigin, MessageOriginKind, QueuedMessage,
    message_origin,
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
        Self::start_with(extra, |work| script(work, names)).await
    }

    /// [`start`](Self::start) with the transcripts `scripted` writes (the directory it returns,
    /// used one transcript per start of the agent).
    async fn start_with(extra: &str, scripted: impl FnOnce(&Path) -> PathBuf) -> Self {
        capture_audit_log();
        let work = tempfile::tempdir().unwrap();
        let scripted = scripted(work.path());
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
        let daemon = common::start_when_free(config).await;
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
        target_session_id: None,
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
    assert!(m.argv().is_empty(), "opening starts no agent");
    m.user.chat_send(mawas.id, "Say hello.").await.unwrap();
    wait_turn_end(&mut m.events).await;
    // Ask before acting: everything the agent can do, asking Hutan.
    let first = m.argv();
    assert!(first.iter().any(|a| a == "--approval-mode"), "{first:?}");
    assert!(!first.iter().any(|a| a == "--tools"), "{first:?}");

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

// ---- acting on another agent: target-bound grants (MAWAS-GATE, protocol 0.62, ADR 0025) --------

use hutan_core::agent::MessageRole;
use hutan_protocol::messages::{AgentReportRequest, AgentState};
use hutan_protocol::ui::SessionSendText;
use serde_json::{Value, json};

/// The words of the message Mawas sends in these tests.
const NUDGE: &str = "Please run the formatter before you commit";

/// The entries of a committed OMP transcript.
fn entries(name: &str) -> Vec<Value> {
    std::fs::read_to_string(fixtures().join(format!("{name}.jsonl")))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// One more turn of OMP: a prompt, a reply, its end.
fn acp_turn(reply: &str) -> [Value; 3] {
    [
        json!({"dir": "out", "msg": {"jsonrpc": "2.0", "method": "session/prompt", "id": 1}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "sess_fixture_1",
            "update": {"sessionUpdate": "agent_message_chunk", "messageId": format!("msg-{reply}"),
                "content": {"type": "text", "text": reply}}}}}),
        json!({"dir": "in", "msg": {"jsonrpc": "2.0", "id": 0, "result": {"stopReason": "end_turn"}}}),
    ]
}

/// OMP's `two-turns` with its first turn held after the first text until the file `release` of
/// `work` exists, then one turn for each of `more`.
fn held_turns(work: &Path, more: &[&str]) -> Vec<Value> {
    let mut script = entries("two-turns");
    // initialize, its answer, session/new, its answer, the prompt, "one" | the end of turn 1.
    script.insert(6, json!({"dir": "hold", "file": work.join("release")}));
    for reply in more {
        script.extend(acp_turn(reply));
    }
    script
}

/// Writes `transcripts` as the scripted starts of the fake agent, in this order.
fn write_script(work: &Path, transcripts: Vec<Vec<Value>>) -> PathBuf {
    let dir = work.join("script");
    std::fs::create_dir_all(&dir).unwrap();
    for (index, entries) in transcripts.iter().enumerate() {
        let lines: Vec<String> = entries.iter().map(Value::to_string).collect();
        std::fs::write(
            dir.join(format!("{:02}-custom.jsonl", index + 1)),
            lines.join("\n") + "\n",
        )
        .unwrap();
    }
    dir
}

fn mawas_origin(chat: SessionId, approval: &str) -> MessageOrigin {
    MessageOrigin {
        kind: MessageOriginKind::Mawas,
        session_id: chat,
        approval: approval.to_owned(),
    }
}

/// The error of a refusal, with the decision the daemon names.
fn decision(error: ClientError) -> (ErrorCode, String) {
    match error {
        ClientError::Remote(body) => (
            body.code,
            body.data
                .as_ref()
                .and_then(|data| data["control"]["decision"].as_str())
                .unwrap_or_default()
                .to_owned(),
        ),
        other => panic!("expected a remote error, got {other:?}"),
    }
}

impl M {
    /// A chat with the fake OMP (the next scripted start), as the user opened it.
    async fn target(&self, title: &str) -> SessionInfo {
        self.user
            .chat_create(ChatCreate {
                extra_dirs: Vec::new(),
                contain: None,
                workspace_id: None,
                cwd: self.work.path().to_owned(),
                agent: "omp".to_owned(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: Some(title.to_owned()),
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            })
            .await
            .unwrap()
    }

    /// Lets a held turn go on.
    fn release(&self) {
        std::fs::write(self.work.path().join("release"), "").unwrap();
    }

    /// The prompts the fake OMP received, in order.
    fn prompts(&self) -> Vec<Value> {
        self.received_of("session/prompt")
    }

    async fn history_of(&self, id: SessionId) -> ChatHistory {
        self.user
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: id,
                before_seq: None,
                limit: 500,
            }))
            .await
            .unwrap()
    }

    /// Waits for the next event of a chat that `pick` accepts.
    async fn chat_event(
        &mut self,
        what: &str,
        pick: impl Fn(&hutan_core::AgentEvent) -> bool,
    ) -> hutan_core::AgentEvent {
        wait_event(&mut self.events, what, |event| match event {
            Event::ChatEvent { event, .. } if pick(event) => Some((**event).clone()),
            _ => None,
        })
        .await
    }

    /// Waits until `count` more turns have ended.
    async fn turn_ends(&mut self, count: usize) {
        for _ in 0..count {
            self.chat_event("a turn to end", |e| e.kind.ends_turn())
                .await;
        }
    }

    /// Runs `call` (a request of Mawas) in the background and waits for the approval it raises.
    async fn asked<T: Send + 'static>(
        &self,
        call: impl std::future::Future<Output = Result<T, ClientError>> + Send + 'static,
    ) -> (
        ApprovalInfo,
        tokio::task::JoinHandle<Result<T, ClientError>>,
    ) {
        let handle = tokio::spawn(call);
        (self.next_approval().await, handle)
    }

    /// The user messages of a chat, with who sent each.
    async fn user_messages(&self, id: SessionId) -> Vec<(String, Option<MessageOrigin>)> {
        self.history_of(id)
            .await
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    text,
                    ..
                } => Some((text.clone(), message_origin(event))),
                _ => None,
            })
            .collect()
    }

    async fn notices_of(&self, id: SessionId) -> Vec<String> {
        self.history_of(id)
            .await
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                AgentEventKind::Notice { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_to_another_agent_asks_per_target_waits_in_the_queue_and_says_who_sent_it() {
    let mut m = M::start_with("", |work| {
        write_script(
            work,
            vec![
                held_turns(work, &["three", "four", "five"]),
                entries("plain-turn"),
            ],
        )
    })
    .await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;
    m.user.chat_send(api.id, "first").await.unwrap();
    m.chat_event("the first text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;

    // R3, R4: the card names the agent, shows every word and offers no workspace-wide option.
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_queue_add(id, NUDGE).await }
        })
        .await;
    assert_eq!(card.session_id, mawas.id);
    assert!(
        card.title
            .starts_with("Mawas asks Hutan to queue a message")
            && card.title.contains("“api”"),
        "{}",
        card.title
    );
    assert_eq!(card.detail.as_deref(), Some(NUDGE), "the whole text");
    assert_eq!(
        card.target,
        Some(ControlTarget {
            session_id: api.id,
            title: "api".into(),
            text: Some(NUDGE.into()),
        })
    );
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    assert_eq!(card.options[1].label, "Allow for this chat and this agent");
    m.answer(card.id, "allow_once").await;
    let queued = queuing.await.unwrap().unwrap();
    assert_eq!(queued.origin, Some(mawas_origin(mawas.id, "allow_once")));
    assert!(m.grants().await.is_empty(), "allowing once stores nothing");

    // R4: it is in the queue, not in a draft, and goes when the turn ends.
    let queue = m.user.chat_queue(api.id).await.unwrap();
    assert_eq!(queue.items, vec![queued.clone()]);
    assert_eq!(
        m.prompts().len(),
        1,
        "nothing was sent into the running turn"
    );
    m.release();
    m.turn_ends(2).await;
    let prompts = m.prompts();
    assert_eq!(prompts[1]["params"]["prompt"][0]["text"], NUDGE);
    assert!(
        prompts[1].to_string().contains("not written by the user"),
        "the agent is told who sent it: {}",
        prompts[1]
    );
    // … and so is the journal: the message carries its origin, and a notice says it.
    let sent = m.user_messages(api.id).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0],
        ("first".to_owned(), None),
        "the user's own message"
    );
    assert_eq!(
        sent[1],
        (NUDGE.to_owned(), Some(mawas_origin(mawas.id, "allow_once")))
    );
    assert!(
        m.notices_of(api.id)
            .await
            .iter()
            .any(|n| n.contains("from Mawas") && n.contains("approved")),
        "{:?}",
        m.notices_of(api.id).await
    );

    // *Allow for this chat and this agent* stores a grant bound to both.
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_queue_add(id, "second").await }
        })
        .await;
    m.answer(card.id, "allow_chat").await;
    let second = queuing.await.unwrap().unwrap();
    assert_eq!(second.origin, Some(mawas_origin(mawas.id, "allow_chat")));
    let grants = m.grants().await;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].tool, "chat.queue.add");
    assert_eq!(grants[0].chat_session_id, Some(mawas.id));
    assert_eq!(grants[0].target_session_id, Some(api.id));
    assert_eq!(grants[0].workspace_id, None, "never workspace-wide");
    m.turn_ends(1).await;
    // The same agent, the same chat: covered, and the message says it was under the grant.
    let third = agent.chat_queue_add(api.id, "third").await.unwrap();
    assert_eq!(third.origin, Some(mawas_origin(mawas.id, "allow_grant")));
    assert!(m.pending().await.is_empty());
    m.turn_ends(1).await;

    // Another agent asks again: a grant is per target …
    let docs = m.target("docs").await;
    let (card, denied) = m
        .asked({
            let (agent, id) = (agent.clone(), docs.id);
            async move { agent.chat_queue_add(id, NUDGE).await }
        })
        .await;
    assert_eq!(card.target.as_ref().map(|t| t.session_id), Some(docs.id));
    m.answer(card.id, "deny").await;
    forbidden(denied.await.unwrap().unwrap_err());
    // … and per Mawas chat.
    let other = m.mawas(ws, true).await;
    let other_agent = m.from(other.id).await;
    let (card, denied) = m
        .asked({
            let id = api.id;
            async move { other_agent.chat_queue_add(id, NUDGE).await }
        })
        .await;
    assert_eq!(card.session_id, other.id);
    m.answer(card.id, "deny").await;
    forbidden(denied.await.unwrap().unwrap_err());
    // Another tool toward the same agent asks too: the grant is for `chat.queue.add` only.
    let (card, denied) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_send_now(id, Some("now".into()), None).await }
        })
        .await;
    assert!(
        card.title.contains("send a message to the chat"),
        "{}",
        card.title
    );
    m.answer(card.id, "deny").await;
    forbidden(denied.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_send_and_a_send_now_to_an_idle_agent_start_a_turn_that_says_who_sent_it() {
    let mut m = M::start_with("", |work| write_script(work, vec![entries("two-turns")])).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;

    let (card, sending) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_send(id, NUDGE).await }
        })
        .await;
    assert!(
        card.title.starts_with("Mawas asks Hutan to send a message")
            && card.title.contains("“api”"),
        "{}",
        card.title
    );
    assert_eq!(card.detail.as_deref(), Some(NUDGE));
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    m.answer(card.id, "allow_once").await;
    sending.await.unwrap().unwrap();
    m.turn_ends(1).await;

    let (card, sending) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move {
                agent
                    .chat_send_now(id, Some("and now this".into()), None)
                    .await
            }
        })
        .await;
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    m.answer(card.id, "allow_once").await;
    sending.await.unwrap().unwrap();
    m.turn_ends(1).await;

    let sent = m.user_messages(api.id).await;
    assert_eq!(
        sent,
        [
            (NUDGE.to_owned(), Some(mawas_origin(mawas.id, "allow_once"))),
            (
                "and now this".to_owned(),
                Some(mawas_origin(mawas.id, "allow_once"))
            ),
        ]
    );
    // The user's queue was never touched.
    assert!(m.user.chat_queue(api.id).await.unwrap().items.is_empty());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mawas_never_acts_on_a_mawas_session_and_never_sends_more_than_the_card_shows() {
    let m = M::start_with("", |work| write_script(work, vec![entries("plain-turn")])).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let other = m.mawas(ws, true).await;
    let agent = m.from(mawas.id).await;

    // R6: no chains — toward another Mawas chat, or toward herself, whatever the request; no card.
    for victim in [other.id, mawas.id] {
        let refused = [
            agent.chat_queue_add(victim, "hi").await.map(|_| ()),
            agent.chat_send(victim, "hi").await.map(|_| ()),
            agent
                .chat_send_now(victim, Some("hi".into()), None)
                .await
                .map(|_| ()),
            agent.agent_interrupt(victim, None, false).await.map(|_| ()),
            agent.chat_interrupt(victim).await,
        ];
        for result in refused {
            assert_eq!(
                decision(result.unwrap_err()),
                (ErrorCode::Forbidden, "mawas_target".to_owned())
            );
        }
    }
    assert!(m.pending().await.is_empty(), "a refusal raises no card");

    // R8: the card shows every word, so a longer message is refused before anything is asked.
    let api = m.target("api").await;
    let too_long = "x".repeat(2049);
    for result in [
        agent
            .chat_queue_add(api.id, too_long.clone())
            .await
            .map(|_| ()),
        agent.chat_send(api.id, too_long.clone()).await.map(|_| ()),
        agent
            .chat_send_now(api.id, Some(too_long.clone()), None)
            .await
            .map(|_| ()),
    ] {
        assert_eq!(
            decision(result.unwrap_err()),
            (ErrorCode::BadRequest, "too_long".to_owned())
        );
    }
    assert!(m.pending().await.is_empty());
    // Exactly 2 KiB is fine: it asks, and shows all of it.
    let at_the_limit = "y".repeat(2048);
    let (card, denied) = m
        .asked({
            let (agent, id, text) = (agent.clone(), api.id, at_the_limit.clone());
            async move { agent.chat_queue_add(id, text).await }
        })
        .await;
    assert_eq!(card.detail.as_deref(), Some(at_the_limit.as_str()));
    m.answer(card.id, "deny").await;
    forbidden(denied.await.unwrap().unwrap_err());
    let lines = audit_lines().join("\n");
    assert!(
        lines.contains("deny_mawas_target") && lines.contains("deny_too_long"),
        "{lines}"
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mawas_never_types_into_an_agent_that_is_blocked_on_a_prompt() {
    let m = M::start("", &["plain-turn"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let shell = m.shell().await;
    let report = |state, seq| AgentReportRequest {
        pid: None,
        session_id: shell.id,
        source: "fake-hooks".into(),
        agent: "fake".into(),
        state: Some(state),
        message: Some("Allow this command?".into()),
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    };
    let reported = |request| {
        let user = m.user.clone();
        async move {
            user.call::<hutan_protocol::messages::AgentApplied>(Request::AgentReport(request))
                .await
                .unwrap();
        }
    };
    let shell_id = shell.id;
    let type_hi = move || SessionSendText {
        session_id: shell_id,
        text: "y".into(),
        enter: true,
    };

    // R5: the agent waits for the user; typing would answer its prompt for them.
    reported(report(AgentState::Blocked, 1)).await;
    let refused = agent.session_send_text(type_hi()).await.unwrap_err();
    assert_eq!(
        decision(refused),
        (ErrorCode::Forbidden, "blocked_agent".to_owned())
    );
    assert!(m.pending().await.is_empty(), "no card for it either");
    assert!(
        audit_lines()
            .iter()
            .any(|l| l.contains("deny_blocked_agent")),
        "audited"
    );

    // Not blocked: it asks as before (and always: no grant is offered).
    reported(report(AgentState::Working, 2)).await;
    let (card, typing) = m
        .asked({
            let agent = agent.clone();
            async move { agent.session_send_text(type_hi()).await }
        })
        .await;
    assert_eq!(options_of(&card), ["allow_once", "deny"]);
    m.answer(card.id, "deny").await;
    forbidden(typing.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_limit_counts_messages_and_interrupts_and_a_grant_cannot_lift_it() {
    let mut m = M::start_with("max_actions_per_turn = 2", |work| {
        // OMP's `cancel`: one turn that runs until it is cancelled.
        write_script(work, vec![entries("cancel")])
    })
    .await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;
    m.user.chat_send(api.id, "long task").await.unwrap();
    m.chat_event("the first text", |e| {
        matches!(e.kind, AgentEventKind::MessageDelta { .. })
    })
    .await;

    // Change 1: a message, change 2: an interrupt — each allowed (and the second for this chat
    // and this agent, so a grant exists).
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_queue_add(id, "one").await }
        })
        .await;
    m.answer(card.id, "allow_once").await;
    queuing.await.unwrap().unwrap();
    let (card, stopping) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.agent_interrupt(id, None, false).await }
        })
        .await;
    assert!(
        card.title.contains("interrupt the main agent"),
        "{}",
        card.title
    );
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    assert_eq!(card.target.as_ref().map(|t| t.session_id), Some(api.id));
    assert_eq!(card.target.as_ref().and_then(|t| t.text.as_deref()), None);
    m.answer(card.id, "allow_chat").await;
    stopping.await.unwrap().unwrap();
    let grants = m.grants().await;
    assert_eq!(
        (grants[0].tool.as_str(), grants[0].target_session_id),
        ("agent.interrupt", Some(api.id))
    );

    // Two changes made: the next message asks, offering no grant, as would a granted tool.
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_queue_add(id, "two").await }
        })
        .await;
    assert!(
        card.title
            .starts_with("Mawas has made 2 changes this turn."),
        "{}",
        card.title
    );
    assert_eq!(options_of(&card), ["allow_once", "deny"]);
    m.answer(card.id, "deny").await;
    forbidden(queuing.await.unwrap().unwrap_err());
    let (card, stopping) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.agent_interrupt(id, None, false).await }
        })
        .await;
    assert!(
        card.title
            .starts_with("Mawas has made 2 changes this turn."),
        "the grant for agent.interrupt does not lift the limit: {}",
        card.title
    );
    assert_eq!(options_of(&card), ["allow_once", "deny"]);
    m.answer(card.id, "deny").await;
    forbidden(stopping.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn grants_stored_before_the_target_binding_never_cover_a_message_to_an_agent() {
    let m = M::start_with("", |work| write_script(work, vec![entries("plain-turn")])).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    // What an earlier version could have left: a workspace grant and a chat grant, no target.
    let legacy = |id, tool: &str, workspace: bool| GrantRow {
        id,
        tool: tool.into(),
        workspace_id: workspace.then_some(ws),
        chat_session_id: (!workspace).then_some(mawas.id),
        target_session_id: None,
        created_ms: 1,
        created_by: "test".into(),
    };
    let m = m
        .restarted_with_grants(vec![
            legacy(201, "chat.queue.add", true),
            legacy(202, "chat.queue.add", false),
            legacy(203, "chat.send", true),
            legacy(204, "agent.interrupt", false),
        ])
        .await;
    assert_eq!(m.grants().await.len(), 4, "they stay listed until revoked");
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move { agent.chat_queue_add(id, NUDGE).await }
        })
        .await;
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    m.answer(card.id, "deny").await;
    forbidden(queuing.await.unwrap().unwrap_err());
    for tool in ["send", "interrupt"] {
        let (agent, id) = (agent.clone(), api.id);
        let (card, call) = if tool == "send" {
            m.asked(async move { agent.chat_send(id, NUDGE).await.map(|_| ()) })
                .await
        } else {
            m.asked(async move { agent.agent_interrupt(id, None, false).await.map(|_| ()) })
                .await
        };
        m.answer(card.id, "deny").await;
        forbidden(call.await.unwrap().unwrap_err());
    }

    // A grant bound to an agent survives a restart, and still covers only that agent.
    let (card, queuing) = m
        .asked({
            let (agent, id) = (agent.clone(), api.id);
            async move {
                agent
                    .chat_queue_add(id, "x")
                    .await
                    .map(|item: QueuedMessage| item.id)
            }
        })
        .await;
    m.answer(card.id, "allow_chat").await;
    queuing.await.unwrap().unwrap();
    let bound: Vec<ControlGrant> = m
        .grants()
        .await
        .into_iter()
        .filter(|grant| grant.target_session_id.is_some())
        .collect();
    assert_eq!(bound.len(), 1);
    let m = m.restarted().await;
    let after: Vec<ControlGrant> = m
        .grants()
        .await
        .into_iter()
        .filter(|grant| grant.target_session_id.is_some())
        .collect();
    assert_eq!(after, bound, "stored, not remembered");
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_review_is_bound_to_the_agent_of_the_task() {
    use hutan_core::{ReviewDecision, TaskState};
    use hutan_protocol::task::{TaskCreate, TaskUpdate};

    let m = M::start("", &["plain-turn"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    // A task that is done: it ran in a session (a shell here), which is the agent it speaks to.
    let task = m
        .user
        .task_create(TaskCreate {
            workspace_id: ws,
            title: "Write the docs".into(),
            prompt: None,
            depends_on: Vec::new(),
            placement: None,
            run: None,
            auto_start: false,
            after_policy: Default::default(),
        })
        .await
        .unwrap();
    let started = m.user.task_start(task.id).await.unwrap();
    m.user
        .task_update(TaskUpdate {
            id: task.id,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            after_policy: None,
            state: Some(TaskState::Done),
        })
        .await
        .unwrap();
    let ran = *started
        .task
        .sessions
        .last()
        .expect("the task ran in a session");

    let (card, reviewing) = m
        .asked({
            let (agent, id) = (agent.clone(), task.id);
            async move {
                agent
                    .task_review(id, ReviewDecision::Accept, Some("looks fine".into()))
                    .await
                    .map(|_| ())
            }
        })
        .await;
    assert!(card.title.contains("accept the work"), "{}", card.title);
    assert!(
        card.title.contains(&ran.to_string()),
        "names the agent: {}",
        card.title
    );
    assert_eq!(card.detail.as_deref(), Some("looks fine"));
    assert_eq!(card.target.as_ref().map(|t| t.session_id), Some(ran));
    assert_eq!(options_of(&card), ["allow_once", "allow_chat", "deny"]);
    m.answer(card.id, "allow_chat").await;
    reviewing.await.unwrap().unwrap();
    let grants = m.grants().await;
    assert_eq!(grants.len(), 1);
    assert_eq!(
        (
            grants[0].tool.as_str(),
            grants[0].target_session_id,
            grants[0].workspace_id
        ),
        ("task.review", Some(ran), None)
    );
    // The same task again: covered.
    agent
        .task_review(task.id, ReviewDecision::RequestChanges, None)
        .await
        .unwrap();
    assert!(m.pending().await.is_empty());
    m.daemon.stop().await;
}

// ---- suggested answers (ASK-SUGGEST, protocol 0.64, ADR 0025 R2) --------------------------------

use hutan_core::question::QuestionResponse;
use hutan_protocol::chat::{QuestionAnswer, QuestionList, QuestionListRequest};
use hutan_protocol::suggest::{AskSuggest, AskTarget, SuggestedAnswer};

impl M {
    /// Waits until the approval of `session` is pending and returns it.
    async fn card_of(&self, session: SessionId) -> ApprovalInfo {
        timeout(WAIT, async {
            loop {
                if let Some(info) = self
                    .pending()
                    .await
                    .into_iter()
                    .find(|a| a.session_id == session)
                {
                    return info;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("an approval never appeared")
    }

    async fn questions(&self) -> Vec<hutan_protocol::chat::QuestionInfo> {
        self.user
            .call::<QuestionList>(Request::QuestionList(QuestionListRequest::default()))
            .await
            .unwrap()
            .questions
    }

    /// Waits until a question is pending and returns it.
    async fn next_question(&self) -> hutan_protocol::chat::QuestionInfo {
        timeout(WAIT, async {
            loop {
                if let Some(q) = self.questions().await.into_iter().next() {
                    return q;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a question never appeared")
    }
}

fn suggest(
    agent: &Client,
    target: AskTarget,
    answer: SuggestedAnswer,
    reason: &str,
) -> impl std::future::Future<Output = Result<hutan_protocol::suggest::AskSuggested, ClientError>>
+ Send
+ 'static {
    let agent = agent.clone();
    let reason = reason.to_owned();
    async move {
        agent
            .ask_suggest(AskSuggest {
                target,
                answer,
                reason,
            })
            .await
    }
}

fn option_of(option_id: &str) -> SuggestedAnswer {
    SuggestedAnswer::Option {
        option_id: option_id.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mawas_suggests_an_answer_to_an_approval_and_only_the_user_answers() {
    let mut m = M::start("", &["permission-allow"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;
    m.user.chat_send(api.id, "go").await.unwrap();
    let waiting = m.card_of(api.id).await;
    let target = AskTarget::Approval { id: waiting.id };
    assert_eq!(waiting.suggestion, None);

    // Only Mawas suggests: the user's own connection, and another agent, are refused — the
    // other agent before any card.
    assert_eq!(
        decision(
            m.user
                .ask_suggest(AskSuggest {
                    target,
                    answer: option_of("allow_once"),
                    reason: "because".into(),
                })
                .await
                .unwrap_err()
        ),
        (ErrorCode::Forbidden, "not_mawas".to_owned())
    );
    let shell = m.shell().await;
    let reported = hutan_protocol::messages::AgentReportRequest {
        pid: None,
        session_id: shell.id,
        source: "fake-hooks".into(),
        agent: "fake".into(),
        state: Some(hutan_protocol::messages::AgentState::Working),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
    };
    m.user
        .call::<hutan_protocol::messages::AgentApplied>(Request::AgentReport(reported))
        .await
        .unwrap();
    let other_agent = m.from(shell.id).await;
    assert_eq!(
        decision(
            suggest(&other_agent, target, option_of("allow_once"), "because")
                .await
                .unwrap_err()
        ),
        (ErrorCode::Forbidden, "not_mawas".to_owned())
    );
    assert_eq!(m.pending().await.len(), 1, "no card for a refusal");

    // Mawas: a change like the others (it asks), allowed for this chat.
    let first = tokio::spawn(suggest(
        &agent,
        target,
        option_of("allow_once"),
        "Same edit you allowed twice today.",
    ));
    let card = m.card_of(mawas.id).await;
    assert!(
        card.title
            .contains("suggest an answer to the waiting approval"),
        "{}",
        card.title
    );
    m.answer(card.id, "allow_chat").await;
    let given = first.await.unwrap().unwrap();
    assert!(!given.replaced);
    assert_eq!(given.suggestion.from, mawas.id);
    assert_eq!(given.suggestion.answer, option_of("allow_once"));
    // It is on the request's card — and nothing was answered.
    let listed = m
        .pending()
        .await
        .into_iter()
        .find(|a| a.id == waiting.id)
        .expect("still waiting for the user");
    assert_eq!(listed.suggestion, Some(given.suggestion.clone()));
    let set = wait_event(&mut m.events, "the suggestion event", |event| match event {
        Event::AskSuggestion(change) if change.target == target && change.suggestion.is_some() => {
            Some((**change).clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(set.suggestion, Some(given.suggestion));

    // The latest replaces the previous one (covered by the grant: no card).
    let second = suggest(
        &agent,
        target,
        option_of("reject_once"),
        "On reflection, no.",
    )
    .await
    .unwrap();
    assert!(second.replaced);
    let listed = m
        .pending()
        .await
        .into_iter()
        .find(|a| a.id == waiting.id)
        .unwrap();
    assert_eq!(listed.suggestion, Some(second.suggestion));

    // Bounds and checks.
    let bad = |result: Result<_, ClientError>| match result.unwrap_err() {
        ClientError::Remote(body) => body.code,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        bad(suggest(&agent, target, option_of("no_such_option"), "x").await),
        ErrorCode::BadRequest,
        "an option the approval does not offer"
    );
    assert_eq!(
        bad(suggest(&agent, target, option_of("allow_once"), "  ").await),
        ErrorCode::BadRequest,
        "a reason is required"
    );
    assert_eq!(
        bad(suggest(&agent, target, option_of("allow_once"), &"r".repeat(401)).await),
        ErrorCode::BadRequest,
        "a reason of at most 400 characters"
    );
    assert_eq!(
        bad(suggest(&agent, target, SuggestedAnswer::Decline, "x").await),
        ErrorCode::BadRequest,
        "an approval takes an option"
    );

    // Answering stays the user's: Mawas cannot, and her suggestion answered nothing.
    let refused = agent
        .call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
            approval_id: waiting.id,
            option_id: "allow_once".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        decision(refused),
        (ErrorCode::Forbidden, "user_only".to_owned())
    );
    assert!(m.pending().await.iter().any(|a| a.id == waiting.id));

    // The user's own answer ends the request, and the suggestion goes with it.
    m.answer(waiting.id, "allow_once").await;
    let cleared = wait_event(&mut m.events, "the suggestion to go", |event| match event {
        Event::AskSuggestion(change) if change.target == target && change.suggestion.is_none() => {
            Some(())
        }
        _ => None,
    })
    .await;
    let () = cleared;
    assert_eq!(
        bad(suggest(&agent, target, option_of("allow_once"), "late").await),
        ErrorCode::NotFound,
        "nothing waits any more"
    );
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mawas_suggests_values_or_a_decline_for_a_question_and_the_user_answers() {
    let mut m = M::start("", &["question-ask"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let agent = m.from(mawas.id).await;
    let api = m.target("api").await;
    m.user.chat_send(api.id, "go").await.unwrap();
    let question = m.next_question().await;
    let target = AskTarget::Question { id: question.id };
    let field = question.spec.fields[0].id.clone();

    // Grant first: one card for the first suggestion, none after.
    let values = |choice: &str| SuggestedAnswer::Values {
        values: [(field.clone(), serde_json::json!(choice))].into(),
    };
    let first = tokio::spawn(suggest(
        &agent,
        target,
        values("SQLite"),
        "The project uses it.",
    ));
    let card = m.card_of(mawas.id).await;
    assert!(
        card.title
            .contains("suggest an answer to the waiting question")
    );
    m.answer(card.id, "allow_chat").await;
    let given = first.await.unwrap().unwrap();
    assert!(!given.replaced);
    let listed = m.next_question().await;
    assert_eq!(listed.suggestion, Some(given.suggestion));

    // Values the question would refuse are refused now, not when the user clicks.
    let unknown = SuggestedAnswer::Values {
        values: [("nope".to_owned(), serde_json::json!("x"))].into(),
    };
    match suggest(&agent, target, unknown, "x").await.unwrap_err() {
        ClientError::Remote(body) => assert_eq!(body.code, ErrorCode::BadRequest),
        other => panic!("{other:?}"),
    }
    // A decline replaces the values.
    let declined = suggest(&agent, target, SuggestedAnswer::Decline, "It can wait.")
        .await
        .unwrap();
    assert!(declined.replaced);
    assert_eq!(
        m.next_question().await.suggestion.map(|s| s.answer),
        Some(SuggestedAnswer::Decline)
    );
    // Mawas cannot answer a question either, and the suggestion answered nothing.
    let refused = agent
        .call::<Empty>(Request::QuestionAnswer(QuestionAnswer {
            question_id: question.id,
            response: QuestionResponse::Decline,
        }))
        .await
        .unwrap_err();
    assert_eq!(
        decision(refused),
        (ErrorCode::Forbidden, "user_only".to_owned())
    );
    assert_eq!(m.questions().await.len(), 1);

    // The user's answer ends it, and the suggestion goes.
    m.user
        .call::<Empty>(Request::QuestionAnswer(QuestionAnswer {
            question_id: question.id,
            response: QuestionResponse::Submit {
                values: [(field.clone(), serde_json::json!("SQLite"))].into(),
            },
        }))
        .await
        .unwrap();
    wait_event(&mut m.events, "the suggestion to go", |event| match event {
        Event::AskSuggestion(change) if change.target == target && change.suggestion.is_none() => {
            Some(())
        }
        _ => None,
    })
    .await;
    m.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mawas_does_not_suggest_answers_to_her_own_requests() {
    let m = M::start("", &["plain-turn"]).await;
    let ws = m.workspace("alpha").await;
    let mawas = m.mawas(ws, false).await;
    let target_session = m.shell().await;
    let renaming = {
        let agent = m.from(mawas.id).await;
        let id = target_session.id;
        tokio::spawn(async move { rename(&agent, id, "x").await })
    };
    let own = m.card_of(mawas.id).await;
    // A second connection of the same chat asks to suggest for it: it asks like any change, and
    // is then refused — she does not steer the user's answer to her own request.
    let agent = m.from(mawas.id).await;
    let suggesting = tokio::spawn(suggest(
        &agent,
        AskTarget::Approval { id: own.id },
        option_of("allow_once"),
        "Please.",
    ));
    let card = timeout(WAIT, async {
        loop {
            if let Some(info) = m
                .pending()
                .await
                .into_iter()
                .find(|a| a.id != own.id && a.session_id == mawas.id)
            {
                return info;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    m.answer(card.id, "allow_once").await;
    forbidden(suggesting.await.unwrap().unwrap_err());
    assert_eq!(
        m.pending()
            .await
            .iter()
            .find(|a| a.id == own.id)
            .and_then(|a| a.suggestion.clone()),
        None
    );
    m.answer(own.id, "deny").await;
    forbidden(renaming.await.unwrap().unwrap_err());
    m.daemon.stop().await;
}
