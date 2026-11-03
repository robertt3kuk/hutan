//! The insight of a terminal Claude Code and Codex from their own transcripts (INSIGHT-C):
//! tokens, context, cost, tools with times and exit codes, files, per terminal session, through a
//! real daemon whose `[history]` roots point at temporary directories (never the user's agent
//! directories), over a real socket.
//!
//! The transcripts are the synthetic ones of `crates/index/tests/fixtures/usage/` in the shapes
//! `docs/integrations.md` § Insight lists as recorded or not; the hooks' reports are what the hook
//! script sends (`crates/cli/src/hook.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{self, TestDaemon, WAIT, sh, wait_event};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hutan_client::Client;
use hutan_core::agent::FileChangeKind;
use hutan_core::insight::{
    AgentInsight, InsightBlock, InsightField, InsightOutcome, InsightReport, InsightSourceKind,
    ReportedFile, ReportedTool, ReportedTurn, TurnPhase,
};
use hutan_core::{NodeKind, SessionId};
use hutan_protocol::checks::{AgentsFindings, AgentsFindingsRequest, detector};
use hutan_protocol::history::{
    HistorySessionRow, HistorySessions, HistorySessionsRequest, HistorySetEnabled,
    HistoryStatusInfo,
};
use hutan_protocol::messages::{
    AgentApplied, AgentReportRequest, AgentState, Event, Request, SessionInfo, TerminalSpec,
};
use hutan_protocol::tree::NodeReport;
use tokio::time::{sleep, timeout};

const CLAUDE_ID: &str = "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60";
const CODEX_ID: &str = "0198aaaa-bbbb-cccc-dddd-eeeeffff0001";

// Fixtures ------------------------------------------------------------------------------------

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../index/tests/fixtures/usage")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The recorded-shape Claude Code transcript, in `cwd` as conversation `id`.
fn claude_transcript(cwd: &Path, id: &str) -> String {
    fixture("claude-session.jsonl")
        .replace("/work/app", &cwd.display().to_string())
        .replace(CLAUDE_ID, id)
}

fn codex_transcript(cwd: &Path, id: &str) -> String {
    fixture("codex-rollout.jsonl")
        .replace("/work/app", &cwd.display().to_string())
        .replace(CODEX_ID, id)
}

/// `2026-11-03T10:00:00.000Z` for Unix milliseconds.
fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 1970 to a civil date (H. Hinnant's algorithm).
    let z = i64::try_from(days).unwrap() + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let day_of_year = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_p = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_p + 2) / 5 + 1;
    let month = if month_p < 10 {
        month_p + 3
    } else {
        month_p - 9
    };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        ms % 1000
    )
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A short Claude Code conversation that happened just now: one prompt, one answer.
fn fresh_claude(cwd: &Path, id: &str, input: u64) -> String {
    let at = now_ms();
    let cwd = cwd.display();
    let (a, b) = (iso(at), iso(at + 500));
    format!(
        r#"{{"type":"user","sessionId":"{id}","cwd":"{cwd}","timestamp":"{a}","message":{{"role":"user","content":"hello"}}}}
{{"type":"assistant","sessionId":"{id}","cwd":"{cwd}","timestamp":"{b}","message":{{"id":"msg_{id}","role":"assistant","model":"claude-opus-5-5","content":[{{"type":"text","text":"hi"}}],"stop_reason":"end_turn","usage":{{"input_tokens":{input},"cache_read_input_tokens":100,"output_tokens":7}}}}}}
"#
    )
}

// The environment -----------------------------------------------------------------------------

struct Env {
    _tmp: tempfile::TempDir,
    /// The temporary directory, canonical (`/private/var/…`).
    base: PathBuf,
    daemon: TestDaemon,
    client: Client,
    events: tokio::sync::broadcast::Receiver<(hutan_core::Seq, Event)>,
}

fn toml(base: &Path, enabled: bool, claude: bool, codex: bool) -> String {
    format!(
        "[history]\nenabled = {enabled}\nclaude = {claude}\ncodex = {codex}\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
        base.join("sources/claude").display(),
        base.join("sources/codex").display(),
        base.join("sources/omp").display(),
    )
}

