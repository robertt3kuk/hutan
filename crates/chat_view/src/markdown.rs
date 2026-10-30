//! The markdown of chat messages and of Markdown files (the editor's preview): paragraphs,
//! headings, emphasis, inline code, fenced code, lists (nested, with task markers), quotes,
//! rules, tables, links and images. Raw HTML is plain text (an HTML comment is dropped).
//! Text that is still streaming parses too: an unclosed fence is a code block so far, an
//! unclosed `**` is literal.
//!
//! [`parse`] gives blocks; [`parse_located`] the blocks of a document with the line each starts
//! at (a preview follows its source by them); [`flatten`] turns a run of inlines into one string
//! with styled spans for the renderer, which draws each text run as one element.

use std::ops::Range;

/// Nesting beyond this is read as plain text (a hostile message cannot recurse the parser).
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// A fenced block.
    Code {
        lang: Option<String>,
        text: String,
    },
    /// A table (GitHub's pipe syntax): the header cells, one alignment per column and the rows
    /// (each as long as the header).
    Table {
        align: Vec<Align>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Rule,
}

/// How a table column's cells are aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Link {
        content: Vec<Inline>,
        url: String,
    },
    /// `![alt](url)`. Flattened, it is its alt text.
    Image {
        alt: String,
        url: String,
    },
    /// The box of a task list item (`- [x] done`): the first inline of the item's first
    /// paragraph. Flattened, it is `☑` or `☐` and a space.
    Task(bool),
    LineBreak,
}

/// How the single newline inside a paragraph reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Newlines {
    /// A space (CommonMark): the lines of a paragraph flow together.
    Soft,
    /// A line break (GitHub's "breaks" mode, what chat users expect): every line stays a line.
    Hard,
}

/// The mark between two lines of a paragraph that is a line break (Unicode's line separator).
/// [`inline`] reads it as [`Inline::LineBreak`]; the text of a message holds it as itself, as a
/// line break too.
const HARD_NEWLINE: char = '\u{2028}';

/// Blocks of `text`, CommonMark's way: a single newline inside a paragraph is a space.
pub fn parse(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    parse_lines(&lines, 0, Newlines::Soft)
}

/// Blocks of `text` with GitHub-flavoured Markdown's "breaks": a single newline inside a
/// paragraph (or a list item or a quote) is a line break, as chat users write. Code blocks,
/// tables and everything else read as in [`parse`].
pub fn parse_breaks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.lines().collect();
    parse_lines(&lines, 0, Newlines::Hard)
}

/// A top-level block of a document and the line it starts at (0-based).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub line: usize,
    pub block: Block,
}

/// The blocks of a Markdown file, CommonMark's way (a single newline is a space), each with the
/// line it starts at: what a preview scrolls by. A YAML front matter (`---` … `---` on the first
/// lines) is a `yaml` code block.
pub fn parse_located(text: &str) -> Vec<Located> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut from = 0;
    if let Some(end) = front_matter_end(&lines) {
        out.push(Located {
            line: 0,
            block: Block::Code {
                lang: Some("yaml".to_owned()),
                text: lines[1..end].join("\n"),
            },
        });
        from = end + 1;
    }
    out.extend(
        parse_spanned(&lines[from..], 0, Newlines::Soft)
            .into_iter()
            .map(|(line, block)| Located {
                line: line + from,
                block,
            }),
    );
    out
}

/// The line of the `---` that closes a front matter opened on the first line (within the first
/// 200 lines).
fn front_matter_end(lines: &[&str]) -> Option<usize> {
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return None;
    }
    lines
        .iter()
        .enumerate()
        .take(200)
        .skip(1)
        .find(|(_, line)| matches!(line.trim_end(), "---" | "..."))
        .map(|(at, _)| at)
}

// ---- blocks ---------------------------------------------------------------------------------

struct Fence {
    marker: char,
    len: usize,
    indent: usize,
    lang: Option<String>,
}

fn indent_of(line: &str) -> usize {
    let mut width = 0;
    for c in line.chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// `line` without up to `width` columns of leading whitespace.
fn strip_indent(line: &str, width: usize) -> &str {
    let mut used = 0;
    for (at, c) in line.char_indices() {
        if used >= width {
            return &line[at..];
        }
        match c {
            ' ' => used += 1,
            '\t' => used += 4 - used % 4,
            _ => return &line[at..],
        }
    }
    ""
}

fn fence_open(line: &str) -> Option<Fence> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let rest = line.trim_start();
    let marker = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.chars().take_while(|c| *c == marker).count();
    if len < 3 {
        return None;
    }
    let info = rest[len..].trim();
    // A backtick fence's info string cannot contain a backtick (that is inline code).
    if marker == '`' && info.contains('`') {
        return None;
    }
    let lang = info
        .split_whitespace()
        .next()
        .map(|word| word.trim_matches(|c| c == '{' || c == '}').to_owned())
        .filter(|word| !word.is_empty());
    Some(Fence {
        marker,
        len,
        indent,
        lang,
    })
}

fn fence_close(line: &str, open: &Fence) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let rest = line.trim();
    rest.chars().count() >= open.len && rest.chars().all(|c| c == open.marker)
}

fn heading(line: &str) -> Option<(u8, &str)> {
    if indent_of(line) > 3 {
        return None;
    }
    let rest = line.trim_start();
    let level = rest.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let text = after.trim();
    // A closing run of `#` is decoration.
    let without_close = text.trim_end_matches('#');
    let text = if without_close.len() < text.len()
        && (without_close.is_empty() || without_close.ends_with([' ', '\t']))
    {
        without_close.trim_end()
    } else {
        text
    };
    Some((u8::try_from(level).unwrap_or(6), text))
}

