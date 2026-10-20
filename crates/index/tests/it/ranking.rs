//! FTS5 availability and ranking: identifier queries, role weights, grouping, filters.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hutan_index::search::search;
use hutan_index::store;
use hutan_index::text::idents;
use hutan_index::{Role, SearchQuery, Source};
use rusqlite::Connection;

struct Db {
    _dir: tempfile::TempDir,
    conn: Connection,
}

impl Db {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::open(&dir.path().join("index.sqlite")).unwrap();
        Self { _dir: dir, conn }
    }

    /// A Hutan-style session (no file) holding the given rows.
    fn session(&self, key: &str, cwd: &str, rows: &[(Role, u64, &str)]) {
        let id = store::ensure_session(&self.conn, Source::Hutan, key, None, Some(key), Some(cwd))
            .unwrap();
        for (role, ts, text) in rows {
            let ids = if *role == Role::Command {
                (*text).to_owned()
            } else {
                idents(text)
            };
            store::insert_entry(&self.conn, id, *role, Some(*ts), None, None, text, &ids).unwrap();
        }
    }

    fn keys(&self, query: &str) -> Vec<String> {
        self.find(&SearchQuery {
            text: query.to_owned(),
            limit: 50,
            ..SearchQuery::default()
        })
        .into_iter()
        .map(|hit| hit.session_key)
        .collect()
    }

    fn find(&self, query: &SearchQuery) -> Vec<hutan_index::Hit> {
        search(&self.conn, query).unwrap()
    }

    /// Unrelated conversation rows so that the words of the queries are rare.
    fn noise(&self, sessions: usize) {
        for i in 0..sessions {
            self.session(
                &format!("noise-{i}"),
                "/work/other",
                &[
                    (Role::User, 1000, &format!("question number {i} about gardening and tomatoes in the greenhouse")),
                    (Role::Assistant, 1001, &format!("answer {i}: water the plants early, mulch the beds and keep notes about soil")),
                    (Role::Tool, 1002, &format!("read file notes/garden-{i}.md ok")),
                ],
            );
        }
    }
}

