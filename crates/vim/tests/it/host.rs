//! What the modal layer asks of its host: the transactions it sends, the undo groups it opens.

use hutan_vim::{Edit, Mode};

use crate::common::Rig;

#[test]
fn a_delete_is_one_transaction_with_the_caret_where_vim_leaves_it() {
    let mut r = Rig::new("|foo bar");
    r.keys("dw");
    assert_eq!(r.host.log.len(), 1);
    let tx = &r.host.log[0];
    assert_eq!(
        tx.edits,
        [Edit {
            range: 0..4,
            text: String::new()
        }]
    );
    assert_eq!(tx.cursor, 0);
}

#[test]
fn a_linewise_put_inserts_a_whole_line() {
    let mut r = Rig::new("|one");
    r.keys("yyp");
    let tx = r.host.log.last().unwrap();
    assert_eq!(
        tx.edits,
        [Edit {
            range: 3..3,
            text: "\none".into()
        }]
    );
    assert_eq!(r.shown(), "one\n|one");
}

#[test]
fn open_line_is_one_insertion() {
    let mut r = Rig::new("a|b\nc");
    r.keys("o");
    assert_eq!(r.host.log.len(), 1);
    assert_eq!(r.host.log[0].edits[0].range, 2..2);
    assert_eq!(r.host.log[0].edits[0].text, "\n");
    assert_eq!(r.host.log[0].cursor, 3);
    assert!(r.host.is_grouped(), "the Insert session is open");
}

#[test]
fn replace_and_toggle_case_replace_in_place() {
    let mut r = Rig::new("|abc");
    r.keys("2rx");
    assert_eq!(r.host.log[0].edits[0].range, 0..2);
    assert_eq!(r.host.log[0].edits[0].text, "xx");
    assert_eq!(r.host.log[0].cursor, 1, "on the last replaced character");
    r.keys("~");
    assert_eq!(r.text(), "xXc");
}

#[test]
fn a_change_and_its_typing_are_one_group() {
    let mut r = Rig::new("|foo bar");
    r.keys("cw");
    assert!(r.host.is_grouped());
    assert_eq!(r.mode(), Mode::Insert);
    r.keys("xyz<Esc>");
    assert!(!r.host.is_grouped());
    assert_eq!((r.host.begun, r.host.ended), (1, 1));
    r.keys("u");
    assert_eq!(r.text(), "foo bar");
}

#[test]
fn every_complete_command_closes_its_group() {
    let mut r = Rig::new("|one two\nthree four\nfive");
    for keys in [
        "dw", "x", "yyP", "~", "rX", "3x", "dd", "ddp", "jD", "ve~", "vx",
    ] {
        r.keys(keys);
        assert!(!r.host.is_grouped(), "after {keys}");
        assert_eq!(r.mode(), Mode::Normal, "after {keys}");
    }
}

#[test]
fn a_command_that_changes_nothing_leaves_no_undo_step() {
    let mut r = Rig::new("|abc");
    r.keys("x");
    r.keys("hhhdb");
    r.keys("u");
    assert_eq!(r.text(), "abc");
    r.keys("u");
    assert_eq!(r.text(), "abc", "nothing else to undo");
}

#[test]
fn dot_applies_through_the_host_like_the_first_time() {
    let mut r = Rig::new("|a b c");
    r.keys("dw");
    let first = r.host.log[0].clone();
    r.keys(".");
    assert_eq!(r.text(), "c");
    assert_eq!(r.host.log.len(), 2);
    assert_eq!(r.host.log[1].edits, first.edits);
    r.keys("u");
    assert_eq!(r.text(), "b c", "a dot is one undo step");
}

#[test]
fn a_command_reads_what_it_needs_not_the_whole_text() {
    // A long second line: the commands on the first one slice a few bytes.
    let text = format!("one two three\n{}", "x".repeat(100_000));
    let mut r = Rig::new(&format!("|{text}"));
    r.keys("wldwu");
    r.keys("ywP$x0dwdd");
    assert!(
        r.host.sliced.get() < 100,
        "{} bytes read",
        r.host.sliced.get()
    );
}
