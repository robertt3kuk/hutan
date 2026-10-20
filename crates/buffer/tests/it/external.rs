use hutan_buffer::{Anchor, Bias, Buffer, LineEnding};
use proptest::prelude::*;

use crate::common::{boundaries, config, text};

/// What happens to each line of the old text.
#[derive(Clone, Debug)]
enum Fate {
    Keep,
    Delete,
    Modify(String),
}

#[derive(Clone, Debug)]
struct LineSpec {
    body: String,
    fate: Fate,
    insert_after: Vec<String>,
    anchor_at: usize,
    right: bool,
}

fn line_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        proptest::sample::select(&["a", "é", "日", "🦀", "e\u{301}", " ", "x", "-"][..]),
        0..8,
    )
    .prop_map(|v| v.concat())
}

fn line_spec() -> impl Strategy<Value = LineSpec> {
    (
        line_text(),
        prop_oneof![3 => Just(Fate::Keep), 1 => Just(Fate::Delete), 2 => line_text().prop_map(Fate::Modify)],
        proptest::collection::vec(line_text(), 0..3),
        any::<usize>(),
        any::<bool>(),
    )
        .prop_map(|(body, fate, insert_after, anchor_at, right)| LineSpec { body, fate, insert_after, anchor_at, right })
}

proptest! {
    #![proptest_config(config())]

    /// Agent edits arrive as one transaction; anchors inside lines the agent did not touch stay
    /// on exactly the same text, wherever the other lines were added, removed or rewritten.
    #[test]
    fn anchors_in_unchanged_lines_stay_put(lines in proptest::collection::vec(line_spec(), 1..14)) {
        // Unique line identities make the expected alignment unambiguous.
        let mut old = String::new();
        let mut new = String::new();
        let mut expectations: Vec<(usize, usize, Bias)> = Vec::new(); // (old offset, new offset, bias)
        for (i, spec) in lines.iter().enumerate() {
            let line = format!("L{i}:{}", spec.body);
            let old_start = old.len();
            old.push_str(&line);
            old.push('\n');
            match &spec.fate {
                Fate::Keep => {
                    // Inside the line; a left-biased anchor at the very start would stick to the
                    // end of the previous (possibly rewritten) line, which is its contract.
                    let b: Vec<usize> = boundaries(&line);
                    let k = if spec.right { b[spec.anchor_at % b.len()] } else { b[1 + spec.anchor_at % (b.len() - 1)] };
                    expectations.push((old_start + k, new.len() + k, if spec.right { Bias::Right } else { Bias::Left }));
                    new.push_str(&line);
                    new.push('\n');
                }
                Fate::Delete => {}
                Fate::Modify(t) => {
                    new.push_str(&format!("M{i}:{t}\n"));
                }
            }
            for (j, ins) in spec.insert_after.iter().enumerate() {
                new.push_str(&format!("N{i}.{j}:{ins}\n"));
            }
        }

        let mut buffer = Buffer::from_content(&old);
        let ids: Vec<_> = expectations.iter().map(|(o, _, bias)| buffer.track_anchor(Anchor::new(*o, *bias)).unwrap()).collect();
        let version = buffer.version();
        let applied = buffer.apply_external(&new).unwrap();
        prop_assert_eq!(buffer.text().to_string(), new.clone());
        if old == new {
            prop_assert!(applied.is_none());
            prop_assert_eq!(buffer.version(), version);
        } else {
            // one transaction, one version step, one undo step
            let applied = applied.unwrap();
            prop_assert_eq!(buffer.version(), version + 1);
            prop_assert_eq!(applied.transaction.apply_to_string(&old).unwrap(), new.clone());
            prop_assert_eq!(buffer.history_groups(), 1);
        }
        for (id, (_, expected, _)) in ids.iter().zip(&expectations) {
            prop_assert_eq!(buffer.anchor(*id).unwrap().offset, *expected);
        }
        if old != new {
            buffer.undo().unwrap();
            prop_assert_eq!(buffer.text().to_string(), old);
            prop_assert!(!buffer.can_undo());
        }
    }

    /// For arbitrary old and new texts the result is exact and one undo step away from the old
    /// text, and anchors in the common leading and trailing *lines* keep their text (left-biased
    /// ones in the leading lines stay, right-biased ones in the trailing lines shift by the length
    /// change).
    #[test]
    fn external_changes_are_exact_and_keep_common_lines(old in text(60), new in text(60), pa in any::<usize>(), pb in any::<usize>()) {
        let mut buffer = Buffer::from_content(&old);
        let old = buffer.text().to_string();
        let target = Buffer::from_content(&new).text().to_string();
        let a_lines: Vec<&str> = old.split_inclusive('\n').collect();
        let b_lines: Vec<&str> = target.split_inclusive('\n').collect();
        let p = a_lines.iter().zip(&b_lines).take_while(|(a, b)| a == b).count();
        let max_s = a_lines.len().min(b_lines.len()) - p;
        let s = a_lines.iter().rev().zip(b_lines.iter().rev()).take(max_s).take_while(|(a, b)| a == b).count();
        let prefix_end: usize = a_lines[..p].iter().map(|l| l.len()).sum();
        let suffix_start: usize = old.len() - a_lines[a_lines.len() - s..].iter().map(|l| l.len()).sum::<usize>();

        let pre: Vec<usize> = boundaries(&old).into_iter().filter(|b| *b <= prefix_end).collect();
        let suf: Vec<usize> = boundaries(&old).into_iter().filter(|b| *b >= suffix_start).collect();
        let in_prefix = buffer.track_anchor(Anchor::left(pre[pa % pre.len()])).unwrap();
        let in_suffix = buffer.track_anchor(Anchor::right(suf[pb % suf.len()])).unwrap();
        let (prefix_from, suffix_from) = (buffer.anchor(in_prefix).unwrap().offset, buffer.anchor(in_suffix).unwrap().offset);

        let version = buffer.version();
        let applied = buffer.apply_external(&new).unwrap();
        prop_assert_eq!(buffer.text().to_string(), target.clone());
        prop_assert_eq!(applied.is_none(), old == target);
        if let Some(applied) = applied {
            prop_assert_eq!(buffer.version(), version + 1);
            prop_assert_eq!(applied.transaction.apply_to_string(&old).unwrap(), target.clone());
            buffer.undo().unwrap();
            prop_assert_eq!(buffer.text().to_string(), old.clone());
            buffer.redo().unwrap();
        }
        prop_assert_eq!(buffer.anchor(in_prefix).unwrap().offset, prefix_from);
        let now = buffer.anchor(in_suffix).unwrap().offset;
        prop_assert_eq!(now as isize - suffix_from as isize, target.len() as isize - old.len() as isize);
        prop_assert_eq!(&target[now..], &old[suffix_from..]);
    }
}

