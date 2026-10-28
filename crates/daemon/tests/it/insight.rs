//! Agent insight over the wire (protocol 0.59, capability `agent.insight`): what a chat's
//! context holds, what each turn used, tools and files, `agent.measure_context`, and who is told
//! when it changes. The ACP harnesses are the fake agent replaying `insight-omp.jsonl` (the
//! recorded OMP 18.12.3 exchange of `docs/spikes/2026-10-28-insight/`, plus its `/context`) and
//! `insight-claude.jsonl` (the Claude Code adapter's shapes); Codex replays the recording
//! `file-change.jsonl` of codex 0.162.1.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{self, TestDaemon, WAIT, remote_code, wait_event};

use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::insight::{
    AgentInsight, BreakdownKind, CostKind, Exactness, InsightField, InsightOutcome,
};
use hutan_core::{AgentEventKind, Seq, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{Event, Request, SessionInfo, SessionResult};
use tokio::sync::broadcast;
use tokio::time::timeout;

type Events = broadcast::Receiver<(Seq, Event)>;

fn acp_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/acp-omp-18.11")
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

/// `[harness.<id>]` pointing at the fake agent replaying `transcript`.
fn harness_toml(id: &str, transcript: &Path, received: &Path) -> String {
    format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.{id}]\nbin = {bin:?}\nargs = [\"replay\", \"--received\", {received:?}, {transcript:?}, \"acp\"]\n",
        bin = bin("hutan-acp-fixture"),
    )
}

struct T {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    client: Client,
    events: Events,
}

impl T {
    /// A daemon whose harness `agent` replays the ACP fixture `name`.
    async fn harness(agent: &str, name: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let transcript = acp_fixtures().join(format!("{name}.jsonl"));
        let toml = harness_toml(agent, &transcript, &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with(&toml, |_| {}).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    /// A daemon whose Codex replays the recording `name`.
    async fn codex(name: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../agents/tests/fixtures/codex-0.162")
            .join(format!("{name}.jsonl"));
        let config = CodexConfig {
            program: bin("hutan-codex-fixture"),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                work.path().join("received.jsonl").display().to_string(),
                transcript.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        };
        let daemon = TestDaemon::start_with("", move |c| c.codex = Some(config)).await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            client,
            events,
        }
    }

    async fn create(&self, agent: &str) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
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
                account: None,
                safety: None,
            }))
            .await
            .unwrap()
            .session
    }

    /// Sends `text` and waits for the turn to end.
    async fn turn(&mut self, id: SessionId, text: &str) {
        let sent = self.client.chat_send(id, text).await.unwrap();
        self.end_of_turn(sent.turn_id).await;
    }

    /// Waits for the end of `turn` (earlier turns' ends may be waiting in the queue).
    async fn end_of_turn(&mut self, turn: hutan_core::TurnId) {
        wait_event(&mut self.events, "the turn to end", |event| match event {
            Event::ChatEvent { event, .. }
                if event.kind.ends_turn() && event.turn_id == Some(turn) =>
            {
                assert!(
                    matches!(event.kind, AgentEventKind::TurnCompleted { .. }),
                    "{:?}",
                    event.kind
                );
                Some(())
            }
            _ => None,
        })
        .await;
    }

    async fn insight(&self, id: SessionId) -> AgentInsight {
        self.client.agent_insight(id, None).await.unwrap().insight
    }
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-9)
}

