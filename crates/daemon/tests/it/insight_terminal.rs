//! Terminal agents' insight (INSIGHT-B, protocol 0.71, capability `agent.insight.report`): what
//! the integrations report (`agent.report`'s `insight`) is folded by the same reducer as a chat's
//! journal, per terminal session, and answered by `agent.insight` with the source of each block.
//!
//! The OMP reports are the ones the shipped extension (`integrations/omp/hutan-agent-state.ts`)
//! sent in a real OMP 18.14.1 run against a loopback provider (`fixtures/insight-terminal/
//! README.md`); Claude Code's and Codex's are the reports the hook script and the status-line tee
//! build from their documented payloads (`crates/cli/src/hook/insight.rs` tests that translation).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{self, TestDaemon, WAIT, remote_code, sh, wait_event};

use std::path::Path;

use hutan_client::Client;
use hutan_core::SessionId;
use hutan_core::agent::{FileChangeKind, TokenCounts};
use hutan_core::insight::{
    AgentInsight, InsightBlock, InsightField, InsightOutcome, InsightReport, InsightSourceKind,
    ReportedContext, ReportedFile, ReportedTool, ReportedTurn, TurnPhase,
};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Empty, Event, Request, SessionSnapshot,
};
use tokio::time::timeout;

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/insight-terminal")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// What a report call carries: the insight only, as the extension's own calls do.
fn insight_only(
    session: SessionId,
    source: &str,
    agent: &str,
    seq: u64,
    insight: InsightReport,
) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        session_id: session,
        source: source.into(),
        agent: agent.into(),
        state: None,
        message: None,
        seq,
        agent_session_id: None,
        resume_argv: None,
        node: None,
        insight: Some(insight),
    }
}

fn state_report(
    session: SessionId,
    source: &str,
    agent: &str,
    state: AgentState,
    seq: u64,
) -> AgentReportRequest {
    AgentReportRequest {
        state: Some(state),
        insight: None,
        ..insight_only(session, source, agent, seq, InsightReport::default())
    }
}

/// The agent says it is there (a state report), as every integration's first report does.
async fn state(client: &Client, session: SessionId, source: &str, agent: &str, seq: u64) {
    assert!(
        send(
            client,
            state_report(session, source, agent, AgentState::Idle, seq)
        )
        .await
    );
}

async fn send(client: &Client, request: AgentReportRequest) -> bool {
    client
        .call::<AgentApplied>(Request::AgentReport(request))
        .await
        .unwrap()
        .applied
}

async fn insight_of(client: &Client, session: SessionId) -> AgentInsight {
    client.agent_insight(session, None).await.unwrap().insight
}

