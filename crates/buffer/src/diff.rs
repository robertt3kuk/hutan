//! Minimal edits between two texts, for [`Buffer::apply_external`](crate::Buffer::apply_external).

use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp, DiffTag, capture_diff_slices_deadline};

use crate::edit::Edit;

/// A changed line range is refined to characters only up to this many bytes per side.
const CHAR_REFINE_MAX: usize = 8 * 1024;
/// Time the diff may take; past it `similar` returns a correct, less minimal script.
const DEADLINE: Duration = Duration::from_millis(500);

/// Edits that turn `old` into `new`.
///
/// Whole lines are compared first: the common leading and trailing lines are kept, the lines in
/// between are diffed (Myers), and every changed line range is narrowed to its differing bytes
/// (and, when small, to the differing characters). Text of an unchanged line is therefore never
/// part of an edit, whatever characters it shares with the lines around it. Adjacent edits are
/// merged.
pub(crate) fn minimal_edits(old: &str, new: &str) -> Vec<Edit> {
    let deadline = Instant::now().checked_add(DEADLINE);
    let a_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let b_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = a_lines
        .iter()
        .zip(&b_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = a_lines.len().min(b_lines.len()) - prefix;
    let suffix = a_lines
        .iter()
        .rev()
        .zip(b_lines.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    let a_mid = &a_lines[prefix..a_lines.len() - suffix];
    let b_mid = &b_lines[prefix..b_lines.len() - suffix];
    let a_starts = line_starts(&a_lines);
    let b_starts = line_starts(&b_lines);

    let mut out = Vec::new();
    for op in capture_diff_slices_deadline(Algorithm::Myers, a_mid, b_mid, deadline) {
        let (tag, old_lines, new_lines) = op_ranges(&op);
        if tag == DiffTag::Equal {
            continue;
        }
        let (a0, a1) = (
            a_starts[prefix + old_lines.start],
            a_starts[prefix + old_lines.end],
        );
        let (b0, b1) = (
            b_starts[prefix + new_lines.start],
            b_starts[prefix + new_lines.end],
        );
        let (ha, hb) = (&old[a0..a1], &new[b0..b1]);
        if tag == DiffTag::Replace {
            narrow(&mut out, a0, ha, hb, deadline);
        } else {
            push(&mut out, a0, ha, hb);
        }
    }
    out
}

/// A replaced line range: keep its common leading and trailing bytes out of the edit, then refine
/// what is left by characters when it is small.
fn narrow(out: &mut Vec<Edit>, base: usize, a: &str, b: &str, deadline: Option<Instant>) {
    let mut prefix = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while !a.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .bytes()
        .rev()
        .zip(b.bytes().rev())
        .take(max_suffix)
        .take_while(|(x, y)| x == y)
        .count();
    while !a.is_char_boundary(a.len() - suffix) || !b.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    let (ma, mb) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    if !ma.is_empty()
        && !mb.is_empty()
        && ma.len() <= CHAR_REFINE_MAX
        && mb.len() <= CHAR_REFINE_MAX
    {
        refine(out, base + prefix, ma, mb, deadline);
    } else {
        push(out, base + prefix, ma, mb);
    }
}

fn op_ranges(op: &DiffOp) -> (DiffTag, std::ops::Range<usize>, std::ops::Range<usize>) {
    op.as_tag_tuple()
}

/// Byte offset of every line start, plus the total length.
fn line_starts(lines: &[&str]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len() + 1);
    let mut at = 0;
    starts.push(0);
    for l in lines {
        at += l.len();
        starts.push(at);
    }
    starts
}

/// Character-level diff of `a` against `b`, `base` being the offset of `a` in the old text.
fn refine(out: &mut Vec<Edit>, base: usize, a: &str, b: &str, deadline: Option<Instant>) {
    let (a_chars, a_pos) = chars_and_offsets(a);
    let (b_chars, b_pos) = chars_and_offsets(b);
    for op in capture_diff_slices_deadline(Algorithm::Myers, &a_chars, &b_chars, deadline) {
        let (tag, old, new) = op_ranges(&op);
        if tag == DiffTag::Equal {
            continue;
        }
        let (a0, a1) = (a_pos[old.start], a_pos[old.end]);
        let (b0, b1) = (b_pos[new.start], b_pos[new.end]);
        push(out, base + a0, &a[a0..a1], &b[b0..b1]);
    }
}

fn chars_and_offsets(s: &str) -> (Vec<char>, Vec<usize>) {
    let mut chars = Vec::with_capacity(s.len());
    let mut offsets = Vec::with_capacity(s.len() + 1);
    for (i, c) in s.char_indices() {
        chars.push(c);
        offsets.push(i);
    }
    offsets.push(s.len());
    (chars, offsets)
}

/// Appends "replace `deleted` (which starts at `at` in the old text) with `inserted`", merging it
/// into the previous edit when they touch.
fn push(out: &mut Vec<Edit>, at: usize, deleted: &str, inserted: &str) {
    if deleted.is_empty() && inserted.is_empty() {
        return;
    }
    let end = at + deleted.len();
    if let Some(prev) = out.last_mut()
        && prev.range.end == at
    {
        let mut text = String::with_capacity(prev.text.len() + inserted.len());
        text.push_str(&prev.text);
        text.push_str(inserted);
        *prev = Edit::new(prev.range.start..end, text);
        return;
    }
    out.push(Edit::new(at..end, inserted));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, edits: &[Edit]) -> String {
        let mut out = String::new();
        let mut at = 0;
        for e in edits {
            out.push_str(&old[at..e.range.start]);
            out.push_str(&e.text);
            at = e.range.end;
        }
        out.push_str(&old[at..]);
        out
    }

    #[test]
    fn single_change_is_one_small_edit() {
        let old = "fn main() {\n    println!(\"hi\");\n}\n";
        let new = "fn main() {\n    println!(\"hello\");\n}\n";
        let edits = minimal_edits(old, new);
        assert_eq!(apply(old, &edits), new);
        assert_eq!(edits.len(), 1);
        assert_eq!(&*edits[0].text, "ello");
        assert_eq!(edits[0].range.len(), 1);
    }

    #[test]
    fn identical_texts_need_no_edit() {
        assert!(minimal_edits("same\n", "same\n").is_empty());
        assert!(minimal_edits("", "").is_empty());
    }

    #[test]
    fn line_inserts_and_deletes_keep_the_rest_in_place() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nc\nd\nX\nY\ne\n";
        let edits = minimal_edits(old, new);
        assert_eq!(apply(old, &edits), new);
        // "b\n" deleted and "X\nY\n" inserted; "a", "c", "d", "e" untouched
        assert!(
            edits
                .iter()
                .all(|e| !old[e.range.clone()].contains(['a', 'c', 'e']))
        );
    }

    #[test]
    fn multibyte_boundaries_hold() {
        let old = "é🦀日本\nzé";
        let new = "ã🦀日本語\nzè";
        let edits = minimal_edits(old, new);
        assert_eq!(apply(old, &edits), new);
    }
}
