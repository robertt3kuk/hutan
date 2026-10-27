//! The git change marks of the gutter (EDITOR-POLISH): which lines of the buffer were added,
//! changed or removed against the file as the index holds it (or `HEAD` when nothing of the file
//! is staged).
//!
//! The owner fetches the **base** text once (`git.file` of `HEAD`, and the file's staged hunks
//! of `worktree.hunks` applied to it: [`apply_hunks`]) and gives it to the buffer
//! ([`FileBuffer::set_git_base`](crate::FileBuffer::set_git_base)); the buffer diffs its text
//! against it in the background after every edit, with the same line diff as an agent's changes
//! ([`hunks_between`](crate::agent::hunks_between)). GPUI-free.

use crate::agent::normalize;

/// The file the marks are drawn against, for the peek's wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBaseKind {
    /// Something of the file is staged: the index holds a text that differs from `HEAD`'s.
    Index,
    /// Nothing of the file is staged: the index and `HEAD` agree.
    Head,
}

impl GitBaseKind {
    /// `index` or `HEAD`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Head => "HEAD",
        }
    }
}

/// The text the gutter compares the buffer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBase {
    /// Normalized: `\n` line breaks, no byte-order mark (what the buffer holds).
    pub text: std::sync::Arc<str>,
    pub kind: GitBaseKind,
}

impl GitBase {
    /// A base from text as git holds it (CRLF and a byte-order mark are dropped).
    pub fn new(text: &str, kind: GitBaseKind) -> Self {
        Self {
            text: std::sync::Arc::from(normalize(text)),
            kind,
        }
    }
}

/// A hunk of a unified diff, as `worktree.hunks` lists it.
#[derive(Debug, Clone, Copy)]
pub struct PatchHunk<'a> {
    /// First line of the old text the hunk covers (1-based; the line *before* the hunk when
    /// `old_lines` is 0, as in `@@ -3,0 +4,2 @@`).
    pub old_start: u32,
    pub old_lines: u32,
    /// The lines after the `@@` line: ` `, `+`, `-` or `\` first, joined by `\n`.
    pub body: &'a str,
}

/// `before` with `hunks` (sorted, not overlapping) applied: the text the diff produced. `None`
/// when a context or removed line is not what `before` has there (the hunks are of another
/// text). A hunk that adds to an empty region names the line before it.
pub fn apply_hunks(before: &str, hunks: &[PatchHunk<'_>]) -> Option<String> {
    let lines: Vec<&str> = before.split_inclusive('\n').collect();
    let mut out = String::with_capacity(before.len());
    let mut at = 0usize;
    for hunk in hunks {
        let start = if hunk.old_lines == 0 {
            hunk.old_start as usize
        } else {
            (hunk.old_start as usize).saturating_sub(1)
        };
        if start < at || start > lines.len() {
            return None;
        }
        for line in &lines[at..start] {
            out.push_str(line);
        }
        at = start;
        // What the last line of `out` came from, for `\ No newline at end of file`.
        let mut last = ' ';
        for body_line in hunk
            .body
            .strip_suffix('\n')
            .unwrap_or(hunk.body)
            .split('\n')
        {
            let mut chars = body_line.chars();
            let side = chars.next();
            let text = chars.as_str();
            match side {
                Some('\\') => {
                    if last == '+' && out.ends_with('\n') {
                        out.pop();
                    }
                    continue;
                }
                Some(' ') | None => {
                    // A context line (an empty one may have lost its space).
                    let line = lines.get(at)?;
                    if line.strip_suffix('\n').unwrap_or(line) != text {
                        return None;
                    }
                    out.push_str(line);
                    at += 1;
                    last = ' ';
                }
                Some('-') => {
                    let line = lines.get(at)?;
                    if line.strip_suffix('\n').unwrap_or(line) != text {
                        return None;
                    }
                    at += 1;
                    last = '-';
                }
                Some('+') => {
                    out.push_str(text);
                    out.push('\n');
                    last = '+';
                }
                Some(_) => return None,
            }
        }
    }
    for line in &lines[at.min(lines.len())..] {
        out.push_str(line);
    }
    Some(out)
}

/// The lines of the new text a patch hunk changes, and where it removes lines: for matching a
/// hunk of the gutter with a hunk of git's diff (the changed lines are the unit; git's hunks
/// also carry context).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeBlock {
    /// Positions of the block's `+`/`-` lines in the hunk's body (from 0).
    pub body_lines: Vec<u32>,
    /// The lines of the new text it adds (0-based, `start..end`; empty for a pure removal, then
    /// both are the line the removed lines sat before).
    pub new_lines: std::ops::Range<usize>,
}

