//! Nodes, structural selection, brackets, folds and indentation on small sources.

use hutan_syntax::{Fold, Language};

use crate::common::parse;

fn at(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in {text:?}"))
}

fn text_of<'a>(text: &'a str, range: &std::ops::Range<usize>) -> &'a str {
    &text[range.clone()]
}

#[test]
fn node_at_finds_the_smallest_named_node() {
    let src = "fn main() { let value = 1 + 2; }";
    let tree = parse(Language::Rust, src);
    let offset = at(src, "value") + 2;
    let node = tree.node_at(offset..offset).expect("node");
    assert_eq!(node.kind(), "identifier");
    assert_eq!(text_of(src, &node.range()), "value");
    assert!(node.is_named() && !node.has_error());

    let both = at(src, "1 + 2")..at(src, "1 + 2") + 5;
    assert_eq!(
        tree.node_at(both).expect("node").kind(),
        "binary_expression"
    );

    let leaf = tree
        .leaf_at(at(src, "fn")..at(src, "fn") + 2)
        .expect("leaf");
    assert_eq!(leaf.kind(), "fn");
    assert!(!leaf.is_named());
    assert_eq!(tree.root().kind(), "source_file");
}

#[test]
fn expand_selection_climbs_to_the_root_and_stops() {
    let src = "fn main() { let value = foo(1, 2); }";
    let tree = parse(Language::Rust, src);
    let cursor = at(src, "value") + 1;
    let mut selection = cursor..cursor;
    let mut chain = Vec::new();
    while let Some(next) = tree.expand_selection(selection.clone()) {
        assert!(next.start <= selection.start && next.end >= selection.end);
        assert_ne!(next, selection);
        chain.push(text_of(src, &next).to_string());
        selection = next;
    }
    assert_eq!(chain.first().map(String::as_str), Some("value"));
    assert!(chain.iter().any(|s| s == "let value = foo(1, 2);"));
    assert_eq!(
        selection,
        0..src.len(),
        "the last expansion is the whole file"
    );
    assert_eq!(tree.expand_selection(0..src.len()), None);
}

#[test]
fn shrink_selection_takes_the_first_named_child() {
    let src = "fn main() { let value = foo(1, 2); }";
    let tree = parse(Language::Rust, src);
    let call = at(src, "foo(1, 2)")..at(src, "foo(1, 2)") + 9;
    let shrunk = tree.shrink_selection(call.clone()).expect("child");
    assert_eq!(text_of(src, &shrunk), "foo");
    // A selection that is not a whole node does not shrink.
    assert_eq!(tree.shrink_selection(call.start + 1..call.end), None);
    // A leaf has nothing to shrink to.
    assert_eq!(tree.shrink_selection(shrunk), None);
}

#[test]
fn expand_then_shrink_returns_inside_the_selection() {
    let src = "struct S { a: u8 }\nfn f(x: u8) -> u8 { x + 1 }\n";
    let tree = parse(Language::Rust, src);
    let start = at(src, "x + 1");
    let mut selection = start..start;
    let mut stack = vec![selection.clone()];
    while let Some(next) = tree.expand_selection(selection.clone()) {
        stack.push(next.clone());
        selection = next;
    }
    for pair in stack.windows(2) {
        let (inner, outer) = (&pair[0], &pair[1]);
        assert!(outer.start <= inner.start && inner.end <= outer.end);
        if let Some(shrunk) = tree.shrink_selection(outer.clone()) {
            assert!(outer.start <= shrunk.start && shrunk.end <= outer.end);
        }
    }
}

