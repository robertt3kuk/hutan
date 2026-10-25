//! Staging (protocol 0.53, capability `worktree.stage`, ADR 0020 amendment) end to end: a real
//! daemon on a temporary `HUTAN_HOME`, real client connections and git repositories the tests
//! create in temporary directories — stage and unstage files and hunks (new, deleted, renamed,
//! binary, CRLF), stale hunk ids, discard and Undo through its checkpoint, who may do it, and
//! committing only what is staged.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, create, remote_code, sh};
use hutan_client::Client;
use hutan_core::CheckpointKind;
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::checkpoint::CheckpointPreviewRequest;
use hutan_protocol::messages::{
    Origin, Request, SessionList, SessionListFilter, SessionResult, WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::worktree::{
    DiffHunk, FileHunks, HunkRef, MergeStrategy, WorktreeCommit, WorktreeCreate, WorktreeDiscard,
    WorktreeFinish, WorktreeHunksRequest, WorktreeIndexChange,
};
use tempfile::TempDir;

use crate::checkpoint_restore::{codex, git_env, held_turns, restore_of, turn_end};
use crate::finish::report_agent;

// Fixtures -------------------------------------------------------------------------------------

fn run(dir: &Path, args: &[&str]) -> Vec<u8> {
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
    output.stdout
}

fn git(dir: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&run(dir, args)).into_owned()
}

/// 30 numbered lines.
fn numbered() -> String {
    (0..30).map(|n| format!("line {n:02}\n")).collect()
}

/// `numbered()` with lines 2, 15 and 28 changed: three hunks.
fn edited() -> String {
    numbered()
        .replace("line 02\n", "line 02 changed\n")
        .replace("line 15\n", "line 15 changed\n")
        .replace("line 28\n", "line 28 changed\n")
}

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
            ("core.autocrlf", "false"),
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), numbered()).unwrap();
        fs::write(root.join("b.txt"), "beta\n").unwrap();
        fs::write(root.join("gone.txt"), "gone\n").unwrap();
        fs::write(root.join("crlf.txt"), "one\r\ntwo\r\nthree\r\nfour\r\n").unwrap();
        fs::write(
            root.join("old name.txt"),
            (0..20)
                .map(|n| format!("moved line {n}\n"))
                .collect::<String>(),
        )
        .unwrap();
        fs::write(root.join("pic.bin"), [0_u8, 1, 2, 3, 0, 5]).unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        Self { _dir: dir, root }
    }

    /// The index's content of `file` (`:file`), bytes.
    fn staged(&self, file: &str) -> Vec<u8> {
        run(&self.root, &["show", &format!(":{file}")])
    }

    fn status(&self) -> String {
        git(&self.root, &["status", "--porcelain=v1"])
    }

    /// `git diff --cached` as text.
    fn cached(&self) -> String {
        git(&self.root, &["diff", "--cached", "--no-color"])
    }
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    repo: Repo,
    workspace: hutan_core::WorkspaceId,
}

impl Env {
    async fn start() -> Self {
        let daemon = TestDaemon::start_with("", |c| c.checkpoints.git_env = git_env()).await;
        Self::with(daemon).await
    }

    async fn with(daemon: TestDaemon) -> Self {
        let repo = Repo::new();
        let client = daemon.connect().await;
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
            repo,
            workspace,
        }
    }

    async fn hunks(&self, file: &str) -> FileHunks {
        let listed = self
            .client
            .worktree_hunks(WorktreeHunksRequest {
                path: self.repo.root.clone(),
                files: vec![file.to_owned()],
            })
            .await
            .unwrap();
        listed
            .files
            .into_iter()
            .find(|f| f.path == file)
            .unwrap_or_else(|| panic!("no hunks listed for {file}"))
    }

    fn change(&self, paths: &[&str], hunks: Vec<HunkRef>) -> WorktreeIndexChange {
        WorktreeIndexChange {
            checkout: self.repo.root.clone(),
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            hunks,
            all: false,
        }
    }

    async fn stage(&self, paths: &[&str]) {
        self.client
            .worktree_stage(self.change(paths, vec![]))
            .await
            .unwrap();
    }

    async fn stage_hunks(&self, hunks: Vec<HunkRef>) {
        self.client
            .worktree_stage(self.change(&[], hunks))
            .await
            .unwrap();
    }
}

