//! Git history (protocol 0.47, capability `git.history`): `git.log`, `git.blame`, `git.show` and
//! `git.file`.
//!
//! Read-only, like the diffs of [`super::review`]: every call is a `git --no-pager
//! --literal-pathspecs` command without optional locks and under a timeout, with no external
//! diff or text conversion started by a repository's attributes, and its output is read only up
//! to a bound (git is killed beyond it). The calls hold no lock, and dropping the future (the
//! connection closed) kills the git process.
//!
//! A request names a place by an absolute path; the innermost checkout git lists for a
//! repository of a workspace that holds it is the one that answers (so a file in a linked
//! worktree is read in that worktree), and the answer's paths are relative to its top level.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::git::{
    BlameCommit, BlameHunk, GIT_BLAME_MAX_CONTENTS, GIT_BLAME_MAX_HUNKS, GIT_FILE_MAX_BYTES,
    GIT_LOG_MAX_LIMIT, GIT_MESSAGE_MAX_BYTES, GIT_SHOW_MAX_FILES, GIT_SHOW_PATCH_CAP,
    GIT_SUBJECT_MAX_CHARS, GIT_UNCOMMITTED, GitBlame, GitBlameResult, GitCommit, GitFile,
    GitFileResult, GitLog, GitLogResult, GitShow, GitShowResult,
};
use hutan_protocol::worktree::DiffFile;

use super::review::{
    git, parse_name_status, parse_numstat, run_git_capped, run_git_capped_with_input,
};
use super::{
    READ_TIMEOUT, RawWorktree, WorktreeService, bad_request, git_failed, invalid_state, run_git,
};

/// A history query is a walk over objects: slower than a status, well within a diff.
const HISTORY_TIMEOUT: Duration = Duration::from_secs(30);
/// `git log` output is read up to this many bytes when a page is not the first (about 40 000
/// commits of history): `before` is looked for in it.
const LOG_SCAN_CAP: usize = 8 * 1024 * 1024;
/// The first page of a log never needs more than 200 commits and their subjects.
const LOG_PAGE_CAP: usize = 8 * 1024 * 1024;
/// `git blame --porcelain` output (it repeats every line's text) is read up to this.
const BLAME_CAP: usize = 24 * 1024 * 1024;
/// git's own test for a binary file: a NUL in the first bytes.
const BINARY_PROBE: usize = 8000;

const LOG_FORMAT: &str = "--format=%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%P%x1f%s";
const SHOW_FORMAT: &str = "--format=%H%x1f%an%x1f%ae%x1f%at%x1f%P%x1f%s%x1f%B";

/// Where a request points: the checkout that holds it and the path inside that.
#[derive(Debug)]
struct Target {
    entry: RawWorktree,
    /// The absolute path as asked, resolved through symlinks as far as it exists.
    absolute: PathBuf,
    /// Relative to the checkout's top level; `None` for the top level itself.
    rel: Option<String>,
}

impl Target {
    fn root(&self) -> &Path {
        &self.entry.path
    }
}

