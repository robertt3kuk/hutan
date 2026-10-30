//! Mawas memory (protocol 0.32, `docs/assistant.md` § Memory): a real daemon on a temporary
//! `HUTAN_HOME`, real client connections (the user's, and ones that say they run inside a Mawas
//! chat), `hutan-acp-fixture replay` standing in for `omp acp` and Claude Code's adapter,
//! `hutan-codex-fixture replay` for `codex app-server`, and a mock OpenAI-compatible server for
//! the provider backend. Nothing here starts a real harness or calls a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::{AgentEventKind, ApprovalId, Seq, SessionId, WorkspaceId};
use hutan_daemon::GatewayConfig;
use hutan_gateway::testing::{MockServer, Reply};
use hutan_gateway::{CredentialStore, HttpConfig, MemoryStore};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{AssistantOpen, AssistantScope};
use hutan_protocol::chat::{
    ApprovalInfo, ApprovalList, ApprovalListRequest, ApprovalRespond, ChatHistory,
    ChatHistoryRequest,
};
use hutan_protocol::memory::{
    MemoryAuthor, MemoryCategory, MemoryEntry, MemoryList, MemoryOp, MemoryProposal,
    MemoryProposalResolve, MemoryPropose, MemoryScope, MemoryState, MemoryTransition, MemoryUpdate,
    MemoryWrite, ProposalDecision, ProposalState,
};
use hutan_protocol::messages::{
    ClientKind, Empty, Event, Origin, Request, SessionInfo, SessionResult, WorkspaceCreate,
    WorkspaceInfo,
};
use hutan_protocol::provider::{ProviderAdd, ProviderResult, ProviderSetCredential};
use hutan_storage::Storage;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

/// The model the fixtures start on, so that the configured assistant needs no model switch.
const FIXTURE_MODEL: &str = "gitlab-duo/duo-chat-opus-4-6";
const SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";

fn acp_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
}

fn codex_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

fn fixture_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        bin.is_file(),
        "{name} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// A wrapper that records the arguments it was started with in `$ARGV_LOG` and becomes the fake
/// agent (`$FIXTURE`).
fn argv_logging_wrapper() -> PathBuf {
    let text = "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$ARGV_LOG\"\nexec \"$FIXTURE\" \"$@\"\n";
    let dir = std::env::temp_dir().join("hutan-test-fake-clis");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("memory-argv-wrapper");
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return path;
    }
    let staging = dir.join(format!(".{}-memory-argv-wrapper", std::process::id()));
    std::fs::write(&staging, text).unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&staging, &path).unwrap();
    path
}

/// A directory of transcripts from `from`, used one per spawn of the agent, in this order.
fn script(work: &Path, id: &str, from: &Path, names: &[&str]) -> PathBuf {
    let scripted = work.join(format!("script-{id}"));
    std::fs::create_dir_all(&scripted).unwrap();
    for (index, name) in names.iter().enumerate() {
        std::fs::copy(
            from.join(format!("{name}.jsonl")),
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
    }
}

/// Which fake agents run, and what each replays (one transcript per start of it).
#[derive(Default)]
struct Scripts {
    omp: &'static [&'static str],
    claude: &'static [&'static str],
    codex: &'static [&'static str],
}

struct W {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    user: Client,
    events: broadcast::Receiver<(Seq, Event)>,
    mock: MockServer,
}

impl W {
    /// A daemon whose `[assistant]` table has `assistant` in it (`backend = "omp"` and the
    /// fixtures' model by default).
    async fn start(assistant: &str, scripts: Scripts) -> Self {
        let work = tempfile::tempdir().unwrap();
        let wrapper = argv_logging_wrapper();
        let acp = fixture_bin("hutan-acp-fixture");
        let harness = |id: &str, names: &[&str]| {
            if names.is_empty() {
                return String::new();
            }
            format!(
                "[harness.{id}]\nbin = {wrapper:?}\nargs = [\"replay\", \"--received\", {received:?}, {scripted:?}, \"acp\"]\n\
                 env = {{ ARGV_LOG = {argv:?}, FIXTURE = {acp:?} }}\n\n",
                received = work.path().join(format!("{id}-received.jsonl")),
                scripted = script(work.path(), id, &acp_fixtures(), names),
                argv = work.path().join(format!("{id}-argv.log")),
            )
        };
        let codex = if scripts.codex.is_empty() {
            String::new()
        } else {
            format!(
                "[codex]\nbin = {:?}\nargs = [\"replay\", \"--received\", {:?}, {:?}, \"app-server\"]\n\n",
                fixture_bin("hutan-codex-fixture"),
                work.path().join("codex-received.jsonl"),
                script(work.path(), "codex", &codex_fixtures(), scripts.codex),
            )
        };
        let backend = if assistant.contains("backend") || assistant.contains("provider") {
            String::new()
        } else {
            format!("backend = \"omp\"\nmodel = \"{FIXTURE_MODEL}\"\n")
        };
        let toml = format!(
            "[terminal]\nenv_inherit = \"daemon\"\n\n{omp}{claude}{codex}[assistant]\n{backend}{assistant}\n",
            omp = harness("omp", scripts.omp),
            claude = harness("claude", scripts.claude),
        );
        let daemon = TestDaemon::start_with(&toml, tweak).await;
        Self::around(daemon, work).await
    }

    async fn around(daemon: TestDaemon, work: tempfile::TempDir) -> Self {
        let (user, events) = daemon.connect_subscribed().await;
        let mock = MockServer::start().await;
        Self {
            daemon,
            work,
            user,
            events,
            mock,
        }
    }

    async fn restarted(self) -> Self {
        let Self {
            daemon, work, mock, ..
        } = self;
        let daemon = daemon.restart_with(tweak).await;
        let (user, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            user,
            events,
            mock,
        }
    }

    async fn workspace(&self, name: &str) -> WorkspaceInfo {
        let folder = self.work.path().canonicalize().unwrap().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        self.user
            .call::<WorkspaceInfo>(Request::WorkspaceCreate(WorkspaceCreate {
                name: name.into(),
                paths: vec![folder],
                project_ids: Vec::new(),
                created_by: None,
            }))
            .await
            .unwrap()
    }

    /// Mawas of a workspace (its default chat, or a new one).
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

