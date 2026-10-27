//! The Files panel's git decorations (FX-E2E2-C): what each changed file of the project's
//! checkout is, as a letter and a tint, and which folders hold changes (a dot).
//!
//! The answer is the file list of `worktree.diff` (no patches), read when the panel gets a root,
//! on `fs.changed` and when a checkout's activity ends, one read in flight at a time; a frame
//! only looks marks up. A path is decorated for what is **uncommitted** (git's own `status`):
//! committed changes of a branch are not marks.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use hutan_protocol::worktree::{DiffFile, DiffStatus};

use crate::theme::Theme;

/// What git says of one file that is not committed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitMark {
    Modified,
    /// New and staged.
    Added,
    /// New and not tracked.
    Untracked,
    Deleted,
    Renamed,
}

impl GitMark {
    /// The letter beside the name: the mark is never colour alone.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Untracked => "U",
            Self::Deleted => "D",
            Self::Renamed => "R",
        }
    }

    /// What the letter's tooltip says.
    pub fn words(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Added => "Added (staged)",
            Self::Untracked => "Untracked",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
        }
    }

    pub fn color(self, theme: &Theme) -> gpui::Hsla {
        match self {
            Self::Modified => theme.diff_modified,
            Self::Added | Self::Untracked => theme.diff_added,
            Self::Deleted => theme.diff_removed,
            Self::Renamed => theme.diff_renamed,
        }
    }

    /// The mark of a listed file; `None` when everything it changed is committed.
    pub fn of(file: &DiffFile) -> Option<Self> {
        if !file.uncommitted {
            return None;
        }
        Some(match file.status {
            DiffStatus::Modified => Self::Modified,
            DiffStatus::Deleted => Self::Deleted,
            DiffStatus::Renamed => Self::Renamed,
            // New and only in the working tree: untracked; in the index: added.
            DiffStatus::Added if file.staged => Self::Added,
            DiffStatus::Added => Self::Untracked,
        })
    }
}

/// The marks of a root: its changed files and the folders that hold them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitStatus {
    marks: HashMap<PathBuf, GitMark>,
    dirs: HashSet<PathBuf>,
}

impl GitStatus {
    /// The marks for `files` (relative to the checkout `root`).
    pub fn from_files(root: &Path, files: &[DiffFile]) -> Self {
        let mut status = Self::default();
        for file in files {
            let Some(mark) = GitMark::of(file) else {
                continue;
            };
            let path = root.join(&file.path);
            let mut dir = path.parent();
            while let Some(parent) = dir.filter(|d| d.starts_with(root) && *d != root) {
                status.dirs.insert(parent.to_owned());
                dir = parent.parent();
            }
            status.marks.insert(path, mark);
        }
        status
    }

    /// The root's own row (a workspace's tree) holds the changes below it.
    pub fn carry_root(&mut self, root: &Path) {
        if !self.marks.is_empty() {
            self.dirs.insert(root.to_owned());
        }
    }

    /// Adds the marks of another root.
    pub fn merge(&mut self, other: Self) {
        self.marks.extend(other.marks);
        self.dirs.extend(other.dirs);
    }

    pub fn mark(&self, path: &Path) -> Option<GitMark> {
        self.marks.get(path).copied()
    }

    /// A folder (below the root) that holds a changed file at any depth.
    pub fn has_changes_in(&self, dir: &Path) -> bool {
        self.dirs.contains(dir)
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, status: DiffStatus, staged: bool, unstaged: bool) -> DiffFile {
        DiffFile {
            path: path.to_owned(),
            old_path: None,
            status,
            additions: 1,
            deletions: 0,
            binary: false,
            committed: false,
            uncommitted: staged || unstaged,
            staged,
            unstaged,
        }
    }

    #[test]
    fn marks_follow_what_is_uncommitted() {
        let root = Path::new("/p");
        let files = [
            file("a.rs", DiffStatus::Modified, false, true),
            file("src/b.rs", DiffStatus::Added, true, false),
            file("src/deep/c.rs", DiffStatus::Added, false, true),
            file("gone.rs", DiffStatus::Deleted, false, true),
            file("committed.rs", DiffStatus::Modified, false, false),
        ];
        let status = GitStatus::from_files(root, &files);
        assert_eq!(status.mark(Path::new("/p/a.rs")), Some(GitMark::Modified));
        assert_eq!(status.mark(Path::new("/p/src/b.rs")), Some(GitMark::Added));
        assert_eq!(
            status.mark(Path::new("/p/src/deep/c.rs")),
            Some(GitMark::Untracked)
        );
        assert_eq!(status.mark(Path::new("/p/gone.rs")), Some(GitMark::Deleted));
        assert_eq!(status.mark(Path::new("/p/committed.rs")), None);
        assert!(status.has_changes_in(Path::new("/p/src")));
        assert!(status.has_changes_in(Path::new("/p/src/deep")));
        assert!(
            !status.has_changes_in(Path::new("/p")),
            "the root has no dot"
        );
        assert!(!status.has_changes_in(Path::new("/p/other")));
    }

    #[test]
    fn every_mark_has_its_own_letter() {
        let letters: HashSet<_> = [
            GitMark::Modified,
            GitMark::Added,
            GitMark::Untracked,
            GitMark::Deleted,
            GitMark::Renamed,
        ]
        .into_iter()
        .map(GitMark::letter)
        .collect();
        assert_eq!(letters.len(), 5);
    }
}
