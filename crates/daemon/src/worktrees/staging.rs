//! Staging (protocol 0.53, capability `worktree.stage`, ADR 0020 amendment): `worktree.hunks`,
//! `worktree.stage`, `worktree.unstage` and `worktree.discard`.
//!
//! Hunks are read from `git diff` (working tree against the index: *unstaged*) and `git diff
//! --cached` (index against `HEAD`: *staged*), both without rename detection and with the review's
//! options (no external diff, no textconv, literal pathspecs); an untracked file is diffed with
//! `--no-index` and has one unstaged hunk. A hunk's id digests its side, path and text, so a
//! request that names a hunk looks the hunks up again and refuses (`conflict`) one that is gone.
//!
//! Staging whole files is `git add -A` / `git reset`; staging a hunk applies a patch made of the
//! hunks asked for with `git apply --cached` (`--reverse` to unstage) — never the working tree.
//! A patch is checked with `git apply --check` before anything changes. Discarding a hunk applies
//! it in reverse to the working tree; discarding a file restores it from the index (or `HEAD`)
//! or deletes it when git does not track it. Every discard runs between two snapshots of the
//! checkout ([`crate::checkpoints::turns::TurnCheckpoints::guarded_change`]), so Undo exists.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::Path;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::worktree::{
    DiffHunk, FileHunks, HunkRef, WorktreeDiscard, WorktreeDiscarded, WorktreeHunks,
    WorktreeHunksRequest, WorktreeIndexChange, WorktreeIndexed,
};
use sha2::{Digest, Sha256};

use super::review::{
    DIFF_TIMEOUT, FILE_PATCH_CAP, MAX_FILES, TOTAL_PATCH_CAP, diff_prefix, git,
    operation_in_progress, os_refs, run_git_capped, run_git_capped_with_input, validate_relative,
};
use super::{
    READ_TIMEOUT, WORKTREE_MERGE_TIMEOUT, WorktreeService, bad_request, git_failed, invalid_state,
    workspaces_of_repo,
};
use crate::checkpoints::turns::RestoreContext;

// Patches -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Working tree against the index.
    Unstaged,
    /// Index against `HEAD`.
    Staged,
}

impl Side {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::Unstaged => b"unstaged",
            Self::Staged => b"staged",
        }
    }
}

/// Which way a patch is applied to the thing it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// As diffed: staging a hunk applies the unstaged patch to the index.
    Forward,
    /// Reversed: unstaging applies the staged patch backwards to the index, discarding applies
    /// the unstaged patch backwards to the working tree.
    Reverse,
}

/// One hunk of a patch, bytes as git printed them.
#[derive(Debug, Clone)]
struct Hunk {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    /// What follows the closing `@@` of the header, a leading space included.
    section: Vec<u8>,
    /// The lines after the header, each with its prefix, without the newline.
    body: Vec<Vec<u8>>,
    id: String,
}

impl Hunk {
    fn header(&self) -> Vec<u8> {
        let mut line = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
        .into_bytes();
        line.extend_from_slice(&self.section);
        line
    }

    fn digest(&self, side: Side, path: &str) -> String {
        let mut hash = Sha256::new();
        hash.update(side.tag());
        hash.update([0]);
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(self.header());
        for line in &self.body {
            hash.update(b"\n");
            hash.update(line);
        }
        let digest = hash.finalize();
        let mut id = String::with_capacity(16);
        for byte in &digest[..8] {
            id.push_str(&format!("{byte:02x}"));
        }
        id
    }

    fn info(&self) -> DiffHunk {
        let body: Vec<String> = self
            .body
            .iter()
            .map(|line| String::from_utf8_lossy(line).into_owned())
            .collect();
        DiffHunk {
            id: self.id.clone(),
            header: String::from_utf8_lossy(&self.header()).into_owned(),
            old_start: self.old_start,
            old_lines: self.old_lines,
            new_start: self.new_start,
            new_lines: self.new_lines,
            body: body.join("\n"),
        }
    }

    /// `new_lines - old_lines`: how much this hunk moves what follows it.
    fn delta(&self) -> i64 {
        i64::from(self.new_lines) - i64::from(self.old_lines)
    }
}

/// One file of a patch: the lines before its first hunk, and its hunks.
#[derive(Debug, Clone)]
struct FilePatch {
    path: String,
    header: Vec<Vec<u8>>,
    hunks: Vec<Hunk>,
    binary: bool,
}

