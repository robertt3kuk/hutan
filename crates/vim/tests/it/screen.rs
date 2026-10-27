//! Soft wrap (EDITOR-WRAP): `gj` `gk` `g0` `g^` `g$` move by the display rows a host reports
//! (`VimHost::display_row`), `j` `k` stay on lines, and a host that does not wrap answers them
//! as `j` `k` `0` `^` `$`. The reference host wraps every 10 characters.

use crate::common::{Rig, run};

fn wrapped(text: &str, keys: &str) -> String {
    Rig::wrapped(text, 10).keys(keys).shown()
}

#[test]
fn gj_and_gk_move_by_display_rows_keeping_the_column() {
    let at3 = "012|3456789abcdefghijklmnopqrstuvwxyz";
    assert_eq!(wrapped(at3, "gj"), "0123456789abc|defghijklmnopqrstuvwxyz");
    assert_eq!(
        wrapped(at3, "gjgj"),
        "0123456789abcdefghijklm|nopqrstuvwxyz"
    );
    assert_eq!(wrapped(at3, "3gj"), "0123456789abcdefghijklmnopqrstuvw|xyz");
    // Past the end of the last row: its last character.
    let at8 = "01234567|89abcdefghijklmnopqrstuvwxyz";
    assert_eq!(wrapped(at8, "3gj"), "0123456789abcdefghijklmnopqrstuvwxy|z");
    // gk goes back, and the column the caret wanted comes back with it.
    assert_eq!(
        wrapped(at8, "3gjgk"),
        "0123456789abcdefghijklmnopqr|stuvwxyz"
    );
    assert_eq!(
        wrapped("0123456789abcdefghijklm|nopqrstuvwxyz", "gk"),
        "0123456789abc|defghijklmnopqrstuvwxyz"
    );
    assert_eq!(
        wrapped("0123456789abcdefghijklm|nopqrstuvwxyz", "gkgk"),
        "012|3456789abcdefghijklmnopqrstuvwxyz"
    );
    // Nowhere to go: nothing moves.
    assert_eq!(wrapped(at3, "gk"), "012|3456789abcdefghijklmnopqrstuvwxyz");
    assert_eq!(
        wrapped("0123456789abcdefghijklmnopqrstuvwx|yz", "gj"),
        "0123456789abcdefghijklmnopqrstuvwx|yz"
    );
}

#[test]
fn gj_crosses_from_the_last_row_of_a_line_to_the_next_line() {
    let text = "01234|56789abc\nxyzxyzxyz";
    assert_eq!(wrapped(text, "gj"), "0123456789ab|c\nxyzxyzxyz");
    assert_eq!(wrapped(text, "gjgj"), "0123456789abc\nxyzxy|zxyz");
    assert_eq!(wrapped(text, "gjgjgk"), "0123456789ab|c\nxyzxyzxyz");
    assert_eq!(wrapped(text, "gjgjgkgk"), "01234|56789abc\nxyzxyzxyz");
}

#[test]
fn j_and_k_stay_on_lines() {
    assert_eq!(
        wrapped("012|3456789abcdefghijklmn\nxyz", "j"),
        "0123456789abcdefghijklmn\nxy|z"
    );
    assert_eq!(
        wrapped("0123456789abcdefghijklmn\nx|yz", "k"),
        "0|123456789abcdefghijklmn\nxyz"
    );
}

#[test]
fn g0_g_caret_and_g_dollar_go_to_the_edges_of_the_row() {
    assert_eq!(
        wrapped("0123456789abc|defghijklmnopqrstuvwxyz", "g0"),
        "0123456789|abcdefghijklmnopqrstuvwxyz"
    );
    assert_eq!(
        wrapped("0123456789abc|defghijklmnopqrstuvwxyz", "g$"),
        "0123456789abcdefghi|jklmnopqrstuvwxyz"
    );
    assert_eq!(
        wrapped("0123456789abcdefghijklmnopqrst|uvwxyz", "g$"),
        "0123456789abcdefghijklmnopqrstuvwxy|z"
    );
    assert_eq!(
        wrapped("|0123456789abcdefghijklmnopqrstuvwxyz", "$"),
        "0123456789abcdefghijklmnopqrstuvwxy|z"
    );
    // With a count, the end of the row n - 1 rows down.
    assert_eq!(
        wrapped("|0123456789abcdefghijklmnopqrstuvwxyz", "2g$"),
        "0123456789abcdefghi|jklmnopqrstuvwxyz"
    );
    // The first non-blank of the row: the row's own start where it has no indent.
    assert_eq!(
        wrapped("    abcd|efghijklmnop", "g^"),
        "    |abcdefghijklmnop"
    );
    assert_eq!(
        wrapped("    abcdefghi|jklmnop", "g^"),
        "    abcdef|ghijklmnop"
    );
}

#[test]
fn an_operator_with_gj_is_characterwise_and_exclusive() {
    assert_eq!(
        wrapped("012|3456789abcdefghijklmnopqrstuvwxyz", "dgj"),
        "012|defghijklmnopqrstuvwxyz"
    );
    assert_eq!(
        wrapped("012|3456789abcdefghijklmnopqrstuvwxyz", "dg$"),
        "012|abcdefghijklmnopqrstuvwxyz",
        "inclusive: the last character of the row goes too"
    );
    assert_eq!(
        wrapped("012|3456789abcdefghijklmnopqrstuvwxyz", "d2gj"),
        "012|nopqrstuvwxyz"
    );
}

#[test]
fn without_soft_wrap_they_are_j_k_0_and_dollar() {
    assert_eq!(run("abc|def\nxy", "gj"), "abcdef\nx|y");
    assert_eq!(run("abcdef\nx|y", "gk"), "a|bcdef\nxy");
    assert_eq!(run("abc|def", "g0"), "|abcdef");
    assert_eq!(run("abc|def", "g$"), "abcde|f");
    assert_eq!(run("  abc|def", "g^"), "  |abcdef");
    // Linewise, as `dj` is.
    assert_eq!(run("a|bc\nxy\nz", "dgj"), "|z");
}
