//! `hutan git log|blame|show` through the real binary against a real in-process daemon and
//! repositories the tests create in temporary directories.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

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

fn commit(dir: &Path, who: &str, message: &str) {
    git_as(dir, who, &["add", "-A"]);
    git_as(dir, who, &["commit", "-q", "-m", message]);
}

struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Repo {
    /// `a.txt` in three commits (Ada, Grace, Ada) and `old.txt` renamed to `new.txt` in a
    /// fourth.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("repo");
        fs::create_dir(&root).unwrap();
        git_as(&root, "Ada", &["init", "-q", "-b", "main"]);
        for (key, value) in [("commit.gpgsign", "false"), ("core.hooksPath", "/dev/null")] {
            git_as(&root, "Ada", &["config", key, value]);
        }
        fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(
            root.join("old.txt"),
            (0..30).map(|n| format!("body {n}\n")).collect::<String>(),
        )
        .unwrap();
        commit(&root, "Ada", "create a");
        fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        commit(&root, "Grace", "shout two");
        fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
        commit(&root, "Ada", "add four");
        git_as(&root, "Ada", &["mv", "old.txt", "new.txt"]);
        commit(&root, "Grace", "rename old to new");
        Self { _dir: dir, root }
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

async fn hutan_in(t: &TestDaemon, cwd: &Path, args: &[&str]) -> Out {
    let mut cmd = command(Path::new(HUTAN), Some(&t.paths.socket), args);
    cmd.current_dir(cwd);
    finish(cmd).await
}

async fn setup(t: &TestDaemon) -> Repo {
    let repo = Repo::new();
    t.hutan(&["project", "open", s(&repo.root)]).await.ok();
    repo
}

#[tokio::test(flavor = "multi_thread")]
async fn log_lists_commits_pages_and_follows_a_file_across_a_rename() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    let out = hutan_in(&t, &repo.root, &["git", "log"]).await.ok();
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{out:?}");
    assert!(
        lines[0].contains("Grace") && lines[0].ends_with("rename old to new"),
        "{}",
        lines[0]
    );
    assert!(
        lines[3].contains("Ada") && lines[3].ends_with("create a"),
        "{}",
        lines[3]
    );
    assert_eq!(lines[0].split(' ').next().unwrap().len(), 7, "a short hash");
    assert_eq!(out.stderr, "");

    // Paging by the hash the answer names.
    let page = hutan_in(&t, &repo.root, &["git", "log", "-n", "2"])
        .await
        .ok();
    assert_eq!(page.stdout.lines().count(), 2);
    let next = page
        .stderr
        .trim()
        .strip_prefix("more: continue with --before ")
        .unwrap_or_else(|| panic!("{page:?}"))
        .to_owned();
    let rest = hutan_in(
        &t,
        &repo.root,
        &["git", "log", "-n", "2", "--before", &next],
    )
    .await
    .ok();
    assert_eq!(rest.stdout.lines().count(), 2);
    assert!(
        rest.stdout.contains("shout two") && rest.stdout.contains("create a"),
        "{rest:?}"
    );
    assert!(!rest.stderr.contains("more:"), "{rest:?}");

    // A file: renames are followed and the old name is shown.
    let file = hutan_in(&t, &repo.root, &["git", "log", "new.txt"])
        .await
        .ok();
    let lines: Vec<&str> = file.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{file:?}");
    assert!(lines[0].contains("(renamed from old.txt)"), "{}", lines[0]);
    // A path of another directory works, and a revision starts the log there.
    let at = hutan_in(
        &t,
        Path::new("/"),
        &["git", "log", s(&repo.root.join("a.txt")), "--rev", "HEAD~2"],
    )
    .await
    .ok();
    assert_eq!(at.stdout.lines().count(), 2, "{at:?}");

    let json = hutan_in(&t, &repo.root, &["--json", "git", "log", "-n", "1"])
        .await
        .json();
    assert_eq!(json["commits"][0]["subject"], "rename old to new");
    assert_eq!(json["more"], true);
    assert_eq!(json["root"], s(&repo.root));

    // Refusals say why.
    let bad = hutan_in(&t, &repo.root, &["git", "log", "--rev", "no-such-branch"]).await;
    assert_ne!(bad.code, 0);
    assert!(bad.stderr.contains("no commit named"), "{bad:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn blame_prints_the_commit_of_every_line_and_marks_uncommitted_ones() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    let out = hutan_in(&t, &repo.root, &["git", "blame", "a.txt"])
        .await
        .ok();
    let lines: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{out:?}");
    assert!(
        lines[0].contains("Ada") && lines[0].ends_with("   1) one"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("Grace") && lines[1].ends_with("   2) TWO"),
        "{}",
        lines[1]
    );
    assert!(
        lines[3].contains("Ada") && lines[3].ends_with("   4) four"),
        "{}",
        lines[3]
    );

    // An edit on disk is not in a commit.
    fs::write(repo.root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").unwrap();
    let dirty = hutan_in(&t, &repo.root, &["git", "blame", "a.txt"])
        .await
        .ok();
    let last = dirty.stdout.lines().last().unwrap();
    assert!(last.starts_with("0000000 Not Committed Yet"), "{last}");
    assert!(last.ends_with("   5) five"), "{last}");

    // A revision blames what was committed there, with that revision's text.
    let old = hutan_in(
        &t,
        &repo.root,
        &["git", "blame", "a.txt", "--rev", "HEAD~2"],
    )
    .await
    .ok();
    let lines: Vec<&str> = old.stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{old:?}");
    assert!(lines[1].ends_with("   2) TWO"), "{}", lines[1]);

    let json = hutan_in(&t, &repo.root, &["--json", "git", "blame", "a.txt"])
        .await
        .json();
    assert_eq!(json["path"], "a.txt");
    assert_eq!(json["lines"], 5);
    assert!(json["hunks"].as_array().unwrap().len() >= 3);

    let missing = hutan_in(&t, &repo.root, &["git", "blame", "nope.txt"]).await;
    assert_ne!(missing.code, 0);
    assert!(
        missing.stderr.contains("not found") || missing.stderr.contains("not a tracked"),
        "{missing:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn show_prints_the_message_the_files_and_the_diff() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    let out = hutan_in(&t, &repo.root, &["git", "show", "HEAD~1"])
        .await
        .ok();
    assert!(out.stdout.starts_with("commit "), "{out:?}");
    assert!(
        out.stdout.contains("Author: Ada <ada@example.com>"),
        "{out:?}"
    );
    assert!(out.stdout.contains("    add four"), "{out:?}");
    assert!(out.stdout.contains("diff --git a/a.txt b/a.txt"), "{out:?}");
    assert!(out.stdout.contains("+four"), "{out:?}");

    let stat = hutan_in(&t, &repo.root, &["git", "show", "HEAD", "--stat"])
        .await
        .ok();
    assert!(
        stat.stdout.contains("R  old.txt -> new.txt  +0 -0"),
        "{stat:?}"
    );
    assert!(!stat.stdout.contains("diff --git"), "{stat:?}");

    let one = hutan_in(&t, &repo.root, &["git", "show", "HEAD~2", "a.txt"])
        .await
        .ok();
    assert!(one.stdout.contains("+TWO"), "{one:?}");

    let json = hutan_in(&t, &repo.root, &["--json", "git", "show", "HEAD~3"])
        .await
        .json();
    assert_eq!(json["commit"]["subject"], "create a");
    assert_eq!(json["files"].as_array().unwrap().len(), 2);
    assert!(json["parent"].is_null());
    t.stop().await;
}
