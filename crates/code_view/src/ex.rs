//! The text work of `:s` and `:g` for the code view (slice E-8): run over a snapshot of the
//! rope on a background thread, so a 1 MB file never blocks a frame. The `:` line itself is
//! parsed by `hutan_vim::ex` (E-7b) and its patterns compiled to [`Pattern`] there.

use std::ops::Range;

use hutan_buffer::{Edit, Text};
use hutan_vim::Pattern;

/// What `:s` found in `lines`: the edits (in text order) and how many matches on how many
/// lines; the line of the last substitution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Substitution {
    pub edits: Vec<Edit>,
    pub matches: usize,
    pub lines: usize,
    pub last_line: Option<usize>,
}

/// Runs `:s` over `lines` of `text`: `replacement` (already expanded for `~`) for the first
/// match of every line, or every match with `global`. Pure: reads a snapshot, returns edits;
/// with `count_only` it only counts.
pub fn substitute(
    text: &Text,
    lines: Range<usize>,
    pattern: &Pattern,
    replacement: &str,
    global: bool,
    count_only: bool,
    cancelled: &dyn Fn() -> bool,
) -> Substitution {
    let mut out = Substitution::default();
    for line in lines {
        if line % 256 == 0 && cancelled() {
            return Substitution::default();
        }
        let Ok(content) = text.line_content_range(line) else {
            continue;
        };
        let Ok(line_text) = text.slice(content.clone()) else {
            continue;
        };
        let found = pattern.replacements(&line_text, replacement, global);
        if found.is_empty() {
            continue;
        }
        out.matches += found.len();
        out.lines += 1;
        out.last_line = Some(line);
        if !count_only {
            out.edits.extend(found.into_iter().map(|(range, new)| {
                Edit::new(content.start + range.start..content.start + range.end, new)
            }));
        }
    }
    out
}

/// The lines of `lines` whose text matches `pattern` (or does not, `invert`).
pub fn matching_lines(
    text: &Text,
    lines: Range<usize>,
    pattern: &Pattern,
    invert: bool,
    cancelled: &dyn Fn() -> bool,
) -> Vec<usize> {
    let mut out = Vec::new();
    for line in lines {
        if line % 256 == 0 && cancelled() {
            return Vec::new();
        }
        let Ok(line_text) = text.line_text(line) else {
            continue;
        };
        if pattern.is_match(&line_text) != invert {
            out.push(line);
        }
    }
    out
}

/// Edits that delete whole `lines` (sorted, 0-based) of `text`, adjacent lines merged.
pub fn delete_lines(text: &Text, lines: &[usize]) -> Vec<Edit> {
    let ends_with_break = text.char_before(text.len()) == Some('\n');
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for &line in lines {
        let Ok(mut range) = text.line_range(line) else {
            continue;
        };
        // The last line has no break of its own: take the one before it.
        if range.end == text.len() && range.start > 0 && !ends_with_break {
            range.start -= 1;
        }
        match ranges.last_mut() {
            Some(last) if last.end >= range.start => last.end = last.end.max(range.end),
            _ => ranges.push(range),
        }
    }
    ranges.into_iter().map(Edit::delete).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_vim::CaseOptions;

    fn pattern(p: &str) -> Pattern {
        Pattern::new(p, None, CaseOptions::default(), None).unwrap()
    }

    #[test]
    fn substitute_replaces_with_groups_and_counts() {
        let text = Text::from("let a = a;\nfoo\nlet b = a;\n");
        let found = substitute(&text, 0..4, &pattern(r"\<a\>"), "[&]", true, false, &|| {
            false
        });
        assert_eq!(found.matches, 3);
        assert_eq!(found.lines, 2);
        assert_eq!(found.last_line, Some(2));
        let mut s = text.to_string();
        for edit in found.edits.iter().rev() {
            s.replace_range(edit.range.clone(), &edit.text);
        }
        assert_eq!(s, "let [a] = [a];\nfoo\nlet b = [a];\n");
        let counted = substitute(&text, 0..4, &pattern("a"), "x", true, true, &|| false);
        // `a` in "let a = a;" twice and in "let b = a;" once.
        assert_eq!(counted.matches, 3);
        assert!(counted.edits.is_empty());
    }

    #[test]
    fn global_finds_lines_and_deletes_them() {
        let text = Text::from("keep\ndrop 1\ndrop 2\nkeep\ndrop 3");
        let drop = pattern("drop");
        let lines = matching_lines(&text, 0..5, &drop, false, &|| false);
        assert_eq!(lines, vec![1, 2, 4]);
        let mut s = text.to_string();
        for edit in delete_lines(&text, &lines).iter().rev() {
            s.replace_range(edit.range.clone(), &edit.text);
        }
        assert_eq!(s, "keep\nkeep");
        assert_eq!(
            matching_lines(&text, 0..5, &drop, true, &|| false),
            vec![0, 3]
        );
    }
}
