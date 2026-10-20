use hutan_buffer::{Anchor, Bias, Buffer, Edit, Selection, Selections, Transaction};
use proptest::prelude::*;

use crate::common::{boundaries, config, edits_for, model_apply, raw_edits, text};

proptest! {
    #![proptest_config(config())]

    /// An anchor that no edit touches stays on the same text: the characters around it are the
    /// same and its offset moves by the length changes before it. One touched by an edit lands
    /// within the replacement, on the side its bias says.
    #[test]
    fn anchors_stay_on_the_same_text(
        init in text(40),
        picks in proptest::collection::vec((any::<usize>(), any::<bool>()), 1..10),
        ops in proptest::collection::vec(raw_edits(), 1..10),
    ) {
        let mut buffer = Buffer::from_content(&init);
        let mut model = buffer.text().to_string();
        let b = boundaries(&model);
        let ids: Vec<_> = picks
            .iter()
            .map(|(p, right)| buffer.track_anchor(Anchor::new(b[p % b.len()], if *right { Bias::Right } else { Bias::Left })).unwrap())
            .collect();
        for raw in ops {
            let edits = edits_for(&model, &raw);
            let tx = Transaction::new(edits.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
            let next = model_apply(&model, &edits);
            let before: Vec<Anchor> = ids.iter().map(|id| buffer.anchor(*id).unwrap()).collect();
            buffer.apply(tx.clone()).unwrap();
            let changes = tx.changes();
            for (id, old) in ids.iter().zip(&before) {
                let now = buffer.anchor(*id).unwrap();
                prop_assert_eq!(now.bias, old.bias);
                prop_assert!(next.is_char_boundary(now.offset));
                // Closed-range test: does any edit touch the anchor?
                let touching: Vec<_> = changes.iter().filter(|c| c.old.start <= old.offset && old.offset <= c.old.end).collect();
                if touching.is_empty() {
                    let delta: isize = changes.iter().filter(|c| c.old.end < old.offset).map(|c| c.new.len() as isize - c.old.len() as isize).sum();
                    prop_assert_eq!(now.offset as isize, old.offset as isize + delta);
                    prop_assert_eq!(next[now.offset..].chars().next(), model[old.offset..].chars().next());
                    prop_assert_eq!(next[..now.offset].chars().next_back(), model[..old.offset].chars().next_back());
                } else {
                    let first = touching[0];
                    let last = touching[touching.len() - 1];
                    match old.bias {
                        Bias::Left => prop_assert_eq!(now.offset, first.new.start),
                        Bias::Right => prop_assert_eq!(now.offset, last.new.end),
                    }
                }
            }
            model = next;
        }
        prop_assert_eq!(buffer.text().to_string(), model);
    }

    /// Anchors stay valid offsets through undo and redo, and an anchor away from the edit is back
    /// where it started after an undo.
    #[test]
    fn anchors_survive_undo_and_redo(init in text(40), pick in any::<usize>(), ops in proptest::collection::vec(raw_edits(), 1..6)) {
        let mut buffer = Buffer::from_content(&init);
        let mut model = buffer.text().to_string();
        let b = boundaries(&model);
        let start = b[pick % b.len()];
        let id = buffer.track_anchor(Anchor::left(start)).unwrap();
        let mut untouched = true;
        for raw in ops {
            let edits = edits_for(&model, &raw);
            let anchor_now = buffer.anchor(id).unwrap().offset;
            untouched &= edits.iter().all(|(r, _)| !(r.start <= anchor_now && anchor_now <= r.end));
            let tx = Transaction::new(edits.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
            buffer.apply(tx).unwrap();
            model = model_apply(&model, &edits);
        }
        let text_now = buffer.text().to_string();
        prop_assert!(text_now.is_char_boundary(buffer.anchor(id).unwrap().offset));
        while buffer.undo().is_some() {
            prop_assert!(buffer.text().is_char_boundary(buffer.anchor(id).unwrap().offset));
        }
        if untouched {
            prop_assert_eq!(buffer.anchor(id).unwrap().offset, start);
        }
        while buffer.redo().is_some() {
            prop_assert!(buffer.text().is_char_boundary(buffer.anchor(id).unwrap().offset));
        }
    }

    /// Mapped selections stay ordered, in bounds and on boundaries.
    #[test]
    fn selections_stay_valid_through_edits(
        init in text(40),
        picks in proptest::collection::vec((any::<usize>(), any::<usize>()), 1..6),
        raw in raw_edits(),
    ) {
        let model = Buffer::from_content(&init).text().to_string();
        let b = boundaries(&model);
        let ranges: Vec<Selection> = picks.iter().map(|(a, h)| Selection::new(b[a % b.len()], b[h % b.len()])).collect();
        let sel = Selections::new(ranges, 0).unwrap();
        let edits = edits_for(&model, &raw);
        let tx = Transaction::new(edits.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
        let next = tx.apply_to_string(&model).unwrap();
        let mapped = sel.map(&tx);
        let text = hutan_buffer::Text::from(next.as_str());
        prop_assert!(mapped.is_valid_for(&text));
        let mut prev_end = 0;
        for r in mapped.ranges() {
            prop_assert!(r.start() >= prev_end);
            prop_assert!(r.end() <= next.len());
            prev_end = r.end();
        }
        prop_assert!(mapped.primary_index() < mapped.len());
    }
}

#[test]
fn bias_decides_which_side_of_an_insertion_an_anchor_sticks_to() {
    let mut buffer = Buffer::from_content("hello");
    let left = buffer.track_anchor(Anchor::left(2)).unwrap();
    let right = buffer.track_anchor(Anchor::right(2)).unwrap();
    buffer.insert(2, "XX").unwrap();
    assert_eq!(buffer.anchor(left).unwrap().offset, 2);
    assert_eq!(buffer.anchor(right).unwrap().offset, 4);
    buffer.undo().unwrap();
    assert_eq!(buffer.anchor(left).unwrap().offset, 2);
    assert_eq!(buffer.anchor(right).unwrap().offset, 2);
}

#[test]
fn anchors_are_bounded_and_validated() {
    let mut buffer = Buffer::from_content("é");
    assert!(buffer.track_anchor(Anchor::left(1)).is_err());
    assert!(buffer.track_anchor(Anchor::left(3)).is_err());
    let id = buffer.track_anchor(Anchor::left(2)).unwrap();
    assert_eq!(buffer.tracked_anchors(), 1);
    assert_eq!(buffer.untrack_anchor(id), Some(Anchor::left(2)));
    assert_eq!(buffer.anchor(id), None);
}
