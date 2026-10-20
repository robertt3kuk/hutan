//! Vim's modal editing over a [`TextInput`]: the state machine lives in `hutan-vim`
//! ([`hutan_vim::Vim`], GPUI-free, generic over [`VimHost`]); this is the adapter that lets it edit
//! the application's text input, and the paths the views import it by.
//!
//! The adapter keeps the `TextInput`'s own behaviour: its undo steps and groups, its caret and
//! selection rules, its Insert-mode editing keys. Every one-line field of the application goes
//! through it, so the `String` the input holds answers the host's reads directly.

use std::borrow::Cow;
use std::ops::Range;

pub use hutan_vim::{Clipboard, Key, Mode, Options, Outcome};
use hutan_vim::{Transaction, VimHost};

use super::input::TextInput;

/// The modal state of one field; the text stays in the [`TextInput`] the keys are applied to.
pub type Vim = hutan_vim::Vim<TextInput>;

impl VimHost for TextInput {
    fn len(&self) -> usize {
        self.text().len()
    }

    fn char_at(&self, at: usize) -> Option<char> {
        self.text().get(at..)?.chars().next()
    }

    fn char_before(&self, at: usize) -> Option<char> {
        self.text().get(..at)?.chars().next_back()
    }

    fn slice(&self, range: Range<usize>) -> Cow<'_, str> {
        Cow::Borrowed(&self.text()[range])
    }

    fn line_count(&self) -> usize {
        self.text().bytes().filter(|b| *b == b'\n').count() + 1
    }

    fn line_of(&self, at: usize) -> usize {
        self.text()[..at].bytes().filter(|b| *b == b'\n').count()
    }

    fn line_start(&self, line: usize) -> usize {
        match line.checked_sub(1) {
            None => 0,
            Some(n) => self
                .text()
                .match_indices('\n')
                .nth(n)
                .map_or(self.text().len(), |(at, _)| at + 1),
        }
    }

    fn line_start_at(&self, at: usize) -> usize {
        self.text()[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end_at(&self, at: usize) -> usize {
        self.text()[at..]
            .find('\n')
            .map_or(self.text().len(), |i| at + i)
    }

    fn cursor(&self) -> usize {
        TextInput::cursor(self)
    }

    fn set_cursor(&mut self, at: usize) {
        TextInput::set_cursor(self, at);
    }

    fn apply(&mut self, tx: Transaction) {
        // The edits are sorted and apply to the text before them: back to front keeps the
        // offsets of the ones not yet applied valid.
        for edit in tx.edits.iter().rev() {
            self.replace_range(edit.range.clone(), &edit.text);
        }
        TextInput::set_cursor(self, tx.cursor);
    }

    fn type_text(&mut self, text: &str) {
        self.insert(text);
    }

    fn backspace(&mut self) {
        TextInput::backspace(self);
    }

    fn delete_forward(&mut self) {
        self.delete();
    }

    fn move_left(&mut self) {
        self.left();
    }

    fn move_right(&mut self) {
        self.right();
    }

    fn move_line_start(&mut self) {
        self.home();
    }

    fn move_line_end(&mut self) {
        self.end();
    }

    fn move_up(&mut self) -> bool {
        self.up()
    }

    fn move_down(&mut self) -> bool {
        self.down()
    }

    fn begin_group(&mut self) {
        TextInput::begin_group(self);
    }

    fn end_group(&mut self) {
        TextInput::end_group(self);
    }

    fn undo(&mut self) -> bool {
        TextInput::undo(self)
    }

    fn redo(&mut self) -> bool {
        TextInput::redo(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory system clipboard.
    struct Board(Option<String>);

    impl Clipboard for Board {
        fn read(&mut self) -> Option<String> {
            self.0.clone()
        }
        fn write(&mut self, text: &str) {
            self.0 = Some(text.to_owned());
        }
    }

    /// A field and its modal state, fed keys written like Vim documents them: plain
    /// characters, `<Esc>`, `<C-r>`, `<CR>`.
    struct Rig {
        input: TextInput,
        vim: Vim,
        opts: Options,
        clip: Option<String>,
    }

    impl Rig {
        /// Normal mode with the caret at `|` in `text` (at the end when there is none).
        fn new(text: &str) -> Self {
            let mut input = TextInput::new();
            // A fresh `Vim` types; Escape is how it gets to Normal mode.
            let mut vim = Vim::new();
            vim.key(&mut input, Key::Escape, Options::default(), &mut ());
            let (plain, cursor) = match text.find('|') {
                Some(at) => (text.replacen('|', "", 1), at),
                None => (text.to_owned(), text.len()),
            };
            input.reset_to(&plain);
            input.set_cursor(cursor);
            Self {
                input,
                vim,
                opts: Options::default(),
                clip: None,
            }
        }

        fn single(text: &str) -> Self {
            let mut rig = Self::new(text);
            rig.opts.single_line = true;
            rig
        }

        fn key(&mut self, key: Key) -> Outcome {
            let mut board = Board(self.clip.take());
            let outcome = self.vim.key(&mut self.input, key, self.opts, &mut board);
            self.clip = board.0;
            outcome
        }

        fn keys(&mut self, keys: &str) -> &mut Self {
            let mut chars = keys.chars();
            while let Some(c) = chars.next() {
                let key = if c == '<' {
                    let name: String = chars.by_ref().take_while(|c| *c != '>').collect();
                    match name.as_str() {
                        "Esc" => Key::Escape,
                        "CR" => Key::Enter,
                        "BS" => Key::Backspace,
                        "Left" => Key::Left,
                        n if n.starts_with("C-") => Key::Ctrl(n.chars().nth(2).unwrap()),
                        n => panic!("unknown key <{n}>"),
                    }
                } else {
                    Key::Char(c)
                };
                self.key(key);
            }
            self
        }

        fn text(&self) -> &str {
            self.input.text()
        }

        /// The text with `|` before the caret.
        fn shown(&self) -> String {
            let mut s = self.input.text().to_owned();
            s.insert(self.input.cursor(), '|');
            s
        }
    }

    /// `text` in Normal mode, `keys` typed, the result shown with the caret.
    fn run(text: &str, keys: &str) -> String {
        Rig::new(text).keys(keys).shown()
    }

    #[test]
    fn horizontal_motions_stay_on_the_line() {
        assert_eq!(run("|abc def", "l"), "a|bc def");
        assert_eq!(run("|abc def", "3l"), "abc| def");
        assert_eq!(run("|abc def", "99l"), "abc de|f");
        assert_eq!(run("abc de|f", "h"), "abc d|ef");
        assert_eq!(run("a|bc", "5h"), "|abc");
        assert_eq!(run("one\n|two", "h"), "one\n|two", "h does not wrap");
        assert_eq!(run("ab|c\ndef", "l"), "ab|c\ndef", "l does not wrap");
        assert_eq!(run("  ab|c", "0"), "|  abc");
        assert_eq!(run("  ab|c", "^"), "  |abc");
        assert_eq!(run("|abc\ndef", "$"), "ab|c\ndef");
        assert_eq!(
            run("abc de|f", "0"),
            "|abc def",
            "a leading zero is a motion"
        );
        assert_eq!(
            run("abcdefghijkl|m", "10h"),
            "ab|cdefghijklm",
            "a zero after a digit counts"
        );
    }

    #[test]
    fn vertical_motions_keep_the_column() {
        let text = "abc|def\nxy\nlonger line";
        assert_eq!(run(text, "j"), "abcdef\nx|y\nlonger line");
        assert_eq!(run(text, "jj"), "abcdef\nxy\nlon|ger line");
        assert_eq!(run(text, "jjk"), "abcdef\nx|y\nlonger line");
        assert_eq!(run("|abc\ndef\nghi", "G"), "abc\ndef\n|ghi");
        assert_eq!(run("abc\ndef\n|ghi", "gg"), "|abc\ndef\nghi");
        assert_eq!(run("|abc\ndef\nghi", "2G"), "abc\n|def\nghi");
        assert_eq!(run("|abc\ndef\nghi", "3gg"), "abc\ndef\n|ghi");
        assert_eq!(
            run("|abc\n  def", "G"),
            "abc\n  |def",
            "G goes to the first non-blank"
        );
        assert_eq!(run("ab|c", "j"), "ab|c", "no line below");
        assert_eq!(run("ab|c", "k"), "ab|c", "no line above");
        assert_eq!(
            run("|ab\nlonger", "$j"),
            "ab\nlonge|r",
            "$ sticks to the line end"
        );
    }

    #[test]
    fn word_motions() {
        assert_eq!(run("|foo bar  baz", "w"), "foo |bar  baz");
        assert_eq!(run("|foo bar  baz", "2w"), "foo bar  |baz");
        assert_eq!(run("|foo bar  baz", "9w"), "foo bar  ba|z");
        assert_eq!(
            run("|foo.bar baz", "w"),
            "foo|.bar baz",
            "punctuation is a word of its own"
        );
        assert_eq!(run("|foo.bar baz", "W"), "foo.bar |baz");
        assert_eq!(run("foo bar  ba|z", "b"), "foo bar  |baz");
        assert_eq!(run("foo bar  ba|z", "2b"), "foo |bar  baz");
        assert_eq!(run("foo b|ar", "b"), "foo |bar");
        assert_eq!(run("foo.b|ar", "B"), "|foo.bar");
        assert_eq!(run("|foo bar", "e"), "fo|o bar");
        assert_eq!(run("fo|o bar", "e"), "foo ba|r");
        assert_eq!(run("|foo.bar", "E"), "foo.ba|r");
        assert_eq!(run("|foo\nbar", "w"), "foo\n|bar", "w crosses lines");
        assert_eq!(
            run("|foo\n\nbar", "w"),
            "foo\n|\nbar",
            "an empty line is a word"
        );
        assert_eq!(run("foo\n\n|bar", "b"), "foo\n|\nbar");
        assert_eq!(
            run("|a_b c", "w"),
            "a_b |c",
            "the underscore belongs to words"
        );
    }

    #[test]
    fn words_are_unicode_aware() {
        let text = "|привет мир, ёжик";
        assert_eq!(run(text, "w"), "привет |мир, ёжик");
        assert_eq!(run(text, "ww"), "привет мир|, ёжик");
        assert_eq!(run(text, "e"), "приве|т мир, ёжик");
        assert_eq!(run("привет мир|, ёжик", "b"), "привет |мир, ёжик");
        assert_eq!(run(text, "dw"), "|мир, ёжик");
        assert_eq!(run(text, "cwпока<Esc>"), "пок|а мир, ёжик");
        assert_eq!(run(text, "x"), "|ривет мир, ёжик");
        assert_eq!(run("при|вет", "X"), "пр|вет");
        assert_eq!(run("|ёж", "~"), "Ё|ж");
        assert_eq!(run("|привет мир", "ywP"), "привет| привет мир");
        assert_eq!(run("пр|ивет мир", "ciwX<Esc>"), "|X мир");
        assert_eq!(run("при|вет мир", "daw"), "|мир");
    }

    #[test]
    fn find_motions() {
        assert_eq!(run("|a,b,c,d", "f,"), "a|,b,c,d");
        assert_eq!(run("|a,b,c,d", "2f,"), "a,b|,c,d");
        assert_eq!(run("|ab,c", "t,"), "a|b,c");
        assert_eq!(run("a,b,c|d", "F,"), "a,b|,cd");
        assert_eq!(run("|a,b,c,d", "f,;"), "a,b|,c,d");
        assert_eq!(run("|a,b,c,d", "f,;,"), "a|,b,c,d");
        assert_eq!(run("|a,b,c,d", "df,"), "|b,c,d");
        assert_eq!(run("|ab,c", "dt,"), "|,c");
        assert_eq!(
            run("|a,b", "fz"),
            "|a,b",
            "no such character: nothing happens"
        );
    }

    #[test]
    fn x_and_friends() {
        assert_eq!(run("a|bc", "x"), "a|c");
        assert_eq!(run("a|bcd", "2x"), "a|d");
        assert_eq!(run("ab|c", "x"), "a|b", "the caret steps back at the end");
        assert_eq!(run("a|bc", "X"), "|bc");
        assert_eq!(run("|abc", "X"), "|abc");
        assert_eq!(run("a|bcd", "99x"), "|a");
        assert_eq!(
            run("one\n|\ntwo", "x"),
            "one\n|\ntwo",
            "x on an empty line does nothing"
        );
        assert_eq!(run("|", "x"), "|");
    }

    #[test]
    fn delete_with_motions() {
        assert_eq!(run("|foo bar baz", "dw"), "|bar baz");
        assert_eq!(run("|foo bar baz", "2dw"), "|baz");
        assert_eq!(run("|foo bar baz", "d2w"), "|baz");
        assert_eq!(run("|foo bar baz", "de"), "| bar baz");
        assert_eq!(run("foo b|ar baz", "db"), "foo |ar baz");
        assert_eq!(run("foo b|ar\nbaz", "d0"), "|ar\nbaz");
        assert_eq!(run("  fo|o", "d^"), "  |o");
        assert_eq!(
            run("|foo\nbar", "dw"),
            "|\nbar",
            "dw on the last word keeps the newline"
        );
        assert_eq!(
            run("one |two\nthree", "dw"),
            "one| \nthree",
            "dw stops at the line end"
        );
    }

    #[test]
    fn delete_to_the_end_of_the_line() {
        assert_eq!(run("foo b|ar baz", "d$"), "foo |b");
        assert_eq!(run("foo b|ar baz", "D"), "foo |b");
        assert_eq!(run("foo b|ar\nbaz", "D"), "foo |b\nbaz");
        assert_eq!(run("foo b|ar\nbaz", "2D"), "foo |b");
    }

    #[test]
    fn delete_lines() {
        assert_eq!(run("one\n|two\nthree", "dd"), "one\n|three");
        assert_eq!(run("one\ntwo\n|three", "dd"), "one\n|two");
        assert_eq!(run("|one", "dd"), "|");
        assert_eq!(run("|one\ntwo\nthree", "2dd"), "|three");
        assert_eq!(run("|one\ntwo\nthree", "d2d"), "|three");
        assert_eq!(run("|one\ntwo\nthree", "9dd"), "|");
        assert_eq!(run("one\n  |two\nthree", "dj"), "|one");
        assert_eq!(run("one\n|two\nthree", "dk"), "|three");
        assert_eq!(run("one\n|two\nthree", "dG"), "|one");
        assert_eq!(run("one\ntwo\n|three", "dgg"), "|");
        assert_eq!(run("  one\n|two", "dd"), "  |one");
    }

    #[test]
    fn change_with_motions() {
        assert_eq!(run("|foo bar", "cwxy<Esc>"), "x|y bar");
        assert_eq!(run("|foo bar", "cexy<Esc>"), "x|y bar");
        assert_eq!(run("|foo bar", "c2wxy<Esc>"), "x|y");
        assert_eq!(run("foo b|ar", "cbxy<Esc>"), "foo x|yar");
        assert_eq!(run("foo b|ar baz", "c$X<Esc>"), "foo b|X");
        assert_eq!(run("foo |bar baz", "CX<Esc>"), "foo |X");
        assert_eq!(run("one\n|two\nthree", "ccX<Esc>"), "one\n|X\nthree");
        assert_eq!(run("one\n|two\nthree", "SX<Esc>"), "one\n|X\nthree");
        assert_eq!(run("a|bc", "sX<Esc>"), "a|Xc");
        assert_eq!(run("a|bcd", "2sX<Esc>"), "a|Xd");
        assert_eq!(run("|foo  bar", "cwX<Esc>"), "|X  bar");
        assert_eq!(
            run("foo| bar", "cwX<Esc>"),
            "foo|Xbar",
            "cw on a blank changes the blank"
        );
        assert_eq!(run("|a b", "cwX<Esc>"), "|X b", "a one-letter word");
        assert_eq!(run("|foo\nbar", "cjX<Esc>"), "|X");
    }

    #[test]
    fn change_enters_insert_and_escape_leaves() {
        let mut r = Rig::new("|foo bar");
        r.keys("cw");
        assert_eq!(r.vim.mode(), Mode::Insert);
        r.keys("zz");
        assert_eq!(r.text(), "zz bar", "keys are typed in Insert mode");
        r.keys("<Esc>");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(
            r.shown(),
            "z|z bar",
            "the caret steps back onto the last character"
        );
    }

    #[test]
    fn insert_commands() {
        assert_eq!(run("a|bc", "iX<Esc>"), "a|Xbc");
        assert_eq!(run("a|bc", "aX<Esc>"), "ab|Xc");
        assert_eq!(run("  a|bc", "IX<Esc>"), "  |Xabc");
        assert_eq!(run("a|bc\ndef", "AX<Esc>"), "abc|X\ndef");
        assert_eq!(run("a|bc\ndef", "oX<Esc>"), "abc\n|X\ndef");
        assert_eq!(run("abc\nd|ef", "OX<Esc>"), "abc\n|X\ndef");
        assert_eq!(run("|", "aX<Esc>"), "|X");
        assert_eq!(
            run("ab|c", "a<Esc>"),
            "ab|c",
            "a on the last character appends"
        );
        assert_eq!(
            run("|abc", "i<Esc>"),
            "|abc",
            "Escape at the line start stays"
        );
    }

    #[test]
    fn yank_and_put() {
        assert_eq!(run("|foo bar", "ywP"), "foo| foo bar");
        assert_eq!(run("|foo bar", "yep"), "ffo|ooo bar");
        assert_eq!(run("|foo bar", "y$p"), "ffoo ba|roo bar");
        assert_eq!(run("one\n|two\nthree", "yyp"), "one\ntwo\n|two\nthree");
        assert_eq!(run("one\n|two\nthree", "yyP"), "one\n|two\ntwo\nthree");
        assert_eq!(run("one\ntwo\n|three", "yyp"), "one\ntwo\nthree\n|three");
        assert_eq!(run("|one\ntwo", "yy2p"), "one\n|one\none\ntwo");
        assert_eq!(run("|foo bar", "dwp"), "bfoo| ar");
        assert_eq!(run("ab|c", "xP"), "a|cb");
        assert_eq!(run("|abc", "p"), "|abc", "an empty register puts nothing");
        assert_eq!(run("|one\ntwo", "ddp"), "two\n|one");
    }

    #[test]
    fn yank_leaves_the_text_alone() {
        let mut r = Rig::new("foo |bar");
        r.keys("yw");
        assert_eq!(r.shown(), "foo |bar");
        assert_eq!(r.vim.register(), ("bar", false));
        r.keys("yy");
        assert_eq!(r.vim.register(), ("foo bar", true));
        let mut r = Rig::new("foo ba|r");
        r.keys("yb");
        assert_eq!(r.vim.register(), ("ba", false));
        assert_eq!(
            r.shown(),
            "foo |bar",
            "yb moves the caret to where the yanked text starts"
        );
    }

    #[test]
    fn deletes_fill_the_register() {
        let mut r = Rig::new("|foo bar");
        r.keys("dw");
        assert_eq!(r.vim.register(), ("foo ", false));
        r.keys("x");
        assert_eq!(r.vim.register(), ("b", false));
        let mut r = Rig::new("one\n|two");
        r.keys("dd");
        assert_eq!(r.vim.register(), ("two", true));
    }

    #[test]
    fn replace_and_toggle_case() {
        assert_eq!(run("|abc", "rX"), "|Xbc");
        assert_eq!(run("|abc", "2rX"), "X|Xc");
        assert_eq!(run("|abc", "9rX"), "|abc", "not enough characters: nothing");
        assert_eq!(run("|abc", "r<Esc>"), "|abc");
        assert_eq!(run("|aBc", "~"), "A|Bc");
        assert_eq!(run("|aBc", "3~"), "Ab|C");
        assert_eq!(run("a|Bc", "99~"), "ab|C");
        assert_eq!(run("|аБ", "2~"), "А|б");
    }

    #[test]
    fn undo_and_redo() {
        let mut r = Rig::new("|foo bar");
        r.keys("dw");
        assert_eq!(r.text(), "bar");
        r.keys("x");
        assert_eq!(r.text(), "ar");
        r.keys("u");
        assert_eq!(r.text(), "bar");
        r.keys("u");
        assert_eq!(
            r.shown(),
            "|foo bar",
            "the caret goes back to where the change was"
        );
        r.keys("u");
        assert_eq!(r.text(), "foo bar", "nothing left to undo");
        r.keys("<C-r>");
        assert_eq!(r.text(), "bar");
        r.keys("<C-r><C-r>");
        assert_eq!(r.text(), "ar");
        r.keys("uu<C-r>");
        assert_eq!(r.text(), "bar");
    }

    #[test]
    fn a_change_with_its_typing_is_one_undo_step() {
        let mut r = Rig::new("|foo bar");
        r.keys("cwhello<Esc>");
        assert_eq!(r.text(), "hello bar");
        r.keys("u");
        assert_eq!(r.text(), "foo bar");
        r.keys("<C-r>");
        assert_eq!(r.text(), "hello bar");
        r.keys("oline two<Esc>");
        assert_eq!(r.text(), "hello bar\nline two");
        r.keys("u");
        assert_eq!(r.text(), "hello bar");
    }

    #[test]
    fn undo_takes_a_count() {
        let mut r = Rig::new("|abcdef");
        r.keys("xxx");
        assert_eq!(r.text(), "def");
        r.keys("2u");
        assert_eq!(r.text(), "bcdef");
    }

    #[test]
    fn dot_repeats_x_dw_cw_and_dd() {
        let mut r = Rig::new("|abcd");
        r.keys("x.");
        assert_eq!(r.text(), "cd");

        let mut r = Rig::new("|one two three four");
        r.keys("dw.");
        assert_eq!(r.text(), "three four");

        let mut r = Rig::new("|one two three");
        r.keys("cwX<Esc>w.");
        assert_eq!(r.text(), "X X three");
        r.keys("w.");
        assert_eq!(r.text(), "X X X");

        let mut r = Rig::new("|a\nb\nc\nd");
        r.keys("dd.");
        assert_eq!(r.text(), "c\nd");

        let mut r = Rig::new("|abc");
        r.keys("rXl.");
        assert_eq!(r.text(), "XXc");

        let mut r = Rig::new("|abc abc");
        r.keys("ixy<Esc>.");
        assert_eq!(r.text(), "xxyyabc abc");
    }

    #[test]
    fn dot_without_a_change_does_nothing() {
        let mut r = Rig::new("|abc");
        r.keys(".");
        assert_eq!(r.text(), "abc");
        r.keys("l.");
        assert_eq!(r.text(), "abc", "a motion is not a change");
        r.keys("x");
        r.keys("l.");
        assert_eq!(
            r.text(),
            "a",
            "the last change is the x, not the motion after it"
        );
    }

    #[test]
    fn a_dot_is_one_undo_step() {
        let mut r = Rig::new("|abcd");
        r.keys("x.");
        r.keys("u");
        assert_eq!(r.text(), "bcd");
    }

    #[test]
    fn visual_motions_extend_and_operators_act() {
        let mut r = Rig::new("|foo bar baz");
        r.keys("v");
        assert_eq!(r.vim.mode(), Mode::Visual);
        r.keys("e");
        assert_eq!(r.vim.selection(&r.input), Some(0..3));
        r.keys("w");
        assert_eq!(r.vim.selection(&r.input), Some(0..5));
        r.keys("d");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.text(), "ar baz");
        assert_eq!(r.vim.register(), ("foo b", false));

        assert_eq!(run("|foo bar", "vey"), "|foo bar");
        assert_eq!(run("|foo bar", "veyP"), "fo|ofoo bar");
        assert_eq!(run("|foo bar", "vlx"), "|o bar");
        assert_eq!(run("|foo bar", "vecX<Esc>"), "|X bar");
        assert_eq!(run("foo b|ar", "vbd"), "foo |r");
        assert_eq!(run("|foo bar", "vls!<Esc>"), "|!o bar");
    }

    #[test]
    fn visual_escape_and_v_return_to_normal() {
        let mut r = Rig::new("|foo");
        r.keys("vl<Esc>");
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.vim.selection(&r.input), None);
        r.keys("vlv");
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_o_swaps_the_ends_and_tilde_toggles() {
        let mut r = Rig::new("|foo bar");
        r.keys("vlllo");
        assert_eq!(r.input.cursor(), 0);
        assert_eq!(r.vim.selection(&r.input), Some(0..4));
        r.keys("~");
        assert_eq!(r.text(), "FOO bar");
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn visual_selection_spans_lines() {
        assert_eq!(run("a|bc\ndef", "vjd"), "a|f");
        assert_eq!(run("a|bc\ndef", "vjy"), "a|bc\ndef");
        let mut r = Rig::new("a|bc\ndef");
        r.keys("vjy");
        assert_eq!(r.vim.register(), ("bc\nde", false));
    }

    #[test]
    fn text_objects() {
        assert_eq!(run("foo b|ar baz", "diw"), "foo | baz");
        assert_eq!(run("foo b|ar baz", "daw"), "foo |baz");
        assert_eq!(run("foo bar b|az", "daw"), "foo ba|r");
        assert_eq!(run("foo b|ar baz", "ciwX<Esc>"), "foo |X baz");
        assert_eq!(run("foo b|ar baz", "yiwP"), "foo ba|rbar baz");
        assert_eq!(run("foo.b|ar", "diw"), "foo|.");
        assert_eq!(run("foo.b|ar", "diW"), "|");
        assert_eq!(run("fo|o bar", "viwd"), "| bar");
    }

    #[test]
    fn single_line_rules() {
        assert_eq!(Rig::single("ab|c").keys("j").shown(), "ab|c");
        assert_eq!(Rig::single("ab|c").keys("k").shown(), "ab|c");
        assert_eq!(Rig::single("ab|c").keys("dd").shown(), "|");
        assert_eq!(
            Rig::single("ab|c").keys("dd").vim.register(),
            ("abc", false)
        );
        assert_eq!(Rig::single("ab|c").keys("ccX<Esc>").shown(), "|X");
        assert_eq!(Rig::single("ab|c").keys("yyP").shown(), "ab|cabc");
        assert_eq!(Rig::single("a|bc").keys("dG").shown(), "|");
        assert_eq!(Rig::single("a|bc").keys("ox<Esc>").shown(), "abc|x");
        assert_eq!(Rig::single("a|bc").keys("Ox<Esc>").shown(), "|xabc");
    }

    #[test]
    fn single_line_paste_flattens_newlines() {
        let mut r = Rig::single("|ab");
        r.opts.use_system_clipboard = true;
        r.clip = Some("x\ny\n".into());
        r.keys("p");
        assert_eq!(r.text(), "ax yb");
    }

    #[test]
    fn enter_and_tab_and_escape_in_normal_are_the_fields_own() {
        let mut r = Rig::new("|abc");
        assert_eq!(r.key(Key::Enter), Outcome::Ignored);
        assert_eq!(r.key(Key::Tab), Outcome::Ignored);
        assert_eq!(r.key(Key::Escape), Outcome::Ignored);
        assert_eq!(
            r.key(Key::Char('z')),
            Outcome::Handled,
            "unknown keys are swallowed"
        );
        assert_eq!(r.text(), "abc");
    }

    #[test]
    fn escape_cancels_a_pending_command() {
        let mut r = Rig::new("|abc def");
        assert_eq!(r.key(Key::Char('d')), Outcome::Handled);
        assert_eq!(r.vim.pending_keys(), "d");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        r.keys("x");
        assert_eq!(
            r.text(),
            "bc def",
            "the d was cancelled: x deletes one character"
        );
        let mut r = Rig::new("|abc def");
        r.keys("2<Esc>x");
        assert_eq!(r.text(), "bc def");
        r.keys("d2");
        assert_eq!(r.vim.pending_keys(), "d2");
    }

    #[test]
    fn insert_mode_types_and_escape_leaves() {
        let mut r = Rig::new("|");
        r.keys("i");
        r.keys("ab<BS>c<Left>X");
        assert_eq!(r.shown(), "aX|c");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        assert_eq!(r.vim.mode(), Mode::Normal);
        assert_eq!(r.shown(), "a|Xc");
    }

    #[test]
    fn insert_passthrough_only_takes_escape() {
        let mut r = Rig::new("ab|");
        r.keys("i");
        r.opts.insert_passthrough = true;
        assert_eq!(r.key(Key::Char('x')), Outcome::Ignored);
        assert_eq!(r.text(), "ab");
        assert_eq!(r.key(Key::Escape), Outcome::Handled);
        assert_eq!(r.vim.mode(), Mode::Normal);
    }

    #[test]
    fn dot_repeats_text_a_field_typed_for_itself() {
        // The field types in Insert mode on its own; the text it typed is found by the caret.
        let mut r = Rig::new("|one two");
        r.opts.insert_passthrough = true;
        r.keys("cw");
        r.input.insert("1");
        r.input.insert("2");
        r.keys("<Esc>");
        assert_eq!(r.text(), "12 two");
        r.keys("w.");
        assert_eq!(r.text(), "12 12");
    }

    #[test]
    fn the_system_clipboard_follows_the_register() {
        let mut r = Rig::new("|foo bar");
        r.opts.use_system_clipboard = true;
        r.keys("dw");
        assert_eq!(r.clip.as_deref(), Some("foo "));
        r.keys("yy");
        assert_eq!(
            r.clip.as_deref(),
            Some("bar\n"),
            "a line goes with its newline"
        );
        r.clip = Some("from outside".into());
        r.keys("P");
        assert_eq!(r.text(), "from outsidebar");
    }

    #[test]
    fn clipboard_lines_paste_as_lines() {
        let mut r = Rig::new("one\n|two");
        r.opts.use_system_clipboard = true;
        r.clip = Some("new\n".into());
        r.keys("p");
        assert_eq!(r.text(), "one\ntwo\nnew");
        r.keys("P");
        assert_eq!(r.text(), "one\ntwo\nnew\nnew");
    }

    #[test]
    fn the_register_is_private_without_the_system_clipboard() {
        let mut r = Rig::new("|foo bar");
        r.opts.use_system_clipboard = false;
        r.clip = Some("outside".into());
        r.keys("dw");
        assert_eq!(r.clip.as_deref(), Some("outside"), "nothing is written");
        r.keys("P");
        assert_eq!(
            r.text(),
            "foo bar",
            "the register, not the clipboard, is put"
        );
    }

    #[test]
    fn the_caret_stays_on_a_character_after_every_command() {
        for keys in ["$", "x$x", "dw", "d$", "wwwx", "ddk", "Gdd"] {
            let mut r = Rig::new("|foo\nbar baz");
            r.keys(keys);
            let (text, at) = (r.text(), r.input.cursor());
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
            assert!(
                at < line_end || line_end == line_start,
                "after {keys}: {}",
                r.shown()
            );
        }
    }

    #[test]
    fn multibyte_text_survives_every_operator() {
        let text = "|héllo wörld ✓ ok\nsecond ünï line";
        for keys in [
            "dw",
            "de",
            "db",
            "x",
            "X",
            "D",
            "dd",
            "cwx<Esc>",
            "ccx<Esc>",
            "yyp",
            "ywP",
            "~",
            "rX",
            "wwwd$",
            "$dB",
            "jdk",
            "vjd",
            "wveyP",
            "u",
            "daw",
            "ciwX<Esc>",
            "fwx",
            "tld",
        ] {
            let mut r = Rig::new(text);
            r.keys(keys);
            assert!(
                r.input.text().is_char_boundary(r.input.cursor()),
                "after {keys}: {}",
                r.shown()
            );
        }
    }

    #[test]
    fn reset_returns_to_insert_and_keeps_the_register() {
        let mut r = Rig::new("|foo bar");
        r.keys("yw");
        r.keys("d");
        r.vim.reset();
        assert_eq!(r.vim.mode(), Mode::Insert);
        assert_eq!(r.vim.pending_keys(), "");
        assert_eq!(r.vim.register(), ("foo ", false));
    }
}
