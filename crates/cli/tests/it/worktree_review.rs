//! `hutan wt diff` and `hutan wt merge` through the real binary against a real in-process
//! daemon. Every repository is created by the test in a temporary directory.

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

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
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
        ] {
            git(&root, &["config", key, value]);
        }
        fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        commit_all(&root, "first");
        Self { _dir: dir, root }
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join("repo-wt").join(name)
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

/// A workspace on a fresh repository and a worktree `feature/x` with one commit, one staged
/// rename-free edit and one untracked file.
async fn setup(t: &TestDaemon) -> Repo {
    let repo = Repo::new();
    t.hutan(&["project", "open", s(&repo.root)]).await.ok();
    hutan_in(t, &repo.root, &["wt", "new", "feature/x"])
        .await
        .ok();
    let wt = repo.wt("feature-x");
    fs::write(wt.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(wt.join("b.txt"), "b1\nb2\n").unwrap();
    commit_all(&wt, "feature work");
    fs::write(wt.join("untracked.txt"), "u\n").unwrap();
    repo
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_diff_prints_the_patch_the_stat_and_json() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    // By branch, from inside the repository: the whole patch, uncommitted file included.
    let patch = hutan_in(&t, &repo.root, &["wt", "diff", "feature/x"])
        .await
        .ok()
        .stdout;
    assert!(patch.contains("diff --git a/a.txt b/a.txt"), "{patch}");
    assert!(patch.contains("+three"), "{patch}");
    assert!(patch.contains("diff --git a/b.txt b/b.txt"), "{patch}");
    assert!(patch.contains("+++ b/untracked.txt"), "{patch}");
    assert!(!patch.contains('\u{1b}'), "no colors");

    // By path, committed changes only.
    let committed = hutan_in(
        &t,
        &repo.root,
        &["wt", "diff", s(&repo.wt("feature-x")), "--committed"],
    )
    .await
    .ok()
    .stdout;
    assert!(
        committed.contains("b.txt") && !committed.contains("untracked.txt"),
        "{committed}"
    );

    let stat = hutan_in(&t, &repo.root, &["wt", "diff", "feature/x", "--stat"])
        .await
        .ok()
        .stdout;
    let lines: Vec<&str> = stat.lines().collect();
    assert_eq!(lines.len(), 7, "{stat}");
    assert!(
        lines[0].starts_with("M a.txt") && lines[0].ends_with("+1 -0"),
        "{stat}"
    );
    assert!(
        lines[1].starts_with("A b.txt") && lines[1].ends_with("+2 -0"),
        "{stat}"
    );
    assert!(
        lines[2].starts_with("A untracked.txt") && lines[2].ends_with("(uncommitted)"),
        "{stat}"
    );
    assert!(
        lines[3].starts_with("3 file(s) changed, 4 insertion(s)(+), 0 deletion(s)(-) against main"),
        "{stat}"
    );
    // What a merge takes, and what it leaves behind.
    assert_eq!(lines[4], "a merge takes 1 commit(s):", "{stat}");
    assert!(lines[5].ends_with(" feature work"), "{stat}");
    assert!(
        lines[6].starts_with("1 file(s) with uncommitted changes: a merge leaves them behind")
            && lines[6].contains("hutan wt commit feature/x -m MSG"),
        "{stat}"
    );

    let json = hutan_in(&t, &repo.root, &["wt", "diff", "feature/x", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["base"], "main");
    assert_eq!(json["files"].as_array().unwrap().len(), 3);
    assert_eq!(json["files"][0]["status"], "modified");
    assert_eq!(json["commits"].as_array().unwrap().len(), 1);
    assert_eq!(json["files"][2]["uncommitted"], true);
    assert!(json["files"][2].get("committed").is_none());
    let json_stat = hutan_in(
        &t,
        &repo.root,
        &["wt", "diff", "feature/x", "--stat", "--json"],
    )
    .await
    .ok()
    .json();
    assert!(json_stat["patch"].is_null());

    // A branch that is no worktree, a base that is no ref.
    let missing = hutan_in(&t, &repo.root, &["wt", "diff", "nope"]).await;
    assert_eq!(missing.code, 1, "{missing:?}");
    assert!(
        missing.stderr.contains("no worktree matches"),
        "{missing:?}"
    );
    let bad_base = hutan_in(
        &t,
        &repo.root,
        &["wt", "diff", "feature/x", "--base", "nowhere"],
    )
    .await;
    assert_eq!(bad_base.code, 1, "{bad_base:?}");
    assert!(
        bad_base.stderr.contains("not a branch or commit"),
        "{bad_base:?}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_merge_merges_squashes_and_rebases_and_reports_refusals() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    // The main checkout has an untracked file: refused, nothing changes.
    fs::write(repo.root.join("stray.txt"), "x\n").unwrap();
    let refused = hutan_in(&t, &repo.root, &["wt", "merge", "feature/x"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(
        refused.stderr.contains("uncommitted changes"),
        "{refused:?}"
    );
    fs::remove_file(repo.root.join("stray.txt")).unwrap();

    // Squash with a message; the untracked file in the worktree is reported.
    let out = hutan_in(
        &t,
        &repo.root,
        &["wt", "merge", "feature/x", "--squash", "-m", "Add b"],
    )
    .await
    .ok();
    assert!(
        out.stdout
            .starts_with("squash-merged feature/x into main ("),
        "{out:?}"
    );
    assert!(
        out.stdout.contains("still has uncommitted changes")
            && out.stdout.contains("would discard them"),
        "{out:?}"
    );
    assert!(out.stdout.contains("hutan wt rm"), "{out:?}");
    assert_eq!(
        git(&repo.root, &["log", "-1", "--format=%s"]).trim(),
        "Add b"
    );
    assert_eq!(
        fs::read_to_string(repo.root.join("b.txt")).unwrap(),
        "b1\nb2\n"
    );

    // A second branch with a conflict: the files are listed and nothing changed.
    hutan_in(&t, &repo.root, &["wt", "new", "feature/y"])
        .await
        .ok();
    let y = repo.wt("feature-y");
    fs::write(y.join("a.txt"), "y side\n").unwrap();
    commit_all(&y, "y edit");
    fs::write(repo.root.join("a.txt"), "main side\n").unwrap();
    commit_all(&repo.root, "main edit");
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    let conflict = hutan_in(&t, &repo.root, &["wt", "merge", "feature/y"]).await;
    assert_eq!(conflict.code, 1, "{conflict:?}");
    assert!(
        conflict.stderr.contains("conflicts in 1 file(s)"),
        "{conflict:?}"
    );
    assert!(conflict.stderr.contains("conflict: a.txt"), "{conflict:?}");
    assert_eq!(git(&repo.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");

    // A rebase of a third branch, as JSON.
    hutan_in(&t, &repo.root, &["wt", "new", "feature/z"])
        .await
        .ok();
    let z = repo.wt("feature-z");
    fs::write(z.join("z.txt"), "z\n").unwrap();
    commit_all(&z, "z work");
    fs::write(repo.root.join("main.txt"), "m\n").unwrap();
    commit_all(&repo.root, "main moves");
    let json = hutan_in(
        &t,
        &repo.root,
        &["wt", "merge", "feature/z", "--rebase", "--json"],
    )
    .await
    .ok()
    .json();
    assert_eq!(json["strategy"], "rebase");
    assert_eq!(json["target"], "main");
    assert_eq!(json["branch"], "feature/z");
    assert_eq!(
        json["commit"].as_str().unwrap(),
        git(&repo.root, &["rev-parse", "HEAD"]).trim()
    );
    let log = git(&repo.root, &["log", "--format=%s", "-2"]);
    assert_eq!(log, "z work\nmain moves\n");

    // A plain merge by path; the message of git's own.
    hutan_in(&t, &repo.root, &["wt", "new", "feature/w"])
        .await
        .ok();
    let w = repo.wt("feature-w");
    fs::write(w.join("w.txt"), "w\n").unwrap();
    commit_all(&w, "w work");
    fs::write(repo.root.join("main2.txt"), "m2\n").unwrap();
    commit_all(&repo.root, "main moves again");
    let out = hutan_in(&t, &repo.root, &["wt", "merge", s(&w)]).await.ok();
    assert!(
        out.stdout.starts_with("merged feature/w into main ("),
        "{out:?}"
    );
    assert!(!out.stdout.contains("uncommitted"), "{out:?}");
    assert!(git(&repo.root, &["log", "-1", "--format=%s"]).starts_with("Merge branch 'feature/w'"));

    // `--squash` and `--rebase` are exclusive; so are `-m` and `--rebase`.
    let both = hutan_in(
        &t,
        &repo.root,
        &["wt", "merge", "x", "--squash", "--rebase"],
    )
    .await;
    assert_eq!(both.code, 2, "{both:?}");
    let msg = hutan_in(&t, &repo.root, &["wt", "merge", "x", "--rebase", "-m", "m"]).await;
    assert_eq!(msg.code, 2, "{msg:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_commit_commits_everything_so_the_merge_takes_it() {
    let t = TestDaemon::start().await;
    let repo = setup(&t).await;

    // The message is required; nothing to commit is a refusal.
    let none = hutan_in(&t, &repo.root, &["wt", "commit", "feature/x"]).await;
    assert_eq!(none.code, 2, "clap: -m is required: {none:?}");

    let out = hutan_in(
        &t,
        &repo.root,
        &["wt", "commit", "feature/x", "-m", "Finish it"],
    )
    .await
    .ok();
    assert!(
        out.stdout.starts_with("committed 1 file(s) on feature/x ("),
        "{out:?}"
    );
    let wt = repo.wt("feature-x");
    assert_eq!(git(&wt, &["log", "-1", "--format=%s"]).trim(), "Finish it");
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    let again = hutan_in(
        &t,
        &repo.root,
        &["wt", "commit", "feature/x", "-m", "Again"],
    )
    .await;
    assert_eq!(again.code, 1, "{again:?}");
    assert!(again.stderr.contains("nothing to commit"), "{again:?}");

    // The stat says there is nothing left behind, and the merge leaves no leftovers.
    let stat = hutan_in(&t, &repo.root, &["wt", "diff", "feature/x", "--stat"])
        .await
        .ok()
        .stdout;
    assert!(!stat.contains("uncommitted"), "{stat}");
    assert!(stat.contains("a merge takes 2 commit(s):"), "{stat}");
    let merged = hutan_in(&t, &repo.root, &["wt", "merge", "feature/x"])
        .await
        .ok();
    assert!(!merged.stdout.contains("uncommitted"), "{merged:?}");
    assert!(repo.root.join("untracked.txt").is_file());
    let json = hutan_in(
        &t,
        &repo.root,
        &["wt", "commit", "feature/x", "-m", "x", "--json"],
    )
    .await;
    assert_eq!(
        json.code, 1,
        "the branch is merged, nothing to commit: {json:?}"
    );
    t.stop().await;
}

/// `hutan finish` in a main checkout whose `main` tracks a local bare `origin`: `--no-push`
/// keeps the commit local, a plain `hutan finish` pushes it, and a run with nothing left says so.
#[tokio::test(flavor = "multi_thread")]
async fn finish_commits_then_pushes_unless_told_not_to() {
    let t = TestDaemon::start_toml("[terminal]\nenv_inherit = \"daemon\"\n", |_| {}).await;
    let repo = Repo::new();
    let bare = repo.root.parent().unwrap().join("origin.git");
    git(
        repo.root.parent().unwrap(),
        &["init", "-q", "--bare", "-b", "main", s(&bare)],
    );
    git(&repo.root, &["remote", "add", "origin", s(&bare)]);
    git(&repo.root, &["push", "-q", "-u", "origin", "main"]);
    t.hutan(&["project", "open", s(&repo.root)]).await.ok();
    let remote_main = || git(&bare, &["rev-parse", "main"]).trim().to_owned();
    let before = remote_main();

    fs::write(repo.root.join("a.txt"), "local\n").unwrap();
    let local = hutan_in(&t, &repo.root, &["finish", "--no-push", "-m", "Local"])
        .await
        .ok();
    assert!(
        local.stdout.contains("commit: committed 1 file on main")
            && local.stdout.contains("not pushed"),
        "{local:?}"
    );
    assert_eq!(remote_main(), before);

    // Without a message there is nothing to commit; the push takes the commit made above.
    let pushed = hutan_in(&t, &repo.root, &["finish", "--json"]).await.ok();
    let json = pushed.json();
    assert_eq!(json["commit"]["reason"], "nothing_to_commit", "{json}");
    assert_eq!(json["push"]["status"], "ok", "{json}");
    assert_eq!(json["push"]["upstream"], "origin/main");
    assert_eq!(
        remote_main(),
        git(&repo.root, &["rev-parse", "HEAD"]).trim()
    );

    // Uncommitted changes need a message.
    fs::write(repo.root.join("a.txt"), "again\n").unwrap();
    let refused = hutan_in(&t, &repo.root, &["finish"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("needs a message"), "{refused:?}");
    t.stop().await;
}
