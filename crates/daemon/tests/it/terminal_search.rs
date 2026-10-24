//! `terminal.search` (protocol 0.44): matches over wrapped rows, beyond the screen, regex, case,
//! blocks, bounds and cancellation, against real terminals — over the daemon's socket, and
//! straight at the scan where a test needs an exact point (a slice boundary, the clock).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::time::{Duration, Instant};

use common::*;
use hutan_client::Client;
use hutan_core::TerminalId;
use hutan_daemon::terminal_search::{Bounds, Params, SearchError, Searches, run};
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{Request, TerminalHistory, TerminalHistoryRows};
use hutan_protocol::terminal_search::{TerminalSearch, TerminalSearchMatch, TerminalSearchResult};
use hutan_terminal::{RenderTracker, Terminal, TerminalConfig};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

fn query(terminal: TerminalId, text: &str) -> TerminalSearch {
    TerminalSearch {
        terminal_id: terminal,
        query: text.to_owned(),
        case: false,
        regex: false,
        limit: 1000,
        from: None,
        block_id: None,
        text: false,
    }
}

async fn search(client: &Client, req: TerminalSearch) -> TerminalSearchResult {
    client.terminal_search(req).await.unwrap()
}

/// Searches until `pred` holds for the matches of `req` (the output has arrived).
async fn wait_search(
    client: &Client,
    req: TerminalSearch,
    what: &str,
    pred: impl Fn(&TerminalSearchResult) -> bool,
) -> TerminalSearchResult {
    let deadline = Instant::now() + WAIT;
    loop {
        let result = search(client, req.clone()).await;
        if pred(&result) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: timed out; last answer {result:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

fn spots(result: &TerminalSearchResult) -> Vec<(u64, u16, u64, u16)> {
    result
        .matches
        .iter()
        .map(|m| (m.row, m.col, m.end_row, m.end_col))
        .collect()
}

/// The text of absolute row `row` as `terminal.history` shows it.
async fn row_text_at(client: &Client, terminal: TerminalId, viewport_top: u64, row: u64) -> String {
    assert!(row < viewport_top, "row {row} is above the viewport");
    let from = u32::try_from(viewport_top - row - 1).unwrap();
    let rows: TerminalHistoryRows = client
        .call(Request::TerminalHistory(TerminalHistory {
            terminal_id: terminal,
            from_line: from,
            count: 1,
        }))
        .await
        .unwrap();
    common::row_text(&rows.rows[0]).trim_end().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn matches_across_wrapped_rows_report_rows_and_columns() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    assert!(client.supports_terminal_search());
    // 20 columns: the line takes three rows; "needle" starts at column 16 of row 0 and ends on
    // row 1, "STUV" is inside row 1, a second line follows.
    let mut spec =
        sh("printf 'abcdefghijklmnopneedleQRSTUVWXYZ\\r\\nsecond needle\\r\\nEND\\r\\n'; exec cat");
    spec.size = Some(size(20, 6));
    let session = create(&client, spec).await;
    let terminal = terminal_of(&session);
    let mut req = query(terminal, "needle");
    req.text = true;
    let found = wait_search(&client, req, "both lines", |r| r.matches.len() == 2).await;
    assert_eq!(spots(&found), [(0, 16, 1, 2), (2, 7, 2, 13)]);
    assert_eq!(
        found.matches[0].line.as_deref(),
        Some("abcdefghijklmnopneedleQRSTUVWXYZ")
    );
    assert_eq!(found.matches[1].line.as_deref(), Some("second needle"));
    assert!(!found.truncated && found.next_from.is_none() && !found.timed_out);
    assert_eq!(found.first_row, 0);
    // Positions only without `text`.
    let bare = search(&client, query(terminal, "needle")).await;
    assert!(bare.matches.iter().all(|m| m.line.is_none()));
    // A match inside one row, and one that spans a wrap.
    let inner = search(&client, query(terminal, "STUV")).await;
    assert_eq!(spots(&inner), [(1, 4, 1, 8)]);
    let across = search(&client, query(terminal, "NEEDLEQR")).await;
    assert_eq!(spots(&across), [(0, 16, 1, 4)]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_scrollback_is_searched_and_rows_map_to_history() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("seq 1 3000; printf 'END-MARK\\r\\n'; exec cat")).await;
    let terminal = terminal_of(&session);
    wait_search(&client, query(terminal, "END-MARK"), "the output", |r| {
        r.matches.len() == 1
    })
    .await;
    // "1234" is a thousand rows above the 24-row screen: far beyond what a view loads.
    let found = search(&client, query(terminal, "1234")).await;
    assert_eq!(found.matches.len(), 1, "{found:?}");
    let m = &found.matches[0];
    // The tty adds no echo of seq's output: line n is row n - 1.
    assert_eq!((m.row, m.col, m.end_col), (1233, 0, 4));
    assert!(m.row < found.viewport_top);
    assert_eq!(
        row_text_at(&client, terminal, found.viewport_top, m.row).await,
        "1234"
    );
    // Searching from after it finds nothing; from its row, it again.
    let mut later = query(terminal, "1234");
    later.from = Some(m.row + 1);
    assert!(search(&client, later).await.matches.is_empty());
    let mut at = query(terminal, "1234");
    at.from = Some(m.row);
    assert_eq!(search(&client, at).await.matches.len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn case_regex_and_bad_queries() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(
        &client,
        sh("printf 'Error: Foo\\r\\nerror: bar\\r\\nERROR baz\\r\\nCAFÉ café\\r\\nEND\\r\\n'; exec cat"),
    )
    .await;
    let terminal = terminal_of(&session);
    wait_search(&client, query(terminal, "END"), "the output", |r| {
        r.matches.len() == 1
    })
    .await;
    let insensitive = search(&client, query(terminal, "error")).await;
    assert_eq!(
        spots(&insensitive),
        [(0, 0, 0, 5), (1, 0, 1, 5), (2, 0, 2, 5)]
    );
    let mut sensitive = query(terminal, "error");
    sensitive.case = true;
    assert_eq!(spots(&search(&client, sensitive).await), [(1, 0, 1, 5)]);
    // Case folding is Unicode: É matches é, in columns of cells.
    assert_eq!(
        spots(&search(&client, query(terminal, "café")).await),
        [(3, 0, 3, 4), (3, 5, 3, 9)]
    );

    let mut regex = query(terminal, r"^(error|ERROR)\b.*(foo|baz)$");
    regex.regex = true;
    assert_eq!(
        spots(&search(&client, regex.clone()).await),
        [(0, 0, 0, 10), (2, 0, 2, 9)]
    );
    regex.case = true;
    assert_eq!(spots(&search(&client, regex).await), [(2, 0, 2, 9)]);
    // `.` and `\s` stay inside one logical line.
    let mut across = query(terminal, r"Foo\s+error");
    across.regex = true;
    assert!(search(&client, across).await.matches.is_empty());
    // Characters that mean something to a regex are literal without `regex`.
    assert!(
        search(&client, query(terminal, "(foo|baz)"))
            .await
            .matches
            .is_empty()
    );

    // Refused: a pattern that does not compile, one over the size limit, a long query.
    let mut invalid = query(terminal, "(unclosed");
    invalid.regex = true;
    assert_eq!(
        remote_code(client.terminal_search(invalid).await),
        ErrorCode::BadRequest
    );
    let mut huge = query(terminal, r"((a{100}){100}){100}");
    huge.regex = true;
    assert_eq!(
        remote_code(client.terminal_search(huge).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(
            client
                .terminal_search(query(terminal, &"x".repeat(2000)))
                .await
        ),
        ErrorCode::BadRequest
    );
    // An empty query matches nothing; an unknown terminal is not found.
    assert!(
        search(&client, query(terminal, ""))
            .await
            .matches
            .is_empty()
    );
    assert_eq!(
        remote_code(client.terminal_search(query(TerminalId(9999), "x")).await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn limits_clamp_and_paging_covers_everything_once() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("seq 1 600; printf 'END-MARK\\r\\n'; exec cat")).await;
    let terminal = terminal_of(&session);
    wait_search(&client, query(terminal, "END-MARK"), "the output", |r| {
        r.matches.len() == 1
    })
    .await;
    // Every line holding a "1" (and "END-MARK" none): the reference, in one call.
    let all = search(&client, query(terminal, "1")).await;
    let expected = (1..=600).filter(|n| n.to_string().contains('1')).count();
    assert!(
        all.matches.len() >= expected,
        "{} < {expected}",
        all.matches.len()
    );
    assert!(!all.truncated);

    let mut paged: Vec<TerminalSearchMatch> = Vec::new();
    let mut from = None;
    let mut calls = 0;
    loop {
        calls += 1;
        let mut page = query(terminal, "1");
        page.limit = 25;
        page.from = from;
        let answer = search(&client, page).await;
        assert!(answer.matches.len() <= 25);
        paged.extend(answer.matches.clone());
        if !answer.truncated {
            assert!(answer.next_from.is_none());
            break;
        }
        from = Some(answer.next_from.expect("next_from with truncated"));
    }
    assert!(calls >= 8, "{calls} calls");
    assert_eq!(paged, all.matches, "paging misses or repeats a match");

    // The limit is clamped to 1..=1000: 0 behaves as 1, and more than 1000 as 1000.
    let mut none = query(terminal, "1");
    none.limit = 0;
    let one = search(&client, none).await;
    assert_eq!(one.matches.len(), 1);
    assert!(one.truncated);
    let mut many = query(terminal, "e");
    many.limit = u32::MAX;
    assert!(search(&client, many).await.matches.len() <= 1000);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_thousand_matches_in_one_line_are_cut_at_the_limit() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let mut spec = sh("printf 'aaaaaaaaaa%.0s' $(seq 1 30); printf '\\r\\nEND\\r\\n'; exec cat");
    spec.size = Some(size(40, 10));
    let session = create(&client, spec).await;
    let terminal = terminal_of(&session);
    wait_search(&client, query(terminal, "END"), "the output", |r| {
        r.matches.len() == 1
    })
    .await;
    let mut req = query(terminal, "a");
    req.limit = 100;
    let answer = search(&client, req).await;
    // 300 matches on one line (8 rows): the first 100, then on after that line.
    assert_eq!(answer.matches.len(), 100);
    assert!(answer.truncated);
    assert_eq!(answer.next_from, Some(8));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_output_keeps_rows_and_adds_matches() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("seq 1 50; printf 'needle one\\r\\n'; exec cat")).await;
    let terminal = terminal_of(&session);
    let first = wait_search(&client, query(terminal, "needle"), "the first match", |r| {
        r.matches.len() == 1
    })
    .await;
    // The terminal echoes the typed line and cat prints it again: two more matches, and the
    // first keeps its row although the screen scrolled.
    client
        .send_input(terminal, b"another needle\n")
        .await
        .unwrap();
    let later = wait_search(&client, query(terminal, "needle"), "the new matches", |r| {
        r.matches.len() == 3
    })
    .await;
    assert_eq!(later.matches[0], first.matches[0]);
    assert!(later.viewport_top > first.viewport_top);
    assert!(later.matches[1].row > later.matches[0].row);
    t.stop().await;
}

const MARKS: &str = "printf '\\033]133;A\\007$ \\033]133;B\\007run one\\r\\n\\033]133;C\\007shared hit\\r\\nonly-one\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007run two\\r\\n\\033]133;C\\007shared hit\\r\\nonly-two\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat";

#[tokio::test(flavor = "multi_thread")]
async fn matches_know_their_block_and_a_block_can_be_the_only_scope() {
    let t = TestDaemon::start().await;
    let (client, _events) = t.connect_subscribed().await;
    let session = create(&client, sh(MARKS)).await;
    let terminal = terminal_of(&session);
    let blocks = loop {
        let blocks = client
            .terminal_blocks(terminal, None, 10)
            .await
            .unwrap()
            .blocks;
        if blocks.len() == 3 && blocks[1].end_line.is_some() {
            break blocks;
        }
        sleep(Duration::from_millis(25)).await;
    };
    // Newest first: [prompt, two, one].
    let (one, two) = (blocks[2].id, blocks[1].id);
    let all = search(&client, query(terminal, "shared hit")).await;
    assert_eq!(
        all.matches.iter().map(|m| m.block_id).collect::<Vec<_>>(),
        [Some(one), Some(two)]
    );
    let mut only = query(terminal, "shared hit");
    only.block_id = Some(two);
    let in_two = search(&client, only).await;
    assert_eq!(in_two.matches.len(), 1);
    assert_eq!(in_two.matches[0], all.matches[1]);
    // The command line is not output: `run one` is in block one's rows but not its output.
    assert_eq!(
        search(&client, query(terminal, "run one"))
            .await
            .matches
            .len(),
        1
    );
    let mut command = query(terminal, "run one");
    command.block_id = Some(one);
    assert!(search(&client, command).await.matches.is_empty());
    let mut missing = query(terminal, "x");
    missing.block_id = Some(99);
    assert_eq!(
        remote_code(client.terminal_search(missing).await),
        ErrorCode::NotFound
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_alternate_screen_is_searched_as_it_is() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let mut spec = sh("seq 1 40; printf '\\033[?1049h\\033[2;3Halt needle'; exec cat");
    spec.size = Some(size(30, 5));
    let session = create(&client, spec).await;
    let terminal = terminal_of(&session);
    let found = wait_search(
        &client,
        query(terminal, "needle"),
        "the alternate screen",
        |r| !r.matches.is_empty(),
    )
    .await;
    // Row 1 (0-based) of the screen, column 6; the primary history is not searched.
    assert_eq!(
        spots(&found),
        [(found.viewport_top + 1, 6, found.viewport_top + 1, 12)]
    );
    assert!(
        search(&client, query(terminal, "17"))
            .await
            .matches
            .is_empty()
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_searches_answer_or_are_superseded_and_one_answers() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let session = create(&client, sh("seq 1 4000; printf 'END-MARK\\r\\n'; exec cat")).await;
    let terminal = terminal_of(&session);
    wait_search(&client, query(terminal, "END-MARK"), "the output", |r| {
        r.matches.len() == 1
    })
    .await;
    let mut calls = Vec::new();
    for round in 0..12 {
        let client = client.clone();
        let mut req = query(terminal, "1");
        req.limit = 5 + round;
        calls.push(tokio::spawn(
            async move { client.terminal_search(req).await },
        ));
    }
    let mut answered = 0;
    for call in calls {
        match call.await.unwrap() {
            Ok(answer) => {
                answered += 1;
                assert!(!answer.matches.is_empty());
            }
            Err(hutan_client::ClientError::Remote(error)) => {
                assert_eq!(error.code, ErrorCode::Conflict, "{error:?}");
                assert_eq!(
                    error.data.as_ref().and_then(|d| d["reason"].as_str()),
                    Some("superseded")
                );
            }
            Err(other) => panic!("{other:?}"),
        }
    }
    // Whichever search the daemon began last cannot be superseded.
    assert!(answered >= 1);
    t.stop().await;
}

// Straight at the scan -----------------------------------------------------------------------

/// A terminal that printed `lines` lines and then `END-MARK`, and waits.
async fn printed(lines: u32) -> Terminal {
    let terminal = Terminal::spawn(TerminalConfig {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!("seq 1 {lines}; printf 'END-MARK\\r\\n'; exec cat"),
        ],
        size: size(40, 10),
        ..TerminalConfig::default()
    })
    .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let snapshot = terminal.snapshot(&mut RenderTracker::new());
        if snapshot
            .lines
            .iter()
            .any(|row| row_text(row).contains("END-MARK"))
        {
            return terminal;
        }
        assert!(Instant::now() < deadline, "the output never arrived");
        sleep(Duration::from_millis(10)).await;
    }
}

fn params(text: &str, limit: u32) -> Params {
    Params::new(&TerminalSearch {
        limit,
        ..query(TerminalId(1), text)
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_time_budget_ends_a_search_with_where_to_go_on() {
    let terminal = printed(2000).await;
    let params = params("7", 1000);
    let token = CancellationToken::new();
    let whole = run(&terminal, &params, &Bounds::default(), &token, || {}).unwrap();
    assert!(!whole.truncated);
    // A zero budget with four-row slices: every call scans one slice, then says where it got to.
    let tight = Bounds {
        chunk_rows: 4,
        budget: Duration::ZERO,
    };
    let mut merged = Vec::new();
    let mut from = 0;
    let mut calls = 0;
    loop {
        calls += 1;
        let answer = run(
            &terminal,
            &Params::new(&TerminalSearch {
                from: Some(from),
                ..query(TerminalId(1), "7")
            })
            .unwrap(),
            &tight,
            &token,
            || {},
        )
        .unwrap();
        merged.extend(answer.matches.clone());
        if !answer.truncated {
            break;
        }
        assert!(answer.timed_out, "{answer:?}");
        assert!(answer.next_from.unwrap() > from);
        from = answer.next_from.unwrap();
    }
    assert!(calls > 100, "{calls} calls of four rows");
    assert_eq!(merged, whole.matches);
    terminal.kill().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_stops_the_scan_at_the_next_slice() {
    let terminal = printed(2000).await;
    let params = params("7", 1000);
    let token = CancellationToken::new();
    let tight = Bounds {
        chunk_rows: 8,
        budget: Duration::from_secs(60),
    };
    let mut slices = 0;
    let outcome = run(&terminal, &params, &tight, &token, || {
        slices += 1;
        if slices == 3 {
            token.cancel();
        }
    });
    assert_eq!(outcome.unwrap_err(), SearchError::Superseded);
    assert_eq!(slices, 3, "no slice runs after the cancel");
    // Cancelled before it starts: no slice at all.
    let mut ran = 0;
    let outcome = run(&terminal, &params, &tight, &token, || ran += 1);
    assert_eq!(outcome.unwrap_err(), SearchError::Superseded);
    assert_eq!(ran, 0);
    terminal.kill().unwrap();
}

#[test]
fn a_newer_search_cancels_the_older_of_the_same_connection_and_terminal_only() {
    use hutan_daemon::conn::ConnId;
    let mut searches = Searches::default();
    let (a, a_token) = searches.begin(ConnId(1), TerminalId(1));
    let (b_other_terminal, b_token) = searches.begin(ConnId(1), TerminalId(2));
    let (c_other_conn, c_token) = searches.begin(ConnId(2), TerminalId(1));
    assert!(!a_token.is_cancelled() && !b_token.is_cancelled() && !c_token.is_cancelled());
    let (newer, newer_token) = searches.begin(ConnId(1), TerminalId(1));
    assert!(a_token.is_cancelled());
    assert!(!newer_token.is_cancelled() && !b_token.is_cancelled() && !c_token.is_cancelled());
    // The older one ending does not unregister the newer; the newer still gets cancelled.
    searches.end(ConnId(1), TerminalId(1), a);
    let (_, newest_token) = searches.begin(ConnId(1), TerminalId(1));
    assert!(newer_token.is_cancelled());
    searches.end(ConnId(1), TerminalId(1), newer);
    assert!(!newest_token.is_cancelled());
    // A closed connection cancels its searches, a removed terminal the searches on it.
    searches.cancel_connection(ConnId(1));
    assert!(newest_token.is_cancelled() && b_token.is_cancelled());
    assert!(!c_token.is_cancelled());
    searches.cancel_terminal(TerminalId(1));
    assert!(c_token.is_cancelled());
    let _ = (b_other_terminal, c_other_conn);
}
