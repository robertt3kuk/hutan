//! An agent's changes in a code view (slice E-10): an agent's write to a clean buffer is one
//! undo step that keeps the caret, its hunks are the turn's changes and not the user's typing,
//! Reject puts back one hunk as an undoable step, the hunk arrows and *Follow* find the newest
//! change, and `editor.agent_highlight = false` computes nothing.

use std::sync::Arc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_code_view::{
    AgentChanges, AgentLabel, CodeView, FileBuffer, HunkKind, actions as a, agent,
};

use crate::editing::{draw, open, place, ranges, run, text_of};

fn numbered(lines: usize) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

fn changes(base: &str, agent_text: &str) -> AgentChanges {
    AgentChanges {
        base: Arc::from(base),
        agent: Arc::from(agent_text),
        label: AgentLabel {
            session: "01K7F3B5N2Q9W8E4R6T1Y0V3KM".to_owned(),
            title: Some("Codex".to_owned()),
            agent: Some("Codex".to_owned()),
        },
    }
}

/// *Reject* on the hunk at the caret (the file tab's action runs this).
fn reject_at_caret(view: &Entity<CodeView>, cx: &mut VisualTestContext) {
    view.update(cx, |v, cx| v.reject_agent_hunk_at_caret(cx));
    draw(cx);
}

/// Lets the debounced diff after an edit run.
fn settle(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(agent::SLOW_DIFF_DELAY * 2);
    draw(cx);
}

fn hunk_texts(buffer: &Entity<FileBuffer>, cx: &VisualTestContext) -> Vec<String> {
    buffer.read_with(cx, |b, _| {
        assert!(b.agent_hunks_current(), "hunks of the current text");
        let text = b.text().to_string();
        b.agent_hunks()
            .iter()
            .map(|h| text[h.bytes.clone()].to_owned())
            .collect()
    })
}

/// The agent rewrote lines 3 and 25 of a 40-line file.
fn agent_version(base: &str) -> String {
    base.replace("line 3\n", "line three\n")
        .replace("line 25\n", "line twenty-five\nline 25b\n")
}

/// A clean buffer of `base` that took the agent's write, as the desktop does it: reload, then
/// the turn's changes.
fn written(
    cx: &mut TestAppContext,
) -> (
    Entity<CodeView>,
    Entity<FileBuffer>,
    &mut VisualTestContext,
    String,
    String,
) {
    let base = numbered(40);
    let (view, buffer, cx) = open(cx, "notes.txt", &base, false);
    let agent_text = agent_version(&base);
    buffer.update(cx, |b, cx| {
        assert!(b.reload(&agent_text, cx));
        let at = b.last_reload().map(|r| r.start);
        b.agent_wrote(changes(&base, &agent_text), at, cx);
    });
    draw(cx);
    (view, buffer, cx, base, agent_text)
}

#[gpui::test]
fn an_agent_write_to_a_clean_buffer_is_one_undo_step_and_keeps_the_caret(cx: &mut TestAppContext) {
    let base = numbered(40);
    let (view, buffer, cx) = open(cx, "notes.txt", &base, false);
    // The caret on line 20, column 3.
    let line_20 = base.find("line 20").unwrap();
    place(&view, cx, &[line_20 + 2]);
    let agent_text = agent_version(&base);
    buffer.update(cx, |b, cx| {
        assert!(b.reload(&agent_text, cx));
    });
    draw(cx);
    assert_eq!(text_of(&buffer, cx), agent_text);
    assert!(
        !buffer.read_with(cx, |b, _| b.is_dirty()),
        "clean: it is the disk"
    );
    let caret = ranges(&view, cx)[0].start;
    assert_eq!(
        &agent_text[caret - 2..caret + 5],
        "line 20",
        "the caret stays on its text"
    );
    assert_eq!(view.read_with(cx, |v, cx| v.position(cx)), (20, 3));
    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        base,
        "one undo step takes the whole write back"
    );
    run(cx, a::Redo);
    assert_eq!(text_of(&buffer, cx), agent_text);
}

#[gpui::test]
fn the_hunks_are_the_agents_changes(cx: &mut TestAppContext) {
    let (_view, buffer, cx, _base, _agent) = written(cx);
    assert_eq!(
        hunk_texts(&buffer, cx),
        ["line three\n", "line twenty-five\nline 25b\n"]
    );
    let kinds: Vec<HunkKind> = buffer.read_with(cx, |b, _| {
        b.agent_hunks().iter().map(|h| h.kind()).collect()
    });
    assert_eq!(kinds, [HunkKind::Modified, HunkKind::Modified]);
}

#[gpui::test]
fn reject_restores_that_hunk_only_and_is_undoable(cx: &mut TestAppContext) {
    let (view, buffer, cx, base, agent_text) = written(cx);
    let second = agent_text.find("line twenty-five").unwrap();
    place(&view, cx, &[second + 3]);
    reject_at_caret(&view, cx);
    let expected = agent_text.replace("line twenty-five\nline 25b\n", "line 25\n");
    assert_eq!(
        text_of(&buffer, cx),
        expected,
        "only the hunk at the caret went back"
    );
    assert_ne!(text_of(&buffer, cx), base, "the other hunk stays");
    assert_eq!(hunk_texts(&buffer, cx), ["line three\n"]);
    assert!(buffer.read_with(cx, |b, _| b.is_dirty()));

    run(cx, a::Undo);
    assert_eq!(
        text_of(&buffer, cx),
        agent_text,
        "undo brings the agent's text back"
    );
    settle(cx);
    assert_eq!(
        hunk_texts(&buffer, cx),
        ["line three\n", "line twenty-five\nline 25b\n"],
        "and its hunk"
    );
    assert_eq!(
        ranges(&view, cx),
        vec![second + 3..second + 3],
        "and the caret"
    );
}

