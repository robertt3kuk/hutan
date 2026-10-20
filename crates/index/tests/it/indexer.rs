//! The running indexer: FSEvents-driven indexing, Hutan records, clear, rebuild, and that
//! nothing is read when no source is configured.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hutan_index::{
    BlockRef, HutanRecord, IndexConfig, IndexHandle, Indexer, Role, SearchQuery, SessionTextQuery,
    Source,
};

const WAIT: Duration = Duration::from_secs(15);

fn query(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_owned(),
        limit: 20,
        ..SearchQuery::default()
    }
}

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

fn keys(handle: &IndexHandle, text: &str) -> Vec<String> {
    handle
        .search(&query(text))
        .map(|hits| hits.into_iter().map(|h| h.session_key).collect())
        .unwrap_or_default()
}

fn claude_line(text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/app","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    db: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude/projects");
    fs::create_dir_all(claude.join("-work-app")).unwrap();
    let db = tmp.path().join("home/index.sqlite");
    Env {
        _tmp: tmp,
        claude,
        db,
    }
}

fn config(env: &Env, hutan: bool) -> IndexConfig {
    IndexConfig {
        db_path: env.db.clone(),
        range_days: 0,
        roots: vec![(Source::Claude, env.claude.clone())],
        hutan,
    }
}

fn append(path: &Path, line: &str) {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(file, "{line}").unwrap();
}

#[test]
fn existing_files_are_indexed_at_start_and_new_ones_through_file_events() {
    let env = env();
    let existing = env.claude.join("-work-app/old-session.jsonl");
    append(
        &existing,
        &claude_line("an earlier conversation about kubernetes"),
    );

    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("the startup pass", || {
        keys(&handle, "kubernetes") == ["old-session"]
    });

    // A new file in a new project directory, found by FSEvents.
    fs::create_dir_all(env.claude.join("-work-other")).unwrap();
    let fresh = env.claude.join("-work-other/new-session.jsonl");
    append(&fresh, &claude_line("brand new talk about terraform"));
    wait_for("the new file", || {
        keys(&handle, "terraform") == ["new-session"]
    });

    // An append to an existing file reads only the new line.
    let opened = handle.metrics().files_opened();
    let before = handle.metrics().bytes_read();
    let line = claude_line("follow up about prometheus");
    append(&existing, &line);
    wait_for("the appended line", || {
        keys(&handle, "prometheus") == ["old-session"]
    });
    assert_eq!(
        handle.metrics().bytes_read() - before,
        line.len() as u64 + 1
    );
    assert!(handle.metrics().files_opened() > opened);
    assert_eq!(handle.metrics().reindexed(), 0);

    let status = handle.status().unwrap();
    let claude = status
        .sources
        .iter()
        .find(|s| s.source == Source::Claude)
        .unwrap();
    assert_eq!((claude.files, claude.rows, claude.errors), (2, 3, 0));
    assert!(claude.last_indexed_ms.is_some());
    assert!(status.db_bytes > 0);
}

