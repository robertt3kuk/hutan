//! The session history index end to end: a real daemon on a temporary `HUTAN_HOME` whose
//! `daemon.toml` points the sources at temporary directories (never the user's real agent
//! directories), real file events (FSEvents), real `history.*` requests over the socket.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use hutan_client::Client;
use hutan_daemon::{DaemonConfig, DaemonSettings, Paths};
use hutan_protocol::ErrorCode;
use hutan_protocol::history::{
    HistoryHit, HistoryHits, HistoryRole, HistorySearch, HistorySetEnabled, HistorySource,
    HistoryStatusInfo,
};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Empty, Request};
use rusqlite::Connection;
use tokio::time::sleep;

struct Env {
    _tmp: tempfile::TempDir,
    /// The temporary directory, canonical (`/private/var/…`, as FSEvents reports it).
    base: PathBuf,
    daemon: TestDaemon,
}

impl Env {
    fn claude(&self) -> PathBuf {
        self.base.join("sources/claude")
    }
    fn codex(&self) -> PathBuf {
        self.base.join("sources/codex")
    }
    fn omp(&self) -> PathBuf {
        self.base.join("sources/omp")
    }
    fn index_db(&self) -> PathBuf {
        self.daemon.paths.index_database.clone()
    }
}

/// Starts a daemon whose history sources live under a temporary directory; `enabled` goes into
/// `daemon.toml`.
async fn start(enabled: bool) -> Env {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    let sources = base.join("sources");
    for dir in ["claude", "codex", "omp"] {
        fs::create_dir_all(sources.join(dir)).unwrap();
    }
    fs::create_dir_all(&home).unwrap();
    fs::write(
        home.join("daemon.toml"),
        format!(
            "[history]\nenabled = {enabled}\n[history.roots]\nclaude = \"{c}\"\ncodex = \"{x}\"\nomp = \"{o}\"\n",
            c = sources.join("claude").display(),
            x = sources.join("codex").display(),
            o = sources.join("omp").display(),
        ),
    )
    .unwrap();
    let paths = Paths::in_home(&home);
    let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    Env {
        _tmp: tmp,
        base,
        daemon: TestDaemon {
            _dir: tempfile::tempdir().unwrap(),
            paths,
            daemon,
        },
    }
}

