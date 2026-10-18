//! The session catalog (`IndexHandle::catalog`): per source fixtures (titles, first prompt,
//! latest turns, counts, model, branch), appends that update a summary without rereading the
//! file, rewritten files, sub-agents, scan progress, refresh, Hutan sessions, and the upgrade
//! from the previous schema.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hutan_index::{
    CatalogRow, FIRST_PROMPT_MAX_CHARS, HutanRecord, IndexConfig, IndexHandle, Indexer,
    LATEST_TURNS_MAX, Role, Source, TURN_TEXT_MAX_CHARS,
};

const WAIT: Duration = Duration::from_secs(15);

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < WAIT {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    codex: PathBuf,
    omp: PathBuf,
    db: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude/projects");
    let codex = tmp.path().join("codex/sessions");
    let omp = tmp.path().join("omp/sessions");
    for dir in [&claude, &codex, &omp] {
        fs::create_dir_all(dir).unwrap();
    }
    Env {
        db: tmp.path().join("home/index.sqlite"),
        _tmp: tmp,
        claude,
        codex,
        omp,
    }
}

fn config(env: &Env, hutan: bool) -> IndexConfig {
    IndexConfig {
        db_path: env.db.clone(),
        range_days: 0,
        roots: vec![
            (Source::Claude, env.claude.clone()),
            (Source::Codex, env.codex.clone()),
            (Source::Omp, env.omp.clone()),
        ],
        hutan,
    }
}

