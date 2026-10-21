//! `worktree.diff`, `worktree.file_diff` and `worktree.merge` (protocol 0.21, capability
//! `worktree.review`) end to end: a real daemon on a temporary `HUTAN_HOME`, real client
//! connections, and git repositories the tests create in temporary directories.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create_in, sh, wait_event};
use hutan_client::{Client, ClientError};
use hutan_core::WorkspaceId;
use hutan_protocol::ErrorCode;
use hutan_protocol::messages::{Event, Request, WorkspaceOpen, WorkspaceOpened};
use hutan_protocol::worktree::{
    DiffFile, DiffStatus, MergeStrategy, WorktreeCommit, WorktreeCreate, WorktreeDiff,
    WorktreeDiffRequest, WorktreeFileDiffRequest, WorktreeInfo, WorktreeMerge, WorktreeMerged,
    WorktreeRemove, conflict_files,
};
use tempfile::TempDir;
use tokio::sync::broadcast;

// Git fixtures --------------------------------------------------------------------------------

/// Runs git in `dir` without the user's configuration; returns stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// Content with enough lines for git to see a rename.
fn lines(prefix: &str, count: usize) -> String {
    (0..count).map(|n| format!("{prefix} line {n}\n")).collect()
}

/// `<tmp>/repo` on `main` with a few files; the daemon runs git with the repository's own
/// configuration only, so the identity is set there (worktrees share it).
struct Repo {
    _dir: TempDir,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), lines("a", 10)).unwrap();
        fs::write(root.join("keep.txt"), "keep\n").unwrap();
        fs::write(root.join("gone.txt"), "gone\n").unwrap();
        fs::write(root.join("old name.txt"), lines("moved", 20)).unwrap();
        commit_all(&root, "first");
        Self { _dir: dir, root }
    }

    fn head(&self) -> String {
        git(&self.root, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn status(&self) -> String {
        git(&self.root, &["status", "--porcelain"])
    }

    fn log(&self, rev: &str) -> Vec<String> {
        git(&self.root, &["log", "--format=%s", rev])
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    repo: Repo,
    workspace: WorkspaceId,
}

impl Env {
    async fn start() -> Self {
        let daemon = TestDaemon::start().await;
        let repo = Repo::new();
        let (client, events) = daemon.connect_subscribed().await;
        let workspace = client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: repo.root.clone(),
                created_by: None,
            }))
            .await
            .unwrap()
            .workspace
            .id;
        Self {
            daemon,
            client,
            events,
            repo,
            workspace,
        }
    }

    async fn worktree(&self, branch: &str) -> WorktreeInfo {
        self.client
            .worktree_create(WorktreeCreate {
                workspace_id: self.workspace,
                repo: None,
                branch: branch.to_owned(),
                base: None,
                path: None,
            })
            .await
            .unwrap()
    }

    async fn diff(&self, path: &Path, include_uncommitted: bool) -> WorktreeDiff {
        self.client
            .worktree_diff(WorktreeDiffRequest {
                path: path.to_owned(),
                base: None,
                include_uncommitted,
            })
            .await
            .unwrap()
    }

    async fn merge(
        &self,
        path: &Path,
        strategy: MergeStrategy,
        message: Option<&str>,
    ) -> Result<WorktreeMerged, ClientError> {
        self.merge_into(path, None, strategy, message).await
    }

    async fn merge_into(
        &self,
        path: &Path,
        into: Option<&str>,
        strategy: MergeStrategy,
        message: Option<&str>,
    ) -> Result<WorktreeMerged, ClientError> {
        self.client
            .worktree_merge(WorktreeMerge {
                path: path.to_owned(),
                into: into.map(str::to_owned),
                strategy,
                message: message.map(str::to_owned),
            })
            .await
    }

    async fn file_diff(
        &self,
        path: &Path,
        file: &str,
        old_path: Option<&str>,
    ) -> Result<hutan_protocol::worktree::WorktreeFileDiff, ClientError> {
        self.client
            .worktree_file_diff(WorktreeFileDiffRequest {
                path: path.to_owned(),
                file: file.to_owned(),
                old_path: old_path.map(str::to_owned),
                base: None,
                include_uncommitted: true,
            })
            .await
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

fn remote<T: std::fmt::Debug>(result: Result<T, ClientError>) -> hutan_protocol::ErrorBody {
    match result {
        Err(ClientError::Remote(error)) => error,
        other => panic!("expected a remote error, got {other:?}"),
    }
}

fn file<'a>(diff: &'a WorktreeDiff, path: &str) -> &'a DiffFile {
    diff.files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no `{path}` in {:?}", diff.files))
}