impl FilePatch {
    fn has_header(&self, prefix: &[u8]) -> bool {
        self.header.iter().any(|line| line.starts_with(prefix))
    }
}

/// `@@ -a[,b] +c[,d] @@ section`.
fn parse_hunk_header(line: &[u8]) -> Option<Hunk> {
    let rest = line.strip_prefix(b"@@ -")?;
    let end = rest.windows(3).position(|w| w == b" @@")?;
    let (numbers, section) = (&rest[..end], &rest[end + 3..]);
    let numbers = std::str::from_utf8(numbers).ok()?;
    let (old, new) = numbers.split_once(" +")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let ((old_start, old_lines), (new_start, new_lines)) = (range(old)?, range(new)?);
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section: section.to_vec(),
        body: Vec::new(),
        id: String::new(),
    })
}

/// The bytes of a C-quoted name (`"a\tb\303\251"`), without the quotes.
fn unquote(quoted: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut at = 1;
    while at < quoted.len() {
        match quoted[at] {
            b'"' => return (out, at + 1),
            b'\\' if at + 1 < quoted.len() => {
                at += 1;
                match quoted[at] {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    digit @ b'0'..=b'7' => {
                        let mut value = u32::from(digit - b'0');
                        let mut taken = 1;
                        while taken < 3
                            && at + 1 < quoted.len()
                            && matches!(quoted[at + 1], b'0'..=b'7')
                        {
                            at += 1;
                            value = value * 8 + u32::from(quoted[at] - b'0');
                            taken += 1;
                        }
                        out.push(u8::try_from(value & 0xff).unwrap_or(0));
                    }
                    other => out.push(other),
                }
            }
            other => out.push(other),
        }
        at += 1;
    }
    (out, quoted.len())
}

/// The file a `diff --git a/X b/X` line names (no renames: both sides are the same name).
fn header_path(rest: &[u8]) -> String {
    let name = if rest.starts_with(b"\"") {
        let (first, _) = unquote(rest);
        first
            .strip_prefix(b"a/")
            .map(<[u8]>::to_vec)
            .unwrap_or(first)
    } else {
        let len = rest.len();
        if len >= 5 && (len - 5).is_multiple_of(2) && rest.starts_with(b"a/") {
            rest[2..2 + (len - 5) / 2].to_vec()
        } else {
            let at = rest
                .windows(3)
                .position(|w| w == b" b/")
                .unwrap_or(rest.len());
            rest[..at]
                .strip_prefix(b"a/")
                .unwrap_or(&rest[..at])
                .to_vec()
        }
    };
    String::from_utf8_lossy(&name).into_owned()
}

