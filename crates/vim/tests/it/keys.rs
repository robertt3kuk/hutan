//! What keys do: motions, operators, text objects, registers, `.`, undo, modes.

use hutan_vim::{Key, Mode, Outcome};

use crate::common::{Rig, run};

#[test]
fn motions_move_the_caret_and_stay_on_the_line() {
    assert_eq!(run("|abc def", "3l"), "abc| def");
    assert_eq!(run("|abc def", "99l"), "abc de|f");
    assert_eq!(run("one\n|two", "h"), "one\n|two", "h does not wrap");
    assert_eq!(run("  ab|c", "^"), "  |abc");
    assert_eq!(run("|abc\ndef", "$"), "ab|c\ndef");
    assert_eq!(run("abc de|f", "0"), "|abc def");
    assert_eq!(run("|foo bar", "w"), "foo |bar");
    assert_eq!(run("|foo bar", "e"), "fo|o bar");
    assert_eq!(run("foo b|ar", "b"), "foo |bar");
    assert_eq!(run("|foo.bar baz", "W"), "foo.bar |baz");
    assert_eq!(run("|ab.c", "t."), "a|b.c");
    assert_eq!(run("|a.b.c", "f.;"), "a.b|.c");
    assert_eq!(run("a.b.c|", "F.;"), "a|.b.c");
}

#[test]
fn vertical_motions_keep_the_column() {
    let text = "abc|def\nxy\nlonger line";
    assert_eq!(run(text, "j"), "abcdef\nx|y\nlonger line");
    assert_eq!(run(text, "jj"), "abcdef\nxy\nlon|ger line");
    assert_eq!(run(text, "jjk"), "abcdef\nx|y\nlonger line");
    assert_eq!(run("|abc\n  def", "G"), "abc\n  |def");
    assert_eq!(run("abc\ndef\n|ghi", "gg"), "|abc\ndef\nghi");
    assert_eq!(run("|abc\ndef\nghi", "2G"), "abc\n|def\nghi");
    assert_eq!(run("|ab\nlonger", "$j"), "ab\nlonge|r");
}

#[test]
fn columns_are_display_columns() {
    // Like Vim: a CJK character is two columns wide, a tab reaches the next tab stop.
    assert_eq!(run("日本|語\nabcdef", "j"), "日本語\nabcd|ef");
    assert_eq!(run("日本語\nab|cdef", "k"), "日|本語\nabcdef");
    assert_eq!(run("é|x\nðð", "j"), "éx\nð|ð");
    assert_eq!(run("\t|ab\nabcdefghij", "j"), "\tab\nabcdefgh|ij");
}

#[test]
fn operators_with_motions() {
    assert_eq!(run("|foo bar", "dw"), "|bar");
    assert_eq!(run("|a b c d e", "2dw"), "|c d e");
    assert_eq!(run("|a b c d e", "d2w"), "|c d e");
    assert_eq!(run("|foo bar", "d$"), "|");
    assert_eq!(run("foo b|ar", "db"), "foo |ar");
    assert_eq!(run("|foo bar", "cwX<Esc>"), "|X bar");
    assert_eq!(run("|foo\nbar", "dd"), "|bar");
    assert_eq!(run("one\n|two\nthree", "dd"), "one\n|three");
    assert_eq!(run("one\ntwo\n|three", "dd"), "one\n|two");
    assert_eq!(run("|one\ntwo", "yyp"), "one\n|one\ntwo");
    assert_eq!(run("|foo bar", "ywP"), "foo| foo bar");
    assert_eq!(run("a|bc", "x"), "a|c");
    assert_eq!(run("ab|c", "x"), "a|b");
    assert_eq!(run("|abc", "rx"), "|xbc");
    assert_eq!(run("|abc", "~"), "A|bc");
}

#[test]
fn text_objects() {
    assert_eq!(run("fo|o bar", "diw"), "| bar");
    assert_eq!(run("fo|o bar", "daw"), "|bar");
    assert_eq!(run("foo b|ar", "daw"), "fo|o");
    assert_eq!(run("fo|o.bar baz", "diW"), "| baz");
    assert_eq!(run("|foo bar", "viwd"), "| bar");
}

#[test]
fn dot_repeats_the_last_change() {
    assert_eq!(run("|a b c d", "dw."), "|c d");
    assert_eq!(run("|foo bar baz", "cwx<Esc>w."), "x |x baz");
    assert_eq!(run("|a\nb\nc", "dd."), "|c");
}

#[test]
fn undo_and_redo_step_over_whole_commands() {
    assert_eq!(run("|foo bar", "dwu"), "|foo bar");
    assert_eq!(
        run("|foo bar", "cwxyz<Esc>u"),
        "|foo bar",
        "typing is one step"
    );
    assert_eq!(run("|foo bar", "dwu<C-r>"), "|bar");
    assert_eq!(run("|a b c", "dwdwdw2u"), "|b c");
}

#[test]
fn visual_mode_selects_and_operates() {
    let mut r = Rig::new("|foo bar");
    r.keys("vl");
    assert_eq!(r.mode(), Mode::Visual);
    assert_eq!(r.vim.selection(&r.host), Some(0..2));
    r.keys("y");
    assert_eq!(r.mode(), Mode::Normal);
    assert_eq!(r.vim.register(), ("fo", false));
    assert_eq!(r.vim.selection(&r.host), None);
    assert_eq!(run("|foobar", "vlld"), "|bar");
    assert_eq!(run("|foo bar", "vee~"), "|FOO BAR");
    let mut r = Rig::new("|foo");
    r.keys("vl<Esc>");
    assert_eq!(r.mode(), Mode::Normal);
}

