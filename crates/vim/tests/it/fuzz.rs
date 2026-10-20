//! Random keys over random text: the host contract holds (the reference host asserts it on every
//! call), the caret rests where Normal mode allows, and everything is undoable.

use hutan_vim::{Mode, Options, VimHost};
use proptest::prelude::*;

use crate::common::{Rig, StringHost, parse_keys};

const PIECES: &[&str] = &[
    "a", "b", "foo", " ", "  ", "é", "日", "🦀", "_", ".", ",", "\n", "\n", "\t", "ß", "Ab", "x-y",
    "(", ")",
];

const KEYS: &[&str] = &[
    "h", "j", "k", "l", "w", "b", "e", "W", "B", "E", "0", "^", "$", "G", "g", "f", "t", "F", "T",
    "d", "c", "y", "x", "X", "D", "C", "s", "S", "Y", "i", "a", "A", "I", "o", "O", "p", "P", "u",
    "v", "r", "~", ".", ";", ",", "1", "2", "3", "9", "0", " ", "a", "b", "é", "日", "ß", "\n",
    "-", "_", "<Esc>", "<Esc>", "<CR>", "<BS>", "<Left>", "<C-r>",
];

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(PIECES), 0..14).prop_map(|v| v.concat())
}

fn keys() -> impl Strategy<Value = Vec<&'static str>> {
    proptest::collection::vec(proptest::sample::select(KEYS), 1..60)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

fn options() -> impl Strategy<Value = Options> {
    (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(single_line, passthrough, clip)| {
        Options {
            single_line,
            insert_passthrough: passthrough,
            use_system_clipboard: clip,
        }
    })
}

fn rig(text: &str, at_end: bool, opts: Options) -> Rig {
    let mut r = Rig::new(text);
    r.opts = opts;
    let cursor = if at_end { text.len() } else { 0 };
    r.host = StringHost::with(text, cursor);
    r
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_caret_and_the_selection_stay_valid(
        text in text(), at_end in any::<bool>(), opts in options(), keys in keys(),
    ) {
        let mut r = rig(&text, at_end, opts);
        for token in &keys {
            for key in parse_keys(token) {
                r.key(key);
                let (t, at) = (r.text(), r.host.cursor());
                prop_assert!(at <= t.len() && t.is_char_boundary(at), "caret {at} in {t:?}");
                if r.mode() == Mode::Visual {
                    let sel = r.vim.selection(&r.host).unwrap();
                    prop_assert!(sel.start <= sel.end && sel.end <= t.len());
                    prop_assert!(t.is_char_boundary(sel.start) && t.is_char_boundary(sel.end));
                }
                if r.mode() == Mode::Normal && r.vim.pending_keys().is_empty() {
                    prop_assert!(!r.host.is_grouped(), "a finished command leaves its group");
                    let ls = t[..at].rfind('\n').map_or(0, |i| i + 1);
                    let le = t[at..].find('\n').map_or(t.len(), |i| at + i);
                    prop_assert!(at < le || le == ls, "Normal mode rests on a character: {t:?} at {at}");
                }
            }
        }
    }

    #[test]
    fn everything_undoes_and_redoes(
        text in text(), at_end in any::<bool>(), mut opts in options(), keys in keys(),
    ) {
        opts.insert_passthrough = false;
        let mut r = rig(&text, at_end, opts);
        for token in &keys {
            r.keys(token);
        }
        r.keys("<Esc><Esc>");
        for _ in 0..200 {
            r.keys("u");
        }
        prop_assert_eq!(r.text(), text.as_str());
        for _ in 0..200 {
            r.keys("<C-r>");
        }
        let redone = r.text().to_owned();
        for _ in 0..200 {
            r.keys("u");
        }
        prop_assert_eq!(r.text(), text.as_str(), "undo after redo ({:?})", redone);
    }
}