fn source_of(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSourceKind> {
    insight
        .sources
        .iter()
        .find(|source| source.block == block)
        .map(|source| source.source)
}

fn tokens(input: u64, output: u64, cache_read: u64) -> TokenCounts {
    TokenCounts {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cached_input_tokens: Some(cache_read),
        total_tokens: Some(input + output + cache_read),
        ..TokenCounts::default()
    }
}

/// The `insight` of a recorded `hutan agent report` command line, and its `--seq`.
fn recorded(line: &str) -> Option<(u64, InsightReport)> {
    let argv: Vec<String> = serde_json::from_str(line).unwrap();
    let json = argv
        .iter()
        .find_map(|arg| arg.strip_prefix("--insight-json="))?;
    let seq = argv.iter().find_map(|arg| arg.strip_prefix("--seq="))?;
    Some((seq.parse().unwrap(), serde_json::from_str(json).unwrap()))
}

/// The reports of the shipped OMP extension, recorded from a real run, replayed over the wire:
/// the context and its fixed part at the start, the two model calls of a turn with a read and a
/// write, the turn's end, and a second turn.
#[tokio::test(flavor = "multi_thread")]
async fn the_omp_extensions_recorded_reports_become_the_insight_of_a_terminal_session() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let session = common::create(&client, sh("exec sleep 600")).await;
    // What the extension's state report says first: the agent is there.
    assert!(
        send(
            &client,
            state_report(session.id, "omp-extension", "omp", AgentState::Idle, 1)
        )
        .await
    );

    let reports: Vec<(u64, InsightReport)> = fixture("omp-extension-reports.jsonl")
        .lines()
        .filter_map(recorded)
        .collect();
    assert!(
        reports.len() >= 4,
        "the recording has the start, a turn and its end"
    );
    for (seq, report) in &reports {
        // The recorded `seq`s are the extension's own (microseconds): newer than the state's 1.
        assert!(
            send(
                &client,
                insight_only(session.id, "omp-extension", "omp", *seq, report.clone())
            )
            .await
        );
    }
    let insight = insight_of(&client, session.id).await;

    assert_eq!(insight.harness.as_deref(), Some("omp"));
    assert!(
        !insight.can_measure,
        "a terminal's OMP is not asked for /context"
    );
    let context = insight.context.as_ref().expect("a context");
    // 7 745 tokens before the first message (all of it fixed), 11 300 after the turn.
    assert_eq!((context.used, context.window), (11_300, Some(200_000)));
    assert_eq!(
        context
            .breakdown
            .iter()
            .map(|row| row.tokens)
            .collect::<Vec<_>>(),
        [7_745, 3_555]
    );
    let rows: Vec<&str> = context
        .breakdown
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(
        rows,
        ["fixed", "messages"],
        "the two-part split of OMP's extension"
    );
    assert_eq!(
        insight.turns.last().map(|turn| turn.outcome),
        Some(InsightOutcome::Completed)
    );
    let totals = insight.totals.as_ref().expect("totals");
    // Two turns: 8 300 + 1 300 uncached input, 14 000 + 10 000 read from the cache.
    assert_eq!(
        (totals.turns, totals.input, totals.output, totals.cache_read),
        (2, Some(9_600), Some(76), Some(24_000))
    );
    assert_eq!(
        totals.cost_usd,
        Some(0.03714),
        "the cost OMP's table gives, cumulative"
    );
    let first = &insight.turns[0];
    assert_eq!(
        (first.input, first.cost_usd, first.tool_calls, first.files),
        (Some(8_300), Some(0.03012), 2, 1)
    );
    assert_eq!(
        insight.turns[1].cost_usd.map(|c| (c * 1e5).round()),
        Some(702.0)
    );
    let tools = insight.tools.as_ref().expect("tools");
    assert_eq!(tools.calls, 2);
    let read = tools.by_name.iter().find(|tool| tool.name == "read");
    assert_eq!(read.map(|tool| (tool.calls, tool.ms)), Some((1, Some(6))));
    assert!(tools.by_name.iter().any(|tool| tool.name == "write"));
    let files = insight.files.as_ref().expect("files");
    let written = files
        .items
        .iter()
        .find(|file| file.path.ends_with("out.txt"))
        .expect("the written file");
    assert_eq!(
        written.kind,
        FileChangeKind::Add,
        "a file the write tool created"
    );
    for block in [
        InsightBlock::Context,
        InsightBlock::Breakdown,
        InsightBlock::Turns,
        InsightBlock::Tools,
        InsightBlock::Files,
    ] {
        assert_eq!(
            source_of(&insight, block),
            Some(InsightSourceKind::Extension),
            "{block:?}"
        );
    }

    // Insight-only reports left the agent's state alone: still the idle the first report said.
    let snapshot: SessionSnapshot = client
        .call(Request::SessionSnapshot(Empty {}))
        .await
        .unwrap();
    let now = snapshot
        .sessions
        .into_iter()
        .find(|s| s.id == session.id)
        .unwrap();
    assert_eq!(now.agent.map(|agent| agent.state), Some(AgentState::Idle));
    t.stop().await;
}