#[test]
fn clear_empties_the_index_and_rebuild_reads_everything_again() {
    let env = env();
    let file = env.claude.join("-work-app/s.jsonl");
    append(&file, &claude_line("notes about elasticsearch"));
    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    wait_for("the startup pass", || {
        keys(&handle, "elasticsearch") == ["s"]
    });
    assert!(handle.push_hutan(HutanRecord {
        session_key: "01HUTANSESSION".to_owned(),
        title: Some("shell".to_owned()),
        cwd: Some("/work/app".to_owned()),
        role: Role::Command,
        ts_ms: 1_790_000_000_000,
        text: "kubectl get pods".to_owned(),
        ext_key: "b:1:1".to_owned(),
        block: None,
    }));
    handle.flush().unwrap();
    assert_eq!(keys(&handle, "kubectl pods"), ["01HUTANSESSION"]);

    handle.clear().unwrap();
    assert!(keys(&handle, "elasticsearch").is_empty());
    assert!(keys(&handle, "kubectl").is_empty());
    let status = handle.status().unwrap();
    assert!(
        status.sources.iter().all(|s| s.rows == 0 && s.files == 0),
        "{status:?}"
    );

    // Reads the file again: rebuild restores transcript rows, Hutan rows are gone with clear.
    handle.rebuild().unwrap();
    wait_for("the rebuild", || keys(&handle, "elasticsearch") == ["s"]);

    // Hutan rows survive a rebuild.
    assert!(handle.push_hutan(HutanRecord {
        session_key: "01HUTANSESSION".to_owned(),
        title: None,
        cwd: None,
        role: Role::Output,
        ts_ms: 1_790_000_000_001,
        text: "NAME READY STATUS pods listed".to_owned(),
        ext_key: "b:1:1:out".to_owned(),
        block: None,
    }));
    handle.rebuild().unwrap();
    handle.flush().unwrap();
    wait_for("the second rebuild", || {
        keys(&handle, "elasticsearch") == ["s"]
    });
    assert_eq!(keys(&handle, "pods listed"), ["01HUTANSESSION"]);
}

#[test]
fn hutan_records_are_replaced_by_their_key_and_redacted() {
    let env = env();
    let indexer = Indexer::start(IndexConfig {
        roots: Vec::new(),
        ..config(&env, true)
    })
    .unwrap();
    let handle = indexer.handle();
    let record = |text: &str| HutanRecord {
        session_key: "S1".to_owned(),
        title: Some("build".to_owned()),
        cwd: Some("/work/app".to_owned()),
        role: Role::Output,
        ts_ms: 1_790_000_000_000,
        text: text.to_owned(),
        ext_key: "b:3:1790000000000".to_owned(),
        block: None,
    };
    handle.push_hutan(record("compiling widgets with token=abcdef0123456789xyz"));
    handle.flush().unwrap();
    assert_eq!(keys(&handle, "widgets"), ["S1"]);
    assert!(keys(&handle, "abcdef0123456789xyz").is_empty());

    handle.push_hutan(record("compiling gadgets"));
    handle.flush().unwrap();
    assert!(keys(&handle, "widgets").is_empty());
    assert_eq!(keys(&handle, "gadgets"), ["S1"]);
    let status = handle.status().unwrap();
    let hutan = status
        .sources
        .iter()
        .find(|s| s.source == Source::Hutan)
        .unwrap();
    assert_eq!(hutan.rows, 1);
    assert!(hutan.last_indexed_ms.is_some());
}

#[test]
fn hutan_records_are_ignored_when_the_source_is_off() {
    let env = env();
    let indexer = Indexer::start(IndexConfig {
        roots: Vec::new(),
        ..config(&env, false)
    })
    .unwrap();
    let handle = indexer.handle();
    handle.push_hutan(HutanRecord {
        session_key: "S1".to_owned(),
        title: None,
        cwd: None,
        role: Role::Command,
        ts_ms: 1,
        text: "ls".to_owned(),
        ext_key: "k".to_owned(),
        block: None,
    });
    handle.flush().unwrap();
    assert!(handle.status().unwrap().sources.is_empty());
    assert!(keys(&handle, "ls").is_empty());
}

#[test]
fn no_configured_source_means_no_file_is_read() {
    let env = env();
    append(
        &env.claude.join("-work-app/s.jsonl"),
        &claude_line("never indexed words"),
    );
    let indexer = Indexer::start(IndexConfig {
        roots: Vec::new(),
        ..config(&env, false)
    })
    .unwrap();
    let handle = indexer.handle();
    append(
        &env.claude.join("-work-app/s2.jsonl"),
        &claude_line("also never indexed"),
    );
    handle.flush().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    handle.flush().unwrap();
    assert_eq!(handle.metrics().files_opened(), 0);
    assert_eq!(handle.metrics().bytes_read(), 0);
    assert!(keys(&handle, "indexed").is_empty());
}