/// The recorded OMP 18.12.3 exchange: two prompts (the first with a `read` call), then `/context`.
/// `usage_update` says 11 200 of 200 000 after both, with the session's cost 0.02958 then
/// 0.03636; the prompts' answers carry the turns' own counts.
#[tokio::test(flavor = "multi_thread")]
async fn an_omp_chat_reports_its_context_tokens_cost_tools_and_measures_its_breakdown() {
    let mut t = T::harness("omp", "insight-omp").await;
    let chat = t.create("omp").await;
    assert!(t.client.supports_agent_insight());
    t.turn(chat.id, "Read hello.txt and tell me what it says.")
        .await;
    t.turn(chat.id, "Say it again briefly.").await;

    let insight = t.insight(chat.id).await;
    assert_eq!(insight.session_id, chat.id);
    assert_eq!(insight.harness.as_deref(), Some("omp"));
    assert!(insight.node_id.is_some(), "the main agent's node");
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window, context.exactness),
        (11_200, Some(200_000), Exactness::Reported)
    );
    assert!(context.breakdown.is_empty(), "nothing measured yet");
    let totals = insight.totals.as_ref().expect("totals");
    assert_eq!(
        (
            totals.turns,
            totals.input,
            totals.output,
            totals.cache_read,
            totals.total
        ),
        (2, Some(9_400), Some(64), Some(24_000), Some(33_464))
    );
    assert_eq!((totals.cache_write, totals.reasoning), (None, None));
    assert!(close(totals.cost_usd, 0.036_36), "{:?}", totals.cost_usd);
    assert_eq!(totals.cost_kind, Some(CostKind::Estimate));
    let ratio = totals.cache_hit_ratio.expect("a cache ratio");
    assert!((ratio - 24_000.0 / 33_400.0).abs() < 1e-9, "{ratio}");
    assert_eq!(insight.turns.len(), 2);
    let (first, second) = (&insight.turns[0], &insight.turns[1]);
    assert_eq!(
        (
            first.n,
            first.input,
            first.output,
            first.cache_read,
            first.tool_calls
        ),
        (1, Some(8_200), Some(52), Some(14_000), 1)
    );
    assert!(close(first.cost_usd, 0.029_58));
    assert_eq!(
        (second.n, second.input, second.tool_calls),
        (2, Some(1_200), 0)
    );
    assert!(close(second.cost_usd, 0.006_78));
    assert_eq!(first.outcome, InsightOutcome::Completed);
    assert!(first.turn_id.is_some() && first.duration_ms.is_some());
    let tools = insight.tools.as_ref().expect("tools");
    assert_eq!((tools.calls, tools.failed), (1, 0));
    assert!(insight.can_measure);
    assert_eq!(insight.unsupported, [InsightField::Reasoning]);
    assert!(insight.sources.iter().all(|s| s.as_of_ms > 0));

    // Measure: `/context` goes to the harness as a turn that costs nothing; its answer is read.
    let measured = t.client.agent_measure_context(chat.id).await.unwrap();
    assert!(measured.measured);
    let context = measured.insight.context.as_ref().expect("a context");
    assert_eq!(context.used, 11_200, "the harness's own figure stays");
    let rows: Vec<_> = context
        .breakdown
        .iter()
        .map(|row| (row.id.as_str(), row.tokens, row.kind))
        .collect();
    assert_eq!(
        rows,
        [
            ("system_prompt", 2_559, BreakdownKind::Used),
            ("tools", 3_976, BreakdownKind::Used),
            ("project_context", 113, BreakdownKind::Used),
            ("messages", 4_552, BreakdownKind::Used),
            ("autocompact_buffer", 30_000, BreakdownKind::Buffer),
            ("free", 158_800, BreakdownKind::Free),
        ]
    );
    assert_eq!(context.breakdown_exactness, Some(Exactness::Reported));
    assert_eq!(context.compact_at, Some(170_000));
    assert!(context.breakdown_as_of_ms.is_some());
    assert_eq!(
        measured.insight.totals.as_ref().map(|t| t.turns),
        Some(2),
        "/context is not a turn of the conversation"
    );
    // The same answer is in the journal as an exchange the user can see.
    let history = t.client.chat_history(chat.id, None, 100).await.unwrap();
    assert!(history.events.iter().any(|e| matches!(
        &e.kind,
        AgentEventKind::MessageEnd { text, .. } if text == "/context"
    )));
    t.daemon.stop().await;
}

