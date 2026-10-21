//! What changed between two checkpoint commits (or trees) of a repository, bounded like
//! `worktree.diff`: 2 MiB of patch per file, 20 MiB in all, 5 000 files, binary files flagged
//! (`checkpoint.diff`, protocol 0.37).
//!
//! Every call is a read of the repository's object store (`git diff-tree` in its common
//! directory, through the checkpoint runner: no hook, no external diff, no text conversion, no
//! renames), so it works for a checkout that was removed as long as the repository is there.
//! The patch is read up to the total bound and git is killed past it.

use std::collections::HashMap;
use std::path::Path;

use hutan_protocol::checkpoint::{
    CheckpointDiff, CheckpointFile, CheckpointFileStatus, DIFF_FILE_PATCH_CAP, DIFF_MAX_FILES,
    DIFF_TOTAL_PATCH_CAP,
};

use super::git::nul_records;
use super::{CheckpointError, Checkpoints};

/// The bounds of a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// One file's part of the patch is cut here.
    pub file_patch: usize,
    /// The patch is not read beyond this.
    pub total_patch: usize,
    /// At most this many files are listed.
    pub files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file_patch: DIFF_FILE_PATCH_CAP,
            total_patch: DIFF_TOTAL_PATCH_CAP,
            files: DIFF_MAX_FILES,
        }
    }
}

/// `diff-tree` options of every call: whole paths, no rename detection, no program a
/// repository's attributes or the user's configuration could start, plain output.
const DIFF_TREE: &[&str] = &[
    "diff-tree",
    "-r",
    "--no-renames",
    "--no-ext-diff",
    "--no-textconv",
    "--no-color",
];

impl Checkpoints {
    /// The files that differ from `from` to `to` (commits or trees of the repository at
    /// `common_dir`), within `path` (relative, a file or a directory) when given, and their
    /// patch unless `stat`.
    pub async fn diff(
        &self,
        common_dir: &Path,
        from: &str,
        to: &str,
        path: Option<&str>,
        stat: bool,
    ) -> Result<CheckpointDiff, CheckpointError> {
        self.diff_within(common_dir, from, to, path, stat, Limits::default())
            .await
    }

    /// [`diff`](Self::diff) within `limits`.
    pub async fn diff_within(
        &self,
        common_dir: &Path,
        from: &str,
        to: &str,
        path: Option<&str>,
        stat: bool,
        limits: Limits,
    ) -> Result<CheckpointDiff, CheckpointError> {
        let git = self.repo_git(common_dir);
        let pathspec = path.map(|p| format!(":(literal){p}"));
        let pathspec = pathspec.as_deref();
        let names_refs = diff_args(&["-z", "--name-status"], from, to, pathspec);
        let stat_refs = diff_args(&["-z", "--numstat"], from, to, pathspec);
        let patch_refs = diff_args(
            &["-p", "--src-prefix=a/", "--dst-prefix=b/"],
            from,
            to,
            pathspec,
        );
        let (names, stats, patch) =
            tokio::join!(git.read_ok(&names_refs), git.read_ok(&stat_refs), async {
                if stat {
                    Ok(None)
                } else {
                    git.read_capped(&patch_refs, limits.total_patch)
                        .await
                        .map(Some)
                }
            },);
        Ok(assemble(
            &names?.stdout,
            &stats?.stdout,
            patch?.as_ref().map(|(bytes, cut)| (bytes.as_slice(), *cut)),
            limits,
        ))
    }
}

/// `diff-tree` with [`DIFF_TREE`], `mode`, the two sides and the pathspec.
fn diff_args<'a>(
    mode: &[&'a str],
    from: &'a str,
    to: &'a str,
    pathspec: Option<&'a str>,
) -> Vec<&'a str> {
    DIFF_TREE
        .iter()
        .chain(mode)
        .copied()
        .chain([from, to, "--"])
        .chain(pathspec)
        .collect()
}

/// The answer from `diff-tree -z --name-status`, `-z --numstat` and the patch (with whether it
/// was cut at the total bound); every file is in the patch once, in the same order.
pub(crate) fn assemble(
    names: &[u8],
    stats: &[u8],
    patch: Option<(&[u8], bool)>,
    limits: Limits,
) -> CheckpointDiff {
    let counts = parse_numstat(stats);
    let mut files: Vec<CheckpointFile> = parse_name_status(names)
        .into_iter()
        .map(|(status, path)| {
            let (added, removed, binary) = counts.get(path.as_str()).copied().unwrap_or_default();
            CheckpointFile {
                path,
                status,
                added,
                removed,
                binary,
                truncated: false,
            }
        })
        .collect();
    let mut truncated = false;
    if files.len() > limits.files {
        files.truncate(limits.files);
        truncated = true;
    }
    let patch = patch.map(|(bytes, cut)| {
        let parts = split_parts(bytes);
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len().min(limits.total_patch));
        for (index, file) in files.iter_mut().enumerate() {
            let Some(part) = parts.get(index) else {
                // Past the total bound: not in the patch at all.
                file.truncated = true;
                continue;
            };
            let incomplete = cut && index + 1 == parts.len();
            if part.len() > limits.file_patch || incomplete {
                let room = part.len().min(limits.file_patch);
                // Whole lines only.
                let keep = part[..room]
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i + 1);
                out.extend_from_slice(&part[..keep]);
                file.truncated = true;
            } else {
                out.extend_from_slice(part);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    });
    truncated |= files.iter().any(|f| f.truncated);
    CheckpointDiff {
        files,
        patch,
        truncated,
    }
}