fn pick(path: &str, hunk: &DiffHunk) -> HunkRef {
    HunkRef {
        path: path.to_owned(),
        hunk_id: hunk.id.clone(),
        lines: vec![],
    }
}

// Files ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn files_are_staged_and_unstaged_new_deleted_and_renamed_without_touching_the_working_tree() {
    let env = Env::start().await;
    let root = &env.repo.root;
    fs::write(root.join("a.txt"), edited()).unwrap();
    fs::write(root.join("new.txt"), "fresh\n").unwrap();
    fs::remove_file(root.join("gone.txt")).unwrap();
    fs::rename(root.join("old name.txt"), root.join("new name.txt")).unwrap();
    let tree = |root: &Path| {
        ["a.txt", "new.txt", "new name.txt", "b.txt"].map(|f| fs::read(root.join(f)).ok())
    };
    let before = tree(root);

    env.stage(&[
        "a.txt",
        "new.txt",
        "gone.txt",
        "old name.txt",
        "new name.txt",
    ])
    .await;
    assert_eq!(
        env.repo.status(),
        "M  a.txt\nD  gone.txt\nR  \"old name.txt\" -> \"new name.txt\"\nA  new.txt\n"
    );
    assert_eq!(tree(root), before, "staging never writes the working tree");

    // Unstage the rename and one more: what is left staged stays.
    let unstaged = env
        .client
        .worktree_unstage(env.change(&["old name.txt", "new name.txt", "new.txt"], vec![]))
        .await
        .unwrap();
    assert_eq!((unstaged.paths, unstaged.hunks), (3, 0));
    assert_eq!(
        env.repo.status(),
        "M  a.txt\nD  gone.txt\n D \"old name.txt\"\n?? \"new name.txt\"\n?? new.txt\n"
    );
    assert_eq!(tree(root), before);

    // Everything at once, both ways.
    env.client
        .worktree_stage(WorktreeIndexChange {
            checkout: root.clone(),
            all: true,
            ..WorktreeIndexChange::default()
        })
        .await
        .unwrap();
    assert!(
        env.repo
            .status()
            .lines()
            .all(|l| !l.starts_with(' ') && !l.starts_with('?'))
    );
    env.client
        .worktree_unstage(WorktreeIndexChange {
            checkout: root.clone(),
            all: true,
            ..WorktreeIndexChange::default()
        })
        .await
        .unwrap();
    assert!(
        env.repo
            .status()
            .lines()
            .all(|l| l.as_bytes()[0] == b' ' || l.starts_with("??"))
    );
    assert_eq!(tree(root), before);
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_names_nothing_or_leaves_the_checkout_is_refused_and_changes_nothing() {
    let env = Env::start().await;
    fs::write(env.repo.root.join("b.txt"), "changed\n").unwrap();
    let nothing = env.client.worktree_stage(env.change(&[], vec![])).await;
    assert_eq!(remote_code(nothing), ErrorCode::BadRequest);
    let outside = env
        .client
        .worktree_stage(env.change(&["b.txt", "../elsewhere"], vec![]))
        .await;
    assert_eq!(remote_code(outside), ErrorCode::BadRequest);
    let relative = env
        .client
        .worktree_stage(WorktreeIndexChange {
            checkout: "repo".into(),
            paths: vec!["b.txt".into()],
            ..WorktreeIndexChange::default()
        })
        .await;
    assert_eq!(remote_code(relative), ErrorCode::BadRequest);
    assert_eq!(env.repo.status(), " M b.txt\n");
    env.daemon.stop().await;
}

