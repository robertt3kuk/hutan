//! `hutan wt new|ls|rm`, `hutan new --worktree` and the placement column of `hutan ls`, through
//! the real binary against a real in-process daemon. Every repository is created by the test in
//! a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::*;
use serde_json::Value;
use tokio::time::{sleep, timeout};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
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
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        Self { _dir: dir, root }
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join("repo-wt").join(name)
    }
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// `hutan` run from `cwd`.
async fn hutan_in(t: &TestDaemon, cwd: &Path, args: &[&str]) -> Out {
    let mut cmd = command(Path::new(HUTAN), Some(&t.paths.socket), args);
    cmd.current_dir(cwd);
    finish(cmd).await
}

async fn open_workspace(t: &TestDaemon, repo: &Path) -> String {
    t.hutan(&["project", "open", s(repo), "--json"])
        .await
        .ok()
        .json()["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Polls `hutan ls --json` until `pick` accepts the session.
async fn wait_session(
    t: &TestDaemon,
    id: &str,
    what: &str,
    pick: impl Fn(&Value) -> bool,
) -> Value {
    timeout(WAIT, async {
        loop {
            let session = t.session(id).await;
            if pick(&session) {
                return session;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("session {id}: {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_new_ls_and_rm_from_inside_the_repository() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    open_workspace(&t, &repo.root).await;

    // The workspace is found by the current directory; the path is printed for scripts.
    let created = hutan_in(&t, &repo.root.join("."), &["wt", "new", "feature/login"])
        .await
        .ok();
    assert_eq!(created.stdout.trim(), s(&repo.wt("feature-login")));
    assert!(repo.wt("feature-login").join("a.txt").is_file());

    let json = hutan_in(
        &t,
        &repo.root,
        &["wt", "new", "second", "--base", "main", "--json"],
    )
    .await
    .ok()
    .json();
    assert_eq!(json["worktree"]["branch"], "second");
    assert_eq!(json["worktree"]["created_by_hutan"], true);

    let listed = hutan_in(&t, &repo.root, &["wt", "ls", "--json"])
        .await
        .ok()
        .json();
    let worktrees = listed["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 3);
    assert_eq!(worktrees[0]["main"], true);
    assert_eq!(worktrees[0]["path"], s(&repo.root));

    let table = hutan_in(&t, &repo.root, &["wt", "ls"]).await.ok().stdout;
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 4, "{table}");
    assert!(lines[0].starts_with("BRANCH"), "{table}");
    assert!(
        lines[1].contains("main") && lines[1].contains(s(&repo.root)),
        "{table}"
    );
    assert!(
        lines[2].contains("feature/login") && lines[2].contains("hutan"),
        "{table}"
    );

    // Remove by branch, then by path; the branch survives unless asked.
    let removed = hutan_in(&t, &repo.root, &["wt", "rm", "feature/login"])
        .await
        .ok();
    assert_eq!(
        removed.stdout.trim(),
        format!("removed {}", s(&repo.wt("feature-login")))
    );
    assert!(!repo.wt("feature-login").exists());
    assert!(git(&repo.root, &["branch"]).contains("feature/login"));
    let removed = hutan_in(
        &t,
        &repo.root,
        &[
            "wt",
            "rm",
            s(&repo.wt("second")),
            "--delete-branch",
            "--json",
        ],
    )
    .await
    .ok()
    .json();
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["branch_kept"], Value::Null);
    assert!(!git(&repo.root, &["branch"]).contains("second"));
    let listed = hutan_in(&t, &repo.root, &["wt", "ls", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(listed["worktrees"].as_array().unwrap().len(), 1);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_reports_refusals_unmerged_branches_and_usage_errors() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    let ws = open_workspace(&t, &repo.root).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere = fs::canonicalize(elsewhere.path()).unwrap();

    // Home has no project: a usage error that says what to do.
    let home = fs::create_dir_all(&t.paths.default_home_folder)
        .map(|()| fs::canonicalize(&t.paths.default_home_folder).unwrap())
        .unwrap();
    let out = hutan_in(&t, &home, &["wt", "new", "x"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("belongs to no project"), "{out:?}");
    // A folder outside git is a project of its own, which has no repository.
    let out = hutan_in(&t, &elsewhere, &["wt", "new", "x"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("not inside a git repository"),
        "{out:?}"
    );
    // ... but --ws names it, and the main checkout cannot be removed.
    let out = hutan_in(&t, &elsewhere, &["wt", "rm", s(&repo.root), "--ws", &ws]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("main worktree"), "{out:?}");
    let out = hutan_in(&t, &elsewhere, &["wt", "rm", "no-such", "--ws", &ws]).await;
    assert_eq!(out.code, 1, "{out:?}");

    // A branch git would not take.
    let out = hutan_in(
        &t,
        &elsewhere,
        &["wt", "new", "a..b", "--ws", &ws, "--repo", s(&repo.root)],
    )
    .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("not a valid branch name"), "{out:?}");

    // Unmerged work: the worktree goes, the branch stays, and the answer says so.
    let path = hutan_in(
        &t,
        &elsewhere,
        &[
            "wt",
            "new",
            "wip",
            "--ws",
            &ws,
            "--repo",
            s(&repo.root),
            "--path",
            s(&elsewhere.join("wip-tree")),
        ],
    )
    .await
    .ok()
    .stdout;
    assert_eq!(path.trim(), s(&elsewhere.join("wip-tree")));
    let tree = elsewhere.join("wip-tree");
    fs::write(tree.join("new.txt"), "work\n").unwrap();
    git(&tree, &["add", "new.txt"]);
    git(&tree, &["commit", "-q", "-m", "work"]);
    fs::write(tree.join("dirty.txt"), "scratch\n").unwrap();
    let out = hutan_in(&t, &elsewhere, &["wt", "rm", "wip", "--ws", &ws]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("uncommitted"), "{out:?}");
    let out = hutan_in(
        &t,
        &elsewhere,
        &["wt", "rm", "wip", "--ws", &ws, "--force", "--delete-branch"],
    )
    .await
    .ok();
    assert!(out.stdout.contains("removed"), "{out:?}");
    assert!(out.stdout.contains("branch wip kept"), "{out:?}");
    assert!(git(&repo.root, &["branch"]).contains("wip"));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_worktree_starts_the_session_in_the_branchs_worktree_and_reuses_it() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    let ws = open_workspace(&t, &repo.root).await;

    let first = hutan_in(
        &t,
        &repo.root,
        &[
            "new",
            "--worktree",
            "topic",
            "--json",
            "--",
            "sh",
            "-c",
            "exec cat",
        ],
    )
    .await
    .ok()
    .json();
    let session = &first["session"];
    let id = session["id"].as_str().unwrap().to_owned();
    assert_eq!(session["cwd"], s(&repo.wt("topic")), "{first}");
    assert_eq!(session["workspace_id"], ws.as_str());
    assert_eq!(session["placement"]["kind"], "worktree");
    assert_eq!(session["placement"]["branch"], "topic");
    assert!(repo.wt("topic").join("a.txt").is_file());

    // The same branch again reuses the worktree: no second one, and both sessions live there.
    let second = hutan_in(
        &t,
        &repo.root,
        &[
            "new",
            "--worktree",
            "topic",
            "--json",
            "--",
            "sh",
            "-c",
            "exec cat",
        ],
    )
    .await
    .ok()
    .json();
    assert_eq!(second["session"]["cwd"], s(&repo.wt("topic")));
    let listed = hutan_in(&t, &repo.root, &["wt", "ls", "--json"])
        .await
        .ok()
        .json();
    let worktrees = listed["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 2);
    assert_eq!(worktrees[1]["live_sessions"].as_array().unwrap().len(), 2);

    // Both show it in `hutan ls`; a worktree with live sessions is not removed.
    let table = t.hutan(&["ls"]).await.ok().stdout;
    assert_eq!(table.matches("wt topic").count(), 2, "{table}");
    let out = hutan_in(&t, &repo.root, &["wt", "rm", "topic"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("live session"), "{out:?}");

    // The main checkout's branch cannot be made a worktree.
    let out = hutan_in(&t, &repo.root, &["new", "--worktree", "main", "--", "true"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("main checkout"), "{out:?}");
    let _ = id;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_shows_the_placement_and_warns_when_two_agents_share_a_checkout() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    let ws = open_workspace(&t, &repo.root).await;
    let plain = tempfile::tempdir().unwrap();
    let plain = fs::canonicalize(plain.path()).unwrap();

    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            t.new_session(
                &["--ws", &ws, "--cwd", s(&repo.root)],
                &["sh", "-c", "exec sleep 600"],
            )
            .await,
        );
    }
    let outside = t
        .new_session(&["--cwd", s(&plain)], &["sh", "-c", "exec sleep 600"])
        .await;
    let table = t.hutan(&["ls"]).await.ok().stdout;
    assert_eq!(table.matches("main").count(), 2, "{table}");
    assert!(!table.contains("shared"), "{table}");
    let outside_row = table.lines().find(|l| l.contains(&outside)).unwrap();
    assert!(
        !outside_row.contains("main"),
        "a session outside git has no badge: {table}"
    );

    // Two agents report: both rows warn, JSON lists who shares.
    for (index, id) in ids.iter().enumerate() {
        let source = format!("test-{index}");
        let mut cmd = command(
            Path::new(HUTAN),
            Some(&t.paths.socket),
            &[
                "agent", "report", "--source", &source, "--agent", "fake", "--state", "working",
                "--seq", "1",
            ],
        );
        cmd.env("HUTAN_SESSION_ID", id);
        finish(cmd).await.ok();
    }
    let first = ids[0].clone();
    let shared = wait_session(&t, &first, "never shared", |s| {
        !s["placement"]["shared_with"].as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(shared["placement"]["shared_with"][0], ids[1].as_str());
    let table = t.hutan(&["ls"]).await.ok().stdout;
    assert_eq!(table.matches("main ⚠ shared").count(), 2, "{table}");
    t.stop().await;
}

// Setup, teardown and cleanup (protocol 0.49) ---------------------------------------------------

/// Polls `hutan wt ls --json` from `cwd` until the worktree at `path` has a setup in `state`.
async fn wait_setup(t: &TestDaemon, cwd: &Path, path: &Path, state: &str) -> Value {
    timeout(WAIT, async {
        loop {
            let listed = hutan_in(t, cwd, &["wt", "ls", "--json"]).await.ok().json();
            let found = listed["worktrees"]
                .as_array()
                .unwrap()
                .iter()
                .find(|w| w["path"] == s(path))
                .and_then(|w| w.get("setup").cloned());
            if let Some(setup) = found
                && setup["state"] == state
            {
                return setup;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("setup of {} never reached {state}", path.display()))
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_create_is_wt_new_and_no_setup_skips_what_wt_setup_runs_later() {
    let t = TestDaemon::start_toml(
        "[worktrees.projects.repo]\nsetup = [\"touch ran\"]\ncopy = [\".env\"]\n",
        |_| {},
    )
    .await;
    let repo = Repo::new();
    fs::write(repo.root.join(".gitignore"), ".env\n").unwrap();
    git(&repo.root, &["add", ".gitignore"]);
    git(&repo.root, &["commit", "-q", "-m", "ignore"]);
    fs::write(repo.root.join(".env"), "A=1\n").unwrap();
    open_workspace(&t, &repo.root).await;

    let quiet = hutan_in(&t, &repo.root, &["wt", "create", "quiet", "--no-setup"])
        .await
        .ok();
    assert_eq!(quiet.stdout.trim(), s(&repo.wt("quiet")));
    assert!(!quiet.stderr.contains("setup"), "{}", quiet.stderr);
    assert!(!repo.wt("quiet").join(".env").exists());
    assert!(!repo.wt("quiet").join("ran").exists());

    // `wt setup` runs it afterwards, in a Setup session.
    let again = hutan_in(&t, &repo.root, &["wt", "setup", "quiet"])
        .await
        .ok();
    assert!(again.stdout.contains("copied 1 file"), "{}", again.stdout);
    wait_setup(&t, &repo.root, &repo.wt("quiet"), "done").await;
    assert_eq!(
        fs::read_to_string(repo.wt("quiet").join(".env")).unwrap(),
        "A=1\n"
    );
    assert!(repo.wt("quiet").join("ran").exists());

    // Without the flag, `wt new` does it at once and says so on stderr (the path stays alone on
    // stdout).
    let made = hutan_in(&t, &repo.root, &["wt", "new", "eager"]).await.ok();
    assert_eq!(made.stdout.trim(), s(&repo.wt("eager")));
    assert!(made.stderr.contains("copied 1 file"), "{}", made.stderr);
    wait_setup(&t, &repo.root, &repo.wt("eager"), "done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_setup_shows_in_wt_ls() {
    let t =
        TestDaemon::start_toml("[worktrees.projects.repo]\nsetup = [\"exit 3\"]\n", |_| {}).await;
    let repo = Repo::new();
    open_workspace(&t, &repo.root).await;
    hutan_in(&t, &repo.root, &["wt", "new", "bad"]).await.ok();
    let setup = wait_setup(&t, &repo.root, &repo.wt("bad"), "failed").await;
    assert!(setup["message"].as_str().unwrap().contains("code 3"));
    let table = hutan_in(&t, &repo.root, &["wt", "ls"]).await.ok().stdout;
    assert!(table.contains("setup failed"), "{table}");
}

#[tokio::test(flavor = "multi_thread")]
async fn commands_of_the_repository_need_yes_without_a_terminal() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    fs::create_dir(repo.root.join(".hutan")).unwrap();
    fs::write(
        repo.root.join(".hutan/worktree.toml"),
        "setup = [\"touch ran\"]\n",
    )
    .unwrap();
    git(&repo.root, &["add", ".hutan/worktree.toml"]);
    git(&repo.root, &["commit", "-q", "-m", "worktree config"]);
    open_workspace(&t, &repo.root).await;

    let made = hutan_in(&t, &repo.root, &["wt", "new", "feature/x"])
        .await
        .ok();
    assert!(
        made.stderr.contains("need your confirmation")
            && made.stderr.contains("hutan wt setup feature/x"),
        "{}",
        made.stderr
    );
    let table = hutan_in(&t, &repo.root, &["wt", "ls"]).await.ok().stdout;
    assert!(table.contains("setup needs confirmation"), "{table}");
    let wt = repo.wt("feature-x");
    assert!(!wt.join("ran").exists());

    // No terminal and no --yes: the commands are shown and nothing runs.
    let refused = hutan_in(&t, &repo.root, &["wt", "setup", "feature/x"]).await;
    assert_ne!(refused.code, 0);
    assert!(refused.stderr.contains("pass --yes"), "{}", refused.stderr);
    assert!(refused.stderr.contains("$ touch ran"), "{}", refused.stderr);
    assert!(!wt.join("ran").exists());

    hutan_in(&t, &repo.root, &["wt", "setup", "feature/x", "--yes"])
        .await
        .ok();
    wait_setup(&t, &repo.root, &wt, "done").await;
    assert!(wt.join("ran").exists());
    // The same commands in the next worktree run without a question.
    let next = hutan_in(&t, &repo.root, &["wt", "new", "feature/y"])
        .await
        .ok();
    assert!(
        !next.stderr.contains("need your confirmation"),
        "{}",
        next.stderr
    );
    wait_setup(&t, &repo.root, &repo.wt("feature-y"), "done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_prune_lists_merged_worktrees_and_removes_them_only_when_told() {
    let t = TestDaemon::start_toml("[worktrees]\ncleanup = \"merged\"\n", |_| {}).await;
    let repo = Repo::new();
    open_workspace(&t, &repo.root).await;
    let done = hutan_in(&t, &repo.root, &["wt", "new", "done"]).await.ok();
    let done_path = PathBuf::from(done.stdout.trim());
    hutan_in(&t, &repo.root, &["wt", "new", "open"]).await.ok();
    fs::write(done_path.join("d.txt"), "d\n").unwrap();
    git(&done_path, &["add", "d.txt"]);
    git(&done_path, &["commit", "-q", "-m", "d"]);
    git(
        &repo.root,
        &["merge", "-q", "--no-ff", "-m", "merge done", "done"],
    );

    let dry = hutan_in(&t, &repo.root, &["wt", "prune", "--dry-run"])
        .await
        .ok();
    assert!(
        dry.stdout.contains("done") && dry.stdout.contains("merged"),
        "{}",
        dry.stdout
    );
    assert!(!dry.stdout.contains("open"), "{}", dry.stdout);
    let json = hutan_in(&t, &repo.root, &["wt", "prune", "--dry-run", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["mode"], "merged");
    assert_eq!(json["candidates"].as_array().unwrap().len(), 1);
    assert!(done_path.exists());

    // Without a terminal it asks for --yes and removes nothing.
    let refused = hutan_in(&t, &repo.root, &["wt", "prune"]).await;
    assert_ne!(refused.code, 0);
    assert!(refused.stderr.contains("--yes"), "{}", refused.stderr);
    assert!(done_path.exists());

    let pruned = hutan_in(&t, &repo.root, &["wt", "prune", "--yes"])
        .await
        .ok();
    assert!(pruned.stdout.contains("removed"), "{}", pruned.stdout);
    assert!(!done_path.exists());
    assert!(repo.wt("open").exists());
    let after = hutan_in(&t, &repo.root, &["wt", "prune", "--dry-run"])
        .await
        .ok();
    assert!(
        after.stdout.contains("no merged worktree"),
        "{}",
        after.stdout
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_prune_says_how_to_turn_cleanup_on_while_the_policy_is_never() {
    let t = TestDaemon::start().await;
    let repo = Repo::new();
    open_workspace(&t, &repo.root).await;
    let out = hutan_in(&t, &repo.root, &["wt", "prune", "--dry-run"])
        .await
        .ok();
    assert!(out.stdout.contains("cleanup is never"), "{}", out.stdout);
    // The flags pick a policy for this run.
    let idle = hutan_in(
        &t,
        &repo.root,
        &["wt", "prune", "--dry-run", "--idle-days", "30"],
    )
    .await
    .ok();
    assert!(
        idle.stdout.contains("untouched for 30 day"),
        "{}",
        idle.stdout
    );
}