    /// The global Mawas (Home).
    async fn home_mawas(&self) -> SessionInfo {
        self.user
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            }))
            .await
            .unwrap()
            .session
    }

    /// A connection that says it runs inside `session`.
    async fn from(&self, session: SessionId) -> Client {
        let mut options = self.daemon.options();
        options.client.origin = Some(Origin {
            session_id: session,
        });
        Client::connect(options).await.unwrap()
    }

    async fn remember(
        &self,
        scope: MemoryScope,
        workspace: Option<WorkspaceId>,
        category: MemoryCategory,
        body: &str,
    ) -> MemoryEntry {
        let receipt = self
            .user
            .memory_write(MemoryWrite {
                operation_id: ulid::Ulid::generate().to_string(),
                scope,
                workspace_id: workspace,
                category,
                body: body.to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(receipt.status, "applied");
        self.entry(&receipt.entry_id).await
    }

    async fn entry(&self, id: &str) -> MemoryEntry {
        self.user.memory_history(id).await.unwrap().entry
    }

    async fn entries(&self, archived: bool) -> Vec<MemoryEntry> {
        self.user
            .memory_list(MemoryList {
                include_archived: archived,
                ..MemoryList::default()
            })
            .await
            .unwrap()
    }

    async fn pending(&self) -> Vec<ApprovalInfo> {
        self.user
            .call::<ApprovalList>(Request::ApprovalList(ApprovalListRequest::default()))
            .await
            .unwrap()
            .approvals
    }

    async fn proposals(&self, resolved: bool) -> Vec<MemoryProposal> {
        self.user.memory_proposals(resolved).await.unwrap()
    }

    async fn turn(&mut self) {
        wait_event(&mut self.events, "the turn to end", |event| match event {
            Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
            _ => None,
        })
        .await;
    }

    async fn send(&mut self, session: SessionId, text: &str) {
        self.user.chat_send(session, text).await.unwrap();
        self.turn().await;
    }

    async fn set_setting(&self, key: &str, value: Value) {
        self.user
            .daemon_setting_set(key, Some(value))
            .await
            .unwrap();
    }

    async fn history(&self, session: SessionId) -> ChatHistory {
        self.user
            .call(Request::ChatHistory(ChatHistoryRequest {
                session_id: session,
                before_seq: None,
                limit: 500,
            }))
            .await
            .unwrap()
    }

    async fn notices(&self, session: SessionId) -> Vec<String> {
        self.history(session)
            .await
            .events
            .iter()
            .filter_map(|e| match &e.kind {
                AgentEventKind::Notice { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    /// The snapshot rows of a chat, oldest first.
    fn snapshots(&self, chat: SessionId) -> Vec<hutan_storage::SnapshotRow> {
        let storage = Storage::open(&self.daemon.paths.database).unwrap();
        let rows = storage.memory().snapshots(chat).unwrap();
        storage.close();
        rows
    }

    /// What the fake `id` agent received, by method.
    fn received_of(&self, id: &str, method: &str) -> Vec<Value> {
        std::fs::read_to_string(self.work.path().join(format!("{id}-received.jsonl")))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"] == method)
            .collect()
    }

    /// The arguments the wrapper started `id` with, one per line, all starts.
    fn argv(&self, id: &str) -> Vec<String> {
        std::fs::read_to_string(self.work.path().join(format!("{id}-argv.log")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The value after `--append-system-prompt` of each start of `id`.
    fn append_prompts(&self, id: &str) -> Vec<String> {
        let argv = self.argv(id);
        argv.iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--append-system-prompt")
            .map(|(at, _)| argv[at + 1].clone())
            .collect()
    }

    fn prompt_dir(&self) -> PathBuf {
        self.daemon.paths.base.join("memory")
    }
}

fn remote(error: ClientError) -> hutan_protocol::ErrorBody {
    match error {
        ClientError::Remote(body) => body,
        other => panic!("expected a remote error, got {other:?}"),
    }
}

fn propose(op: &str, scope: MemoryScope, category: MemoryCategory, body: &str) -> MemoryPropose {
    MemoryPropose {
        operation_id: op.to_owned(),
        op: MemoryOp::Create,
        scope: Some(scope),
        workspace_id: None,
        category: Some(category),
        entry_id: None,
        expected_revision: None,
        body: Some(body.to_owned()),
    }
}

async fn answer(user: &Client, approval: ApprovalId, option: &str) {
    user.call::<Empty>(Request::ApprovalRespond(ApprovalRespond {
        approval_id: approval,
        option_id: option.to_owned(),
    }))
    .await
    .unwrap();
}

/// Every text of a `session/prompt` of the fake ACP agent, joined.
fn prompt_text(prompt: &Value) -> String {
    serde_json::to_string(&prompt["params"]["prompt"]).unwrap()
}

// ---- who may write ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_read_and_propose_but_never_write_update_archive_restore_or_resolve() {
    let w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let entry = w
        .remember(
            MemoryScope::Global,
            None,
            MemoryCategory::Profile,
            "Prefers short answers.",
        )
        .await;
    let agent = w.from(mawas.id).await;

    // Reading is free.
    let listed = agent.memory_list(MemoryList::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        agent
            .memory_history(&entry.id)
            .await
            .unwrap()
            .revisions
            .len(),
        1
    );

    let user_only = |error: ClientError| {
        let body = remote(error);
        assert_eq!(body.code, ErrorCode::Forbidden, "{body:?}");
        assert_eq!(
            body.data.as_ref().unwrap()["control"]["decision"],
            "user_only"
        );
    };
    user_only(
        agent
            .memory_write(MemoryWrite {
                operation_id: "a1".into(),
                scope: MemoryScope::Global,
                workspace_id: None,
                category: MemoryCategory::Fact,
                body: "x".into(),
            })
            .await
            .unwrap_err(),
    );
    user_only(
        agent
            .memory_update(MemoryUpdate {
                operation_id: "a2".into(),
                entry_id: entry.id.clone(),
                expected_revision: 1,
                body: "x".into(),
            })
            .await
            .unwrap_err(),
    );
    let transition = MemoryTransition {
        operation_id: "a3".into(),
        entry_id: entry.id.clone(),
        expected_revision: 1,
    };
    user_only(agent.memory_archive(transition.clone()).await.unwrap_err());
    user_only(agent.memory_restore(transition).await.unwrap_err());
    user_only(agent.memory_proposals(false).await.unwrap_err());
    // It cannot answer its own proposal.
    let proposed = agent
        .memory_propose(propose(
            "p1",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "kite",
        ))
        .await
        .unwrap();
    assert_eq!(proposed.status, "proposed");
    assert!(!proposed.applied);
    user_only(
        agent
            .memory_resolve(MemoryProposalResolve {
                proposal_id: proposed.proposal_id,
                decision: ProposalDecision::Approve,
                edited_body: None,
            })
            .await
            .unwrap_err(),
    );
    let approval = ApprovalRespond {
        approval_id: proposed.proposal_id,
        option_id: "approve".into(),
    };
    user_only(
        agent
            .call::<Empty>(Request::ApprovalRespond(approval))
            .await
            .unwrap_err(),
    );
    // Nothing moved.
    assert_eq!(w.entries(true).await.len(), 1);
    assert_eq!(w.proposals(false).await.len(), 1);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proposal_applies_only_what_the_user_reviewed_and_the_daemon_stamps_where_it_came_from() {
    let w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;

    let proposed = agent
        .memory_propose(propose(
            "p1",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "Staging is called kite.",
        ))
        .await
        .unwrap();
    assert_eq!(
        (proposed.status.as_str(), proposed.applied),
        ("proposed", false)
    );
    // Not saved: the memory is empty, and an approval waits for the user.
    assert!(w.entries(true).await.is_empty());
    let approvals = w.pending().await;
    assert_eq!(approvals.len(), 1);
    let approval = &approvals[0];
    assert_eq!(
        approval.id, proposed.proposal_id,
        "the proposal is its approval"
    );
    assert_eq!(approval.session_id, mawas.id);
    assert_eq!(
        approval
            .options
            .iter()
            .map(|o| o.id.as_str())
            .collect::<Vec<_>>(),
        ["approve", "reject"]
    );
    assert!(
        approval.title.contains("Staging is called kite."),
        "{}",
        approval.title
    );
    let listed = w.proposals(false).await;
    assert_eq!((listed.len(), listed[0].state), (1, ProposalState::Pending));
    assert_eq!(listed[0].provenance.session_id, Some(mawas.id));
    assert_eq!(listed[0].provenance.author, MemoryAuthor::Agent);

    // A repeated operation id is the same proposal, not a second one.
    let again = agent
        .memory_propose(propose(
            "p1",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "Staging is called kite.",
        ))
        .await
        .unwrap();
    assert_eq!(again.proposal_id, proposed.proposal_id);
    assert_eq!(w.pending().await.len(), 1);

    // Approving answers through the ordinary approval route and applies exactly that payload.
    answer(&w.user, proposed.proposal_id, "approve").await;
    let entries = w.entries(false).await;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.body, "Staging is called kite.");
    assert!(
        entry.inferred,
        "a model's entry is inferred until the user edits or confirms it"
    );
    assert_eq!(entry.provenance.author, MemoryAuthor::Agent);
    assert_eq!(
        entry.provenance.session_id,
        Some(mawas.id),
        "stamped by the daemon"
    );
    assert!(w.pending().await.is_empty());
    let history = w.user.memory_history(&entry.id).await.unwrap();
    assert_eq!(history.revisions[0].approval_id, Some(proposed.proposal_id));
    // Asking again after the answer reports what became of it.
    let after = agent
        .memory_propose(propose(
            "p1",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "Staging is called kite.",
        ))
        .await
        .unwrap();
    assert_eq!((after.status.as_str(), after.applied), ("applied", true));
    assert_eq!(w.entries(false).await.len(), 1);

    // The user confirms it by saving it as it is: no longer inferred, and it keeps its history.
    let confirmed = w
        .user
        .memory_update(MemoryUpdate {
            operation_id: "confirm".into(),
            entry_id: entry.id.clone(),
            expected_revision: entry.revision,
            body: entry.body.clone(),
        })
        .await
        .unwrap();
    assert_eq!(confirmed.revision, 2);
    assert!(!w.entry(&entry.id).await.inferred);
    assert_eq!(
        w.user
            .memory_history(&entry.id)
            .await
            .unwrap()
            .revisions
            .len(),
        2
    );

    // Edit & approve: the user's text is applied, and the user is its author.
    let second = agent
        .memory_propose(propose(
            "p2",
            MemoryScope::Global,
            MemoryCategory::Note,
            "draft wording",
        ))
        .await
        .unwrap();
    let resolved = w
        .user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: second.proposal_id,
            decision: ProposalDecision::Edit,
            edited_body: Some("Final wording.".into()),
        })
        .await
        .unwrap();
    assert_eq!(resolved.proposal.state, ProposalState::Applied);
    let edited = w.entry(&resolved.receipt.unwrap().entry_id).await;
    assert_eq!(edited.body, "Final wording.");
    assert!(!edited.inferred);
    assert_eq!(edited.provenance.author, MemoryAuthor::User);

    // Rejecting stores nothing and is remembered as rejected.
    let third = agent
        .memory_propose(propose(
            "p3",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "REJECTED-CLAIM",
        ))
        .await
        .unwrap();
    answer(&w.user, third.proposal_id, "reject").await;
    assert!(
        w.entries(true)
            .await
            .iter()
            .all(|e| !e.body.contains("REJECTED"))
    );
    let all = w.proposals(true).await;
    assert_eq!(
        all.iter().map(|p| p.state).collect::<Vec<_>>(),
        [
            ProposalState::Applied,
            ProposalState::Applied,
            ProposalState::Rejected
        ]
    );
    // An answered proposal cannot be answered again.
    let twice = w
        .user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: third.proposal_id,
            decision: ProposalDecision::Approve,
            edited_body: None,
        })
        .await
        .unwrap_err();
    assert_eq!(remote(twice).code, ErrorCode::InvalidState);
    // `approve` takes no new text, `edit` needs one.
    let fourth = agent
        .memory_propose(propose(
            "p4",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "four",
        ))
        .await
        .unwrap();
    for (decision, edited) in [
        (ProposalDecision::Approve, Some("x".to_owned())),
        (ProposalDecision::Edit, None),
    ] {
        let refused = w
            .user
            .memory_resolve(MemoryProposalResolve {
                proposal_id: fourth.proposal_id,
                decision,
                edited_body: edited,
            })
            .await
            .unwrap_err();
        assert_eq!(remote(refused).code, ErrorCode::BadRequest);
    }
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proposal_for_an_entry_that_moved_on_stays_visible_as_a_conflict_and_applies_nothing() {
    let w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;
    let entry = w
        .remember(MemoryScope::Global, None, MemoryCategory::Fact, "Uses jj.")
        .await;
    let change = |op: &str, revision: u64, body: &str| MemoryPropose {
        operation_id: op.to_owned(),
        op: MemoryOp::Update,
        scope: None,
        workspace_id: None,
        category: None,
        entry_id: Some(entry.id.clone()),
        expected_revision: Some(revision),
        body: Some(body.to_owned()),
    };
    // Proposed against what it read; the user edits the entry meanwhile.
    let proposed = agent
        .memory_propose(change("u1", 1, "Uses git."))
        .await
        .unwrap();
    let pending = w.proposals(false).await;
    assert_eq!(
        pending[0].current_body.as_deref(),
        Some("Uses jj."),
        "the user sees both texts"
    );
    w.user
        .memory_update(MemoryUpdate {
            operation_id: "mine".into(),
            entry_id: entry.id.clone(),
            expected_revision: 1,
            body: "Uses jj and git.".into(),
        })
        .await
        .unwrap();
    let resolved = w
        .user
        .memory_resolve(MemoryProposalResolve {
            proposal_id: proposed.proposal_id,
            decision: ProposalDecision::Approve,
            edited_body: None,
        })
        .await
        .unwrap();
    assert_eq!(resolved.proposal.state, ProposalState::Conflict);
    assert!(resolved.receipt.is_none());
    let now = w.entry(&entry.id).await;
    assert_eq!((now.revision, now.body.as_str()), (2, "Uses jj and git."));
    assert_eq!(
        w.proposals(true).await[0].state,
        ProposalState::Conflict,
        "visible"
    );
    assert!(w.pending().await.is_empty(), "its approval is over");

    // A proposal that is stale when it is made is refused with the current revision.
    let stale = agent
        .memory_propose(change("u2", 1, "Uses hg."))
        .await
        .unwrap_err();
    let body = remote(stale);
    assert_eq!(body.code, ErrorCode::Conflict);
    assert_eq!(body.data.unwrap()["current_revision"], 2);
    // So is a user's own stale update.
    let stale = w
        .user
        .memory_update(MemoryUpdate {
            operation_id: "late".into(),
            entry_id: entry.id.clone(),
            expected_revision: 1,
            body: "x".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(remote(stale).code, ErrorCode::Conflict);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn archive_and_restore_proposals_wait_for_the_user_and_keep_history() {
    let w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;
    let entry = w
        .remember(MemoryScope::Global, None, MemoryCategory::Fact, "Old fact.")
        .await;
    let archive = agent
        .memory_propose(MemoryPropose {
            operation_id: "arch".into(),
            op: MemoryOp::Archive,
            scope: None,
            workspace_id: None,
            category: None,
            entry_id: Some(entry.id.clone()),
            expected_revision: Some(1),
            body: None,
        })
        .await
        .unwrap();
    assert_eq!(
        w.entry(&entry.id).await.state,
        MemoryState::Active,
        "only proposed"
    );
    answer(&w.user, archive.proposal_id, "approve").await;
    assert_eq!(w.entry(&entry.id).await.state, MemoryState::Archived);
    assert!(w.entries(false).await.is_empty());
    assert_eq!(w.entries(true).await.len(), 1);
    // Restoring is the user's too.
    let receipt = w
        .user
        .memory_restore(MemoryTransition {
            operation_id: "rest".into(),
            entry_id: entry.id.clone(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    assert_eq!(receipt.revision, 3);
    let history = w.user.memory_history(&entry.id).await.unwrap();
    assert_eq!(
        history
            .revisions
            .iter()
            .map(|r| (r.revision, r.operation))
            .collect::<Vec<_>>(),
        [
            (3, MemoryOp::Restore),
            (2, MemoryOp::Archive),
            (1, MemoryOp::Create)
        ]
    );
    assert!(history.revisions.iter().all(|r| r.body == "Old fact."));
    w.daemon.stop().await;
}

// ---- what is refused -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn memory_is_not_a_secret_store_and_has_bounds() {
    let w = W::start(
        "memory_max_entries = 2",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;
    let write = |op: &str, body: &str| MemoryWrite {
        operation_id: op.to_owned(),
        scope: MemoryScope::Global,
        workspace_id: None,
        category: MemoryCategory::Fact,
        body: body.to_owned(),
    };

    // A credential is refused, not masked: the user's write and an agent's proposal alike.
    for body in [
        format!("my key is {SECRET}"),
        "password = hunter2hunter2".to_owned(),
        "Authorization: Bearer abcdefghijklmnopqrstuvwxyz".to_owned(),
    ] {
        let refused = remote(w.user.memory_write(write("s", &body)).await.unwrap_err());
        assert_eq!(refused.code, ErrorCode::Forbidden);
        assert!(
            refused.message.contains("Memory is not a secret store"),
            "{}",
            refused.message
        );
        assert!(!refused.message.contains(SECRET));
        let refused = remote(
            agent
                .memory_propose(propose(
                    "s",
                    MemoryScope::Global,
                    MemoryCategory::Fact,
                    &body,
                ))
                .await
                .unwrap_err(),
        );
        assert_eq!(refused.code, ErrorCode::Forbidden);
    }
    // Neither was stored anywhere.
    assert!(w.entries(true).await.is_empty());
    assert!(w.proposals(true).await.is_empty());
    assert!(w.pending().await.is_empty());

    // Size: at most 2 KiB, and some text.
    let big = "a".repeat(2 * 1024 + 1);
    assert_eq!(
        remote(w.user.memory_write(write("big", &big)).await.unwrap_err()).code,
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote(w.user.memory_write(write("none", "  ")).await.unwrap_err()).code,
        ErrorCode::BadRequest
    );
    w.user
        .memory_write(write("edge", &"a".repeat(2 * 1024)))
        .await
        .unwrap();

    // The number of entries follows the live setting.
    let first = w.user.memory_write(write("two", "two")).await.unwrap();
    let full = remote(
        w.user
            .memory_write(write("three", "three"))
            .await
            .unwrap_err(),
    );
    assert_eq!(full.code, ErrorCode::InvalidState);
    assert!(full.message.contains("at most 2"), "{}", full.message);
    w.set_setting("assistant.memory_max_entries", json!(3))
        .await;
    w.user.memory_write(write("three", "three")).await.unwrap();
    assert_eq!(w.entries(false).await.len(), 3);
    // Archiving frees a place.
    w.set_setting("assistant.memory_max_entries", json!(2))
        .await;
    w.user
        .memory_archive(MemoryTransition {
            operation_id: "arch-two".into(),
            entry_id: first.entry_id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    assert_eq!(w.entries(false).await.len(), 2);
    let over = w
        .user
        .memory_restore(MemoryTransition {
            operation_id: "rest-two".into(),
            entry_id: first.entry_id,
            expected_revision: 2,
        })
        .await
        .unwrap_err();
    assert_eq!(remote(over).code, ErrorCode::InvalidState);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_operation_id_returns_the_original_receipt_and_provenance_names_the_client() {
    let mut w = W::start("", Scripts::default()).await;
    let write = MemoryWrite {
        operation_id: "once".into(),
        scope: MemoryScope::Global,
        workspace_id: None,
        category: MemoryCategory::Profile,
        body: "Prefers short answers.".into(),
    };
    let first = w.user.memory_write(write.clone()).await.unwrap();
    let again = w
        .user
        .memory_write(MemoryWrite {
            body: "something else entirely".into(),
            ..write.clone()
        })
        .await
        .unwrap();
    assert_eq!(first, again);
    assert_eq!(w.entries(true).await.len(), 1);
    // The event says the scope changed (once).
    let changed = wait_event(&mut w.events, "memory.changed", |event| match event {
        Event::MemoryChanged {
            scope,
            scope_revision,
            ..
        } => Some((*scope, *scope_revision)),
        _ => None,
    })
    .await;
    assert_eq!(changed, (MemoryScope::Global, 1));

    let desktop = w.daemon.connect_as(ClientKind::Desktop).await;
    let receipt = desktop
        .memory_write(MemoryWrite {
            operation_id: "from-desktop".into(),
            body: "Likes tabs.".into(),
            ..write
        })
        .await
        .unwrap();
    assert_eq!(receipt.scope_revision, 2);
    let entry = w.entry(&receipt.entry_id).await;
    assert_eq!(entry.provenance.client.as_deref(), Some("desktop"));
    assert_eq!(
        w.entry(&first.entry_id).await.provenance.client.as_deref(),
        Some("cli")
    );
    assert!(entry.provenance.session_id.is_none() && !entry.inferred);

    // A workspace entry needs a project workspace; Home takes the global scope only.
    let ws = w.workspace("alpha").await;
    let note = w
        .user
        .memory_write(MemoryWrite {
            operation_id: "note".into(),
            scope: MemoryScope::Workspace,
            workspace_id: Some(ws.id),
            category: MemoryCategory::Note,
            body: "Releases come from trunk.".into(),
        })
        .await
        .unwrap();
    assert_eq!(note.scope_revision, 1, "each scope counts its own changes");
    let missing = w
        .user
        .memory_write(MemoryWrite {
            operation_id: "nowhere".into(),
            scope: MemoryScope::Workspace,
            workspace_id: Some(WorkspaceId::new()),
            category: MemoryCategory::Note,
            body: "x".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(remote(missing).code, ErrorCode::NotFound);
    let listed = w
        .user
        .memory_list(MemoryList {
            workspace_id: Some(ws.id),
            ..MemoryList::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.len(), 3, "global entries and the workspace's own");
    // Newest first inside a category; profile before fact before note.
    assert_eq!(
        listed.iter().map(|e| e.category).collect::<Vec<_>>(),
        [
            MemoryCategory::Profile,
            MemoryCategory::Profile,
            MemoryCategory::Note
        ]
    );
    assert_eq!(listed[0].body, "Likes tabs.");
    let only = w
        .user
        .memory_list(MemoryList {
            query: Some("TRUNK".into()),
            ..MemoryList::default()
        })
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_proposals_survive_a_daemon_restart() {
    let w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;
    let proposed = agent
        .memory_propose(propose(
            "p1",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "kept across restarts",
        ))
        .await
        .unwrap();
    drop(agent);
    let w = w.restarted().await;
    let approvals = w.pending().await;
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].id, proposed.proposal_id);
    assert_eq!(w.proposals(false).await.len(), 1);
    answer(&w.user, proposed.proposal_id, "approve").await;
    assert_eq!(w.entries(false).await[0].body, "kept across restarts");
    w.daemon.stop().await;
}

// ---- what a conversation is given -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_omp_assistant_gets_its_memory_in_a_private_prompt_file_never_in_argv() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"; 2],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    w.remember(
        MemoryScope::Workspace,
        Some(ws.id),
        MemoryCategory::Note,
        "Releases come from trunk.",
    )
    .await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Staging is called kite.",
    )
    .await;
    // Another workspace's note and a rejected proposal are not for this conversation.
    let other = w.workspace("beta").await;
    w.remember(
        MemoryScope::Workspace,
        Some(other.id),
        MemoryCategory::Note,
        "BETA-ONLY",
    )
    .await;
    let proposer = w.mawas(other.id, false).await;
    let agent = w.from(proposer.id).await;
    let rejected = agent
        .memory_propose(propose(
            "r",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "REJECTED-CLAIM",
        ))
        .await
        .unwrap();
    answer(&w.user, rejected.proposal_id, "reject").await;

    let mawas = w.mawas(ws.id, true).await;
    // Opening starts no agent: the agent of a Mawas chat starts with its first message.
    assert!(w.append_prompts("omp").is_empty());
    w.send(mawas.id, "first").await;
    let prompts = w.append_prompts("omp");
    assert_eq!(
        prompts.len(),
        1,
        "one start, for the chat that was written to: {prompts:?}"
    );
    let file = PathBuf::from(&prompts[0]);
    assert!(file.starts_with(w.prompt_dir()), "{file:?}");
    assert!(
        !w.argv("omp")
            .iter()
            .any(|a| a.contains("kite") || a.contains("short answers")),
        "memory never rides in argv"
    );
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
    let dir_mode = std::fs::metadata(w.prompt_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "{dir_mode:o}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("You are Mawas"),
        "Mawas's own prompt stays: {text}"
    );
    assert!(text.contains("--- Hutan memory"), "{text}");
    for wanted in [
        "Prefers short answers.",
        "Releases come from trunk.",
        "Staging is called kite.",
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
    assert!(
        !text.contains("BETA-ONLY") && !text.contains("REJECTED-CLAIM"),
        "{text}"
    );
    // Profile, then fact, then note.
    let at = |needle: &str| text.find(needle).unwrap();
    assert!(
        at("Prefers short") < at("Staging is called")
            && at("Staging is called") < at("Releases come")
    );

    // The exact bytes are recorded, with a manifest of ids and revisions.
    let rows = w.snapshots(mawas.id);
    assert_eq!(rows.len(), 1);
    assert!(
        text.ends_with(&format!("{}\n", rows[0].rendered)),
        "the file holds the recorded text"
    );
    assert_eq!(
        rows[0].content_hash,
        hutan_storage::hash_text(&rows[0].rendered)
    );
    let manifest: Value = serde_json::from_str(&rows[0].manifest_json).unwrap();
    assert_eq!(manifest["kind"], "initial");
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 3);
    assert_eq!(manifest["truncated"], false);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_are_deterministic_and_bounded_with_an_explicit_cut() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"; 2],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    for n in 0..8 {
        w.remember(
            MemoryScope::Workspace,
            Some(ws.id),
            MemoryCategory::Note,
            &format!("note {n} {}", "x".repeat(1_900)),
        )
        .await;
    }
    let first = w.mawas(ws.id, false).await;
    let second = w.mawas(ws.id, true).await;
    // The snapshot is taken when each agent starts: with its first message.
    assert!(w.snapshots(first.id).is_empty() && w.snapshots(second.id).is_empty());
    w.send(first.id, "hello").await;
    w.send(second.id, "hello").await;
    let (one, two) = (w.snapshots(first.id), w.snapshots(second.id));
    assert_eq!(one[0].rendered, two[0].rendered, "same memory, same bytes");
    assert!(
        one[0].rendered.len() <= 8 * 1024,
        "{}",
        one[0].rendered.len()
    );
    let manifest: Value = serde_json::from_str(&one[0].manifest_json).unwrap();
    assert_eq!(manifest["truncated"], true);
    let listed = manifest["entries"].as_array().unwrap().len();
    let omitted = manifest["omitted"].as_array().unwrap().len();
    assert!(
        listed >= 1 && omitted >= 1 && listed + omitted == 8,
        "{manifest}"
    );
    assert!(
        one[0].rendered.contains("more active entr"),
        "{}",
        one[0].rendered
    );
    // Newest first: the newest note is listed, the oldest left out.
    assert!(one[0].rendered.contains("note 7 "));
    assert!(!one[0].rendered.contains("note 0 "));
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_between_turns_reaches_the_next_turn_as_labelled_hutan_context() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["two-turns"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let old = w
        .remember(
            MemoryScope::Global,
            None,
            MemoryCategory::Fact,
            "Uses jj for version control.",
        )
        .await;
    let edited = w
        .remember(
            MemoryScope::Workspace,
            Some(ws.id),
            MemoryCategory::Note,
            "Deploys on friday.",
        )
        .await;
    let mawas = w.mawas(ws.id, false).await;

    // Nothing changed since the start: the first turn carries nothing of Hutan's.
    w.send(mawas.id, "first").await;
    let first = w.received_of("omp", "session/prompt");
    assert!(
        !prompt_text(&first[0]).contains("Hutan memory"),
        "{}",
        prompt_text(&first[0])
    );
    assert_eq!(w.snapshots(mawas.id).len(), 1);

    // The user adds, edits and archives; an archive is never silently left out.
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    w.user
        .memory_update(MemoryUpdate {
            operation_id: "edit".into(),
            entry_id: edited.id.clone(),
            expected_revision: 1,
            body: "Deploys on thursday.".into(),
        })
        .await
        .unwrap();
    w.user
        .memory_archive(MemoryTransition {
            operation_id: "gone".into(),
            entry_id: old.id.clone(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    w.send(mawas.id, "second").await;
    let second = prompt_text(&w.received_of("omp", "session/prompt")[1]);
    assert!(second.contains("--- Hutan memory update"), "{second}");
    assert!(second.contains("the user did not type this"), "{second}");
    assert!(
        second.contains("Added:") && second.contains("Prefers short answers."),
        "{second}"
    );
    assert!(
        second.contains("Deploys on thursday.") && second.contains("(was rev 1)"),
        "{second}"
    );
    assert!(
        second.contains("Archived (stop using these):")
            && second.contains(&format!("id={}", old.id)),
        "{second}"
    );
    // The user's words are the message; Hutan's block is apart from them.
    assert!(second.contains("second"));

    // The journal says what Hutan added, as a notice and not as user text; the record is kept.
    let notices = w.notices(mawas.id).await;
    assert!(
        notices
            .iter()
            .any(|n| n.contains("Hutan memory update")
                && n.contains("1 added, 1 changed, 1 archived")),
        "{notices:?}"
    );
    let rows = w.snapshots(mawas.id);
    assert_eq!(rows.len(), 2);
    let manifest: Value = serde_json::from_str(&rows[1].manifest_json).unwrap();
    assert_eq!(manifest["kind"], "delta");
    assert_eq!(manifest["base"], rows[0].id);
    assert_eq!(manifest["archived"], json!([old.id]));
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_omp_conversation_gets_its_recorded_snapshot_and_the_delta_since() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["plain-turn", "load"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Original fact.",
    )
    .await;
    let mawas = w.mawas(ws.id, false).await;
    w.send(mawas.id, "first").await;
    let initial = w.snapshots(mawas.id)[0].rendered.clone();
    // Memory changes; the daemon restarts; the next message wakes the assistant.
    let fact = w.entries(false).await[0].clone();
    w.user
        .memory_update(MemoryUpdate {
            operation_id: "later".into(),
            entry_id: fact.id.clone(),
            expected_revision: 1,
            body: "Changed fact.".into(),
        })
        .await
        .unwrap();
    let mut w = w.restarted().await;
    w.send(mawas.id, "again").await;
    let prompts = w.append_prompts("omp");
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert_eq!(
        prompts[0], prompts[1],
        "the same file: the recorded snapshot"
    );
    let file = std::fs::read_to_string(&prompts[1]).unwrap();
    assert!(
        file.contains("Original fact.") && !file.contains("Changed fact."),
        "{file}"
    );
    assert!(file.contains(&initial));
    // The change reaches the conversation as the delta of the first turn after the wake.
    let again = prompt_text(w.received_of("omp", "session/prompt").last().unwrap());
    assert!(
        again.contains("Changed fact.") && again.contains("--- Hutan memory update"),
        "{again}"
    );
    assert_eq!(
        w.snapshots(mawas.id).len(),
        2,
        "no new initial snapshot on resume"
    );
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_assistant_gets_developer_instructions_on_start_and_on_resume() {
    let mut w = W::start(
        "backend = \"codex\"\nmodel = \"gpt-6.1-sol\"",
        Scripts {
            codex: &["plain-turn", "resume-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    let mawas = w.mawas(ws.id, false).await;
    assert_eq!(mawas.chat.as_ref().unwrap().agent, "codex");
    assert!(w.received_of("codex", "thread/start").is_empty());
    w.send(mawas.id, "hello").await;
    let start = &w.received_of("codex", "thread/start")[0]["params"];
    let instructions = start["developerInstructions"]
        .as_str()
        .expect("developerInstructions");
    assert!(instructions.contains("--- Hutan memory"), "{instructions}");
    assert!(
        instructions.contains("Prefers short answers."),
        "{instructions}"
    );
    assert!(
        start.get("baseInstructions").is_none(),
        "Codex keeps its own: {start}"
    );
    assert_eq!(w.snapshots(mawas.id)[0].rendered, instructions);

    let turn = w.received_of("codex", "turn/start");
    let text = turn[0]["params"]["input"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("Hutan memory"),
        "nothing changed yet: {text}"
    );

    // A change, a daemon restart and the next message: the thread is resumed with the recorded
    // instructions, and the turn carries what changed.
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Staging is called kite.",
    )
    .await;
    let mut w = w.restarted().await;
    w.send(mawas.id, "again").await;
    let resume = &w.received_of("codex", "thread/resume")[0]["params"];
    assert_eq!(resume["developerInstructions"].as_str(), Some(instructions));
    let turns = w.received_of("codex", "turn/start");
    let text = turns.last().unwrap()["params"]["input"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.starts_with("again"), "{text}");
    assert!(
        text.contains("--- Hutan memory update") && text.contains("Staging is called kite."),
        "{text}"
    );
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_assistant_gets_its_memory_on_the_first_turn_only() {
    let mut w = W::start(
        &format!("backend = \"claude\"\nmodel = \"{FIXTURE_MODEL}\""),
        Scripts {
            claude: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    let mawas = w.mawas(ws.id, false).await;
    assert_eq!(mawas.chat.as_ref().unwrap().agent, "claude");
    assert!(
        !w.argv("claude").iter().any(|a| a.contains("short answers")),
        "{:?}",
        w.argv("claude")
    );
    w.send(mawas.id, "hello there").await;
    let prompt = prompt_text(&w.received_of("claude", "session/prompt")[0]);
    assert!(prompt.contains("hello there"), "{prompt}");
    assert!(prompt.contains("--- Hutan memory"), "{prompt}");
    assert!(prompt.contains("Prefers short answers."), "{prompt}");
    assert!(prompt.contains("the user did not type this"), "{prompt}");
    assert!(
        w.notices(mawas.id)
            .await
            .iter()
            .any(|n| n.contains("first turn")),
        "{:?}",
        w.notices(mawas.id).await
    );
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_assistant_gets_a_system_message_and_later_deltas() {
    let mut w = W::start("provider = \"mock\"\nmodel = \"m-1\"", Scripts::default()).await;
    w.user
        .call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
            kind: "openai-compatible".into(),
            name: "mock".into(),
            base_url: Some(format!("{}/v1", w.mock.url())),
            default_model: Some("m-1".into()),
        }))
        .await
        .unwrap();
    w.user
        .call::<ProviderResult>(Request::ProviderSetCredential(ProviderSetCredential {
            provider_id: "mock".into(),
            secret: "mock-key".into(),
        }))
        .await
        .unwrap();
    let completion = |text: &str| {
        Reply::sse([
            format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"content": text}}]})
            ),
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_owned(),
            "data: [DONE]\n\n".to_owned(),
        ])
    };
    w.mock
        .route("POST", "/v1/chat/completions", completion("ok"));
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    let mawas = w.home_mawas().await;
    assert_eq!(mawas.chat.as_ref().unwrap().agent, "assistant");
    w.send(mawas.id, "hello").await;
    let requests = w.mock.requests_to("/v1/chat/completions");
    let messages = requests[0].json()["messages"].clone();
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("You are Mawas"), "Mawas's prompt stays");
    assert!(system.contains("Prefers short answers."), "{system}");

    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Staging is called kite.",
    )
    .await;
    w.send(mawas.id, "again").await;
    let requests = w.mock.requests_to("/v1/chat/completions");
    let last = requests.last().unwrap().json()["messages"].clone();
    let user = last.as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(user.starts_with("again"), "{user}");
    assert!(
        user.contains("--- Hutan memory update") && user.contains("Staging is called kite."),
        "{user}"
    );
    assert!(
        w.notices(mawas.id)
            .await
            .iter()
            .any(|n| n.contains("1 added")),
        "{:?}",
        w.notices(mawas.id).await
    );
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_assistant_memory_off_nothing_is_injected_and_switching_it_on_catches_up() {
    let mut w = W::start(
        "memory = false",
        Scripts {
            omp: &["two-turns"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    let mawas = w.mawas(ws.id, false).await;
    assert!(
        w.append_prompts("omp").is_empty(),
        "opening starts no agent"
    );
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Fact,
        "Staging is called kite.",
    )
    .await;
    w.send(mawas.id, "first").await;
    // The plain prompt, inline, and no file, no snapshot.
    let prompts = w.append_prompts("omp");
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("You are Mawas"), "{}", prompts[0]);
    assert!(
        !w.prompt_dir().exists() || std::fs::read_dir(w.prompt_dir()).unwrap().next().is_none()
    );
    assert!(w.snapshots(mawas.id).is_empty());
    let first = prompt_text(&w.received_of("omp", "session/prompt")[0]);
    assert!(
        !first.contains("Hutan memory") && !first.contains("kite"),
        "{first}"
    );
    assert!(w.snapshots(mawas.id).is_empty());
    assert!(
        !w.notices(mawas.id)
            .await
            .iter()
            .any(|n| n.contains("memory"))
    );

    // Switched on mid-conversation: the next turn is told the whole current view, once.
    w.set_setting("assistant.memory", json!(true)).await;
    w.send(mawas.id, "second").await;
    let second = prompt_text(&w.received_of("omp", "session/prompt")[1]);
    assert!(
        second.contains("Prefers short answers.") && second.contains("Staging is called kite."),
        "{second}"
    );
    assert!(second.contains("the complete current memory"), "{second}");
    assert_eq!(w.snapshots(mawas.id).len(), 1);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_remembered_means_nothing_added_and_the_plain_prompt_stays_inline() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    w.send(mawas.id, "hello").await;
    let prompts = w.append_prompts("omp");
    assert!(prompts[0].contains("You are Mawas"), "{}", prompts[0]);
    // The empty snapshot is still recorded: it is what later deltas are measured against.
    let rows = w.snapshots(mawas.id);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].rendered.is_empty());
    w.daemon.stop().await;
}

/// Waits for the memory page's refresh signal after a proposal arrives.
#[tokio::test(flavor = "multi_thread")]
async fn a_proposal_and_its_answer_tell_clients_to_refresh_without_polling() {
    let mut w = W::start(
        "",
        Scripts {
            omp: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    let mawas = w.mawas(ws.id, false).await;
    let agent = w.from(mawas.id).await;
    let proposed = agent
        .memory_propose(propose(
            "p",
            MemoryScope::Global,
            MemoryCategory::Fact,
            "kite",
        ))
        .await
        .unwrap();
    wait_event(&mut w.events, "memory.changed for the proposal", |e| {
        matches!(e, Event::MemoryChanged { .. }).then_some(())
    })
    .await;
    let required = timeout(WAIT, async {
        loop {
            if w.pending()
                .await
                .iter()
                .any(|a| a.id == proposed.proposal_id)
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(required.is_ok());
    answer(&w.user, proposed.proposal_id, "approve").await;
    let resolved = wait_event(&mut w.events, "approval.resolved", |e| match e {
        Event::ApprovalResolved { approval_id, .. } => Some(*approval_id),
        _ => None,
    })
    .await;
    assert_eq!(resolved, proposed.proposal_id);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistant_is_told_what_the_projects_agents_md_opens_with_and_never_its_secrets() {
    let mut w = W::start(
        "backend = \"codex\"\nmodel = \"gpt-6.1-sol\"",
        Scripts {
            codex: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    std::fs::write(
        ws.roots[0].join("AGENTS.md"),
        format!(
            "# Alpha guide\n\nA tiny service. Staging key {SECRET}\n\n{}\n",
            "padding ".repeat(2_000)
        ),
    )
    .unwrap();
    let mawas = w.mawas(ws.id, false).await;
    w.send(mawas.id, "hello").await;
    let start = &w.received_of("codex", "thread/start")[0]["params"];
    let instructions = start["developerInstructions"]
        .as_str()
        .expect("developerInstructions");
    assert!(
        instructions.starts_with("--- Hutan project instructions"),
        "memory is empty, so the line stands alone: {instructions}"
    );
    assert!(
        instructions.contains("KiB) opens with: \"Alpha guide — A tiny service."),
        "{instructions}"
    );
    assert!(!instructions.contains(SECRET), "{instructions}");
    assert!(!instructions.contains("padding padding"), "{instructions}");
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_files_permission_the_assistant_is_not_told_about_agents_md() {
    let mut w = W::start(
        "backend = \"codex\"\nmodel = \"gpt-6.1-sol\"\n\n[assistant.context]\nfiles = false",
        Scripts {
            codex: &["plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let ws = w.workspace("alpha").await;
    std::fs::write(
        ws.roots[0].join("AGENTS.md"),
        "# Alpha guide\n\nA tiny service.\n",
    )
    .unwrap();
    let mawas = w.mawas(ws.id, false).await;
    w.send(mawas.id, "hello").await;
    let start = &w.received_of("codex", "thread/start")[0]["params"];
    assert!(
        start
            .get("developerInstructions")
            .is_none_or(|v| v.is_null() || !v.to_string().contains("AGENTS.md")),
        "{start}"
    );
    w.daemon.stop().await;
}

// Focus (protocol 0.61, ADR 0025): a Global Mawas chat looks at one workspace ----------------

/// A daemon whose Mawas answers from the mock provider (`extra` goes into `[assistant]`).
async fn provider_world(extra: &str) -> W {
    let w = W::start(
        &format!("provider = \"mock\"\nmodel = \"m-1\"\n{extra}"),
        Scripts::default(),
    )
    .await;
    w.user
        .call::<ProviderResult>(Request::ProviderAdd(ProviderAdd {
            kind: "openai-compatible".into(),
            name: "mock".into(),
            base_url: Some(format!("{}/v1", w.mock.url())),
            default_model: Some("m-1".into()),
        }))
        .await
        .unwrap();
    w.user
        .call::<ProviderResult>(Request::ProviderSetCredential(ProviderSetCredential {
            provider_id: "mock".into(),
            secret: "mock-key".into(),
        }))
        .await
        .unwrap();
    w.mock.route(
        "POST",
        "/v1/chat/completions",
        Reply::sse([
            format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"content": "ok"}}]})
            ),
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n".to_owned(),
            "data: [DONE]\n\n".to_owned(),
        ]),
    );
    w
}

impl W {
    /// The user's message of the newest provider request.
    fn last_user_message(&self) -> String {
        let requests = self.mock.requests_to("/v1/chat/completions");
        let messages = requests.last().unwrap().json()["messages"].clone();
        messages.as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn focus(&self, chat: SessionId, workspace: Option<WorkspaceId>) -> SessionInfo {
        self.user.assistant_focus(chat, workspace).await.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_global_chat_has_a_focus_that_is_kept_and_announced_and_only_the_user_sets_it() {
    let mut w = provider_world("").await;
    let alpha = w.workspace("alpha").await;
    let beta = w.workspace("beta").await;
    let home = w.home_mawas().await;
    let workspace_chat = w.mawas(alpha.id, false).await;
    assert_eq!(home.chat.as_ref().unwrap().focus, None);

    // Setting it answers with the session, announces it, and survives a daemon restart.
    let set = w.focus(home.id, Some(alpha.id)).await;
    assert_eq!(set.chat.as_ref().unwrap().focus, Some(alpha.id));
    wait_event(
        &mut w.events,
        "the focus to be announced",
        |event| match event {
            Event::SessionUpdated { session }
                if session.id == home.id
                    && session
                        .chat
                        .as_ref()
                        .is_some_and(|c| c.focus == Some(alpha.id)) =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    // Setting what is set changes nothing; moving it and clearing it work.
    assert_eq!(
        w.focus(home.id, Some(alpha.id)).await.chat.unwrap().focus,
        Some(alpha.id)
    );
    assert_eq!(
        w.focus(home.id, Some(beta.id)).await.chat.unwrap().focus,
        Some(beta.id)
    );
    let w = w.restarted().await;
    let listed = w.home_mawas().await;
    assert_eq!(listed.id, home.id);
    assert_eq!(listed.chat.as_ref().unwrap().focus, Some(beta.id));
    assert_eq!(w.focus(home.id, None).await.chat.unwrap().focus, None);

    // Only a Global Mawas chat has one; Home is not a workspace to focus on; things that do
    // not exist are not found.
    let refused = |error: ClientError, code: ErrorCode| {
        let body = remote(error);
        assert_eq!(body.code, code, "{body:?}");
        body
    };
    refused(
        w.user
            .assistant_focus(workspace_chat.id, Some(alpha.id))
            .await
            .unwrap_err(),
        ErrorCode::BadRequest,
    );
    let home_id = w
        .user
        .call::<hutan_protocol::messages::WorkspaceList>(Request::WorkspaceList(Empty {}))
        .await
        .unwrap()
        .workspaces
        .iter()
        .find(|w| w.kind == hutan_protocol::messages::WorkspaceKind::Home)
        .unwrap()
        .id;
    refused(
        w.user
            .assistant_focus(home.id, Some(home_id))
            .await
            .unwrap_err(),
        ErrorCode::BadRequest,
    );
    refused(
        w.user
            .assistant_focus(home.id, Some(WorkspaceId::new()))
            .await
            .unwrap_err(),
        ErrorCode::NotFound,
    );
    refused(
        w.user
            .assistant_focus(SessionId::new(), None)
            .await
            .unwrap_err(),
        ErrorCode::NotFound,
    );
    // User-only: Mawas learns her Focus from her prompt and can never move it.
    let mawas = w.from(home.id).await;
    let body = refused(
        mawas
            .assistant_focus(home.id, Some(alpha.id))
            .await
            .unwrap_err(),
        ErrorCode::Forbidden,
    );
    assert!(body.message.contains("assistant.focus"), "{body:?}");
    assert_eq!(w.home_mawas().await.chat.unwrap().focus, None);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_focus_set_before_the_first_turn_is_told_at_the_start_with_its_notes_and_agents_md() {
    // A Codex assistant is created asleep and starts with its first message, so the Focus the
    // user chose by then is part of its developer instructions.
    let mut w = W::start(
        "backend = \"codex\"\nmodel = \"gpt-6.1-sol\"",
        Scripts {
            codex: &["plain-turn", "plain-turn"],
            ..Scripts::default()
        },
    )
    .await;
    let alpha = w.workspace("alpha").await;
    let beta = w.workspace("beta").await;
    std::fs::write(
        alpha.roots[0].join("AGENTS.md"),
        "# Alpha guide\n\nA tiny service.\n",
    )
    .unwrap();
    w.remember(
        MemoryScope::Global,
        None,
        MemoryCategory::Profile,
        "Prefers short answers.",
    )
    .await;
    w.remember(
        MemoryScope::Workspace,
        Some(alpha.id),
        MemoryCategory::Note,
        "Alpha deploys on friday.",
    )
    .await;
    w.remember(
        MemoryScope::Workspace,
        Some(beta.id),
        MemoryCategory::Note,
        "Beta is frozen.",
    )
    .await;
    let home = w.home_mawas().await;
    w.focus(home.id, Some(alpha.id)).await;
    w.send(home.id, "hello").await;
    let starts = w.received_of("codex", "thread/start");
    let system = starts[0]["params"]["developerInstructions"]
        .as_str()
        .expect("developerInstructions")
        .to_owned();
    assert!(system.contains("Prefers short answers."), "{system}");
    assert!(system.contains("Alpha deploys on friday."), "{system}");
    assert!(!system.contains("Beta is frozen."), "{system}");
    assert!(
        system.contains("--- Hutan focus") && system.contains("Focus: \"alpha\""),
        "{system}"
    );
    assert!(
        system.contains("the focused workspace alpha's AGENTS.md")
            && system.contains("opens with: \"Alpha guide — A tiny service.\""),
        "{system}"
    );
    assert!(
        system.contains("not a permission") && system.contains("you cannot change it"),
        "the block says what a Focus is not: {system}"
    );
    // Told at the start, so the first turn adds nothing.
    assert!(
        !w.notices(home.id).await.iter().any(|n| n.contains("focus")),
        "{:?}",
        w.notices(home.id).await
    );
    // A Global chat with no Focus is told nothing about one.
    let plain = w.mawas_new_global().await;
    w.send(plain.id, "hello").await;
    let starts = w.received_of("codex", "thread/start");
    assert_eq!(starts.len(), 2);
    let system = starts[1]["params"]["developerInstructions"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(!system.contains("Hutan focus"), "{system}");
    assert!(!system.contains("Alpha deploys"), "{system}");
    w.daemon.stop().await;
}

impl W {
    /// One more Global Mawas chat.
    async fn mawas_new_global(&self) -> SessionInfo {
        self.user
            .call::<SessionResult>(Request::AssistantOpen(AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: true,
            }))
            .await
            .unwrap()
            .session
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_the_focus_reaches_the_next_turn_as_a_delta_with_notes_and_project_line() {
    let mut w = provider_world("").await;
    let alpha = w.workspace("alpha").await;
    let beta = w.workspace("beta").await;
    std::fs::write(
        beta.roots[0].join("AGENTS.md"),
        "# Beta guide\n\nThe batch jobs.\n",
    )
    .unwrap();
    w.remember(
        MemoryScope::Workspace,
        Some(alpha.id),
        MemoryCategory::Note,
        "Alpha deploys on friday.",
    )
    .await;
    w.remember(
        MemoryScope::Workspace,
        Some(beta.id),
        MemoryCategory::Note,
        "Beta is frozen.",
    )
    .await;
    let home = w.home_mawas().await;
    w.send(home.id, "hello").await;
    let at = w.snapshots(home.id).len();

    // Nothing moved: nothing is added.
    w.send(home.id, "again").await;
    assert!(
        !w.last_user_message().contains("--- Hutan"),
        "{}",
        w.last_user_message()
    );
    assert_eq!(w.snapshots(home.id).len(), at);

    // A Focus on alpha: the next turn carries it, its note, and says so in the journal. Nothing
    // restarts and no chat is created.
    w.focus(home.id, Some(alpha.id)).await;
    w.send(home.id, "now alpha").await;
    let user = w.last_user_message();
    assert!(user.starts_with("now alpha"), "{user}");
    assert!(user.contains("--- Hutan focus update"), "{user}");
    assert!(user.contains("Focus is now \"alpha\""), "{user}");
    assert!(user.contains("was All workspaces"), "{user}");
    assert!(
        user.contains("--- Hutan memory update") && user.contains("Alpha deploys on friday."),
        "{user}"
    );
    assert!(!user.contains("Beta is frozen."), "{user}");
    assert!(
        w.notices(home.id)
            .await
            .iter()
            .any(|n| n.contains("Hutan focus update")
                && n.contains("alpha")
                && n.contains("1 added")),
        "{:?}",
        w.notices(home.id).await
    );
    // It is told once.
    w.send(home.id, "still alpha").await;
    assert!(
        !w.last_user_message().contains("--- Hutan"),
        "{}",
        w.last_user_message()
    );

    // A note of the focused workspace changes meanwhile: the ordinary memory delta.
    w.remember(
        MemoryScope::Workspace,
        Some(alpha.id),
        MemoryCategory::Fact,
        "Alpha uses sqlite.",
    )
    .await;
    w.send(home.id, "a note").await;
    let user = w.last_user_message();
    assert!(
        user.contains("Alpha uses sqlite.") && !user.contains("Hutan focus update"),
        "{user}"
    );

    // A note of a workspace that is not focused does not reach the chat.
    w.remember(
        MemoryScope::Workspace,
        Some(beta.id),
        MemoryCategory::Fact,
        "Beta is also frozen.",
    )
    .await;
    w.send(home.id, "unrelated").await;
    assert!(!w.last_user_message().contains("Beta is also frozen."));

    // Moving it to beta: beta's note and project line arrive; alpha's entries are out of focus
    // (still active in the memory, so never "archived").
    w.focus(home.id, Some(beta.id)).await;
    w.send(home.id, "now beta").await;
    let user = w.last_user_message();
    assert!(
        user.contains("Focus is now \"beta\"") && user.contains("was \"alpha\""),
        "{user}"
    );
    assert!(
        user.contains("the focused workspace beta's AGENTS.md"),
        "{user}"
    );
    assert!(user.contains("The batch jobs."), "{user}");
    assert!(
        user.contains("Beta is frozen.") && user.contains("Beta is also frozen."),
        "{user}"
    );
    assert!(
        user.contains("No longer in focus") && user.contains("Alpha deploys on friday."),
        "{user}"
    );
    assert!(!user.contains("Archived (stop using these)"), "{user}");

    // Clearing it: All workspaces; beta's entries leave the view.
    w.focus(home.id, None).await;
    w.send(home.id, "all").await;
    let user = w.last_user_message();
    assert!(
        user.contains("Focus is cleared: All workspaces (was \"beta\")"),
        "{user}"
    );
    assert!(
        user.contains("No longer in focus") && user.contains("Beta is frozen."),
        "{user}"
    );
    w.send(home.id, "quiet").await;
    assert!(
        !w.last_user_message().contains("--- Hutan"),
        "{}",
        w.last_user_message()
    );

    // The Focus the conversation knows is what its last snapshot was taken for.
    let rows = w.snapshots(home.id);
    let focus_of = |row: &hutan_storage::SnapshotRow| {
        serde_json::from_str::<Value>(&row.manifest_json).unwrap()["workspace_id"].clone()
    };
    assert_eq!(focus_of(rows.last().unwrap()), Value::Null);
    assert_eq!(focus_of(&rows[rows.len() - 2]), json!(beta.id.to_string()));
    // A workspace chat is untouched by all of this: no focus, its own notes.
    let workspace_chat = w.mawas(alpha.id, false).await;
    assert_eq!(workspace_chat.chat.unwrap().focus, None);
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_memory_off_the_focus_is_still_told_and_only_the_focus() {
    let mut w = provider_world("memory = false").await;
    let alpha = w.workspace("alpha").await;
    std::fs::write(
        alpha.roots[0].join("AGENTS.md"),
        "# Alpha guide\n\nA tiny service.\n",
    )
    .unwrap();
    w.remember(
        MemoryScope::Workspace,
        Some(alpha.id),
        MemoryCategory::Note,
        "Alpha deploys on friday.",
    )
    .await;
    let home = w.home_mawas().await;
    w.send(home.id, "hello").await;
    assert!(
        !w.last_user_message().contains("--- Hutan"),
        "memory is off"
    );
    w.focus(home.id, Some(alpha.id)).await;
    w.send(home.id, "now alpha").await;
    let user = w.last_user_message();
    assert!(user.contains("Focus is now \"alpha\""), "{user}");
    assert!(
        user.contains("opens with: \"Alpha guide — A tiny service.\""),
        "{user}"
    );
    assert!(
        !user.contains("Alpha deploys on friday."),
        "memory stays off: {user}"
    );
    assert!(!user.contains("--- Hutan memory"), "{user}");
    w.send(home.id, "again").await;
    assert!(
        !w.last_user_message().contains("--- Hutan"),
        "{}",
        w.last_user_message()
    );
    w.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_focused_workspace_that_forbids_files_has_its_agents_md_left_out_but_the_focus_named() {
    let mut w = provider_world("\n[assistant.workspaces.alpha]\nfiles = false").await;
    let alpha = w.workspace("alpha").await;
    std::fs::write(
        alpha.roots[0].join("AGENTS.md"),
        "# Alpha guide\n\nA tiny service.\n",
    )
    .unwrap();
    let home = w.home_mawas().await;
    w.send(home.id, "hello").await;
    w.focus(home.id, Some(alpha.id)).await;
    w.send(home.id, "now alpha").await;
    let user = w.last_user_message();
    assert!(user.contains("Focus is now \"alpha\""), "{user}");
    assert!(!user.contains("Alpha guide"), "{user}");
    // A Focus is no permission: Home's own context permissions are unchanged.
    w.daemon.stop().await;
}
