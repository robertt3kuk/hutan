//! The snippet subset a completion may insert (slice E-12b): tab stops `$1`, `${1}`, the
//! placeholder `${1:text}` and the final stop `$0`, with the escapes `\$`, `\}` and `\\`.
//! Anything else — choices `${1|a,b|}`, variables `$TM_FILENAME`, nested placeholders,
//! transformations — is inserted as plain text, exactly as the server wrote it.

use std::ops::Range;

/// A snippet made plain: the text to insert and its tab stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snippet {
    pub text: String,
    /// The tab stops in the order Tab visits them (`$1`, `$2`, …, then the final one: `$0`, or
    /// the end of the text), each with the byte ranges in `text` of all its occurrences (the
    /// placeholder's text, or empty).
    pub stops: Vec<Vec<Range<usize>>>,
}

impl Snippet {
    /// Text that is not a snippet: no stops but the end.
    pub fn plain(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            stops: vec![vec![end..end]],
        }
    }
}

/// The digits at the start of `s`, as a number, and how many bytes they take.
fn number(s: &str) -> Option<(u32, usize)> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > 4 {
        return None;
    }
    s[..len].parse().ok().map(|n| (n, len))
}

/// `${N:placeholder}` (or `${N}`) at the start of `s` (which starts after `${`): the number,
/// the placeholder with its escapes resolved, and the bytes taken up to the `}`. `None` for
/// anything the subset does not have (a nested `$`, a choice, a transformation).
fn braced(s: &str) -> Option<(u32, String, usize)> {
    let (n, mut at) = number(s)?;
    let mut placeholder = String::new();
    match s[at..].chars().next()? {
        '}' => return Some((n, placeholder, at + 1)),
        ':' => at += 1,
        _ => return None,
    }
    let mut chars = s[at..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '}' => return Some((n, placeholder, at + i + 1)),
            '$' => return None,
            '\\' => match chars.next() {
                Some((_, escaped @ ('$' | '}' | '\\'))) => placeholder.push(escaped),
                Some((_, other)) => {
                    placeholder.push('\\');
                    placeholder.push(other);
                }
                None => placeholder.push('\\'),
            },
            _ => placeholder.push(c),
        }
    }
    None
}

/// Parses `snippet` (see the module docs).
pub(crate) fn parse(snippet: &str) -> Snippet {
    let mut text = String::with_capacity(snippet.len());
    // (stop number, range in `text`)
    let mut found: Vec<(u32, Range<usize>)> = Vec::new();
    let mut rest = snippet;
    while let Some(c) = rest.chars().next() {
        match c {
            '\\' if matches!(rest[1..].chars().next(), Some('$' | '}' | '\\')) => {
                let escaped = rest[1..].chars().next().unwrap_or('\\');
                text.push(escaped);
                rest = &rest[1 + escaped.len_utf8()..];
            }
            '$' => {
                if let Some((n, len)) = number(&rest[1..]) {
                    found.push((n, text.len()..text.len()));
                    rest = &rest[1 + len..];
                } else if let Some((n, placeholder, len)) =
                    rest[1..].strip_prefix('{').and_then(braced)
                {
                    let start = text.len();
                    text.push_str(&placeholder);
                    found.push((n, start..text.len()));
                    rest = &rest[2 + len..];
                } else {
                    text.push('$');
                    rest = &rest[1..];
                }
            }
            _ => {
                text.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    let mut numbers: Vec<u32> = found.iter().map(|(n, _)| *n).filter(|n| *n > 0).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let mut stops: Vec<Vec<Range<usize>>> = numbers
        .iter()
        .map(|n| {
            found
                .iter()
                .filter(|(m, _)| m == n)
                .map(|(_, r)| r.clone())
                .collect()
        })
        .collect();
    let last: Vec<Range<usize>> = found
        .iter()
        .filter(|(n, _)| *n == 0)
        .map(|(_, r)| r.clone())
        .take(1)
        .collect();
    stops.push(if last.is_empty() {
        vec![text.len()..text.len()]
    } else {
        last
    });
    Snippet { text, stops }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_stops_placeholders_and_the_final_stop() {
        let s = parse("push(${1:value}, $2)$0");
        assert_eq!(s.text, "push(value, )");
        assert_eq!(s.stops, vec![vec![5..10], vec![12..12], vec![13..13]]);
    }

    #[test]
    fn the_end_is_the_final_stop_without_a_zero_and_repeats_share_a_stop() {
        let s = parse("${1:a} = ${1:a}; ${2}");
        assert_eq!(s.text, "a = a; ");
        assert_eq!(s.stops, vec![vec![0..1, 4..5], vec![7..7], vec![7..7]]);
    }

    #[test]
    fn anything_else_is_plain_text() {
        assert_eq!(parse("${1|a,b|}").text, "${1|a,b|}");
        assert_eq!(parse("$TM_FILENAME x").text, "$TM_FILENAME x");
        assert_eq!(parse("${1:outer ${2:inner}}").text, "${1:outer inner}");
        assert_eq!(parse(r"cost \$5 \} \\").text, r"cost $5 } \");
        assert_eq!(parse("${1:a\\}b}").text, "a}b");
        assert_eq!(parse("trailing $").text, "trailing $");
        assert_eq!(parse("é$1ü").stops[0], vec![2..2]);
    }
}
