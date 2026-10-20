//! Shared strategies and helpers.

use proptest::prelude::*;

/// Proptest settings: 256 cases (`PROPTEST_CASES` overrides), no regression files in the source tree
/// (the failing input is printed).
pub fn config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

/// Pieces that exercise every coordinate system: ASCII, 2/3/4-byte characters (surrogate pairs),
/// combining marks, tabs, line breaks of both kinds and a lone `\r`.
const PIECES: &[&str] = &[
    "a",
    "b",
    "z",
    " ",
    "é",
    "日",
    "🦀",
    "👩‍👩‍👧",
    "e\u{301}",
    "\t",
    "\n",
    "\r\n",
    "\r",
    "\n",
    "x",
];

/// A text of up to `max` pieces.
pub fn text(max: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(PIECES), 0..=max).prop_map(|v| v.concat())
}

/// Like [`text`] without any `\r`, so that line endings are unambiguous.
pub fn text_without_cr(max: usize) -> impl Strategy<Value = String> {
    text(max).prop_map(|s| s.replace('\r', ""))
}

/// Every byte offset of `s` that is a character boundary (including `s.len()`).
pub fn boundaries(s: &str) -> Vec<usize> {
    let mut v: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
    v.push(s.len());
    v
}

/// Raw numbers that [`edits_for`] turns into valid edits for a given text.
#[derive(Clone, Debug)]
pub struct RawEdits {
    pub cuts: Vec<usize>,
    pub texts: Vec<String>,
}

pub fn raw_edits() -> impl Strategy<Value = RawEdits> {
    (
        proptest::collection::vec(any::<usize>(), 1..=6),
        proptest::collection::vec(text(4), 3),
    )
        .prop_map(|(cuts, texts)| RawEdits { cuts, texts })
}

/// Non-overlapping edits on character boundaries of `s`: the cut points are sorted and paired.
pub fn edits_for(s: &str, raw: &RawEdits) -> Vec<(std::ops::Range<usize>, String)> {
    let b = boundaries(s);
    let mut cuts: Vec<usize> = raw.cuts.iter().map(|c| b[c % b.len()]).collect();
    cuts.sort_unstable();
    cuts.chunks(2)
        .enumerate()
        .map(|(i, pair)| {
            let (start, end) = (pair[0], *pair.get(1).unwrap_or(&pair[0]));
            (start..end, raw.texts[i % raw.texts.len()].clone())
        })
        .collect()
}

/// Applies `edits` (coordinates of `s`) to a plain `String`.
pub fn model_apply(s: &str, edits: &[(std::ops::Range<usize>, String)]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for (r, t) in edits {
        out.push_str(&s[at..r.start]);
        out.push_str(t);
        at = r.end;
    }
    out.push_str(&s[at..]);
    out
}