fn append(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

fn claude_fixture(env: &Env) -> (PathBuf, Vec<String>) {
    let path = env
        .claude()
        .join("-work-app/5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60.jsonl");
    let lines = vec![
        r#"{"type":"user","timestamp":"2026-10-15T09:00:00.000Z","cwd":"/work/app","sessionId":"5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60","message":{"role":"user","content":[{"type":"text","text":"why does the quokka scheduler stall under load"}]}}"#.to_owned(),
        r#"{"type":"assistant","timestamp":"2026-10-15T09:00:05.000Z","cwd":"/work/app","message":{"role":"assistant","content":[{"type":"text","text":"Checking the scheduler."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"grep -rn stall crates/scheduler/src"}}]}}"#.to_owned(),
        r#"{"type":"user","timestamp":"2026-10-15T09:00:06.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"crates/scheduler/src/queue.rs:42: // stall when the queue is full"}]}}"#.to_owned(),
        r#"{"type":"ai-title","aiTitle":"Scheduler stall investigation","sessionId":"5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60"}"#.to_owned(),
    ];
    (path, lines)
}

fn codex_fixture(env: &Env) -> (PathBuf, Vec<String>) {
    let path = env
        .codex()
        .join("2026/10/15/rollout-2026-10-15T10-00-00-0198aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl");
    let lines = vec![
        r#"{"timestamp":"2026-10-15T10:00:00.000Z","type":"session_meta","payload":{"id":"0198aaaa-bbbb-cccc-dddd-eeeeffff0000","cwd":"/work/codex-app"}}"#.to_owned(),
        r#"{"timestamp":"2026-10-15T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add retry with jitter to the capybara client"}]}}"#.to_owned(),
        r#"{"timestamp":"2026-10-15T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test -p capybara\"]}","call_id":"c1"}}"#.to_owned(),
        r#"{"timestamp":"2026-10-15T10:00:03.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"test result: ok. 4 passed"}}"#.to_owned(),
    ];
    (path, lines)
}

fn omp_fixture(env: &Env) -> (PathBuf, Vec<String>) {
    let path = env
        .omp()
        .join("-work-omp-app/2026-10-15T11-00-00-000Z_0f4c7d52.jsonl");
    let lines = vec![
        r#"{"type":"title","v":1,"title":"Axolotl cache design","updatedAt":"2026-10-15T11:00:30.000Z","pad":"      "}"#.to_owned(),
        r#"{"type":"session","version":3,"id":"0f4c7d52","timestamp":"2026-10-15T11:00:00.000Z","cwd":"/work/omp-app"}"#.to_owned(),
        r#"{"type":"message","timestamp":"2026-10-15T11:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"sketch the axolotl eviction policy"}]}}"#.to_owned(),
        r#"{"type":"message","timestamp":"2026-10-15T11:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Use LRU with a size cap."}]}}"#.to_owned(),
    ];
    (path, lines)
}

fn search_req(query: &str) -> HistorySearch {
    HistorySearch {
        query: query.to_owned(),
        limit: 20,
        sources: Vec::new(),
        roles: Vec::new(),
        project: None,
        since_ms: None,
        until_ms: None,
    }
}

async fn search(client: &Client, query: &str) -> Vec<HistoryHit> {
    client
        .call::<HistoryHits>(Request::HistorySearch(search_req(query)))
        .await
        .unwrap()
        .hits
}

/// Polls until `query` has hits.
async fn wait_hits(client: &Client, query: &str) -> Vec<HistoryHit> {
    let deadline = Instant::now() + WAIT;
    loop {
        let hits = search(client, query).await;
        if !hits.is_empty() {
            return hits;
        }
        assert!(Instant::now() < deadline, "no hits for `{query}` in time");
        sleep(Duration::from_millis(50)).await;
    }
}

async fn status(client: &Client) -> HistoryStatusInfo {
    client.call(Request::HistoryStatus(Empty {})).await.unwrap()
}

fn sources_in_db(path: &Path) -> i64 {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    conn.query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn disabled_by_default_means_no_file_is_read_and_no_index_exists() {
    let env = start(false).await;
    let client = env.daemon.connect().await;
    assert!(client.supports_history());

    // Transcripts exist before and appear after the daemon started.
    let (claude, lines) = claude_fixture(&env);
    append(&claude, &lines);
    sleep(Duration::from_millis(800)).await;
    let (codex, lines) = codex_fixture(&env);
    append(&codex, &lines);
    sleep(Duration::from_millis(1200)).await;

    let info = status(&client).await;
    assert!(!info.enabled);
    assert!(info.sources.is_empty());
    assert_eq!(info.db_bytes, 0);
    assert!(
        !env.index_db().exists(),
        "no database is created while disabled"
    );
    let refused = client
        .call::<HistoryHits>(Request::HistorySearch(search_req("quokka")))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);

    // Enabling reads what is there; the choice lands in daemon.toml.
    let info: HistoryStatusInfo = client
        .call(Request::HistorySetEnabled(HistorySetEnabled {
            enabled: true,
        }))
        .await
        .unwrap();
    assert!(info.enabled);
    wait_hits(&client, "quokka").await;
    wait_hits(&client, "capybara").await;
    assert_eq!(sources_in_db(&env.index_db()), 2);
    let settings = DaemonSettings::load(&env.daemon.paths.config_file, None).unwrap();
    assert!(settings.history.enabled);
    assert_eq!(
        settings.history.roots.claude.as_deref(),
        Some(env.claude().as_path())
    );

    // Disabling stops the reading: a new transcript never reaches the sources table.
    let info: HistoryStatusInfo = client
        .call(Request::HistorySetEnabled(HistorySetEnabled {
            enabled: false,
        }))
        .await
        .unwrap();
    assert!(!info.enabled && info.sources.is_empty());
    assert!(info.db_bytes > 0, "the index stays on disk");
    let (omp, lines) = omp_fixture(&env);
    append(&omp, &lines);
    append(
        &claude,
        &[
            r#"{"type":"user","message":{"role":"user","content":"appended after disabling"}}"#
                .to_owned(),
        ],
    );
    sleep(Duration::from_millis(1500)).await;
    assert_eq!(sources_in_db(&env.index_db()), 2);
    assert!(
        !DaemonSettings::load(&env.daemon.paths.config_file, None)
            .unwrap()
            .history
            .enabled
    );
    let refused = client
        .call::<HistoryHits>(Request::HistorySearch(search_req("axolotl")))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);

    // Clear works while disabled: the file goes.
    client
        .call::<Empty>(Request::HistoryClear(Empty {}))
        .await
        .unwrap();
    assert!(!env.index_db().exists());
    env.daemon.stop().await;
}