#[test]
fn external_crlf_and_bom_follow_the_new_content() {
    let mut buffer = Buffer::from_content("a\nb\n");
    let applied = buffer
        .apply_external("\u{feff}a\r\nB\r\n")
        .unwrap()
        .unwrap();
    assert_eq!(buffer.text().to_string(), "a\nB\n");
    assert_eq!(buffer.line_ending(), LineEnding::CrLf);
    assert!(buffer.has_bom());
    assert_eq!(applied.transaction.edits().len(), 1);
    assert_eq!(buffer.to_bytes(), b"\xEF\xBB\xBFa\r\nB\r\n");
    // Same content again: nothing to do.
    assert!(
        buffer
            .apply_external("\u{feff}a\r\nB\r\n")
            .unwrap()
            .is_none()
    );
}

#[test]
fn external_change_is_its_own_undo_step_and_never_joins_a_group() {
    let mut buffer = Buffer::from_content("one\ntwo\n");
    buffer.begin_group();
    buffer.insert(0, "x").unwrap();
    buffer
        .apply_external("xone\ntwo\nthree\n")
        .unwrap()
        .unwrap();
    buffer.insert(0, "y").unwrap();
    buffer.end_group();
    assert_eq!(buffer.text().to_string(), "yxone\ntwo\nthree\n");
    assert_eq!(buffer.history_groups(), 3);
    buffer.undo().unwrap(); // the typing after the external change
    assert_eq!(buffer.text().to_string(), "xone\ntwo\nthree\n");
    buffer.undo().unwrap(); // the external change alone
    assert_eq!(buffer.text().to_string(), "xone\ntwo\n");
}

#[test]
fn reloading_a_clean_buffer_can_be_marked_saved() {
    let mut buffer = Buffer::from_content("a\n");
    buffer.apply_external("a\nb\n").unwrap();
    assert!(buffer.is_dirty());
    buffer.mark_saved();
    assert!(!buffer.is_dirty());
}

#[test]
fn a_big_file_with_one_changed_line_is_one_small_edit() {
    let mut old = String::new();
    for i in 0..50_000 {
        old.push_str(&format!("let value_{i} = {i};\n"));
    }
    let new = old.replace("value_25000 = 25000", "value_25000 = 25001");
    let mut buffer = Buffer::from_content(&old);
    let applied = buffer.apply_external(&new).unwrap().unwrap();
    assert_eq!(applied.transaction.edits().len(), 1);
    assert_eq!(applied.transaction.edits()[0].range.len(), 1);
    assert_eq!(buffer.text().to_string(), new);
}