/// `X NUL path NUL` records of `diff-tree -z --name-status --no-renames`.
fn parse_name_status(bytes: &[u8]) -> Vec<(CheckpointFileStatus, String)> {
    let mut fields = nul_records(bytes);
    let mut out = Vec::new();
    while let (Some(code), Some(path)) = (fields.next(), fields.next()) {
        let status = match code.first() {
            Some(b'A') => CheckpointFileStatus::Added,
            Some(b'D') => CheckpointFileStatus::Deleted,
            // M, T (a type change) and whatever git adds.
            _ => CheckpointFileStatus::Modified,
        };
        out.push((status, String::from_utf8_lossy(path).into_owned()));
    }
    out
}

/// `added TAB removed TAB path NUL` records of `diff-tree -z --numstat`; `-` for a binary file.
fn parse_numstat(bytes: &[u8]) -> HashMap<String, (u64, u64, bool)> {
    let mut out = HashMap::new();
    for record in nul_records(bytes) {
        let mut fields = record.splitn(3, |b| *b == b'\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let binary = added == b"-" || removed == b"-";
        let number = |digits: &[u8]| {
            std::str::from_utf8(digits)
                .ok()
                .and_then(|d| d.parse::<u64>().ok())
                .unwrap_or(0)
        };
        out.insert(
            String::from_utf8_lossy(path).into_owned(),
            (number(added), number(removed), binary),
        );
    }
    out
}

/// One slice per file of a unified diff, split at its `diff --git ` header lines.
fn split_parts(patch: &[u8]) -> Vec<&[u8]> {
    const HEADER: &[u8] = b"diff --git ";
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    while at < patch.len() {
        if patch[at..].starts_with(HEADER) {
            starts.push(at);
        }
        match patch[at..].iter().position(|b| *b == b'\n') {
            Some(end) => at += end + 1,
            None => break,
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(i, start)| &patch[*start..starts.get(i + 1).copied().unwrap_or(patch.len())])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(path: &str, lines: usize) -> String {
        let mut text =
            format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1 @@\n");
        for n in 0..lines {
            text.push_str(&format!("+line {n}\n"));
        }
        text
    }

    fn names(files: &[(&str, &str)]) -> Vec<u8> {
        files
            .iter()
            .flat_map(|(code, path)| format!("{code}\0{path}\0").into_bytes())
            .collect()
    }

    #[test]
    fn statuses_counts_and_the_binary_flag_come_from_git() {
        let listed = names(&[("A", "a.png"), ("D", "b.txt"), ("M", "c d.rs"), ("T", "e")]);
        let stats = b"-\t-\ta.png\x000\t3\tb.txt\x002\t1\tc d.rs\x001\t1\te\x00";
        let diff = assemble(&listed, stats, None, Limits::default());
        let got: Vec<_> = diff
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.removed, f.binary))
            .collect();
        assert_eq!(
            got,
            [
                ("a.png", CheckpointFileStatus::Added, 0, 0, true),
                ("b.txt", CheckpointFileStatus::Deleted, 0, 3, false),
                ("c d.rs", CheckpointFileStatus::Modified, 2, 1, false),
                ("e", CheckpointFileStatus::Modified, 1, 1, false),
            ]
        );
        assert_eq!(diff.patch, None);
        assert!(!diff.truncated);
    }

    #[test]
    fn a_large_file_is_cut_on_a_line_and_files_past_the_total_are_left_out() {
        let limits = Limits {
            file_patch: 200,
            total_patch: 10_000,
            files: 10,
        };
        let listed = names(&[("A", "big"), ("A", "small")]);
        let patch = part("big", 100) + &part("small", 2);
        let diff = assemble(&listed, b"", Some((patch.as_bytes(), false)), limits);
        let text = diff.patch.unwrap();
        assert!(diff.files[0].truncated && !diff.files[1].truncated);
        assert!(diff.truncated);
        let (big, small) = text.split_at(text.find("diff --git a/small").unwrap());
        assert!(big.len() <= 200 && big.ends_with('\n'), "{big:?}");
        assert_eq!(small, part("small", 2));

        // The total bound cut the patch inside the second file; the third is not in it.
        let listed = names(&[("A", "a"), ("A", "b"), ("A", "c")]);
        let whole = part("a", 2) + &part("b", 50);
        let cut = &whole.as_bytes()[..whole.len() - 7];
        let diff = assemble(&listed, b"", Some((cut, true)), limits);
        let flags: Vec<bool> = diff.files.iter().map(|f| f.truncated).collect();
        assert_eq!(flags, [false, true, true]);
        assert!(diff.patch.unwrap().ends_with('\n'));
    }

    #[test]
    fn more_files_than_the_bound_are_cut_from_the_list_and_the_patch() {
        let limits = Limits {
            files: 2,
            ..Limits::default()
        };
        let listed = names(&[("M", "a"), ("M", "b"), ("M", "c")]);
        let patch = part("a", 1) + &part("b", 1) + &part("c", 1);
        let diff = assemble(&listed, b"", Some((patch.as_bytes(), false)), limits);
        assert_eq!(diff.files.len(), 2);
        assert!(diff.truncated);
        assert!(!diff.patch.unwrap().contains("a/c"));
    }
}