/// Splits git's unified diff into files and hunks.
fn parse_patch(side: Side, patch: &[u8]) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    for line in patch.split(|b| *b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            files.push(FilePatch {
                path: header_path(rest),
                header: vec![line.to_vec()],
                hunks: Vec::new(),
                binary: false,
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if line.starts_with(b"@@ ")
            && let Some(hunk) = parse_hunk_header(line)
        {
            file.hunks.push(hunk);
        } else if let Some(hunk) = file.hunks.last_mut() {
            // The empty piece after the last newline is not a line.
            if !line.is_empty() {
                hunk.body.push(line.to_vec());
            }
        } else {
            if line.starts_with(b"Binary files ") || line.starts_with(b"GIT binary patch") {
                file.binary = true;
            }
            file.header.push(line.to_vec());
        }
    }
    for file in &mut files {
        let path = file.path.clone();
        for hunk in &mut file.hunks {
            hunk.id = hunk.digest(side, &path);
        }
    }
    files
}

/// The hunk limited to the changed lines `lines` (numbers into its body) and the others left as
/// they are: going `Forward` an unnamed `+` line is left out and an unnamed `-` line becomes
/// context; going `Reverse` the other way round (an unnamed `+` line stays in the file, an
/// unnamed `-` line stays out of it).
fn restrict(hunk: &Hunk, lines: &[u32], direction: Direction) -> Result<Hunk, ErrorBody> {
    if lines.is_empty() {
        return Ok(hunk.clone());
    }
    let chosen: HashSet<usize> = lines.iter().map(|n| *n as usize).collect();
    for number in &chosen {
        match hunk.body.get(*number).and_then(|line| line.first()) {
            Some(b'+' | b'-') => {}
            _ => {
                return Err(bad_request(format!(
                    "line {number} of hunk {} is not a changed line",
                    hunk.id
                )));
            }
        }
    }
    let mut body: Vec<Vec<u8>> = Vec::new();
    // Whether the last line the loop looked at is in the new body (a `\ No newline` marker
    // follows its line).
    let mut kept_last = false;
    for (number, line) in hunk.body.iter().enumerate() {
        let picked = chosen.contains(&number);
        match line.first() {
            Some(b'+') => {
                kept_last = match (picked, direction) {
                    (true, _) => {
                        body.push(line.clone());
                        true
                    }
                    (false, Direction::Forward) => false,
                    (false, Direction::Reverse) => {
                        body.push(context_of(line));
                        true
                    }
                };
            }
            Some(b'-') => {
                kept_last = match (picked, direction) {
                    (true, _) => {
                        body.push(line.clone());
                        true
                    }
                    (false, Direction::Forward) => {
                        body.push(context_of(line));
                        true
                    }
                    (false, Direction::Reverse) => false,
                };
            }
            Some(b'\\') => {
                if kept_last {
                    body.push(line.clone());
                }
            }
            _ => {
                body.push(line.clone());
                kept_last = true;
            }
        }
    }
    let count = |prefixes: &[u8]| -> u32 {
        let n = body
            .iter()
            .filter(|l| l.first().is_some_and(|b| prefixes.contains(b)))
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    };
    let restricted = Hunk {
        old_lines: count(b" -"),
        new_lines: count(b" +"),
        body,
        ..hunk.clone()
    };
    Ok(restricted)
}

fn context_of(line: &[u8]) -> Vec<u8> {
    let mut out = line.to_vec();
    if let Some(first) = out.first_mut() {
        *first = b' ';
    }
    out
}

/// The patch of the hunks `refs` name, looked up in `files` (the current diff of one side): per
/// file the header, then each chosen hunk in file order. `conflict` when a file or hunk is gone,
/// `bad_request` for a binary file, a hunk named twice or a line that is not a changed line.
fn pick_patch(
    files: &[FilePatch],
    refs: &[HunkRef],
    direction: Direction,
    what: &str,
) -> Result<Vec<u8>, ErrorBody> {
    let mut by_file: BTreeMap<&str, Vec<&HunkRef>> = BTreeMap::new();
    for hunk in refs {
        by_file.entry(hunk.path.as_str()).or_default().push(hunk);
    }
    let mut patch = Vec::new();
    for (path, wanted) in by_file {
        let Some(file) = files.iter().find(|f| f.path == path) else {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                format!(
                    "{path} has no {what} changes any more: it changed since the hunks were listed"
                ),
            ));
        };
        if file.binary {
            return Err(bad_request(format!(
                "{path} is binary: stage, unstage or discard the whole file, not a hunk"
            )));
        }
        let mut seen = HashSet::new();
        for hunk in &wanted {
            if !seen.insert(hunk.hunk_id.as_str()) {
                return Err(bad_request(format!(
                    "hunk {} of {path} is named twice",
                    hunk.hunk_id
                )));
            }
            if !file.hunks.iter().any(|h| h.id == hunk.hunk_id) {
                return Err(ErrorBody::new(
                    ErrorCode::Conflict,
                    format!(
                        "hunk {} of {path} is gone: the file changed since the hunks were listed",
                        hunk.hunk_id
                    ),
                ));
            }
        }
        // Where a hunk starts moves with what the hunks before it leave out or change: `git
        // apply` looks for the lines around the position the header says.
        let mut chosen: Vec<Hunk> = Vec::new();
        // Forward: what earlier hunks would have moved this one by, and did not.
        let mut lost: i64 = 0;
        // Reverse: how far the earlier chosen hunks have already moved the file.
        let mut moved: i64 = 0;
        for hunk in &file.hunks {
            let Some(pick) = wanted.iter().find(|w| w.hunk_id == hunk.id) else {
                lost += hunk.delta();
                continue;
            };
            let mut restricted = restrict(hunk, &pick.lines, direction)?;
            match direction {
                Direction::Forward => {
                    restricted.new_start = shifted(hunk.new_start, -lost);
                    lost += hunk.delta() - restricted.delta();
                }
                Direction::Reverse => {
                    restricted.old_start = shifted(hunk.new_start, -moved);
                    restricted.new_start = hunk.new_start;
                    moved += restricted.delta();
                }
            }
            chosen.push(restricted);
        }
        patch.extend(render_file(file, &chosen));
    }
    Ok(patch)
}