/// The Claude Code adapter's shapes: a cache write in the prompt's usage, an edit and a failed
/// command, the `/context` markdown, and a prompt whose counts come only in `_meta.quota`.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_chat_reports_cache_writes_files_failed_tools_and_estimated_categories() {
    let mut t = T::harness("claude", "insight-claude").await;
    let chat = t.create("claude").await;
    t.turn(chat.id, "Fix it.").await;

    let insight = t.insight(chat.id).await;
    let totals = insight.totals.as_ref().expect("totals");
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write,
            totals.total
        ),
        (
            Some(1_500),
            Some(420),
            Some(30_000),
            Some(9_000),
            Some(40_920)
        )
    );
    assert!(close(totals.cost_usd, 0.31));
    let ratio = totals.cache_hit_ratio.expect("a ratio");
    assert!(
        (ratio - 30_000.0 / 40_500.0).abs() < 1e-9,
        "the cache writes are on top: {ratio}"
    );
    let context = insight.context.as_ref().expect("a context");
    assert_eq!((context.used, context.window), (42_000, Some(200_000)));
    let tools = insight.tools.as_ref().expect("tools");
    assert_eq!((tools.calls, tools.failed), (2, 1));
    let failed: Vec<_> = tools.by_name.iter().filter(|t| t.failed == 1).collect();
    assert_eq!(failed.len(), 1);
    let files = insight.files.as_ref().expect("files");
    assert_eq!((files.count, files.added, files.removed), (1, 2, 1));
    assert_eq!(files.items[0].path, "/tmp/hutan-fixture/src/lib.rs");
    assert_eq!(insight.turns[0].files, 1);
    assert_eq!(insight.turns[0].tool_failed, 1);

    let measured = t.client.agent_measure_context(chat.id).await.unwrap();
    assert!(measured.measured);
    let context = measured.insight.context.as_ref().expect("a context");
    assert_eq!(context.breakdown_exactness, Some(Exactness::Estimated));
    let ids: Vec<_> = context.breakdown.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "system_prompt",
            "tools",
            "tools_deferred",
            "skills",
            "messages",
            "free",
            "autocompact_buffer"
        ]
    );
    assert_eq!(context.compact_at, Some(167_000));
    assert_eq!(context.used, 42_000);

    // A prompt whose counts come only in the adapter's `_meta.quota`.
    t.turn(chat.id, "Anything else?").await;
    let totals = t.insight(chat.id).await.totals.expect("totals");
    assert_eq!(
        (
            totals.turns,
            totals.input,
            totals.cache_read,
            totals.cache_write,
            totals.total
        ),
        (2, Some(1_550), Some(72_000), Some(9_120), Some(83_095))
    );
    t.daemon.stop().await;
}

