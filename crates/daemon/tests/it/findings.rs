//! `agents.findings` (protocol 0.70) end to end: a real daemon on a temporary `HUTAN_HOME`.
//!
//! The journals of three chats (Codex, OMP and Claude Code, in two workspaces) are written into
//! the daemon's database the way the daemons of those harnesses write them, with the commands,
//! exit codes, final messages and file changes each rule needs; the daemon then reads them back
//! through the real socket. Live sessions — two chats of OMP and Claude Code that replay the same
//! recorded edit, and two terminal agents that report from one checkout — show what only live
//! sessions can: overlap, a shared checkout, and the coverage of a terminal agent. Nothing here
//! starts a real harness or calls a model; the rules themselves are the unit tests of
//! `checks::detectors`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::checkpoint_turns::{codex, git_env, repository};
use crate::common::{self, TestDaemon, WAIT, create_in, remote_code, sh, wait_event};

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_client::Client;
use hutan_core::agent::{
    AgentEvent, AgentEventKind, Delivery, FileChange, FileChangeKind, MessageRole, ProviderIds,
    ToolKind, ToolStatus,
};
use hutan_core::{ApprovalId, ItemId, SessionId, TurnId, WorkspaceId};
use hutan_daemon::DaemonConfig;
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSend};
use hutan_protocol::checks::{
    AgentsFindings, AgentsFindingsDismiss, AgentsFindingsRequest, AgentsFindingsRestore, Finding,
    FindingConfidence, FindingDismissed, FindingRestored, FindingSeverity, SessionCoverage,
    detector,
};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Origin, Request, SessionInfo,
    SessionResult, WorkspaceCreate, WorkspaceInfo,
};
use hutan_storage::{ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, Storage};
use tokio::time::{sleep, timeout};

const MIN: u64 = 60_000;

fn agents_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures")
}

fn bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().and_then(Path::parent).unwrap().join(name);
    assert!(
        bin.is_file(),
        "{name} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    bin
}

/// `[harness.<id>]` pointing at the fake agent replaying the ACP fixture `name`.
fn harness_toml(id: &str, fixture: &str, received: &Path) -> String {
    let transcript = agents_fixtures()
        .join("acp-omp-18.11")
        .join(format!("{fixture}.jsonl"));
    format!(
        "[harness.{id}]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n\n",
        bin = bin("hutan-acp-fixture"),
    )
}

async fn workspace(client: &Client, name: &str) -> WorkspaceInfo {
    client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: name.into(),
            paths: Vec::new(),
            project_ids: Vec::new(),
            created_by: None,
        }))
        .await
        .unwrap()
}

async fn chat(client: &Client, workspace: WorkspaceId, agent: &str, cwd: &Path) -> SessionInfo {
    client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            extra_dirs: Vec::new(),
            contain: None,
            workspace_id: Some(workspace),
            cwd: cwd.to_owned(),
            agent: agent.to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some(format!("{agent} chat")),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
        }))
        .await
        .unwrap()
        .session
}

async fn findings(client: &Client, req: AgentsFindingsRequest) -> AgentsFindings {
    client.call(Request::AgentsFindings(req)).await.unwrap()
}