// Hunks ------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_hunk_is_staged_and_unstaged_alone_and_the_working_tree_stays() {
    let env = Env::start().await;
    let root = &env.repo.root;
    fs::write(root.join("a.txt"), edited()).unwrap();

    let listed = env.hunks("a.txt").await;
    assert_eq!(listed.unstaged.len(), 3, "{listed:?}");
    assert!(listed.staged.is_empty());
    let ids: Vec<&str> = listed.unstaged.iter().map(|h| h.id.as_str()).collect();
    assert!(ids.iter().all(|id| id.len() == 16));
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "ids differ"
    );
    assert!(listed.unstaged[1].body.contains("+line 15 changed"));
    // The same state, the same ids.
    assert_eq!(env.hunks("a.txt").await, listed);

    // The middle one only.
    env.stage_hunks(vec![pick("a.txt", &listed.unstaged[1])])
        .await;
    let staged_text = String::from_utf8(env.repo.staged("a.txt")).unwrap();
    assert_eq!(
        staged_text,
        numbered().replace("line 15\n", "line 15 changed\n")
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), edited());
    assert_eq!(env.repo.status(), "MM a.txt\n");

    // Now the staged side lists it, the unstaged side the other two.
    let after = env.hunks("a.txt").await;
    assert_eq!(after.staged.len(), 1);
    assert_eq!(after.unstaged.len(), 2);
    assert!(after.staged[0].body.contains("+line 15 changed"));

    // Stage the first of the rest, then unstage the middle again.
    env.stage_hunks(vec![pick("a.txt", &after.unstaged[0])])
        .await;
    let both = env.hunks("a.txt").await;
    assert_eq!(both.staged.len(), 2);
    let middle = both
        .staged
        .iter()
        .find(|h| h.body.contains("line 15 changed"))
        .unwrap()
        .clone();
    env.client
        .worktree_unstage(env.change(&[], vec![pick("a.txt", &middle)]))
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(env.repo.staged("a.txt")).unwrap(),
        numbered().replace("line 02\n", "line 02 changed\n")
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), edited());

    // Two hunks in one request; the last unstaged ones.
    let rest = env.hunks("a.txt").await;
    env.stage_hunks(rest.unstaged.iter().map(|h| pick("a.txt", h)).collect())
        .await;
    assert_eq!(env.repo.status(), "M  a.txt\n");
    assert_eq!(
        String::from_utf8(env.repo.staged("a.txt")).unwrap(),
        edited()
    );
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_hunk_id_is_a_conflict_and_nothing_changes() {
    let env = Env::start().await;
    let root = &env.repo.root;
    fs::write(root.join("a.txt"), edited()).unwrap();
    fs::write(root.join("b.txt"), "changed\n").unwrap();
    let listed = env.hunks("a.txt").await;

    // The file changes under the listing.
    fs::write(
        root.join("a.txt"),
        edited().replace("line 15 changed\n", "line 15 changed again\n"),
    )
    .unwrap();
    let refused = env
        .client
        .worktree_stage(env.change(&["b.txt"], vec![pick("a.txt", &listed.unstaged[1])]))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::Conflict);
    assert_eq!(env.repo.status(), " M a.txt\n M b.txt\n", "all or nothing");

    // An id that never existed, and a file that has no changes.
    let unknown = HunkRef {
        path: "a.txt".into(),
        hunk_id: "0000000000000000".into(),
        lines: vec![],
    };
    let refused = env
        .client
        .worktree_stage(env.change(&[], vec![unknown]))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::Conflict);
    let refused = env
        .client
        .worktree_stage(env.change(
            &[],
            vec![HunkRef {
                path: "keep.txt".into(),
                hunk_id: listed.unstaged[0].id.clone(),
                lines: vec![],
            }],
        ))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::Conflict);

    // A fresh listing works again; a staged hunk cannot be staged twice.
    let fresh = env.hunks("a.txt").await;
    env.stage_hunks(vec![pick("a.txt", &fresh.unstaged[0])])
        .await;
    let twice = env
        .client
        .worktree_stage(env.change(&[], vec![pick("a.txt", &fresh.unstaged[0])]))
        .await;
    assert_eq!(remote_code(twice), ErrorCode::Conflict);
    // Naming a file and one of its hunks is ambiguous.
    let both = env
        .client
        .worktree_stage(env.change(&["a.txt"], vec![pick("a.txt", &fresh.unstaged[1])]))
        .await;
    assert_eq!(remote_code(both), ErrorCode::BadRequest);
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lines_of_a_hunk_are_staged_and_unstaged_one_by_one() {
    let env = Env::start().await;
    let root = &env.repo.root;
    // One hunk: three lines replaced by three others.
    let changed = numbered()
        .replace("line 10\n", "ten\n")
        .replace("line 11\n", "eleven\n")
        .replace("line 12\n", "twelve\n");
    fs::write(root.join("a.txt"), &changed).unwrap();
    let hunk = env.hunks("a.txt").await.unstaged.remove(0);
    let body: Vec<&str> = hunk.body.split('\n').collect();
    let number = |needle: &str| body.iter().position(|l| *l == needle).unwrap() as u32;

    // Only `-line 11` and `+eleven`: the other changes stay out of the index.
    env.client
        .worktree_stage(env.change(
            &[],
            vec![HunkRef {
                path: "a.txt".into(),
                hunk_id: hunk.id.clone(),
                lines: vec![number("-line 11"), number("+eleven")],
            }],
        ))
        .await
        .unwrap();
    // Git puts the removals of a hunk before its additions: what is left of the hunk is `10`,
    // `12` (the removals it keeps as context) and then the one added line.
    assert_eq!(
        String::from_utf8(env.repo.staged("a.txt")).unwrap(),
        numbered().replace("line 11\nline 12\n", "line 12\neleven\n")
    );
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), changed);

    // Unstage just the added line of that: `eleven` goes, its removal stays staged.
    let staged = env.hunks("a.txt").await.staged.remove(0);
    let body: Vec<&str> = staged.body.split('\n').collect();
    let added = body.iter().position(|l| *l == "+eleven").unwrap() as u32;
    env.client
        .worktree_unstage(env.change(
            &[],
            vec![HunkRef {
                path: "a.txt".into(),
                hunk_id: staged.id.clone(),
                lines: vec![added],
            }],
        ))
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(env.repo.staged("a.txt")).unwrap(),
        numbered().replace("line 11\n", "")
    );

    // A context line is not a changed line.
    let fresh = env.hunks("a.txt").await.unstaged.remove(0);
    let context = fresh
        .body
        .split('\n')
        .position(|l| l.starts_with(' '))
        .unwrap() as u32;
    let refused = env
        .client
        .worktree_stage(env.change(
            &[],
            vec![HunkRef {
                path: "a.txt".into(),
                hunk_id: fresh.id.clone(),
                lines: vec![context],
            }],
        ))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_deleted_and_crlf_files_stage_by_hunk_and_a_binary_one_is_refused_per_hunk() {
    let env = Env::start().await;
    let root = &env.repo.root;
    fs::write(root.join("new.txt"), "one\ntwo\nthree\n").unwrap();
    fs::remove_file(root.join("gone.txt")).unwrap();
    fs::write(
        root.join("crlf.txt"),
        "one\r\nTWO\r\nthree\r\nfour\r\nfive\r\n",
    )
    .unwrap();
    fs::write(root.join("pic.bin"), [9_u8, 9, 0, 9]).unwrap();

    // An untracked file has one hunk: all of it.
    let new = env.hunks("new.txt").await;
    assert_eq!(new.unstaged.len(), 1);
    env.stage_hunks(vec![pick("new.txt", &new.unstaged[0])])
        .await;
    assert_eq!(env.repo.staged("new.txt"), b"one\ntwo\nthree\n");

    // A deleted file too.
    let gone = env.hunks("gone.txt").await;
    assert_eq!(gone.unstaged.len(), 1);
    env.stage_hunks(vec![pick("gone.txt", &gone.unstaged[0])])
        .await;
    assert!(
        env.repo.status().contains("D  gone.txt"),
        "{}",
        env.repo.status()
    );

    // CRLF survives: the index holds the bytes of the working tree.
    let crlf = env.hunks("crlf.txt").await;
    env.stage_hunks(crlf.unstaged.iter().map(|h| pick("crlf.txt", h)).collect())
        .await;
    assert_eq!(
        env.repo.staged("crlf.txt"),
        b"one\r\nTWO\r\nthree\r\nfour\r\nfive\r\n"
    );
    assert_eq!(
        env.repo.status().lines().find(|l| l.ends_with("crlf.txt")),
        Some("M  crlf.txt")
    );

    // Binary: listed as binary with no hunks; by hunk refused, whole file fine.
    let listed = env
        .client
        .worktree_hunks(WorktreeHunksRequest {
            path: root.clone(),
            files: vec!["pic.bin".into()],
        })
        .await
        .unwrap();
    assert_eq!(listed.files.len(), 1);
    assert!(listed.files[0].binary);
    assert!(listed.files[0].unstaged.is_empty());
    let refused = env
        .client
        .worktree_stage(env.change(
            &[],
            vec![HunkRef {
                path: "pic.bin".into(),
                hunk_id: "0123456789abcdef".into(),
                lines: vec![],
            }],
        ))
        .await;
    assert_eq!(remote_code(refused), ErrorCode::BadRequest);
    env.stage(&["pic.bin"]).await;
    assert_eq!(env.repo.staged("pic.bin"), vec![9_u8, 9, 0, 9]);

    // Unstaging the new file's hunk takes it out of the index; the file stays.
    let staged_new = env.hunks("new.txt").await.staged.remove(0);
    env.client
        .worktree_unstage(env.change(&[], vec![pick("new.txt", &staged_new)]))
        .await
        .unwrap();
    assert!(env.repo.status().contains("?? new.txt"));
    assert!(root.join("new.txt").is_file());
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hunks_of_a_whole_checkout_are_listed_sorted_with_both_sides() {
    let env = Env::start().await;
    let root = &env.repo.root;
    fs::write(root.join("a.txt"), edited()).unwrap();
    fs::write(root.join("b.txt"), "beta\nmore\n").unwrap();
    env.stage(&["b.txt"]).await;
    fs::write(root.join("b.txt"), "beta\nmore\neven more\n").unwrap();
    let all = env
        .client
        .worktree_hunks(WorktreeHunksRequest {
            path: root.clone(),
            files: vec![],
        })
        .await
        .unwrap();
    assert!(!all.truncated);
    let names: Vec<&str> = all.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["a.txt", "b.txt"]);
    assert_eq!(all.files[0].unstaged.len(), 3);
    assert_eq!(
        (all.files[1].staged.len(), all.files[1].unstaged.len()),
        (1, 1)
    );
    env.daemon.stop().await;
}