// diff ------------------------------------------------------------------------------------------

/// A worktree whose branch has commits (a change, a rename, a deletion, a binary file) and
/// uncommitted edits on top (a staged file, an unstaged edit, an untracked file).
async fn busy_worktree(env: &Env) -> WorktreeInfo {
    let wt = env.worktree("feature/busy").await;
    let dir = &wt.path;
    fs::write(
        dir.join("a.txt"),
        format!("{}added later\n", lines("a", 10)),
    )
    .unwrap();
    git(dir, &["mv", "old name.txt", "new name.txt"]);
    git(dir, &["rm", "-q", "gone.txt"]);
    fs::write(dir.join("logo.bin"), [0_u8, 1, 2, 0, 255, 0]).unwrap();
    commit_all(dir, "work");

    // Uncommitted: more of a.txt, a staged file, an untracked file, an ignored one.
    fs::write(
        dir.join("a.txt"),
        format!("{}added later\nand more\n", lines("a", 10)),
    )
    .unwrap();
    fs::write(dir.join("staged.txt"), "s1\ns2\n").unwrap();
    git(dir, &["add", "staged.txt"]);
    fs::write(dir.join("untracked.txt"), "u1\nu2\nu3\n").unwrap();
    fs::write(dir.join(".gitignore"), "ignored.log\n").unwrap();
    fs::write(dir.join("ignored.log"), "noise\n").unwrap();
    wt
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_lists_commits_and_uncommitted_changes_against_the_merge_base() {
    let env = Env::start().await;
    let wt = busy_worktree(&env).await;
    // The base moves on after the branch left it: the diff must not show that.
    fs::write(env.repo.root.join("keep.txt"), "keep, changed on main\n").unwrap();
    commit_all(&env.repo.root, "main moves");

    let diff = env.diff(&wt.path, true).await;
    assert_eq!(diff.base, "main");
    assert_eq!(diff.merge_base.len(), 40);
    assert!(!diff.patch_truncated);
    let names: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        names,
        [
            ".gitignore",
            "a.txt",
            "gone.txt",
            "logo.bin",
            "new name.txt",
            "staged.txt",
            "untracked.txt"
        ],
        "sorted by path; no keep.txt (changed on main only), no ignored.log"
    );

    let a = file(&diff, "a.txt");
    assert_eq!(a.status, DiffStatus::Modified);
    assert_eq!(
        (a.additions, a.deletions),
        (2, 0),
        "committed and uncommitted"
    );
    assert_eq!(file(&diff, "gone.txt").status, DiffStatus::Deleted);
    assert_eq!(file(&diff, "gone.txt").deletions, 1);
    let renamed = file(&diff, "new name.txt");
    assert_eq!(renamed.status, DiffStatus::Renamed);
    assert_eq!(renamed.old_path.as_deref(), Some("old name.txt"));
    assert_eq!((renamed.additions, renamed.deletions), (0, 0));
    let binary = file(&diff, "logo.bin");
    assert!(binary.binary);
    assert_eq!(binary.status, DiffStatus::Added);
    assert_eq!((binary.additions, binary.deletions), (0, 0));
    let staged = file(&diff, "staged.txt");
    assert_eq!((staged.status, staged.additions), (DiffStatus::Added, 2));
    let untracked = file(&diff, "untracked.txt");
    assert_eq!(
        (untracked.status, untracked.additions, untracked.binary),
        (DiffStatus::Added, 3, false)
    );

    // What a merge takes and what it does not, file by file (protocol 0.25).
    let scope = |f: &DiffFile| (f.committed, f.uncommitted);
    assert_eq!(scope(file(&diff, "a.txt")), (true, true), "both");
    for committed_only in ["gone.txt", "logo.bin", "new name.txt"] {
        assert_eq!(
            scope(file(&diff, committed_only)),
            (true, false),
            "{committed_only}"
        );
    }
    for uncommitted_only in [".gitignore", "staged.txt", "untracked.txt"] {
        assert_eq!(
            scope(file(&diff, uncommitted_only)),
            (false, true),
            "{uncommitted_only}"
        );
    }
    assert_eq!(diff.commits.len(), 1, "exactly the commits a merge takes");
    assert_eq!(diff.commits[0].subject, "work");
    assert_eq!(diff.commits[0].id.len(), 40);
    assert!(!diff.commits_truncated);

    // Without the uncommitted part: only what the branch committed.
    let committed = env.diff(&wt.path, false).await;
    let names: Vec<&str> = committed.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["a.txt", "gone.txt", "logo.bin", "new name.txt"]);
    assert_eq!(file(&committed, "a.txt").additions, 1);
    assert!(committed.files.iter().all(|f| scope(f) == (true, false)));
    assert_eq!(committed.commits, diff.commits);

    // An explicit base: the first commit of the repository.
    let first = git(&env.repo.root, &["rev-list", "--max-parents=0", "HEAD"])
        .trim()
        .to_owned();
    let by_commit = env
        .client
        .worktree_diff(WorktreeDiffRequest {
            path: wt.path.clone(),
            base: Some(first.clone()),
            include_uncommitted: false,
        })
        .await
        .unwrap();
    assert_eq!(by_commit.base, first);
    assert_eq!(by_commit.files, committed.files);
    env.stop().await;
}