async fn findings_where(
    client: &Client,
    req: AgentsFindingsRequest,
    what: &str,
    pred: impl Fn(&AgentsFindings) -> bool,
) -> AgentsFindings {
    timeout(WAIT, async {
        loop {
            let answer = findings(client, req).await;
            if pred(&answer) {
                return answer;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the findings never showed {what}"))
}

fn of_session(session: SessionId) -> AgentsFindingsRequest {
    AgentsFindingsRequest {
        session_id: Some(session),
        ..Default::default()
    }
}

fn detectors_of(answer: &AgentsFindings, session: Option<SessionId>) -> Vec<&str> {
    answer
        .findings
        .iter()
        .filter(|f| f.scope.session_id == session)
        .map(|f| f.detector.as_str())
        .collect()
}

fn find<'a>(answer: &'a AgentsFindings, session: SessionId, id: &str) -> &'a Finding {
    let found: Vec<&Finding> = answer
        .findings
        .iter()
        .filter(|f| f.scope.session_id == Some(session) && f.detector == id)
        .collect();
    assert_eq!(found.len(), 1, "{id} of {session}: {:?}", answer.findings);
    found[0]
}

fn coverage_of(answer: &AgentsFindings, session: SessionId) -> &SessionCoverage {
    answer
        .coverage
        .iter()
        .find(|c| c.session_id == session)
        .unwrap_or_else(|| panic!("no coverage of {session}"))
}

// A journal ----------------------------------------------------------------------------------

/// What an agent did in a turn.
enum Step {
    /// A command and its exit code.
    Command(&'static str, i32),
    /// The agent edits `path` and the file change carries `patch`.
    Edit(&'static str, &'static str),
    /// The agent's message.
    Say(&'static str),
}

/// Writes a chat's journal and its turn table the way the daemon does.
struct Journal<'a> {
    storage: &'a Storage,
    session: SessionId,
    seq: u64,
    ts: u64,
}

impl<'a> Journal<'a> {
    fn new(storage: &'a Storage, session: SessionId) -> Self {
        // Hours ago: nothing is quiet or waiting by the clock of the test.
        let ts = hutan_client::time::now_ms() - 3 * 60 * MIN;
        Self {
            storage,
            session,
            seq: 1_000,
            ts,
        }
    }

    fn event(&mut self, turn: TurnId, item: Option<ItemId>, kind: AgentEventKind) {
        self.seq += 1;
        self.ts += 250;
        let event = AgentEvent {
            session_id: self.session,
            seq: self.seq,
            ts_ms: self.ts,
            delivery: Delivery::Live,
            turn_id: Some(turn),
            item_id: item,
            provider: ProviderIds::default(),
            kind,
            payload: None,
        };
        self.storage
            .chat()
            .append(ChatItemRow {
                session_id: self.session,
                seq: self.seq,
                ts_ms: self.ts,
                turn_id: Some(turn.to_string()),
                item_id: item.map(|i| i.to_string()),
                kind: event.kind.name().to_owned(),
                event_json: serde_json::to_string(&event).unwrap(),
            })
            .unwrap();
    }

    /// One turn: the user's words, the steps, how it ended.
    fn turn(&mut self, status: ChatTurnStatus, error: Option<&str>, steps: &[Step]) -> TurnId {
        let turn = TurnId::new();
        let started = self.ts + 250;
        self.event(turn, None, AgentEventKind::TurnStarted);
        self.event(
            turn,
            None,
            AgentEventKind::MessageEnd {
                role: MessageRole::User,
                phase: None,
                text: "please do the next step".into(),
            },
        );
        for step in steps {
            match step {
                Step::Command(text, code) => {
                    let item = ItemId::new();
                    self.event(
                        turn,
                        Some(item),
                        AgentEventKind::ToolStart {
                            tool: ToolKind::Command,
                            name: "bash".into(),
                            title: (*text).into(),
                            input: None,
                        },
                    );
                    self.event(
                        turn,
                        Some(item),
                        AgentEventKind::ToolEnd {
                            tool: ToolKind::Command,
                            status: if *code == 0 {
                                ToolStatus::Completed
                            } else {
                                ToolStatus::Failed
                            },
                            output: None,
                            truncated: false,
                            exit_code: Some(*code),
                            duration_ms: Some(900),
                        },
                    );
                }
                Step::Edit(path, patch) => self.event(
                    turn,
                    Some(ItemId::new()),
                    AgentEventKind::FileDiff {
                        changes: vec![FileChange {
                            path: (*path).into(),
                            kind: FileChangeKind::Update,
                            move_path: None,
                            patch: Some((*patch).into()),
                            before: None,
                            after: None,
                        }],
                        patch: None,
                    },
                ),
                Step::Say(text) => self.event(
                    turn,
                    None,
                    AgentEventKind::MessageEnd {
                        role: MessageRole::Assistant,
                        phase: None,
                        text: (*text).into(),
                    },
                ),
            }
        }
        let end = match status {
            ChatTurnStatus::Failed => AgentEventKind::TurnFailed {
                message: error.unwrap_or("failed").into(),
                details: None,
            },
            ChatTurnStatus::Cancelled => AgentEventKind::TurnCancelled,
            _ => AgentEventKind::TurnCompleted {
                duration_ms: Some(2_000),
            },
        };
        self.event(turn, None, end);
        self.storage
            .chat()
            .upsert_turn(ChatTurnRow {
                session_id: self.session,
                turn_id: turn.to_string(),
                provider_turn_id: None,
                status,
                started_at_ms: started,
                ended_at_ms: Some(self.ts),
                error: error.map(str::to_owned),
            })
            .unwrap();
        turn
    }
}

/// Stops `t`, lets `seed` write into its database, and starts a daemon on the same home.
async fn seed_while_stopped(t: TestDaemon, seed: impl FnOnce(&Storage)) -> TestDaemon {
    let TestDaemon {
        _dir,
        paths,
        daemon,
    } = t;
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    let storage = Storage::open(&paths.database).unwrap();
    seed(&storage);
    storage.close();
    let daemon = common::start_when_free(DaemonConfig::new(paths.clone())).await;
    TestDaemon {
        _dir,
        paths,
        daemon,
    }
}

const SKIP: &str = "--- a/tests/health.rs\n+++ b/tests/health.rs\n@@ -1,3 +1,4 @@\n+#[ignore = \"flaky\"]\n #[test]\n fn health() {\n }\n";

#[tokio::test(flavor = "multi_thread")]
async fn journals_of_three_harnesses_in_two_workspaces_give_findings_with_their_coverage() {
    let work = tempfile::tempdir().unwrap();
    let toml = format!(
        "{}{}",
        harness_toml("omp", "plain-turn", &work.path().join("omp.jsonl")),
        harness_toml("claude", "plain-turn", &work.path().join("claude.jsonl")),
    );
    let transcript = agents_fixtures().join("codex-0.162/approval-touch.jsonl");
    let config = codex(&transcript, &work.path().join("received.jsonl"));
    let t = TestDaemon::start_with(&toml, move |c| c.codex = Some(config)).await;
    let client = t.connect().await;
    let api = workspace(&client, "api").await;
    let web = workspace(&client, "web").await;
    let cwd_api = work.path().join("api");
    let cwd_web = work.path().join("web");
    std::fs::create_dir_all(&cwd_api).unwrap();
    std::fs::create_dir_all(&cwd_web).unwrap();
    let codex_chat = chat(&client, api.id, "codex", &cwd_api).await;
    let omp_chat = chat(&client, api.id, "omp", &cwd_api).await;
    let claude_chat = chat(&client, web.id, "claude", &cwd_web).await;
    for session in [&codex_chat, &omp_chat, &claude_chat] {
        common::stop(&client, session.id).await.unwrap();
    }
    drop(client);

    let mut turns: Vec<TurnId> = Vec::new();
    let t = seed_while_stopped(t, |storage| {
        // Codex, in `api`: a turn that ends "done" after its last test command failed; a turn
        // that skips a test and changes code with no test run; the same failure three times; a
        // question that waited 42 minutes.
        let mut j = Journal::new(storage, codex_chat.id);
        turns.push(j.turn(
            ChatTurnStatus::Completed,
            None,
            &[
                Step::Command("cargo nextest run -p api", 101),
                Step::Say("Done, all tests pass."),
            ],
        ));
        turns.push(j.turn(
            ChatTurnStatus::Completed,
            None,
            &[
                Step::Edit(
                    "src/health.rs",
                    "--- a/src/health.rs\n+++ b/src/health.rs\n@@ -1 +1,2 @@\n+fn health() {}\n",
                ),
                Step::Edit("tests/health.rs", SKIP),
                Step::Command("cargo build", 0),
                Step::Say("Updated the endpoint."),
            ],
        ));
        for n in 1..=3 {
            j.turn(
                ChatTurnStatus::Failed,
                Some(&format!("rate limited, retry in {n}0 s")),
                &[],
            );
        }
        let asked = j.ts;
        storage
            .approvals()
            .upsert(ApprovalRow {
                id: ApprovalId::new(),
                session_id: codex_chat.id,
                turn_id: None,
                kind: "command".into(),
                title: "git push origin main".into(),
                options_json: "[]".into(),
                requested_at_ms: asked,
                resolved_at_ms: Some(asked + 42 * MIN),
                outcome: Some(hutan_storage::ApprovalOutcome::Selected),
                option_id: Some("accept".into()),
                decided_by: Some("desktop".into()),
            })
            .unwrap();

        // OMP, in `api`: says the build passes and ran nothing.
        let mut j = Journal::new(storage, omp_chat.id);
        turns.push(j.turn(
            ChatTurnStatus::Completed,
            None,
            &[
                Step::Command("ls -la", 0),
                Step::Say("Cleaned up. The build passes."),
            ],
        ));
        // …and the user ran its tests through Hutan afterwards: they failed.
        let started = hutan_client::time::now_ms() - 5 * MIN;
        let run = storage
            .verify_runs()
            .start(hutan_storage::VerifyRunRow {
                id: 0,
                session_id: omp_chat.id,
                command: "cargo test".into(),
                cwd: "/work/api".into(),
                source: "detected".into(),
                why: "Cargo.toml in /work/api".into(),
                status: "running".into(),
                exit_code: None,
                signal: None,
                started_ms: started,
                ended_ms: None,
                duration_ms: None,
                output_tail: String::new(),
                output_truncated: false,
                requested_by: "user".into(),
                forced: false,
                error: None,
            })
            .unwrap();
        storage
            .verify_runs()
            .finish(
                run,
                hutan_storage::VerifyRunEnd {
                    status: "failed".into(),
                    exit_code: Some(101),
                    signal: None,
                    ended_ms: started + 20_000,
                    duration_ms: 20_000,
                    output_tail: "running 41 tests\ntest result: FAILED. 40 passed; 1 failed\n"
                        .into(),
                    output_truncated: false,
                    error: None,
                },
            )
            .unwrap();

        // Claude Code, in `web`: a clean turn — the test ran and passed, and it says so.
        let mut j = Journal::new(storage, claude_chat.id);
        turns.push(j.turn(
            ChatTurnStatus::Completed,
            None,
            &[
                Step::Edit(
                    "src/app.ts",
                    "--- a/src/app.ts\n+++ b/src/app.ts\n@@ -1 +1,2 @@\n+let a = 1;\n",
                ),
                Step::Command("npm test", 0),
                Step::Say("All tests pass."),
            ],
        ));
    })
    .await;
    let client = t.connect().await;

    // Everything: the alerts first, each finding with its confidence and its evidence.
    let all = findings(&client, AgentsFindingsRequest::default()).await;
    assert_eq!(
        all.findings[0].severity,
        FindingSeverity::Alert,
        "alerts first"
    );
    let done = find(&all, codex_chat.id, detector::DONE_AFTER_FAILED_COMMAND);
    assert_eq!(
        (done.severity, done.confidence),
        (FindingSeverity::Alert, FindingConfidence::Exact)
    );
    assert_eq!(done.scope.turn_id, Some(turns[0]));
    assert_eq!(done.scope.workspace_id, api.id);
    assert!(
        done.evidence[0]
            .excerpt
            .contains("exit 101: cargo nextest run -p api"),
        "{:?}",
        done.evidence
    );
    let weakened = find(&all, codex_chat.id, detector::TESTS_WEAKENED);
    assert_eq!(
        (weakened.severity, weakened.confidence),
        (FindingSeverity::Alert, FindingConfidence::Heuristic)
    );
    assert!(
        weakened.evidence[0].excerpt.contains("#[ignore"),
        "{:?}",
        weakened.evidence
    );
    assert_eq!(
        find(&all, codex_chat.id, detector::TESTS_EDITED).severity,
        FindingSeverity::Info
    );
    let repeated = find(&all, codex_chat.id, detector::REPEATED_FAILURE);
    assert_eq!(
        (repeated.count, repeated.confidence),
        (3, FindingConfidence::Heuristic)
    );
    let waited = find(&all, codex_chat.id, detector::LONG_WAIT);
    assert_eq!(waited.confidence, FindingConfidence::Exact);
    assert!(waited.title.contains("42 min"), "{}", waited.title);
    let claim = find(&all, omp_chat.id, detector::CLAIMED_PASS_WITHOUT_TEST);
    let verify = find(&all, omp_chat.id, detector::VERIFY_FAILED);
    assert_eq!(
        (verify.severity, verify.confidence),
        (FindingSeverity::Alert, FindingConfidence::Exact)
    );
    assert!(
        verify.evidence[0].excerpt.contains("exit 101: cargo test"),
        "{:?}",
        verify.evidence
    );
    assert!(verify.evidence[1].excerpt.contains("1 failed"));
    assert_eq!(
        (claim.severity, claim.confidence),
        (FindingSeverity::Warn, FindingConfidence::Heuristic)
    );
    // The chat that ran its tests and passed has nothing against it.
    assert!(
        all.findings
            .iter()
            .all(|f| f.scope.session_id != Some(claude_chat.id)),
        "{:?}",
        detectors_of(&all, Some(claude_chat.id))
    );
    assert!(all.counts.alert >= 2 && all.counts.warn >= 2 && all.counts.info >= 1);

    // Coverage: every chat, with what could not run and why (no checkpoints in these folders).
    assert_eq!(all.coverage.len(), 3);
    for session in [&codex_chat, &omp_chat, &claude_chat] {
        let c = coverage_of(&all, session.id);
        assert!(c.chat);
        assert!(
            c.ran
                .iter()
                .any(|d| d == detector::DONE_AFTER_FAILED_COMMAND)
        );
    }
    let gap = coverage_of(&all, codex_chat.id)
        .gaps
        .iter()
        .find(|g| g.detectors.iter().any(|d| d == detector::TESTS_EDITED))
        .expect("the turns without files are a gap");
    assert!(gap.why.contains("no checkpoint"), "{}", gap.why);
    assert_eq!(
        coverage_of(&all, codex_chat.id).harness.as_deref(),
        Some("codex")
    );
    assert_eq!(
        coverage_of(&all, omp_chat.id).harness.as_deref(),
        Some("omp")
    );
    assert_eq!(
        coverage_of(&all, claude_chat.id).harness.as_deref(),
        Some("claude")
    );
    assert!(serde_json::to_vec(&all).unwrap().len() <= 24 * 1024);

    // Stable ids: the same problems keep their ids on every read.
    let again = findings(&client, AgentsFindingsRequest::default()).await;
    let ids =
        |a: &AgentsFindings| -> Vec<String> { a.findings.iter().map(|f| f.id.clone()).collect() };
    assert_eq!(ids(&all), ids(&again));

    // Scope: a workspace, a session, both; a bad one.
    let web_only = findings(
        &client,
        AgentsFindingsRequest {
            workspace_id: Some(web.id),
            ..Default::default()
        },
    )
    .await;
    assert!(web_only.findings.is_empty());
    assert_eq!(web_only.coverage.len(), 1);
    assert_eq!(web_only.coverage[0].session_id, claude_chat.id);
    let omp_only = findings(&client, of_session(omp_chat.id)).await;
    assert_eq!(
        detectors_of(&omp_only, Some(omp_chat.id)),
        [detector::VERIFY_FAILED, detector::CLAIMED_PASS_WITHOUT_TEST]
    );
    assert_eq!(omp_only.coverage.len(), 1);
    let both = AgentsFindingsRequest {
        workspace_id: Some(web.id),
        session_id: Some(omp_chat.id),
        ..Default::default()
    };
    assert_eq!(
        remote_code(
            client
                .call::<AgentsFindings>(Request::AgentsFindings(both))
                .await
        ),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(
            client
                .call::<AgentsFindings>(Request::AgentsFindings(of_session(SessionId::new())))
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            client
                .call::<AgentsFindings>(Request::AgentsFindings(AgentsFindingsRequest {
                    workspace_id: Some(WorkspaceId::new()),
                    ..Default::default()
                }))
                .await
        ),
        ErrorCode::NotFound
    );
    // `since` leaves out what was last seen before it; a limit cuts the least important.
    let future = findings(
        &client,
        AgentsFindingsRequest {
            since_ms: Some(hutan_client::time::now_ms() + 60_000),
            ..Default::default()
        },
    )
    .await;
    assert!(future.findings.is_empty());
    let two = findings(
        &client,
        AgentsFindingsRequest {
            limit: Some(2),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(two.findings.len(), 2);
    assert_eq!(
        usize::try_from(two.omitted).unwrap(),
        all.findings.len() - 2
    );
    assert!(
        two.findings
            .iter()
            .all(|f| f.severity == FindingSeverity::Alert)
    );

    // Dismissing is the user's: an agent is refused, a malformed id is a bad request, the user's
    // dismissal hides the finding, is remembered, and survives a restart of the daemon.
    let agent_shell = common::create(&client, sh("exec sleep 600")).await;
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: agent_shell.id,
            source: "test-hooks".into(),
            agent: "fake".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
            insight: None,
        }))
        .await
        .unwrap();
    let mut options = t.options();
    options.client.origin = Some(Origin {
        session_id: agent_shell.id,
    });
    let agent = Client::connect(options).await.unwrap();
    let dismiss =
        |id: &str| Request::AgentsFindingsDismiss(AgentsFindingsDismiss { id: id.to_owned() });
    assert_eq!(
        remote_code(agent.call::<FindingDismissed>(dismiss(&done.id)).await),
        ErrorCode::Forbidden,
        "Mawas and every other agent: a problem is not hidden by the one who made it"
    );
    // The agent may still read.
    assert!(
        !findings(&agent, AgentsFindingsRequest::default())
            .await
            .findings
            .is_empty()
    );
    assert_eq!(
        remote_code(client.call::<FindingDismissed>(dismiss("not-an-id")).await),
        ErrorCode::BadRequest
    );
    let dismissed: FindingDismissed = client.call(dismiss(&done.id)).await.unwrap();
    assert_eq!(dismissed.id, done.id);
    let first_time = dismissed.dismissed_at_ms;
    let hidden = findings(&client, AgentsFindingsRequest::default()).await;
    assert!(hidden.findings.iter().all(|f| f.id != done.id));
    assert_eq!(hidden.dismissed_hidden, 1);
    assert_eq!(hidden.counts.alert, all.counts.alert - 1);
    let shown = findings(
        &client,
        AgentsFindingsRequest {
            include_dismissed: true,
            ..Default::default()
        },
    )
    .await;
    let marked = shown.findings.iter().find(|f| f.id == done.id).unwrap();
    assert!(marked.dismissed);
    assert_eq!(shown.dismissed_hidden, 0);
    // Dismissed again: the first time stays.
    let twice: FindingDismissed = client.call(dismiss(&done.id)).await.unwrap();
    assert_eq!(twice.dismissed_at_ms, first_time);
    // Restoring is the user's too: an agent cannot bring back what the user hid, nor hide
    // by restoring; and the dismissal is still in force after the refusal.
    let restore =
        |id: &str| Request::AgentsFindingsRestore(AgentsFindingsRestore { id: id.to_owned() });
    assert_eq!(
        remote_code(agent.call::<FindingRestored>(restore(&done.id)).await),
        ErrorCode::Forbidden
    );
    assert_eq!(
        remote_code(client.call::<FindingRestored>(restore("not-an-id")).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        findings(&client, AgentsFindingsRequest::default())
            .await
            .dismissed_hidden,
        1
    );

    drop((client, agent));
    let t = t.restart().await;
    let client = t.connect().await;
    let after = findings(&client, AgentsFindingsRequest::default()).await;
    assert!(
        after.findings.iter().all(|f| f.id != done.id),
        "dismissed survives a restart"
    );
    assert_eq!(after.dismissed_hidden, 1);
    // …and the other ids are the ones of before the restart.
    let rest = |a: &AgentsFindings| -> Vec<String> {
        a.findings
            .iter()
            .map(|f| f.id.clone())
            .filter(|id| *id != done.id)
            .collect()
    };
    assert_eq!(rest(&all), ids(&after));
    // The user's restore lists it again — also after a restart; restoring twice says so.
    let restored: FindingRestored = client.call(restore(&done.id)).await.unwrap();
    assert_eq!((restored.id.as_str(), restored.restored), (done.id.as_str(), true));
    let back = findings(&client, AgentsFindingsRequest::default()).await;
    assert!(back.findings.iter().any(|f| f.id == done.id && !f.dismissed));
    assert_eq!(back.dismissed_hidden, 0);
    assert_eq!(back.counts.alert, all.counts.alert);
    let again: FindingRestored = client.call(restore(&done.id)).await.unwrap();
    assert!(!again.restored);
    t.stop().await;
}

// Live sessions ------------------------------------------------------------------------------

/// A terminal agent in `cwd`, working, as a hook reports it.
async fn working_terminal(
    client: &Client,
    workspace: WorkspaceId,
    cwd: &Path,
    source: &str,
) -> SessionInfo {
    let mut spec = sh("exec sleep 600");
    spec.cwd = Some(cwd.to_owned());
    let session = create_in(client, Some(workspace), spec).await;
    client
        .call::<AgentApplied>(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session.id,
            source: source.into(),
            agent: "claude".into(),
            state: Some(AgentState::Working),
            message: None,
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
            insight: None,
        }))
        .await
        .unwrap();
    session
}

#[tokio::test(flavor = "multi_thread")]
async fn live_sessions_that_change_one_file_or_share_a_checkout_are_found_and_a_terminal_agent_says_what_is_not_seen()
 {
    let work = tempfile::tempdir().unwrap();
    let repo = repository(work.path());
    let toml = format!(
        "{}{}",
        harness_toml("omp", "tool-diff", &work.path().join("omp.jsonl")),
        harness_toml("claude", "tool-diff", &work.path().join("claude.jsonl")),
    );
    let t = TestDaemon::start_with(&toml, |c| c.checkpoints.git_env = git_env()).await;
    let (client, mut events) = t.connect_subscribed().await;
    let api = workspace(&client, "api").await;
    let web = workspace(&client, "web").await;
    let omp_chat = chat(&client, api.id, "omp", &repo).await;
    let claude_chat = chat(&client, web.id, "claude", &repo).await;
    for session in [&omp_chat, &claude_chat] {
        client
            .call::<hutan_protocol::chat::ChatSent>(Request::ChatSend(ChatSend {
                session_id: session.id,
                text: "edit src/lib.rs".into(),
                attachments: vec![],
            }))
            .await
            .unwrap();
    }
    // Both replay the same recorded edit of one file; each turn ends.
    for session in [&omp_chat, &claude_chat] {
        let id = session.id;
        wait_event(&mut events, "the turn's end", |event| match event {
            Event::ChatEvent { event, .. } if event.session_id == id && event.kind.ends_turn() => {
                Some(())
            }
            _ => None,
        })
        .await;
    }

    // Two agents at work in one checkout, beside the chats (which wait between turns).
    let first = working_terminal(&client, api.id, &repo, "claude-hooks").await;
    let second = working_terminal(&client, web.id, &repo, "omp-hooks").await;

    let answer = findings_where(
        &client,
        AgentsFindingsRequest::default(),
        "the overlap and the shared checkout",
        |a| {
            a.findings.iter().any(|f| f.detector == detector::OVERLAP)
                && a.findings
                    .iter()
                    .any(|f| f.detector == detector::SHARED_CHECKOUT)
        },
    )
    .await;

    let overlap: Vec<&Finding> = answer
        .findings
        .iter()
        .filter(|f| f.detector == detector::OVERLAP)
        .collect();
    assert_eq!(overlap.len(), 1, "{overlap:?}");
    let overlap = overlap[0];
    assert_eq!(overlap.confidence, FindingConfidence::Exact);
    // One checkout: the two chats collide in it, not later in a merge.
    assert_eq!(overlap.severity, FindingSeverity::Alert);
    assert_eq!(overlap.count, 2);
    assert!(overlap.scope.session_id.is_none());
    assert!(
        overlap
            .evidence
            .iter()
            .all(|e| e.excerpt.contains("src/lib.rs")),
        "{:?}",
        overlap.evidence
    );
    let named: Vec<&str> = overlap
        .evidence
        .iter()
        .filter_map(|e| e.reference.as_deref())
        .collect();
    assert!(named.contains(&omp_chat.id.to_string().as_str()));
    assert!(named.contains(&claude_chat.id.to_string().as_str()));

    let shared: Vec<&Finding> = answer
        .findings
        .iter()
        .filter(|f| f.detector == detector::SHARED_CHECKOUT)
        .collect();
    assert_eq!(shared.len(), 1, "{shared:?}");
    assert_eq!(shared[0].severity, FindingSeverity::Warn);
    assert_eq!(shared[0].count, 2);
    let members: Vec<&str> = shared[0]
        .evidence
        .iter()
        .filter_map(|e| e.reference.as_deref())
        .collect();
    assert!(members.contains(&first.id.to_string().as_str()));
    assert!(members.contains(&second.id.to_string().as_str()));

    // The scope narrows what is listed but a finding about a session of it stays whole.
    let api_only = findings(
        &client,
        AgentsFindingsRequest {
            workspace_id: Some(api.id),
            ..Default::default()
        },
    )
    .await;
    assert!(
        api_only
            .findings
            .iter()
            .any(|f| f.detector == detector::OVERLAP)
    );
    assert!(
        api_only
            .coverage
            .iter()
            .all(|c| c.session_id != claude_chat.id)
    );
    let by_session = findings(&client, of_session(first.id)).await;
    assert!(
        by_session
            .findings
            .iter()
            .any(|f| f.detector == detector::SHARED_CHECKOUT)
    );
    assert!(
        by_session
            .findings
            .iter()
            .all(|f| f.detector != detector::OVERLAP)
    );

    // A terminal agent has state and nothing else: coverage says which detectors cannot see.
    let c = coverage_of(&answer, first.id);
    assert!(!c.chat);
    assert_eq!(c.harness.as_deref(), Some("claude"));
    for id in [
        detector::DONE_AFTER_FAILED_COMMAND,
        detector::TESTS_WEAKENED,
        detector::REPEATED_FAILURE,
        detector::THRASH,
        detector::COSTLY_TURN,
        detector::OVERLAP,
    ] {
        assert!(
            !c.ran.iter().any(|r| r == id),
            "{id} cannot run on a terminal agent"
        );
    }
    for id in [
        detector::STALLED,
        detector::LONG_WAIT,
        detector::SHARED_CHECKOUT,
    ] {
        assert!(
            c.ran.iter().any(|r| r == id),
            "{id} runs on what it reports"
        );
    }
    let why = &c
        .gaps
        .iter()
        .find(|g| g.detectors.iter().any(|d| d == detector::TESTS_WEAKENED))
        .unwrap()
        .why;
    assert!(why.contains("terminal agent: no tool events"), "{why}");
    // A chat that edited files runs the overlap detector.
    assert!(
        coverage_of(&answer, omp_chat.id)
            .ran
            .iter()
            .any(|r| r == detector::OVERLAP)
    );
    t.stop().await;
}
