#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Completion and signature help in file tabs (slice E-12b) over a real daemon and
//! `hutan-fake-lsp`: the popup on a trigger character, ranked and filtered here while typing,
//! the selected item resolved for its documentation, an accepted item with an import as one
//! undo step, snippet tab stops, signature help with the active parameter, Vim Insert's ⌃N /
//! ⌃P and the Escape rule with an input method's marked text, a superseded request cancelled
//! on the server, and B16 (keystroke → items on screen).

use crate::common;
use crate::lsp_gpui::{Env, Tab, caret, open_file};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{draw, wait_until};
use gpui::{Entity, EntityInputHandler as _, TestAppContext, VisualTestContext};
use hutan_code_view::{COMPLETION_DEBOUNCE, CodeView, FileBuffer, VimMode};
use hutan_desktop::workbench::Workbench;
use serde_json::{Value, json};

/// A server with completion (trigger `.`, resolve) and signature help (`(`, `,`).
fn script(journal: Option<&Path>) -> Value {
    json!({
        "capabilities": {
            "textDocumentSync": 2,
            "completionProvider": {"triggerCharacters": ["."], "resolveProvider": true},
            "signatureHelpProvider": {"triggerCharacters": ["("], "retriggerCharacters": [","]},
        },
        "completion": {"items": [
            {"label": "push", "kind": 2, "sortText": "1", "insertTextFormat": 2,
             "insertText": "push(${1:value}, ${2:n})$0", "detail": "fn(&mut self, T, usize)"},
            {"label": "pop", "kind": 2, "sortText": "2", "detail": "fn(&mut self) -> Option<T>"},
            {"label": "HashMap", "kind": 22, "sortText": "3",
             "additionalTextEdits": [{"range": {"start": {"line": 0, "character": 0},
                 "end": {"line": 0, "character": 0}}, "newText": "use std::collections::HashMap;\n"}]},
        ]},
        "resolve": {"push": {"documentation": {"kind": "markdown",
            "value": "Appends **value** to the back, `n` times."}}},
        "signature_help": {"label": "push(value: T, n: usize)", "parameters": ["value: T", "n: usize"],
                           "documentation": "Pushes."},
        "hang_once": if journal.is_some() { json!(["textDocument/completion"]) } else { json!([]) },
        "journal": journal,
    })
}

fn text(buffer: &Entity<FileBuffer>, cx: &mut VisualTestContext) -> String {
    buffer.read_with(cx, |b, _| b.text().to_string())
}

fn labels(code: &Entity<CodeView>, cx: &mut VisualTestContext) -> Vec<String> {
    code.read_with(cx, |c, _| {
        c.completion()
            .filter(|s| s.is_visible())
            .map(|s| s.labels())
            .unwrap_or_default()
    })
}

fn selected(code: &Entity<CodeView>, cx: &mut VisualTestContext) -> Option<String> {
    code.read_with(cx, |c, _| {
        c.completion()
            .and_then(|s| s.selected())
            .map(|i| i.label.clone())
    })
}

fn selection(code: &Entity<CodeView>, cx: &mut VisualTestContext) -> Vec<(usize, usize)> {
    code.read_with(cx, |c, _| {
        c.selections()
            .iter()
            .map(|s| (s.range().start, s.range().end))
            .collect()
    })
}

/// Opens `name` with `text` and waits until its view knows the server's triggers.
fn open_ready(
    env: &Env,
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    name: &str,
    text: &str,
) -> Tab {
    let tab = open_file(view, cx, &env.file(name), text);
    wait_until(cx, "the server's completion triggers", |cx| {
        tab.code.read_with(cx, |c, _| {
            let t = c.completion_triggers();
            t.enabled && t.completion == ['.'] && t.signature == ['(', ',']
        })
    });
    tab
}

fn wait_labels(code: &Entity<CodeView>, cx: &mut VisualTestContext, expected: &[&str]) {
    wait_until(cx, &format!("the popup showing {expected:?}"), |cx| {
        labels(code, cx) == expected
    });
}