fn is_rule(line: &str) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let mut chars = compact.chars();
    match chars.next() {
        Some(first @ ('-' | '*' | '_')) => compact.len() >= 3 && chars.all(|c| c == first),
        _ => false,
    }
}

fn is_quote(line: &str) -> bool {
    indent_of(line) <= 3 && line.trim_start().starts_with('>')
}

/// The cells of a table row: split at the `|`s that are not escaped, the outer ones optional.
fn split_row(line: &str) -> Vec<String> {
    let line = line.trim();
    let ends_with_pipe = line.ends_with('|') && !line.ends_with("\\|");
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    if !(cell.trim().is_empty() && ends_with_pipe) {
        cells.push(cell);
    }
    cells
}

/// The alignments of a table delimiter row (`|---|:--:|--:|`), `None` when it is not one.
fn table_delimiter(line: &str) -> Option<Vec<Align>> {
    if !line.contains('-') {
        return None;
    }
    split_row(line)
        .iter()
        .map(|cell| {
            let cell = cell.trim();
            let (left, right) = (cell.starts_with(':'), cell.ends_with(':'));
            let dashes = cell.trim_matches(':');
            (!dashes.is_empty() && dashes.chars().all(|c| c == '-')).then_some(
                match (left, right) {
                    (true, true) => Align::Center,
                    (false, true) => Align::Right,
                    _ => Align::Left,
                },
            )
        })
        .collect()
}

/// The header cells and alignments of the table that starts at line `at`, if one does: a row
/// with a `|`, then a delimiter row of as many cells.
fn table_head(lines: &[&str], at: usize) -> Option<(Vec<String>, Vec<Align>)> {
    let line = lines.get(at)?;
    if !line.contains('|') || indent_of(line) > 3 {
        return None;
    }
    let align = table_delimiter(lines.get(at + 1)?)?;
    let header = split_row(line);
    (header.len() == align.len()).then_some((header, align))
}

struct Marker<'a> {
    ordered: bool,
    number: u64,
    /// Columns before the item's content.
    content_offset: usize,
    content: &'a str,
}

fn list_marker(line: &str) -> Option<Marker<'_>> {
    let indent = indent_of(line);
    if indent > 3 {
        return None;
    }
    let rest = line.trim_start_matches([' ', '\t']);
    let (ordered, number, width) = match rest.chars().next()? {
        '-' | '*' | '+' => (false, 0, 1),
        c if c.is_ascii_digit() => {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 9 {
                return None;
            }
            let delimiter = rest[digits..].chars().next()?;
            if delimiter != '.' && delimiter != ')' {
                return None;
            }
            (true, rest[..digits].parse().ok()?, digits + 1)
        }
        _ => return None,
    };
    let after = &rest[width..];
    // The marker needs a space after it (or nothing: an empty item).
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let spaces = after
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    let gap = if spaces == 0 || spaces > 4 { 1 } else { spaces };
    // A rule (`* * *`, `- - -`) is not a list.
    if !ordered && is_rule(line) {
        return None;
    }
    Some(Marker {
        ordered,
        number,
        content_offset: indent + width + gap,
        content: after.get(spaces.min(gap).min(after.len())..).unwrap_or(""),
    })
}

/// Whether `line` starts a block that interrupts a paragraph.
fn interrupts_paragraph(line: &str) -> bool {
    fence_open(line).is_some()
        || heading(line).is_some()
        || is_rule(line)
        || is_quote(line)
        || list_marker(line).is_some_and(|m| !m.ordered || m.number == 1)
}

fn parse_lines(lines: &[&str], depth: usize, newlines: Newlines) -> Vec<Block> {
    parse_spanned(lines, depth, newlines)
        .into_iter()
        .map(|(_, block)| block)
        .collect()
}

/// The blocks of `lines`, each with the index of the line it starts at.
fn parse_spanned(lines: &[&str], depth: usize, newlines: Newlines) -> Vec<(usize, Block)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        if depth >= MAX_DEPTH {
            let (paragraph, next) = paragraph(lines, i, newlines);
            out.push((
                start,
                Block::Paragraph(vec![Inline::Text(paragraph.replace(HARD_NEWLINE, "\n"))]),
            ));
            i = next;
            continue;
        }
        if let Some(fence) = fence_open(line) {
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                if fence_close(lines[i], &fence) {
                    i += 1;
                    break;
                }
                body.push(strip_indent(lines[i], fence.indent));
                i += 1;
            }
            out.push((
                start,
                Block::Code {
                    lang: fence.lang,
                    text: body.join("\n"),
                },
            ));
            continue;
        }
        if line.trim_start().starts_with("<!--") {
            // An HTML comment is nobody's text: it runs to the line that holds its end.
            while i < lines.len() && !lines[i].contains("-->") {
                i += 1;
            }
            i += 1;
            continue;
        }
        if let Some((level, text)) = heading(line) {
            out.push((
                start,
                Block::Heading {
                    level,
                    content: parse_inline(text),
                },
            ));
            i += 1;
            continue;
        }
        if is_rule(line) {
            out.push((start, Block::Rule));
            i += 1;
            continue;
        }
        if is_quote(line) {
            let mut inner = Vec::new();
            while i < lines.len() && is_quote(lines[i]) {
                let rest = lines[i].trim_start().trim_start_matches('>');
                inner.push(rest.strip_prefix(' ').unwrap_or(rest));
                i += 1;
            }
            out.push((
                start,
                Block::Quote(parse_lines(&inner, depth + 1, newlines)),
            ));
            continue;
        }
        if let Some(first) = list_marker(line) {
            let (block, next) = list(lines, i, first.ordered, first.number, depth, newlines);
            out.push((start, block));
            i = next;
            continue;
        }
        if let Some((header, align)) = table_head(lines, i) {
            i += 2;
            let columns = align.len();
            let mut rows = Vec::new();
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && fence_open(lines[i]).is_none()
                && heading(lines[i]).is_none()
                && !is_quote(lines[i])
            {
                let mut cells = split_row(lines[i]);
                cells.resize(columns, String::new());
                rows.push(cells.iter().map(|c| parse_inline(c.trim())).collect());
                i += 1;
            }
            out.push((
                start,
                Block::Table {
                    align,
                    header: header.iter().map(|c| parse_inline(c.trim())).collect(),
                    rows,
                },
            ));
            continue;
        }
        let (text, next) = paragraph(lines, i, newlines);
        out.push((start, Block::Paragraph(parse_inline(&text))));
        i = next;
    }
    out
}

