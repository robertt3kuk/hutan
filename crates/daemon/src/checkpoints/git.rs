//! The git runner of checkpoints: every call works on Hutan's private index of a checkout and
//! never on the user's.
//!
//! Environment of every call (ADR 0019 Decision 4): the caller's `GIT_*` location and config
//! variables removed, `GIT_INDEX_FILE` set to the private index (when there is one),
//! `GIT_OPTIONAL_LOCKS=0`, `GIT_NO_LAZY_FETCH=1` (a partial clone never fetches),
//! `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`, stdin closed unless the step feeds it, a fixed identity;
//! configuration overridden so no hook, fsmonitor, signing, reflog, automatic gc or split index
//! runs or is written. The user's global and repository configuration is otherwise read (the
//! global `core.excludesFile` decides what is ignored).
//!
//! Reads (`rev-parse`, `ls-files`, `diff-files`, `check-attr`, `config`) are killed when they
//! outlive the read timeout. Writes (`hash-object -w`, `update-index`, `read-tree`, `write-tree`,
//! `commit-tree`, `update-ref`) have no timeout and are never killed: a git write cancelled in
//! flight is what left Codex users with hundreds of GiB of `tmp_pack_*` (#19588, #50920).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use super::CheckpointError;

/// Configuration every call overrides, whatever the user's or the repository's says.
const OVERRIDES: &[&str] = &[
    // A `reference-transaction` hook runs on `update-ref` (measured, spike §3.2).
    "core.hooksPath=/dev/null",
    "core.fsmonitor=false",
    "core.untrackedCache=false",
    "core.splitIndex=false",
    // No reflog for `refs/hutan/*`, even with `core.logAllRefUpdates=always`.
    "core.logAllRefUpdates=false",
    "commit.gpgSign=false",
    "gc.auto=0",
    "maintenance.auto=false",
    "core.autocrlf=false",
    "core.safecrlf=false",
];

/// The identity of checkpoint commits.
const NAME: &str = "Hutan";
const EMAIL: &str = "hutan@localhost";

/// The caller's variables that would point git at another repository, index or configuration.
const REMOVED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_ATTR_SOURCE",
    "GIT_EXTERNAL_DIFF",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
    "GIT_TRACE",
];

/// What a git process printed.
#[derive(Debug)]
pub(crate) struct Output {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub success: bool,
}

impl Output {
    /// git's own words: its error output, else its output.
    pub fn message(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            String::from_utf8_lossy(&self.stdout).into_owned()
        } else {
            self.stderr.clone()
        };
        text.trim().to_owned()
    }

    /// `stdout` as one trimmed line (an object id, a path).
    pub fn line(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_owned()
    }
}

/// Runs git in one checkout, on one index.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    cwd: PathBuf,
    /// The private index; `None` reads the user's index (read-only commands only).
    index: Option<PathBuf>,
    /// Read worktree files as raw bytes: attributes come from the empty tree and no attributes
    /// file, so `update-index --refresh` and `diff-files` compare raw content with the raw
    /// blobs of the private index and run no filter driver.
    raw: Option<String>,
    env: BTreeMap<String, String>,
    read_timeout: Duration,
}

impl Git {
    pub fn new(cwd: &Path, env: &BTreeMap<String, String>, read_timeout: Duration) -> Self {
        Self {
            cwd: cwd.to_owned(),
            index: None,
            raw: None,
            env: env.clone(),
            read_timeout,
        }
    }

    /// The same runner on `index`.
    pub fn on_index(&self, index: &Path) -> Self {
        Self {
            index: Some(index.to_owned()),
            ..self.clone()
        }
    }

    /// The same runner with attributes switched off; `empty_tree` is the empty tree's id in
    /// the repository's object format.
    pub fn raw(&self, empty_tree: &str) -> Self {
        Self {
            raw: Some(empty_tree.to_owned()),
            ..self.clone()
        }
    }

