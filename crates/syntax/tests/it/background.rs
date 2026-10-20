//! The asynchronous side: cancellation by version, never blocking on a stale tree, the edit log.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hutan_syntax::{Language, MAX_LOGGED_EDITS, Syntax, TextEdit, Token};

use crate::common::{fixture, parse};
use crate::incremental::{Rng, apply, script};
use crate::support::{Gate, Gated, watchdog};

const LONG: Duration = Duration::from_secs(20);

/// Rust source of at least `bytes` bytes (the fixture repeated).
fn big_rust(bytes: usize) -> String {
    let unit = fixture(Language::Rust);
    let mut text = String::with_capacity(bytes + unit.len());
    while text.len() < bytes {
        text.push_str(&unit);
    }
    text
}

fn rust() -> (Syntax, Arc<Mutex<Vec<u64>>>) {
    let published = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&published);
    let syntax = Syntax::with_notify(Language::Rust, move |version| {
        sink.lock().unwrap().push(version);
    })
    .expect("syntax");
    (syntax, published)
}

#[test]
fn a_newer_version_cancels_the_parse_in_flight() {
    let (syntax, published) = rust();
    let gate = Gate::new();
    let entered = Gate::new();
    watchdog(&gate, Duration::from_secs(30));

    let old = big_rust(300_000);
    assert!(syntax.reparse(1, Gated::new(&old, &gate, &entered)));
    entered.wait(); // the worker is inside the parse of version 1

    let new = big_rust(2_000);
    assert!(syntax.reparse(2, new.clone()));
    gate.open();

    let snapshot = syntax.wait_for(2, LONG).expect("version 2");
    assert_eq!(snapshot.version(), 2);
    assert_eq!(snapshot.to_sexp(), parse(Language::Rust, &new).to_sexp());
    let stats = syntax.stats();
    assert_eq!(stats.cancelled, 1, "{stats:?}");
    assert_eq!(stats.completed, 1, "{stats:?}");
    assert_eq!(
        *published.lock().unwrap(),
        vec![2],
        "version 1 must never be published"
    );
}

#[test]
fn a_request_the_worker_has_not_started_is_replaced() {
    let (syntax, published) = rust();
    let gate = Gate::new();
    let entered = Gate::new();
    watchdog(&gate, Duration::from_secs(30));

    let text = big_rust(2_000);
    assert!(syntax.reparse(1, Gated::new(&text, &gate, &entered)));
    entered.wait();
    assert!(syntax.reparse(2, text.clone()));
    assert!(syntax.reparse(3, text.clone()));
    gate.open();

    syntax.wait_for(3, LONG).expect("version 3");
    let stats = syntax.stats();
    assert_eq!(stats.superseded, 1, "version 2 never started: {stats:?}");
    assert_eq!(stats.started, 2, "{stats:?}");
    assert_eq!(*published.lock().unwrap(), vec![3]);
}

#[test]
fn old_and_repeated_versions_are_refused() {
    let (syntax, _) = rust();
    assert!(syntax.reparse(5, "fn a() {}".to_string()));
    assert!(!syntax.reparse(5, "fn b() {}".to_string()));
    assert!(!syntax.reparse(4, "fn c() {}".to_string()));
    syntax.wait_for(5, LONG).expect("version 5");
    assert!(!syntax.reparse(5, "fn d() {}".to_string()));
    assert!(syntax.reparse(6, "fn e() {}".to_string()));
}