/// Claude Code: the hooks give tools, files and the turn's edges; the status-line tee the context
/// and the cost. Each block names its source, and a worse source never overwrites a better one.
#[tokio::test(flavor = "multi_thread")]
async fn claude_hooks_and_the_status_line_fill_their_own_blocks_and_say_so() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let session = common::create(&client, sh("exec sleep 600")).await;
    assert!(
        send(
            &client,
            state_report(session.id, "claude-hooks", "claude", AgentState::Working, 1)
        )
        .await
    );

    let hooks = |seq: u64, report: InsightReport| {
        insight_only(session.id, "claude-hooks", "claude", seq, report)
    };
    let from_hooks = |report: InsightReport| InsightReport {
        source: InsightSourceKind::Hooks,
        ..report
    };
    let turn = |phase| {
        Some(ReportedTurn {
            phase,
            outcome: None,
            duration_ms: None,
        })
    };
    assert!(
        send(
            &client,
            hooks(
                10,
                from_hooks(InsightReport {
                    turn: turn(TurnPhase::Start),
                    ..InsightReport::default()
                })
            )
        )
        .await
    );
    assert!(
        send(
            &client,
            hooks(
                11,
                from_hooks(InsightReport {
                    tools: vec![ReportedTool {
                        name: "Edit".into(),
                        calls: 1,
                        failed: 0,
                        ms: Some(38)
                    }],
                    files: vec![ReportedFile {
                        path: "/does/not/matter/src/lib.rs".into(),
                        kind: FileChangeKind::Update
                    }],
                    ..InsightReport::default()
                })
            )
        )
        .await
    );
    // The status-line tee: a source of its own, with its own sequence.
    let tee = InsightReport {
        source: InsightSourceKind::Statusline,
        context: Some(ReportedContext {
            used: 60_008,
            window: Some(200_000),
            fixed: None,
            estimated: false,
        }),
        cost_usd: Some(0.4127),
        api_ms: Some(21_400),
        ..InsightReport::default()
    };
    assert!(
        send(
            &client,
            insight_only(session.id, "claude-statusline", "claude", 5, tee)
        )
        .await
    );
    // The hooks also guess a context: the status line has it already, so it is not taken.
    let worse = InsightReport {
        source: InsightSourceKind::Hooks,
        context: Some(ReportedContext {
            used: 1,
            window: None,
            fixed: None,
            estimated: true,
        }),
        ..InsightReport::default()
    };
    assert!(send(&client, hooks(12, worse)).await);
    assert!(
        send(
            &client,
            hooks(
                13,
                from_hooks(InsightReport {
                    turn: turn(TurnPhase::End),
                    ..InsightReport::default()
                })
            )
        )
        .await
    );

    let insight = insight_of(&client, session.id).await;
    assert_eq!(
        insight.context.as_ref().map(|c| (c.used, c.window)),
        Some((60_008, Some(200_000)))
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Context),
        Some(InsightSourceKind::Statusline)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Tools),
        Some(InsightSourceKind::Hooks)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Files),
        Some(InsightSourceKind::Hooks)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Turns),
        Some(InsightSourceKind::Hooks)
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Time),
        Some(InsightSourceKind::Statusline)
    );
    assert_eq!(
        insight.time.as_ref().and_then(|time| time.api_ms),
        Some(21_400)
    );
    assert_eq!(
        insight.totals.as_ref().and_then(|totals| totals.cost_usd),
        Some(0.4127)
    );
    // Hooks carry no tokens: the turn has none, and nothing is drawn as zero.
    assert_eq!(insight.turns.len(), 1);
    assert_eq!(
        (
            insight.turns[0].input,
            insight.turns[0].tool_calls,
            insight.turns[0].files
        ),
        (None, 1, 1)
    );
    assert!(insight.unsupported.contains(&InsightField::Breakdown));
    t.stop().await;
}

/// A report that does not carry the newest `seq` of its source is not about the present: its
/// tools and files still count (deltas that arrive late are as true as the others), its levels
/// do not replace the newer ones.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_report_adds_its_deltas_and_does_not_replace_a_newer_level() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let session = common::create(&client, sh("exec sleep 600")).await;
    state(&client, session.id, "claude-hooks", "claude", 1).await;
    let level = |used: u64, tool: &str| InsightReport {
        source: InsightSourceKind::Hooks,
        context: Some(ReportedContext {
            used,
            window: Some(1_000),
            fixed: None,
            estimated: false,
        }),
        tools: vec![ReportedTool {
            name: tool.into(),
            calls: 1,
            failed: 0,
            ms: None,
        }],
        ..InsightReport::default()
    };
    assert!(
        send(
            &client,
            insight_only(session.id, "claude-hooks", "claude", 20, level(700, "Bash"))
        )
        .await
    );
    // Older than the report above: ignored for the state, counted for the tools.
    assert!(
        !send(
            &client,
            insight_only(session.id, "claude-hooks", "claude", 19, level(100, "Read"))
        )
        .await
    );
    let insight = insight_of(&client, session.id).await;
    assert_eq!(insight.context.map(|context| context.used), Some(700));
    let tools = insight.tools.unwrap();
    assert_eq!(tools.calls, 2);
    assert!(tools.by_name.iter().any(|tool| tool.name == "Read"));
    t.stop().await;
}