async fn start(enabled: bool, claude: bool, codex: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for dir in ["claude", "codex", "omp"] {
        fs::create_dir_all(base.join("sources").join(dir)).unwrap();
    }
    let daemon = TestDaemon::start_with(&toml(&base, enabled, claude, codex), |_| {}).await;
    let (client, events) = daemon.connect_subscribed().await;
    Env {
        _tmp: tmp,
        base,
        daemon,
        client,
        events,
    }
}

impl Env {
    fn work(&self, name: &str) -> PathBuf {
        let dir = self.base.join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn claude_file(&self, cwd: &Path, id: &str) -> PathBuf {
        let slug = cwd.display().to_string().replace('/', "-");
        self.base
            .join("sources/claude")
            .join(slug)
            .join(format!("{id}.jsonl"))
    }

    fn codex_file(&self, id: &str) -> PathBuf {
        self.base.join(format!(
            "sources/codex/2026/11/03/rollout-2026-11-03T10-00-00-{id}.jsonl"
        ))
    }

    async fn terminal(&self, cwd: &Path) -> SessionInfo {
        common::create(
            &self.client,
            TerminalSpec {
                cwd: Some(cwd.to_owned()),
                ..sh("exec sleep 600")
            },
        )
        .await
    }

    async fn insight(&self, session: SessionId) -> AgentInsight {
        self.client
            .agent_insight(session, None)
            .await
            .unwrap()
            .insight
    }

    /// Asks for the insight until `done` says it is the one waited for.
    async fn insight_where(
        &self,
        session: SessionId,
        what: &str,
        done: impl Fn(&AgentInsight) -> bool,
    ) -> AgentInsight {
        timeout(WAIT, async {
            loop {
                let insight = self.insight(session).await;
                if done(&insight) {
                    return insight;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the insight never showed {what}"))
    }

    /// Until the history index has read the transcript of `key` (the catalog lists it).
    async fn indexed(&self, key: &str) -> HistorySessionRow {
        timeout(WAIT, async {
            loop {
                let answer: HistorySessions = self
                    .client
                    .call(Request::HistorySessions(HistorySessionsRequest {
                        scope: hutan_protocol::history::HistoryScope::All,
                        workspace_id: None,
                        filter: String::new(),
                        sort: hutan_protocol::history::HistorySessionSort::Updated,
                        sources: Vec::new(),
                        limit: 250,
                        refresh: true,
                    }))
                    .await
                    .unwrap();
                if let Some(row) = answer.sessions.into_iter().find(|r| r.session_key == key) {
                    return row;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the history index never listed {key}"))
    }

    async fn set_history(&self, enabled: bool) {
        self.client
            .call::<HistoryStatusInfo>(Request::HistorySetEnabled(HistorySetEnabled { enabled }))
            .await
            .unwrap();
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

// What the hooks say -----------------------------------------------------------------------------

async fn report(client: &Client, request: AgentReportRequest) -> bool {
    client
        .call::<AgentApplied>(Request::AgentReport(request))
        .await
        .unwrap()
        .applied
}

fn base_report(
    session: SessionId,
    source: &str,
    agent: &str,
    seq: u64,
    id: Option<&str>,
) -> AgentReportRequest {
    AgentReportRequest {
        pid: None,
        session_id: session,
        source: source.into(),
        agent: agent.into(),
        state: Some(AgentState::Idle),
        message: None,
        seq,
        agent_session_id: id.map(str::to_owned),
        resume_argv: None,
        node: None,
        insight: None,
    }
}

/// What Claude Code's hook script reports for a main-agent event: the state, the conversation
/// id and (with `path`) the transcript the hook named, on the root node.
async fn claude_hook(
    client: &Client,
    session: SessionId,
    seq: u64,
    id: Option<&str>,
    path: Option<&Path>,
) {
    let mut request = base_report(session, "claude-hooks", "claude", seq, id);
    request.node = Some(NodeReport {
        id: "root".into(),
        parent: None,
        kind: NodeKind::Main,
        name: None,
        agent_type: None,
        outcome: None,
        model: None,
        activity: None,
        usage: None,
        transcript_path: path.map(|p| p.display().to_string()),
    });
    assert!(report(client, request).await);
}

/// What Codex's hook script reports: the state and the conversation id.
async fn codex_hook(client: &Client, session: SessionId, seq: u64, id: &str) {
    assert!(
        report(
            client,
            base_report(session, "codex-hooks", "codex", seq, Some(id))
        )
        .await
    );
}

fn source_of(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSourceKind> {
    insight
        .sources
        .iter()
        .find(|source| source.block == block)
        .map(|source| source.source)
}

fn tool(insight: &AgentInsight, name: &str) -> (u64, u64, Option<u64>) {
    let row = insight
        .tools
        .as_ref()
        .and_then(|tools| tools.by_name.iter().find(|row| row.name == name))
        .unwrap_or_else(|| panic!("no tool row {name}"));
    (row.calls, row.failed, row.ms)
}

// Tests -----------------------------------------------------------------------------------------

/// Claude Code's hook names its transcript: the session has tokens, the context (with the
/// window `/context` gave and its categories), the cost, tools with their times and failures,
/// files, time and the model.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_claude_code_gets_its_insight_from_the_transcript_its_hook_named() {
    let env = start(true, true, true).await;
    let cwd = env.work("claude-app");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    write(&path, &claude_transcript(&cwd, CLAUDE_ID));
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;

    let insight = env.insight(session.id).await;
    assert_eq!(insight.harness.as_deref(), Some("claude"));
    assert_eq!(
        insight.model.as_ref().map(|m| m.id.as_str()),
        Some("claude-opus-5-5")
    );
    let totals = insight.totals.as_ref().expect("totals");
    // Two turns; the first of five messages (one split over two lines), the second of one.
    assert_eq!(totals.turns, 2);
    assert_eq!(
        (
            totals.input,
            totals.output,
            totals.cache_read,
            totals.cache_write
        ),
        (Some(19), Some(500), Some(62_863), Some(500))
    );
    assert_eq!(totals.cost_usd, Some(0.42), "Claude Code's own cost-state");
    assert!(
        !insight.unsupported.contains(&InsightField::Cost),
        "a cost was reported"
    );
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window),
        (16_964, Some(1_000_000)),
        "the newest figure is `/context`'s, with its window"
    );
    assert_eq!(context.breakdown.len(), 6);
    assert_eq!(context.compact_at, Some(1_000_000 - 33_000));
    assert!(
        !insight.unsupported.contains(&InsightField::Breakdown),
        "a /context record is a breakdown"
    );
    // Tools with their times; the failed command is the interrupted turn's.
    assert_eq!(tool(&insight, "Bash"), (2, 1, Some(5_300)));
    assert_eq!(tool(&insight, "Edit"), (1, 0, Some(400)));
    assert_eq!(tool(&insight, "Write"), (1, 0, Some(100)));
    let files = insight.files.as_ref().expect("files");
    let listed: Vec<(&str, FileChangeKind)> = files
        .items
        .iter()
        .map(|file| (file.path.as_str(), file.kind))
        .collect();
    assert_eq!(
        listed,
        [
            ("src/lib.rs", FileChangeKind::Update),
            ("src/new.rs", FileChangeKind::Add)
        ],
        "relative to the session's folder"
    );
    assert_eq!(insight.turns.len(), 2);
    assert_eq!(insight.turns[0].outcome, InsightOutcome::Completed);
    assert_eq!(insight.turns[0].duration_ms, Some(12_000));
    assert_eq!(insight.turns[0].tool_calls, 3);
    assert_eq!(insight.turns[1].outcome, InsightOutcome::Cancelled);
    assert_eq!(insight.time.as_ref().and_then(|t| t.api_ms), Some(9_100));
    for block in [
        InsightBlock::Context,
        InsightBlock::Breakdown,
        InsightBlock::Turns,
        InsightBlock::Tools,
        InsightBlock::Files,
    ] {
        assert_eq!(
            source_of(&insight, block),
            Some(InsightSourceKind::Transcript),
            "{block:?} is labelled `transcript`"
        );
    }
    env.stop().await;
}

/// Codex's hooks give the conversation id; the history catalog gives the rollout. The context is
/// the latest request's, not the thread's total, with Codex's window; tool times and exit codes
/// come from the rollout.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_codex_gets_its_insight_from_the_rollout_its_session_id_names() {
    let env = start(true, true, true).await;
    let cwd = env.work("codex-app");
    write(&env.codex_file(CODEX_ID), &codex_transcript(&cwd, CODEX_ID));
    let session = env.terminal(&cwd).await;
    codex_hook(&env.client, session.id, 1, CODEX_ID).await;
    // The rollout is in the catalog once the index has read it.
    let row = env.indexed(CODEX_ID).await;
    assert!(row.path.as_deref().is_some_and(|p| p.ends_with(".jsonl")));

    let insight = env
        .insight_where(session.id, "the rollout's numbers", |i| i.totals.is_some())
        .await;
    assert_eq!(insight.harness.as_deref(), Some("codex"));
    assert_eq!(
        insight.model.as_ref().map(|m| m.id.as_str()),
        Some("gpt-5-codex")
    );
    let totals = insight.totals.as_ref().expect("totals");
    // Two turns (the second was aborted). The thread's cumulative total ended at 4 100.
    assert_eq!(totals.turns, 2);
    assert_eq!(
        (
            totals.input,
            totals.cache_read,
            totals.output,
            totals.reasoning,
            totals.total
        ),
        (Some(3_900), Some(2_400), Some(200), Some(60), Some(4_100))
    );
    assert_eq!(totals.cost_usd, None, "Codex reports no cost");
    assert!(insight.unsupported.contains(&InsightField::Cost));
    let context = insight.context.as_ref().expect("a context");
    assert_eq!(
        (context.used, context.window),
        (1_580, Some(272_000)),
        "the latest request, not the 4 100 of the thread"
    );
    // Exit codes: the second `cargo test` failed. Times from the rollout.
    assert_eq!(tool(&insight, "Bash"), (2, 1, Some(4_500)));
    assert_eq!(tool(&insight, "apply_patch"), (1, 0, None));
    assert_eq!(tool(&insight, "mcp__docs__search"), (1, 0, Some(250)));
    let files = insight.files.as_ref().expect("files");
    let listed: Vec<(&str, FileChangeKind)> = files
        .items
        .iter()
        .map(|file| (file.path.as_str(), file.kind))
        .collect();
    assert_eq!(
        listed,
        [
            ("src/lib.rs", FileChangeKind::Update),
            ("src/retry.rs", FileChangeKind::Add)
        ]
    );
    for block in [
        InsightBlock::Context,
        InsightBlock::Turns,
        InsightBlock::Tools,
        InsightBlock::Files,
    ] {
        assert_eq!(
            source_of(&insight, block),
            Some(InsightSourceKind::Transcript),
            "{block:?}"
        );
    }
    env.stop().await;
}

/// Without the user's consent nothing is read: a transcript the hook names is not opened, and the
/// session has no tokens. The consent is `[history]`: the index on and the agent's source on.
/// Turning it on shows the numbers, turning it off takes them away again.
#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_read_without_consent_and_withdrawing_it_takes_the_numbers_away() {
    // The index is off (the default): the hook names a transcript, which stays closed.
    let env = start(false, true, true).await;
    let cwd = env.work("consent");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    write(&path, &claude_transcript(&cwd, CLAUDE_ID));
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;
    let insight = env.insight(session.id).await;
    assert!(
        insight.totals.is_none() && insight.context.is_none() && insight.tools.is_none(),
        "{insight:?}"
    );
    assert!(insight.sources.is_empty());
    assert!(
        !env.daemon.paths.index_database.exists(),
        "no index, no transcript directory opened"
    );

    // The user allows it.
    env.set_history(true).await;
    let insight = env
        .insight_where(session.id, "the transcript's numbers", |i| {
            i.totals.is_some()
        })
        .await;
    assert_eq!(insight.totals.as_ref().map(|t| t.turns), Some(2));

    // And takes it back: what was read is dropped.
    env.set_history(false).await;
    let insight = env
        .insight_where(session.id, "nothing", |i| {
            i.totals.is_none() && i.context.is_none()
        })
        .await;
    assert!(insight.sources.is_empty(), "{insight:?}");
    env.stop().await;
}

/// The consent is per agent: with `codex = false` a Codex session reads nothing while a Claude
/// Code one beside it does.
#[tokio::test(flavor = "multi_thread")]
async fn the_consent_is_per_source() {
    let env = start(true, true, false).await;
    let cwd = env.work("per-source");
    let claude_path = env.claude_file(&cwd, CLAUDE_ID);
    write(&claude_path, &claude_transcript(&cwd, CLAUDE_ID));
    write(&env.codex_file(CODEX_ID), &codex_transcript(&cwd, CODEX_ID));
    let claude = env.terminal(&cwd).await;
    claude_hook(
        &env.client,
        claude.id,
        1,
        Some(CLAUDE_ID),
        Some(&claude_path),
    )
    .await;
    let codex = env.terminal(&cwd).await;
    codex_hook(&env.client, codex.id, 1, CODEX_ID).await;
    env.indexed(CLAUDE_ID).await;

    assert!(env.insight(claude.id).await.totals.is_some());
    let codex_insight = env.insight(codex.id).await;
    assert!(codex_insight.totals.is_none() && codex_insight.context.is_none());
    env.stop().await;
}

/// Two Claude Codes in one folder: each gets its own transcript through its own conversation id,
/// and neither gets the other's.
#[tokio::test(flavor = "multi_thread")]
async fn two_agents_in_one_folder_each_get_their_own_transcript() {
    let env = start(true, true, true).await;
    let cwd = env.work("shared");
    let (id_a, id_b) = (CLAUDE_ID, "7a7a7a7a-1111-4222-8333-944444444444");
    let (path_a, path_b) = (env.claude_file(&cwd, id_a), env.claude_file(&cwd, id_b));
    write(&path_a, &claude_transcript(&cwd, id_a));
    write(&path_b, &fresh_claude(&cwd, id_b, 4_321));
    let a = env.terminal(&cwd).await;
    let b = env.terminal(&cwd).await;
    claude_hook(&env.client, a.id, 1, Some(id_a), Some(&path_a)).await;
    claude_hook(&env.client, b.id, 1, Some(id_b), Some(&path_b)).await;

    let (insight_a, insight_b) = (env.insight(a.id).await, env.insight(b.id).await);
    let a_totals = insight_a.totals.as_ref().expect("a's totals");
    let b_totals = insight_b.totals.as_ref().expect("b's totals");
    assert_eq!((a_totals.turns, a_totals.input), (2, Some(19)));
    assert_eq!((b_totals.turns, b_totals.input), (1, Some(4_321)));
    assert!(
        insight_b.tools.is_none() && insight_b.files.is_none(),
        "a's tools are not b's"
    );

    // The same with the conversation id alone (Codex-style: the catalog gives the file).
    let cwd = env.work("shared-codex");
    let (codex_a, codex_b) = (CODEX_ID, "0198aaaa-bbbb-cccc-dddd-eeeeffff0002");
    write(&env.codex_file(codex_a), &codex_transcript(&cwd, codex_a));
    write(
        &env.codex_file(codex_b),
        &codex_transcript(&cwd, codex_b).replace("\"total_tokens\":4100", "\"total_tokens\":9100"),
    );
    let (c, d) = (env.terminal(&cwd).await, env.terminal(&cwd).await);
    codex_hook(&env.client, c.id, 1, codex_a).await;
    codex_hook(&env.client, d.id, 1, codex_b).await;
    env.indexed(codex_a).await;
    env.indexed(codex_b).await;
    let first = env
        .insight_where(c.id, "c's rollout", |i| i.totals.is_some())
        .await;
    let second = env
        .insight_where(d.id, "d's rollout", |i| i.totals.is_some())
        .await;
    assert_eq!(first.totals.as_ref().and_then(|t| t.total), Some(4_100));
    assert_eq!(second.totals.as_ref().and_then(|t| t.total), Some(9_100));
    env.stop().await;
}

/// Without an id or a path the folder is the only clue, and it is not enough while another
/// session runs the same agent there: nothing is attached. Once the other one is gone the only
/// transcript written since this agent started is attached, and said to be a likely match.
#[tokio::test(flavor = "multi_thread")]
async fn a_folder_alone_never_picks_between_two_agents_and_picks_the_only_candidate() {
    let env = start(true, true, true).await;
    let cwd = env.work("anonymous");
    let id = "8b8b8b8b-1111-4222-8333-944444444444";
    write(&env.claude_file(&cwd, id), &fresh_claude(&cwd, id, 777));
    let (c, d) = (env.terminal(&cwd).await, env.terminal(&cwd).await);
    // Two Claude Codes that report no conversation and no transcript.
    for session in [&c, &d] {
        claude_hook(&env.client, session.id, 1, None, None).await;
    }
    env.indexed(id).await;
    for session in [&c, &d] {
        let insight = env.insight(session.id).await;
        assert!(
            insight.totals.is_none() && insight.context.is_none(),
            "a transcript was attached to one of two agents by its folder: {insight:?}"
        );
    }
    // One of them ends: the other is the only agent of the folder.
    common::stop(&env.client, c.id).await.unwrap();
    let insight = env
        .insight_where(d.id, "the only transcript of the folder", |i| {
            i.totals.is_some()
        })
        .await;
    assert_eq!(insight.totals.as_ref().and_then(|t| t.input), Some(777));
    env.stop().await;
}

/// A transcript that is rewritten shorter, and a conversation that is replaced by another one
/// (`/clear`: a new id and a new file), start the numbers again: what is shown is the new
/// transcript's, not the sum.
#[tokio::test(flavor = "multi_thread")]
async fn a_transcript_rotated_mid_session_starts_the_numbers_again() {
    let env = start(true, true, true).await;
    let cwd = env.work("rotation");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    write(&path, &claude_transcript(&cwd, CLAUDE_ID));
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;
    let before = env.insight(session.id).await;
    assert_eq!(before.totals.as_ref().map(|t| t.turns), Some(2));

    // The file is rewritten: only the first turn is in it now.
    let whole = claude_transcript(&cwd, CLAUDE_ID);
    let first_turn: String = whole
        .lines()
        .take(13)
        .map(|line| format!("{line}\n"))
        .collect();
    write(&path, &first_turn);
    let after = env
        .insight_where(session.id, "the rewritten file's numbers", |i| {
            i.totals.as_ref().is_some_and(|t| t.turns == 1)
        })
        .await;
    assert_eq!(after.totals.as_ref().and_then(|t| t.output), Some(470));
    assert!(after.tools.as_ref().is_some_and(|t| t.calls == 3));

    // The user typed /clear: another conversation, another file.
    let other = "9c9c9c9c-1111-4222-8333-944444444444";
    let other_path = env.claude_file(&cwd, other);
    write(&other_path, &fresh_claude(&cwd, other, 5));
    claude_hook(&env.client, session.id, 2, Some(other), Some(&other_path)).await;
    let cleared = env
        .insight_where(session.id, "the new conversation's numbers", |i| {
            i.totals.as_ref().is_some_and(|t| t.input == Some(5))
        })
        .await;
    let totals = cleared.totals.as_ref().expect("totals");
    assert_eq!(
        (totals.turns, totals.output),
        (1, Some(7)),
        "the old conversation is not added to the new one"
    );
    assert!(cleared.tools.is_none(), "the old tools went with it");
    env.stop().await;
}

/// A focused session's transcript is followed: lines the agent appends reach the focused client
/// as `agent.insight_updated` events without anyone asking.
#[tokio::test(flavor = "multi_thread")]
async fn a_focused_session_is_followed_and_pushed_as_the_transcript_grows() {
    let mut env = start(true, true, true).await;
    let cwd = env.work("follow");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    let whole = claude_transcript(&cwd, CLAUDE_ID);
    let lines: Vec<&str> = whole.lines().collect();
    let first: String = lines[..13].iter().map(|l| format!("{l}\n")).collect();
    let rest: String = lines[13..].iter().map(|l| format!("{l}\n")).collect();
    write(&path, &first);
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;

    env.client.client_focus(Some(session.id)).await.unwrap();
    let wanted = session.id;
    let one = wait_event(&mut env.events, "the first turn", |event| match event {
        Event::AgentInsightUpdated {
            session_id,
            insight,
        } if *session_id == wanted && insight.totals.as_ref().is_some_and(|t| t.turns == 1) => {
            Some((**insight).clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(one.totals.as_ref().and_then(|t| t.output), Some(470));

    // The agent goes on writing.
    append(&path, &rest);
    let two = wait_event(&mut env.events, "the second turn", |event| match event {
        Event::AgentInsightUpdated {
            session_id,
            insight,
        } if *session_id == wanted && insight.totals.as_ref().is_some_and(|t| t.turns == 2) => {
            Some((**insight).clone())
        }
        _ => None,
    })
    .await;
    assert_eq!(two.totals.as_ref().and_then(|t| t.cost_usd), Some(0.42));
    assert_eq!(two.totals.as_ref().and_then(|t| t.output), Some(500));
    env.stop().await;
}

/// Hooks and transcript see the same tools and files: they are not added. Each block is the best
/// source's that has the numbers: the hooks' tools and files (they ranked higher), the
/// transcript's turns (the hooks' carry no tokens) and context.
#[tokio::test(flavor = "multi_thread")]
async fn what_hooks_and_the_transcript_both_saw_is_counted_once() {
    let env = start(true, true, true).await;
    let cwd = env.work("both");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    write(&path, &claude_transcript(&cwd, CLAUDE_ID));
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;

    let hooks = |seq: u64, report: InsightReport| {
        let mut request = base_report(session.id, "claude-hooks", "claude", seq, None);
        request.state = None;
        request.insight = Some(InsightReport {
            source: InsightSourceKind::Hooks,
            ..report
        });
        request
    };
    let turn = |phase| {
        Some(ReportedTurn {
            phase,
            outcome: None,
            duration_ms: None,
        })
    };
    let reports = [
        InsightReport {
            turn: turn(TurnPhase::Start),
            ..InsightReport::default()
        },
        InsightReport {
            tools: vec![
                ReportedTool {
                    name: "Bash".into(),
                    calls: 2,
                    failed: 1,
                    ms: Some(5_200),
                },
                ReportedTool {
                    name: "Edit".into(),
                    calls: 1,
                    failed: 0,
                    ms: Some(30),
                },
                ReportedTool {
                    name: "Write".into(),
                    calls: 1,
                    failed: 0,
                    ms: Some(20),
                },
            ],
            files: vec![
                ReportedFile {
                    path: cwd.join("src/lib.rs").display().to_string(),
                    kind: FileChangeKind::Update,
                },
                ReportedFile {
                    path: cwd.join("src/new.rs").display().to_string(),
                    kind: FileChangeKind::Add,
                },
            ],
            turn: turn(TurnPhase::End),
            ..InsightReport::default()
        },
    ];
    for (seq, report) in reports.into_iter().enumerate() {
        assert!(self::report(&env.client, hooks(10 + seq as u64, report)).await);
    }

    let insight = env.insight(session.id).await;
    // Four tool calls and two files, not eight and four; one turn from the hooks and two from
    // the transcript are not three.
    assert_eq!(
        insight.tools.as_ref().map(|t| t.calls),
        Some(4),
        "{insight:?}"
    );
    assert_eq!(
        tool(&insight, "Bash"),
        (2, 1, Some(5_200)),
        "the hooks' time"
    );
    assert_eq!(insight.files.as_ref().map(|f| f.count), Some(2));
    assert_eq!(insight.totals.as_ref().map(|t| t.turns), Some(2));
    assert_eq!(insight.totals.as_ref().and_then(|t| t.input), Some(19));
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
        Some(InsightSourceKind::Transcript),
        "the hooks' turns have no tokens"
    );
    assert_eq!(
        source_of(&insight, InsightBlock::Context),
        Some(InsightSourceKind::Transcript)
    );
    env.stop().await;
}

async fn coverage_of(
    client: &Client,
    session: SessionId,
) -> hutan_protocol::checks::SessionCoverage {
    let answer: AgentsFindings = client
        .call(Request::AgentsFindings(AgentsFindingsRequest {
            session_id: Some(session),
            ..Default::default()
        }))
        .await
        .unwrap();
    answer
        .coverage
        .into_iter()
        .find(|c| c.session_id == session)
        .unwrap_or_else(|| panic!("no coverage of {session}"))
}

/// The detectors that read the insight run over a terminal agent once its transcript gives them
/// a context window and tokens per turn, and say what turns it on while it is not read: a
/// session with nothing but its state, and one whose hooks report tools.
#[tokio::test(flavor = "multi_thread")]
async fn the_findings_coverage_says_the_detectors_ran_over_a_transcript_and_what_would_make_them() {
    let env = start(false, true, true).await;
    let cwd = env.work("coverage");
    let path = env.claude_file(&cwd, CLAUDE_ID);
    write(&path, &claude_transcript(&cwd, CLAUDE_ID));
    let session = env.terminal(&cwd).await;
    claude_hook(&env.client, session.id, 1, Some(CLAUDE_ID), Some(&path)).await;

    // History is off: the agent reported its state and nothing else.
    let off = coverage_of(&env.client, session.id).await;
    let why = |c: &hutan_protocol::checks::SessionCoverage, detector: &str| {
        c.gaps
            .iter()
            .find(|g| g.detectors.iter().any(|d| d == detector))
            .map(|g| g.why.clone())
            .unwrap_or_default()
    };
    for id in [detector::CONTEXT_PRESSURE, detector::COSTLY_TURN] {
        assert!(!off.ran.iter().any(|r| r == id), "{id} cannot run yet");
        assert!(
            why(&off, id).contains("turn on Claude Code history"),
            "{id}: {}",
            why(&off, id)
        );
    }

    // The agent's hooks report a tool: the insight has something, and says what is missing.
    let mut hooks = base_report(session.id, "claude-hooks", "claude", 5, None);
    hooks.state = None;
    hooks.insight = Some(InsightReport {
        source: InsightSourceKind::Hooks,
        tools: vec![ReportedTool {
            name: "Bash".into(),
            calls: 1,
            failed: 0,
            ms: Some(10),
        }],
        ..InsightReport::default()
    });
    assert!(report(&env.client, hooks).await);
    let hooked = coverage_of(&env.client, session.id).await;
    assert!(
        why(&hooked, detector::CONTEXT_PRESSURE).contains("turn on Claude Code history"),
        "{}",
        why(&hooked, detector::CONTEXT_PRESSURE)
    );

    // The user turns it on: the transcript gives the window and the tokens of each turn.
    env.set_history(true).await;
    let on = timeout(WAIT, async {
        loop {
            let c = coverage_of(&env.client, session.id).await;
            if c.ran.iter().any(|r| r == detector::CONTEXT_PRESSURE) {
                return c;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the detectors ran once the transcript was read");
    assert!(on.ran.iter().any(|r| r == detector::COSTLY_TURN));
    assert!(
        !on.gaps
            .iter()
            .any(|g| g.why.contains("turn on Claude Code history")),
        "{:?}",
        on.gaps
    );
    // The turn detectors still need what a transcript read as counts cannot give.
    let turn_gap = why(&on, detector::TESTS_WEAKENED);
    assert!(turn_gap.contains("hooks and its transcript"), "{turn_gap}");
    env.stop().await;
}
