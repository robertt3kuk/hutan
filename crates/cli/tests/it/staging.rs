//! `hutan wt diff --hunks`, `wt stage|unstage|discard`, `wt commit --staged` and `hutan finish
//! --staged` through the real binary against a real in-process daemon. Every repository is
//! created by the test in a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;

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

fn numbered() -> String {
    (0..30).map(|n| format!("line {n:02}\n")).collect()
}

struct Repo {
    _dir: tempfile::TempDir,
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
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
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

async fn start() -> (TestDaemon, Repo) {
    let t = TestDaemon::start_with(|c| {
        c.checkpoints.git_env = std::collections::BTreeMap::from([
            ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ]);
    })
    .await;
    let repo = Repo::new();
    t.hutan(&["project", "open", s(&repo.root)]).await.ok();
    (t, repo)
}

fn edited() -> String {
    numbered()
        .replace("line 02\n", "line 02 changed\n")
        .replace("line 15\n", "line 15 changed\n")
        .replace("line 28\n", "line 28 changed\n")
}

/// The ids `wt diff --hunks` prints for `side` (`unstaged` or `staged`), in order.
fn ids(listing: &str, side: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some(side)).then(|| words.next().unwrap().to_owned())
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn hunks_are_listed_staged_and_unstaged_by_id_and_files_by_name() {
    let (t, repo) = start().await;
    fs::write(repo.root.join("a.txt"), edited()).unwrap();
    fs::write(repo.root.join("b.txt"), "beta\nmore\n").unwrap();
    let root = s(&repo.root);

    let listing = hutan_in(&t, &repo.root, &["wt", "diff", root, "--hunks"])
        .await
        .ok()
        .stdout;
    assert!(listing.contains("a.txt\n"), "{listing}");
    let unstaged = ids(&listing, "unstaged");
    assert_eq!(unstaged.len(), 4, "{listing}");
    assert!(listing.contains("@@ -"), "{listing}");

    // JSON carries the bodies.
    let json = hutan_in(&t, &repo.root, &["wt", "diff", root, "--hunks", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["files"][0]["path"], "a.txt", "{json}");
    assert!(
        json["files"][0]["unstaged"][1]["body"]
            .as_str()
            .unwrap()
            .contains("+line 15 changed"),
        "{json}"
    );

    // One hunk by id: the daemon finds its file.
    let middle = &unstaged[1];
    let out = hutan_in(&t, &repo.root, &["wt", "stage", root, "--hunk", middle])
        .await
        .ok();
    assert!(
        out.stdout.contains("staged 0 file(s) and 1 hunk(s)"),
        "{out:?}"
    );
    assert_eq!(
        git(&repo.root, &["show", ":a.txt"]),
        numbered().replace("line 15\n", "line 15 changed\n")
    );
    let after = hutan_in(&t, &repo.root, &["wt", "diff", root, "--hunks"])
        .await
        .ok()
        .stdout;
    let staged = ids(&after, "staged");
    assert_eq!(staged.len(), 1, "{after}");
    assert_eq!(ids(&after, "unstaged").len(), 3, "{after}");

    // A file by name (absolute paths inside the checkout work too), then all of it.
    hutan_in(&t, &repo.root, &["wt", "stage", root, "b.txt"])
        .await
        .ok();
    assert!(
        git(&repo.root, &["status", "--porcelain"]).contains("M  b.txt"),
        "{}",
        git(&repo.root, &["status", "--porcelain"])
    );
    hutan_in(
        &t,
        &repo.root,
        &["wt", "unstage", root, "--hunk", &staged[0]],
    )
    .await
    .ok();
    assert_eq!(git(&repo.root, &["show", ":a.txt"]), numbered());
    hutan_in(&t, &repo.root, &["wt", "stage", root, "--all"])
        .await
        .ok();
    assert_eq!(
        git(&repo.root, &["status", "--porcelain"]),
        "M  a.txt\nM  b.txt\n"
    );
    hutan_in(&t, &repo.root, &["wt", "unstage", root, "--all"])
        .await
        .ok();
    assert_eq!(
        git(&repo.root, &["status", "--porcelain"]),
        " M a.txt\n M b.txt\n"
    );

    // Lines of a hunk.
    let first = &ids(
        &hutan_in(&t, &repo.root, &["wt", "diff", root, "--hunks"])
            .await
            .ok()
            .stdout,
        "unstaged",
    )[0];
    let by_line = hutan_in(
        &t,
        &repo.root,
        &["wt", "stage", root, "--hunk", first, "--lines", "2,3"],
    )
    .await
    .ok();
    assert!(by_line.stdout.contains("1 hunk(s)"), "{by_line:?}");

    // Mistakes: nothing named, an unknown id, --lines without one hunk.
    let nothing = hutan_in(&t, &repo.root, &["wt", "stage", root]).await;
    assert_eq!(nothing.code, 2, "{nothing:?}");
    let unknown = hutan_in(
        &t,
        &repo.root,
        &["wt", "stage", root, "--hunk", "0123456789abcdef"],
    )
    .await;
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert!(
        unknown.stderr.contains("no hunk 0123456789abcdef"),
        "{unknown:?}"
    );
    let lines = hutan_in(
        &t,
        &repo.root,
        &["wt", "stage", root, "b.txt", "--lines", "1"],
    )
    .await;
    assert_eq!(lines.code, 2, "{lines:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn discard_asks_unless_told_prints_the_undo_and_the_undo_works() {
    let (t, repo) = start().await;
    fs::write(repo.root.join("a.txt"), edited()).unwrap();
    fs::write(repo.root.join("new.txt"), "fresh\n").unwrap();
    let root = s(&repo.root);

    // No terminal and no --yes: nothing is discarded.
    let refused = hutan_in(&t, &repo.root, &["wt", "discard", root, "a.txt"]).await;
    assert_eq!(refused.code, 2, "{refused:?}");
    assert!(refused.stderr.contains("--yes"), "{refused:?}");
    assert_eq!(
        fs::read_to_string(repo.root.join("a.txt")).unwrap(),
        edited()
    );

    let out = hutan_in(
        &t,
        &repo.root,
        &["wt", "discard", root, "a.txt", "new.txt", "--yes"],
    )
    .await
    .ok();
    assert!(
        out.stdout.contains("discarded 2 file(s) and 0 hunk(s)"),
        "{out:?}"
    );
    let undo = out
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("undo: "))
        .unwrap()
        .to_owned();
    assert!(undo.starts_with("hutan checkpoint restore "), "{out:?}");
    assert_eq!(
        fs::read_to_string(repo.root.join("a.txt")).unwrap(),
        numbered()
    );
    assert!(!repo.root.join("new.txt").exists());

    let id = undo.rsplit(' ').next().unwrap();
    hutan_in(&t, &repo.root, &["checkpoint", "restore", id, "--yes"])
        .await
        .ok();
    assert_eq!(
        fs::read_to_string(repo.root.join("a.txt")).unwrap(),
        edited()
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("new.txt")).unwrap(),
        "fresh\n"
    );

    // A hunk, by id.
    let listing = hutan_in(&t, &repo.root, &["wt", "diff", root, "--hunks"])
        .await
        .ok()
        .stdout;
    let unstaged = ids(&listing, "unstaged");
    hutan_in(
        &t,
        &repo.root,
        &["wt", "discard", root, "--hunk", &unstaged[2], "--yes"],
    )
    .await
    .ok();
    assert_eq!(
        fs::read_to_string(repo.root.join("a.txt")).unwrap(),
        edited().replace("line 28 changed\n", "line 28\n")
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn commit_and_finish_take_only_what_is_staged_with_staged() {
    let (t, repo) = start().await;
    fs::write(repo.root.join("a.txt"), edited()).unwrap();
    fs::write(repo.root.join("b.txt"), "beta\nmore\n").unwrap();
    let root = s(&repo.root);

    let nothing = hutan_in(
        &t,
        &repo.root,
        &["wt", "commit", root, "-m", "x", "--staged"],
    )
    .await;
    assert_eq!(nothing.code, 1, "{nothing:?}");
    assert!(nothing.stderr.contains("nothing is staged"), "{nothing:?}");

    hutan_in(&t, &repo.root, &["wt", "stage", root, "b.txt"])
        .await
        .ok();
    let out = hutan_in(
        &t,
        &repo.root,
        &["wt", "commit", root, "-m", "Only b", "--staged"],
    )
    .await
    .ok();
    assert!(
        out.stdout.starts_with("committed 1 file(s) on main ("),
        "{out:?}"
    );
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%s"]).trim(),
        "Only b"
    );
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), " M a.txt\n");

    hutan_in(&t, &repo.root, &["wt", "stage", root, "a.txt"])
        .await
        .ok();
    let done = hutan_in(
        &t,
        &repo.root,
        &["finish", root, "--staged", "--no-push", "-m", "Only a"],
    )
    .await
    .ok();
    assert!(done.stdout.contains("commit"), "{done:?}");
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%s"]).trim(),
        "Only a"
    );
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    t.stop().await;
}