#[gpui::test]
fn the_users_typing_is_not_an_agent_hunk(cx: &mut TestAppContext) {
    let (view, buffer, cx, _base, agent_text) = written(cx);
    let line_10 = agent_text.find("line 10\n").unwrap();
    place(&view, cx, &[line_10]);
    cx.simulate_input("// mine ");
    settle(cx);
    assert_eq!(
        hunk_texts(&buffer, cx),
        ["line three\n", "line twenty-five\nline 25b\n"],
        "a line the user changed is not the agent's"
    );
    // An edit inside the agent's hunk stays part of it.
    let three = text_of(&buffer, cx).find("line three").unwrap();
    place(&view, cx, &[three + "line three".len()]);
    cx.simulate_input("!");
    settle(cx);
    assert_eq!(
        hunk_texts(&buffer, cx),
        ["line three!\n", "line twenty-five\nline 25b\n"]
    );
}

#[gpui::test]
fn hunks_follow_an_edit_before_the_next_diff(cx: &mut TestAppContext) {
    let (view, buffer, cx, _base, _agent) = written(cx);
    place(&view, cx, &[0]);
    run(cx, a::Newline);
    // Before the pause is over the hunks moved with the text.
    let moved = buffer.read_with(cx, |b, _| {
        let text = b.text().to_string();
        b.agent_hunks()
            .iter()
            .map(|h| text[h.bytes.clone()].to_owned())
            .collect::<Vec<_>>()
    });
    assert_eq!(moved, ["line three\n", "line twenty-five\nline 25b\n"]);
    assert!(!buffer.read_with(cx, |b, _| b.agent_hunks_current()));
    settle(cx);
    assert!(buffer.read_with(cx, |b, _| b.agent_hunks_current()));
}

#[gpui::test]
fn the_arrows_and_the_newest_change_find_the_hunks(cx: &mut TestAppContext) {
    let (view, buffer, cx, _base, agent_text) = written(cx);
    let first = agent_text.find("line three").unwrap();
    let second = agent_text.find("line twenty-five").unwrap();
    assert_eq!(
        buffer.read_with(cx, |b, _| b.newest_agent_hunk()),
        Some(0),
        "the write began with the first change"
    );
    assert_eq!(buffer.read_with(cx, |b, _| b.agent_epoch()), 1);
    place(&view, cx, &[0]);
    run(cx, a::NextAgentHunk);
    assert_eq!(ranges(&view, cx), vec![first..first]);
    run(cx, a::NextAgentHunk);
    assert_eq!(ranges(&view, cx), vec![second..second]);
    run(cx, a::NextAgentHunk);
    assert_eq!(ranges(&view, cx), vec![first..first], "wraps around");
    run(cx, a::PreviousAgentHunk);
    assert_eq!(ranges(&view, cx), vec![second..second]);

    // A second write further down: the newest change is there.
    let again = agent_text.replace("line 38\n", "line thirty-eight\n");
    buffer.update(cx, |b, cx| {
        assert!(b.reload(&again, cx));
        let at = b.last_reload().map(|r| r.start);
        let base = b.agent_changes().unwrap().base.clone();
        b.agent_wrote(changes(&base, &again), at, cx);
    });
    draw(cx);
    assert_eq!(buffer.read_with(cx, |b, _| b.agent_epoch()), 2);
    assert_eq!(buffer.read_with(cx, |b, _| b.newest_agent_hunk()), Some(2));
    view.update(cx, |v, cx| v.reveal_agent_hunk(2, cx));
    draw(cx);
    let third = again.find("line thirty-eight").unwrap();
    assert_eq!(ranges(&view, cx), vec![third..third]);
    assert!(
        view.read_with(cx, |v, _| v.scroll_y()) > 0.0,
        "scrolled down to it"
    );
}

#[gpui::test]
fn without_the_highlight_nothing_is_computed(cx: &mut TestAppContext) {
    let (_view, buffer, cx, _base, _agent) = written(cx);
    buffer.update(cx, |b, cx| b.set_agent_highlight(false, cx));
    draw(cx);
    assert!(buffer.read_with(cx, |b, _| b.agent_hunks().is_empty()));
    assert!(buffer.read_with(cx, |b, _| b.agent_hunks_current()));
    buffer.update(cx, |b, cx| b.set_agent_highlight(true, cx));
    draw(cx);
    assert_eq!(hunk_texts(&buffer, cx).len(), 2);
}

#[gpui::test]
fn a_created_file_rejects_to_empty_text(cx: &mut TestAppContext) {
    let (view, buffer, cx) = open(cx, "new.txt", "", false);
    buffer.update(cx, |b, cx| {
        assert!(b.reload("hello\n", cx));
        b.agent_wrote(changes("", "hello\n"), Some(0), cx);
    });
    draw(cx);
    assert_eq!(hunk_texts(&buffer, cx), ["hello\n"]);
    place(&view, cx, &[0]);
    reject_at_caret(&view, cx);
    assert_eq!(text_of(&buffer, cx), "");
    assert!(buffer.read_with(cx, |b, _| b.agent_hunks().is_empty()));
}