// Who ----------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_read_hunks_but_never_stage_unstage_discard_or_commit_staged() {
    let env = Env::start().await;
    fs::write(env.repo.root.join("b.txt"), "changed\n").unwrap();
    env.stage(&["b.txt"]).await;
    fs::write(env.repo.root.join("a.txt"), edited()).unwrap();
    let session = create(&env.client, sh("sleep 60")).await;
    report_agent(&env.daemon, session.id).await;
    let mut options = env.daemon.options();
    options.client.origin = Some(Origin {
        session_id: session.id,
    });
    let agent = Client::connect(options).await.unwrap();
    let root = env.repo.root.clone();

    let read = agent
        .worktree_hunks(WorktreeHunksRequest {
            path: root.clone(),
            files: vec![],
        })
        .await
        .unwrap();
    assert_eq!(read.files.len(), 2);
    let refused = [
        remote_code(agent.worktree_stage(env.change(&["a.txt"], vec![])).await),
        remote_code(agent.worktree_unstage(env.change(&["b.txt"], vec![])).await),
        remote_code(
            agent
                .worktree_discard(WorktreeDiscard {
                    checkout: root.clone(),
                    paths: vec!["a.txt".into()],
                    ..WorktreeDiscard::default()
                })
                .await,
        ),
        remote_code(
            agent
                .worktree_commit(WorktreeCommit {
                    path: root.clone(),
                    message: "agent".into(),
                    staged_only: true,
                })
                .await,
        ),
        remote_code(
            agent
                .worktree_finish(WorktreeFinish {
                    checkout: root.clone(),
                    message: "agent".into(),
                    push: Some(false),
                    into: None,
                    strategy: MergeStrategy::Merge,
                    staged_only: true,
                })
                .await,
        ),
    ];
    assert_eq!(refused, [ErrorCode::Forbidden; 5]);
    assert_eq!(env.repo.status(), " M a.txt\nM  b.txt\n");
    assert_eq!(
        git(&root, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "nothing was committed"
    );
    drop(agent);
    env.daemon.stop().await;
}

