//! `git.log`, `git.blame`, `git.show` and `git.file` (protocol 0.46, capability `git.history`)
//! end to end: a real daemon on a temporary `HUTAN_HOME`, real client connections, and git
//! repositories the tests create in temporary directories (git runs without the account's
//! configuration).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, remote_code};
use hutan_client::Client;
use hutan_protocol::ErrorCode;
use hutan_protocol::git::{
    GIT_LOG_MAX_LIMIT, GIT_SHOW_PATCH_CAP, GIT_UNCOMMITTED, GitBlame, GitFile, GitLog, GitShow,
};
use hutan_protocol::messages::{Request, WorkspaceOpen, WorkspaceOpened};
use hutan_protocol::worktree::{DiffStatus, WorktreeCreate};
use tempfile::TempDir;

/// Runs git in `dir` without the user's configuration, as the author `who`; returns stdout.
fn git_as(dir: &Path, who: &str, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", who)
        .env(
            "GIT_AUTHOR_EMAIL",
            format!("{}@example.com", who.to_lowercase()),
        )
        .env("GIT_COMMITTER_NAME", who)
        .env(
            "GIT_COMMITTER_EMAIL",
            format!("{}@example.com", who.to_lowercase()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_as(dir, "Test", args)
}

fn commit_all(dir: &Path, who: &str, message: &str) -> String {
    git_as(dir, who, &["add", "-A"]);
    git_as(dir, who, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn lines(prefix: &str, count: usize) -> String {
    (0..count).map(|n| format!("{prefix} line {n}\n")).collect()
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    _dir: TempDir,
    root: PathBuf,
}

impl Env {
    /// A repository `<tmp>/repo` with no commit, opened as a project.
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        for (key, value) in [("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")] {
            git(&root, &["config", key, value]);
        }
        let daemon = TestDaemon::start().await;
        let client = daemon.connect().await;
        client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: root.clone(),
                created_by: None,
            }))
            .await
            .unwrap();
        Self {
            daemon,
            client,
            _dir: dir,
            root,
        }
    }

    async fn stop(self) {
        drop(self.client);
        self.daemon.stop().await;
    }

    async fn log(
        &self,
        path: &Path,
        limit: u32,
        before: Option<&str>,
    ) -> hutan_protocol::git::GitLogResult {
        self.client
            .git_log(GitLog {
                path: path.to_owned(),
                rev: None,
                limit,
                before: before.map(str::to_owned),
            })
            .await
            .unwrap()
    }

    async fn blame(&self, path: &Path, rev: Option<&str>) -> hutan_protocol::git::GitBlameResult {
        self.client
            .git_blame(GitBlame {
                path: path.to_owned(),
                rev: rev.map(str::to_owned),
                contents: None,
            })
            .await
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_log_pages_by_the_last_commit_it_answered() {
    let env = Env::start().await;
    let mut ids = Vec::new();
    for n in 0..5 {
        fs::write(env.root.join("f.txt"), lines("v", n + 1)).unwrap();
        ids.push(commit_all(
            &env.root,
            if n % 2 == 0 { "Ada" } else { "Grace" },
            &format!("step {n}"),
        ));
    }
    ids.reverse();

    let first = env.log(&env.root, 2, None).await;
    assert_eq!(first.root, env.root);
    assert_eq!(
        first
            .commits
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        ids[..2]
    );
    assert!(first.more);
    assert_eq!(first.next_before.as_deref(), Some(ids[1].as_str()));
    assert_eq!(first.commits[0].subject, "step 4");
    assert_eq!(first.commits[0].author, "Ada");
    assert_eq!(first.commits[0].email, "ada@example.com");
    assert!(first.commits[0].time > 1_500_000_000);
    assert!(!first.commits[0].merge);
    assert_eq!(
        first.commits[0].path, None,
        "a repository log has no file name"
    );

    let second = env.log(&env.root, 2, first.next_before.as_deref()).await;
    assert_eq!(
        second
            .commits
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        ids[2..4]
    );
    assert!(second.more);
    let third = env.log(&env.root, 2, second.next_before.as_deref()).await;
    assert_eq!(
        third
            .commits
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>(),
        ids[4..]
    );
    assert!(!third.more);
    assert_eq!(third.next_before, None);

    // The whole history in one page: no `more`.
    let all = env.log(&env.root, 5, None).await;
    assert_eq!(all.commits.len(), 5);
    assert!(!all.more);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_files_history_follows_a_rename_and_pages_across_it() {
    let env = Env::start().await;
    fs::write(env.root.join("old.txt"), lines("body", 30)).unwrap();
    fs::write(env.root.join("other.txt"), "other\n").unwrap();
    let created = commit_all(&env.root, "Ada", "create old");
    fs::write(
        env.root.join("old.txt"),
        format!("{}tail\n", lines("body", 30)),
    )
    .unwrap();
    let edited = commit_all(&env.root, "Grace", "edit old");
    git(&env.root, &["mv", "old.txt", "new.txt"]);
    let renamed = commit_all(&env.root, "Ada", "rename to new");
    fs::write(env.root.join("other.txt"), "changed\n").unwrap();
    commit_all(&env.root, "Ada", "unrelated");
    fs::write(
        env.root.join("new.txt"),
        format!("{}more\n", lines("body", 31)),
    )
    .unwrap();
    let last = commit_all(&env.root, "Grace", "edit new");

    let file = env.root.join("new.txt");
    let all = env.log(&file, 50, None).await;
    let subjects: Vec<&str> = all.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(
        subjects,
        ["edit new", "rename to new", "edit old", "create old"]
    );
    assert_eq!(all.commits[0].path.as_deref(), Some("new.txt"));
    assert_eq!(
        all.commits[1].old_path.as_deref(),
        Some("old.txt"),
        "the rename names the old file"
    );
    assert_eq!(all.commits[1].path.as_deref(), Some("new.txt"));
    assert_eq!(all.commits[2].path.as_deref(), Some("old.txt"));
    assert_eq!(all.commits[3].old_path, None);

    // One commit per page walks the same history, rename included.
    let mut seen = Vec::new();
    let mut before: Option<String> = None;
    loop {
        let page = env.log(&file, 1, before.as_deref()).await;
        seen.extend(page.commits.iter().map(|c| c.id.clone()));
        if !page.more {
            break;
        }
        before = page.next_before;
    }
    assert_eq!(seen, [last, renamed, edited, created]);

    // A folder is limited, not followed.
    fs::create_dir(env.root.join("dir")).unwrap();
    fs::write(env.root.join("dir/x.txt"), "x\n").unwrap();
    commit_all(&env.root, "Ada", "add dir");
    let dir = env.log(&env.root.join("dir"), 10, None).await;
    assert_eq!(dir.commits.len(), 1);
    assert_eq!(dir.commits[0].path, None);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blame_maps_every_line_to_its_commit_and_marks_what_is_not_committed() {
    let env = Env::start().await;
    let file = env.root.join("f.txt");
    fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let first = commit_all(&env.root, "Ada", "first");
    fs::write(&file, "one\nTWO\nthree\nfour\n").unwrap();
    let second = commit_all(&env.root, "Grace", "second: shout two");

    let committed = env.blame(&file, None).await;
    assert_eq!(committed.root, env.root);
    assert_eq!(committed.path, "f.txt");
    assert_eq!(committed.rev, None);
    assert_eq!(committed.lines, 4);
    let map: Vec<(u32, u32, &str)> = committed
        .hunks
        .iter()
        .map(|h| (h.line, h.count, h.commit.as_str()))
        .collect();
    assert_eq!(
        map,
        [
            (1, 1, first.as_str()),
            (2, 1, second.as_str()),
            (3, 1, first.as_str()),
            (4, 1, second.as_str()),
        ]
    );
    let who = |id: &str| committed.commits.iter().find(|c| c.id == id).unwrap();
    assert_eq!(who(&first).author, "Ada");
    assert_eq!(who(&first).subject, "first");
    assert!(who(&first).boundary, "the root commit is git's boundary");
    assert_eq!(who(&second).author, "Grace");
    assert_eq!(who(&second).subject, "second: shout two");
    assert_eq!(who(&second).email, "grace@example.com");
    assert_eq!(who(&second).path, "f.txt");

    // An edit on disk: those lines belong to no commit.
    fs::write(&file, "one\nTWO\nnew line\nthree\nfour\n").unwrap();
    let dirty = env.blame(&file, None).await;
    assert_eq!(dirty.lines, 5);
    let line3 = dirty.hunks.iter().find(|h| h.line == 3).unwrap();
    assert_eq!(line3.commit, GIT_UNCOMMITTED);
    let pending = dirty
        .commits
        .iter()
        .find(|c| c.id == GIT_UNCOMMITTED)
        .unwrap();
    assert_eq!(pending.subject, "");
    assert_eq!(dirty.hunks.iter().map(|h| h.count).sum::<u32>(), 5);

    // The editor's unsaved text is blamed as it stands: its own lines belong to no commit,
    // however the disk differs.
    let buffer = "one\nTWO\nmine\nthree\nfour\nmore\n";
    let from_buffer = env
        .client
        .git_blame(GitBlame {
            path: file.clone(),
            rev: None,
            contents: Some(buffer.to_owned()),
        })
        .await
        .unwrap();
    assert_eq!(from_buffer.lines, 6);
    let by_line: Vec<(u32, bool)> = from_buffer
        .hunks
        .iter()
        .flat_map(|h| (h.line..h.line + h.count).map(move |l| (l, h.commit == GIT_UNCOMMITTED)))
        .collect();
    assert_eq!(
        by_line,
        [
            (1, false),
            (2, false),
            (3, true),
            (4, false),
            (5, false),
            (6, true)
        ]
    );

    // A revision blames what was committed there.
    let at_first = env.blame(&file, Some(&first)).await;
    assert_eq!(at_first.rev.as_deref(), Some(first.as_str()));
    assert_eq!(at_first.lines, 3);
    assert_eq!(at_first.hunks.len(), 1);
    assert_eq!(at_first.hunks[0].commit, first);

    // A renamed file's lines name the file as it was.
    git(&env.root, &["checkout", "-q", "--", "f.txt"]);
    git(&env.root, &["mv", "f.txt", "g.txt"]);
    commit_all(&env.root, "Ada", "rename");
    let renamed = env.blame(&env.root.join("g.txt"), None).await;
    assert_eq!(renamed.path, "g.txt");
    let one = renamed.commits.iter().find(|c| c.id == first).unwrap();
    assert_eq!(one.path, "f.txt");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn show_gives_the_message_files_and_patch_of_a_commit_limited_to_a_file() {
    let env = Env::start().await;
    fs::write(env.root.join("a.txt"), "a\n").unwrap();
    fs::write(env.root.join("old.txt"), lines("body", 30)).unwrap();
    let root_commit = commit_all(&env.root, "Ada", "root");
    fs::write(env.root.join("a.txt"), "a\nb\n").unwrap();
    git(&env.root, &["mv", "old.txt", "new.txt"]);
    fs::write(env.root.join("added.txt"), "added\n").unwrap();
    git_as(&env.root, "Grace", &["add", "-A"]);
    git_as(
        &env.root,
        "Grace",
        &["commit", "-q", "-m", "second\n\nThe body of the message."],
    );
    let second = git(&env.root, &["rev-parse", "HEAD"]).trim().to_owned();

    let shown = env
        .client
        .git_show(GitShow {
            path: env.root.clone(),
            rev: second.clone(),
            old_path: None,
            patch: true,
        })
        .await
        .unwrap();
    assert_eq!(shown.root, env.root);
    assert_eq!(shown.commit.id, second);
    assert_eq!(shown.commit.author, "Grace");
    assert_eq!(shown.commit.subject, "second");
    assert!(shown.message.contains("The body of the message."));
    assert_eq!(shown.parent.as_deref(), Some(root_commit.as_str()));
    let paths: Vec<(&str, DiffStatus)> = shown
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.status))
        .collect();
    assert_eq!(
        paths,
        [
            ("a.txt", DiffStatus::Modified),
            ("added.txt", DiffStatus::Added),
            ("new.txt", DiffStatus::Renamed),
        ]
    );
    assert_eq!(shown.files[2].old_path.as_deref(), Some("old.txt"));
    assert!(shown.files.iter().all(|f| f.committed && !f.uncommitted));
    assert_eq!((shown.files[0].additions, shown.files[0].deletions), (1, 0));
    assert!(shown.patch.contains("diff --git a/a.txt b/a.txt"));
    assert!(shown.patch.contains("+b"));
    assert!(!shown.truncated);

    // A file, and the name it had: the rename stays a rename.
    let one = env
        .client
        .git_show(GitShow {
            path: env.root.join("new.txt"),
            rev: second.clone(),
            old_path: Some(env.root.join("old.txt")),
            patch: true,
        })
        .await
        .unwrap();
    assert_eq!(one.files.len(), 1);
    assert_eq!(one.files[0].status, DiffStatus::Renamed);
    assert!(one.patch.contains("rename from old.txt"));
    assert!(!one.patch.contains("a.txt b/a.txt"));

    // Without the old name the same file looks new; without the patch only counts come back.
    let bare = env
        .client
        .git_show(GitShow {
            path: env.root.join("new.txt"),
            rev: second.clone(),
            old_path: None,
            patch: false,
        })
        .await
        .unwrap();
    assert_eq!(bare.files[0].status, DiffStatus::Added);
    assert_eq!(bare.patch, "");

    // The root commit diffs against nothing.
    let first = env
        .client
        .git_show(GitShow {
            path: env.root.clone(),
            rev: "HEAD~1".into(),
            old_path: None,
            patch: true,
        })
        .await
        .unwrap();
    assert_eq!(first.parent, None);
    assert_eq!(first.files.len(), 2);
    assert!(first.files.iter().all(|f| f.status == DiffStatus::Added));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn file_at_a_revision_is_text_bounded_and_refuses_what_is_not_there() {
    let env = Env::start().await;
    fs::write(env.root.join("f.txt"), "old text\n").unwrap();
    fs::write(env.root.join("blob.bin"), [0_u8, 1, 2, 3]).unwrap();
    let first = commit_all(&env.root, "Ada", "first");
    fs::write(env.root.join("f.txt"), "new text\n").unwrap();
    fs::write(env.root.join("big.txt"), vec![b'x'; 2 * 1024 * 1024 + 10]).unwrap();
    let second = commit_all(&env.root, "Ada", "second");

    let at = |rev: &str, file: &str| GitFile {
        path: env.root.join(file),
        rev: rev.to_owned(),
    };
    let old = env.client.git_file(at(&first, "f.txt")).await.unwrap();
    assert_eq!(
        (old.content.as_str(), old.size, old.rev.as_str()),
        ("old text\n", 9, first.as_str())
    );
    let new = env.client.git_file(at("HEAD", "f.txt")).await.unwrap();
    assert_eq!(new.content, "new text\n");
    assert_eq!(new.rev, second);
    let blob = env.client.git_file(at("HEAD", "blob.bin")).await.unwrap();
    assert!(blob.binary);
    assert_eq!(blob.content, "");
    let big = env.client.git_file(at("HEAD", "big.txt")).await.unwrap();
    assert!(big.too_large);
    assert_eq!(big.size, 2 * 1024 * 1024 + 10);
    assert_eq!(big.content, "");

    // Not in that revision, or no such revision.
    assert_eq!(
        remote_code(env.client.git_file(at(&first, "big.txt")).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(env.client.git_file(at("no-such-rev", "f.txt")).await),
        ErrorCode::NotFound
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queries_are_bounded_and_refuse_bad_requests() {
    let env = Env::start().await;

    // No commit yet: an empty log, not an error.
    let empty = env.log(&env.root, 10, None).await;
    assert!(empty.commits.is_empty() && !empty.more);

    // 205 commits: a page never holds more than the maximum, however much is asked for.
    fs::write(env.root.join("f.txt"), "x\n").unwrap();
    git(&env.root, &["add", "-A"]);
    git(&env.root, &["commit", "-q", "-m", "c0"]);
    for n in 1..=(GIT_LOG_MAX_LIMIT + 4) {
        git(
            &env.root,
            &["commit", "-q", "--allow-empty", "-m", &format!("c{n}")],
        );
    }
    let page = env.log(&env.root, 10_000, None).await;
    assert_eq!(page.commits.len(), GIT_LOG_MAX_LIMIT as usize);
    assert!(page.more);
    let rest = env
        .log(&env.root, 10_000, page.next_before.as_deref())
        .await;
    assert_eq!(rest.commits.len(), 5);
    assert!(!rest.more);

    let log = |limit: u32, rev: Option<&str>, before: Option<&str>| GitLog {
        path: env.root.clone(),
        rev: rev.map(str::to_owned),
        limit,
        before: before.map(str::to_owned),
    };
    assert_eq!(
        remote_code(env.client.git_log(log(0, None, None)).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(
            env.client
                .git_log(log(5, Some("--output=/tmp/x"), None))
                .await
        ),
        ErrorCode::BadRequest,
        "a revision never starts an option"
    );
    assert_eq!(
        remote_code(
            env.client
                .git_log(log(5, Some("no-such-branch"), None))
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(env.client.git_log(log(5, None, Some("not hex"))).await),
        ErrorCode::BadRequest
    );
    assert_eq!(
        remote_code(
            env.client
                .git_log(log(
                    5,
                    None,
                    Some("0123456789abcdef0123456789abcdef01234567")
                ))
                .await
        ),
        ErrorCode::NotFound,
        "a `before` that is not in the log"
    );
    // A path that is not absolute, or in no checkout of a project.
    assert_eq!(
        remote_code(
            env.client
                .git_log(GitLog {
                    path: "relative/f.txt".into(),
                    rev: None,
                    limit: 5,
                    before: None
                })
                .await
        ),
        ErrorCode::BadRequest
    );
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(
        remote_code(
            env.client
                .git_log(GitLog {
                    path: fs::canonicalize(elsewhere.path()).unwrap(),
                    rev: None,
                    limit: 5,
                    before: None
                })
                .await
        ),
        ErrorCode::NotFound
    );
    // Blame needs a tracked file.
    assert_eq!(
        remote_code(
            env.client
                .git_blame(GitBlame {
                    path: env.root.clone(),
                    rev: None,
                    contents: None
                })
                .await
        ),
        ErrorCode::BadRequest
    );
    fs::write(env.root.join("untracked.txt"), "u\n").unwrap();
    assert_eq!(
        remote_code(
            env.client
                .git_blame(GitBlame {
                    path: env.root.join("untracked.txt"),
                    rev: None,
                    contents: None
                })
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        remote_code(
            env.client
                .git_blame(GitBlame {
                    path: env.root.join("missing.txt"),
                    rev: None,
                    contents: None
                })
                .await
        ),
        ErrorCode::NotFound
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_patch_over_the_cap_is_cut_on_a_line() {
    let env = Env::start().await;
    fs::write(env.root.join("seed.txt"), "seed\n").unwrap();
    commit_all(&env.root, "Ada", "seed");
    fs::write(
        env.root.join("big.txt"),
        lines("a long enough line of text to fill a file", 80_000),
    )
    .unwrap();
    let big = commit_all(&env.root, "Ada", "big");
    let shown = env
        .client
        .git_show(GitShow {
            path: env.root.clone(),
            rev: big,
            old_path: None,
            patch: true,
        })
        .await
        .unwrap();
    assert!(shown.truncated);
    assert!(shown.patch.len() <= GIT_SHOW_PATCH_CAP);
    assert!(shown.patch.ends_with('\n'), "cut at a line boundary");
    assert_eq!(
        shown.files[0].additions, 80_000,
        "the counts are still exact"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_linked_worktree_answers_for_its_own_files() {
    let env = Env::start().await;
    fs::write(env.root.join("f.txt"), "main\n").unwrap();
    commit_all(&env.root, "Ada", "on main");
    let workspace = env
        .client
        .call::<hutan_protocol::messages::WorkspaceList>(Request::WorkspaceList(
            hutan_protocol::messages::Empty {},
        ))
        .await
        .unwrap()
        .workspaces
        .into_iter()
        .find(|w| w.roots.contains(&env.root))
        .unwrap()
        .id;
    let worktree = env
        .client
        .worktree_create(WorktreeCreate {
            workspace_id: workspace,
            repo: None,
            branch: "feature".into(),
            base: None,
            path: None,
        })
        .await
        .unwrap();
    fs::write(worktree.path.join("f.txt"), "main\nfeature\n").unwrap();
    let on_feature = commit_all(&worktree.path, "Grace", "on feature");

    let log = env.log(&worktree.path.join("f.txt"), 10, None).await;
    assert_eq!(log.root, fs::canonicalize(&worktree.path).unwrap());
    assert_eq!(
        log.commits[0].id, on_feature,
        "the worktree's branch, not the main checkout's"
    );
    let blame = env.blame(&worktree.path.join("f.txt"), None).await;
    assert_eq!(blame.hunks.len(), 2);
    assert_eq!(blame.hunks[1].commit, on_feature);
    // The main checkout is untouched by all of it.
    assert_eq!(
        env.log(&env.root.join("f.txt"), 10, None)
            .await
            .commits
            .len(),
        1
    );
    env.stop().await;
}