#[test]
fn the_range_limits_what_is_read() {
    let env = env();
    let old = env.claude.join("-work-app/old.jsonl");
    append(&old, &claude_line("ancient history about fortran"));
    // Make the file look a year old.
    let year_ago = std::time::SystemTime::now() - Duration::from_secs(365 * 86_400);
    fs::OpenOptions::new()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(year_ago)
        .unwrap();
    let recent = env.claude.join("-work-app/recent.jsonl");
    append(&recent, &claude_line("fresh news about rust"));

    let indexer = Indexer::start(IndexConfig {
        range_days: 30,
        ..config(&env, false)
    })
    .unwrap();
    let handle = indexer.handle();
    wait_for("the recent file", || keys(&handle, "rust") == ["recent"]);
    assert!(keys(&handle, "fortran").is_empty());
    assert_eq!(
        handle.metrics().files_opened(),
        1,
        "the old file was not even opened"
    );
}

#[test]
fn stopping_joins_the_worker_and_further_calls_fail() {
    let env = env();
    let mut indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    handle.flush().unwrap();
    indexer.stop();
    assert!(handle.flush().is_err());
    assert!(!handle.push_hutan(HutanRecord {
        session_key: "S".to_owned(),
        title: None,
        cwd: None,
        role: Role::User,
        ts_ms: 1,
        text: "x".to_owned(),
        ext_key: "k".to_owned(),
        block: None,
    }));
    indexer.stop();
}

#[test]
fn the_rows_of_a_session_come_back_in_conversation_order_filtered_and_bounded() {
    let env = env();
    let file = env.claude.join("-work-app/conv.jsonl");
    append(&file, &claude_line("first question about zebras"));
    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    wait_for("the startup pass", || keys(&handle, "zebras") == ["conv"]);
    for (index, (role, text)) in [
        (Role::User, "hello"),
        (Role::Assistant, "hi there"),
        (Role::Tool, "ls -la"),
        (Role::User, "and a token=abcdef0123456789xyz"),
    ]
    .into_iter()
    .enumerate()
    {
        handle.push_hutan(HutanRecord {
            session_key: "CHAT".to_owned(),
            title: Some("A chat".to_owned()),
            cwd: None,
            role,
            ts_ms: 1_790_000_000_000 + index as u64,
            text: text.to_owned(),
            ext_key: format!("c:{index}"),
            block: None,
        });
    }
    handle.flush().unwrap();

    let query = |roles: Vec<Role>, limit: usize| SessionTextQuery {
        key: "CHAT".to_owned(),
        sources: Vec::new(),
        roles,
        limit,
    };
    let all = handle.session_text(&query(Vec::new(), 100)).unwrap();
    assert_eq!(
        all.iter().map(|r| r.role).collect::<Vec<_>>(),
        [Role::User, Role::Assistant, Role::Tool, Role::User]
    );
    assert_eq!(all[0].text, "hello");
    assert_eq!(all[0].title.as_deref(), Some("A chat"));
    assert!(
        !all[3].text.contains("abcdef0123456789xyz"),
        "redacted: {all:?}"
    );
    // Only the conversation, and only the newest rows when there are more than asked for.
    let talk = handle
        .session_text(&query(vec![Role::User, Role::Assistant], 2))
        .unwrap();
    assert_eq!(
        talk.iter().map(|r| r.role).collect::<Vec<_>>(),
        [Role::Assistant, Role::User]
    );
    // A transcript file's session is found by its key, filtered by source.
    let conv = handle
        .session_text(&SessionTextQuery {
            key: "conv".to_owned(),
            sources: vec![Source::Claude],
            roles: Vec::new(),
            limit: 10,
        })
        .unwrap();
    assert_eq!(conv.len(), 1);
    assert_eq!(conv[0].source, Source::Claude);
    assert!(
        handle
            .session_text(&SessionTextQuery {
                key: "conv".to_owned(),
                sources: vec![Source::Hutan],
                roles: Vec::new(),
                limit: 10,
            })
            .unwrap()
            .is_empty()
    );
    assert!(handle.session_text(&query(Vec::new(), 10)).is_ok());
}