fn append(path: &Path, line: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

fn row(handle: &IndexHandle, source: Source, key: &str) -> Option<CatalogRow> {
    handle
        .catalog()
        .unwrap()
        .into_iter()
        .find(|row| row.source == source && row.key == key)
}

fn wait_for_row(
    handle: &IndexHandle,
    source: Source,
    key: &str,
    mut ready: impl FnMut(&CatalogRow) -> bool,
) -> CatalogRow {
    let mut found = None;
    wait_for(&format!("the {source} session {key}"), || {
        found = row(handle, source, key).filter(&mut ready);
        found.is_some()
    });
    found.unwrap()
}

// ---- Claude Code -------------------------------------------------------------------------------

const CLAUDE_ID: &str = "5f3e1c1a-8d51-4a7e-9c0d-1b2c3d4e5f60";

fn claude_file(env: &Env) -> PathBuf {
    env.claude.join(format!("-work-app/{CLAUDE_ID}.jsonl"))
}

fn claude_user(ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{ts}","cwd":"/work/app","gitBranch":"feature/x","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn claude_assistant(ts: &str, text: &str, model: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","cwd":"/work/app","message":{{"role":"assistant","model":"{model}","content":[{{"type":"text","text":"{text}"}},{{"type":"tool_use","id":"t","name":"Bash","input":{{"command":"ls"}}}}]}}}}"#
    )
}

#[test]
fn a_claude_session_has_title_prompt_turns_counts_model_and_branch() {
    let env = env();
    let file = claude_file(&env);
    append(
        &file,
        &claude_user(
            "2026-09-28T05:00:00.000Z",
            "fix the flaky test\\nwith details",
        ),
    );
    // Machinery the harness injects as user turns is neither counted nor shown.
    append(
        &file,
        r#"{"type":"user","timestamp":"2026-09-28T05:00:01.000Z","message":{"role":"user","content":"<local-command-stdout>ok</local-command-stdout>"}}"#,
    );
    append(
        &file,
        &claude_assistant(
            "2026-09-28T05:00:10.000Z",
            "Looking at it.",
            "claude-sonnet-5",
        ),
    );
    // A tool result is a user line, but not a message.
    append(
        &file,
        r#"{"type":"user","timestamp":"2026-09-28T05:00:11.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"3 passed"}]}}"#,
    );
    append(
        &file,
        &claude_user(
            "2026-09-28T05:01:00.000Z",
            "use sk-abcdefghijklmnopqrstuvwxyz0123 for it",
        ),
    );
    append(
        &file,
        &claude_assistant("2026-09-28T05:02:00.000Z", "Done.", "claude-opus-5"),
    );
    append(
        &file,
        r#"{"type":"ai-title","aiTitle":"Fix flaky test","sessionId":"s"}"#,
    );

    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    let found = wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| {
        row.title.is_some()
    });
    assert_eq!(found.title.as_deref(), Some("Fix flaky test"));
    assert_eq!(found.cwd.as_deref(), Some("/work/app"));
    assert_eq!(found.branch.as_deref(), Some("feature/x"));
    assert_eq!(found.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(found.messages, 4);
    assert_eq!(found.created_ms, Some(1_790_571_600_000));
    assert_eq!(found.updated_ms, Some(1_790_571_720_000));
    assert_eq!(
        found.first_prompt.as_deref(),
        Some("fix the flaky test\nwith details")
    );
    assert_eq!(
        found.path.as_deref(),
        Some(file.canonicalize().unwrap().to_str().unwrap())
    );
    let turns: Vec<_> = found
        .latest
        .iter()
        .map(|t| (t.role, t.text.as_str()))
        .collect();
    assert_eq!(
        turns,
        [
            (Role::Assistant, "Looking at it."),
            (Role::User, "use [REDACTED] for it"),
            (Role::Assistant, "Done."),
        ],
        "oldest first, secrets redacted, tool results and injected turns left out"
    );
}

#[test]
fn a_custom_title_beats_the_generated_one_and_the_first_prompt_is_the_fallback() {
    let env = env();
    let file = claude_file(&env);
    append(
        &file,
        &claude_user("2026-09-28T05:00:00.000Z", "plan the migration"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    let found = wait_for_row(&handle, Source::Claude, CLAUDE_ID, |_| true);
    assert_eq!(found.title.as_deref(), Some("plan the migration"));

    append(&file, r#"{"type":"ai-title","aiTitle":"Generated"}"#);
    wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| {
        row.title.as_deref() == Some("Generated")
    });
    append(&file, r#"{"type":"custom-title","customTitle":"Mine"}"#);
    append(&file, r#"{"type":"ai-title","aiTitle":"Generated again"}"#);
    wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| {
        row.title.as_deref() == Some("Mine")
    });
}

#[test]
fn an_append_updates_the_summary_without_rereading_the_file() {
    let env = env();
    let file = claude_file(&env);
    append(
        &file,
        &claude_user("2026-09-28T05:00:00.000Z", "first question"),
    );
    append(
        &file,
        &claude_assistant("2026-09-28T05:00:10.000Z", "first answer", "m1"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| row.messages == 2);

    let before = handle.metrics().bytes_read();
    let line = claude_user("2026-09-28T05:05:00.000Z", "second question");
    append(&file, &line);
    let found = wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| row.messages == 3);
    assert_eq!(
        handle.metrics().bytes_read() - before,
        line.len() as u64 + 1
    );
    assert_eq!(handle.metrics().reindexed(), 0);
    assert_eq!(found.first_prompt.as_deref(), Some("first question"));
    assert_eq!(found.updated_ms, Some(1_790_571_900_000));
    assert_eq!(found.latest.last().unwrap().text, "second question");
}

#[test]
fn a_rewritten_file_is_folded_again_from_its_start() {
    let env = env();
    let file = claude_file(&env);
    append(
        &file,
        &claude_user("2026-09-28T05:00:00.000Z", "old question"),
    );
    append(
        &file,
        &claude_assistant("2026-09-28T05:00:10.000Z", "old answer", "m1"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| row.messages == 2);

    fs::write(
        &file,
        format!(
            "{}\n",
            claude_user("2026-09-28T06:00:00.000Z", "brand new start")
        ),
    )
    .unwrap();
    let found = wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| {
        row.first_prompt.as_deref() == Some("brand new start")
    });
    assert_eq!((found.messages, found.latest.len()), (1, 1));
    assert_eq!(found.created_ms, Some(1_790_575_200_000));
}

#[test]
fn long_texts_are_cut_and_only_three_turns_stay() {
    let env = env();
    let file = claude_file(&env);
    append(
        &file,
        &claude_user("2026-09-28T05:00:00.000Z", &"long ".repeat(1000)),
    );
    for n in 0..5 {
        append(
            &file,
            &claude_assistant(
                &format!("2026-09-28T05:00:0{n}.000Z"),
                &format!("answer {n}"),
                "m",
            ),
        );
    }
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    let found = wait_for_row(&handle, Source::Claude, CLAUDE_ID, |row| row.messages == 6);
    assert_eq!(
        found.first_prompt.as_ref().map(|p| p.chars().count()),
        Some(FIRST_PROMPT_MAX_CHARS)
    );
    assert!(found.first_prompt.as_ref().unwrap().ends_with('…'));
    assert_eq!(found.latest.len(), LATEST_TURNS_MAX);
    assert!(
        found
            .latest
            .iter()
            .all(|t| t.text.chars().count() <= TURN_TEXT_MAX_CHARS)
    );
    assert_eq!(found.latest[2].text, "answer 4");
}

#[test]
fn sub_agent_transcripts_are_not_sessions_of_their_own() {
    let env = env();
    append(
        &claude_file(&env),
        &claude_user("2026-09-28T05:00:00.000Z", "parent work"),
    );
    append(
        &env.claude
            .join(format!("-work-app/{CLAUDE_ID}/subagents/agent-a1.jsonl")),
        &claude_user("2026-09-28T05:00:05.000Z", "helper work"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("both files", || {
        handle.scan_info().files == 2 && !handle.scan_info().scanning
    });
    let rows = handle.catalog().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].first_prompt.as_deref(), Some("parent work"));
}

#[test]
fn a_session_without_messages_or_a_title_is_not_listed() {
    let env = env();
    append(
        &claude_file(&env),
        r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-28T05:00:00.000Z"}"#,
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("the scan", || {
        let scan = handle.scan_info();
        scan.files == 1 && !scan.scanning
    });
    assert!(handle.catalog().unwrap().is_empty());
}

// ---- Codex -------------------------------------------------------------------------------------

const CODEX_ID: &str = "0198aaaa-bbbb-cccc-dddd-eeeeffff0000";

fn codex_file(env: &Env) -> PathBuf {
    env.codex.join(format!(
        "2026/09/28/rollout-2026-09-28T05-00-00-{CODEX_ID}.jsonl"
    ))
}

fn codex_message(ts: &str, role: &str, kind: &str, text: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"{kind}","text":"{text}"}}]}}}}"#
    )
}

