//! `hutan_buffer::Text` as the text of a parse, and buffer transactions as parse edits.

use hutan_buffer::{Buffer, Edit, Text};
use hutan_syntax::{Language, Snapshot, Syntax, TextEdit, TextProvider, edits_of_transaction};

use crate::common::{fixture, parse};
use crate::incremental::{Rng, random_edit};

fn multi_chunk_rust() -> String {
    let unit = fixture(Language::Rust);
    unit.repeat(40) + "// é日本😀\n"
}

#[test]
fn a_rope_parses_like_a_string() {
    let source = multi_chunk_rust();
    let text = Text::from(source.as_str());
    assert!(text.chunks().count() > 4, "the test needs several chunks");
    let from_rope = Snapshot::parse(Language::Rust, text.clone(), 0).expect("rope");
    let from_string = parse(Language::Rust, &source);
    assert_eq!(from_rope.to_sexp(), from_string.to_sexp());
    assert_eq!(
        from_rope.highlights(0..source.len()),
        from_string.highlights(0..source.len())
    );
}

#[test]
fn rope_points_and_lines_equal_the_default_scan() {
    let source = multi_chunk_rust();
    let text = Text::from(source.as_str());
    for offset in (0..=source.len()).step_by(997).chain([source.len()]) {
        if !source.is_char_boundary(offset) {
            continue;
        }
        assert_eq!(
            TextProvider::point_at(&text, offset),
            TextProvider::point_at(source.as_str(), offset),
            "offset {offset}"
        );
    }
    for row in [0, 1, 17, 400, 100_000] {
        assert_eq!(
            TextProvider::line_start(&text, row),
            TextProvider::line_start(source.as_str(), row),
            "row {row}"
        );
    }
}

#[test]
fn chunks_start_exactly_at_the_offset_even_inside_a_character() {
    let source = "aé日😀z".repeat(500);
    let text = Text::from(source.as_str());
    for offset in 0..source.len() {
        let chunk = TextProvider::chunk(&text, offset);
        assert!(!chunk.is_empty(), "offset {offset}");
        assert_eq!(
            chunk,
            &source.as_bytes()[offset..offset + chunk.len()],
            "offset {offset}"
        );
    }
    assert!(TextProvider::chunk(&text, source.len()).is_empty());
    assert!(TextProvider::chunk(&text, source.len() + 10).is_empty());
}

#[test]
fn buffer_transactions_drive_incremental_parses() {
    let mut rng = Rng::new(5);
    let original = fixture(Language::Rust);
    let mut buffer = Buffer::from_content(&original);
    let mut snapshot =
        Snapshot::parse(Language::Rust, buffer.text().clone(), buffer.version()).expect("first");
    let mut clean_steps = 0;
    for round in 0..60 {
        // One to three random edits as one transaction, in the coordinates of the text before it;
        // overlapping candidates are dropped (a transaction's edits must not overlap).
        let before = buffer.text().clone();
        let mut candidates: Vec<Edit> = (0..1 + rng.below(3))
            .map(|_| {
                let (range, replacement) = random_edit(&mut rng, &before.to_string());
                Edit::new(range, replacement)
            })
            .collect();
        candidates.sort_by_key(|e| (e.range.start, e.range.end));
        let mut kept: Vec<Edit> = Vec::new();
        for edit in candidates {
            if kept
                .last()
                .is_none_or(|last| last.range.end <= edit.range.start)
            {
                kept.push(edit);
            }
        }
        let applied = buffer
            .edit(kept)
            .expect("non-overlapping edits of the current text");
        let parse_edits = edits_of_transaction(&before, &applied.transaction).expect("edits");
        snapshot = snapshot
            .reparse(&parse_edits, buffer.text().clone(), applied.version)
            .expect("reparse");
        // Random fragments break the syntax; tree-sitter's error recovery may differ between an
        // incremental and a fresh parse (see `incremental.rs`), so compare where it parses.
        let fresh = parse(Language::Rust, &buffer.text().to_string());
        if !fresh.has_errors() {
            assert_eq!(snapshot.to_sexp(), fresh.to_sexp(), "round {round}");
            clean_steps += 1;
        }

        // Undo is the inverse transaction: the text is the valid original again.
        let before = buffer.text().clone();
        let undone = buffer.undo().expect("undo");
        let parse_edits = edits_of_transaction(&before, &undone.transaction).expect("edits");
        snapshot = snapshot
            .reparse(&parse_edits, buffer.text().clone(), undone.version)
            .expect("reparse");
        assert_eq!(buffer.text().to_string(), original);
        assert_eq!(
            snapshot.to_sexp(),
            parse(Language::Rust, &original).to_sexp(),
            "round {round}: the undo diverged"
        );
    }
    assert!(clean_steps < 60, "the random edits were all harmless");
}

#[test]
fn buffer_undo_and_redo_feed_the_same_way() {
    let mut buffer = Buffer::from_content("fn a() { 1 }\nfn b() { 2 }\n");
    let syntax = Syntax::new(Language::Rust).expect("syntax");
    syntax.reparse(buffer.version(), buffer.text().clone());
    syntax
        .wait_for(buffer.version(), std::time::Duration::from_secs(10))
        .expect("first");

    let step = |buffer: &mut Buffer,
                action: &dyn Fn(&mut Buffer) -> Option<hutan_buffer::Applied>| {
        let before = buffer.text().clone();
        let applied = action(buffer).expect("applied");
        for edit in edits_of_transaction(&before, &applied.transaction).expect("edits") {
            syntax.edit(applied.version, edit);
        }
        syntax.reparse(applied.version, buffer.text().clone());
        let tree = syntax
            .wait_for(applied.version, std::time::Duration::from_secs(10))
            .expect("tree");
        assert_eq!(
            tree.to_sexp(),
            parse(Language::Rust, &buffer.text().to_string()).to_sexp()
        );
    };
    step(&mut buffer, &|b| b.insert(10, "let x = 3; ").ok());
    step(&mut buffer, &|b| b.delete(0..13).ok());
    step(&mut buffer, &|b| b.undo());
    step(&mut buffer, &|b| b.undo());
    step(&mut buffer, &|b| b.redo());
}

#[test]
fn text_edits_from_a_rope_match_those_from_a_string() {
    let source = multi_chunk_rust();
    let text = Text::from(source.as_str());
    let at = source.len() / 2;
    let at = (at..)
        .find(|i| source.is_char_boundary(*i))
        .expect("boundary");
    let from_rope = TextEdit::new(&text, at..at + 5, "x\ny").expect("rope edit");
    let from_string = TextEdit::new(source.as_str(), at..at + 5, "x\ny").expect("string edit");
    assert_eq!(from_rope, from_string);
}