#[tokio::test]
async fn enabled_daemon_indexes_new_transcripts_from_file_events_and_searches_them() {
    let env = start(true).await;
    let client = env.daemon.connect().await;
    let info = status(&client).await;
    assert!(info.enabled);
    let names: Vec<_> = info.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["hutan", "claude", "codex", "omp"]);

    // Fixtures written after the daemon started: found through file events.
    let (claude, claude_lines) = claude_fixture(&env);
    let (codex, codex_lines) = codex_fixture(&env);
    let (omp, omp_lines) = omp_fixture(&env);
    append(&claude, &claude_lines);
    append(&codex, &codex_lines);
    append(&omp, &omp_lines);

    let hits = wait_hits(&client, "quokka scheduler").await;
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.source, HistorySource::Claude);
    assert_eq!(hit.session_key, "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60");
    assert_eq!(hit.role, HistoryRole::User);
    assert_eq!(hit.cwd.as_deref(), Some("/work/app"));
    assert_eq!(hit.title.as_deref(), Some("Scheduler stall investigation"));
    // Where the transcript is, so that a client can reveal it; no block.
    assert_eq!(hit.path.as_deref(), Some(claude.to_str().unwrap()));
    assert_eq!((hit.terminal_id, hit.block_id), (None, None));
    assert!(hit.snippet.contains("«quokka»"), "{}", hit.snippet);
    assert_eq!(
        hit.resume_argv.as_deref(),
        Some(
            &[
                "claude".to_owned(),
                "--resume".to_owned(),
                "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60".to_owned()
            ][..]
        )
    );

    let codex_hit = &wait_hits(&client, "capybara client").await[0];
    assert_eq!(codex_hit.source, HistorySource::Codex);
    assert_eq!(
        codex_hit.session_key,
        "0198aaaa-bbbb-cccc-dddd-eeeeffff0000"
    );
    assert_eq!(codex_hit.cwd.as_deref(), Some("/work/codex-app"));
    assert_eq!(codex_hit.path.as_deref(), Some(codex.to_str().unwrap()));
    assert_eq!(
        codex_hit.resume_argv.as_deref(),
        Some(
            &[
                "codex".to_owned(),
                "resume".to_owned(),
                "0198aaaa-bbbb-cccc-dddd-eeeeffff0000".to_owned()
            ][..]
        )
    );

    let omp_hit = &wait_hits(&client, "axolotl eviction").await[0];
    assert_eq!(omp_hit.source, HistorySource::Omp);
    assert_eq!(omp_hit.session_key, "0f4c7d52");
    assert_eq!(omp_hit.title.as_deref(), Some("Axolotl cache design"));
    assert_eq!(omp_hit.path.as_deref(), Some(omp.to_str().unwrap()));
    assert_eq!(
        omp_hit.resume_argv.as_deref(),
        Some(&["omp".to_owned(), "--resume=0f4c7d52".to_owned()][..])
    );

    // A tool row is found too, with its role; filters narrow.
    let tool = wait_hits(&client, "crates/scheduler/src/queue.rs").await;
    assert_eq!(tool[0].role, HistoryRole::Tool);
    let mut request = search_req("capybara");
    request.sources = vec![HistorySource::Claude];
    let none = client
        .call::<HistoryHits>(Request::HistorySearch(request))
        .await
        .unwrap();
    assert!(none.hits.is_empty());
    let mut request = search_req("capybara");
    request.project = Some("/work/codex-app".to_owned());
    request.roles = vec![HistoryRole::User];
    let some = client
        .call::<HistoryHits>(Request::HistorySearch(request))
        .await
        .unwrap();
    assert_eq!(some.hits.len(), 1);

    // Secrets in a transcript are not searchable.
    append(
        &claude,
        &[r#"{"type":"user","timestamp":"2026-10-15T09:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"use sk-proj-abcdefghijklmnop1234567890 and say walrus"}]}}"#.to_owned()],
    );
    wait_hits(&client, "walrus").await;
    assert!(
        search(&client, "abcdefghijklmnop1234567890")
            .await
            .is_empty()
    );

    // Status counts what was read.
    let info = status(&client).await;
    let claude_status = info.sources.iter().find(|s| s.name == "claude").unwrap();
    assert_eq!((claude_status.files, claude_status.errors), (1, 0));
    assert_eq!(claude_status.rows, 5);
    assert!(claude_status.last_indexed_ms.is_some());
    assert!(info.db_bytes > 0);

    // Clear empties the index; rebuild reads the transcripts again.
    client
        .call::<Empty>(Request::HistoryClear(Empty {}))
        .await
        .unwrap();
    assert!(search(&client, "quokka").await.is_empty());
    let info = status(&client).await;
    assert!(info.enabled);
    assert!(
        info.sources.iter().all(|s| s.rows == 0 && s.files == 0),
        "{info:?}"
    );
    client
        .call::<Empty>(Request::HistoryRebuild(Empty {}))
        .await
        .unwrap();
    wait_hits(&client, "quokka").await;
    wait_hits(&client, "axolotl").await;
    env.daemon.stop().await;
}