/// The blocks of consecutive changed lines of a hunk (`new_start`: its first line in the new
/// text, 1-based; 0 for an empty new region as in `@@ -1,2 +0,0 @@`).
pub fn change_blocks(new_start: u32, new_lines: u32, body: &str) -> Vec<ChangeBlock> {
    let mut blocks: Vec<ChangeBlock> = Vec::new();
    // The next line of the new text, 0-based.
    let mut next = if new_lines == 0 {
        new_start as usize
    } else {
        (new_start as usize).saturating_sub(1)
    };
    let mut open = false;
    for (index, line) in body.split('\n').enumerate() {
        match line.chars().next() {
            Some('+') => {
                if !open {
                    blocks.push(ChangeBlock {
                        body_lines: Vec::new(),
                        new_lines: next..next,
                    });
                    open = true;
                }
                if let Some(block) = blocks.last_mut() {
                    block.body_lines.push(index as u32);
                    block.new_lines.end = next + 1;
                }
                next += 1;
            }
            Some('-') => {
                if !open {
                    blocks.push(ChangeBlock {
                        body_lines: Vec::new(),
                        new_lines: next..next,
                    });
                    open = true;
                }
                if let Some(block) = blocks.last_mut() {
                    block.body_lines.push(index as u32);
                }
            }
            Some('\\') => {
                // `\ No newline at end of file` belongs to the line before; it is not a
                // changed line of its own.
            }
            _ => {
                open = false;
                next += 1;
            }
        }
    }
    blocks
}