#[test]
fn a_codex_session_takes_its_title_from_session_index_jsonl() {
    let env = env();
    let file = codex_file(&env);
    append(
        &file,
        &format!(
            r#"{{"timestamp":"2026-09-28T05:00:00.000Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/work/svc","git":{{"branch":"main","commit_hash":"abc"}},"source":"cli"}}}}"#
        ),
    );
    append(
        &file,
        r#"{"timestamp":"2026-09-28T05:00:01.000Z","type":"turn_context","payload":{"cwd":"/work/svc","model":"gpt-5.6-luna"}}"#,
    );
    // Injected context is not a prompt.
    append(
        &file,
        &codex_message(
            "2026-09-28T05:00:02.000Z",
            "user",
            "input_text",
            "<environment_context><cwd>/work/svc</cwd></environment_context>",
        ),
    );
    append(
        &file,
        &codex_message(
            "2026-09-28T05:00:03.000Z",
            "user",
            "input_text",
            "add a retry to the client",
        ),
    );
    append(
        &file,
        &codex_message(
            "2026-09-28T05:00:09.000Z",
            "assistant",
            "output_text",
            "Added it.",
        ),
    );

    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    let found = wait_for_row(&handle, Source::Codex, CODEX_ID, |row| row.messages == 2);
    assert_eq!(found.title.as_deref(), Some("add a retry to the client"));
    assert_eq!(found.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(found.branch.as_deref(), Some("main"));
    assert_eq!(found.cwd.as_deref(), Some("/work/svc"));
    assert_eq!(
        found.first_prompt.as_deref(),
        Some("add a retry to the client")
    );
    assert_eq!(found.created_ms, Some(1_790_571_600_000));

    // Codex names the thread later, in the file next to `sessions/`.
    let index = env.codex.parent().unwrap().join("session_index.jsonl");
    append(
        &index,
        &format!(r#"{{"id":"{CODEX_ID}","thread_name":"Retry for the client"}}"#),
    );
    assert!(handle.refresh(Duration::from_secs(10)).unwrap());
    let found = row(&handle, Source::Codex, CODEX_ID).unwrap();
    assert_eq!(found.title.as_deref(), Some("Retry for the client"));
    append(
        &index,
        &format!(r#"{{"id":"{CODEX_ID}","thread_name":"Renamed"}}"#),
    );
    assert!(handle.refresh(Duration::from_secs(10)).unwrap());
    assert_eq!(
        row(&handle, Source::Codex, CODEX_ID)
            .unwrap()
            .title
            .as_deref(),
        Some("Renamed")
    );
}

#[test]
fn a_codex_worker_thread_is_not_a_session_of_its_own() {
    let env = env();
    append(
        &codex_file(&env),
        &format!(
            r#"{{"timestamp":"2026-09-28T05:00:00.000Z","type":"session_meta","payload":{{"id":"{CODEX_ID}","cwd":"/w","source":{{"subagent":"review"}}}}}}"#
        ),
    );
    append(
        &codex_file(&env),
        &codex_message(
            "2026-09-28T05:00:03.000Z",
            "user",
            "input_text",
            "review this",
        ),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("the scan", || {
        let scan = handle.scan_info();
        scan.files == 1 && !scan.scanning
    });
    assert!(handle.catalog().unwrap().is_empty());
}

// ---- OMP ---------------------------------------------------------------------------------------

fn omp_file(env: &Env) -> PathBuf {
    env.omp
        .join("-work-meroving/2026-09-28T05-01-22-983Z_0f4c7d52.jsonl")
}

#[test]
fn an_omp_session_follows_its_title_record_even_when_it_is_rewritten_in_place() {
    let env = env();
    let file = omp_file(&env);
    let head = |title: &str| {
        format!(
            r#"{{"type":"title","v":1,"title":"{title}","pad":"{}"}}"#,
            " ".repeat(40 - title.len())
        )
    };
    append(&file, &head("Refactor storage"));
    append(
        &file,
        r#"{"type":"session","version":3,"id":"0f4c7d52","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/meroving"}"#,
    );
    append(&file, r#"{"type":"model_change","model":"gpt-5.6"}"#);
    append(
        &file,
        r#"{"type":"message","timestamp":"2026-09-28T05:01:23.000Z","message":{"role":"user","content":[{"type":"text","text":"list the tests"}]}}"#,
    );
    append(
        &file,
        r#"{"type":"message","timestamp":"2026-09-28T05:01:30.000Z","message":{"role":"assistant","model":"gpt-5.6-luna","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Here they are."}]}}"#,
    );
    append(
        &file,
        r#"{"type":"message","timestamp":"2026-09-28T05:01:31.000Z","message":{"role":"toolResult","toolName":"bash","content":[{"type":"text","text":"3 passed"}]}}"#,
    );
    // A sub-agent of that session.
    append(
        &env.omp
            .join("-work-meroving/2026-09-28T05-01-22-983Z_0f4c7d52/Explore.jsonl"),
        r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"helper"}]}}"#,
    );

    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    let found = wait_for_row(&handle, Source::Omp, "0f4c7d52", |row| row.messages == 2);
    assert_eq!(found.title.as_deref(), Some("Refactor storage"));
    assert_eq!(found.cwd.as_deref(), Some("/work/meroving"));
    assert_eq!(
        found.model.as_deref(),
        Some("gpt-5.6-luna"),
        "the last turn that names one"
    );
    assert_eq!(found.first_prompt.as_deref(), Some("list the tests"));
    assert_eq!(
        handle.catalog().unwrap().len(),
        1,
        "the sub-agent is not listed"
    );

    // The title line is fixed width and rewritten in place; the file does not grow.
    let text = fs::read_to_string(&file).unwrap();
    let rewritten = text.replacen(&head("Refactor storage"), &head("Better title now"), 1);
    assert_eq!(text.len(), rewritten.len());
    fs::write(&file, rewritten).unwrap();
    wait_for_row(&handle, Source::Omp, "0f4c7d52", |row| {
        row.title.as_deref() == Some("Better title now")
    });
    assert_eq!(
        row(&handle, Source::Omp, "0f4c7d52").unwrap().messages,
        2,
        "reading the head again does not count its messages twice"
    );
}

// ---- scan state and refresh --------------------------------------------------------------------

#[test]
fn the_scan_counts_files_per_source_and_refresh_reads_what_changed() {
    let env = env();
    append(
        &claude_file(&env),
        &claude_user("2026-09-28T05:00:00.000Z", "one"),
    );
    append(
        &codex_file(&env),
        &codex_message("2026-09-28T05:00:03.000Z", "user", "input_text", "two"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("the first scan", || {
        let scan = handle.scan_info();
        !scan.scanning && scan.files == 2
    });
    let scan = handle.scan_info();
    assert_eq!((scan.files, scan.done), (2, 2));
    assert!(scan.finished_ms.is_some());
    assert_eq!(
        scan.per_source,
        vec![(Source::Claude, 1), (Source::Codex, 1), (Source::Omp, 0)]
    );

    let finished = scan.finished_ms;
    std::thread::sleep(Duration::from_millis(5));
    append(
        &omp_file(&env),
        r#"{"type":"session","id":"0f4c7d52","cwd":"/w"}"#,
    );
    append(
        &omp_file(&env),
        r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"three"}]}}"#,
    );
    assert!(handle.refresh(Duration::from_secs(10)).unwrap());
    let scan = handle.scan_info();
    assert_eq!((scan.files, scan.done, scan.scanning), (3, 3, false));
    assert!(scan.finished_ms > finished);
    assert!(row(&handle, Source::Omp, "0f4c7d52").is_some());
}