#[test]
fn insert_mode_types_through_the_host() {
    let mut r = Rig::new("|");
    r.keys("i");
    r.keys("ab<BS>c<Left>X");
    assert_eq!(r.shown(), "aX|c");
    assert_eq!(r.key(Key::Escape), Outcome::Handled);
    assert_eq!(r.mode(), Mode::Normal);
    assert_eq!(r.shown(), "a|Xc");
    assert_eq!(run("a|b", "ix<Esc>"), "a|xb");
    assert_eq!(run("a|b", "Ax<Esc>"), "ab|x");
    assert_eq!(run("  a|b", "Ix<Esc>"), "  |xab");
    assert_eq!(run("a|b\nc", "ox<Esc>"), "ab\n|x\nc");
    assert_eq!(run("ab\n|c", "Ox<Esc>"), "ab\n|x\nc");
}

#[test]
fn passthrough_leaves_typing_to_the_field() {
    let mut r = Rig::new("ab|");
    r.keys("i");
    r.opts.insert_passthrough = true;
    assert_eq!(r.key(Key::Char('x')), Outcome::Ignored);
    assert_eq!(r.text(), "ab");
    assert_eq!(r.key(Key::Escape), Outcome::Handled);
    assert_eq!(r.mode(), Mode::Normal);
}

#[test]
fn normal_mode_hands_its_own_keys_back() {
    let mut r = Rig::new("|ab");
    assert_eq!(r.key(Key::Enter), Outcome::Ignored);
    assert_eq!(r.key(Key::Tab), Outcome::Ignored);
    assert_eq!(r.key(Key::Escape), Outcome::Ignored);
    assert_eq!(
        r.key(Key::Char('?')),
        Outcome::Handled,
        "unknown keys are swallowed"
    );
}

#[test]
fn registers_and_the_clipboard() {
    let mut r = Rig::new("|foo bar");
    r.keys("dw");
    assert_eq!(r.vim.register(), ("foo ", false));
    assert_eq!(r.clip.0, None, "private without the setting");
    r.opts.use_system_clipboard = true;
    r.keys("yy");
    assert_eq!(r.clip.0.as_deref(), Some("bar\n"));
    r.clip.0 = Some("one\ntwo\n".into());
    r.keys("p");
    assert_eq!(
        r.text(),
        "bar\none\ntwo",
        "a clipboard of lines pastes as lines"
    );
}

#[test]
fn single_line_fields_have_one_line() {
    let mut r = Rig::single("ab|c");
    r.keys("dd");
    assert_eq!(r.text(), "");
    assert_eq!(r.vim.register(), ("abc", false));
    assert_eq!(Rig::single("a|b").keys("j").shown(), "a|b");
    assert_eq!(Rig::single("a|b").keys("oX<Esc>").text(), "abX");
}

#[test]
fn multibyte_text_survives() {
    assert_eq!(run("|héllo wörld", "dw"), "|wörld");
    assert_eq!(run("h|éllo", "x"), "h|llo");
    assert_eq!(run("|日本語 テスト", "wD"), "日本語| ");
    assert_eq!(run("|ß", "~"), "S|S", "a letter that becomes two");
}

#[test]
fn pending_keys_show_a_command_in_progress() {
    let mut r = Rig::new("|a b c");
    r.keys("2d");
    assert_eq!(r.vim.pending_keys(), "2d");
    r.keys("<Esc>");
    assert_eq!(r.vim.pending_keys(), "");
    r.keys("f");
    assert_eq!(r.vim.pending_keys(), "f");
}

#[test]
fn reset_returns_to_insert_and_keeps_the_register() {
    let mut r = Rig::new("|foo bar");
    r.keys("dw");
    r.vim.reset();
    assert_eq!(r.mode(), Mode::Insert);
    assert_eq!(r.vim.register(), ("foo ", false));
}

#[test]
fn a_character_is_text_only_where_vim_waits_for_one() {
    let mut r = Rig::new("|abc def");
    // Idle Normal mode: a letter of another alphabet is no command, so it is a key by place.
    assert!(!r.vim.takes_text('в'));
    r.keys("d");
    assert!(!r.vim.takes_text('в'), "an operator waits for a motion");
    r.keys("<Esc>2");
    assert!(!r.vim.takes_text('в'), "a count waits for a command");
    r.keys("<Esc>\"");
    assert!(!r.vim.takes_text('в'), "a register name is Latin");
    r.keys("<Esc>'");
    assert!(!r.vim.takes_text('в'), "a mark name is Latin");
    for waits in ["f", "dt", "r", "2F"] {
        r.keys("<Esc>").keys(waits);
        assert!(r.vim.takes_text('в'), "{waits} waits for a character");
    }
    r.keys("<Esc>/");
    assert!(r.vim.takes_text('в'), "the search line is text");
    r.keys("<Esc>vr");
    assert!(r.vim.takes_text('в'), "Visual r waits for a character");
    r.keys("<Esc><Esc>i");
    assert!(r.vim.takes_text('в'), "Insert mode types");
}