#[test]
fn reading_never_waits_for_a_parse_in_flight() {
    let (syntax, _) = rust();
    let source = "fn main() { let s = \"text\"; }".to_string();
    assert!(syntax.reparse(1, source.clone()));
    syntax.wait_for(1, LONG).expect("first tree");

    let gate = Gate::new();
    let entered = Gate::new();
    watchdog(&gate, Duration::from_secs(10));
    // Version 2 inserts a comment line; the new parse is held inside the text read.
    let edited = format!("// c\n{source}");
    syntax.edit(
        2,
        TextEdit::new(source.as_str(), 0..0, "// c\n").expect("edit"),
    );
    assert!(syntax.reparse(2, Gated::new(&edited, &gate, &entered)));
    entered.wait();

    let started = Instant::now();
    for _ in 0..200 {
        let view = syntax.view().expect("a finished tree exists");
        assert!(!view.is_current());
        assert_eq!(view.snapshot().version(), 1);
        let spans = view.highlights(0..edited.len());
        // The old tree's spans, moved down by the inserted line.
        let string = spans
            .iter()
            .find(|s| s.token == Token::String)
            .expect("string span");
        assert_eq!(&edited[string.range.clone()], "\"text\"");
        let _ = syntax.snapshot();
        let _ = syntax.stats();
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "reads waited for the parser: {:?}",
        started.elapsed()
    );
    gate.open();
    let snapshot = syntax.wait_for(2, LONG).expect("version 2");
    assert!(syntax.view().expect("view").is_current());
    assert_eq!(snapshot.to_sexp(), parse(Language::Rust, &edited).to_sexp());
}

#[test]
fn typing_inside_a_string_keeps_it_highlighted_until_the_tree_lands() {
    let (syntax, _) = rust();
    let source = "let s = \"abc\"; let n = 1;".to_string();
    syntax.reparse(1, source.clone());
    syntax.wait_for(1, LONG).expect("first tree");

    let gate = Gate::new();
    let entered = Gate::new();
    watchdog(&gate, Duration::from_secs(10));
    let mut text = source.clone();
    let mut version = 1;
    let held = |text: &str| Gated::new(text, &gate, &entered);
    for typed in ["X", "Y", "Z"] {
        let at = text.find("\";").expect("closing quote");
        syntax.edit(
            version + 1,
            TextEdit::new(text.as_str(), at..at, typed).expect("edit"),
        );
        text.insert_str(at, typed);
        version += 1;
        assert!(syntax.reparse(version, held(&text)));
    }
    entered.wait();
    let spans = syntax.highlights(0..text.len());
    let string = spans
        .iter()
        .find(|s| s.token == Token::String)
        .expect("string");
    assert_eq!(&text[string.range.clone()], "\"abcXYZ\"");
    let number = spans
        .iter()
        .find(|s| s.token == Token::Number)
        .expect("number");
    assert_eq!(&text[number.range.clone()], "1");
    gate.open();
    syntax.wait_for(version, LONG).expect("final tree");
}

#[test]
fn many_unawaited_edits_end_with_the_tree_of_the_final_text() {
    for seed in 0..4 {
        let (syntax, _) = rust();
        let mut rng = Rng::new(seed + 100);
        let original = fixture(Language::Rust);
        syntax.reparse(0, original.clone());
        syntax.wait_for(0, LONG).expect("first tree");

        // 75 garbage edits, each followed by its undo, so the final text is valid again.
        let mut text = original.clone();
        let mut version = 0;
        for (range, replacement) in script(&mut rng, &original, 75) {
            let (edit, _) = apply(&mut text, range, &replacement);
            version += 1;
            syntax.edit(version, edit);
            syntax.reparse(version, text.clone());
            if version % 17 == 0 {
                // Let some parses finish mid-way so both paths (log kept, log pruned) run.
                std::thread::sleep(Duration::from_millis(3));
            }
        }
        assert_eq!(text, original);
        let snapshot = syntax.wait_for(version, LONG).expect("final tree");
        assert_eq!(snapshot.version(), version);
        let fresh = parse(Language::Rust, &text);
        assert_eq!(snapshot.to_sexp(), fresh.to_sexp(), "seed {seed}");
        assert_eq!(
            snapshot.highlights(0..text.len()),
            fresh.highlights(0..text.len())
        );
        let stats = syntax.stats();
        assert_eq!(
            stats.started,
            stats.completed + stats.cancelled,
            "{stats:?}"
        );
    }
}

