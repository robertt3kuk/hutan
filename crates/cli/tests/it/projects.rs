//! `hutan project ls|open|rename|pin|unpin|close|recent` and what the session commands show of
//! projects (roadmap M18-A, protocol 0.19), through the real binary against a real in-process
//! daemon. Repositories and folders are temporary directories; git runs only in them.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use tokio::time::sleep;

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn mkdir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    path.to_owned()
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

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

fn repo(base: &Path, name: &str) -> PathBuf {
    let root = mkdir(&base.join(name));
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
    root
}

/// `hutan` run from `cwd`.
async fn hutan_in(t: &TestDaemon, cwd: &Path, args: &[&str]) -> Out {
    let mut cmd = command(Path::new(HUTAN), Some(&t.paths.socket), args);
    cmd.current_dir(cwd);
    finish(cmd).await
}

async fn projects(t: &TestDaemon, args: &[&str]) -> Vec<Value> {
    let mut full = vec!["project", "ls", "--json"];
    full.extend_from_slice(args);
    t.hutan(&full).await.ok().json()["projects"]
        .as_array()
        .unwrap()
        .clone()
}

async fn names(t: &TestDaemon, args: &[&str]) -> Vec<String> {
    projects(t, args)
        .await
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn project_open_makes_a_project_once_and_finds_it_again() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let plain = mkdir(&base.join("plain"));
    let inner = mkdir(&plain.join("src/deep"));

    let created = t
        .hutan(&["project", "open", s(&plain), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(created["created"], true);
    let project = &created["workspace"];
    assert_eq!(project["name"], "plain");
    assert_eq!(project["roots"], json!([s(&plain)]));
    assert_eq!(project["kind"], "project");
    assert_eq!(project["created_by"], "cli");
    assert_eq!(project["project"]["root"], s(&plain));
    let id = project["id"].as_str().unwrap().to_owned();

    // The human form says what happened; a folder below a project's folder is that project.
    let human = t.hutan(&["project", "open", s(&inner)]).await.ok();
    assert_eq!(human.stdout.trim(), format!("opened {id} (plain)"));

    // A relative folder is the caller's; a folder inside a repository opens its project.
    let app = repo(&base, "clone");
    mkdir(&app.join("crates/x"));
    let out = hutan_in(&t, &app, &["project", "open", "crates/x", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(out["created"], true);
    assert_eq!(out["workspace"]["roots"], json!([s(&app)]));
    assert_eq!(out["workspace"]["name"], "clone");
    let again = hutan_in(&t, &app, &["project", "open", ".", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(again["created"], false);
    assert_eq!(again["workspace"]["id"], out["workspace"]["id"]);

    // Failures.
    let out = t
        .hutan(&["project", "open", s(&base.join("missing"))])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.starts_with("hutan: not_found:"), "{out:?}");
    assert_eq!(t.hutan(&["project", "open"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_of_a_repository_and_its_worktree_share_one_project_in_ls() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let app = repo(&base, "app");
    let wt = base.join("app-wt");
    git(&app, &["worktree", "add", "-q", "-b", "feature", s(&wt)]);
    let sub = mkdir(&app.join("src"));

    // No workspace named: each session goes to the project of its folder.
    let main = t.new_session(&["--cwd", s(&app)], &["cat"]).await;
    let in_sub = t.new_session(&["--cwd", s(&sub)], &["cat"]).await;
    let in_wt = t.new_session(&["--cwd", s(&wt)], &["cat"]).await;
    // From inside the worktree too: the current directory decides.
    let from_wt = hutan_in(&t, &wt, &["new", "--json", "--", "cat"])
        .await
        .ok()
        .json()["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sessions = t.sessions().await;
    let workspace_of =
        |id: &str| sessions.iter().find(|s| s["id"] == id).unwrap()["workspace_id"].clone();
    assert_eq!(workspace_of(&in_sub), workspace_of(&main));
    assert_eq!(workspace_of(&in_wt), workspace_of(&main));
    assert_eq!(workspace_of(&from_wt), workspace_of(&main));
    assert_eq!(names(&t, &["--all"]).await, ["app"], "one project");

    // `hutan ls` shows the project, the placement and the folder.
    let table = t.hutan(&["ls"]).await.ok().stdout;
    let header: Vec<_> = table.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(
        header,
        [
            "ID", "TERMINAL", "STATE", "RECOVERY", "PROJECT", "PLACE", "TITLE", "FOLDER", "AGE"
        ]
    );
    let row = |id: &str| {
        table
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap()
            .to_owned()
    };
    assert!(
        row(&main).contains("app") && row(&main).contains("main"),
        "{table}"
    );
    assert!(
        row(&in_wt).contains("wt feature") && row(&in_wt).contains(s(&wt)),
        "{table}"
    );
    assert!(row(&in_sub).contains(s(&sub)), "{table}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn project_ls_hides_empty_projects_unless_pinned_or_in_a_workspace() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let busy = mkdir(&base.join("busy"));
    let idle = mkdir(&base.join("idle"));
    let pinned = mkdir(&base.join("pinned"));
    let member = mkdir(&base.join("member"));
    t.new_session(&["--cwd", s(&busy)], &["cat"]).await;
    for folder in [&idle, &pinned] {
        t.hutan(&["project", "open", s(folder)]).await.ok();
    }
    t.hutan(&["project", "pin", "pinned"]).await.ok();
    t.hutan(&["ws", "new", "shop", s(&member)]).await.ok();

    assert_eq!(names(&t, &[]).await, ["pinned", "busy", "member"]);
    assert_eq!(
        names(&t, &["--all"]).await,
        ["pinned", "busy", "idle", "member"]
    );
    // The table names each project's workspace and folder.
    let human = t.hutan(&["project", "ls"]).await.ok().stdout;
    let lines: Vec<_> = human.lines().collect();
    assert_eq!(lines.len(), 4, "{human}");
    assert!(
        lines[0].contains("WORKSPACE") && lines[0].contains("FOLDER"),
        "{human}"
    );
    let member_line = lines.iter().find(|l| l.contains("member")).unwrap();
    assert!(
        member_line.contains("shop") && member_line.ends_with(s(&member)),
        "{human}"
    );
    let busy_line = lines.iter().find(|l| l.contains("busy")).unwrap();
    assert!(busy_line.contains("1/1"), "{human}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn project_rename_pin_close_and_recent() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let one = mkdir(&base.join("one"));
    let two = mkdir(&base.join("two"));
    let id_one = t
        .hutan(&["project", "open", s(&one), "--json"])
        .await
        .ok()
        .json()["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sleep(Duration::from_millis(10)).await;
    t.hutan(&["project", "open", s(&two)]).await.ok();

    let renamed = t
        .hutan(&["project", "rename", "two", "second", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(renamed["name"], "second");
    let pinned = t
        .hutan(&["project", "pin", "second", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(pinned["pinned"], true);
    let human = t.hutan(&["project", "unpin", "second"]).await.ok();
    assert!(human.stdout.starts_with("unpinned "), "{human:?}");
    // Home is no project: every project command says so.
    for args in [
        &["project", "rename", "Home", "x"][..],
        &["project", "pin", "Home"],
        &["project", "close", "Home"],
    ] {
        let out = t.hutan(args).await;
        assert_eq!(out.code, 1, "{args:?}: {out:?}");
        assert!(
            out.stderr.contains("the Home workspace, not a project"),
            "{out:?}"
        );
    }

    // Closing keeps a project in `recent`; one with live sessions is refused.
    let live = t.new_session(&["--ws", &id_one], &["cat"]).await;
    let out = t.hutan(&["project", "close", "one"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("invalid_state"), "{out:?}");
    t.hutan(&["stop", &live]).await.ok();
    let closed = t
        .hutan(&["project", "close", "one", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(closed, json!({"workspace_id": id_one, "closed": true}));
    assert_eq!(names(&t, &["--all"]).await, ["second"]);

    // Newest first: `one` was just closed, `second` was opened before.
    let out = t.hutan(&["project", "recent", "--json"]).await.ok().json();
    let recent = out["projects"].as_array().unwrap();
    assert_eq!(recent.len(), 2, "{out}");
    assert_eq!(recent[0]["name"], "one");
    assert_eq!(recent[0]["path"], s(&one));
    assert!(recent[0].get("workspace_id").is_none(), "closed");
    assert_eq!(recent[1]["name"], "second");
    assert!(recent[1]["workspace_id"].is_string(), "open");
    let human = t.hutan(&["project", "recent"]).await.ok().stdout;
    let lines: Vec<_> = human.lines().collect();
    assert_eq!(lines.len(), 3, "{human}");
    assert!(
        lines[0].starts_with("NAME") && lines[0].contains("PATH"),
        "{human}"
    );
    assert!(
        lines[1].starts_with("one") && lines[1].contains("closed"),
        "{human}"
    );
    let out = t
        .hutan(&["project", "recent", "--limit", "1", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(out["projects"].as_array().unwrap().len(), 1);
    assert_eq!(
        t.hutan(&["project", "recent", "--limit", "0"]).await.code,
        2
    );

    // Reopening a closed project brings it back under the name it had.
    let again = t
        .hutan(&["project", "open", s(&one), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(again["created"], true);
    assert_eq!(again["workspace"]["name"], "one");

    // Nothing remembered on another daemon.
    t.stop().await;
    let t = TestDaemon::start().await;
    assert_eq!(
        t.hutan(&["project", "recent"]).await.ok().stdout.trim(),
        "no recent projects"
    );
    assert_eq!(
        t.hutan(&["project", "ls"]).await.ok().stdout.trim(),
        "no projects"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wt_and_task_find_the_project_of_a_linked_worktree() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let app = repo(&base, "app");
    let wt = base.join("app-wt");
    git(&app, &["worktree", "add", "-q", "-b", "feature", s(&wt)]);

    // Neither command was told a project: the directory decides, through the repository, so a
    // worktree outside the main checkout's folder still finds the project of its checkout.
    let listed = hutan_in(&t, &wt, &["wt", "ls", "--json"]).await.ok().json();
    let branches: Vec<_> = listed["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["branch"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(branches, ["main", "feature"]);
    let id = hutan_in(&t, &wt, &["task", "add", "From the worktree"])
        .await
        .ok()
        .stdout
        .trim()
        .to_owned();
    let tasks = hutan_in(&t, &app, &["task", "ls", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(
        tasks["tasks"][0]["id"], id,
        "the same project from the main checkout"
    );
    assert_eq!(names(&t, &["--all"]).await, ["app"]);

    // Home's folder is no project: the commands stop with a hint instead of making one.
    let home = mkdir(&t.paths.default_home_folder);
    let out = hutan_in(&t, &home, &["task", "add", "Nowhere"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    assert!(out.stderr.contains("belongs to no project"), "{out:?}");
    t.stop().await;
}

/// A configuration file with a git identity (the user's own, for the first commit).
fn identity(dir: &Path) -> PathBuf {
    let path = dir.join("gitconfig");
    fs::write(
        &path,
        "[user]\n\tname = Test\n\temail = test@example.com\n[commit]\n\tgpgsign = false\n",
    )
    .unwrap();
    path
}

fn commits(dir: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-list", "--count", "HEAD"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn project_git_init_tracks_the_home_folder_and_a_plain_project_with_the_daemons_refusals() {
    let (_keep, base) = tmp();
    let home_folder = mkdir(&base.join("Hutan"));
    fs::write(home_folder.join("notes.md"), "# notes\n").unwrap();
    let config = identity(&base);
    let folder = home_folder.clone();
    let t = TestDaemon::start_with(move |c| {
        c.checkpoints.git_env = std::collections::BTreeMap::from([
            ("GIT_CONFIG_GLOBAL".to_owned(), config.display().to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ]);
        c.paths.default_home_folder = folder;
    })
    .await;

    // Without a terminal it asks for --yes and changes nothing; neither does a bare call run.
    let out = t.hutan(&["project", "git-init", "--home"]).await;
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("--yes"), "{}", out.stderr);
    assert!(!home_folder.join(".git").exists());
    assert_ne!(t.hutan(&["project", "git-init"]).await.code, 0);
    assert_ne!(
        t.hutan(&["project", "git-init", "--home", "lab", "--yes"])
            .await
            .code,
        0,
        "a project and --home exclude each other"
    );

    // --yes: the daemon does it.
    let done = t
        .hutan(&["project", "git-init", "--home", "--yes", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(done["root"], json!(home_folder));
    assert_eq!(done["files"], 2, "notes.md and .gitignore: {done}");
    assert_eq!(done["gitignore_written"], true);
    assert_eq!(commits(&home_folder), "1");

    // A second time the daemon refuses, in its own words.
    let again = t.hutan(&["project", "git-init", "--home", "--yes"]).await;
    assert_ne!(again.code, 0);
    assert!(
        again.stderr.contains("git repository already"),
        "{}",
        again.stderr
    );
    assert_eq!(commits(&home_folder), "1");

    // A project that is a plain folder.
    let plain = mkdir(&base.join("plain"));
    fs::write(plain.join("a.txt"), "a\n").unwrap();
    let opened = t
        .hutan(&["project", "open", s(&plain), "--json"])
        .await
        .ok()
        .json();
    let id = opened["workspace"]["id"].as_str().unwrap().to_owned();
    let text = t.hutan(&["project", "git-init", &id, "--yes"]).await.ok();
    assert!(
        text.stdout.contains("tracking changes in"),
        "{}",
        text.stdout
    );
    assert_eq!(commits(&plain), "1");

    // A project in a repository is refused as well.
    let inside = repo(&base, "app");
    let opened = t
        .hutan(&["project", "open", s(&inside), "--json"])
        .await
        .ok()
        .json();
    let id = opened["workspace"]["id"].as_str().unwrap().to_owned();
    let refused = t.hutan(&["project", "git-init", &id, "--yes"]).await;
    assert_ne!(refused.code, 0);
    assert!(refused.stderr.contains("already"), "{}", refused.stderr);
}