fn shifted(start: u32, by: i64) -> u32 {
    u32::try_from((i64::from(start) + by).max(0)).unwrap_or(u32::MAX)
}

/// A file's header and `hunks` as a patch git can apply. A file git creates or deletes whole is
/// applied as a plain edit when only some of its lines are taken.
fn render_file(file: &FilePatch, hunks: &[Hunk]) -> Vec<u8> {
    let creates = file.has_header(b"new file mode ") && hunks.iter().any(|h| h.old_lines > 0);
    let deletes = file.has_header(b"deleted file mode ") && hunks.iter().any(|h| h.new_lines > 0);
    let mut out = Vec::new();
    for line in &file.header {
        if (creates && line.starts_with(b"new file mode "))
            || (deletes && line.starts_with(b"deleted file mode "))
        {
            continue;
        }
        if creates && line.starts_with(b"--- /dev/null") {
            out.extend_from_slice(format!("--- a/{}", file.path).as_bytes());
        } else if deletes && line.starts_with(b"+++ /dev/null") {
            out.extend_from_slice(format!("+++ b/{}", file.path).as_bytes());
        } else {
            out.extend_from_slice(line);
        }
        out.push(b'\n');
    }
    for hunk in hunks {
        out.extend_from_slice(&hunk.header());
        out.push(b'\n');
        for line in &hunk.body {
            out.extend_from_slice(line);
            out.push(b'\n');
        }
    }
    out
}

// git ---------------------------------------------------------------------------------------

/// `git diff` of one side, for `files` (every file when empty), as parsed files; the flag says
/// the output was cut.
async fn diff_side(
    root: &Path,
    side: Side,
    files: &[String],
) -> Result<(Vec<FilePatch>, bool), ErrorBody> {
    let mut args = diff_prefix();
    args.extend(["--no-renames", "-U3"].map(OsString::from));
    if side == Side::Staged {
        args.push(OsString::from("--cached"));
    }
    args.push(OsString::from("--"));
    args.extend(files.iter().map(OsString::from));
    let out = run_git_capped(root, &os_refs(&args), DIFF_TIMEOUT, TOTAL_PATCH_CAP).await?;
    if out.code != Some(0) && !out.truncated {
        return Err(invalid_state(format!(
            "git diff failed: {}",
            out.stderr.trim()
        )));
    }
    let mut patch = out.stdout;
    if out.truncated {
        // Whole files only.
        let keep = patch
            .windows(12)
            .rposition(|w| w == b"\ndiff --git ")
            .map_or(0, |at| at + 1);
        patch.truncate(keep);
    }
    Ok((parse_patch(side, &patch), out.truncated))
}

/// The untracked, not ignored files of the checkout (those of `files` when given), each with
/// the one hunk that adds it.
async fn untracked_patches(
    root: &Path,
    files: &[String],
) -> Result<(Vec<FilePatch>, bool), ErrorBody> {
    let mut args = vec![
        "--literal-pathspecs",
        "ls-files",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    ];
    args.extend(files.iter().map(String::as_str));
    let listed = git(root, &args, READ_TIMEOUT).await?;
    if !listed.success {
        return Err(git_failed("ls-files", &listed));
    }
    let names: Vec<String> = listed
        .stdout
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty() && !n.ends_with(b"/"))
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    let mut truncated = names.len() > MAX_FILES;
    let mut patches = Vec::new();
    for name in names.into_iter().take(MAX_FILES) {
        let args: Vec<OsString> = [
            "--no-pager",
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "-U3",
            "--",
            "/dev/null",
        ]
        .iter()
        .map(OsString::from)
        .chain([OsString::from(&name)])
        .collect();
        let out = run_git_capped(root, &os_refs(&args), DIFF_TIMEOUT, FILE_PATCH_CAP).await?;
        if out.truncated {
            truncated = true;
            continue;
        }
        if !matches!(out.code, Some(0 | 1)) {
            continue;
        }
        patches.extend(parse_patch(Side::Unstaged, &out.stdout));
    }
    Ok((patches, truncated))
}