    fn command(&self, args: &[&OsStr], stdin: bool, kill_on_drop: bool) -> Command {
        let mut command = Command::new("git");
        for (key, value) in &self.env {
            command.env(key, value);
        }
        for key in REMOVED {
            command.env_remove(key);
        }
        for setting in OVERRIDES {
            command.arg("-c").arg(setting);
        }
        if let Some(empty_tree) = &self.raw {
            command
                .arg(format!("--attr-source={empty_tree}"))
                .arg("-c")
                .arg("core.attributesFile=/dev/null")
                .env("GIT_ATTR_NOSYSTEM", "1");
        }
        if let Some(index) = &self.index {
            command.env("GIT_INDEX_FILE", index);
        }
        command
            .args(args)
            .current_dir(&self.cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_ASKPASS", "true")
            .env("SSH_ASKPASS", "true")
            .env("GIT_EDITOR", "true")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", NAME)
            .env("GIT_AUTHOR_EMAIL", EMAIL)
            .env("GIT_COMMITTER_NAME", NAME)
            .env("GIT_COMMITTER_EMAIL", EMAIL)
            .stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(kill_on_drop);
        command
    }

    /// A read: killed when it outlives the read timeout.
    pub async fn read(
        &self,
        args: &[&OsStr],
        input: Option<Vec<u8>>,
    ) -> Result<Output, CheckpointError> {
        let step = step_name(args);
        let limit = self.read_timeout;
        let command = self.command(args, input.is_some(), true);
        match tokio::time::timeout(limit, run(command, input, &step)).await {
            Ok(result) => result,
            Err(_) => Err(CheckpointError::TimedOut { step, after: limit }),
        }
    }

    /// A write: runs to its end, whatever happens to the caller.
    pub async fn write(
        &self,
        args: &[&OsStr],
        input: Option<Vec<u8>>,
    ) -> Result<Output, CheckpointError> {
        let command = self.command(args, input.is_some(), false);
        run(command, input, &step_name(args)).await
    }

    /// [`Git::read`] that must succeed.
    pub async fn read_ok(&self, args: &[&str]) -> Result<Output, CheckpointError> {
        let args = os_args(args);
        let output = self.read(&args, None).await?;
        ensure(&args, output)
    }

    /// [`Git::read`] with `input` on stdin that must succeed.
    pub async fn read_ok_with(
        &self,
        args: &[&str],
        input: Vec<u8>,
    ) -> Result<Output, CheckpointError> {
        let args = os_args(args);
        let output = self.read(&args, Some(input)).await?;
        ensure(&args, output)
    }

    /// [`Git::write`] that must succeed.
    pub async fn write_ok(
        &self,
        args: &[&OsStr],
        input: Option<Vec<u8>>,
    ) -> Result<Output, CheckpointError> {
        let output = self.write(args, input).await?;
        ensure(args, output)
    }
}

pub(crate) fn os_args<'a>(args: &[&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(|arg| OsStr::new(*arg)).collect()
}

fn step_name(args: &[&OsStr]) -> String {
    args.first()
        .map_or_else(String::new, |a| a.to_string_lossy().into_owned())
}

fn ensure(args: &[&OsStr], output: Output) -> Result<Output, CheckpointError> {
    if output.success {
        Ok(output)
    } else {
        Err(CheckpointError::GitFailed {
            step: step_name(args),
            message: output.message(),
        })
    }
}

async fn run(
    mut command: Command,
    input: Option<Vec<u8>>,
    step: &str,
) -> Result<Output, CheckpointError> {
    let started = std::time::Instant::now();
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CheckpointError::GitMissing
        } else {
            CheckpointError::Io {
                what: "start git".to_owned(),
                source: error,
            }
        }
    })?;
    // Feed stdin beside reading stdout so neither pipe can fill up and stall the other.
    let feeder = match (input, child.stdin.take()) {
        (Some(bytes), Some(mut stdin)) => Some(tokio::spawn(async move {
            let written = stdin.write_all(&bytes).await;
            drop(stdin);
            written
        })),
        _ => None,
    };
    let output = child
        .wait_with_output()
        .await
        .map_err(|source| CheckpointError::Io {
            what: "wait for git".to_owned(),
            source,
        })?;
    if let Some(feeder) = feeder {
        // A broken pipe means git exited early; its exit status and stderr say why.
        let _ = feeder.await;
    }
    tracing::debug!(
        step,
        ms = started.elapsed().as_secs_f64() * 1e3,
        ok = output.status.success(),
        "checkpoint git"
    );
    Ok(Output {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
}

/// Splits NUL-terminated output into its records.
pub(crate) fn nul_records(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == 0).filter(|r| !r.is_empty())
}

