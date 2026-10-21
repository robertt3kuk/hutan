//! What editing needs to know about a file's language: the indentation unit, the comment
//! markers, which brackets and quotes pair, and what a word is.

use hutan_buffer::Text;
use hutan_syntax::Language;

use crate::layout::TAB_SIZE;

/// One level of indentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentUnit {
    Tab,
    Spaces(usize),
}

impl Default for IndentUnit {
    fn default() -> Self {
        IndentUnit::Spaces(TAB_SIZE)
    }
}

/// Lines looked at to guess a file's indentation.
const DETECT_LINES: usize = 2_000;

impl IndentUnit {
    /// The text of `levels` levels.
    pub fn text(self, levels: usize) -> String {
        match self {
            IndentUnit::Tab => "\t".repeat(levels),
            IndentUnit::Spaces(n) => " ".repeat(n * levels),
        }
    }

    /// Columns one level takes.
    pub fn columns(self) -> usize {
        match self {
            IndentUnit::Tab => TAB_SIZE,
            IndentUnit::Spaces(n) => n,
        }
    }

    /// The unit a file uses: tabs when most indented lines start with one, else the most common
    /// step between the indentation of consecutive lines (2, 3, 4 or 8 spaces); 4 spaces when the
    /// file says nothing.
    pub fn detect(text: &Text) -> IndentUnit {
        let (mut tabs, mut spaces) = (0usize, 0usize);
        let mut steps = [0usize; 9];
        let mut previous = 0usize;
        for line in 0..text.line_count().min(DETECT_LINES) {
            let Ok(range) = text.line_content_range(line) else {
                break;
            };
            // Only the start of the line matters; a pathological long line costs 64 bytes.
            let head = text
                .slice(range.start..text.floor_char_boundary(range.end.min(range.start + 64)))
                .unwrap_or_default();
            if head.trim().is_empty() {
                continue;
            }
            if head.starts_with('\t') {
                tabs += 1;
                continue;
            }
            let width = head.len() - head.trim_start_matches(' ').len();
            if width > 0 {
                spaces += 1;
            }
            if width > previous && width - previous < steps.len() {
                steps[width - previous] += 1;
            }
            previous = width;
        }
        if tabs > spaces {
            return IndentUnit::Tab;
        }
        [2usize, 3, 4, 8]
            .into_iter()
            .filter(|n| steps[*n] > 0)
            .max_by_key(|n| (steps[*n], *n == 4))
            .map_or_else(IndentUnit::default, IndentUnit::Spaces)
    }
}

/// How a language comments a line out: `// x`, `# x`, `<!-- x -->`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentStyle {
    pub prefix: &'static str,
    /// Closing marker of a block comment used per line (`-->`), empty for line comments.
    pub suffix: &'static str,
}

/// The comment of `language`; `None` where there is none (JSON, plain text).
pub fn comment_style(language: Option<Language>) -> Option<CommentStyle> {
    let line = |prefix| CommentStyle { prefix, suffix: "" };
    Some(match language? {
        Language::Rust
        | Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Go
        | Language::C
        | Language::Cpp => line("//"),
        Language::Python | Language::Toml | Language::Yaml | Language::Bash => line("#"),
        Language::Css => CommentStyle {
            prefix: "/*",
            suffix: "*/",
        },
        Language::Html | Language::Markdown => CommentStyle {
            prefix: "<!--",
            suffix: "-->",
        },
        Language::Json => return None,
    })
}

/// The closing partner `open` gets when typed, if it is an opening bracket or quote that pairs
/// in `language`. A Rust `'` starts a lifetime as often as a character, so it does not pair; a
/// backquote pairs where it quotes.
pub fn closing_pair(language: Option<Language>, open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' if language != Some(Language::Rust) => Some('\''),
        '`' if matches!(
            language,
            Some(
                Language::JavaScript
                    | Language::TypeScript
                    | Language::Tsx
                    | Language::Go
                    | Language::Markdown
                    | Language::Bash
            )
        ) =>
        {
            Some('`')
        }
        _ => None,
    }
}

/// Whether `c` closes a pair (what typing over an auto-inserted partner skips).
pub fn is_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
}

/// Whether `c` is a quote (pairs with itself).
pub fn is_quote(c: char) -> bool {
    matches!(c, '"' | '\'' | '`')
}

/// Whether a line that ends in `c` (ignoring spaces) opens a block the next line indents into.
pub fn opens_block(language: Option<Language>, c: char) -> bool {
    match c {
        '{' | '(' | '[' => true,
        ':' => language == Some(Language::Python) || language == Some(Language::Yaml),
        _ => false,
    }
}

/// Whether `c` belongs to a word (what ⌥←/⌥→, double click and ⌘D stop at).
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indent_unit_is_read_from_the_file() {
        let two = Text::from("a {\n  b {\n    c\n  }\n}\n");
        assert_eq!(IndentUnit::detect(&two), IndentUnit::Spaces(2));
        let four = Text::from("fn a() {\n    x\n    if y {\n        z\n    }\n}\n");
        assert_eq!(IndentUnit::detect(&four), IndentUnit::Spaces(4));
        let tabs = Text::from("func a() {\n\tx\n\tif y {\n\t\tz\n\t}\n}\n");
        assert_eq!(IndentUnit::detect(&tabs), IndentUnit::Tab);
        assert_eq!(
            IndentUnit::detect(&Text::from("flat\n")),
            IndentUnit::Spaces(4)
        );
        assert_eq!(IndentUnit::Spaces(2).text(2), "    ");
        assert_eq!(IndentUnit::Tab.text(2), "\t\t");
    }

    #[test]
    fn languages_comment_and_pair_their_own_way() {
        assert_eq!(
            comment_style(Some(Language::Rust)).map(|c| c.prefix),
            Some("//")
        );
        assert_eq!(
            comment_style(Some(Language::Python)).map(|c| c.prefix),
            Some("#")
        );
        assert_eq!(
            comment_style(Some(Language::Html)).map(|c| c.suffix),
            Some("-->")
        );
        assert_eq!(comment_style(Some(Language::Json)), None);
        assert_eq!(comment_style(None), None);
        assert_eq!(closing_pair(Some(Language::Rust), '\''), None, "a lifetime");
        assert_eq!(closing_pair(Some(Language::Python), '\''), Some('\''));
        assert_eq!(closing_pair(None, '{'), Some('}'));
        assert_eq!(closing_pair(Some(Language::Rust), '`'), None);
        assert!(opens_block(Some(Language::Python), ':'));
        assert!(!opens_block(Some(Language::Rust), ':'));
    }
}
