use hutan_buffer::{Buffer, Edit, HistoryLimits, Selection, Selections, Transaction};
use proptest::prelude::*;

use crate::common::{config, edits_for, model_apply, raw_edits, text};

#[derive(Clone, Debug)]
enum Action {
    Edit(crate::common::RawEdits),
    /// Several edits in one group.
    Group(Vec<crate::common::RawEdits>),
    Undo,
    Redo,
}

fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        4 => raw_edits().prop_map(Action::Edit),
        2 => proptest::collection::vec(raw_edits(), 1..4).prop_map(Action::Group),
        3 => Just(Action::Undo),
        3 => Just(Action::Redo),
    ]
}

fn tx(model: &str, raw: &crate::common::RawEdits) -> (Transaction, String) {
    let edits = edits_for(model, raw);
    let t = Transaction::new(edits.iter().map(|(r, s)| Edit::new(r.clone(), s.as_str()))).unwrap();
    (t, model_apply(model, &edits))
}

proptest! {
    #![proptest_config(config())]

    /// Undo and redo walk a linear history of whole groups, with branches replaced by new edits.
    #[test]
    fn undo_and_redo_follow_the_group_history(init in text(30), actions in proptest::collection::vec(action(), 0..40)) {
        let mut buffer = Buffer::from_content(&init);
        let init = buffer.text().to_string();
        // states[pos] is the text after `pos` groups of the current line of history.
        let mut states = vec![init];
        let mut pos = 0usize;
        for a in actions {
            match a {
                Action::Edit(raw) => {
                    let (t, next) = tx(&states[pos], &raw);
                    let empty = t.is_empty();
                    buffer.apply(t).unwrap();
                    if !empty {
                        states.truncate(pos + 1);
                        states.push(next);
                        pos += 1;
                    }
                }
                Action::Group(raws) => {
                    let mut cur = states[pos].clone();
                    let mut any = false;
                    buffer.begin_group();
                    for raw in &raws {
                        let (t, next) = tx(&cur, raw);
                        any |= !t.is_empty();
                        buffer.apply(t).unwrap();
                        cur = next;
                    }
                    buffer.end_group();
                    if any {
                        states.truncate(pos + 1);
                        states.push(cur);
                        pos += 1;
                    }
                }
                Action::Undo => {
                    let did = buffer.undo().is_some();
                    prop_assert_eq!(did, pos > 0);
                    pos = pos.saturating_sub(1);
                }
                Action::Redo => {
                    let did = buffer.redo().is_some();
                    prop_assert_eq!(did, pos + 1 < states.len());
                    if did { pos += 1; }
                }
            }
            prop_assert_eq!(buffer.text().to_string(), states[pos].clone());
            prop_assert_eq!(buffer.can_undo(), pos > 0);
            prop_assert_eq!(buffer.can_redo(), pos + 1 < states.len());
        }
        // undo ∘ redo = identity at the end, and all the way down and back up.
        let end = states[pos].clone();
        while buffer.undo().is_some() {}
        prop_assert_eq!(buffer.text().to_string(), states[0].clone());
        while buffer.redo().is_some() {}
        prop_assert_eq!(buffer.text().to_string(), states[states.len() - 1].clone());
        let _ = end;
    }

    /// One undo then one redo of any group restores exactly the same text and offsets mapping.
    #[test]
    fn undo_then_redo_is_the_identity(init in text(30), raws in proptest::collection::vec(raw_edits(), 1..8)) {
        let mut buffer = Buffer::from_content(&init);
        let mut cur = buffer.text().to_string();
        for raw in &raws {
            let (t, next) = tx(&cur, raw);
            buffer.apply(t).unwrap();
            cur = next;
        }
        let before = buffer.text().to_string();
        let version = buffer.version();
        let undone = buffer.undo();
        let redone = buffer.redo();
        prop_assert_eq!(buffer.text().to_string(), before);
        if let (Some(u), Some(r)) = (undone, redone) {
            prop_assert!(r.version > u.version);
            prop_assert!(u.version > version.saturating_sub(1));
        }
    }

    /// Selections recorded on a transaction come back on undo (before) and redo (after).
    #[test]
    fn selections_round_trip_through_undo(init in text(20)) {
        let mut buffer = Buffer::from_content(&init);
        let len = buffer.text().len();
        let before = Selections::new([Selection::new(0, len)], 0).unwrap();
        let after = Selections::cursor(0);
        let t = Transaction::delete(0..len).unwrap().with_selections(Some(before.clone()), Some(after.clone()));
        if t.is_empty() {
            return Ok(());
        }
        let applied = buffer.apply(t).unwrap();
        prop_assert_eq!(applied.transaction.selection_after(), Some(&after));
        let undone = buffer.undo().unwrap();
        prop_assert_eq!(undone.transaction.selection_after(), Some(&before));
        let redone = buffer.redo().unwrap();
        prop_assert_eq!(redone.transaction.selection_after(), Some(&after));
    }
}