/// One record of `diff-files --raw -z`: the status letter and the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffEntry {
    pub status: u8,
    pub path: Vec<u8>,
}

/// Parses `diff-files --raw -z --no-renames`: `:<mode> <mode> <oid> <oid> <status>\0<path>\0`.
pub(crate) fn parse_diff_files(bytes: &[u8]) -> Vec<DiffEntry> {
    let mut entries = Vec::new();
    let mut records = bytes.split(|b| *b == 0);
    while let Some(head) = records.next() {
        if head.is_empty() {
            continue;
        }
        let Some(path) = records.next() else { break };
        let status = head
            .rsplit(|b| *b == b' ')
            .next()
            .and_then(|s| s.first().copied())
            .unwrap_or(b'M');
        entries.push(DiffEntry {
            status,
            path: path.to_vec(),
        });
    }
    entries
}

/// One entry of `ls-files -s -z`: mode, object id, stage, path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexEntry {
    pub mode: String,
    pub oid: String,
    pub path: Vec<u8>,
}

/// Parses `ls-files -s -z`: `<mode> <oid> <stage>\t<path>\0`.
pub(crate) fn parse_staged(bytes: &[u8]) -> Vec<IndexEntry> {
    nul_records(bytes)
        .filter_map(|record| {
            let tab = record.iter().position(|b| *b == b'\t')?;
            let head = std::str::from_utf8(&record[..tab]).ok()?;
            let mut fields = head.split(' ');
            let mode = fields.next()?.to_owned();
            let oid = fields.next()?.to_owned();
            Some(IndexEntry {
                mode,
                oid,
                path: record[tab + 1..].to_vec(),
            })
        })
        .collect()
}

/// Parses `check-attr -z`: `<path>\0<attribute>\0<value>\0` triples.
pub(crate) fn parse_check_attr(bytes: &[u8]) -> Vec<(Vec<u8>, String, String)> {
    let mut triples = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);
    while let (Some(path), Some(attr), Some(value)) = (fields.next(), fields.next(), fields.next())
    {
        triples.push((
            path.to_vec(),
            String::from_utf8_lossy(attr).into_owned(),
            String::from_utf8_lossy(value).into_owned(),
        ));
    }
    triples
}

/// Path bytes as an `OsString` (paths are bytes on Unix).
pub(crate) fn os(path: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    OsStr::from_bytes(path).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_files_records_keep_status_and_odd_paths() {
        let out = b":100644 100644 aaaa 0000 M\0a b.txt\0:100644 000000 bbbb 0000 D\0dir/\xffx\0:100644 120000 cccc 0000 T\0l\0";
        let entries = parse_diff_files(out);
        assert_eq!(
            entries,
            vec![
                DiffEntry {
                    status: b'M',
                    path: b"a b.txt".to_vec()
                },
                DiffEntry {
                    status: b'D',
                    path: b"dir/\xffx".to_vec()
                },
                DiffEntry {
                    status: b'T',
                    path: b"l".to_vec()
                },
            ]
        );
        assert!(parse_diff_files(b"").is_empty());
    }

    #[test]
    fn staged_entries_and_attribute_triples_parse() {
        let staged = parse_staged(b"160000 abcd 0\tsub\x00100644 ef01 0\tdir/with\ttab\0");
        assert_eq!(staged.len(), 2);
        assert_eq!(staged[0].mode, "160000");
        assert_eq!(staged[1].path, b"dir/with\ttab".to_vec());
        let triples = parse_check_attr(b"x.bin\0filter\0lfs\0a\0text\0unspecified\0");
        assert_eq!(
            triples,
            vec![
                (b"x.bin".to_vec(), "filter".to_owned(), "lfs".to_owned()),
                (b"a".to_vec(), "text".to_owned(), "unspecified".to_owned()),
            ]
        );
    }
}