/// The paragraph starting at `start`: its lines joined by `\n` (by [`HARD_NEWLINE`] when every
/// line break is kept), and the index after it.
fn paragraph(lines: &[&str], start: usize, newlines: Newlines) -> (String, usize) {
    let mut text = String::new();
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() || (i > start && interrupts_paragraph(line)) {
            break;
        }
        if !text.is_empty() {
            text.push(match newlines {
                Newlines::Soft => '\n',
                Newlines::Hard => HARD_NEWLINE,
            });
        }
        text.push_str(line.trim_start());
        i += 1;
    }
    (text, i)
}

fn list(
    lines: &[&str],
    start: usize,
    ordered: bool,
    number: u64,
    depth: usize,
    newlines: Newlines,
) -> (Block, usize) {
    let mut items = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let Some(marker) = list_marker(lines[i]).filter(|m| m.ordered == ordered) else {
            break;
        };
        let offset = marker.content_offset;
        let mut content = vec![marker.content];
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                // Blank lines stay in the item only when its content goes on, indented.
                let next = (i..lines.len()).find(|j| !lines[*j].trim().is_empty());
                match next {
                    Some(j) if indent_of(lines[j]) >= offset => {
                        content.push("");
                        i += 1;
                        continue;
                    }
                    _ => break,
                }
            }
            if indent_of(line) >= offset {
                content.push(strip_indent(line, offset));
            } else if list_marker(line).is_some()
                || fence_open(line).is_some()
                || heading(line).is_some()
                || is_rule(line)
                || is_quote(line)
            {
                break;
            } else if content.last().is_some_and(|last| !last.trim().is_empty()) {
                // Lazy continuation of the item's paragraph.
                content.push(line.trim_start());
            } else {
                break;
            }
            i += 1;
        }
        let mut blocks = parse_lines(&content, depth + 1, newlines);
        mark_task(&mut blocks);
        items.push(blocks);
        // The next item follows directly or after blank lines.
        let next = (i..lines.len()).find(|j| !lines[*j].trim().is_empty());
        match next {
            Some(j) if list_marker(lines[j]).is_some_and(|m| m.ordered == ordered) => i = j,
            _ => break,
        }
    }
    (
        Block::List {
            ordered,
            start: if ordered { number } else { 1 },
            items,
        },
        i,
    )
}

// ---- inlines --------------------------------------------------------------------------------

/// Inlines of one paragraph's text (lines joined by `\n`).
pub fn parse_inline(text: &str) -> Vec<Inline> {
    inline(text, 0)
}

fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
}

fn prev_char(s: &str, at: usize) -> Option<char> {
    s[..at].chars().next_back()
}

fn flush(plain: &mut String, out: &mut Vec<Inline>) {
    if !plain.is_empty() {
        out.push(Inline::Text(std::mem::take(plain)));
    }
}