impl WorktreeService {
    /// `git.log`.
    pub async fn git_log(&self, req: GitLog) -> Result<GitLogResult, ErrorBody> {
        if req.limit == 0 {
            return Err(bad_request("`limit` must be at least 1"));
        }
        let limit = (req.limit.min(GIT_LOG_MAX_LIMIT)) as usize;
        if let Some(before) = &req.before {
            validate_object_name(before)?;
        }
        let target = self.locate(&req.path).await?;
        let root = target.root().to_path_buf();
        let tip = match req.rev.as_deref() {
            Some(rev) => resolve_commit(&root, rev).await?,
            None => match resolve_commit(&root, "HEAD").await {
                Ok(id) => id,
                // A repository with no commit yet has no history.
                Err(_) => {
                    return Ok(GitLogResult {
                        root,
                        commits: Vec::new(),
                        more: false,
                        next_before: None,
                    });
                }
            },
        };
        let follow = target.rel.is_some() && target.absolute.is_file();
        let mut args = history_prefix();
        args.extend(strings(&["log", "--no-color", "--no-show-signature"]));
        args.extend(strings(&["--no-ext-diff", "--no-textconv", LOG_FORMAT]));
        if follow {
            args.extend(strings(&["--follow", "--name-status", "-M"]));
        }
        let cap = if req.before.is_some() {
            LOG_SCAN_CAP
        } else {
            args.push(OsString::from(format!("--max-count={}", limit + 1)));
            LOG_PAGE_CAP
        };
        args.push(OsString::from(&tip));
        args.push(OsString::from("--"));
        args.extend(target.rel.iter().map(OsString::from));
        let run = run_git_capped(&root, &os_refs(&args), HISTORY_TIMEOUT, cap).await?;
        if run.code != Some(0) && !run.truncated {
            return Err(invalid_state(format!(
                "git log failed: {}",
                run.stderr.trim()
            )));
        }
        let mut commits = parse_log(&run.stdout, run.truncated);
        if let Some(before) = &req.before {
            match commits.iter().position(|c| &c.id == before) {
                Some(at) => {
                    commits.drain(..=at);
                }
                None if run.truncated => {
                    return Err(invalid_state(format!(
                        "{before} is farther back in the history than {} MiB of log, which is as far as Hutan pages",
                        LOG_SCAN_CAP / (1024 * 1024)
                    )));
                }
                None => {
                    return Err(ErrorBody::new(
                        ErrorCode::NotFound,
                        format!("{before} is not in the log of {}", req.path.display()),
                    ));
                }
            }
        }
        // Whether git (or the read bound) had more than this page: the next commit exists.
        let more = commits.len() > limit || (run.truncated && commits.len() >= limit);
        commits.truncate(limit);
        let next_before = more.then(|| commits.last().map(|c| c.id.clone())).flatten();
        Ok(GitLogResult {
            root,
            more: more && next_before.is_some(),
            next_before,
            commits,
        })
    }

    /// `git.blame`.
    pub async fn git_blame(&self, req: GitBlame) -> Result<GitBlameResult, ErrorBody> {
        let target = self.locate(&req.path).await?;
        let root = target.root().to_path_buf();
        let Some(rel) = target.rel.clone() else {
            return Err(bad_request("blame needs a file, not the checkout itself"));
        };
        let rev = match req.rev.as_deref() {
            Some(rev) => Some(resolve_commit(&root, rev).await?),
            None => None,
        };
        if req
            .contents
            .as_ref()
            .is_some_and(|text| text.len() > GIT_BLAME_MAX_CONTENTS)
        {
            return Err(bad_request(format!(
                "`contents` is over {} MiB",
                GIT_BLAME_MAX_CONTENTS / (1024 * 1024)
            )));
        }
        let mut args = history_prefix();
        args.extend(strings(&["blame", "--porcelain", "--no-textconv"]));
        if req.contents.is_some() {
            args.extend(strings(&["--contents", "-"]));
        }
        args.extend(rev.iter().map(OsString::from));
        args.push(OsString::from("--"));
        args.push(OsString::from(&rel));
        let run = run_git_capped_with_input(
            &root,
            &os_refs(&args),
            HISTORY_TIMEOUT,
            BLAME_CAP,
            req.contents.map(String::into_bytes),
        )
        .await?;
        if run.code != Some(0) && !run.truncated {
            let message = run.stderr.trim();
            return Err(
                if message.contains("no such path") || message.contains("cannot stat path") {
                    ErrorBody::new(
                        ErrorCode::NotFound,
                        format!(
                            "{rel} is not a tracked file{}",
                            rev.as_deref()
                                .map(|r| format!(" at {r}"))
                                .unwrap_or_default()
                        ),
                    )
                } else {
                    invalid_state(format!("git blame failed: {message}"))
                },
            );
        }
        let parsed = parse_blame(&run.stdout);
        Ok(GitBlameResult {
            root,
            path: rel,
            rev,
            lines: parsed.lines,
            hunks: parsed.hunks,
            commits: parsed.commits,
            truncated: parsed.truncated || run.truncated,
        })
    }