/// Codex 0.162.1 (`file-change.jsonl`): the second request of the turn reports the cumulative
/// `total 43 787` and its own `last 21 908` on a 475 000 window; Codex reports no cost and has
/// no breakdown, and the insight says so instead of drawing zeros.
#[tokio::test(flavor = "multi_thread")]
async fn a_codex_chat_has_the_latest_request_as_its_context_and_says_what_it_does_not_report() {
    let mut t = T::codex("file-change").await;
    let chat = t.create("codex").await;
    let sent = t
        .client
        .chat_send(
            chat.id,
            "Create a file named hello.txt containing the single line hi, using the apply_patch tool, then reply with the word done.",
        )
        .await
        .unwrap();
    let approval = wait_event(&mut t.events, "approval.required", |event| match event {
        Event::ApprovalRequired { approval } => Some((**approval).clone()),
        _ => None,
    })
    .await;
    t.client
        .call::<hutan_protocol::messages::Empty>(Request::ApprovalRespond(
            hutan_protocol::chat::ApprovalRespond {
                approval_id: approval.id,
                option_id: "accept".into(),
            },
        ))
        .await
        .unwrap();
    t.end_of_turn(sent.turn_id).await;

    let insight = t.insight(chat.id).await;
    assert_eq!(insight.harness.as_deref(), Some("codex"));
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window),
        (21_908, Some(475_000)),
        "not the cumulative 43 787"
    );
    let totals = insight.totals.as_ref().expect("totals");
    assert_eq!(
        (
            totals.turns,
            totals.input,
            totals.output,
            totals.cache_read,
            totals.reasoning,
            totals.total
        ),
        (
            1,
            Some(43_732),
            Some(55),
            Some(33_920),
            Some(0),
            Some(43_787)
        )
    );
    assert_eq!(totals.cache_write, Some(0), "Codex says it wrote none");
    assert_eq!(totals.cost_usd, None, "Codex reports no cost");
    assert_eq!(totals.cost_kind, None);
    assert_eq!(
        insight.unsupported,
        [InsightField::Breakdown, InsightField::Cost]
    );
    assert!(!insight.can_measure);
    let files = insight.files.as_ref().expect("files");
    assert_eq!(files.items[0].path, "/tmp/hutan-fixture/hello.txt");
    assert_eq!((files.count, files.added, files.removed), (1, 1, 0));
    let turn = &insight.turns[0];
    assert_eq!(
        (turn.context_after, turn.cost_usd, turn.files),
        (Some(21_908), None, 1)
    );

    let error = t.client.agent_measure_context(chat.id).await;
    assert_eq!(remote_code(error), ErrorCode::Unsupported);
    t.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn insight_errors_name_what_is_wrong() {
    let mut t = T::harness("omp", "insight-omp").await;
    let chat = t.create("omp").await;
    // Unknown session, a node that is not the session's, a session that is not a chat.
    let unknown = t.client.agent_insight(SessionId::new(), None).await;
    assert_eq!(remote_code(unknown), ErrorCode::NotFound);
    let node = t
        .client
        .agent_insight(chat.id, Some("no-such-node".into()))
        .await;
    assert_eq!(remote_code(node), ErrorCode::NotFound);
    let terminal = common::create(&t.client, common::sh("sleep 30")).await;
    let not_chat = t.client.agent_insight(terminal.id, None).await;
    assert_eq!(remote_code(not_chat), ErrorCode::BadRequest);

    // A chat nothing has happened in has an insight with no blocks, and says where nothing
    // came from.
    let empty = t.insight(chat.id).await;
    assert!(empty.context.is_none() && empty.totals.is_none() && empty.tools.is_none());
    assert!(empty.turns.is_empty() && empty.sources.is_empty() && empty.files.is_none());
    assert!(
        empty.can_measure,
        "a harness that can be measured says so before it was"
    );

    // The measurement is a turn: not while one runs.
    let sent = t.client.chat_send(chat.id, "go").await.unwrap();
    let busy = t.client.agent_measure_context(chat.id).await;
    assert!(matches!(
        remote_code(busy),
        ErrorCode::InvalidState | ErrorCode::ProviderError
    ));
    t.end_of_turn(sent.turn_id).await;
    t.daemon.stop().await;
}

/// The event goes to the connections that focused the session, and to nobody else, and a burst
/// of changes is coalesced to at most one a second.
#[tokio::test(flavor = "multi_thread")]
async fn updates_go_only_to_focused_connections_and_at_most_once_a_second() {
    let mut t = T::harness("omp", "insight-omp").await;
    let chat = t.create("omp").await;
    let (other, mut other_events) = t.daemon.connect_subscribed().await;
    // The focused one is a second connection, so the first one proves who is left out.
    let (focused, mut focused_events) = t.daemon.connect_subscribed().await;
    focused.client_focus(Some(chat.id)).await.unwrap();

    t.client
        .chat_send(chat.id, "Read hello.txt and tell me what it says.")
        .await
        .unwrap();
    // Updates arrive until one has the turn's end in it.
    let mut updates: Vec<AgentInsight> = Vec::new();
    timeout(WAIT, async {
        loop {
            let (_, event) = focused_events.recv().await.unwrap();
            if let Event::AgentInsightUpdated {
                session_id,
                insight,
            } = event
            {
                assert_eq!(session_id, chat.id);
                let done = insight.totals.as_ref().is_some_and(|t| t.turns == 1);
                updates.push(*insight);
                if done {
                    return;
                }
            }
        }
    })
    .await
    .expect("an update with the turn's end");
    assert!(
        updates.windows(2).all(|w| w[1].seq > w[0].seq),
        "seq increases"
    );
    assert!(
        updates
            .windows(2)
            .all(|w| w[1].as_of_ms - w[0].as_of_ms >= 950),
        "one a second at most: {:?}",
        updates.iter().map(|u| u.as_of_ms).collect::<Vec<_>>()
    );
    assert!(
        updates.len() <= 3,
        "a turn is many changes, not as many updates: {}",
        updates.len()
    );

    // The connection that never focused got none: a request answered after the event is
    // proof, because both queues are ordered.
    other.agent_insight(chat.id, None).await.unwrap();
    while let Ok((_, event)) = other_events.try_recv() {
        assert!(
            !matches!(event, Event::AgentInsightUpdated { .. }),
            "an unfocused connection was sent {event:?}"
        );
    }
    // Neither did the connection that looked elsewhere, nor one that stopped looking.
    focused.client_focus(None).await.unwrap();
    // What was sent while it looked is already queued: drop it, then watch a whole turn.
    while focused_events.try_recv().is_ok() {}
    let sent = t
        .client
        .chat_send(chat.id, "Say it again briefly.")
        .await
        .unwrap();
    t.end_of_turn(sent.turn_id).await;
    focused.agent_insight(chat.id, None).await.unwrap();
    while let Ok((_, event)) = focused_events.try_recv() {
        assert!(
            !matches!(event, Event::AgentInsightUpdated { .. }),
            "a connection that stopped looking was sent {event:?}"
        );
    }
    t.daemon.stop().await;
}