/// Runs `git apply` on `patch` against the index (`cached`) or the working tree, forwards or
/// `reverse`d, or only checks that it would.
async fn apply(
    root: &Path,
    patch: &[u8],
    cached: bool,
    reverse: bool,
    check: bool,
) -> Result<(), ErrorBody> {
    let mut args: Vec<&std::ffi::OsStr> = vec!["apply".as_ref(), "--whitespace=nowarn".as_ref()];
    if cached {
        args.push("--cached".as_ref());
    }
    if reverse {
        args.push("--reverse".as_ref());
    }
    if check {
        args.push("--check".as_ref());
    }
    args.push("-".as_ref());
    let out = run_git_capped_with_input(
        root,
        &args,
        WORKTREE_MERGE_TIMEOUT,
        64 * 1024,
        Some(patch.to_vec()),
    )
    .await?;
    if out.code == Some(0) {
        return Ok(());
    }
    Err(ErrorBody::new(
        ErrorCode::Conflict,
        format!(
            "the change no longer applies, nothing was {}: {}",
            if check { "changed" } else { "completed" },
            out.stderr.trim()
        ),
    ))
}

/// Runs a git command that changes the checkout; `invalid_state` with git's words when it fails.
async fn git_change(root: &Path, args: &[&str], what: &str) -> Result<(), ErrorBody> {
    let out = git(root, args, WORKTREE_MERGE_TIMEOUT).await?;
    if out.success {
        Ok(())
    } else {
        Err(git_failed(what, &out))
    }
}

/// `git --literal-pathspecs <head> -- <paths>` for a change.
async fn git_paths(
    root: &Path,
    head: &[&str],
    paths: &[String],
    what: &str,
) -> Result<(), ErrorBody> {
    let mut args: Vec<&str> = vec!["--literal-pathspecs"];
    args.extend(head);
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    git_change(root, &args, what).await
}

/// The output of a listing `git --literal-pathspecs <head> -- <paths>`.
async fn list_paths(
    root: &Path,
    head: &[&str],
    paths: &[String],
) -> Result<super::GitOutput, ErrorBody> {
    let mut args: Vec<&str> = vec!["--literal-pathspecs"];
    args.extend(head);
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    git(root, &args, READ_TIMEOUT).await
}

fn distinct_paths(refs: &[HunkRef]) -> Vec<String> {
    let mut seen = HashSet::new();
    refs.iter()
        .filter(|r| seen.insert(r.path.as_str()))
        .map(|r| r.path.clone())
        .collect()
}

/// Every path of a request must stay inside the checkout and must not be named twice, as a file
/// and by one of its hunks.
fn validate_selection(paths: &[String], hunks: &[HunkRef]) -> Result<(), ErrorBody> {
    for path in paths.iter().chain(hunks.iter().map(|h| &h.path)) {
        validate_relative(path)?;
    }
    if let Some(both) = hunks.iter().find(|h| paths.contains(&h.path)) {
        return Err(bad_request(format!(
            "{} is named as a file and by one of its hunks",
            both.path
        )));
    }
    Ok(())
}

// The service -------------------------------------------------------------------------------

impl WorktreeService {
    /// `worktree.hunks`.
    pub async fn hunks(&self, req: WorktreeHunksRequest) -> Result<WorktreeHunks, ErrorBody> {
        for file in &req.files {
            validate_relative(file)?;
        }
        let (_, entry) = self.checkout(&req.path).await?;
        let root = entry.path.as_path();
        let (unstaged, staged, untracked) = tokio::join!(
            diff_side(root, Side::Unstaged, &req.files),
            diff_side(root, Side::Staged, &req.files),
            untracked_patches(root, &req.files),
        );
        let (unstaged, staged, untracked) = (unstaged?, staged?, untracked?);
        let truncated = unstaged.1 || staged.1 || untracked.1;
        let mut by_path: BTreeMap<String, FileHunks> = BTreeMap::new();
        let sides = [
            (
                false,
                unstaged
                    .0
                    .into_iter()
                    .chain(untracked.0)
                    .collect::<Vec<_>>(),
            ),
            (true, staged.0),
        ];
        for (staged_side, patches) in sides {
            for file in patches {
                let found = by_path
                    .entry(file.path.clone())
                    .or_insert_with(|| FileHunks {
                        path: file.path.clone(),
                        binary: false,
                        unstaged: Vec::new(),
                        staged: Vec::new(),
                    });
                found.binary |= file.binary;
                let hunks = file.hunks.iter().map(Hunk::info);
                if staged_side {
                    found.staged.extend(hunks);
                } else {
                    found.unstaged.extend(hunks);
                }
            }
        }
        let mut files: Vec<FileHunks> = by_path
            .into_values()
            .filter(|f| f.binary || !f.unstaged.is_empty() || !f.staged.is_empty())
            .collect();
        let cut = files.len() > MAX_FILES;
        files.truncate(MAX_FILES);
        Ok(WorktreeHunks {
            files,
            truncated: truncated || cut,
        })
    }