#[gpui::test]
fn the_popup_filters_resolves_and_accepts_an_import_in_one_undo_step(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| script(None));
    let (view, cx) = env.open(cx, "");
    let tab = open_ready(&env, &view, cx, "a.fake", "x\nv");
    cx.simulate_keystrokes("cmd-down");

    // The trigger character asks at once; the server's order is its sort text.
    cx.simulate_input(".");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    // The selected item is resolved: its documentation shows next to the list.
    wait_until(cx, "push's documentation", |cx| {
        tab.code.read_with(cx, |c, _| {
            c.completion()
                .and_then(|s| s.selected())
                .and_then(|i| i.documentation.clone())
                .is_some_and(|d| d.contains("Appends **value**"))
        })
    });
    assert!(
        tab.code
            .read_with(cx, |c, _| c.completion().unwrap().drawn_at().is_some())
    );

    // Typing filters here, without asking again (the list is complete).
    cx.simulate_input("p");
    assert_eq!(labels(&tab.code, cx), ["push", "pop", "HashMap"]);
    cx.simulate_input("o");
    assert_eq!(labels(&tab.code, cx), ["pop"]);
    cx.simulate_keystrokes("backspace backspace");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    cx.simulate_keystrokes("down down");
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("HashMap"));
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("pop"));

    // `Ha` then Return: the item and its import, one undo step.
    cx.simulate_input("Ha");
    assert_eq!(labels(&tab.code, cx), ["HashMap"]);
    cx.simulate_keystrokes("enter");
    assert_eq!(
        text(&tab.buffer, cx),
        "use std::collections::HashMap;\nx\nv.HashMap"
    );
    assert_eq!(caret(&tab.code, cx), text(&tab.buffer, cx).len());
    assert!(labels(&tab.code, cx).is_empty(), "the popup closed");
    cx.simulate_keystrokes("cmd-z");
    assert_eq!(
        text(&tab.buffer, cx),
        "x\nv.Ha",
        "one undo takes both edits"
    );

    // A character that is no word and no trigger closes it; Escape does too.
    cx.simulate_input(".");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    cx.simulate_input(" ");
    assert!(labels(&tab.code, cx).is_empty());
    cx.simulate_keystrokes("ctrl-space");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    cx.simulate_keystrokes("escape");
    assert!(labels(&tab.code, cx).is_empty());
}

#[gpui::test]
fn a_snippet_has_tab_stops_and_signature_help_follows_the_parameters(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| script(None));
    let (view, cx) = env.open(cx, "");
    let tab = open_ready(&env, &view, cx, "s.fake", "v");
    cx.simulate_keystrokes("cmd-down");
    cx.simulate_input(".pu");
    wait_labels(&tab.code, cx, &["push"]);
    cx.simulate_keystrokes("tab");
    assert_eq!(text(&tab.buffer, cx), "v.push(value, n)");
    // The first stop's placeholder is selected; signature help shows its parameter.
    assert_eq!(selection(&tab.code, cx), [(7, 12)]);
    assert!(tab.code.read_with(cx, |c, _| c.in_snippet()));
    wait_until(cx, "signature help on `value`", |cx| {
        tab.code.read_with(cx, |c, _| c.signature_shown())
            == Some(("push(value: T, n: usize)".into(), Some("value: T".into())))
    });
    cx.simulate_input("item");
    cx.simulate_keystrokes("tab");
    assert_eq!(selection(&tab.code, cx), [(13, 14)]);
    cx.simulate_input("3");
    assert_eq!(text(&tab.buffer, cx), "v.push(item, 3)");
    wait_until(cx, "signature help on `n`", |cx| {
        tab.code.read_with(cx, |c, _| c.signature_shown())
            == Some(("push(value: T, n: usize)".into(), Some("n: usize".into())))
    });
    // The last Tab goes to the end (`$0`) and the snippet is over.
    cx.simulate_keystrokes("tab");
    assert_eq!(selection(&tab.code, cx), [(15, 15)]);
    assert!(!tab.code.read_with(cx, |c, _| c.in_snippet()));
    cx.simulate_keystrokes("escape");
    assert_eq!(tab.code.read_with(cx, |c, _| c.signature_shown()), None);

    // Typed by hand: `(` opens it on the first parameter, `,` moves to the next.
    cx.simulate_keystrokes("enter");
    cx.simulate_input("push(");
    wait_until(cx, "signature help after `(`", |cx| {
        tab.code
            .read_with(cx, |c, _| c.signature_shown())
            .and_then(|s| s.1)
            == Some("value: T".into())
    });
    cx.simulate_input("a,");
    wait_until(cx, "signature help after `,`", |cx| {
        tab.code
            .read_with(cx, |c, _| c.signature_shown())
            .and_then(|s| s.1)
            == Some("n: usize".into())
    });
}