#[test]
fn fts5_with_bm25_is_available() {
    let db = Db::new();
    let enabled: i64 = db
        .conn
        .query_row("SELECT sqlite_compileoption_used('ENABLE_FTS5')", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(enabled, 1);
    db.conn
        .execute_batch(
            "CREATE VIRTUAL TABLE probe USING fts5(a, b);
             INSERT INTO probe VALUES ('hello world', 'x');
             INSERT INTO probe VALUES ('other', 'y');",
        )
        .unwrap();
    let score: f64 = db
        .conn
        .query_row(
            "SELECT bm25(probe, 1.0, 2.0) FROM probe WHERE probe MATCH 'hello'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(score < 0.0, "bm25 is negative for a match: {score}");
}

#[test]
fn an_identifier_query_finds_the_command_row_first() {
    let db = Db::new();
    db.noise(60);
    db.session(
        "talk-about-it",
        "/work/hutan",
        &[
            (Role::User, 2000, "could you run cargo test -p daemon for me and tell me what fails"),
            (Role::Assistant, 2001, "I will run the daemon tests with cargo and report back; the daemon test suite is large and cargo test takes a while"),
        ],
    );
    db.session(
        "tool-output",
        "/work/hutan",
        &[(
            Role::Output,
            2002,
            "warning: unused import in crates/daemon/src/lib.rs\n    Running unittests (target/debug/deps/hutan_daemon)\nnote: to rerun use `cargo test -p daemon` after fixing\ntest result: ok. 120 passed; 0 failed",
        )],
    );
    db.session(
        "agent-tool-call",
        "/work/hutan",
        &[(
            Role::Tool,
            2003,
            "Bash\ncommand: cargo test -p daemon --locked\ndescription: run the daemon tests",
        )],
    );
    db.session(
        "the-command",
        "/work/hutan",
        &[(Role::Command, 2004, "cargo test -p daemon")],
    );

    let keys = db.keys("cargo test -p daemon");
    assert_eq!(
        keys.first().map(String::as_str),
        Some("the-command"),
        "{keys:?}"
    );
    // Every session that contains all the words is found.
    for expected in ["talk-about-it", "tool-output", "agent-tool-call"] {
        assert!(
            keys.iter().any(|k| k == expected),
            "{expected} missing from {keys:?}"
        );
    }
    assert!(!keys.iter().any(|k| k.starts_with("noise-")), "{keys:?}");
}

#[test]
fn role_weights_order_otherwise_equal_rows() {
    let db = Db::new();
    db.noise(80);
    let text = "investigating the zebra migration failure in the storage layer";
    for (key, role) in [
        ("as-output", Role::Output),
        ("as-tool", Role::Tool),
        ("as-assistant", Role::Assistant),
        ("as-user", Role::User),
        ("as-command", Role::Command),
    ] {
        db.session(key, "/work/storage", &[(role, 3000, text)]);
    }
    assert_eq!(
        db.keys("zebra migration"),
        [
            "as-command",
            "as-user",
            "as-assistant",
            "as-tool",
            "as-output"
        ]
    );
}

#[test]
fn each_session_is_one_hit_with_its_best_row_and_a_count() {
    let db = Db::new();
    db.noise(40);
    db.session(
        "busy",
        "/work/busy",
        &[
            (
                Role::Assistant,
                10,
                "the quokka problem appears in many places, quokka quokka",
            ),
            (Role::User, 20, "quokka"),
            (Role::Tool, 30, "grep quokka src"),
        ],
    );
    db.session(
        "quiet",
        "/work/quiet",
        &[(
            Role::Assistant,
            15,
            "one mention of a quokka only, nothing more to say about it here",
        )],
    );
    let hits = db.find(&SearchQuery {
        text: "quokka".to_owned(),
        limit: 10,
        ..SearchQuery::default()
    });
    assert_eq!(hits.len(), 2);
    let busy = hits.iter().find(|h| h.session_key == "busy").unwrap();
    assert_eq!(busy.matches, 3);
    assert_eq!(
        busy.role,
        Role::User,
        "the short user row is the best of the session"
    );
    assert!(busy.snippet.contains("«quokka»"), "{}", busy.snippet);
    assert!(busy.score > 0.0);
    let limited = db.find(&SearchQuery {
        text: "quokka".to_owned(),
        limit: 1,
        ..SearchQuery::default()
    });
    assert_eq!(limited.len(), 1);
}

#[test]
fn filters_narrow_the_hits() {
    let db = Db::new();
    db.noise(30);
    db.session(
        "a",
        "/work/alpha",
        &[(Role::User, 1_000_000, "platypus in alpha")],
    );
    db.session(
        "b",
        "/work/alpha/sub",
        &[(
            Role::Assistant,
            2_000_000,
            "platypus in the subdirectory of alpha",
        )],
    );
    db.session(
        "c",
        "/work/alphabet",
        &[(Role::User, 3_000_000, "platypus in alphabet")],
    );
    let search = |f: &dyn Fn(&mut SearchQuery)| {
        let mut query = SearchQuery {
            text: "platypus".to_owned(),
            limit: 50,
            ..SearchQuery::default()
        };
        f(&mut query);
        let mut keys: Vec<String> = db.find(&query).into_iter().map(|h| h.session_key).collect();
        keys.sort();
        keys
    };
    assert_eq!(search(&|_| {}), ["a", "b", "c"]);
    assert_eq!(search(&|q| q.roles = vec![Role::User]), ["a", "c"]);
    assert_eq!(
        search(&|q| q.sources = vec![Source::Claude]),
        Vec::<String>::new()
    );
    assert_eq!(
        search(&|q| q.sources = vec![Source::Hutan]),
        ["a", "b", "c"]
    );
    // A project is a directory: itself and what is inside, not a sibling with the same prefix.
    assert_eq!(
        search(&|q| q.project = Some("/work/alpha".to_owned())),
        ["a", "b"]
    );
    assert_eq!(search(&|q| q.since_ms = Some(2_000_000)), ["b", "c"]);
    assert_eq!(search(&|q| q.until_ms = Some(2_000_000)), ["a", "b"]);
    assert_eq!(
        search(&|q| {
            q.since_ms = Some(1_500_000);
            q.until_ms = Some(2_500_000);
        }),
        ["b"]
    );
}

#[test]
fn punctuation_in_the_query_never_breaks_it() {
    let db = Db::new();
    db.session(
        "s",
        "/w",
        &[(
            Role::Assistant,
            1,
            "see crates/daemon/src/lib.rs for the crate::paths helper and --locked flag",
        )],
    );
    for query in [
        "crates/daemon/src/lib.rs",
        "crate::paths",
        "--locked",
        "\"the crate::paths helper\"",
        "lib.rs AND OR NOT",
        "col:value NEAR(",
        "(unbalanced",
        "star*",
    ] {
        let result = search(
            &db.conn,
            &SearchQuery {
                text: query.to_owned(),
                limit: 5,
                ..SearchQuery::default()
            },
        );
        assert!(result.is_ok(), "{query}: {result:?}");
    }
    assert_eq!(db.keys("crates/daemon/src/lib.rs"), ["s"]);
    assert_eq!(db.keys("\"crate::paths helper\""), ["s"]);
    assert!(db.keys("daemon nonexistentword").is_empty());
    assert!(
        search(
            &db.conn,
            &SearchQuery {
                text: "!!".into(),
                limit: 5,
                ..SearchQuery::default()
            }
        )
        .is_err()
    );
}

#[test]
fn deleting_rows_removes_them_from_search() {
    let db = Db::new();
    db.session("keep", "/w", &[(Role::User, 1, "narwhal sighting")]);
    db.session("drop", "/w", &[(Role::User, 5, "narwhal sighting again")]);
    assert_eq!(db.keys("narwhal").len(), 2);
    let pruned = store::prune_before(&db.conn, 3).unwrap();
    assert_eq!(pruned, 1);
    assert_eq!(db.keys("narwhal"), ["drop"]);
}