    /// `worktree.stage` (`stage`) and `worktree.unstage`.
    pub async fn change_index(
        &self,
        req: WorktreeIndexChange,
        stage: bool,
    ) -> Result<WorktreeIndexed, ErrorBody> {
        if req.paths.is_empty() && req.hunks.is_empty() && !req.all {
            return Err(bad_request("name files, hunks or `all`"));
        }
        if req.all && (!req.paths.is_empty() || !req.hunks.is_empty()) {
            return Err(bad_request("`all` cannot be combined with files or hunks"));
        }
        validate_selection(&req.paths, &req.hunks)?;
        let _writing = self.writes.lock().await;
        let (main, entry) = self.checkout(&req.checkout).await?;
        let root = entry.path.as_path();
        if let Some(operation) = operation_in_progress(root).await? {
            return Err(invalid_state(format!(
                "{operation} is in progress in {}; finish or abort it first",
                root.display()
            )));
        }
        // Everything is looked up and checked before the first change.
        let patch = if req.hunks.is_empty() {
            None
        } else {
            let files = distinct_paths(&req.hunks);
            let patch = if stage {
                let (mut found, _) = diff_side(root, Side::Unstaged, &files).await?;
                found.extend(untracked_patches(root, &files).await?.0);
                pick_patch(&found, &req.hunks, Direction::Forward, "unstaged")?
            } else {
                let (found, _) = diff_side(root, Side::Staged, &files).await?;
                pick_patch(&found, &req.hunks, Direction::Reverse, "staged")?
            };
            apply(root, &patch, true, !stage, true).await?;
            Some(patch)
        };
        if req.all {
            if stage {
                git_change(root, &["add", "-A"], "add").await?;
            } else {
                git_change(root, &["reset", "-q"], "reset").await?;
            }
        } else if !req.paths.is_empty() {
            let head: &[&str] = if stage {
                &["add", "-A"]
            } else {
                &["reset", "-q"]
            };
            git_paths(root, head, &req.paths, if stage { "add" } else { "reset" }).await?;
        }
        if let Some(patch) = patch {
            apply(root, &patch, true, !stage, false).await?;
        }
        self.index_changed(&main).await;
        Ok(WorktreeIndexed {
            paths: u32::try_from(req.paths.len()).unwrap_or(u32::MAX),
            hunks: u32::try_from(req.hunks.len()).unwrap_or(u32::MAX),
        })
    }

    async fn index_changed(&self, main: &Path) {
        if let Ok(listed) = self.registry.list_workspaces().await {
            self.changed(&workspaces_of_repo(&listed.workspaces, main).await);
        }
    }

    /// `worktree.discard`.
    pub async fn discard(
        &self,
        req: WorktreeDiscard,
        ctx: RestoreContext,
    ) -> Result<WorktreeDiscarded, ErrorBody> {
        if req.paths.is_empty() && req.hunks.is_empty() {
            return Err(bad_request("name the files or hunks to discard"));
        }
        validate_selection(&req.paths, &req.hunks)?;
        let _writing = self.writes.lock().await;
        let (main, entry) = self.checkout(&req.checkout).await?;
        let root = entry.path.clone();
        if let Some(operation) = operation_in_progress(&root).await? {
            return Err(invalid_state(format!(
                "{operation} is in progress in {}; finish or abort it first",
                root.display()
            )));
        }
        // Looked up and checked before the checkpoint and the first change.
        let patch = if req.hunks.is_empty() {
            None
        } else {
            let files = distinct_paths(&req.hunks);
            let (mut found, _) = diff_side(&root, Side::Unstaged, &files).await?;
            found.extend(untracked_patches(&root, &files).await?.0);
            let patch = pick_patch(&found, &req.hunks, Direction::Reverse, "unstaged")?;
            apply(&root, &patch, false, true, true).await?;
            Some(patch)
        };
        let plan = plan_paths(&root, &req.paths, req.staged).await?;
        let touched: Vec<String> = req
            .hunks
            .iter()
            .map(|h| h.path.clone())
            .chain(plan.touched())
            .collect();

        let ((), checkpoint) = self
            .checkpoints
            .guarded_change(&root, &ctx, "discard", |id| {
                let (root, plan, announce) = (root.clone(), plan, ctx.announce.clone());
                async move {
                    for path in &touched {
                        announce(id, &root.join(path));
                    }
                    if let Some(patch) = patch {
                        apply(&root, &patch, false, true, false).await?;
                    }
                    plan.run(&root).await
                }
            })
            .await?;
        self.index_changed(&main).await;
        Ok(WorktreeDiscarded {
            paths: u32::try_from(req.paths.len()).unwrap_or(u32::MAX),
            hunks: u32::try_from(req.hunks.len()).unwrap_or(u32::MAX),
            checkpoint,
        })
    }
}