#[gpui::test]
fn vim_insert_cycles_with_ctrl_n_ctrl_p_and_escape_keeps_its_order(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| script(None));
    let (view, cx) = env.open(cx, "[editor]\nvim_mode = true\n");
    let tab = open_ready(&env, &view, cx, "v.fake", "v.\n");
    let mode = |cx: &mut VisualTestContext| tab.code.read_with(cx, |c, _| c.vim_mode());
    cx.simulate_keystrokes("A");
    assert_eq!(mode(cx), Some(VimMode::Insert));
    // ⌃N with no popup asks; with it, ⌃N / ⌃P move (wrapping).
    cx.simulate_keystrokes("ctrl-n");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("push"));
    cx.simulate_keystrokes("ctrl-n");
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("pop"));
    cx.simulate_keystrokes("ctrl-n ctrl-n");
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("push"));
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(selected(&tab.code, cx).as_deref(), Some("HashMap"));

    // An input method composes `po` (marked): the popup follows it.
    tab.code.update_in(cx, |code, window, cx| {
        code.replace_and_mark_text_in_range(None, "po", None, window, cx);
    });
    draw(cx);
    assert_eq!(labels(&tab.code, cx), ["pop"]);
    assert!(!tab.code.read_with(cx, |c, _| c.marked_ranges().is_empty()));
    // Escape 1 ends the composition only; 2 closes the popup; 3 leaves Insert.
    cx.simulate_keystrokes("escape");
    assert!(tab.code.read_with(cx, |c, _| c.marked_ranges().is_empty()));
    assert_eq!(labels(&tab.code, cx), ["pop"]);
    assert_eq!(mode(cx), Some(VimMode::Insert));
    cx.simulate_keystrokes("escape");
    assert!(labels(&tab.code, cx).is_empty());
    assert_eq!(mode(cx), Some(VimMode::Insert));
    cx.simulate_keystrokes("escape");
    assert_eq!(mode(cx), Some(VimMode::Normal));
    assert_eq!(text(&tab.buffer, cx), "v.po\n");
}

/// The fake server's journal (one line per message it received).
fn journal(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[gpui::test]
fn a_superseded_completion_is_cancelled_on_the_server(cx: &mut TestAppContext) {
    let work = tempfile::tempdir().unwrap();
    let log = work.path().join("journal");
    let env = Env::new("always", |_| script(Some(&log)));
    let (view, cx) = env.open(cx, "");
    let tab = open_ready(&env, &view, cx, "c.fake", "v");
    cx.simulate_keystrokes("cmd-down");
    // The server never answers the first request…
    cx.simulate_input(".");
    let mut first = None;
    wait_until(cx, "the first completion at the server", |_| {
        first = journal(&log).into_iter().find_map(|l| {
            l.strip_prefix("textDocument/completion ")
                .map(str::to_owned)
        });
        first.is_some()
    });
    let first = first.unwrap();
    // …⌃Space asks again: the daemon cancels the first one and the second one is shown.
    cx.simulate_keystrokes("ctrl-space");
    wait_labels(&tab.code, cx, &["push", "pop", "HashMap"]);
    wait_until(cx, "the server told to cancel the first one", |_| {
        journal(&log).contains(&format!("cancelled {first}"))
    });
}

/// B16: keystroke → the first frame with the items, with a local fake server. The test clock
/// skips the typing debounce of a word ([`COMPLETION_DEBOUNCE`]), so it is added back.
#[gpui::test]
fn b16_keystroke_to_items_on_screen(cx: &mut TestAppContext) {
    let env = Env::new("always", |_| script(None));
    let (view, cx) = env.open(cx, "");
    let tab = open_ready(&env, &view, cx, "b.fake", "v");
    cx.simulate_keystrokes("cmd-down");
    let mut trigger = Vec::new();
    let mut word = Vec::new();
    for round in 0..12 {
        let by_word = round % 2 == 1;
        let started = Instant::now();
        if by_word {
            cx.simulate_input(" p");
            cx.executor().advance_clock(COMPLETION_DEBOUNCE);
        } else {
            cx.simulate_input(".");
        }
        let deadline = started + common::WAIT;
        let drawn = loop {
            cx.run_until_parked();
            draw(cx);
            if let Some(at) = tab
                .code
                .read_with(cx, |c, _| c.completion().and_then(|s| s.drawn_at()))
            {
                break at;
            }
            assert!(Instant::now() < deadline, "no popup in round {round}");
            std::thread::sleep(Duration::from_millis(1));
        };
        let took = drawn - started;
        if by_word {
            word.push(took + COMPLETION_DEBOUNCE);
        } else {
            trigger.push(took);
        }
        cx.simulate_keystrokes("escape");
    }
    trigger.sort();
    word.sort();
    let (t, w) = (trigger[trigger.len() / 2], word[word.len() / 2]);
    eprintln!(
        "B16 keystroke → items on screen: trigger character median {t:?} (max {:?}), word median {w:?} (max {:?})",
        trigger.last().unwrap(),
        word.last().unwrap()
    );
    // 100 ms is the budget; a debug build on a loaded machine is given more room.
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(100)
    };
    assert!(t <= budget && w <= budget, "trigger {t:?}, word {w:?}");
}