#[test]
fn hits_of_every_transcript_source_carry_their_file_and_block_hits_their_block() {
    let tmp = tempfile::tempdir().unwrap();
    let claude = tmp.path().join("claude/projects");
    let codex = tmp.path().join("codex/sessions");
    let omp = tmp.path().join("omp/sessions");
    fs::create_dir_all(claude.join("-work-app")).unwrap();
    fs::create_dir_all(codex.join("2026/04/16")).unwrap();
    fs::create_dir_all(omp.join("-work-app")).unwrap();
    let claude_file = claude.join("-work-app/s1.jsonl");
    let codex_file = codex
        .join("2026/04/16/rollout-2026-04-16T10-00-00-0198aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl");
    let omp_file = omp.join("-work-app/2026-09-28T05-01-22-983Z_0f4c7d52.jsonl");
    append(&claude_file, &claude_line("notes about quokkas"));
    append(
        &codex_file,
        r#"{"timestamp":"2026-04-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0198aaaa-bbbb-cccc-dddd-eeeeffff0000","cwd":"/work/app"}}"#,
    );
    append(
        &codex_file,
        r#"{"timestamp":"2026-04-16T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"wombat retry policy"}]}}"#,
    );
    append(
        &omp_file,
        r#"{"type":"session","version":3,"id":"0f4c7d52","timestamp":"2026-09-28T05:01:22.983Z","cwd":"/work/app"}"#,
    );
    append(
        &omp_file,
        r#"{"type":"message","timestamp":"2026-09-28T05:01:23.000Z","message":{"role":"user","content":[{"type":"text","text":"numbat scheduler"}]}}"#,
    );
    let db = tmp.path().join("home/index.sqlite");
    let indexer = Indexer::start(IndexConfig {
        db_path: db,
        range_days: 0,
        roots: vec![
            (Source::Claude, claude),
            (Source::Codex, codex),
            (Source::Omp, omp),
        ],
        hutan: true,
    })
    .unwrap();
    let handle = indexer.handle();
    for (word, file) in [
        ("quokkas", &claude_file),
        ("wombat", &codex_file),
        ("numbat", &omp_file),
    ] {
        wait_for(word, || keys(&handle, word).len() == 1);
        let hits = handle.search(&query(word)).unwrap();
        assert_eq!(
            hits[0].path.as_deref(),
            Some(file.canonicalize().unwrap().to_str().unwrap()),
            "the {word} hit names its transcript"
        );
        assert_eq!(hits[0].block, None);
    }

    assert!(handle.push_hutan(HutanRecord {
        session_key: "01HUTANSESSION".to_owned(),
        title: Some("shell".to_owned()),
        cwd: Some("/work/app".to_owned()),
        role: Role::Command,
        ts_ms: 1_790_000_000_000,
        text: "kubectl rollout status".to_owned(),
        ext_key: "b:12:1790000000000".to_owned(),
        block: Some(BlockRef {
            terminal_id: 7,
            block_id: 12,
        }),
    }));
    assert!(handle.push_hutan(HutanRecord {
        session_key: "01HUTANSESSION".to_owned(),
        title: None,
        cwd: None,
        role: Role::Tool,
        ts_ms: 1_790_000_000_500,
        text: "codex idle: finished the rollout plan".to_owned(),
        ext_key: "a:1790000000500".to_owned(),
        block: None,
    }));
    handle.flush().unwrap();
    let block_hit = &handle.search(&query("kubectl")).unwrap()[0];
    assert_eq!(block_hit.path, None);
    assert_eq!(
        block_hit.block,
        Some(BlockRef {
            terminal_id: 7,
            block_id: 12
        })
    );
    let report_hit = &handle.search(&query("plan")).unwrap()[0];
    assert_eq!(report_hit.block, None, "a report row is no block");
}