/// Which of git's hunks (`new_start`, `new_lines`, `body` of each, as `worktree.hunks` lists
/// the unstaged ones) holds the change the gutter shows at `first..end` (0-based lines of the
/// buffer; for removed lines `end == first`): the hunk's index and the body lines of the
/// change blocks that touch it (what `HunkRef::lines` names). A removal matches the removal
/// that sits just before line `first`.
pub fn match_change(
    hunks: &[(u32, u32, &str)],
    first: usize,
    end: usize,
) -> Option<(usize, Vec<u32>)> {
    for (index, (new_start, new_lines, body)) in hunks.iter().enumerate() {
        let blocks = change_blocks(*new_start, *new_lines, body);
        let lines: Vec<u32> = blocks
            .iter()
            .filter(|block| {
                if end == first {
                    block.new_lines.is_empty() && block.new_lines.start == first
                } else {
                    !block.new_lines.is_empty()
                        && block.new_lines.start < end
                        && first < block.new_lines.end
                }
            })
            .flat_map(|block| block.body_lines.iter().copied())
            .collect();
        if !lines.is_empty() {
            return Some((index, lines));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{HunkKind, hunks_between};

    fn marks(base: &str, current: &str) -> Vec<(HunkKind, String, String)> {
        hunks_between(&normalize(base), &normalize(current))
            .into_iter()
            .map(|h| {
                (
                    h.kind(),
                    normalize(current)[h.bytes.clone()].to_owned(),
                    h.base_text.to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn an_insert_at_the_start_the_middle_and_the_end() {
        assert_eq!(
            marks("b\nc\n", "a\nb\nc\n"),
            [(HunkKind::Added, "a\n".into(), String::new())]
        );
        assert_eq!(
            marks("a\nc\n", "a\nb\nc\n"),
            [(HunkKind::Added, "b\n".into(), String::new())]
        );
        assert_eq!(
            marks("a\nb\n", "a\nb\nc\n"),
            [(HunkKind::Added, "c\n".into(), String::new())]
        );
    }

    #[test]
    fn a_modified_line_at_either_edge() {
        assert_eq!(
            marks("a\nb\nc\n", "A\nb\nc\n"),
            [(HunkKind::Modified, "A\n".into(), "a\n".into())]
        );
        assert_eq!(
            marks("a\nb\nc\n", "a\nb\nC\n"),
            [(HunkKind::Modified, "C\n".into(), "c\n".into())]
        );
    }

    #[test]
    fn removed_lines_are_a_mark_where_they_were() {
        let hunks = hunks_between("a\nb\nc\n", "b\nc\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), HunkKind::Removed);
        assert_eq!(hunks[0].bytes, 0..0, "above the first line");
        let hunks = hunks_between("a\nb\nc\n", "a\nb\n");
        assert_eq!(hunks[0].kind(), HunkKind::Removed);
        assert_eq!(hunks[0].bytes, 4..4, "past the last line");
        assert_eq!(&*hunks[0].base_text, "c\n");
    }

    #[test]
    fn a_file_made_empty_or_filled_is_one_hunk() {
        let hunks = hunks_between("a\nb\n", "");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), HunkKind::Removed);
        let hunks = hunks_between("", "a\nb\n");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), HunkKind::Added);
        assert_eq!(hunks[0].bytes, 0..4);
    }

    #[test]
    fn crlf_and_a_byte_order_mark_in_the_base_are_not_changes() {
        let base = GitBase::new("\u{feff}a\r\nb\r\nc\r\n", GitBaseKind::Head);
        assert_eq!(&*base.text, "a\nb\nc\n");
        assert!(hunks_between(&base.text, "a\nb\nc\n").is_empty());
        let edited = hunks_between(&base.text, "a\nB\nc\n");
        assert_eq!(edited.len(), 1);
        assert_eq!(&*edited[0].base_text, "b\n");
    }

    #[test]
    fn a_missing_final_newline_changes_the_last_line() {
        let hunks = hunks_between("a\nb\n", "a\nb");
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].kind(), HunkKind::Modified);
        assert_eq!(hunks[0].bytes, 2..3);
    }

    #[test]
    fn staged_hunks_make_the_index_text() {
        let head = "one\ntwo\nthree\nfour\nfive\n";
        let index = apply_hunks(
            head,
            &[
                PatchHunk {
                    old_start: 1,
                    old_lines: 3,
                    body: " one\n-two\n+TWO\n three",
                },
                PatchHunk {
                    old_start: 5,
                    old_lines: 1,
                    body: " five\n+six",
                },
            ],
        )
        .unwrap();
        assert_eq!(index, "one\nTWO\nthree\nfour\nfive\nsix\n");
        // An addition to an empty region names the line before it.
        assert_eq!(
            apply_hunks(
                "a\nb\n",
                &[PatchHunk {
                    old_start: 1,
                    old_lines: 0,
                    body: "+x"
                }]
            )
            .unwrap(),
            "a\nx\nb\n"
        );
        // Hunks of another text do not apply.
        assert!(
            apply_hunks(
                "zzz\n",
                &[PatchHunk {
                    old_start: 1,
                    old_lines: 1,
                    body: "-one\n+two"
                }]
            )
            .is_none()
        );
    }

    #[test]
    fn a_hunk_about_the_missing_final_newline_applies() {
        let index = apply_hunks(
            "a\nb",
            &[PatchHunk {
                old_start: 1,
                old_lines: 2,
                body: " a\n-b\n\\ No newline at end of file\n+b\n+c\n\\ No newline at end of file",
            }],
        )
        .unwrap();
        assert_eq!(index, "a\nb\nc");
    }

    #[test]
    fn a_change_of_the_gutter_finds_its_lines_in_gits_hunk() {
        // Two changes close enough that git makes one hunk of them.
        let hunks = [(1u32, 7u32, " a\n-b\n+B\n c\n d\n+e\n f")];
        // The gutter's second change: line index 4 ("e").
        let (index, lines) = match_change(&hunks, 4, 5).unwrap();
        assert_eq!((index, lines), (0, vec![5]));
        // The first: line index 1.
        assert_eq!(match_change(&hunks, 1, 2).unwrap().1, vec![1, 2]);
        // No change there.
        assert!(match_change(&hunks, 3, 4).is_none());
        // Removed lines: the block with no new lines before line `first`.
        let removal = [(1u32, 2u32, " a\n-b\n c")];
        assert_eq!(match_change(&removal, 1, 1).unwrap().1, vec![1]);
        assert!(match_change(&removal, 2, 2).is_none());
    }

    #[test]
    fn change_blocks_split_a_hunk_at_its_context() {
        let body = " a\n-b\n+B\n c\n d\n+e\n f";
        let blocks = change_blocks(1, 6, body);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].body_lines, [1, 2]);
        assert_eq!(blocks[0].new_lines, 1..2);
        assert_eq!(blocks[1].body_lines, [5]);
        assert_eq!(blocks[1].new_lines, 4..5);
        // A removal is a block with no new lines, placed before the line that followed it.
        let blocks = change_blocks(1, 2, " a\n-b\n c");
        assert_eq!(blocks[0].new_lines, 1..1);
        assert_eq!(blocks[0].body_lines, [1]);
    }
}