#[test]
fn brackets_match_through_the_tree() {
    let src = "fn f(a: Vec<u8>) -> bool { let s = \"(\"; a.len() < 3 && g([1, (2)]) }";
    let tree = parse(Language::Rust, src);

    let open = at(src, "(a");
    let pair = tree.bracket_match(open).expect("params");
    assert_eq!(pair.open, open..open + 1);
    assert_eq!(pair.close, at(src, ") ->")..at(src, ") ->") + 1);

    // Just after a bracket also matches (the cursor sits right of the `)`).
    let close = at(src, ") ->") + 1;
    assert_eq!(tree.bracket_match(close).expect("after").open, pair.open);

    // Generics pair `<` with `>` ...
    let lt = at(src, "<u8");
    assert_eq!(
        tree.bracket_match(lt).expect("generic").close,
        at(src, ">)")..at(src, ">)") + 1
    );
    // ... but a comparison `<` has no partner, and a bracket in a string is not a token.
    assert_eq!(tree.bracket_match(at(src, "< 3")), None);
    assert_eq!(tree.bracket_match(at(src, "\"(\"") + 1), None);

    // Nesting: the inner pair of `[1, (2)]`.
    let inner = at(src, "(2)");
    let pair = tree.bracket_match(inner).expect("inner");
    assert_eq!(pair.close, inner + 2..inner + 3);
    let outer = at(src, "[1");
    assert_eq!(
        tree.bracket_match(outer).expect("array").close,
        at(src, "])")..at(src, "])") + 1
    );
    // The cursor at the very end sits right after the last `}`; past the end there is nothing.
    assert_eq!(
        tree.bracket_match(src.len()).expect("last").open.start,
        at(src, "{ let")
    );
    assert_eq!(tree.bracket_match(src.len() + 5), None);
}

#[test]
fn brackets_in_other_languages() {
    let json = r#"{"a": [1, {"b": "}"}]}"#;
    let tree = parse(Language::Json, json);
    let pair = tree.bracket_match(0).expect("object");
    assert_eq!(pair.close, json.len() - 1..json.len());
    let inner = at(json, "{\"b");
    assert_eq!(
        tree.bracket_match(inner).expect("inner").close,
        at(json, "}]")..at(json, "}]") + 1
    );

    let py = "print((1, [2]))  # )";
    let tree = parse(Language::Python, py);
    let pair = tree.bracket_match(5).expect("call");
    assert_eq!(pair.close, 14..15);
}

fn fold_texts<'a>(src: &'a str, folds: &[Fold]) -> Vec<&'a str> {
    folds.iter().map(|f| &src[f.range.clone()]).collect()
}

#[test]
fn folds_hide_the_inside_of_brackets() {
    let src = "fn f() {\n    let a = 1;\n    let b = 2;\n}\n\nfn g() { 1 }\n";
    let tree = parse(Language::Rust, src);
    let folds = tree.folds(0..src.len());
    assert_eq!(
        fold_texts(src, &folds),
        vec!["\n    let a = 1;\n    let b = 2;\n"],
        "one multi-line block; the single-line one does not fold"
    );
    assert_eq!((folds[0].start_row, folds[0].end_row), (0, 3));
    // A range that does not touch the fold finds nothing.
    assert!(tree.folds(at(src, "fn g")..src.len()).is_empty());
}

#[test]
fn nested_folds_come_outermost_first() {
    let src = "mod m {\n    fn f() {\n        x();\n    }\n}\n";
    let tree = parse(Language::Rust, src);
    let folds = tree.folds(0..src.len());
    assert_eq!(folds.len(), 2);
    assert!(folds[0].range.start < folds[1].range.start);
    assert!(folds[1].range.end <= folds[0].range.end);
}

#[test]
fn python_folds_hide_the_body_below_its_header() {
    let src = "def f(x):\n    if x:\n        return 1\n    return 2\n\nz = 1\n";
    let tree = parse(Language::Python, src);
    let folds = tree.folds(0..src.len());
    assert_eq!(
        fold_texts(src, &folds),
        vec![
            "\n    if x:\n        return 1\n    return 2",
            "\n        return 1"
        ]
    );
    assert_eq!(folds[0].start_row, 0);
    assert_eq!(folds[1].start_row, 1);
}

#[test]
fn markdown_sections_fold_after_their_heading() {
    let src = "# One\n\ntext\n\n## Two\n\nmore\n";
    let tree = parse(Language::Markdown, src);
    let folds = tree.folds(0..src.len());
    assert!(folds.iter().any(|f| f.start_row == 0), "{folds:?}");
    assert!(folds.iter().any(|f| f.start_row == 4), "{folds:?}");
}