#[test]
fn awaited_edits_each_publish_and_equal_a_fresh_parse() {
    let (syntax, published) = rust();
    let mut rng = Rng::new(31);
    let original = fixture(Language::Rust);
    let mut text = original.clone();
    syntax.reparse(0, text.clone());
    let mut version = 0;
    for (range, replacement) in script(&mut rng, &original, 30) {
        syntax.wait_for(version, LONG).expect("previous tree");
        let (edit, _) = apply(&mut text, range, &replacement);
        version += 1;
        syntax.edit(version, edit);
        syntax.reparse(version, text.clone());
        let snapshot = syntax.wait_for(version, LONG).expect("tree");
        // Every second version undoes the one before it: the text is valid again there.
        if version % 2 == 0 {
            assert_eq!(text, original);
            assert_eq!(
                snapshot.to_sexp(),
                parse(Language::Rust, &text).to_sexp(),
                "version {version}"
            );
        }
    }
    assert_eq!(published.lock().unwrap().len(), 61);
}

#[test]
fn more_edits_than_the_log_keeps_fall_back_to_a_fresh_parse() {
    let (syntax, _) = rust();
    let gate = Gate::new();
    let entered = Gate::new();
    watchdog(&gate, Duration::from_secs(20));
    let mut text = "fn a() {}\n".to_string();
    syntax.reparse(0, text.clone());
    syntax.wait_for(0, LONG).expect("first tree");

    // Hold a parse so nothing publishes while the edits pile up.
    let held = format!("{text}// held\n");
    syntax.edit(
        1,
        TextEdit::new(text.as_str(), text.len()..text.len(), "// held\n").expect("edit"),
    );
    text.push_str("// held\n");
    assert!(syntax.reparse(1, Gated::new(&held, &gate, &entered)));
    entered.wait();
    let mut version = 1;
    for _ in 0..MAX_LOGGED_EDITS + 50 {
        version += 1;
        let end = text.len();
        syntax.edit(
            version,
            TextEdit::new(text.as_str(), end..end, "x").expect("edit"),
        );
        text.push('x');
    }
    // The old tree can no longer be carried forward: no stale highlights rather than wrong ones.
    let view = syntax.view().expect("view");
    assert!(view.highlights(0..text.len()).is_empty());
    assert_eq!(view.map_offset(0), None);

    assert!(syntax.reparse(version, text.clone()));
    gate.open();
    let snapshot = syntax.wait_for(version, LONG).expect("final tree");
    assert_eq!(snapshot.to_sexp(), parse(Language::Rust, &text).to_sexp());
    // And the next edit is incremental again.
    let end = text.len();
    syntax.edit(
        version + 1,
        TextEdit::new(text.as_str(), end..end, "\nfn b() {}").expect("edit"),
    );
    text.push_str("\nfn b() {}");
    syntax.reparse(version + 1, text.clone());
    let snapshot = syntax.wait_for(version + 1, LONG).expect("next tree");
    assert_eq!(snapshot.to_sexp(), parse(Language::Rust, &text).to_sexp());
    assert!(
        !syntax
            .view()
            .expect("view")
            .highlights(0..text.len())
            .is_empty()
    );
}

#[test]
fn reset_replaces_the_document() {
    let (syntax, _) = rust();
    syntax.reparse(1, "fn a() {}".to_string());
    syntax.wait_for(1, LONG).expect("first tree");
    assert!(syntax.reset(2, "struct S;".to_string()));
    let snapshot = syntax.wait_for(2, LONG).expect("reset tree");
    assert_eq!(snapshot.root().named_children()[0].kind(), "struct_item");
    let view = syntax.view().expect("view");
    assert!(view.is_current());
    assert_eq!(
        view.highlights(0..9).first().map(|s| s.token),
        Some(Token::Keyword)
    );
}

#[test]
fn dropping_a_syntax_with_a_parse_in_flight_does_not_wait_for_it() {
    let (syntax, _) = rust();
    let gate = Gate::new();
    let entered = Gate::new();
    let text = big_rust(100_000);
    syntax.reparse(1, Gated::new(&text, &gate, &entered));
    entered.wait();
    let started = Instant::now();
    drop(syntax);
    assert!(started.elapsed() < Duration::from_secs(2));
    gate.open();
}

#[test]
fn unavailable_languages_are_reported_not_panicked() {
    // Every grammar is compiled in by default; a build without a feature reports it.
    for language in Language::ALL {
        match Syntax::new(language) {
            Ok(_) => assert!(language.is_compiled()),
            Err(error) => {
                assert!(!language.is_compiled());
                assert!(error.to_string().contains(language.feature()), "{error}");
            }
        }
    }
}