/// Builds the index database of the build before protocol 0.15: schema 1, an external session
/// with a row and a Hutan session with a row.
fn write_v1_database(path: &Path, transcript: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
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
             role TEXT NOT NULL, ts_ms INTEGER, ext_key TEXT);
         CREATE INDEX entries_session ON entries(session_id);
         CREATE INDEX entries_ts ON entries(ts_ms);
         CREATE UNIQUE INDEX entries_ext ON entries(session_id, ext_key) WHERE ext_key IS NOT NULL;
         CREATE VIRTUAL TABLE fts USING fts5(body, idents, tokenize = 'unicode61 remove_diacritics 2');
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    // The transcript's old row: stale on purpose (it says "stale"), so that a re-read shows.
    conn.execute(
        "INSERT INTO sources VALUES (1, ?1, 'claude', 1, 1, 1, 0, 1, '{}', 1, 0, 1)",
        [transcript.to_str().unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions VALUES (1, 'claude', 's1', 1, NULL, '/work/app')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions VALUES (2, 'hutan', 'OLDHUTAN', NULL, 'shell', '/work/app')",
        [],
    )
    .unwrap();
    for (id, session, role, body) in [
        (1, 1, "user", "stale transcript row"),
        (2, 2, "command", "terraform plan"),
    ] {
        conn.execute(
            "INSERT INTO entries VALUES (?1, ?2, ?3, 1790000000000, NULL)",
            rusqlite::params![id, session, role],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO fts (rowid, body, idents) VALUES (?1, ?2, '')",
            rusqlite::params![id, body],
        )
        .unwrap();
    }
}

#[test]
fn an_index_of_the_previous_schema_is_upgraded_and_reindexed_not_dropped() {
    let env = env();
    let transcript = env.claude.join("-work-app/s1.jsonl");
    append(&transcript, &claude_line("fresh words about axolotls"));
    write_v1_database(&env.db, &transcript);

    let indexer = Indexer::start(config(&env, true)).unwrap();
    let handle = indexer.handle();
    // The transcript was read again: its new text is there, the stale row is not, and the hit
    // names the file.
    wait_for("the transcript to be read again", || {
        keys(&handle, "axolotls") == ["s1"]
    });
    assert!(keys(&handle, "stale").is_empty());
    assert_eq!(
        handle.search(&query("axolotls")).unwrap()[0]
            .path
            .as_deref(),
        Some(transcript.canonicalize().unwrap().to_str().unwrap())
    );
    // Hutan rows cannot be derived again: they survive, without a block.
    let old = handle.search(&query("terraform")).unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(
        (old[0].session_key.as_str(), old[0].block),
        ("OLDHUTAN", None)
    );
    // And the database is current: a block row can be written to it.
    assert!(handle.push_hutan(HutanRecord {
        session_key: "NEW".to_owned(),
        title: None,
        cwd: None,
        role: Role::Command,
        ts_ms: 1_790_000_000_000,
        text: "make deploy".to_owned(),
        ext_key: "b:1:1".to_owned(),
        block: Some(BlockRef {
            terminal_id: 3,
            block_id: 1
        }),
    }));
    handle.flush().unwrap();
    assert_eq!(
        handle.search(&query("deploy")).unwrap()[0].block,
        Some(BlockRef {
            terminal_id: 3,
            block_id: 1
        })
    );
    drop(indexer);
    let conn = rusqlite::Connection::open(&env.db).unwrap();
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, hutan_index::store::SCHEMA_VERSION);
}

#[test]
fn an_index_of_an_unknown_future_schema_is_rebuilt_empty() {
    let env = env();
    fs::create_dir_all(env.db.parent().unwrap()).unwrap();
    {
        let conn = rusqlite::Connection::open(&env.db).unwrap();
        conn.execute_batch("CREATE TABLE junk (x); PRAGMA user_version = 99;")
            .unwrap();
    }
    append(
        &env.claude.join("-work-app/s1.jsonl"),
        &claude_line("words about okapis"),
    );
    let indexer = Indexer::start(config(&env, false)).unwrap();
    let handle = indexer.handle();
    wait_for("the rebuilt index", || keys(&handle, "okapis") == ["s1"]);
}