#[tokio::test]
async fn hutan_blocks_and_agent_reports_are_indexed() {
    let env = start(true).await;
    let client = env.daemon.connect().await;
    let session = create(
        &client,
        sh("printf '\\033]133;A\\007$ \\033]133;B\\007cargo test -p daemon\\r\\n\\033]133;C\\007flibbertigibbet test output\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat"),
    )
    .await;

    let hits = wait_hits(&client, "cargo test -p daemon").await;
    assert_eq!(hits[0].source, HistorySource::Hutan);
    assert_eq!(hits[0].role, HistoryRole::Command);
    assert_eq!(hits[0].session_key, session.id.to_string());
    assert_eq!(hits[0].resume_argv, None);
    assert_eq!(hits[0].path, None);
    // The hit names the block it came from, in the terminal that ran it.
    let terminal = terminal_of(&session);
    let blocks = client
        .terminal_blocks(terminal, None, 500)
        .await
        .unwrap()
        .blocks;
    let block = blocks
        .iter()
        .find(|b| b.command.as_deref() == Some("cargo test -p daemon"))
        .expect("the block exists");
    assert_eq!(hits[0].terminal_id, Some(terminal));
    assert_eq!(hits[0].block_id, Some(block.id));
    let output = wait_hits(&client, "flibbertigibbet").await;
    assert_eq!(output[0].role, HistoryRole::Output);
    assert_eq!(output[0].session_key, session.id.to_string());
    assert_eq!(
        (output[0].terminal_id, output[0].block_id),
        (Some(terminal), Some(block.id)),
        "the output row belongs to the same block"
    );

    let applied: AgentApplied = client
        .call(Request::AgentReport(AgentReportRequest {
            pid: None,
            session_id: session.id,
            source: "test".to_owned(),
            agent: "claude".to_owned(),
            state: Some(AgentState::Working),
            message: Some("untangling the pangolin refactor".to_owned()),
            seq: 1,
            agent_session_id: None,
            resume_argv: None,
            node: None,
        }))
        .await
        .unwrap();
    assert!(applied.applied);
    let report = wait_hits(&client, "pangolin").await;
    assert_eq!(report[0].role, HistoryRole::Tool);
    assert!(
        report[0].snippet.contains("claude working"),
        "{}",
        report[0].snippet
    );
    assert_eq!(
        (report[0].terminal_id, report[0].block_id),
        (None, None),
        "an agent report is no block"
    );

    stop(&client, session.id).await.unwrap();
    env.daemon.stop().await;
}

#[tokio::test]
async fn bad_search_requests_are_rejected() {
    let env = start(true).await;
    let client = env.daemon.connect().await;
    let attempt = |f: &dyn Fn(&mut HistorySearch)| {
        let mut request = search_req("hello");
        f(&mut request);
        request
    };
    for request in [
        attempt(&|r| r.query = "   ".to_owned()),
        attempt(&|r| r.query = "!! ??".to_owned()),
        attempt(&|r| r.limit = 0),
        attempt(&|r| r.limit = 101),
        attempt(&|r| {
            r.since_ms = Some(10);
            r.until_ms = Some(5);
        }),
    ] {
        let result = client
            .call::<HistoryHits>(Request::HistorySearch(request.clone()))
            .await;
        assert_eq!(remote_code(result), ErrorCode::BadRequest, "{request:?}");
    }
    env.daemon.stop().await;
}