#[test]
fn nothing_is_read_for_a_source_that_is_not_configured() {
    let env = env();
    append(
        &claude_file(&env),
        &claude_user("2026-09-28T05:00:00.000Z", "one"),
    );
    let mut only_codex = config(&env, false);
    only_codex.roots = vec![(Source::Codex, env.codex.clone())];
    let indexer = Indexer::start(only_codex).unwrap();
    let handle = indexer.handle();
    wait_for("the scan", || !handle.scan_info().scanning);
    assert!(handle.catalog().unwrap().is_empty());
    assert_eq!(handle.metrics().files_opened(), 0);
}

// ---- Hutan sessions ----------------------------------------------------------------------------

fn record(key: &str, role: Role, ts_ms: u64, text: &str, ext: &str) -> HutanRecord {
    HutanRecord {
        session_key: key.to_owned(),
        title: Some(format!("title of {key}")),
        cwd: Some("/work/app".to_owned()),
        role,
        ts_ms,
        text: text.to_owned(),
        ext_key: ext.to_owned(),
        block: None,
    }
}

#[test]
fn hutan_chats_and_terminals_become_rows() {
    let env = env();
    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    for rec in [
        record(
            "CHAT",
            Role::User,
            1_790_000_000_000,
            "why is the build slow",
            "c:1",
        ),
        record(
            "CHAT",
            Role::Tool,
            1_790_000_001_000,
            "read_file: src/lib.rs",
            "c:2",
        ),
        record(
            "CHAT",
            Role::Assistant,
            1_790_000_002_000,
            "Because of the proc macro.",
            "c:3",
        ),
        // Pushed twice: one message.
        record(
            "CHAT",
            Role::Assistant,
            1_790_000_002_000,
            "Because of the proc macro.",
            "c:3",
        ),
        record(
            "TERM",
            Role::Command,
            1_790_000_010_000,
            "cargo build",
            "b:1:1",
        ),
        record(
            "TERM",
            Role::Output,
            1_790_000_011_000,
            "Compiling x",
            "b:1:1:out",
        ),
        record(
            "TERM",
            Role::Command,
            1_790_000_020_000,
            "cargo test",
            "b:2:2",
        ),
        // An agent report only: nothing to list.
        record(
            "REPORT",
            Role::Tool,
            1_790_000_030_000,
            "claude working",
            "a:1",
        ),
    ] {
        assert!(handle.push_hutan(rec));
    }
    handle.flush().unwrap();

    let rows = handle.catalog().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let chat = row(&handle, Source::Hutan, "CHAT").unwrap();
    assert_eq!(chat.title.as_deref(), Some("title of CHAT"));
    assert_eq!(chat.messages, 2);
    assert_eq!(chat.first_prompt.as_deref(), Some("why is the build slow"));
    assert_eq!(chat.updated_ms, Some(1_790_000_002_000));
    assert_eq!(chat.path, None);
    let roles: Vec<_> = chat.latest.iter().map(|t| t.role).collect();
    assert_eq!(roles, [Role::User, Role::Assistant]);

    let term = row(&handle, Source::Hutan, "TERM").unwrap();
    assert_eq!(term.messages, 2, "a terminal's messages are its commands");
    assert_eq!(term.first_prompt, None);
    assert_eq!(
        term.latest
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>(),
        ["cargo build", "cargo test"]
    );
    assert_eq!(term.created_ms, Some(1_790_000_010_000));
    assert_eq!(term.updated_ms, Some(1_790_000_020_000));
}