/// What discarding whole files comes to, decided before anything is written.
#[derive(Debug, Default)]
struct PathPlan {
    /// Tracked: the working tree is made equal to the index.
    restore_worktree: Vec<String>,
    /// Tracked in `HEAD` and `staged`: file and index are made equal to `HEAD`.
    restore_head: Vec<String>,
    /// In the index only (new): removed from index and working tree.
    remove: Vec<String>,
    /// Not tracked by git: deleted.
    untracked: Vec<String>,
}

impl PathPlan {
    fn touched(&self) -> Vec<String> {
        self.restore_worktree
            .iter()
            .chain(&self.restore_head)
            .chain(&self.remove)
            .chain(&self.untracked)
            .cloned()
            .collect()
    }

    async fn run(self, root: &Path) -> Result<(), ErrorBody> {
        if !self.restore_worktree.is_empty() {
            git_paths(
                root,
                &["restore", "--worktree"],
                &self.restore_worktree,
                "restore",
            )
            .await?;
        }
        if !self.restore_head.is_empty() {
            git_paths(
                root,
                &["restore", "--source=HEAD", "--staged", "--worktree"],
                &self.restore_head,
                "restore",
            )
            .await?;
        }
        if !self.remove.is_empty() {
            git_paths(root, &["rm", "-r", "-f", "-q"], &self.remove, "rm").await?;
        }
        if !self.untracked.is_empty() {
            let root = root.to_path_buf();
            let names = self.untracked;
            tokio::task::spawn_blocking(move || {
                for name in names {
                    let path = root.join(&name);
                    match fs::symlink_metadata(&path) {
                        Ok(meta) if meta.is_dir() => {}
                        Ok(_) => fs::remove_file(&path).map_err(|e| {
                            invalid_state(format!("cannot delete {}: {e}", path.display()))
                        })?,
                        Err(_) => {}
                    }
                }
                Ok::<(), ErrorBody>(())
            })
            .await
            .map_err(super::join_failed)??;
        }
        Ok(())
    }
}

