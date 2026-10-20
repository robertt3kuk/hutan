//! An incremental reparse after an edit equals a fresh parse of the edited text: same tree, same
//! highlights. 200 random edits per language, chained (each result seeds the next reparse).

use hutan_syntax::{Language, Snapshot, TextEdit};

use crate::common::{fixture, parse};

/// A small deterministic generator; the tests must replay exactly.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const FRAGMENTS: &[&str] = &[
    "x", "foo", " ", "  ", "\n", "\n\n", "\t", "(", ")", "{", "}", "[", "]", "<", ">", "\"", "'",
    "`", ";", ",", ".", ":", "=", "+", "-", "*", "/", "#", "//", "/*", "*/", "1", "42", "0x1f",
    "fn ", "let ", "if ", "else", "return ", "class ", "def ", "import ", "->", "=>", "::", "&&",
    "é", "日本", "😀", "\\n", "${", "<div>", "</div>", "**", "- ", "> ", "```",
];

/// A random valid edit of `text`: (range, replacement).
pub fn random_edit(rng: &mut Rng, text: &str) -> (std::ops::Range<usize>, String) {
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let at = |rng: &mut Rng| boundaries[rng.below(boundaries.len())];
    let fragment = |rng: &mut Rng| FRAGMENTS[rng.below(FRAGMENTS.len())].to_string();
    match rng.below(6) {
        // insertion
        0 | 1 => {
            let at = at(rng);
            (at..at, fragment(rng))
        }
        // deletion of a few characters
        2 => {
            let start = rng.below(boundaries.len());
            let end = (start + 1 + rng.below(8)).min(boundaries.len() - 1);
            (boundaries[start]..boundaries[end.max(start)], String::new())
        }
        // replacement
        3 => {
            let start = rng.below(boundaries.len());
            let end = (start + rng.below(6)).min(boundaries.len() - 1);
            (boundaries[start]..boundaries[end.max(start)], fragment(rng))
        }
        // delete a whole line
        4 => {
            let at = at(rng);
            let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
            (start..end, String::new())
        }
        // duplicate a line below itself
        _ => {
            let at = at(rng);
            let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
            let mut line = text[start..end].to_string();
            if !line.ends_with('\n') {
                line.insert(0, '\n');
            }
            (end..end, line)
        }
    }
}

/// An edit that keeps the text valid: one digit replaced by another.
fn benign_edit(rng: &mut Rng, text: &str) -> Option<(std::ops::Range<usize>, String)> {
    let digits: Vec<usize> = text
        .bytes()
        .enumerate()
        .filter(|(_, b)| b.is_ascii_digit())
        .map(|(i, _)| i)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let at = digits[rng.below(digits.len())];
    let digit = char::from(b'0' + rng.below(10) as u8);
    Some((at..at + 1, digit.to_string()))
}

/// Applies `edits` random steps and checks every incremental result against a fresh parse.
///
/// Edits are benign (a digit replaced) or garbage (random fragments). An edit that leaves syntax
/// errors is reverted by the next step, so the text keeps coming back to something that parses.
/// Where the edited text has no syntax errors the two trees and their highlights must be equal.
/// Where it has, tree-sitter documents that error recovery may differ between an incremental and
/// a fresh parse (both are valid recoveries), so only well-formedness is checked there. Returns
/// the number of error-free and erroneous steps.
/// Applies `range -> replacement` to `text`; returns the parse edit and the edit that undoes it.
pub fn apply(
    text: &mut String,
    range: std::ops::Range<usize>,
    replacement: &str,
) -> (TextEdit, (std::ops::Range<usize>, String)) {
    let edit = TextEdit::new(text.as_str(), range.clone(), replacement).expect("edit");
    let undo = (
        range.start..range.start + replacement.len(),
        text[range.clone()].to_string(),
    );
    text.replace_range(range, replacement);
    (edit, undo)
}

/// `pairs` random edits, each followed by the edit that undoes it, as (range, replacement) steps
/// to replay one after the other on `original`: the text is `original` again after every second
/// step.
pub fn script(
    rng: &mut Rng,
    original: &str,
    pairs: usize,
) -> Vec<(std::ops::Range<usize>, String)> {
    let mut text = original.to_string();
    let mut steps = Vec::new();
    for _ in 0..pairs {
        let (range, replacement) = random_edit(rng, &text);
        let (_, (undo_range, old)) = apply(&mut text, range.clone(), &replacement);
        steps.push((range, replacement));
        apply(&mut text, undo_range.clone(), &old);
        steps.push((undo_range, old));
    }
    steps
}