/// The end (exclusive) of a code span opened by a run of `ticks` backticks at `at`, if closed.
fn code_span_end(s: &str, at: usize, ticks: usize) -> Option<usize> {
    let mut i = at + ticks;
    while i < s.len() {
        if s.as_bytes()[i] == b'`' {
            let run = s[i..].chars().take_while(|c| *c == '`').count();
            if run == ticks {
                return Some(i);
            }
            i += run;
        } else {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

fn inline(s: &str, depth: usize) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or('\0');
        let width = c.len_utf8();
        match c {
            '\\' => {
                match s[i + 1..].chars().next() {
                    Some('\n') => {
                        flush(&mut plain, &mut out);
                        out.push(Inline::LineBreak);
                        i += 2;
                    }
                    Some(HARD_NEWLINE) => {
                        flush(&mut plain, &mut out);
                        out.push(Inline::LineBreak);
                        i += 1 + HARD_NEWLINE.len_utf8();
                    }
                    Some(next) if is_punctuation(next) => {
                        plain.push(next);
                        i += 1 + next.len_utf8();
                    }
                    _ => {
                        plain.push('\\');
                        i += 1;
                    }
                }
                continue;
            }
            HARD_NEWLINE => {
                // A kept line break: the spaces before it are of no use.
                let trimmed = plain.trim_end_matches(' ').len();
                plain.truncate(trimmed);
                flush(&mut plain, &mut out);
                out.push(Inline::LineBreak);
                i += width;
                continue;
            }
            '\n' => {
                let hard = plain.ends_with("  ");
                let trimmed = plain.trim_end_matches(' ').len();
                plain.truncate(trimmed);
                if hard {
                    flush(&mut plain, &mut out);
                    out.push(Inline::LineBreak);
                } else {
                    plain.push(' ');
                }
                i += 1;
                continue;
            }
            '`' => {
                let ticks = s[i..].chars().take_while(|c| *c == '`').count();
                if let Some(end) = code_span_end(s, i, ticks) {
                    flush(&mut plain, &mut out);
                    let raw = s[i + ticks..end].replace(['\n', HARD_NEWLINE], " ");
                    let code = if raw.len() > 1
                        && raw.starts_with(' ')
                        && raw.ends_with(' ')
                        && !raw.trim().is_empty()
                    {
                        raw[1..raw.len() - 1].to_owned()
                    } else {
                        raw
                    };
                    out.push(Inline::Code(code));
                    i = end + ticks;
                } else {
                    plain.push_str(&s[i..i + ticks]);
                    i += ticks;
                }
                continue;
            }
            '!' if s[i + 1..].starts_with('[') => {
                if let Some((label, url, end)) = bracket_link(s, i + 1) {
                    flush(&mut plain, &mut out);
                    out.push(Inline::Image {
                        alt: label_text(label, depth),
                        url: url.to_owned(),
                    });
                    i = end;
                    continue;
                }
            }
            '[' if depth < MAX_DEPTH => {
                if let Some((label, url, end)) = bracket_link(s, i) {
                    flush(&mut plain, &mut out);
                    out.push(Inline::Link {
                        content: inline(label, depth + 1),
                        url: url.to_owned(),
                    });
                    i = end;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = s[i..].find('>') {
                    let inner = &s[i + 1..i + end];
                    if is_openable(inner) && !inner.contains(char::is_whitespace) {
                        flush(&mut plain, &mut out);
                        out.push(Inline::Link {
                            content: vec![Inline::Text(inner.to_owned())],
                            url: inner.to_owned(),
                        });
                        i += end + 1;
                        continue;
                    }
                }
            }
            'h' if (s[i..].starts_with("http://") || s[i..].starts_with("https://"))
                && prev_char(s, i).is_none_or(|p| p.is_whitespace() || p == '(') =>
            {
                let end = bare_url_end(s, i);
                if end > i + 8 {
                    flush(&mut plain, &mut out);
                    let url = &s[i..end];
                    out.push(Inline::Link {
                        content: vec![Inline::Text(url.to_owned())],
                        url: url.to_owned(),
                    });
                    i = end;
                    continue;
                }
            }
            '*' | '_' if depth < MAX_DEPTH => {
                let run = s[i..].chars().take_while(|x| *x == c).count();
                if let Some((node, end)) = emphasis(s, i, c, run, depth) {
                    flush(&mut plain, &mut out);
                    out.push(node);
                    i = end;
                    continue;
                }
                plain.push_str(&s[i..i + run]);
                i += run;
                continue;
            }
            _ => {}
        }
        plain.push(c);
        i += width;
    }
    flush(&mut plain, &mut out);
    out
}

fn label_text(label: &str, depth: usize) -> String {
    let mut text = String::new();
    collect_text(&inline(label, depth + 1), &mut text);
    text
}

fn collect_text(inlines: &[Inline], into: &mut String) {
    for node in inlines {
        match node {
            Inline::Text(t) | Inline::Code(t) => into.push_str(t),
            Inline::Emphasis(inner) | Inline::Strong(inner) => collect_text(inner, into),
            Inline::Link { content, .. } => collect_text(content, into),
            Inline::Image { alt, .. } => into.push_str(alt),
            Inline::Task(_) => {}
            Inline::LineBreak => into.push('\n'),
        }
    }
}

/// The text of `inlines` without any marks (what a heading's anchor is made of).
pub fn plain_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    collect_text(inlines, &mut text);
    text
}

/// The anchor GitHub gives a heading: lower case, letters, digits, `-` and `_` kept, spaces
/// become `-`, other punctuation goes. The same heading twice is the caller's to number.
pub fn slug(text: &str) -> String {
    text.trim()
        .chars()
        .filter_map(|c| match c {
            ' ' | '\t' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

/// Turns `[ ]` / `[x]` at the start of a list item into [`Inline::Task`].
fn mark_task(blocks: &mut [Block]) {
    let Some(Block::Paragraph(inlines)) = blocks.first_mut() else {
        return;
    };
    let Some(Inline::Text(first)) = inlines.first() else {
        return;
    };
    let (done, rest) = match first.get(..3) {
        Some("[ ]") => (false, &first[3..]),
        Some("[x]" | "[X]") => (true, &first[3..]),
        _ => return,
    };
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return;
    }
    let rest = rest.trim_start().to_owned();
    if rest.is_empty() {
        inlines.remove(0);
    } else {
        inlines[0] = Inline::Text(rest);
    }
    inlines.insert(0, Inline::Task(done));
}

/// Schemes a click may open.
fn is_openable(url: &str) -> bool {
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
}

/// Whether the view should make `url` clickable (anything else is shown, not opened).
pub fn openable(url: &str) -> bool {
    is_openable(url)
}

fn bare_url_end(s: &str, start: usize) -> usize {
    let mut end = start;
    for (at, c) in s[start..].char_indices() {
        if c.is_whitespace() || c == '<' || c == '>' {
            break;
        }
        end = start + at + c.len_utf8();
    }
    // Sentence punctuation after the link is not part of it; a closing parenthesis only when
    // the link has no opening one.
    loop {
        let url = &s[start..end];
        let last = url.chars().next_back().unwrap_or(' ');
        let trim = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ']' | '*' | '_' => true,
            ')' => url.matches(')').count() > url.matches('(').count(),
            _ => false,
        };
        if trim && end > start + 8 {
            end -= last.len_utf8();
        } else {
            return end;
        }
    }
}

/// `[label](url)` at `at`: the label, the url and the index after the closing parenthesis.
fn bracket_link(s: &str, at: usize) -> Option<(&str, &str, usize)> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = at;
    let mut close = None;
    while i < s.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'`' => {
                let ticks = s[i..].chars().take_while(|c| *c == '`').count();
                if let Some(end) = code_span_end(s, i, ticks) {
                    i = end + ticks - 1;
                } else {
                    i += ticks - 1;
                }
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let close = close?;
    if bytes.get(close + 1) != Some(&b'(') {
        return None;
    }
    let url_start = close + 2;
    let mut parens = 0usize;
    let mut j = url_start;
    while j < s.len() {
        match bytes[j] {
            b'(' => parens += 1,
            b')' if parens == 0 => break,
            b')' => parens -= 1,
            b'\n' => return None,
            _ => {}
        }
        j += 1;
    }
    if j >= s.len() {
        return None;
    }
    let target = s[url_start..j].trim();
    // An optional title after the url is dropped.
    let url = target.split_whitespace().next().unwrap_or("");
    let url = url
        .strip_prefix('<')
        .and_then(|u| u.strip_suffix('>'))
        .unwrap_or(url);
    if url.is_empty() {
        return None;
    }
    Some((&s[at + 1..close], url, j + 1))
}

/// Emphasis or strong text opened by the run of `run` markers `marker` at `at`; the node and the
/// index after its closing markers.
fn emphasis(s: &str, at: usize, marker: char, run: usize, depth: usize) -> Option<(Inline, usize)> {
    let after = at + run;
    let next = s[after..].chars().next()?;
    if next.is_whitespace() {
        return None;
    }
    if marker == '_' && prev_char(s, at).is_some_and(char::is_alphanumeric) {
        return None;
    }
    // `***x***` is strong emphasis, `**x**` strong, `*x*` emphasis.
    let want = run.min(3);
    let close = find_close(s, after, marker, want)?;
    let inner = &s[after..close];
    if inner.trim().is_empty() {
        return None;
    }
    let content = inline(inner, depth + 1);
    let node = match want {
        3 => Inline::Strong(vec![Inline::Emphasis(content)]),
        2 => Inline::Strong(content),
        _ => Inline::Emphasis(content),
    };
    // A longer run of markers (`****x**`) is not emphasis; it stays literal.
    (run == want).then_some((node, close + want))
}

/// The position of the run of `len` `marker`s closing the emphasis that starts at `from`.
/// Emphasis opened inside (`*a **b** c*`) is skipped whole.
fn find_close(s: &str, from: usize, marker: char, len: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    while i < s.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => {
                let ticks = s[i..].chars().take_while(|c| *c == '`').count();
                i = match code_span_end(s, i, ticks) {
                    Some(end) => end + ticks,
                    None => i + ticks,
                };
            }
            b if b as char == marker => {
                let run = s[i..].chars().take_while(|c| *c == marker).count();
                let before = prev_char(s, i);
                let after = s[i + run..].chars().next();
                let intraword = marker == '_';
                let can_close = before.is_some_and(|p| !p.is_whitespace())
                    && (!intraword || after.is_none_or(|n| !n.is_alphanumeric()));
                let can_open = after.is_some_and(|n| !n.is_whitespace())
                    && (!intraword || before.is_none_or(|p| !p.is_alphanumeric()));
                if can_close && run == len {
                    return Some(i);
                }
                if can_open && !can_close && run <= 3 {
                    // A nested emphasis: jump over it when it closes.
                    i = match find_close(s, i + run, marker, run) {
                        Some(close) => close + run,
                        None => i + run,
                    };
                } else {
                    i += run;
                }
            }
            _ => i += s[i..].chars().next().map_or(1, char::len_utf8),
        }
    }
    None
}

