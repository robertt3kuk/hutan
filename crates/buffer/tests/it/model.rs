use hutan_buffer::{Buffer, Edit, Text, Transaction};
use proptest::prelude::*;

use crate::common::{config, edits_for, model_apply, raw_edits, text};

fn assert_same(buffer: &Buffer, model: &str) {
    let t = buffer.text();
    assert!(*t == *model, "text differs from the model");
    assert_eq!(t.len(), model.len());
    assert_eq!(t.utf16_len(), model.encode_utf16().count());
    assert_eq!(t.line_count(), model.matches('\n').count() + 1);
    assert_eq!(t.to_string(), model);
    assert_eq!(t.char_count(), model.chars().count());
}

proptest! {
    #![proptest_config(config())]

    /// Random edit sequences (several non-overlapping edits per transaction) equal a `String`.
    #[test]
    fn random_edit_sequences_equal_a_string_model(init in text(40), ops in proptest::collection::vec(raw_edits(), 0..25)) {
        let mut buffer = Buffer::new();
        buffer.insert(0, init.as_str()).unwrap();
        let mut model = init.clone();
        assert_same(&buffer, &model);
        for raw in ops {
            let edits = edits_for(&model, &raw);
            let before = buffer.version();
            let tx = Transaction::new(edits.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
            let changed = !tx.is_empty();
            let applied = buffer.apply(tx).unwrap();
            model = model_apply(&model, &edits);
            assert_same(&buffer, &model);
            prop_assert_eq!(buffer.version(), before + u64::from(changed));
            prop_assert_eq!(applied.version, buffer.version());
        }
    }

    /// An invalid transaction changes nothing.
    #[test]
    fn invalid_transactions_are_atomic(init in text(30), raw in raw_edits()) {
        let mut buffer = Buffer::new();
        buffer.insert(0, init.as_str()).unwrap();
        let version = buffer.version();
        let mut edits: Vec<Edit> = edits_for(&init, &raw).into_iter().map(|(r, t)| Edit::new(r, t)).collect();
        // One edit past the end makes the whole transaction invalid.
        edits.push(Edit::new(init.len() + 1..init.len() + 2, "!"));
        let tx = Transaction::new(edits).unwrap();
        prop_assert!(buffer.apply(tx).is_err());
        prop_assert_eq!(buffer.version(), version);
        assert_same(&buffer, &init);
    }

    /// `invert` undoes a transaction and `compose` equals applying both in turn.
    #[test]
    fn invert_and_compose(init in text(40), a in raw_edits(), b in raw_edits()) {
        let ea = edits_for(&init, &a);
        let ta = Transaction::new(ea.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
        let mid = model_apply(&init, &ea);
        let eb = edits_for(&mid, &b);
        let tb = Transaction::new(eb.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
        let end = model_apply(&mid, &eb);

        let inv = ta.invert(&Text::from(init.as_str())).unwrap();
        prop_assert_eq!(inv.apply_to_string(&mid).unwrap(), init.clone());

        let both = ta.compose(&tb).unwrap();
        prop_assert_eq!(both.apply_to_string(&init).unwrap(), end.clone());
        prop_assert_eq!(both.len_delta(), end.len() as isize - init.len() as isize);

        // The composed inverse equals inverting the composition.
        let inv_both = both.invert(&Text::from(init.as_str())).unwrap();
        prop_assert_eq!(inv_both.apply_to_string(&end).unwrap(), init.clone());
        let inv_b = tb.invert(&Text::from(mid.as_str())).unwrap();
        let chained = inv_b.compose(&inv).unwrap();
        prop_assert_eq!(chained.apply_to_string(&end).unwrap(), init);
    }

    /// The old/new ranges of `changes` describe the transaction exactly.
    #[test]
    fn changes_describe_the_edit(init in text(40), raw in raw_edits()) {
        let edits = edits_for(&init, &raw);
        let tx = Transaction::new(edits.iter().map(|(r, t)| Edit::new(r.clone(), t.as_str()))).unwrap();
        let after = tx.apply_to_string(&init).unwrap();
        for c in tx.changes() {
            prop_assert!(init.is_char_boundary(c.old.start) && init.is_char_boundary(c.old.end));
            prop_assert!(after.is_char_boundary(c.new.start) && after.is_char_boundary(c.new.end));
            // Text before the change is untouched by the earlier edits' shift only.
            prop_assert!(c.new.end <= after.len());
        }
        // Everything between changes is the same text.
        let changes = tx.changes();
        let mut old_at = 0;
        let mut new_at = 0;
        for c in &changes {
            prop_assert_eq!(&init[old_at..c.old.start], &after[new_at..c.new.start]);
            old_at = c.old.end;
            new_at = c.new.end;
        }
        prop_assert_eq!(&init[old_at..], &after[new_at..]);
    }
}