fn run(language: Language, edits: usize, seed: u64) -> (usize, usize) {
    let mut rng = Rng::new(seed);
    let mut text = fixture(language);
    let mut tree = parse(language, &text);
    let mut revert: Option<(std::ops::Range<usize>, String)> = None;
    let (mut valid, mut broken) = (0, 0);
    for step in 0..edits {
        let (range, replacement) = if let Some(undo) = revert.take() {
            undo
        } else if let Some(benign) = benign_edit(&mut rng, &text).filter(|_| rng.below(2) == 0) {
            benign
        } else {
            random_edit(&mut rng, &text)
        };
        let undo = (
            range.start..range.start + replacement.len(),
            text[range.clone()].to_string(),
        );
        let edit = TextEdit::new(text.as_str(), range.clone(), &replacement).expect("edit");
        text.replace_range(range.clone(), &replacement);
        let incremental = tree
            .reparse(&[edit], text.clone(), step as u64 + 1)
            .unwrap_or_else(|e| panic!("{language} step {step}: {e}"));
        let fresh = parse(language, &text);
        if fresh.has_errors() {
            broken += 1;
            revert = Some(undo);
            assert!(incremental.has_errors(), "{language} step {step}");
            for span in incremental.highlights(0..text.len()) {
                assert!(span.range.end <= text.len() && span.range.start < span.range.end);
            }
        } else {
            valid += 1;
            assert_eq!(
                incremental.to_sexp(),
                fresh.to_sexp(),
                "{language} step {step}: edit {range:?} -> {replacement:?} diverged"
            );
            assert_eq!(
                incremental.highlights(0..text.len()),
                fresh.highlights(0..text.len()),
                "{language} step {step}: highlights diverged after {range:?} -> {replacement:?}"
            );
        }
        tree = incremental;
    }
    (valid, broken)
}

/// `SYNTAX_FUZZ_SEEDS=50 cargo nextest run -p hutan-syntax -E 'test(random_edits)'` replays more
/// random histories than the gate does (one per language).
#[test]
fn two_hundred_random_edits_equal_a_fresh_parse() {
    let seeds: u64 = std::env::var("SYNTAX_FUZZ_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    for language in Language::compiled() {
        let (mut valid, mut broken) = (0, 0);
        for seed in 0..seeds {
            let (v, b) = run(language, 200, 0xC0FFEE ^ language as u64 ^ (seed << 32));
            valid += v;
            broken += b;
        }
        // The comparison is only worth something if many steps are error-free.
        assert!(
            valid * 4 >= (valid + broken),
            "{language}: only {valid} of {} steps parsed without errors",
            valid + broken
        );
    }
}

/// Several edits per reparse. Each round applies a batch of random edits (checked against a fresh
/// parse where the result has no syntax errors) and then the batch's undo as one more batch, which
/// always lands on the valid text the round started from.
#[test]
fn batched_edits_equal_a_fresh_parse() {
    for language in Language::compiled() {
        let mut rng = Rng::new(7 + language as u64);
        let mut text = fixture(language);
        let mut tree = parse(language, &text);
        let mut version = 0;
        for round in 0..30 {
            let (mut edits, mut undo) = (Vec::new(), Vec::new());
            for _ in 0..1 + rng.below(4) {
                let (range, replacement) = random_edit(&mut rng, &text);
                let (edit, inverse) = apply(&mut text, range, &replacement);
                edits.push(edit);
                undo.push(inverse);
            }
            version += 1;
            tree = tree
                .reparse(&edits, text.clone(), version)
                .expect("reparse");
            let fresh = parse(language, &text);
            if !fresh.has_errors() {
                assert_eq!(tree.to_sexp(), fresh.to_sexp(), "{language} round {round}");
            }

            let mut edits = Vec::new();
            for (range, old) in undo.into_iter().rev() {
                let (edit, _) = apply(&mut text, range, &old);
                edits.push(edit);
            }
            version += 1;
            tree = tree
                .reparse(&edits, text.clone(), version)
                .expect("reparse");
            assert_eq!(text, fixture(language));
            assert_eq!(
                tree.to_sexp(),
                parse(language, &text).to_sexp(),
                "{language} round {round}: the undo of a batch diverged"
            );
        }
    }
}

#[test]
fn edits_that_empty_and_refill_the_text() {
    for language in Language::compiled() {
        let original = fixture(language);
        let tree = parse(language, &original);
        let all = TextEdit::new(original.as_str(), 0..original.len(), "").expect("edit");
        let empty = tree.reparse(&[all], String::new(), 1).expect("empty");
        assert_eq!(empty.to_sexp(), parse(language, "").to_sexp(), "{language}");
        let refill = TextEdit::new("", 0..0, &original).expect("edit");
        let back = empty
            .reparse(&[refill], original.clone(), 2)
            .expect("refill");
        assert_eq!(back.to_sexp(), tree.to_sexp(), "{language}");
    }
}

#[allow(dead_code)]
fn _assert_snapshot_is_send_sync(s: Snapshot) -> impl Send + Sync {
    s
}