/// Sorts the files to discard by what git knows of them; `not_found` for one it knows nothing
/// of, before anything changes.
async fn plan_paths(root: &Path, paths: &[String], staged: bool) -> Result<PathPlan, ErrorBody> {
    let mut plan = PathPlan::default();
    if paths.is_empty() {
        return Ok(plan);
    }
    let names_of = |stdout: &[u8]| -> Vec<String> {
        stdout
            .split(|b| *b == 0)
            .filter(|n| !n.is_empty())
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect()
    };
    let (indexed, others, in_head) = tokio::join!(
        list_paths(root, &["ls-files", "-z"], paths),
        list_paths(
            root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            paths
        ),
        list_paths(root, &["ls-tree", "-r", "--name-only", "-z", "HEAD"], paths),
    );
    let (indexed, others) = (indexed?, others?);
    if !indexed.success {
        return Err(git_failed("ls-files", &indexed));
    }
    if !others.success {
        return Err(git_failed("ls-files", &others));
    }
    let (indexed, others) = (names_of(&indexed.stdout), names_of(&others.stdout));
    // An unborn `HEAD` has nothing.
    let in_head = match in_head {
        Ok(listed) if listed.success => names_of(&listed.stdout),
        _ => Vec::new(),
    };
    let under = |names: &[String], path: &str| -> Vec<String> {
        let dir = format!("{}/", path.trim_end_matches('/'));
        names
            .iter()
            .filter(|n| n.as_str() == path || n.starts_with(&dir))
            .cloned()
            .collect()
    };
    for path in paths {
        let (tracked, loose, head) = (
            under(&indexed, path),
            under(&others, path),
            under(&in_head, path),
        );
        if tracked.is_empty() && loose.is_empty() && head.is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("{path} is not a file git knows, and not an untracked one"),
            ));
        }
        plan.untracked.extend(loose);
        if staged {
            if !head.is_empty() {
                plan.restore_head.push(path.clone());
            } else if !tracked.is_empty() {
                plan.remove.push(path.clone());
            }
        } else if !tracked.is_empty() {
            plan.restore_worktree.push(path.clone());
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &[u8] = b"diff --git a/src/a.txt b/src/a.txt\n\
index 1111111..2222222 100644\n\
--- a/src/a.txt\n\
+++ b/src/a.txt\n\
@@ -1,3 +1,4 @@ head\n one\n+inserted\n two\n three\n\
@@ -10,3 +11,3 @@\n ten\n-old\n+new\n twelve\n\
diff --git a/pic.png b/pic.png\n\
index 3333333..4444444 100644\n\
Binary files a/pic.png and b/pic.png differ\n";

    #[test]
    fn a_patch_is_split_into_files_and_hunks_with_stable_ids() {
        let files = parse_patch(Side::Unstaged, PATCH);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/a.txt");
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[0].old_lines, 3);
        assert_eq!(files[0].hunks[0].new_lines, 4);
        assert_eq!(files[0].hunks[0].section, b" head");
        assert_eq!(files[0].hunks[1].body.len(), 4);
        assert!(files[1].binary);
        assert!(files[1].hunks.is_empty());
        let again = parse_patch(Side::Unstaged, PATCH);
        assert_eq!(files[0].hunks[0].id, again[0].hunks[0].id);
        assert_ne!(files[0].hunks[0].id, files[0].hunks[1].id);
        assert_ne!(
            files[0].hunks[0].id,
            parse_patch(Side::Staged, PATCH)[0].hunks[0].id
        );
        assert_eq!(files[0].hunks[0].id.len(), 16);
    }

    #[test]
    fn names_with_spaces_and_quotes_are_read_from_the_header() {
        assert_eq!(
            header_path(b"a/with space.txt b/with space.txt"),
            "with space.txt"
        );
        assert_eq!(
            header_path(br#""a/caf\303\251\t.txt" "b/caf\303\251\t.txt""#),
            "caf\u{e9}\t.txt"
        );
    }

    #[test]
    fn lines_limit_a_hunk_either_way() {
        let files = parse_patch(Side::Unstaged, PATCH);
        let hunk = &files[0].hunks[1];
        // Body: " ten", "-old", "+new", " twelve"; take only the added line.
        let forward = restrict(hunk, &[2], Direction::Forward).unwrap();
        assert_eq!(
            forward.body,
            vec![
                b" ten".to_vec(),
                b" old".to_vec(),
                b"+new".to_vec(),
                b" twelve".to_vec()
            ]
        );
        assert_eq!((forward.old_lines, forward.new_lines), (3, 4));
        let reverse = restrict(hunk, &[1], Direction::Reverse).unwrap();
        assert_eq!(
            reverse.body,
            vec![
                b" ten".to_vec(),
                b"-old".to_vec(),
                b" new".to_vec(),
                b" twelve".to_vec()
            ]
        );
        assert!(restrict(hunk, &[0], Direction::Forward).is_err());
        assert!(restrict(hunk, &[9], Direction::Forward).is_err());
    }

    #[test]
    fn a_missing_hunk_is_a_conflict_and_a_binary_file_a_bad_request() {
        let files = parse_patch(Side::Unstaged, PATCH);
        let gone = HunkRef {
            path: "src/a.txt".into(),
            hunk_id: "0000000000000000".into(),
            lines: vec![],
        };
        let error = pick_patch(&files, &[gone], Direction::Forward, "unstaged").unwrap_err();
        assert_eq!(error.code, ErrorCode::Conflict);
        let binary = HunkRef {
            path: "pic.png".into(),
            hunk_id: "0000000000000000".into(),
            lines: vec![],
        };
        let error = pick_patch(&files, &[binary], Direction::Forward, "unstaged").unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
    }
}