// ---- flattened text for the renderer --------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    /// The url of a link that may be opened.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Byte range in [`Styled::text`]; the spans cover the text without gaps.
    pub range: Range<usize>,
    pub style: SpanStyle,
}

/// Inlines as one string and the styles of its parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Styled {
    pub text: String,
    pub spans: Vec<Span>,
}

/// Which links of a text are clickable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// Web and mail links only (`http`, `https`, `mailto`): a message's text may be anyone's.
    Web,
    /// Every link with a target: the owner of a document decides what each one opens.
    Any,
}

/// [`flatten_links`] for a message: only web and mail links are clickable.
pub fn flatten(inlines: &[Inline]) -> Styled {
    flatten_links(inlines, Links::Web)
}

/// Inlines as one string with their styles; the links `links` allows carry their target.
pub fn flatten_links(inlines: &[Inline], links: Links) -> Styled {
    let mut styled = Styled::default();
    push_inlines(inlines, &SpanStyle::default(), links, &mut styled);
    styled
}

fn push_span(styled: &mut Styled, text: &str, style: &SpanStyle) {
    if text.is_empty() {
        return;
    }
    let start = styled.text.len();
    styled.text.push_str(text);
    styled.spans.push(Span {
        range: start..styled.text.len(),
        style: style.clone(),
    });
}