// Commit staged -----------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn commit_staged_takes_what_is_staged_and_leaves_the_rest_in_a_worktree_and_the_main_checkout()
 {
    let env = Env::start().await;
    let root = env.repo.root.clone();
    fs::write(root.join("b.txt"), "changed\n").unwrap();
    fs::write(root.join("a.txt"), edited()).unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();

    // Nothing staged: refused, nothing committed, nothing staged by the attempt.
    let nothing = env
        .client
        .worktree_commit(WorktreeCommit {
            path: root.clone(),
            message: "nothing".into(),
            staged_only: true,
        })
        .await;
    assert_eq!(remote_code(nothing), ErrorCode::InvalidState);
    assert_eq!(env.repo.status(), " M a.txt\n M b.txt\n?? new.txt\n");

    // The main checkout: one file and one hunk of another.
    env.stage(&["b.txt"]).await;
    let hunks = env.hunks("a.txt").await;
    env.stage_hunks(vec![pick("a.txt", &hunks.unstaged[2])])
        .await;
    let committed = env
        .client
        .worktree_commit(WorktreeCommit {
            path: root.clone(),
            message: "Only the staged".into(),
            staged_only: true,
        })
        .await
        .unwrap();
    assert_eq!(committed.files, 2);
    assert_eq!(committed.branch, "main");
    assert_eq!(
        git(&root, &["show", "--name-only", "--format=%s", "HEAD"]),
        "Only the staged\n\na.txt\nb.txt\n"
    );
    assert_eq!(
        git(&root, &["show", "HEAD:a.txt"]),
        numbered().replace("line 28\n", "line 28 changed\n")
    );
    // The rest is where it was, untouched.
    assert_eq!(env.repo.status(), " M a.txt\n?? new.txt\n");
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), edited());

    // A worktree, and the Finish variant on the main checkout.
    let workspace = env.workspace;
    let wt = env
        .client
        .worktree_create(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature/staged".into(),
            base: None,
            path: None,
            no_setup: true,
        })
        .await
        .unwrap();
    fs::write(wt.path.join("b.txt"), "in the worktree\n").unwrap();
    fs::write(wt.path.join("c.txt"), "also\n").unwrap();
    git(&wt.path, &["add", "b.txt"]);
    let committed = env
        .client
        .worktree_commit(WorktreeCommit {
            path: wt.path.clone(),
            message: "Staged in the worktree".into(),
            staged_only: true,
        })
        .await
        .unwrap();
    assert_eq!(
        (committed.files, committed.branch.as_str()),
        (1, "feature/staged")
    );
    assert_eq!(git(&wt.path, &["status", "--porcelain=v1"]), "?? c.txt\n");

    git(&root, &["add", "new.txt"]);
    let finished = env
        .client
        .worktree_finish(WorktreeFinish {
            checkout: root.clone(),
            message: "Finish the staged".into(),
            push: Some(false),
            into: None,
            strategy: MergeStrategy::Merge,
            staged_only: true,
        })
        .await
        .unwrap();
    assert_eq!(
        finished.commit.status,
        hutan_protocol::worktree::StepStatus::Ok
    );
    assert_eq!(finished.commit.files, 1);
    assert_eq!(env.repo.status(), " M a.txt\n");
    // Nothing staged now: the commit step is skipped, not failed.
    let again = env
        .client
        .worktree_finish(WorktreeFinish {
            checkout: root.clone(),
            message: "Nothing".into(),
            push: Some(false),
            into: None,
            strategy: MergeStrategy::Merge,
            staged_only: true,
        })
        .await
        .unwrap();
    assert_eq!(
        again.commit.status,
        hutan_protocol::worktree::StepStatus::Skipped
    );
    env.daemon.stop().await;
}