#[test]
fn the_summary_of_a_hutan_session_survives_a_restart() {
    let env = env();
    {
        let indexer = Indexer::start(config(&env, true)).unwrap();
        let handle = indexer.handle();
        handle.push_hutan(record(
            "CHAT",
            Role::User,
            1_790_000_000_000,
            "first",
            "c:1",
        ));
        handle.flush().unwrap();
    }
    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    handle.push_hutan(record(
        "CHAT",
        Role::Assistant,
        1_790_000_001_000,
        "second",
        "c:2",
    ));
    handle.flush().unwrap();
    let chat = row(&handle, Source::Hutan, "CHAT").unwrap();
    assert_eq!(chat.messages, 2);
    assert_eq!(chat.first_prompt.as_deref(), Some("first"));
}

#[test]
fn sources_that_are_off_have_no_rows() {
    let env = env();
    append(
        &claude_file(&env),
        &claude_user("2026-09-28T05:00:00.000Z", "one"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for_row(&handle, Source::Claude, CLAUDE_ID, |_| true);
    // A Hutan record is dropped while Hutan indexing is off.
    handle.push_hutan(record("CHAT", Role::User, 1_790_000_000_000, "hi", "c:1"));
    handle.flush().unwrap();
    assert!(row(&handle, Source::Hutan, "CHAT").is_none());
}

// ---- the upgrade from the previous schema ------------------------------------------------------

#[test]
fn an_index_of_schema_2_is_upgraded_keeping_hutan_sessions_in_the_catalog() {
    let env = env();
    fs::create_dir_all(env.db.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&env.db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sources (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, source TEXT NOT NULL,
                 size INTEGER NOT NULL, mtime_ns INTEGER NOT NULL, offset INTEGER NOT NULL,
                 tail_hash INTEGER NOT NULL, format_version INTEGER NOT NULL, state TEXT NOT NULL,
                 rows INTEGER NOT NULL DEFAULT 0, errors INTEGER NOT NULL DEFAULT 0,
                 last_indexed_ms INTEGER NOT NULL);
             CREATE TABLE sessions (id INTEGER PRIMARY KEY, source TEXT NOT NULL, key TEXT NOT NULL,
                 source_id INTEGER UNIQUE REFERENCES sources(id) ON DELETE CASCADE, title TEXT, cwd TEXT);
             CREATE UNIQUE INDEX sessions_hutan_key ON sessions(source, key) WHERE source_id IS NULL;
             CREATE TABLE entries (id INTEGER PRIMARY KEY,
                 session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 role TEXT NOT NULL, ts_ms INTEGER, ext_key TEXT, terminal_id INTEGER, block_id INTEGER);
             CREATE INDEX entries_session ON entries(session_id);
             CREATE INDEX entries_ts ON entries(ts_ms);
             CREATE UNIQUE INDEX entries_ext ON entries(session_id, ext_key) WHERE ext_key IS NOT NULL;
             CREATE VIRTUAL TABLE fts USING fts5(body, idents, tokenize = 'unicode61 remove_diacritics 2');
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO sessions VALUES (1, 'hutan', 'OLD', NULL, 'old shell', '/work/app');
             INSERT INTO entries VALUES (1, 1, 'command', 1790000000000, 'b:1:1', 1, 1);
             INSERT INTO entries VALUES (2, 1, 'command', 1790000005000, 'b:2:2', 1, 2);
             INSERT INTO fts (rowid, body, idents) VALUES (1, 'terraform plan', ''), (2, 'terraform apply', '');
             PRAGMA user_version = 2;",
        )
        .unwrap();
    }
    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    let old = row(&handle, Source::Hutan, "OLD").unwrap();
    assert_eq!(old.title.as_deref(), Some("old shell"));
    assert_eq!(old.messages, 2);
    assert_eq!(
        old.latest
            .iter()
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>(),
        ["terraform plan", "terraform apply"]
    );
    assert_eq!(old.updated_ms, Some(1_790_000_005_000));
    drop(indexer);
    let conn = rusqlite::Connection::open(&env.db).unwrap();
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, hutan_index::store::SCHEMA_VERSION);
}