#[test]
fn crlf_text_folds_end_before_the_carriage_return() {
    let src = "def f(x):\r\n    return 1\r\n    return 2\r\n";
    let tree = parse(Language::Python, src);
    let folds = tree.folds(0..src.len());
    assert_eq!(
        fold_texts(src, &folds),
        vec!["\r\n    return 1\r\n    return 2"]
    );
    assert_eq!(folds[0].range.start, at(src, ":") + 1);
}

#[test]
fn indent_levels_follow_the_scopes() {
    let src =
        "fn main() {\n    if x {\n        y();\n    } else {\n        z();\n    }\n}\nfn k() {}\n";
    let tree = parse(Language::Rust, src);
    let levels: Vec<usize> = (0..8).map(|row| tree.indent_level(row)).collect();
    assert_eq!(levels, vec![0, 1, 2, 1, 2, 1, 0, 0]);
}

#[test]
fn indent_level_for_a_new_line() {
    let src = "fn main() {\n    foo();\n}\n";
    let tree = parse(Language::Rust, src);
    // After `foo();` and after `{`: inside the block. After the closing `}`: outside.
    assert_eq!(tree.indent_level_after(at(src, "foo();") + 6), 1);
    assert_eq!(tree.indent_level_after(at(src, "{\n") + 1), 1);
    assert_eq!(tree.indent_level_after(src.len() - 1), 0);
    assert_eq!(tree.indent_level_after(0), 0);
}

#[test]
fn indent_after_an_unclosed_brace() {
    let src = "fn main() {";
    let tree = parse(Language::Rust, src);
    assert_eq!(tree.indent_level_after(src.len()), 1);
}

#[test]
fn python_indentation() {
    let src = "class A:\n    def f(self):\n        if x:\n            a()\n        else:\n            b()\n        return 1\n";
    let tree = parse(Language::Python, src);
    let levels: Vec<usize> = (0..7).map(|row| tree.indent_level(row)).collect();
    assert_eq!(levels, vec![0, 1, 2, 3, 2, 3, 2]);
}

#[test]
fn json_indentation() {
    let src = "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}\n";
    let tree = parse(Language::Json, src);
    let levels: Vec<usize> = (0..7).map(|row| tree.indent_level(row)).collect();
    assert_eq!(levels, vec![0, 1, 2, 2, 1, 1, 0]);
}

#[test]
fn queries_degrade_on_broken_code_instead_of_panicking() {
    let src = "fn (( { let = ; ]] \"unterminated\n if else";
    let tree = parse(Language::Rust, src);
    assert!(tree.has_errors());
    for offset in 0..=src.len() {
        if !src.is_char_boundary(offset) {
            continue;
        }
        let _ = tree.node_at(offset..offset);
        let _ = tree.expand_selection(offset..offset);
        let _ = tree.shrink_selection(offset..offset);
        let _ = tree.bracket_match(offset);
        let _ = tree.indent_level_after(offset);
    }
    let _ = tree.folds(0..src.len());
    let _ = tree.highlights(0..src.len());
    for row in 0..6 {
        let _ = tree.indent_level(row);
    }
}

#[test]
fn indent_after_unfinished_code_in_other_languages() {
    let py = "def f(x):\nz = 1\n";
    let tree = parse(Language::Python, py);
    assert_eq!(tree.indent_level_after(at(py, "\n")), 1);
    assert_eq!(tree.indent_level_after(py.len()), 0);

    let js = "function f() {\n  if (x) {\n";
    let tree = parse(Language::JavaScript, js);
    // The cursor is at the end of the last line, before its line break.
    assert_eq!(tree.indent_level_after(js.len() - 1), 2);

    let rust = "fn main() {\n    let x = foo(\n";
    let tree = parse(Language::Rust, rust);
    assert_eq!(tree.indent_level_after(rust.len() - 1), 2);
}