// Discard -----------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn discard_restores_from_the_index_and_undo_puts_everything_back() {
    let env = Env::start().await;
    let root = env.repo.root.clone();
    fs::write(root.join("a.txt"), edited()).unwrap();
    fs::write(root.join("b.txt"), "staged\n").unwrap();
    env.stage(&["b.txt"]).await;
    fs::write(root.join("b.txt"), "staged then edited\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();
    fs::remove_file(root.join("gone.txt")).unwrap();
    let index_before = env.repo.cached();

    let discarded = env
        .client
        .worktree_discard(WorktreeDiscard {
            checkout: root.clone(),
            paths: vec![
                "a.txt".into(),
                "b.txt".into(),
                "new.txt".into(),
                "gone.txt".into(),
            ],
            ..WorktreeDiscard::default()
        })
        .await
        .unwrap();
    assert_eq!((discarded.paths, discarded.hunks), (4, 0));
    assert_eq!(discarded.checkpoint.kind, CheckpointKind::Restore);
    // The working tree is the index: a.txt as committed, b.txt as staged, the untracked file
    // gone, the deleted one back; the index itself is as it was.
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), numbered());
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "staged\n");
    assert!(!root.join("new.txt").exists());
    assert!(root.join("gone.txt").is_file());
    assert_eq!(env.repo.status(), "M  b.txt\n");
    assert_eq!(env.repo.cached(), index_before);

    // Undo: the checkpoint's restore brings the files back (the index is not part of it).
    let id = discarded.checkpoint.id;
    let preview = env
        .client
        .checkpoint_preview(CheckpointPreviewRequest {
            checkpoint_id: id,
            through: None,
            paths: vec![],
        })
        .await
        .unwrap();
    env.client
        .checkpoint_restore(restore_of(&preview, id))
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), edited());
    assert_eq!(
        fs::read_to_string(root.join("b.txt")).unwrap(),
        "staged then edited\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("new.txt")).unwrap(),
        "untracked\n"
    );
    assert!(!root.join("gone.txt").exists());
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discarding_a_hunk_or_the_staged_part_works_and_a_stale_id_discards_nothing() {
    let env = Env::start().await;
    let root = env.repo.root.clone();
    fs::write(root.join("a.txt"), edited()).unwrap();
    let listed = env.hunks("a.txt").await;

    // A stale id: nothing is discarded and no checkpoint is left over.
    let stale = HunkRef {
        path: "a.txt".into(),
        hunk_id: "ffffffffffffffff".into(),
        lines: vec![],
    };
    let refused = env
        .client
        .worktree_discard(WorktreeDiscard {
            checkout: root.clone(),
            hunks: vec![stale],
            ..WorktreeDiscard::default()
        })
        .await;
    assert_eq!(remote_code(refused), ErrorCode::Conflict);
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), edited());
    let checkpoints = env
        .client
        .checkpoint_list(hutan_protocol::checkpoint::CheckpointListRequest::default())
        .await
        .map(|l| l.checkpoints.len())
        .unwrap_or(0);
    assert_eq!(checkpoints, 0);

    // The middle hunk goes; the others stay.
    let discarded = env
        .client
        .worktree_discard(WorktreeDiscard {
            checkout: root.clone(),
            hunks: vec![pick("a.txt", &listed.unstaged[1])],
            ..WorktreeDiscard::default()
        })
        .await
        .unwrap();
    assert_eq!((discarded.paths, discarded.hunks), (0, 1));
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).unwrap(),
        edited().replace("line 15 changed\n", "line 15\n")
    );
    assert_eq!(env.repo.status(), " M a.txt\n");

    // A new file's hunk deletes the file; a staged file needs `staged`.
    fs::write(root.join("new.txt"), "x\n").unwrap();
    env.stage(&["new.txt"]).await;
    let refused = env
        .client
        .worktree_discard(WorktreeDiscard {
            checkout: root.clone(),
            paths: vec!["missing.txt".into()],
            ..WorktreeDiscard::default()
        })
        .await;
    assert_eq!(remote_code(refused), ErrorCode::NotFound);
    env.client
        .worktree_discard(WorktreeDiscard {
            checkout: root.clone(),
            paths: vec!["new.txt".into(), "a.txt".into()],
            staged: true,
            ..WorktreeDiscard::default()
        })
        .await
        .unwrap();
    assert!(!root.join("new.txt").exists());
    assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), numbered());
    assert_eq!(env.repo.status(), "");
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_turn_in_the_checkout_refuses_a_discard() {
    let work = tempfile::tempdir().unwrap();
    let gate = work.path().join("gate");
    let config = codex(
        &held_turns(work.path(), std::slice::from_ref(&gate)),
        &work.path().join("received.jsonl"),
    );
    let daemon = TestDaemon::start_with("", |c| {
        c.codex = Some(config);
        c.checkpoints.git_env = git_env();
    })
    .await;
    let env = Env::with(daemon).await;
    let root = env.repo.root.clone();
    let (user, mut events) = env.daemon.connect_subscribed().await;
    let session = user
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: root.clone(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: None,
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
            safety: None,
            contain: None,
        }))
        .await
        .unwrap()
        .session
        .id;
    fs::write(root.join("b.txt"), "edited\n").unwrap();
    user.call::<ChatSent>(Request::ChatSend(ChatSend {
        session_id: session,
        text: "go".into(),
        attachments: vec![],
    }))
    .await
    .unwrap();

    // Wait until the registry says the turn runs, then: staging goes on, discarding is refused.
    for _ in 0..400 {
        let sessions = env
            .client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions;
        if sessions
            .iter()
            .any(|s| s.chat.as_ref().is_some_and(|chat| chat.turn_active))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let discard = || WorktreeDiscard {
        checkout: root.clone(),
        paths: vec!["b.txt".into()],
        ..WorktreeDiscard::default()
    };
    let refused = env.client.worktree_discard(discard()).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "edited\n");
    env.stage(&["b.txt"]).await;

    fs::write(&gate, "").unwrap();
    turn_end(&mut events).await;
    // The turn ended, but the registry may still call the chat busy for a moment.
    let mut done = None;
    for _ in 0..200 {
        match env.client.worktree_discard(discard()).await {
            Ok(done_now) => {
                done = Some(done_now);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    assert!(done.is_some(), "the discard never went through");
    assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "edited\n");
    env.daemon.stop().await;
}