/// Protocol 0.34 (`worktree.index`): A staged, B edited, C staged then edited again, D untracked
/// → A staged, B unstaged, C both, D unstaged; a committed-only file is neither. Read-only:
/// the index is what it was.
#[tokio::test(flavor = "multi_thread")]
async fn staged_and_unstaged_changes_are_told_apart_and_the_index_is_left_alone() {
    let env = Env::start().await;
    let wt = env.worktree("feature/index").await;
    let dir = &wt.path;
    fs::write(dir.join("keep.txt"), "keep, committed\n").unwrap();
    commit_all(dir, "committed only");
    fs::write(dir.join("a.txt"), format!("{}staged\n", lines("a", 10))).unwrap();
    git(dir, &["add", "a.txt"]);
    fs::write(dir.join("gone.txt"), "gone, edited\n").unwrap();
    fs::write(dir.join("old name.txt"), "first\n").unwrap();
    git(dir, &["add", "old name.txt"]);
    fs::write(dir.join("old name.txt"), "first\nthen more\n").unwrap();
    fs::write(dir.join("new.txt"), "new\n").unwrap();
    let status_before = git(dir, &["status", "--porcelain"]);

    assert!(env.client.supports_worktree_index());
    let diff = env.diff(dir, true).await;
    let marks = |path: &str| {
        let f = file(&diff, path);
        (f.staged, f.unstaged, f.uncommitted)
    };
    assert_eq!(marks("a.txt"), (true, false, true), "staged");
    assert_eq!(marks("gone.txt"), (false, true, true), "unstaged");
    assert_eq!(marks("old name.txt"), (true, true, true), "both");
    assert_eq!(
        marks("new.txt"),
        (false, true, true),
        "untracked is unstaged"
    );
    assert_eq!(marks("keep.txt"), (false, false, false), "committed only");
    assert!(file(&diff, "keep.txt").committed);

    // Without uncommitted changes nothing is marked.
    let committed = env.diff(dir, false).await;
    assert!(committed.files.iter().all(|f| !f.staged && !f.unstaged));
    assert_eq!(git(dir, &["status", "--porcelain"]), status_before);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_all_puts_every_uncommitted_change_into_one_commit_so_a_merge_takes_it() {
    let env = Env::start().await;
    let wt = busy_worktree(&env).await;
    assert!(
        !env.diff(&wt.path, true)
            .await
            .files
            .iter()
            .all(|f| f.committed)
    );

    // Refusals first: nothing changes.
    let empty = remote(
        env.client
            .worktree_commit(WorktreeCommit {
                path: wt.path.clone(),
                message: "  ".into(),
            })
            .await,
    );
    assert_eq!(empty.code, ErrorCode::BadRequest);
    let main = remote(
        env.client
            .worktree_commit(WorktreeCommit {
                path: env.repo.root.clone(),
                message: "no".into(),
            })
            .await,
    );
    assert_eq!(main.code, ErrorCode::InvalidState);
    assert!(main.message.contains("main checkout"), "{}", main.message);
    assert!(
        git(&wt.path, &["diff", "--cached", "--name-only"])
            .trim()
            .contains("staged.txt"),
        "the staged file is still the only staged one"
    );

    let committed = env
        .client
        .worktree_commit(WorktreeCommit {
            path: wt.path.clone(),
            message: "Finish the work\n\nmore words".into(),
        })
        .await
        .unwrap();
    assert_eq!(committed.branch, "feature/busy");
    assert_eq!(
        committed.files, 4,
        "a.txt, .gitignore, staged and untracked"
    );
    assert_eq!(
        git(&wt.path, &["rev-parse", "HEAD"]).trim(),
        committed.commit
    );
    assert!(
        !git(&wt.path, &["status", "--porcelain"]).contains("??"),
        "the untracked file was added"
    );
    assert!(
        wt.path.join("ignored.log").is_file(),
        "ignored files stay out"
    );
    assert!(!git(&wt.path, &["ls-files"]).contains("ignored.log"));

    let diff = env.diff(&wt.path, true).await;
    assert!(
        diff.files.iter().all(|f| f.committed && !f.uncommitted),
        "{:?}",
        diff.files
    );
    let subjects: Vec<&str> = diff.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["Finish the work", "work"], "newest first");

    // Nothing left to commit.
    let nothing = remote(
        env.client
            .worktree_commit(WorktreeCommit {
                path: wt.path.clone(),
                message: "again".into(),
            })
            .await,
    );
    assert_eq!(nothing.code, ErrorCode::InvalidState);
    assert!(
        nothing.message.contains("nothing to commit"),
        "{}",
        nothing.message
    );

    // And the merge now takes all of it, with no leftovers.
    let merged = env
        .client
        .worktree_merge(WorktreeMerge {
            path: wt.path.clone(),
            into: None,
            strategy: MergeStrategy::Merge,
            message: None,
        })
        .await
        .unwrap();
    assert!(!merged.source_dirty);
    assert!(env.repo.root.join("untracked.txt").is_file());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn file_diff_returns_the_patch_of_one_file_in_each_state() {
    let env = Env::start().await;
    let wt = busy_worktree(&env).await;

    let modified = env.file_diff(&wt.path, "a.txt", None).await.unwrap();
    assert!(
        modified.patch.starts_with("diff --git a/a.txt b/a.txt\n"),
        "{}",
        modified.patch
    );
    assert!(
        modified.patch.contains("+added later\n+and more\n"),
        "{}",
        modified.patch
    );
    assert!(!modified.truncated && !modified.binary);
    assert!(!modified.patch.contains('\u{1b}'), "no colors");

    let deleted = env.file_diff(&wt.path, "gone.txt", None).await.unwrap();
    assert!(
        deleted.patch.contains("deleted file mode"),
        "{}",
        deleted.patch
    );
    assert!(deleted.patch.contains("-gone\n"), "{}", deleted.patch);

    let renamed = env
        .file_diff(&wt.path, "new name.txt", Some("old name.txt"))
        .await
        .unwrap();
    assert!(
        renamed.patch.contains("rename from old name.txt"),
        "{}",
        renamed.patch
    );
    assert!(
        renamed.patch.contains("rename to new name.txt"),
        "{}",
        renamed.patch
    );

    let binary = env.file_diff(&wt.path, "logo.bin", None).await.unwrap();
    assert!(binary.binary, "{}", binary.patch);
    assert!(binary.patch.contains("Binary files"), "{}", binary.patch);

    let untracked = env
        .file_diff(&wt.path, "untracked.txt", None)
        .await
        .unwrap();
    assert!(
        untracked.patch.contains("new file mode"),
        "{}",
        untracked.patch
    );
    assert!(
        untracked.patch.contains("+u1\n+u2\n+u3\n"),
        "{}",
        untracked.patch
    );
    assert!(
        untracked.patch.contains("--- /dev/null"),
        "{}",
        untracked.patch
    );
    assert!(
        untracked.patch.contains("+++ b/untracked.txt"),
        "{}",
        untracked.patch
    );

    // Untouched files and ignored ones have no patch; uncommitted ones vanish when excluded.
    assert_eq!(
        env.file_diff(&wt.path, "keep.txt", None)
            .await
            .unwrap()
            .patch,
        ""
    );
    assert_eq!(
        env.file_diff(&wt.path, "ignored.log", None)
            .await
            .unwrap()
            .patch,
        ""
    );
    let committed_only = env
        .client
        .worktree_file_diff(WorktreeFileDiffRequest {
            path: wt.path.clone(),
            file: "untracked.txt".into(),
            old_path: None,
            base: None,
            include_uncommitted: false,
        })
        .await
        .unwrap();
    assert_eq!(committed_only.patch, "");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_main_checkout_can_be_reviewed_against_a_base_too() {
    let env = Env::start().await;
    let first = env.repo.head();
    fs::write(env.repo.root.join("a.txt"), "changed\n").unwrap();
    commit_all(&env.repo.root, "second");
    fs::write(env.repo.root.join("new.txt"), "n\n").unwrap();

    // Against itself: only the uncommitted file.
    let own = env.diff(&env.repo.root, true).await;
    assert_eq!(own.files.len(), 1);
    assert_eq!(own.files[0].path, "new.txt");
    // Against its first commit: the commit as well.
    let against_first = env
        .client
        .worktree_diff(WorktreeDiffRequest {
            path: env.repo.root.clone(),
            base: Some(first),
            include_uncommitted: true,
        })
        .await
        .unwrap();
    let names: Vec<&str> = against_first
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(names, ["a.txt", "new.txt"]);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_output_is_bounded_per_file_and_in_total() {
    let env = Env::start().await;
    let wt = env.worktree("feature/big").await;

    // One tracked file over the per-file cap: its patch is cut on a line boundary.
    let big: String = (0..140_000)
        .map(|n| format!("line number {n:07}\n"))
        .collect();
    assert!(big.len() > 2 * 1024 * 1024);
    fs::write(wt.path.join("big.txt"), &big).unwrap();
    commit_all(&wt.path, "big");
    let diff = env.diff(&wt.path, true).await;
    assert!(diff.patch_truncated, "a file over 2 MiB");
    assert_eq!(
        file(&diff, "big.txt").additions,
        140_000,
        "counts stay exact"
    );
    let cut = env.file_diff(&wt.path, "big.txt", None).await.unwrap();
    assert!(cut.truncated);
    assert!(cut.patch.len() <= 2 * 1024 * 1024, "{}", cut.patch.len());
    assert!(cut.patch.len() > 2 * 1024 * 1024 - 1024);
    assert!(cut.patch.ends_with('\n'));
    assert!(cut.patch.starts_with("diff --git a/big.txt"));

    // Many files each under the cap, together over 20 MiB.
    let second = env.worktree("feature/many").await;
    let chunk: String = (0..100_000).map(|n| format!("row {n:07}\n")).collect();
    assert!(chunk.len() < 2 * 1024 * 1024);
    for n in 0..24 {
        fs::write(second.path.join(format!("part-{n:02}.txt")), &chunk).unwrap();
    }
    commit_all(&second.path, "many");
    let many = env.diff(&second.path, true).await;
    assert!(many.patch_truncated, "20 MiB in all");
    assert_eq!(many.files.len(), 24);

    let small = env.worktree("feature/small").await;
    fs::write(small.path.join("small.txt"), "x\n").unwrap();
    assert!(!env.diff(&small.path, true).await.patch_truncated);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_refuses_what_it_cannot_show() {
    let env = Env::start().await;
    let wt = env.worktree("feature/x").await;
    let outside = TempDir::new().unwrap();

    let error = remote(
        env.client
            .worktree_diff(WorktreeDiffRequest {
                path: "relative".into(),
                base: None,
                include_uncommitted: true,
            })
            .await,
    );
    assert_eq!(error.code, ErrorCode::BadRequest);
    let error = remote(
        env.client
            .worktree_diff(WorktreeDiffRequest {
                path: outside.path().to_owned(),
                base: None,
                include_uncommitted: true,
            })
            .await,
    );
    assert_eq!(error.code, ErrorCode::NotFound, "{error:?}");
    for base in ["no-such-branch", "--output=/tmp/x", ""] {
        let error = remote(
            env.client
                .worktree_diff(WorktreeDiffRequest {
                    path: wt.path.clone(),
                    base: Some(base.to_owned()),
                    include_uncommitted: true,
                })
                .await,
        );
        assert_eq!(error.code, ErrorCode::BadRequest, "{base}: {error:?}");
    }
    for name in ["../outside", "/etc/passwd", "", "a/../../b"] {
        let error = remote(env.file_diff(&wt.path, name, None).await);
        assert_eq!(error.code, ErrorCode::BadRequest, "{name:?}: {error:?}");
    }
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_runs_no_external_program_and_leaves_the_repository_alone() {
    let env = Env::start().await;
    let wt = env.worktree("feature/hostile").await;
    // A repository configuration that would run programs on `git diff`: they must not run.
    let outer = env.repo.root.parent().unwrap();
    let marker = outer.join("ran-external-diff");
    let script = outer.join("ext.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(&script)
        .status()
        .unwrap();
    let script_path = script.display().to_string();
    git(&env.repo.root, &["config", "diff.external", &script_path]);
    git(
        &env.repo.root,
        &["config", "diff.hostile.textconv", &script_path],
    );
    fs::write(
        env.repo.root.join(".git/info/attributes"),
        "*.txt diff=hostile\n",
    )
    .unwrap();

    fs::write(wt.path.join("a.txt"), "different\n").unwrap();
    let before = git(&wt.path, &["status", "--porcelain=v2"]);
    let diff = env.diff(&wt.path, true).await;
    assert_eq!(file(&diff, "a.txt").status, DiffStatus::Modified);
    let patch = env.file_diff(&wt.path, "a.txt", None).await.unwrap();
    assert!(patch.patch.contains("+different"), "{}", patch.patch);
    assert!(!marker.exists(), "an external diff or textconv program ran");
    assert_eq!(git(&wt.path, &["status", "--porcelain=v2"]), before);
    env.stop().await;
}

// merge -----------------------------------------------------------------------------------------

/// A worktree on `feature/merge` with two commits touching `feature.txt`; `main` gets its own
/// commit on `main.txt` afterwards (so merges are real merges and a rebase has work to do).
async fn diverged(env: &Env) -> WorktreeInfo {
    let wt = env.worktree("feature/merge").await;
    fs::write(wt.path.join("feature.txt"), "one\n").unwrap();
    commit_all(&wt.path, "feature one");
    fs::write(wt.path.join("feature.txt"), "one\ntwo\n").unwrap();
    commit_all(&wt.path, "feature two");
    fs::write(env.repo.root.join("main.txt"), "main\n").unwrap();
    commit_all(&env.repo.root, "main work");
    wt
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_makes_a_merge_commit_in_the_main_checkout_and_announces_it() {
    let mut env = Env::start().await;
    let wt = diverged(&env).await;
    let before = env.repo.head();

    let merged = env
        .merge(&wt.path, MergeStrategy::Merge, Some("Merge the feature"))
        .await
        .unwrap();
    assert_eq!(merged.target, "main");
    assert_eq!(merged.branch, "feature/merge");
    assert_eq!(merged.strategy, MergeStrategy::Merge);
    assert!(!merged.source_dirty);
    assert_eq!(merged.commit, env.repo.head());
    assert_ne!(merged.commit, before);
    let parents = git(&env.repo.root, &["log", "-1", "--format=%P"]);
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "a merge commit: {parents}"
    );
    assert_eq!(env.repo.log("main")[0], "Merge the feature");
    assert_eq!(
        fs::read_to_string(env.repo.root.join("feature.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert!(env.repo.status().is_empty(), "{}", env.repo.status());
    assert!(wt.path.join("feature.txt").is_file(), "the worktree stays");

    let workspace = env.workspace;
    wait_event(&mut env.events, "worktree.changed", |event| match event {
        Event::WorktreeChanged { workspace_id } if *workspace_id == workspace => Some(()),
        _ => None,
    })
    .await;
    // Merging again: nothing left.
    let error = remote(env.merge(&wt.path, MergeStrategy::Merge, None).await);
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(
        error.message.contains("nothing to merge"),
        "{}",
        error.message
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merge_without_a_message_uses_gits_own_and_reports_uncommitted_leftovers() {
    let env = Env::start().await;
    let wt = diverged(&env).await;
    fs::write(wt.path.join("scratch.txt"), "wip\n").unwrap();
    let merged = env
        .merge(&wt.path, MergeStrategy::Merge, None)
        .await
        .unwrap();
    assert!(
        merged.source_dirty,
        "the untracked file is not part of the merge"
    );
    assert!(
        env.repo.log("main")[0].starts_with("Merge branch 'feature/merge'"),
        "{:?}",
        env.repo.log("main")
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn squash_lands_one_commit_without_the_branch_history() {
    let env = Env::start().await;
    let wt = diverged(&env).await;

    let merged = env
        .merge(&wt.path, MergeStrategy::Squash, Some("Add the feature"))
        .await
        .unwrap();
    assert_eq!(merged.strategy, MergeStrategy::Squash);
    assert_eq!(merged.commit, env.repo.head());
    let log = env.repo.log("main");
    assert_eq!(log[0], "Add the feature");
    assert_eq!(log[1], "main work", "one commit on top: {log:?}");
    assert!(!log.contains(&"feature one".to_owned()), "{log:?}");
    let parents = git(&env.repo.root, &["log", "-1", "--format=%P"]);
    assert_eq!(parents.split_whitespace().count(), 1);
    assert_eq!(
        fs::read_to_string(env.repo.root.join("feature.txt")).unwrap(),
        "one\ntwo\n"
    );
    assert!(env.repo.status().is_empty());

    // The default message names the branch.
    let second = env.worktree("feature/second").await;
    fs::write(second.path.join("second.txt"), "2\n").unwrap();
    commit_all(&second.path, "second work");
    env.merge(&second.path, MergeStrategy::Squash, None)
        .await
        .unwrap();
    assert_eq!(
        env.repo.log("main")[0],
        "Squash merge branch 'feature/second'"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rebase_replays_the_branch_and_fast_forwards_the_target() {
    let env = Env::start().await;
    let wt = diverged(&env).await;

    let merged = env
        .merge(&wt.path, MergeStrategy::Rebase, None)
        .await
        .unwrap();
    assert_eq!(merged.strategy, MergeStrategy::Rebase);
    assert_eq!(merged.commit, env.repo.head());
    let log = env.repo.log("main");
    assert_eq!(
        log[..3],
        ["feature two", "feature one", "main work"],
        "linear history: {log:?}"
    );
    assert_eq!(
        git(&wt.path, &["rev-parse", "HEAD"]).trim(),
        merged.commit,
        "the branch and main are at the same commit"
    );
    assert!(env.repo.status().is_empty());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn merge_refuses_before_changing_anything() {
    let env = Env::start().await;
    let wt = diverged(&env).await;
    let head = env.repo.head();
    let branch_head = git(&wt.path, &["rev-parse", "HEAD"]);

    // The main checkout has uncommitted changes: tracked, and untracked.
    for (what, change) in [("tracked", "a.txt"), ("untracked", "stray.txt")] {
        fs::write(env.repo.root.join(change), "edit\n").unwrap();
        let error = remote(env.merge(&wt.path, MergeStrategy::Merge, None).await);
        assert_eq!(error.code, ErrorCode::InvalidState, "{what}");
        assert!(
            error.message.contains("uncommitted changes"),
            "{what}: {}",
            error.message
        );
        git(&env.repo.root, &["checkout", "-q", "--", "."]);
        let _ = fs::remove_file(env.repo.root.join("stray.txt"));
    }
    assert_eq!(env.repo.head(), head);

    // The target exists but is not what the main checkout has checked out.
    git(&env.repo.root, &["branch", "release"]);
    let error = remote(
        env.merge_into(&wt.path, Some("release"), MergeStrategy::Merge, None)
            .await,
    );
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(
        error
            .message
            .contains("`release` is not checked out in the main checkout")
            && error.message.contains("`main`"),
        "{}",
        error.message
    );
    // ... and one that is checked out in another worktree says where.
    let other = env.worktree("other").await;
    let error = remote(
        env.merge_into(&wt.path, Some("other"), MergeStrategy::Merge, None)
            .await,
    );
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(
        error.message.contains(&other.path.display().to_string()),
        "{}",
        error.message
    );
    // A target that is no branch.
    let error = remote(
        env.merge_into(&wt.path, Some("nope"), MergeStrategy::Merge, None)
            .await,
    );
    assert_eq!(error.code, ErrorCode::NotFound);

    // The main checkout is not a source; a rebase takes no message; bad requests.
    let error = remote(env.merge(&env.repo.root, MergeStrategy::Merge, None).await);
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("main checkout"), "{}", error.message);
    let error = remote(
        env.merge(&wt.path, MergeStrategy::Rebase, Some("msg"))
            .await,
    );
    assert_eq!(error.code, ErrorCode::BadRequest);
    let error = remote(
        env.merge(Path::new("relative"), MergeStrategy::Merge, None)
            .await,
    );
    assert_eq!(error.code, ErrorCode::BadRequest);

    // A rebase needs a clean worktree and no live session in it.
    fs::write(wt.path.join("feature.txt"), "dirty\n").unwrap();
    let error = remote(env.merge(&wt.path, MergeStrategy::Rebase, None).await);
    assert!(
        error.message.contains("uncommitted changes"),
        "{}",
        error.message
    );
    git(&wt.path, &["checkout", "-q", "--", "."]);
    let mut spec = sh("sleep 120");
    spec.cwd = Some(wt.path.clone());
    let _session = create_in(&env.client, Some(env.workspace), spec).await;
    let error = remote(env.merge(&wt.path, MergeStrategy::Rebase, None).await);
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("live sessions"), "{}", error.message);

    // A detached HEAD has no branch to merge.
    let detached = env.worktree("to-detach").await;
    git(&detached.path, &["checkout", "-q", "--detach"]);
    let error = remote(env.merge(&detached.path, MergeStrategy::Merge, None).await);
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(error.message.contains("detached"), "{}", error.message);

    assert_eq!(env.repo.head(), head, "nothing moved");
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), branch_head);
    assert!(env.repo.status().is_empty());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merge_in_progress_in_the_main_checkout_is_refused() {
    let env = Env::start().await;
    let wt = diverged(&env).await;
    // Leave a conflicted merge behind in the main checkout, as a user might.
    git(&env.repo.root, &["branch", "clash"]);
    git(&env.repo.root, &["checkout", "-q", "clash"]);
    fs::write(env.repo.root.join("a.txt"), "clash\n").unwrap();
    commit_all(&env.repo.root, "clash");
    git(&env.repo.root, &["checkout", "-q", "main"]);
    fs::write(env.repo.root.join("a.txt"), "main side\n").unwrap();
    commit_all(&env.repo.root, "main side");
    let started = Command::new("git")
        .arg("-C")
        .arg(&env.repo.root)
        .args(["merge", "clash"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(!started.status.success());

    let error = remote(env.merge(&wt.path, MergeStrategy::Merge, None).await);
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert!(
        error.message.contains("a merge is in progress"),
        "{}",
        error.message
    );
    env.stop().await;
}

/// `a.txt` and `c.txt` are edited differently on both sides.
async fn conflicting(env: &Env) -> WorktreeInfo {
    let wt = env.worktree("feature/conflict").await;
    fs::write(wt.path.join("a.txt"), "branch side\n").unwrap();
    fs::write(wt.path.join("c.txt"), "branch side c\n").unwrap();
    commit_all(&wt.path, "branch edits");
    fs::write(env.repo.root.join("a.txt"), "main side\n").unwrap();
    fs::write(env.repo.root.join("c.txt"), "main side c\n").unwrap();
    commit_all(&env.repo.root, "main edits");
    wt
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_abort_cleanly_and_name_the_files_for_every_strategy() {
    let env = Env::start().await;
    let wt = conflicting(&env).await;
    let head = env.repo.head();
    let branch_head = git(&wt.path, &["rev-parse", "HEAD"]);
    let branch_files = git(&wt.path, &["ls-files"]);

    for strategy in [
        MergeStrategy::Merge,
        MergeStrategy::Squash,
        MergeStrategy::Rebase,
    ] {
        let error = remote(env.merge(&wt.path, strategy, None).await);
        assert_eq!(error.code, ErrorCode::Conflict, "{strategy:?}: {error:?}");
        assert_eq!(conflict_files(&error), ["a.txt", "c.txt"], "{strategy:?}");
        assert!(
            error.message.contains("a.txt, c.txt") && error.message.contains("nothing was changed"),
            "{}",
            error.message
        );

        // The main checkout is exactly as it was.
        assert_eq!(env.repo.head(), head, "{strategy:?}");
        assert!(
            env.repo.status().is_empty(),
            "{strategy:?}: {}",
            env.repo.status()
        );
        assert!(
            !env.repo.root.join(".git/MERGE_HEAD").exists(),
            "{strategy:?}"
        );
        assert_eq!(
            fs::read_to_string(env.repo.root.join("a.txt")).unwrap(),
            "main side\n"
        );
        // ... and the worktree: no rebase left behind, branch untouched.
        assert_eq!(
            git(&wt.path, &["rev-parse", "HEAD"]),
            branch_head,
            "{strategy:?}"
        );
        assert_eq!(
            git(&wt.path, &["status", "--porcelain"]),
            "",
            "{strategy:?}"
        );
        assert_eq!(git(&wt.path, &["ls-files"]), branch_files);
        let private = PathBuf::from(git(&wt.path, &["rev-parse", "--git-dir"]).trim());
        assert!(!private.join("rebase-merge").exists() && !private.join("rebase-apply").exists());
        assert_eq!(
            git(&wt.path, &["symbolic-ref", "--short", "HEAD"]).trim(),
            "feature/conflict"
        );
    }
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_can_be_removed_after_its_branch_is_merged() {
    let env = Env::start().await;
    let wt = diverged(&env).await;
    env.merge(&wt.path, MergeStrategy::Merge, None)
        .await
        .unwrap();
    // The offer after a merge: remove the worktree and its (now merged) branch.
    let removed = env
        .client
        .worktree_remove(WorktreeRemove {
            path: wt.path.clone(),
            force: false,
            delete_branch: true,
        })
        .await
        .unwrap();
    assert_eq!(
        removed.branch_kept, None,
        "a merged branch goes with `git branch -d`"
    );
    assert!(!wt.path.exists());
    env.stop().await;
}
