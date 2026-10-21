//! Unified diffs as the chat view draws them: per file, per line, with counts.
//!
//! Codex reports a file change as the new file's content (added), the old content (deleted) or
//! a unified diff (updated), and a turn's changes as one `git`-style patch of many files.
//! [`from_change`] and [`split_patch`] read both into the same [`FileDiff`].

use hutan_core::agent::{FileChange, FileChangeKind};

/// Lines of one file that a view draws before it cuts the diff short.
pub const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Add,
    Remove,
    Context,
    /// `@@ -1,3 +1,4 @@`: where the next lines are.
    Hunk,
    /// `\ No newline at end of file`.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// Line numbers in the old and the new file (`None` where the line is not in it).
    pub old: Option<u32>,
    pub new: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The path before a rename.
    pub moved_from: Option<String>,
    pub kind: FileChangeKind,
    pub added: usize,
    pub removed: usize,
    pub lines: Vec<DiffLine>,
}

impl FileDiff {
    fn new(path: String, kind: FileChangeKind) -> Self {
        Self {
            path,
            moved_from: None,
            kind,
            added: 0,
            removed: 0,
            lines: Vec::new(),
        }
    }

    fn push(&mut self, kind: LineKind, text: &str, old: Option<u32>, new: Option<u32>) {
        match kind {
            LineKind::Add => self.added += 1,
            LineKind::Remove => self.removed += 1,
            _ => {}
        }
        self.lines.push(DiffLine {
            kind,
            text: text.to_owned(),
            old,
            new,
        });
    }

    /// `+3 −1`.
    pub fn counts(&self) -> String {
        format!("+{} −{}", self.added, self.removed)
    }

    /// The 1-based line of the new file where the first change is: the first added line, or
    /// the line removed lines were above ("Open" goes there).
    pub fn first_changed_line(&self) -> Option<u32> {
        self.changed_line_from(0)
    }

    /// [`first_changed_line`](Self::first_changed_line) from line `from` of [`lines`](Self::lines)
    /// on (a hunk's header: that hunk's first change).
    pub fn changed_line_from(&self, from: usize) -> Option<u32> {
        let mut next_new: Option<u32> = None;
        for line in self.lines.get(from..)? {
            match line.kind {
                LineKind::Hunk => {
                    next_new = line
                        .text
                        .strip_prefix("@@")
                        .and_then(hunk_start)
                        .map(|(_, new)| new);
                }
                LineKind::Context => next_new = line.new.map(|n| n + 1),
                LineKind::Add => return line.new,
                LineKind::Remove => return Some(next_new.unwrap_or(1).max(1)),
                LineKind::Note => {}
            }
        }
        None
    }
}

/// The diff of one reported change. `patch` holds the file's content for an added or deleted
/// file and a unified diff for an update; a change without text (a binary file, a mode change)
/// has no lines.
pub fn from_change(change: &FileChange) -> FileDiff {
    let mut diff = FileDiff::new(change.path.clone(), change.kind);
    diff.moved_from = change.move_path.clone();
    let Some(patch) = change.patch.as_deref() else {
        return diff;
    };
    match change.kind {
        FileChangeKind::Add if !looks_like_diff(patch) => whole_file(&mut diff, patch, true),
        FileChangeKind::Delete if !looks_like_diff(patch) => whole_file(&mut diff, patch, false),
        _ => read_unified(&mut diff, patch),
    }
    diff
}

fn looks_like_diff(text: &str) -> bool {
    text.starts_with("@@") || text.starts_with("diff --git") || text.starts_with("--- ")
}

fn whole_file(diff: &mut FileDiff, content: &str, added: bool) {
    for (index, line) in content.lines().enumerate() {
        let number = u32::try_from(index + 1).ok();
        if added {
            diff.push(LineKind::Add, line, None, number);
        } else {
            diff.push(LineKind::Remove, line, number, None);
        }
    }
}

/// Every file of a `git`-style patch (`diff --git a/x b/x` sections); a patch without those
/// headers is one unnamed file.
pub fn split_patch(patch: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut section: Vec<&str> = Vec::new();
    let flush = |section: &mut Vec<&str>, files: &mut Vec<FileDiff>| {
        if section.is_empty() {
            return;
        }
        files.push(read_section(section));
        section.clear();
    };
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            flush(&mut section, &mut files);
        }
        section.push(line);
    }
    flush(&mut section, &mut files);
    files
}