/// A session that is not running, and a daemon that restarted: the insight is built again from
/// the journal and the usage records and says the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_chat_after_a_restart_has_the_same_insight_from_the_journal() {
    let mut t = T::harness("omp", "insight-omp").await;
    let chat = t.create("omp").await;
    t.turn(chat.id, "Read hello.txt and tell me what it says.")
        .await;
    t.turn(chat.id, "Say it again briefly.").await;
    let before = t.insight(chat.id).await;
    common::stop(&t.client, chat.id).await.unwrap();

    let T { daemon, work, .. } = t;
    let daemon = daemon.restart().await;
    let (client, _events) = daemon.connect_subscribed().await;
    let after = client.agent_insight(chat.id, None).await.unwrap().insight;

    assert_eq!(after.context, before.context);
    assert_eq!(after.tools, before.tools);
    let (a, b) = (
        after.totals.as_ref().unwrap(),
        before.totals.as_ref().unwrap(),
    );
    assert_eq!(
        (a.turns, a.input, a.output, a.cache_read, a.total),
        (b.turns, b.input, b.output, b.cache_read, b.total)
    );
    assert!(
        close(a.cost_usd, 0.036_36),
        "{:?}: the costs come from the usage records",
        a.cost_usd
    );
    assert_eq!(after.turns.len(), 2);
    let ids: Vec<_> = after.turns.iter().map(|t| t.turn_id).collect();
    let was: Vec<_> = before.turns.iter().map(|t| t.turn_id).collect();
    assert_eq!(ids, was);
    assert!(close(after.turns[0].cost_usd, 0.029_58));
    assert!(after.seq > before.seq || after.seq > 0);
    drop(work);
    daemon.stop().await;
}