    /// `git.show`.
    pub async fn git_show(&self, req: GitShow) -> Result<GitShowResult, ErrorBody> {
        let target = self.locate(&req.path).await?;
        let root = target.root().to_path_buf();
        let mut limited: Vec<String> = target.rel.iter().cloned().collect();
        if let Some(old) = &req.old_path {
            let old_target = self.locate(old).await?;
            if old_target.root() != root {
                return Err(bad_request(format!(
                    "{} is not in the same checkout as {}",
                    old.display(),
                    req.path.display()
                )));
            }
            limited.extend(old_target.rel);
        }
        let id = resolve_commit(&root, &req.rev).await?;

        let mut args = history_prefix();
        args.extend(strings(&["log", "-1", "--no-color", "--no-show-signature"]));
        args.extend(strings(&["--no-ext-diff", "--no-textconv", SHOW_FORMAT]));
        args.push(OsString::from(&id));
        args.push(OsString::from("--"));
        let described =
            run_git_capped(&root, &os_refs(&args), HISTORY_TIMEOUT, LOG_PAGE_CAP).await?;
        if described.code != Some(0) {
            return Err(invalid_state(format!(
                "git log failed: {}",
                described.stderr.trim()
            )));
        }
        let (commit, parent, message) = parse_described(&described.stdout)
            .ok_or_else(|| invalid_state(format!("git printed nothing for {id}")))?;

        let base = match &parent {
            Some(parent) => parent.clone(),
            None => empty_tree(&root).await?,
        };
        let names_args = show_args(&["--name-status", "-z"], &base, &id, &limited);
        let stats_args = show_args(&["--numstat", "-z"], &base, &id, &limited);
        let patch_args = show_args(&[], &base, &id, &limited);
        let (names_refs, stats_refs, patch_refs) = (
            os_refs(&names_args),
            os_refs(&stats_args),
            os_refs(&patch_args),
        );
        let (names, stats, patch) = tokio::join!(
            run_git(&root, &names_refs, HISTORY_TIMEOUT),
            run_git(&root, &stats_refs, HISTORY_TIMEOUT),
            async {
                if req.patch {
                    run_git_capped(&root, &patch_refs, HISTORY_TIMEOUT, GIT_SHOW_PATCH_CAP)
                        .await
                        .map(Some)
                } else {
                    Ok(None)
                }
            },
        );
        let names = names?;
        if !names.success {
            return Err(git_failed("diff --name-status", &names));
        }
        let stats = stats?;
        if !stats.success {
            return Err(git_failed("diff --numstat", &stats));
        }
        let counts: HashMap<String, _> = parse_numstat(&stats.stdout)
            .into_iter()
            .map(|stat| (stat.path.clone(), stat))
            .collect();
        let mut files: Vec<DiffFile> = parse_name_status(&names.stdout)
            .into_iter()
            .map(|entry| {
                let stat = counts.get(&entry.path);
                DiffFile {
                    additions: stat.map_or(0, |s| s.additions),
                    deletions: stat.map_or(0, |s| s.deletions),
                    binary: stat.is_some_and(|s| s.binary),
                    path: entry.path,
                    old_path: entry.old_path,
                    status: entry.status,
                    committed: true,
                    uncommitted: false,
                    staged: false,
                    unstaged: false,
                }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut truncated = false;
        if files.len() > GIT_SHOW_MAX_FILES {
            files.truncate(GIT_SHOW_MAX_FILES);
            truncated = true;
        }
        let mut text = String::new();
        if let Some(patch) = patch? {
            if patch.code != Some(0) && !patch.truncated {
                return Err(invalid_state(format!(
                    "git diff failed: {}",
                    patch.stderr.trim()
                )));
            }
            let mut bytes = patch.stdout;
            if patch.truncated {
                // Whole lines only.
                let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
                bytes.truncate(keep);
                truncated = true;
            }
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        Ok(GitShowResult {
            root,
            commit,
            message,
            parent,
            files,
            patch: text,
            truncated,
        })
    }

    /// `git.file`.
    pub async fn git_file(&self, req: GitFile) -> Result<GitFileResult, ErrorBody> {
        let target = self.locate(&req.path).await?;
        let root = target.root().to_path_buf();
        let Some(rel) = target.rel.clone() else {
            return Err(bad_request("`path` is the checkout itself, not a file"));
        };
        let id = resolve_commit(&root, &req.rev).await?;
        let spec = format!("{id}:{rel}");
        let sized = git(&root, &["cat-file", "-s", &spec], READ_TIMEOUT).await?;
        let size: u64 = String::from_utf8_lossy(&sized.stdout)
            .trim()
            .parse()
            .ok()
            .filter(|_| sized.success)
            .ok_or_else(|| {
                ErrorBody::new(ErrorCode::NotFound, format!("{rel} is not in {}", req.rev))
            })?;
        let mut result = GitFileResult {
            root: root.clone(),
            path: rel,
            rev: id,
            size,
            content: String::new(),
            binary: false,
            too_large: false,
        };
        if size > GIT_FILE_MAX_BYTES as u64 {
            result.too_large = true;
            return Ok(result);
        }
        let args = ["cat-file", "blob", spec.as_str()].map(OsString::from);
        let run =
            run_git_capped(&root, &os_refs(&args), HISTORY_TIMEOUT, GIT_FILE_MAX_BYTES).await?;
        if run.code != Some(0) {
            return Err(ErrorBody::new(
                ErrorCode::NotFound,
                format!("{} is not a file at {}", result.path, req.rev),
            ));
        }
        if run.stdout.iter().take(BINARY_PROBE).any(|b| *b == 0) {
            result.binary = true;
        } else {
            result.content = String::from_utf8_lossy(&run.stdout).into_owned();
        }
        Ok(result)
    }

    /// The checkout that holds the absolute `path` and where in it `path` is.
    async fn locate(&self, path: &Path) -> Result<Target, ErrorBody> {
        if !path.is_absolute() {
            return Err(bad_request(format!(
                "`{}` is not an absolute path",
                path.display()
            )));
        }
        let wanted = canonical_lenient(path);
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let (_, entry) = self.find_containing(&workspaces, &wanted, path).await?;
        let inside = wanted
            .strip_prefix(&entry.path)
            .or_else(|_| path.strip_prefix(&entry.path))
            .map_err(|_| bad_request(format!("{} is outside its checkout", path.display())))?
            .to_path_buf();
        let rel = relative_name(&inside)?;
        Ok(Target {
            entry,
            absolute: wanted,
            rel,
        })
    }
}

/// `path` with symlinks resolved as far as it exists (a file deleted from disk still has a
/// history), the rest appended unresolved.
fn canonical_lenient(path: &Path) -> PathBuf {
    let mut tail: Vec<&OsStr> = Vec::new();
    let mut at = path;
    loop {
        if let Ok(resolved) = fs::canonicalize(at) {
            let mut full = resolved;
            full.extend(tail.iter().rev());
            return full;
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                at = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// A path inside a checkout as git names it: `/`-separated, plain components; `None` for the
/// top level itself.
fn relative_name(inside: &Path) -> Result<Option<String>, ErrorBody> {
    let mut parts: Vec<&str> = Vec::new();
    for component in inside.components() {
        match component {
            Component::Normal(name) => parts.push(
                name.to_str()
                    .ok_or_else(|| bad_request("a path that is not UTF-8 has no history here"))?,
            ),
            Component::CurDir => {}
            _ => {
                return Err(bad_request(format!(
                    "`{}` is not a path inside the checkout",
                    inside.display()
                )));
            }
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

fn strings(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn os_refs(args: &[OsString]) -> Vec<&OsStr> {
    args.iter().map(OsString::as_os_str).collect()
}

/// What every history command starts with: no pager, paths are literal, no file system
/// monitor, names printed as they are.
fn history_prefix() -> Vec<OsString> {
    strings(&[
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotepath=false",
    ])
}

/// A hash a client got from an earlier answer.
fn validate_object_name(name: &str) -> Result<(), ErrorBody> {
    let hex = name.len() >= 4 && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_hexdigit());
    if hex {
        Ok(())
    } else {
        Err(bad_request(format!("`{name}` is not a commit hash")))
    }
}

/// The commit `rev` names, as a full hash.
async fn resolve_commit(root: &Path, rev: &str) -> Result<String, ErrorBody> {
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') || rev.contains('\n') {
        return Err(bad_request(format!("`{rev}` is not a revision")));
    }
    let spec = format!("{rev}^{{commit}}");
    let resolved = git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
        READ_TIMEOUT,
    )
    .await?;
    let id = String::from_utf8_lossy(&resolved.stdout).trim().to_owned();
    if resolved.success && !id.is_empty() {
        Ok(id)
    } else {
        Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("no commit named `{rev}`"),
        ))
    }
}

/// The hash of the empty tree in this repository's hash algorithm: what a root commit's diff
/// starts from.
async fn empty_tree(root: &Path) -> Result<String, ErrorBody> {
    let hashed = git(
        root,
        &["hash-object", "-t", "tree", "/dev/null"],
        READ_TIMEOUT,
    )
    .await?;
    if !hashed.success {
        return Err(git_failed("hash-object", &hashed));
    }
    Ok(String::from_utf8_lossy(&hashed.stdout).trim().to_owned())
}

/// `git diff base id` with the options of the review's diffs, in `mode`, limited to `files`.
fn show_args(mode: &[&str], base: &str, id: &str, files: &[String]) -> Vec<OsString> {
    let mut args = super::review::diff_prefix();
    args.extend(mode.iter().map(OsString::from));
    args.push(OsString::from(base));
    args.push(OsString::from(id));
    args.push(OsString::from("--"));
    args.extend(files.iter().map(OsString::from));
    args
}

// Parsing ---------------------------------------------------------------------------------------

fn cut_subject(subject: &str) -> String {
    subject.chars().take(GIT_SUBJECT_MAX_CHARS).collect()
}

/// The commit a `%H%x1f%an%x1f%ae%x1f%at%x1f%P%x1f%s` header describes, with its first parent.
fn parse_header(header: &str) -> Option<(GitCommit, Option<String>)> {
    let mut fields = header.splitn(6, '\x1f');
    let (id, author, email, time, parents, subject) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next().unwrap_or(""),
    );
    if id.is_empty() {
        return None;
    }
    let parents: Vec<&str> = parents.split(' ').filter(|p| !p.is_empty()).collect();
    Some((
        GitCommit {
            id: id.to_owned(),
            author: author.to_owned(),
            email: email.to_owned(),
            time: time.parse().unwrap_or(0),
            subject: cut_subject(subject),
            merge: parents.len() > 1,
            path: None,
            old_path: None,
        },
        parents.first().map(|p| (*p).to_owned()),
    ))
}

/// Parses `git log --format=%x1e…` (with `--name-status` for a followed file). When `cut`, the
/// last record may be incomplete and is dropped.
fn parse_log(output: &[u8], cut: bool) -> Vec<GitCommit> {
    let text = String::from_utf8_lossy(output);
    let mut records: Vec<&str> = text.split('\x1e').skip(1).collect();
    if cut {
        records.pop();
    }
    records
        .into_iter()
        .filter_map(|record| {
            let (header, names) = record.split_once('\n').unwrap_or((record, ""));
            let (mut commit, _) = parse_header(header)?;
            for line in names.lines() {
                let mut parts = line.split('\t');
                let (Some(status), Some(first)) = (parts.next(), parts.next()) else {
                    continue;
                };
                let first = unquote(first);
                match (status.chars().next(), parts.next()) {
                    (Some('R' | 'C'), Some(second)) => {
                        commit.old_path = (!status.starts_with('C')).then(|| first.clone());
                        commit.path = Some(unquote(second));
                    }
                    _ => commit.path = Some(first),
                }
            }
            Some(commit)
        })
        .collect()
}

/// Parses `git log -1 --format=%H…%x1f%B`: the commit, its first parent and the whole message.
fn parse_described(output: &[u8]) -> Option<(GitCommit, Option<String>, String)> {
    let text = String::from_utf8_lossy(output);
    let mut fields = text.splitn(7, '\x1f');
    let head: Vec<&str> = fields.by_ref().take(6).collect();
    if head.len() < 6 {
        return None;
    }
    let message = fields.next().unwrap_or("");
    let (commit, parent) = parse_header(&head.join("\x1f"))?;
    let mut end = message.len().min(GIT_MESSAGE_MAX_BYTES);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    Some((commit, parent, message[..end].trim_end().to_owned() + "\n"))
}

/// Undoes git's C-style quoting of a name with a tab, newline, quote or backslash in it.
fn unquote(name: &str) -> String {
    let Some(inner) = name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) else {
        return name.to_owned();
    };
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let digits = bytes[i..]
                    .iter()
                    .take(3)
                    .take_while(|b| (b'0'..=b'7').contains(b))
                    .count();
                let value = bytes[i..i + digits]
                    .iter()
                    .fold(0_u32, |acc, b| acc * 8 + u32::from(b - b'0'));
                out.push(u8::try_from(value & 0xff).unwrap_or(0));
                i += digits - 1;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a porcelain blame says.
#[derive(Debug, Default, PartialEq, Eq)]
struct Blamed {
    lines: u32,
    hunks: Vec<BlameHunk>,
    commits: Vec<BlameCommit>,
    truncated: bool,
}

/// Parses `git blame --porcelain`: per line a header `<hash> <orig> <final> [<group size>]`,
/// the attributes of a commit (only at its first appearance), and the line's text after a tab.
fn parse_blame(output: &[u8]) -> Blamed {
    let mut blamed = Blamed::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    // The line whose attributes and text are being read.
    let mut current: Option<(usize, u32)> = None;
    for raw in output.split(|b| *b == b'\n') {
        if raw.first() == Some(&b'\t') {
            let Some((commit, line)) = current.take() else {
                continue;
            };
            blamed.lines = blamed.lines.max(line);
            let id = &blamed.commits[commit].id;
            match blamed.hunks.last_mut() {
                Some(last) if &last.commit == id && last.line + last.count == line => {
                    last.count += 1;
                }
                _ => {
                    if blamed.hunks.len() >= GIT_BLAME_MAX_HUNKS {
                        blamed.truncated = true;
                        blamed.lines = line - 1;
                        return blamed;
                    }
                    blamed.hunks.push(BlameHunk {
                        line,
                        count: 1,
                        commit: id.clone(),
                    });
                }
            }
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        if let Some((id, rest)) = text.split_once(' ')
            && matches!(id.len(), 40 | 64)
            && id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let mut numbers = rest.split(' ');
            let line = numbers.nth(1).and_then(|n| n.parse::<u32>().ok());
            let Some(line) = line else { continue };
            let at = *index.entry(id.to_owned()).or_insert_with(|| {
                blamed.commits.push(BlameCommit {
                    id: id.to_owned(),
                    author: String::new(),
                    email: String::new(),
                    time: 0,
                    subject: String::new(),
                    path: String::new(),
                    boundary: false,
                });
                blamed.commits.len() - 1
            });
            current = Some((at, line));
            continue;
        }
        let Some((commit, _)) = current else { continue };
        let entry = &mut blamed.commits[commit];
        if let Some(author) = text.strip_prefix("author ") {
            entry.author = author.to_owned();
        } else if let Some(mail) = text.strip_prefix("author-mail ") {
            entry.email = mail.trim_matches(['<', '>']).to_owned();
        } else if let Some(time) = text.strip_prefix("author-time ") {
            entry.time = time.parse().unwrap_or(0);
        } else if let Some(subject) = text.strip_prefix("summary ") {
            entry.subject = cut_subject(subject);
        } else if let Some(path) = text.strip_prefix("filename ") {
            entry.path = path.to_owned();
        } else if text == "boundary" {
            entry.boundary = true;
        }
    }
    for commit in &mut blamed.commits {
        if commit.id == GIT_UNCOMMITTED {
            commit.subject.clear();
        }
    }
    blamed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_names_are_unquoted() {
        assert_eq!(unquote("plain.rs"), "plain.rs");
        assert_eq!(unquote(r#""a\tb\"c\\d""#), "a\tb\"c\\d");
        assert_eq!(unquote(r#""\303\251.txt""#), "é.txt");
    }

    #[test]
    fn a_followed_log_reads_renames_and_drops_a_cut_record() {
        let output = "\x1eaaaa\x1fAda\x1fa@x\x1f100\x1fbbbb\x1frename it\n\nR100\told.rs\tnew.rs\n\
                      \x1ebbbb\x1fAda\x1fa@x\x1f50\x1f\x1fadd it\n\nA\told.rs\n\x1ecccc\x1fAd";
        let commits = parse_log(output.as_bytes(), true);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].path.as_deref(), Some("new.rs"));
        assert_eq!(commits[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(commits[1].path.as_deref(), Some("old.rs"));
        assert_eq!(commits[1].old_path, None);
        assert!(!commits[1].merge);
    }

    #[test]
    fn porcelain_blame_groups_lines_by_commit() {
        let a = "a".repeat(40);
        let z = GIT_UNCOMMITTED;
        let output = format!(
            "{a} 1 1 2\nauthor Ada\nauthor-mail <ada@x>\nauthor-time 100\nsummary first\nboundary\nfilename f.rs\n\tone\n\
             {a} 2 2\n\ttwo\n\
             {z} 3 3 1\nauthor Not Committed Yet\nauthor-mail <not.committed.yet>\nauthor-time 200\nsummary Version of f.rs\nfilename f.rs\n\tthree\n"
        );
        let blamed = parse_blame(output.as_bytes());
        assert_eq!(blamed.lines, 3);
        assert_eq!(blamed.hunks.len(), 2);
        assert_eq!(
            (blamed.hunks[0].line, blamed.hunks[0].count),
            (1, 2),
            "two lines of one commit are one hunk"
        );
        assert_eq!(blamed.hunks[1].commit, z);
        assert_eq!(blamed.commits.len(), 2);
        assert!(blamed.commits[0].boundary);
        assert_eq!(blamed.commits[0].email, "ada@x");
        assert_eq!(
            blamed.commits[1].subject, "",
            "uncommitted lines have no message"
        );
    }
}