/// One file of a patch: headers (`diff --git`, `new file mode`, `rename from`, `---`, `+++`)
/// name it and tell what happened to it, then hunks follow.
fn read_section(section: &[&str]) -> FileDiff {
    let mut path = String::new();
    let mut moved_from = None;
    let mut kind = FileChangeKind::Update;
    let mut body_start = section.len();
    for (index, line) in section.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/old b/new`: the new name is the second half (paths with spaces are cut at the
            // first ` b/`).
            if let Some((old, new)) = rest.split_once(" b/") {
                path = new.to_owned();
                if old.strip_prefix("a/").is_some_and(|o| o != new) {
                    moved_from = old.strip_prefix("a/").map(str::to_owned);
                }
            }
        } else if line.starts_with("new file mode") {
            kind = FileChangeKind::Add;
        } else if line.starts_with("deleted file mode") {
            kind = FileChangeKind::Delete;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            moved_from = Some(from.to_owned());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            path = to.to_owned();
        } else if let Some(name) = line.strip_prefix("+++ ") {
            if let Some(name) = name.strip_prefix("b/") {
                path = name.to_owned();
            }
        } else if line.starts_with("@@") {
            body_start = index;
            break;
        }
    }
    let mut diff = FileDiff::new(path, kind);
    diff.moved_from = moved_from;
    read_unified(
        &mut diff,
        &section[body_start.min(section.len())..].join("\n"),
    );
    diff
}

/// Hunks of a unified diff into `diff`. Header lines before the first hunk are skipped; text
/// without any hunk header is read line by line by its first character.
fn read_unified(diff: &mut FileDiff, text: &str) {
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            (old, new) = hunk_start(rest).unwrap_or((1, 1));
            in_hunk = true;
            diff.push(LineKind::Hunk, line, None, None);
            continue;
        }
        if !in_hunk {
            // Headers (`diff --git`, `index`, `---`, `+++`, modes) tell nothing the card does not.
            if line.starts_with("diff --git ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("new file mode")
                || line.starts_with("deleted file mode")
                || line.starts_with("similarity index")
                || line.starts_with("rename ")
                || line.starts_with("old mode")
                || line.starts_with("new mode")
                || line.starts_with("Binary files")
            {
                continue;
            }
            // No hunk header at all: number from 1.
            in_hunk = true;
            (old, new) = (1, 1);
        }
        match line.chars().next() {
            Some('+') => {
                diff.push(LineKind::Add, &line[1..], None, Some(new));
                new += 1;
            }
            Some('-') => {
                diff.push(LineKind::Remove, &line[1..], Some(old), None);
                old += 1;
            }
            Some('\\') => diff.push(LineKind::Note, line, None, None),
            Some(' ') => {
                diff.push(LineKind::Context, &line[1..], Some(old), Some(new));
                old += 1;
                new += 1;
            }
            // An empty context line that lost its space.
            None => {
                diff.push(LineKind::Context, "", Some(old), Some(new));
                old += 1;
                new += 1;
            }
            Some(_) => {
                diff.push(LineKind::Context, line, Some(old), Some(new));
                old += 1;
                new += 1;
            }
        }
    }
}

/// `-1,3 +1,4 @@ context` after the leading `@@`: the first old and new line numbers.
fn hunk_start(rest: &str) -> Option<(u32, u32)> {
    let mut parts = rest.split_whitespace();
    let number = |part: &str, sign: char| -> Option<u32> {
        part.strip_prefix(sign)?
            .split(',')
            .next()?
            .parse::<u32>()
            .ok()
    };
    let old = number(parts.next()?, '-')?;
    let new = number(parts.next()?, '+')?;
    Some((old, new))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(kind: FileChangeKind, patch: Option<&str>) -> FileChange {
        FileChange {
            path: "src/a.rs".into(),
            kind,
            move_path: None,
            patch: patch.map(str::to_owned),
            before: None,
            after: None,
        }
    }

    #[test]
    fn an_added_file_is_all_additions() {
        let diff = from_change(&change(FileChangeKind::Add, Some("hi\nthere\n")));
        assert_eq!((diff.added, diff.removed), (2, 0));
        assert_eq!(diff.lines[1].new, Some(2));
        assert_eq!(diff.counts(), "+2 −0");
    }

    #[test]
    fn open_goes_to_the_first_change_of_the_file_or_of_a_hunk() {
        let added = from_change(&change(FileChangeKind::Add, Some("hi\n")));
        assert_eq!(added.first_changed_line(), Some(1));
        let patch = "@@ -10,3 +10,3 @@\n a\n-b\n+B\n c\n@@ -40,2 +40,1 @@\n x\n-y\n";
        let diff = from_change(&change(FileChangeKind::Update, Some(patch)));
        assert_eq!(diff.first_changed_line(), Some(11), "the added line");
        let second = diff
            .lines
            .iter()
            .rposition(|l| l.kind == LineKind::Hunk)
            .unwrap();
        assert_eq!(
            diff.changed_line_from(second),
            Some(41),
            "a removal sits where the next line is"
        );
        assert_eq!(diff.changed_line_from(99), None);
    }

    #[test]
    fn a_deleted_file_is_all_removals() {
        let diff = from_change(&change(FileChangeKind::Delete, Some("gone\n")));
        assert_eq!((diff.added, diff.removed), (0, 1));
        assert_eq!(diff.lines[0].old, Some(1));
    }

    #[test]
    fn an_update_reads_hunks_and_numbers_lines() {
        let patch = "@@ -3,3 +3,4 @@ fn x()\n keep\n-old\n+new\n+extra\n tail\n";
        let diff = from_change(&change(FileChangeKind::Update, Some(patch)));
        assert_eq!((diff.added, diff.removed), (2, 1));
        let kinds: Vec<_> = diff.lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Hunk,
                LineKind::Context,
                LineKind::Remove,
                LineKind::Add,
                LineKind::Add,
                LineKind::Context
            ]
        );
        assert_eq!(diff.lines[1].old, Some(3));
        assert_eq!(diff.lines[1].new, Some(3));
        assert_eq!(diff.lines[2].old, Some(4));
        assert_eq!(diff.lines[3].new, Some(4));
        assert_eq!(diff.lines[5].old, Some(5));
        assert_eq!(diff.lines[5].new, Some(6));
    }

    #[test]
    fn headers_before_the_hunk_are_skipped() {
        let patch = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let diff = from_change(&change(FileChangeKind::Update, Some(patch)));
        assert_eq!(diff.lines.len(), 3);
        assert_eq!((diff.added, diff.removed), (1, 1));
    }

    #[test]
    fn a_change_without_text_has_no_lines() {
        let diff = from_change(&change(FileChangeKind::Update, None));
        assert!(diff.lines.is_empty());
    }

    #[test]
    fn a_git_patch_splits_into_its_files() {
        let patch = "diff --git a/hello.txt b/hello.txt\nnew file mode 100644\nindex 000..45b\n--- /dev/null\n+++ b/hello.txt\n@@ -0,0 +1 @@\n+hi\ndiff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\nindex 1..2 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n\\ No newline at end of file\n";
        let files = split_patch(patch);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "hello.txt");
        assert_eq!(files[0].kind, FileChangeKind::Add);
        assert_eq!((files[0].added, files[0].removed), (1, 0));
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].moved_from.as_deref(), Some("old.rs"));
        assert_eq!((files[1].added, files[1].removed), (1, 1));
        assert_eq!(files[1].lines.last().unwrap().kind, LineKind::Note);
    }

    #[test]
    fn a_deleted_file_in_a_patch() {
        let patch = "diff --git a/x b/x\ndeleted file mode 100644\n--- a/x\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n";
        let files = split_patch(patch);
        assert_eq!(files[0].kind, FileChangeKind::Delete);
        assert_eq!(files[0].path, "x");
        assert_eq!(files[0].removed, 1);
    }

    #[test]
    fn text_without_a_hunk_header_is_read_by_prefix() {
        let diff = from_change(&change(FileChangeKind::Update, Some("-a\n+b\n")));
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(diff.lines[0].old, Some(1));
    }
}