/// A restart with a long journal: the insight is read from the newest 5 000 events that matter
/// (turn boundaries, usage, tools, files, short messages), with payloads and tool output cut out
/// in SQLite. The spike's target is 50 ms for the build; this test fails an order of magnitude
/// past it (a debug build, on a busy machine) and prints what it measured.
#[tokio::test(flavor = "multi_thread")]
async fn a_cold_build_from_five_thousand_journal_events_is_fast() {
    use hutan_core::agent::{
        AgentEvent, Delivery, FileChange, FileChangeKind, MessageRole, ProviderIds, TokenCounts,
        ToolKind, ToolStatus,
    };
    use hutan_core::{ItemId, TurnId};
    use hutan_storage::{ChatItemRow, Storage};

    let t = T::harness("omp", "insight-omp").await;
    let chat = t.create("omp").await;
    common::stop(&t.client, chat.id).await.unwrap();
    let T { daemon, work, .. } = t;
    let TestDaemon {
        _dir,
        paths,
        daemon,
    } = daemon;
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();

    // 700 turns of 8 events: the newest 5 000 are read, the oldest turn(s) cut off.
    let raw = "x".repeat(1_500);
    let output = "y".repeat(2_000);
    let mut seq = 1_000;
    let mut ts = 1_791_000_000_000_u64;
    {
        let storage = Storage::open(&paths.database).unwrap();
        for turn_n in 0..700_u64 {
            let turn = TurnId::new();
            let item = ItemId::new();
            let counts = |n: u64| TokenCounts {
                input_tokens: Some(n),
                cached_input_tokens: Some(n * 3),
                output_tokens: Some(40),
                total_tokens: Some(n * 4 + 40),
                ..TokenCounts::default()
            };
            let kinds = [
                AgentEventKind::TurnStarted,
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    phase: None,
                    text: format!("please do step {turn_n}"),
                },
                AgentEventKind::ToolStart {
                    tool: ToolKind::Command,
                    name: "bash".into(),
                    title: "cargo test".into(),
                    input: None,
                },
                AgentEventKind::ToolEnd {
                    tool: ToolKind::Command,
                    status: ToolStatus::Completed,
                    output: Some(output.clone()),
                    truncated: false,
                    exit_code: Some(0),
                    duration_ms: Some(120),
                },
                AgentEventKind::FileDiff {
                    changes: vec![FileChange {
                        path: format!("src/f{}.rs", turn_n % 40),
                        kind: FileChangeKind::Update,
                        move_path: None,
                        patch: Some("--- a\n+++ b\n@@\n-a\n+b\n".into()),
                        before: None,
                        after: None,
                    }],
                    patch: None,
                },
                AgentEventKind::Usage {
                    total: Some(TokenCounts {
                        total_tokens: Some(20_000 + turn_n),
                        ..TokenCounts::default()
                    }),
                    last: None,
                    context_window: Some(200_000),
                },
                AgentEventKind::Usage {
                    total: None,
                    last: Some(counts(100 + turn_n)),
                    context_window: None,
                },
                AgentEventKind::TurnCompleted {
                    duration_ms: Some(2_000),
                },
            ];
            for kind in kinds {
                seq += 1;
                ts += 250;
                let event = AgentEvent {
                    session_id: chat.id,
                    seq,
                    ts_ms: ts,
                    delivery: Delivery::Live,
                    turn_id: Some(turn),
                    item_id: Some(item),
                    provider: ProviderIds::default(),
                    kind,
                    payload: Some(serde_json::json!({"raw": raw})),
                };
                storage
                    .chat()
                    .append(ChatItemRow {
                        session_id: chat.id,
                        seq,
                        ts_ms: ts,
                        turn_id: event.turn_id.map(|t| t.to_string()),
                        item_id: event.item_id.map(|i| i.to_string()),
                        kind: event.kind.name().to_owned(),
                        event_json: serde_json::to_string(&event).unwrap(),
                    })
                    .unwrap();
            }
        }
        storage.close();
    }
    let daemon = common::start_when_free(hutan_daemon::DaemonConfig::new(paths.clone())).await;
    let daemon = TestDaemon {
        _dir,
        paths,
        daemon,
    };
    let client = daemon.connect().await;

    let started = std::time::Instant::now();
    let insight = client.agent_insight(chat.id, None).await.unwrap().insight;
    let took = started.elapsed();
    eprintln!("insight: a cold build from 5 000 journal events and the answer took {took:?}");
    assert!(took < Duration::from_millis(1_000), "{took:?}");

    let totals = insight.totals.as_ref().expect("totals");
    // 5 000 events of 8 per turn: 625 whole turns.
    assert_eq!(totals.turns, 625);
    assert!(insight.turns_truncated);
    assert!(insight.turns.len() <= 200);
    assert!(serde_json::to_vec(&insight).unwrap().len() <= hutan_core::insight::MAX_SNAPSHOT_BYTES);
    assert_eq!(insight.tools.as_ref().map(|t| t.calls), Some(625));
    let context = insight.context.as_ref().expect("a context");
    assert_eq!((context.used, context.window), (20_699, Some(200_000)));
    drop(work);
    daemon.stop().await;
}
