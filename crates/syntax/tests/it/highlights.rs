//! Golden highlight spans per language: a snippet, the exact spans, regenerated with
//! `UPDATE_GOLDEN=1 cargo nextest run -p hutan-syntax` and reviewed like code.

use hutan_syntax::{Language, Snapshot, Token};

use crate::common::{fixture, fixture_name, manifest_dir, parse};

fn render(language: Language, source: &str, snapshot: &Snapshot) -> String {
    let mut out = format!(
        "language: {language}\nfixture: {}\nerrors: {}\n",
        fixture_name(language),
        snapshot.has_errors()
    );
    for span in snapshot.highlights(0..source.len()) {
        out.push_str(&format!(
            "{:>5}..{:<5} {:<28} {:?}\n",
            span.range.start,
            span.range.end,
            span.token.name(),
            &source[span.range.clone()]
        ));
    }
    out
}

#[test]
fn golden_spans_per_language() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut failures = Vec::new();
    for language in Language::compiled() {
        let source = fixture(language);
        let snapshot = parse(language, &source);
        assert!(
            !snapshot.has_errors(),
            "{language}: the fixture has syntax errors: {}",
            snapshot.to_sexp()
        );
        let got = render(language, &source, &snapshot);
        let path = manifest_dir()
            .join("tests/it/golden")
            .join(format!("{language}.txt"));
        if update {
            std::fs::write(&path, &got).expect("write golden");
            continue;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (run with UPDATE_GOLDEN=1)", path.display()));
        if got != want {
            failures.push(format!(
                "{language}: spans differ from {} (UPDATE_GOLDEN=1 rewrites it)\n--- got\n{got}",
                path.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn spans_are_sorted_disjoint_and_inside_the_range() {
    for language in Language::compiled() {
        let source = fixture(language);
        let snapshot = parse(language, &source);
        let spans = snapshot.highlights(0..source.len());
        assert!(!spans.is_empty(), "{language}: nothing highlighted");
        let mut at = 0;
        for span in &spans {
            assert!(span.range.start >= at, "{language}: overlap at {span:?}");
            assert!(
                span.range.start < span.range.end,
                "{language}: empty {span:?}"
            );
            assert!(span.range.end <= source.len());
            assert!(
                source.is_char_boundary(span.range.start)
                    && source.is_char_boundary(span.range.end)
            );
            at = span.range.end;
        }
    }
}

#[test]
fn a_sub_range_is_the_clipped_whole() {
    for language in Language::compiled() {
        let source = fixture(language);
        let snapshot = parse(language, &source);
        let whole = snapshot.highlights(0..source.len());
        let (from, to) = (source.len() / 3, source.len() * 2 / 3);
        let from = (from..source.len())
            .find(|i| source.is_char_boundary(*i))
            .unwrap_or(from);
        let to = (to..source.len())
            .find(|i| source.is_char_boundary(*i))
            .unwrap_or(to);
        let part = snapshot.highlights(from..to);
        // Captures that merge across the range's edge are merged differently, so compare per byte.
        let mut expected = vec![None; source.len()];
        for span in &whole {
            for slot in &mut expected[span.range.clone()] {
                *slot = Some(span.token);
            }
        }
        let mut got = vec![None; source.len()];
        for span in &part {
            assert!(
                span.range.start >= from && span.range.end <= to,
                "{language}"
            );
            for slot in &mut got[span.range.clone()] {
                *slot = Some(span.token);
            }
        }
        assert_eq!(&got[from..to], &expected[from..to], "{language}");
    }
}

#[test]
fn every_language_highlights_a_keyword_or_a_string() {
    for language in Language::compiled() {
        let source = fixture(language);
        let snapshot = parse(language, &source);
        let tokens: Vec<Token> = snapshot
            .highlights(0..source.len())
            .into_iter()
            .map(|s| s.token)
            .collect();
        assert!(
            tokens.iter().any(|t| matches!(
                t,
                Token::Keyword
                    | Token::String
                    | Token::Property
                    | Token::Tag
                    | Token::MarkupHeading
            )),
            "{language}: {tokens:?}"
        );
    }
}
