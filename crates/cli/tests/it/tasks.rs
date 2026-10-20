//! `hutan task add|ls|start|done|review|cancel|rm|show` through the real binary against a real
//! in-process daemon. Every repository is created by the test in a temporary directory.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::*;
use serde_json::Value;
use tokio::time::{sleep, timeout};

fn git(dir: &Path, args: &[&str]) {
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

struct Env {
    t: TestDaemon,
    repo: Repo,
    workspace: String,
}

impl Env {
    async fn start() -> Self {
        let t = TestDaemon::start().await;
        let repo = Repo::new();
        let workspace = t
            .hutan(&["project", "open", repo.root.to_str().unwrap(), "--json"])
            .await
            .ok()
            .json()["workspace"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        Self { t, repo, workspace }
    }

    /// `hutan` run from inside the repository, so the workspace is found by the directory.
    async fn hutan(&self, args: &[&str]) -> Out {
        let mut cmd = command(Path::new(HUTAN), Some(&self.t.paths.socket), args);
        cmd.current_dir(&self.repo.root);
        finish(cmd).await
    }

    /// `task add` of `title` with `extra` arguments; the task's id.
    async fn add(&self, title: &str, extra: &[&str]) -> String {
        let mut args = vec!["task", "add", title];
        args.extend_from_slice(extra);
        let out = self.hutan(&args).await.ok();
        let id = out.stdout.trim().to_owned();
        assert_eq!(id.len(), 26, "the id alone on stdout: {out:?}");
        id
    }

    async fn tasks(&self) -> Vec<Value> {
        self.hutan(&["task", "ls", "--json"]).await.ok().json()["tasks"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn task(&self, id: &str) -> Value {
        self.tasks()
            .await
            .into_iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("task {id} not listed"))
    }

    async fn wait_state(&self, id: &str, state: &str) -> Value {
        let result = timeout(WAIT, async {
            loop {
                let task = self.task(id).await;
                if task["state"] == state {
                    return task;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        result.unwrap_or_else(|_| panic!("task {id} never reached {state}"))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn add_creates_tasks_with_dependencies_placement_and_run_and_ls_shows_the_tree() {
    let e = Env::start().await;
    let build = e.add("Build", &["--", "sh", "-c", "exit 0"]).await;
    let test = e
        .add(
            "Test it",
            &["--after", "Build", "--auto", "--", "sh", "-c", "exit 0"],
        )
        .await;
    let docs = e
        .add(
            "Write docs",
            &["--after", &build[..12], "--worktree", "--base", "main"],
        )
        .await;
    let review = e
        .add(
            "Review",
            &[
                "--after",
                "Test it",
                "--after",
                "Write docs",
                "--worktree=review/all",
                "--codex",
                "--prompt",
                "Look it over.",
            ],
        )
        .await;

    let tasks = e.tasks().await;
    let by = |id: &str| tasks.iter().find(|t| t["id"] == id).unwrap().clone();
    assert_eq!(by(&build)["state"], "ready");
    assert_eq!(
        by(&build)["run"],
        serde_json::json!({"kind": "terminal", "argv": ["sh", "-c", "exit 0"]})
    );
    assert_eq!(by(&test)["state"], "todo");
    assert_eq!(by(&test)["auto_start"], true);
    assert_eq!(by(&test)["depends_on"], serde_json::json!([build]));
    assert_eq!(
        by(&docs)["placement"],
        serde_json::json!({"kind": "worktree", "base": "main"})
    );
    assert_eq!(by(&docs)["run"], Value::Null);
    assert_eq!(by(&review)["placement"]["branch"], "review/all");
    assert_eq!(
        by(&review)["run"],
        serde_json::json!({"kind": "chat", "agent": "codex"})
    );
    assert_eq!(by(&review)["prompt"], "Look it over.");
    assert_eq!(by(&review)["depends_on"], serde_json::json!([test, docs]));

    // Dependencies first, each task once under the one it waits for last.
    let table = e.hutan(&["task", "ls"]).await.ok().stdout;
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 5, "{table}");
    assert!(
        lines[0].starts_with("TASK") && lines[0].contains("NOTES"),
        "{table}"
    );
    assert!(
        lines[1].starts_with("◎ Build") && lines[1].contains("ready"),
        "{table}"
    );
    assert!(
        lines[2].starts_with("├─ ○ Test it") && lines[2].contains("auto"),
        "{table}"
    );
    // Review waits for both; it hangs under the one that comes last.
    assert!(lines[3].starts_with("└─ ○ Write docs"), "{table}");
    assert!(lines[3].contains("worktree"), "{table}");
    assert!(lines[4].starts_with("   └─ ○ Review"), "{table}");
    assert!(lines[4].contains("also after Test it"), "{table}");
    assert!(
        lines[4].contains("worktree review/all") && lines[4].contains("chat codex"),
        "{table}"
    );
    // The shown id is what `start` and friends accept.
    let shown = lines[1]
        .split_whitespace()
        .find(|word| word.len() >= 4 && build.starts_with(*word))
        .unwrap();
    assert!(shown.len() >= 4, "{shown}");

    // `ls` is per workspace by default and `--ws` names one.
    let other = Repo::new();
    e.t.hutan(&["project", "open", other.root.to_str().unwrap()])
        .await
        .ok();
    let out = e.hutan(&["task", "ls", "--ws", &e.workspace]).await.ok();
    assert_eq!(out.stdout.lines().count(), 5);
    let mut cmd = command(
        Path::new(HUTAN),
        Some(&e.t.paths.socket),
        &["task", "ls", "--json"],
    );
    cmd.current_dir(&other.root);
    let elsewhere = finish(cmd).await.ok().json();
    assert_eq!(elsewhere["tasks"].as_array().unwrap().len(), 0);
    let none = finish({
        let mut cmd = command(Path::new(HUTAN), Some(&e.t.paths.socket), &["task", "ls"]);
        cmd.current_dir(&other.root);
        cmd
    })
    .await
    .ok();
    assert_eq!(none.stdout.trim(), "no tasks");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn start_runs_a_task_and_its_dependants_follow_done_cancel_and_rm_work() {
    let e = Env::start().await;
    let build = e.add("Build", &["--", "sh", "-c", "sleep 1; exit 0"]).await;
    let test = e
        .add("Test", &["--after", "Build", "--", "sh", "-c", "exit 0"])
        .await;
    let lint = e
        .add("Lint", &["--worktree", "--", "sh", "-c", "exec sleep 600"])
        .await;

    // Not ready: refused with what it waits for.
    let out = e.hutan(&["task", "start", "Test"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("waiting for `Build`"), "{out:?}");

    let started = e
        .hutan(&["task", "start", "Build", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(started["task"]["state"], "running");
    let session = started["session"]["id"].as_str().unwrap().to_owned();
    assert_eq!(started["session"]["cwd"], e.repo.root.to_str().unwrap());
    assert_eq!(started["warnings"], Value::Null);
    let done = e.wait_state(&build, "done").await;
    assert_eq!(done["outcome"], "exited 0");
    assert_eq!(done["sessions"], serde_json::json!([session]));
    e.wait_state(&test, "ready").await;
    let human = e.hutan(&["task", "start", &test]).await.ok().stdout;
    assert!(human.starts_with("started `Test`: session "), "{human}");
    e.wait_state(&test, "done").await;

    // A worktree task, then cancelled: its session stops.
    let out = e.hutan(&["task", "start", "Lint"]).await.ok();
    assert!(
        out.stdout
            .contains(e.repo.wt("task-lint").to_str().unwrap()),
        "{out:?}"
    );
    assert!(e.repo.wt("task-lint").join("a.txt").is_file());
    e.wait_state(&lint, "running").await;
    let cancelled = e.hutan(&["task", "cancel", "Lint"]).await.ok();
    assert_eq!(cancelled.stdout.trim(), "`Lint` is cancelled");
    let lint_task = e.task(&lint).await;
    assert_eq!(lint_task["outcome"], "cancelled");
    let lint_session = lint_task["sessions"][0].as_str().unwrap().to_owned();
    e.t.wait_state(&lint_session, "stopped").await;

    // `done` says it by hand; rm refuses a task others depend on, then cascades.
    let extra = e.add("Extra", &["--after", "Test"]).await;
    let out = e.hutan(&["task", "done", "Extra"]).await.ok();
    assert_eq!(out.stdout.trim(), "`Extra` is done");
    assert_eq!(e.task(&extra).await["outcome"], "marked done");
    let leaf = e.add("Leaf", &["--after", "Extra"]).await;
    let out = e.hutan(&["task", "rm", "Extra"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("`Leaf`") && out.stderr.contains("cascade"),
        "{out:?}"
    );
    let out = e.hutan(&["task", "rm", "Extra", "--cascade"]).await.ok();
    assert!(
        out.stdout.contains("removed `Extra` and 1 task(s)"),
        "{out:?}"
    );
    let ids: Vec<String> = e
        .tasks()
        .await
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(!ids.contains(&extra) && !ids.contains(&leaf));
    let out = e.hutan(&["task", "rm", "Lint", "--json"]).await.ok().json();
    assert_eq!(out["removed"], serde_json::json!([lint]));
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn show_prints_everything_about_a_task() {
    let e = Env::start().await;
    let build = e.add("Build", &["--", "sh", "-c", "exit 4"]).await;
    let test = e
        .add("Test", &["--after", "Build", "--prompt", "Run it.\nTwice."])
        .await;
    e.hutan(&["task", "start", "Build"]).await.ok();
    e.wait_state(&build, "failed").await;

    let text = e.hutan(&["task", "show", "Build"]).await.ok().stdout;
    assert!(text.starts_with("✗ Build\n"), "{text}");
    assert!(text.contains(&format!("id         {build}")), "{text}");
    assert!(text.contains("failed (exited 4)"), "{text}");
    assert!(text.contains("main checkout"), "{text}");
    assert!(text.contains(&format!("Test (todo, {test})")), "{text}");
    assert!(text.contains("sh -c exit 4"), "{text}");
    let session = e.task(&build).await["sessions"][0]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains(&format!("{session} (exited (4)")), "{text}");

    let text = e.hutan(&["task", "show", &test]).await.ok().stdout;
    assert!(text.contains(&format!("Build (failed, {build})")), "{text}");
    assert!(text.contains("prompt\n  Run it.\n  Twice."), "{text}");
    let json = e
        .hutan(&["task", "show", &test[..16], "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["task"]["id"], test.as_str());
    assert_eq!(json["depends_on"][0]["id"], build.as_str());
    assert_eq!(json["dependants"], serde_json::json!([]));
    let json = e
        .hutan(&["task", "show", "Build", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["dependants"][0]["id"], test.as_str());
    assert_eq!(json["sessions"][0]["id"], session.as_str());
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_targets_and_arguments_are_usage_errors() {
    let e = Env::start().await;
    let first = e.add("Same", &[]).await;
    let second = e.add("Same", &[]).await;

    // Two tasks with the title: a usage error that names both.
    let out = e.hutan(&["task", "show", "Same"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(
        out.stderr.contains(&first) && out.stderr.contains(&second),
        "{out:?}"
    );
    // An id prefix both share is ambiguous; a short one is not accepted; nothing matches.
    let out = e.hutan(&["task", "show", &first[..4]]).await;
    assert_eq!(out.code, 2, "{out:?}");
    let out = e.hutan(&["task", "show", &first[..3]]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("at least 4 characters"), "{out:?}");
    let out = e.hutan(&["task", "start", "nope"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("hutan task ls"), "{out:?}");
    // The full id always works.
    e.hutan(&["task", "show", &second]).await.ok();

    // A dependency that does not exist, a cycle-free self reference, bad combinations.
    let out = e.hutan(&["task", "add", "X", "--after", "ghost"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    let out = e.hutan(&["task", "add", "X", "--codex", "--", "sh"]).await;
    assert_eq!(out.code, 2, "clap: --codex and a command: {out:?}");
    let out = e.hutan(&["task", "add", "X", "--codex"]).await;
    assert_eq!(out.code, 2, "clap: --codex needs --prompt: {out:?}");
    let out = e.hutan(&["task", "add", "X", "--base", "main"]).await;
    assert_eq!(out.code, 2, "clap: --base needs --worktree: {out:?}");
    let out = e.hutan(&["task", "add", "  "]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("needs a title"), "{out:?}");
    assert_eq!(e.tasks().await.len(), 2);

    // In Home (or the home directory) there is no project to add to.
    let home = fs::create_dir_all(&e.t.paths.default_home_folder)
        .map(|()| fs::canonicalize(&e.t.paths.default_home_folder).unwrap())
        .unwrap();
    let mut cmd = command(
        Path::new(HUTAN),
        Some(&e.t.paths.socket),
        &["task", "add", "Lost"],
    );
    cmd.current_dir(home);
    let out = finish(cmd).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("belongs to no project"), "{out:?}");
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_runs_an_ended_task_again_and_refuses_a_running_one() {
    let e = Env::start().await;
    let build = e.add("Build", &["--", "sh", "-c", "exit 3"]).await;
    let test = e
        .add("Test", &["--after", "Build", "--", "sh", "-c", "exit 0"])
        .await;
    e.hutan(&["task", "start", "Build"]).await.ok();
    let failed = e.wait_state(&build, "failed").await;
    assert_eq!(failed["outcome"], "exited 3");
    let session = failed["sessions"][0].as_str().unwrap().to_owned();

    // A reset task is ready again with a clean slate; its first run stays in the history.
    let out = e.hutan(&["task", "reset", "Build"]).await.ok();
    assert_eq!(out.stdout.trim(), "`Build` is ready");
    let reset = e.task(&build).await;
    assert_eq!(reset["state"], "ready");
    for field in ["outcome", "ended_at_ms", "started_at_ms"] {
        assert_eq!(reset[field], Value::Null, "{field}");
    }
    assert_eq!(reset["sessions"], serde_json::json!([session]));
    assert_eq!(e.task(&test).await["state"], "todo");

    // Running again works, and a running task cannot be reset.
    let running = e.add("Long", &["--", "sh", "-c", "exec sleep 600"]).await;
    e.hutan(&["task", "start", "Long"]).await.ok();
    e.wait_state(&running, "running").await;
    let out = e.hutan(&["task", "reset", "Long"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("running"), "{out:?}");
    e.hutan(&["task", "cancel", "Long"]).await.ok();
    assert_eq!(
        e.hutan(&["task", "reset", "Long"]).await.ok().stdout.trim(),
        "`Long` is ready"
    );
    e.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn review_accepts_or_asks_for_changes_and_ls_shows_the_delivery() {
    let e = Env::start().await;
    let work = e
        .add("Work", &["--worktree", "--", "sh", "-c", "exit 0"])
        .await;
    let gated = e
        .add(
            "Gated",
            &[
                "--after",
                "Work",
                "--after-policy",
                "accepted",
                "--",
                "sh",
                "-c",
                "exit 0",
            ],
        )
        .await;
    assert_eq!(e.task(&gated).await["after_policy"], "accepted");
    let plain = e.add("Plain", &["--", "sh", "-c", "exit 0"]).await;

    // Not reviewable before it is done.
    let early = e.hutan(&["task", "review", "Work", "accept"]).await;
    assert_eq!(early.code, 1, "{early:?}");
    assert!(early.stderr.contains("only a done task"), "{early:?}");

    e.hutan(&["task", "start", "Work"]).await.ok();
    let done = e.wait_state(&work, "done").await;
    assert_eq!(done["delivery"], "awaiting_review");
    e.hutan(&["task", "start", "Plain"]).await.ok();
    e.wait_state(&plain, "done").await;

    let table = e.hutan(&["task", "ls"]).await.ok().stdout;
    let lines: Vec<&str> = table.lines().collect();
    assert!(
        lines[0].starts_with("TASK") && lines[0].contains("DELIVERY"),
        "{table}"
    );
    let row = |title: &str| lines.iter().find(|l| l.contains(title)).copied().unwrap();
    assert!(row("Work").contains("awaiting review"), "{table}");
    assert!(row("Gated").contains("after: accepted"), "{table}");
    let delivery_of = |line: &str| {
        let cut = lines[0].find("DELIVERY").unwrap();
        line.chars()
            .skip(cut)
            .take(17)
            .collect::<String>()
            .trim()
            .to_owned()
    };
    assert_eq!(delivery_of(row("Plain")), "-", "nothing to review: {table}");
    assert_eq!(e.task(&gated).await["state"], "todo");

    // Changes first (no chat to send the note to: it stays on the task), then accept.
    let out = e
        .hutan(&["task", "review", "Work", "changes", "-m", "Rename it."])
        .await
        .ok();
    assert!(
        out.stdout.contains("changes requested on `Work`")
            && out.stdout.contains("kept on the task"),
        "{out:?}"
    );
    let shown = e.hutan(&["task", "show", "Work"]).await.ok().stdout;
    assert!(
        shown.contains("changes requested (“Rename it.”)")
            && shown.contains("awaiting review by daemon")
            && shown.contains("changes requested by cli: Rename it."),
        "{shown}"
    );
    assert!(shown.contains("wait for   done dependencies"), "{shown}");
    assert_eq!(e.task(&gated).await["state"], "todo");

    let out = e
        .hutan(&["task", "review", "Work", "accept", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(out["task"]["delivery"], "accepted");
    assert!(out.get("session").is_none());
    assert_eq!(
        e.hutan(&["task", "review", "Work", "accept"])
            .await
            .ok()
            .stdout
            .trim(),
        "`Work` is accepted"
    );
    // Accepting released the task that waited for it.
    e.wait_state(&gated, "ready").await;
    let table = e.hutan(&["task", "ls"]).await.ok().stdout;
    assert!(
        table
            .lines()
            .find(|l| l.contains("Work"))
            .unwrap()
            .contains("accepted"),
        "{table}"
    );

    // Bad policies and decisions are usage errors.
    let out = e
        .hutan(&["task", "add", "X", "--after-policy", "later"])
        .await;
    assert_eq!(out.code, 2, "{out:?}");
    let out = e.hutan(&["task", "review", "Work", "maybe"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    e.t.stop().await;
}
