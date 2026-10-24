//! `hutan terminal search` against a real daemon and a real PTY.

use crate::common;

use std::time::Duration;

use common::{TestDaemon, WAIT};
use tokio::time::{sleep, timeout};

const SCRIPT: &str =
    "printf 'first Needle line\\n'; seq 1 400; printf 'second needle\\nEND-MARK\\n'; exec cat";

async fn session(t: &TestDaemon) -> String {
    let id = t.new_session(&[], &["sh", "-c", SCRIPT]).await;
    let found = timeout(WAIT, async {
        loop {
            let out = t.hutan(&["terminal", "search", &id, "END-MARK"]).await;
            if out.code == 0 && out.stdout.contains("END-MARK") {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(found.is_ok(), "the output never arrived");
    id
}

#[tokio::test(flavor = "multi_thread")]
async fn prints_row_and_line_for_each_match_in_the_scrollback() {
    let t = TestDaemon::start().await;
    let id = session(&t).await;

    // Case-insensitive by default: both lines; the first is far above the 24-row screen.
    let out = t.hutan(&["terminal", "search", &id, "needle"]).await.ok();
    assert_eq!(
        out.stdout, "0: first Needle line\n401: second needle\n",
        "{out:?}"
    );
    assert_eq!(out.stderr, "");

    // --case matches the capital only.
    let out = t
        .hutan(&["terminal", "search", &id, "Needle", "--case"])
        .await
        .ok();
    assert_eq!(out.stdout, "0: first Needle line\n");

    // --regex works on whole lines.
    let out = t
        .hutan(&[
            "terminal", "search", &id, r"^\d{3}$", "--regex", "--limit", "3",
        ])
        .await
        .ok();
    assert_eq!(out.stdout, "100: 100\n101: 101\n102: 102\n", "{out:?}");
    assert!(
        out.stderr.contains("--from 103"),
        "the limit says how to go on: {}",
        out.stderr
    );
    let more = t
        .hutan(&[
            "terminal", "search", &id, r"^\d{3}$", "--regex", "--limit", "2", "--from", "103",
        ])
        .await
        .ok();
    assert_eq!(more.stdout, "103: 103\n104: 104\n");

    // Nothing found is not an error.
    let out = t.hutan(&["terminal", "search", &id, "no-such-text"]).await;
    assert_eq!((out.code, out.stdout.as_str()), (0, ""), "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn json_carries_positions_and_a_bad_pattern_fails() {
    let t = TestDaemon::start().await;
    let id = session(&t).await;
    let out = t
        .hutan(&["terminal", "search", &id, "second", "--json"])
        .await
        .ok();
    let json = out.json();
    assert_eq!(json["matches"][0]["row"], 401);
    assert_eq!(json["matches"][0]["col"], 0);
    assert_eq!(json["matches"][0]["end_col"], 6);
    assert_eq!(json["matches"][0]["line"], "second needle");
    assert_eq!(json["truncated"], false);
    assert!(json["viewport_top"].as_u64().unwrap() > 300);

    let bad = t
        .hutan(&["terminal", "search", &id, "(open", "--regex"])
        .await;
    assert_eq!(bad.code, 1, "{bad:?}");
    assert!(bad.stderr.contains("invalid pattern"), "{bad:?}");
    let missing = t.hutan(&["terminal", "search", "zzzz", "x"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");
    t.stop().await;
}