/// What is refused, and what a plain terminal and a chat's `/context` do.
#[tokio::test(flavor = "multi_thread")]
async fn bad_reports_and_sessions_without_an_agent_are_errors_not_zeros() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let shell = common::create(&client, sh("exec sleep 600")).await;

    // No agent yet: not an agent session.
    assert_eq!(
        remote_code(client.agent_insight(shell.id, None).await),
        ErrorCode::BadRequest
    );

    // `chat` is the daemon's own source; a report over 4 KiB is too large.
    let chat = InsightReport {
        source: InsightSourceKind::Chat,
        ..InsightReport::default()
    };
    let refused = client
        .call::<AgentApplied>(Request::AgentReport(insight_only(
            shell.id, "x", "omp", 1, chat,
        )))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    let big = InsightReport {
        files: (0..32)
            .map(|n| ReportedFile {
                path: format!("{}{n}", "d/".repeat(120)),
                kind: FileChangeKind::Update,
            })
            .collect(),
        ..InsightReport::default()
    };
    let refused = client
        .call::<AgentApplied>(Request::AgentReport(insight_only(
            shell.id, "x", "omp", 2, big,
        )))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);

    // Once an agent reports, the terminal has an insight (empty), and cannot be measured.
    assert!(
        send(
            &client,
            state_report(shell.id, "omp-extension", "omp", AgentState::Idle, 3)
        )
        .await
    );
    let empty = insight_of(&client, shell.id).await;
    assert!(empty.sources.is_empty() && empty.turns.is_empty() && !empty.can_measure);
    assert_eq!(
        remote_code(client.agent_measure_context(shell.id).await),
        ErrorCode::Unsupported
    );
    t.stop().await;
}

/// A focused connection is told when a terminal agent's insight changes, at most once a second.
#[tokio::test(flavor = "multi_thread")]
async fn a_focused_connection_is_pushed_the_insight_of_a_terminal_agent() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let (watcher, mut watched) = t.connect_subscribed().await;
    let session = common::create(&client, sh("exec sleep 600")).await;
    assert!(
        send(
            &client,
            state_report(session.id, "omp-extension", "omp", AgentState::Working, 1)
        )
        .await
    );
    watcher.client_focus(Some(session.id)).await.unwrap();

    let report = InsightReport {
        source: InsightSourceKind::Extension,
        tokens: Some(tokens(100, 10, 0)),
        tools: vec![ReportedTool {
            name: "bash".into(),
            calls: 1,
            failed: 1,
            ms: Some(9),
        }],
        ..InsightReport::default()
    };
    assert!(
        send(
            &client,
            insight_only(session.id, "omp-extension", "omp", 2, report)
        )
        .await
    );
    let pushed = timeout(WAIT, async {
        wait_event(
            &mut watched,
            "the terminal's insight",
            |event| match event {
                Event::AgentInsightUpdated {
                    session_id,
                    insight,
                } if *session_id == session.id => Some(insight.clone()),
                _ => None,
            },
        )
        .await
    })
    .await
    .expect("pushed");
    assert_eq!(
        pushed.tools.map(|tools| (tools.calls, tools.failed)),
        Some((1, 1))
    );
    t.stop().await;
}

/// Another agent in the same terminal starts the insight again.
#[tokio::test(flavor = "multi_thread")]
async fn another_agent_in_the_terminal_starts_the_insight_again() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let session = common::create(&client, sh("exec sleep 600")).await;
    state(&client, session.id, "omp-extension", "omp", 1).await;
    let tools = |name: &str| InsightReport {
        tools: vec![ReportedTool {
            name: name.into(),
            calls: 3,
            failed: 0,
            ms: None,
        }],
        ..InsightReport::default()
    };
    assert!(
        send(
            &client,
            insight_only(session.id, "omp-extension", "omp", 2, tools("read"))
        )
        .await
    );
    assert_eq!(
        insight_of(&client, session.id).await.tools.map(|t| t.calls),
        Some(3)
    );
    // The user left OMP and started Codex in the same terminal.
    assert!(
        send(
            &client,
            insight_only(
                session.id,
                "codex-hooks",
                "codex",
                1,
                InsightReport {
                    source: InsightSourceKind::Hooks,
                    ..tools("Bash")
                }
            )
        )
        .await
    );
    let insight = insight_of(&client, session.id).await;
    assert_eq!(insight.harness.as_deref(), Some("codex"));
    assert_eq!(insight.tools.map(|t| t.calls), Some(3));
    assert!(
        insight.unsupported.contains(&InsightField::Cost),
        "Codex reports no cost"
    );
    t.stop().await;
}