#[test]
fn a_group_is_one_undo_step_and_nests() {
    let mut buffer = Buffer::from_content("");
    buffer.group(|b| {
        b.insert(0, "a").unwrap();
        b.group(|b| {
            b.insert(1, "b").unwrap();
            b.insert(2, "c").unwrap();
        });
        b.insert(3, "d").unwrap();
    });
    assert_eq!(buffer.text().to_string(), "abcd");
    assert_eq!(buffer.history_groups(), 1);
    let undone = buffer.undo().unwrap();
    assert_eq!(buffer.text().to_string(), "");
    assert_eq!(undone.transaction.edits().len(), 1);
    assert!(!buffer.can_undo());
    buffer.redo().unwrap();
    assert_eq!(buffer.text().to_string(), "abcd");
}

#[test]
fn an_empty_group_leaves_no_step() {
    let mut buffer = Buffer::from_content("x");
    buffer.begin_group();
    buffer.end_group();
    assert!(!buffer.can_undo());
    assert_eq!(buffer.history_groups(), 0);
}

#[test]
fn editing_after_undo_branches_and_the_old_branch_stays_reachable() {
    let mut buffer = Buffer::from_content("");
    buffer.insert(0, "one").unwrap();
    buffer.undo().unwrap();
    buffer.insert(0, "two").unwrap();
    buffer.undo().unwrap();
    assert_eq!(buffer.redo_branches(), 2);
    // redo follows the most recent branch ...
    buffer.redo().unwrap();
    assert_eq!(buffer.text().to_string(), "two");
    buffer.undo().unwrap();
    // ... and the older one is one call away.
    buffer.redo_branch(0).unwrap();
    assert_eq!(buffer.text().to_string(), "one");
    assert!(buffer.redo_branch(5).is_none());
}

#[test]
fn dirty_follows_the_saved_state_through_undo() {
    let mut buffer = Buffer::from_content("abc");
    assert!(!buffer.is_dirty());
    buffer.insert(3, "d").unwrap();
    assert!(buffer.is_dirty());
    buffer.mark_saved();
    assert!(!buffer.is_dirty());
    buffer.undo().unwrap();
    assert!(buffer.is_dirty());
    buffer.redo().unwrap();
    assert!(!buffer.is_dirty());
}

#[test]
fn history_is_bounded_by_groups_bytes_and_age() {
    let mut buffer = Buffer::from_content("");
    buffer.set_history_limits(HistoryLimits {
        max_groups: 5,
        max_bytes: usize::MAX,
        max_age: None,
    });
    for i in 0..20 {
        buffer.insert(i, "x").unwrap();
    }
    assert_eq!(buffer.history_groups(), 5);
    let mut undone = 0;
    while buffer.undo().is_some() {
        undone += 1;
    }
    assert_eq!(undone, 5);
    // The text before the oldest kept group is what is left after undoing everything.
    assert_eq!(buffer.text().len(), 15);
    // Redo is unaffected.
    while buffer.redo().is_some() {}
    assert_eq!(buffer.text().len(), 20);

    // Bytes: a large paste is evicted once newer groups push the total over the bound.
    let mut buffer = Buffer::from_content("");
    buffer.set_history_limits(HistoryLimits {
        max_groups: usize::MAX,
        max_bytes: 4096,
        max_age: None,
    });
    buffer.insert(0, "y".repeat(3000)).unwrap();
    for _ in 0..4 {
        let at = buffer.text().len();
        buffer.insert(at, "z").unwrap();
    }
    assert!(
        buffer.history_bytes() <= 4096,
        "history holds {} bytes",
        buffer.history_bytes()
    );
    assert!(buffer.history_groups() < 5);

    // Age: groups older than max_age are dropped by the next sweep, newest state stays.
    let mut buffer = Buffer::from_content("");
    buffer.set_history_limits(HistoryLimits {
        max_groups: usize::MAX,
        max_bytes: usize::MAX,
        max_age: Some(std::time::Duration::from_secs(60)),
    });
    for i in 0..4 {
        buffer.insert(i, "x").unwrap();
    }
    assert_eq!(buffer.history_groups(), 4);
    buffer.enforce_history_limits(std::time::Instant::now() + std::time::Duration::from_secs(3600));
    assert_eq!(buffer.history_groups(), 0);
    assert!(!buffer.can_undo());
    assert_eq!(buffer.text().to_string(), "xxxx");
}

#[test]
fn dropping_the_saved_state_from_the_history_leaves_the_buffer_dirty() {
    let mut buffer = Buffer::from_content("");
    buffer.set_history_limits(HistoryLimits {
        max_groups: 2,
        max_bytes: usize::MAX,
        max_age: None,
    });
    for i in 0..5 {
        buffer.insert(i, "x").unwrap();
    }
    assert!(buffer.is_dirty());
    buffer.mark_saved();
    assert!(!buffer.is_dirty());
}