fn push_inlines(inlines: &[Inline], style: &SpanStyle, links: Links, out: &mut Styled) {
    for node in inlines {
        match node {
            Inline::Text(text) => push_span(out, text, style),
            Inline::Code(text) => push_span(
                out,
                text,
                &SpanStyle {
                    code: true,
                    ..style.clone()
                },
            ),
            Inline::Emphasis(inner) => push_inlines(
                inner,
                &SpanStyle {
                    italic: true,
                    ..style.clone()
                },
                links,
                out,
            ),
            Inline::Strong(inner) => push_inlines(
                inner,
                &SpanStyle {
                    bold: true,
                    ..style.clone()
                },
                links,
                out,
            ),
            Inline::Link { content, url } => push_inlines(
                content,
                &SpanStyle {
                    link: match links {
                        Links::Web => is_openable(url).then(|| url.clone()),
                        Links::Any => Some(url.clone()),
                    },
                    ..style.clone()
                },
                links,
                out,
            ),
            // The alt text: a renderer that can draw the image does so before it gets here.
            Inline::Image { alt, .. } => push_span(out, alt, style),
            Inline::Task(done) => push_span(out, if *done { "☑ " } else { "☐ " }, style),
            Inline::LineBreak => push_span(out, "\n", style),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_owned())
    }

    #[test]
    fn paragraphs_join_soft_breaks_and_split_on_blank_lines() {
        assert_eq!(
            parse("one\ntwo\n\nthree"),
            vec![
                Block::Paragraph(vec![text("one two")]),
                Block::Paragraph(vec![text("three")]),
            ]
        );
    }

    #[test]
    fn hard_breaks() {
        assert_eq!(
            parse_inline("a  \nb"),
            vec![text("a"), Inline::LineBreak, text("b")]
        );
        assert_eq!(
            parse_inline("a\\\nb"),
            vec![text("a"), Inline::LineBreak, text("b")]
        );
    }

    /// F-14: GitHub's "breaks" mode keeps a single newline as a line break; code stays as it is.
    #[test]
    fn breaks_mode_keeps_single_newlines_as_line_breaks() {
        assert_eq!(
            parse_breaks("1\n2\n3\n\nnext"),
            vec![
                Block::Paragraph(vec![
                    text("1"),
                    Inline::LineBreak,
                    text("2"),
                    Inline::LineBreak,
                    text("3"),
                ]),
                Block::Paragraph(vec![text("next")]),
            ]
        );
        // The same text in CommonMark's reading flows together.
        assert_eq!(
            parse("1\n2\n3"),
            vec![Block::Paragraph(vec![text("1 2 3")])]
        );
        // Trailing spaces and a backslash do not add a second break or leave a mark.
        assert_eq!(
            parse_breaks("a  \nb\\\nc"),
            vec![Block::Paragraph(vec![
                text("a"),
                Inline::LineBreak,
                text("b"),
                Inline::LineBreak,
                text("c"),
            ])]
        );
        // Inside emphasis, a link label, a list item and a quote.
        let blocks = parse_breaks("- one\n  two\n> q1\n> q2\n**a\nb**");
        assert_eq!(
            blocks[0],
            Block::List {
                ordered: false,
                start: 1,
                items: vec![vec![Block::Paragraph(vec![
                    text("one"),
                    Inline::LineBreak,
                    text("two"),
                ])]],
            }
        );
        assert_eq!(
            blocks[1],
            Block::Quote(vec![Block::Paragraph(vec![
                text("q1"),
                Inline::LineBreak,
                text("q2"),
            ])])
        );
        assert_eq!(
            blocks[2],
            Block::Paragraph(vec![Inline::Strong(vec![
                text("a"),
                Inline::LineBreak,
                text("b"),
            ])])
        );
        // Fenced code and tables are text as written.
        assert_eq!(
            parse_breaks("```\na\nb\n```\n| a | b |\n|---|---|\n| 1 | 2 |"),
            parse("```\na\nb\n```\n| a | b |\n|---|---|\n| 1 | 2 |")
        );
        // A code span that spans two lines stays one run of code.
        assert_eq!(
            parse_breaks("`a\nb`"),
            vec![Block::Paragraph(vec![Inline::Code("a b".into())])]
        );
        // The text a renderer gets has the break as a newline.
        let Block::Paragraph(inlines) = &parse_breaks("x\ny")[0] else {
            panic!("a paragraph")
        };
        assert_eq!(flatten(inlines).text, "x\ny");
    }

    #[test]
    fn emphasis_strong_and_code() {
        assert_eq!(
            parse_inline("a *b* **c** `d` e"),
            vec![
                text("a "),
                Inline::Emphasis(vec![text("b")]),
                text(" "),
                Inline::Strong(vec![text("c")]),
                text(" "),
                Inline::Code("d".into()),
                text(" e"),
            ]
        );
        assert_eq!(
            parse_inline("***x***"),
            vec![Inline::Strong(vec![Inline::Emphasis(vec![text("x")])])]
        );
        assert_eq!(
            parse_inline("**a *b* c**"),
            vec![Inline::Strong(vec![
                text("a "),
                Inline::Emphasis(vec![text("b")]),
                text(" c"),
            ])]
        );
    }

    #[test]
    fn markers_without_a_partner_stay_literal() {
        assert_eq!(parse_inline("2 * 3 * 4"), vec![text("2 * 3 * 4")]);
        assert_eq!(parse_inline("**open"), vec![text("**open")]);
        assert_eq!(
            parse_inline("snake_case_name"),
            vec![text("snake_case_name")]
        );
        assert_eq!(parse_inline("a_b_"), vec![text("a_b_")]);
    }

    #[test]
    fn code_spans_win_over_emphasis_and_keep_their_text() {
        assert_eq!(
            parse_inline("`a*b` and *c*"),
            vec![
                Inline::Code("a*b".into()),
                text(" and "),
                Inline::Emphasis(vec![text("c")]),
            ]
        );
        assert_eq!(parse_inline("`` a`b ``"), vec![Inline::Code("a`b".into())]);
        assert_eq!(parse_inline("`open"), vec![text("`open")]);
    }

    #[test]
    fn escapes() {
        assert_eq!(parse_inline("\\*not\\*"), vec![text("*not*")]);
    }

    #[test]
    fn links_autolinks_and_images() {
        assert_eq!(
            parse_inline("see [the docs](https://example.com/a) now"),
            vec![
                text("see "),
                Inline::Link {
                    content: vec![text("the docs")],
                    url: "https://example.com/a".into()
                },
                text(" now"),
            ]
        );
        assert_eq!(
            parse_inline("go to https://example.com/x, then"),
            vec![
                text("go to "),
                Inline::Link {
                    content: vec![text("https://example.com/x")],
                    url: "https://example.com/x".into()
                },
                text(", then"),
            ]
        );
        assert_eq!(
            parse_inline("<https://example.com>"),
            vec![Inline::Link {
                content: vec![text("https://example.com")],
                url: "https://example.com".into()
            }]
        );
        // An image is its own node; flattened it is its alt text (a chat does not load images).
        assert_eq!(
            parse_inline("![a cat](https://x/y.png)"),
            vec![Inline::Image {
                alt: "a cat".into(),
                url: "https://x/y.png".into()
            }]
        );
        assert_eq!(
            flatten(&parse_inline("see ![a cat](https://x/y.png)!")).text,
            "see a cat!"
        );
        // A linked image (a badge).
        assert_eq!(
            parse_inline("[![ci](b.svg)](https://x.y/ci)"),
            vec![Inline::Link {
                content: vec![Inline::Image {
                    alt: "ci".into(),
                    url: "b.svg".into()
                }],
                url: "https://x.y/ci".into()
            }]
        );
        assert_eq!(parse_inline("[open"), vec![text("[open")]);
    }

    #[test]
    fn raw_html_is_text() {
        assert_eq!(
            parse_inline("<b>bold</b> <script>x</script>"),
            vec![text("<b>bold</b> <script>x</script>")]
        );
    }

    #[test]
    fn only_web_and_mail_links_are_openable() {
        let styled = flatten(&parse_inline("[a](javascript:alert(1)) [b](https://x.y)"));
        let links: Vec<_> = styled
            .spans
            .iter()
            .filter_map(|s| s.style.link.clone())
            .collect();
        assert_eq!(links, ["https://x.y"]);
        assert_eq!(styled.text, "a b");
    }

    #[test]
    fn headings() {
        assert_eq!(
            parse("## Title ##\n#nospace"),
            vec![
                Block::Heading {
                    level: 2,
                    content: vec![text("Title")]
                },
                Block::Paragraph(vec![text("#nospace")]),
            ]
        );
    }

    #[test]
    fn fenced_code_keeps_its_text_and_language() {
        let blocks = parse("before\n```rust\nfn main() {\n    *x*\n}\n```\nafter");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![text("before")]),
                Block::Code {
                    lang: Some("rust".into()),
                    text: "fn main() {\n    *x*\n}".into()
                },
                Block::Paragraph(vec![text("after")]),
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_is_code_so_far() {
        assert_eq!(
            parse("```sh\nls -la\n"),
            vec![Block::Code {
                lang: Some("sh".into()),
                text: "ls -la".into()
            }]
        );
    }

    #[test]
    fn a_longer_fence_can_contain_a_shorter_one() {
        let blocks = parse("````\n```\ninner\n```\n````");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: None,
                text: "```\ninner\n```".into()
            }]
        );
    }

    #[test]
    fn lists_nest_and_number() {
        let blocks = parse("1. one\n2. two\n   - nested a\n   - nested b\n3. three");
        let Block::List {
            ordered,
            start,
            items,
        } = &blocks[0]
        else {
            panic!("{blocks:?}");
        };
        assert!(*ordered);
        assert_eq!(*start, 1);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], vec![Block::Paragraph(vec![text("one")])]);
        assert!(matches!(
            &items[1][1],
            Block::List { ordered: false, items, .. } if items.len() == 2
        ));
    }

    #[test]
    fn bullets_with_lazy_continuation_and_blank_lines_between_items() {
        let blocks = parse("- a\n  more\n\n- b\n\ntext");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], vec![Block::Paragraph(vec![text("a more")])]);
        assert_eq!(blocks[1], Block::Paragraph(vec![text("text")]));
    }

    #[test]
    fn a_list_may_follow_a_paragraph_without_a_blank_line() {
        let blocks = parse("Steps:\n- a\n- b");
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[1], Block::List { .. }));
    }

    #[test]
    fn code_inside_a_list_item() {
        let blocks = parse("- run:\n\n  ```sh\n  ls\n  ```\n- done");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0][1], Block::Code { text, .. } if text == "ls"));
    }

    #[test]
    fn quotes_and_rules() {
        assert_eq!(
            parse("> quoted\n> text\n\n---\nafter"),
            vec![
                Block::Quote(vec![Block::Paragraph(vec![text("quoted text")])]),
                Block::Rule,
                Block::Paragraph(vec![text("after")]),
            ]
        );
    }

    #[test]
    fn tables_are_tables_with_alignment_and_escaped_pipes() {
        let blocks =
            parse("| a | b | c |\n|:--|:-:|--:|\n| 1 | `x \\| y` | **3** |\n| 4 |\n\nafter");
        let Block::Table {
            align,
            header,
            rows,
        } = &blocks[0]
        else {
            panic!("{blocks:?}");
        };
        assert_eq!(align, &[Align::Left, Align::Center, Align::Right]);
        assert_eq!(header, &[vec![text("a")], vec![text("b")], vec![text("c")]]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            [
                vec![text("1")],
                vec![Inline::Code("x | y".into())],
                vec![Inline::Strong(vec![text("3")])]
            ]
        );
        assert_eq!(
            rows[1],
            [vec![text("4")], vec![], vec![]],
            "short rows are padded"
        );
        assert_eq!(blocks[1], Block::Paragraph(vec![text("after")]));
        // A delimiter of another width, or a first row without a pipe, is no table.
        assert!(matches!(
            parse("| a | b |\n|---|\n| 1 | 2 |")[0],
            Block::Paragraph(_)
        ));
        assert!(matches!(parse("a\n|---|")[0], Block::Paragraph(_)));
    }

    #[test]
    fn task_list_items_have_their_box() {
        let blocks = parse("- [ ] todo\n- [x] done *well*\n- [X] big\n- [x]\n- [y] no\n- [x]no");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(
            items[0],
            [Block::Paragraph(vec![Inline::Task(false), text("todo")])]
        );
        assert_eq!(
            items[1],
            [Block::Paragraph(vec![
                Inline::Task(true),
                text("done "),
                Inline::Emphasis(vec![text("well")])
            ])]
        );
        assert_eq!(
            items[2],
            [Block::Paragraph(vec![Inline::Task(true), text("big")])]
        );
        assert_eq!(items[3], [Block::Paragraph(vec![Inline::Task(true)])]);
        assert_eq!(items[4], [Block::Paragraph(vec![text("[y] no")])]);
        assert_eq!(items[5], [Block::Paragraph(vec![text("[x]no")])]);
        assert_eq!(
            flatten(&[Inline::Task(true), text("a")]).text,
            "☑ a",
            "a message shows the box as a glyph too"
        );
    }

    #[test]
    fn a_document_knows_the_line_of_each_block() {
        let located = parse_located(
            "# Title\n\npara one\nstill one\n\n```rust\nfn x() {}\n```\n- a\n- b\n\n| a |\n|---|\n| 1 |\n> q",
        );
        let lines: Vec<usize> = located.iter().map(|l| l.line).collect();
        assert_eq!(lines, [0, 2, 5, 8, 11, 14]);
        assert!(matches!(located[4].block, Block::Table { .. }));
    }

    #[test]
    fn front_matter_is_yaml_and_html_comments_are_gone() {
        let located = parse_located("---\ntitle: x\n---\n# H\n<!-- hidden\nmore -->\ntext");
        assert_eq!(
            located[0],
            Located {
                line: 0,
                block: Block::Code {
                    lang: Some("yaml".into()),
                    text: "title: x".into()
                }
            }
        );
        assert_eq!(located[1].line, 3, "lines count the front matter");
        assert_eq!(located[2].line, 6);
        assert_eq!(located.len(), 3, "the comment is dropped: {located:?}");
        // Not a front matter: no closing line, or a rule on the first line of chat text.
        assert_eq!(parse_located("---\nno end\n")[0].block, Block::Rule);
    }

    #[test]
    fn headings_have_github_anchors() {
        assert_eq!(slug("Getting Started"), "getting-started");
        assert_eq!(slug("What's new? (0.3.4)"), "whats-new-034");
        assert_eq!(slug("snake_case and-dash"), "snake_case-and-dash");
        let Block::Heading { content, .. } = &parse("## The `code` and *more*")[0] else {
            panic!("a heading")
        };
        assert_eq!(plain_text(content), "The code and more");
    }

    #[test]
    fn document_links_are_all_clickable_a_messages_only_the_web_ones() {
        let inlines = parse_inline("[a](docs/x.md) [b](#top) [c](https://x.y)");
        let links = |styled: Styled| -> Vec<String> {
            styled
                .spans
                .iter()
                .filter_map(|s| s.style.link.clone())
                .collect()
        };
        assert_eq!(links(flatten(&inlines)), ["https://x.y"]);
        assert_eq!(
            links(flatten_links(&inlines, Links::Any)),
            ["docs/x.md", "#top", "https://x.y"]
        );
    }

    #[test]
    fn deep_nesting_does_not_recurse_without_bound() {
        let nested = "> ".repeat(200) + "x";
        let blocks = parse(&nested);
        assert_eq!(blocks.len(), 1);
        let list = "- ".repeat(200) + "x";
        assert_eq!(parse(&list).len(), 1);
    }

    #[test]
    fn flatten_covers_the_text_without_gaps() {
        let styled = flatten(&parse_inline("a **b `c`** [d](https://x.y)"));
        assert_eq!(styled.text, "a b c d");
        let mut at = 0;
        for span in &styled.spans {
            assert_eq!(span.range.start, at);
            at = span.range.end;
        }
        assert_eq!(at, styled.text.len());
        let code = styled.spans.iter().find(|s| s.style.code).unwrap();
        assert_eq!(&styled.text[code.range.clone()], "c");
        assert!(code.style.bold);
    }

    #[test]
    fn unicode_does_not_split_characters() {
        assert_eq!(
            parse_inline("héllo *wörld* ✓ `ç`"),
            vec![
                text("héllo "),
                Inline::Emphasis(vec![text("wörld")]),
                text(" ✓ "),
                Inline::Code("ç".into()),
            ]
        );
    }
}
