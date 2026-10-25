//! Clipboard text and dropped paths → PTY bytes (GPUI-free).

use std::path::Path;

const BRACKETED_START: &str = "\x1b[200~";
const BRACKETED_END: &str = "\x1b[201~";

/// Bytes to send for pasted `text`.
///
/// With bracketed paste on, the text is wrapped in `ESC[200~ … ESC[201~` and every embedded
/// `ESC[201~` is removed first (repeatedly, so removal cannot assemble a new one): clipboard
/// content must not be able to end the paste early and run as typed input. Without bracketed
/// paste, line breaks become carriage returns like typed Enter.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut clean = text.to_owned();
        while clean.contains(BRACKETED_END) {
            clean = clean.replace(BRACKETED_END, "");
        }
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(BRACKETED_START.as_bytes());
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(BRACKETED_END.as_bytes());
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Whether pasting `text` into a shell that does not know bracketed paste would run more than
/// one line: it has a line break that is not just its last character.
pub fn is_multiline(text: &str) -> bool {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .unwrap_or(text);
    body.contains(['\n', '\r'])
}

/// The lines of `text` as the confirmation counts and previews them: line breaks of any style,
/// one trailing break ignored.
pub fn lines(text: &str) -> Vec<&str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix(['\n', '\r']))
        .unwrap_or(text);
    body.split("\r\n")
        .flat_map(|chunk| chunk.split(['\n', '\r']))
        .collect()
}

/// `text` as a single line: every run of line breaks (with the blanks around it) becomes one
/// space, and breaks at either end are dropped. Nothing runs until the user presses Enter.
pub fn as_one_line(text: &str) -> String {
    lines(text)
        .into_iter()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `path` as one word for a POSIX-like shell and fish: left alone when it only holds characters
/// no shell treats specially, else in single quotes (a quote inside becomes `'\''`).
pub fn shell_quote(path: &str) -> String {
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_alphanumeric() || "_@%+:,./-".contains(c));
    if plain {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// `path` as one word with a backslash before every character a shell treats specially (the
/// form Terminal.app types for a dropped file, which the agents' prompts read back): the same
/// characters stay plain as in [`shell_quote`].
pub fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c.is_alphanumeric() || "_@%+:,./-".contains(c)) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What dropping (or pasting from Finder) `paths` types: each path quoted, separated and
/// followed by a space, as Terminal.app does. Paths that are not UTF-8 or hold a control
/// character (which a shell would act on) are left out; `None` when nothing is left.
pub fn dropped_paths_text<P: AsRef<Path>>(paths: &[P]) -> Option<String> {
    paths_text(paths, None)
}

/// [`dropped_paths_text`] for the prompt of `agent` (see [`crate::image_paste::path_text`]);
/// `None` is a shell.
pub fn paths_text<P: AsRef<Path>>(
    paths: &[P],
    agent: Option<crate::image_paste::PasteAgent>,
) -> Option<String> {
    let mut out = String::new();
    for path in paths {
        let Some(text) = crate::image_paste::path_text(path.as_ref(), agent) else {
            continue;
        };
        out.push_str(&text);
        out.push(' ');
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracketed_paste_wraps_the_text() {
        assert_eq!(paste_bytes("ls\nls", true), b"\x1b[200~ls\nls\x1b[201~");
    }

    #[test]
    fn bracketed_paste_strips_embedded_terminators() {
        assert_eq!(
            paste_bytes("a\x1b[201~rm -rf /\n", true),
            b"\x1b[200~arm -rf /\n\x1b[201~"
        );
        // Removing one terminator must not assemble another.
        assert_eq!(
            paste_bytes("\x1b[2\x1b[201~01~x", true),
            b"\x1b[200~x\x1b[201~"
        );
    }

    #[test]
    fn plain_paste_turns_line_breaks_into_returns() {
        assert_eq!(paste_bytes("a\nb\r\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("héllo", false), "héllo".as_bytes());
    }

    #[test]
    fn plain_paste_leaves_terminators_alone() {
        assert_eq!(paste_bytes("\x1b[201~", false), b"\x1b[201~");
    }

    #[test]
    fn multiline_means_a_break_before_the_end() {
        assert!(!is_multiline(""));
        assert!(!is_multiline("ls"));
        assert!(!is_multiline("ls\n"));
        assert!(!is_multiline("ls\r\n"));
        assert!(is_multiline("a\nb"));
        assert!(is_multiline("a\r\nb\r\n"));
        assert!(is_multiline("a\rb"));
        assert!(is_multiline("\n\nls"));
    }

    #[test]
    fn lines_and_one_line() {
        assert_eq!(lines("a\r\nb\nc\rd\n"), ["a", "b", "c", "d"]);
        assert_eq!(as_one_line("  a \n\n b\r\n c\n"), "a b c");
        assert_eq!(as_one_line("ls"), "ls");
        assert_eq!(as_one_line("\n\n"), "");
    }

    #[test]
    fn paths_are_quoted_only_when_a_shell_would_care() {
        assert_eq!(shell_quote("/tmp/a-b_c.rs"), "/tmp/a-b_c.rs");
        assert_eq!(shell_quote("/tmp/my file.txt"), "'/tmp/my file.txt'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a$b;c&d"), "'a$b;c&d'");
        assert_eq!(shell_quote("~x"), "'~x'");
        assert_eq!(shell_quote("日本語.txt"), "日本語.txt");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn dropped_paths_are_quoted_spaced_and_end_with_a_space() {
        assert_eq!(
            dropped_paths_text(&["/tmp/a.rs", "/tmp/my dir/b c.txt"]).as_deref(),
            Some("/tmp/a.rs '/tmp/my dir/b c.txt' ")
        );
        // A control character would be acted on by the line editor: the path is left out.
        assert_eq!(
            dropped_paths_text(&["/tmp/a\nb", "/tmp/ok"]).as_deref(),
            Some("/tmp/ok ")
        );
        assert_eq!(dropped_paths_text(&["/tmp/a\x1bb"]), None);
        assert_eq!(dropped_paths_text::<&str>(&[]), None);
    }

    #[test]
    fn an_agents_prompt_gets_the_backslash_form() {
        use crate::image_paste::PasteAgent;
        assert_eq!(escape_path("/tmp/a-b_c.png"), "/tmp/a-b_c.png");
        assert_eq!(
            escape_path("/Users/me/Library/Application Support/Hutan/it's.png"),
            "/Users/me/Library/Application\\ Support/Hutan/it\\'s.png"
        );
        assert_eq!(escape_path("日本 語"), "日本\\ 語");
        for agent in [PasteAgent::ClaudeCode, PasteAgent::Codex, PasteAgent::Omp] {
            assert_eq!(
                paths_text(&["/tmp/my dir/a.png"], Some(agent)).as_deref(),
                Some("/tmp/my\\ dir/a.png ")
            );
        }
        assert_eq!(
            paths_text(&["/tmp/my dir/a.png"], None).as_deref(),
            Some("'/tmp/my dir/a.png' ")
        );
        assert_eq!(paths_text(&["/tmp/a\nb"], Some(PasteAgent::Omp)), None);
    }
}
