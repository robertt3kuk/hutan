//! `hutan ws new|add|remove|rename|rm|ls|move|pin|unpin|use` and `HUTAN_WORKSPACE` for `hutan
//! new` (roadmap M12-A, reshaped by M18-A: workspaces contain projects), through the real binary
//! against a real in-process daemon. Every folder is a temporary directory.

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

/// A repository `<base>/<name>` with one commit.
fn repo(base: &Path, name: &str) -> PathBuf {
    let root = mkdir(&base.join(name));
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["commit", "-q", "--allow-empty", "-m", "first"]);
    root
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

async fn ws_new(t: &TestDaemon, name: &str, paths: &[&Path]) -> String {
    let mut args = vec!["ws", "new", name, "--json"];
    args.extend(paths.iter().map(|p| s(p)));
    t.hutan(&args).await.ok().json()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// `hutan ws ls --json <args>`, in listing order.
async fn rows(t: &TestDaemon, args: &[&str]) -> Vec<Value> {
    let mut full = vec!["ws", "ls", "--json"];
    full.extend_from_slice(args);
    t.hutan(&full).await.ok().json()["workspaces"]
        .as_array()
        .unwrap()
        .clone()
}

/// Names of `hutan ws ls --json <args>`, in listing order.
async fn listed(t: &TestDaemon, args: &[&str]) -> Vec<String> {
    rows(t, args)
        .await
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_new_makes_a_workspace_with_the_projects_of_some_folders() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let api = repo(&base, "api");
    let notes = mkdir(&base.join("notes"));

    let created = t
        .hutan(&["ws", "new", "shop", s(&api), s(&notes), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(created["name"], "shop");
    assert_eq!(created["kind"], "workspace");
    assert_eq!(created["created_by"], "cli");
    assert_eq!(created["roots"], json!([s(&api), s(&notes)]));
    let id = created["id"].as_str().unwrap();
    let list = rows(&t, &["--all"]).await;
    let project = |name: &str| list.iter().find(|w| w["name"] == name).unwrap();
    assert_eq!(project("api")["kind"], "project");
    assert_eq!(project("api")["parent_id"], id);
    assert_eq!(project("notes")["parent_id"], id);
    assert_eq!(project("api")["project"]["root"], s(&api));
    assert!(
        project("api")["project"]["common_dir"].is_string(),
        "a repository"
    );
    assert!(
        project("notes")["project"].get("common_dir").is_none(),
        "a folder"
    );

    // The human form says what was made; a relative folder is the caller's.
    let human = t.hutan(&["ws", "new", "empty"]).await.ok();
    assert!(
        human.stdout.contains("(empty) with 0 project(s)"),
        "{human:?}"
    );
    let mut relative = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &["ws", "new", "rel", "./extra", "--json"],
    );
    mkdir(&base.join("extra"));
    relative.current_dir(&base);
    let out = finish(relative).await.ok().json();
    assert_eq!(out["roots"], json!([s(&base.join("extra"))]));

    // A folder that is no directory makes nothing.
    let before = rows(&t, &["--all"]).await.len();
    let out = t
        .hutan(&["ws", "new", "bad", s(&base.join("missing"))])
        .await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.starts_with("hutan: not_found:"), "{out:?}");
    assert_eq!(rows(&t, &["--all"]).await.len(), before);
    assert_eq!(t.hutan(&["ws", "new"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_prints_workspaces_with_their_projects_then_loose_projects_then_home() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let api = repo(&base, "api");
    let web = repo(&base, "web");
    let loose = mkdir(&base.join("loose"));
    let idle = mkdir(&base.join("idle"));
    let _shop = ws_new(&t, "shop", &[&api, &web]).await;
    let _lab = ws_new(&t, "lab", &[]).await;
    t.new_session(&["--cwd", s(&loose)], &["cat"]).await;
    t.new_session(&["--cwd", s(&api)], &["cat"]).await;
    t.hutan(&["project", "open", s(&idle)]).await.ok();

    // Members under their workspace, then the loose project with sessions, then Home; the
    // empty loose project is hidden unless asked for.
    assert_eq!(
        listed(&t, &[]).await,
        ["shop", "api", "web", "lab", "loose", "Home"]
    );
    assert_eq!(
        listed(&t, &["--all"]).await,
        ["shop", "api", "web", "lab", "loose", "idle", "Home"]
    );

    let human = t.hutan(&["ws", "ls"]).await.ok().stdout;
    let lines: Vec<_> = human.lines().collect();
    assert_eq!(lines.len(), 7, "{human}");
    assert!(
        lines[0].starts_with("ID") && lines[0].contains("FOLDER"),
        "{human}"
    );
    let line = |name: &str| *lines.iter().find(|l| l.contains(name)).unwrap();
    assert!(
        line("shop").contains("workspace") && line("shop").contains("1/1"),
        "{human}"
    );
    assert!(
        line("  api").contains("project") && line("  api").ends_with(s(&api)),
        "members are indented and show their folder: {human}"
    );
    assert!(line("Home").contains("home"), "{human}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_ls_sorts_by_manual_order_recent_activity_attention_and_name() {
    let t = TestDaemon::start().await;
    let zeta = ws_new(&t, "zeta", &[]).await;
    let alpha = ws_new(&t, "Alpha", &[]).await;
    let mid = ws_new(&t, "mid", &[]).await;
    for ws in [&zeta, &alpha, &mid] {
        t.new_session(&["--ws", ws], &["cat"]).await;
        sleep(Duration::from_millis(20)).await;
    }

    // Manual: creation order; Home closes the list.
    assert_eq!(
        listed(&t, &["--sort", "manual"]).await,
        ["zeta", "Alpha", "mid", "Home"]
    );
    assert_eq!(
        listed(&t, &[]).await,
        ["zeta", "Alpha", "mid", "Home"],
        "the default"
    );
    // Name, ignoring case.
    assert_eq!(
        listed(&t, &["--sort", "name"]).await,
        ["Alpha", "mid", "zeta", "Home"]
    );
    // Recent: the newest session's workspace first.
    assert_eq!(
        listed(&t, &["--sort", "recent"]).await,
        ["mid", "Alpha", "zeta", "Home"]
    );

    // Attention: a blocked agent in `zeta` puts it first although `mid` is more recent.
    let session = t
        .sessions()
        .await
        .into_iter()
        .find(|s| s["workspace_id"] == zeta)
        .unwrap();
    let report = |state: &str, seq: u64| {
        json!({
            "session_id": session["id"], "source": "test", "agent": "omp",
            "state": state, "seq": seq,
        })
        .to_string()
    };
    t.hutan(&["api", "agent.report", &report("blocked", 1)])
        .await
        .ok();
    assert_eq!(
        listed(&t, &["--sort", "attention"]).await,
        ["zeta", "mid", "Alpha", "Home"]
    );
    // Not blocked any more: back to the recent order.
    t.hutan(&["api", "agent.report", &report("working", 2)])
        .await
        .ok();
    assert_eq!(
        listed(&t, &["--sort", "attention"]).await,
        ["mid", "Alpha", "zeta", "Home"]
    );

    // Pinned workspaces come first in every mode.
    t.hutan(&["ws", "pin", "Alpha"]).await.ok();
    for mode in ["manual", "recent", "attention", "name"] {
        assert_eq!(listed(&t, &["--sort", mode]).await[0], "Alpha", "{mode}");
    }

    let out = t.hutan(&["ws", "ls", "--sort", "sideways"]).await;
    assert_eq!(out.code, 2, "{out:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_add_and_remove_move_folders_and_projects_in_and_out() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let api = repo(&base, "api");
    let web = repo(&base, "web");
    let notes = mkdir(&base.join("notes"));
    let shop = ws_new(&t, "shop", &[&api]).await;
    let lab = ws_new(&t, "lab", &[]).await;
    t.hutan(&["project", "open", s(&notes)]).await.ok();

    // A path adds the project of its folder (made when new); a name adds an existing project.
    let added = t
        .hutan(&["ws", "add", "shop", s(&web), "--json"])
        .await
        .ok()
        .json();
    assert_eq!(added["roots"], json!([s(&api), s(&web)]));
    let human = t.hutan(&["ws", "add", "shop", "notes"]).await.ok();
    assert!(human.stdout.contains("it has 3 project(s)"), "{human:?}");
    // Adding again changes nothing; adding to another workspace moves the project.
    t.hutan(&["ws", "add", "shop", "notes"]).await.ok();
    let moved = t
        .hutan(&["ws", "add", &lab[..10], "notes", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(moved["id"], lab);
    assert_eq!(moved["roots"], json!([s(&notes)]));
    let list = rows(&t, &[]).await;
    let parent_of = |name: &str| {
        list.iter()
            .find(|w| w["name"] == name && w["kind"] == "project")
            .unwrap()["parent_id"]
            .clone()
    };
    assert_eq!(parent_of("notes"), lab);
    assert_eq!(parent_of("api"), shop);

    // Removing takes a project out of its workspace and nothing else.
    let out = t
        .hutan(&["ws", "remove", "shop", "web", "--json"])
        .await
        .ok()
        .json();
    assert!(out.get("parent_id").is_none(), "{out}");
    let human = t.hutan(&["ws", "remove", "lab", "notes"]).await.ok();
    assert!(human.stdout.contains("it stays a project"), "{human:?}");
    assert_eq!(
        listed(&t, &["--all"]).await,
        ["shop", "api", "lab", "notes", "web", "Home"]
    );
    // `web` is not in `shop` any more; the failure says so and changes nothing.
    let out = t.hutan(&["ws", "remove", "shop", "web"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("is not in workspace"), "{out:?}");

    // Targets of the wrong kind say what they are; a folder that does not exist is no target.
    let out = t.hutan(&["ws", "add", "notes", "api"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("`notes` is a project, not a workspace"),
        "{out:?}"
    );
    let out = t.hutan(&["ws", "add", "shop", "Home"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("the Home workspace, not a project"),
        "{out:?}"
    );
    let out = t.hutan(&["ws", "add", "shop", "nope"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no project matches `nope`"), "{out:?}");
    assert_eq!(t.hutan(&["ws", "add", "shop"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_rm_keeps_the_projects_and_refuses_projects_and_live_sessions() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let api = repo(&base, "api");
    let shop = ws_new(&t, "shop", &[&api]).await;

    // A workspace with a live session of its own is refused; the projects are not its own.
    let own = t.new_session(&["--ws", &shop], &["cat"]).await;
    let out = t.hutan(&["ws", "rm", "shop"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("invalid_state"), "{out:?}");
    t.hutan(&["stop", &own]).await.ok();
    let in_project = t.new_session(&["--cwd", s(&api)], &["cat"]).await;

    let out = t.hutan(&["ws", "rm", "api"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("`api` is a project, not a workspace"),
        "{out:?}"
    );
    let human = t.hutan(&["ws", "rm", "shop"]).await.ok();
    assert!(human.stdout.contains("its projects stay"), "{human:?}");
    assert_eq!(listed(&t, &[]).await, ["api", "Home"]);
    assert!(t.sessions().await.iter().any(|s| s["id"] == in_project));
    assert_eq!(t.hutan(&["ws", "rm", "shop"]).await.code, 1);
    assert_eq!(t.hutan(&["ws", "rm"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_move_reorders_by_id_or_name() {
    let t = TestDaemon::start().await;
    let a = ws_new(&t, "a", &[]).await;
    let _b = ws_new(&t, "b", &[]).await;
    let _c = ws_new(&t, "c", &[]).await;
    assert_eq!(listed(&t, &["--all"]).await, ["a", "b", "c", "Home"]);

    let out = t
        .hutan(&["ws", "move", "c", "--before", &a, "--json"])
        .await
        .ok()
        .json();
    let order: Vec<_> = out["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        ["Home", "c", "a", "b"],
        "the result is the daemon's new order"
    );
    assert_eq!(listed(&t, &["--all"]).await, ["c", "a", "b", "Home"]);

    let human = t.hutan(&["ws", "move", "Home", "--last"]).await.ok();
    assert!(human.stdout.contains("to the end"), "{human:?}");
    let human = t.hutan(&["ws", "move", &a, "--before", "Home"]).await.ok();
    assert!(human.stdout.contains("before"), "{human:?}");
    assert_eq!(listed(&t, &["--all"]).await, ["c", "b", "a", "Home"]);

    // Exactly one of --before and --last.
    assert_eq!(t.hutan(&["ws", "move", "a"]).await.code, 2);
    assert_eq!(
        t.hutan(&["ws", "move", "a", "--last", "--before", "b"])
            .await
            .code,
        2
    );
    // Unknown targets fail without moving anything.
    let out = t.hutan(&["ws", "move", "nope", "--last"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("no workspace or project matches"),
        "{out:?}"
    );
    assert_eq!(
        t.hutan(&["ws", "move", "a", "--before", "nope"]).await.code,
        1
    );
    assert_eq!(listed(&t, &["--all"]).await, ["c", "b", "a", "Home"]);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_pin_and_unpin() {
    let t = TestDaemon::start().await;
    let id = ws_new(&t, "later", &[]).await;
    let _other = ws_new(&t, "other", &[]).await;

    let pinned = t.hutan(&["ws", "pin", "later", "--json"]).await.ok().json();
    assert_eq!(pinned["pinned"], true);
    assert_eq!(pinned["id"], id);
    let human = t.hutan(&["ws", "pin", &id]).await.ok();
    assert_eq!(
        human.stdout.trim(),
        format!("pinned {id} (later)"),
        "idempotent"
    );
    assert_eq!(listed(&t, &["--all"]).await, ["later", "other", "Home"]);

    let unpinned = t.hutan(&["ws", "unpin", &id, "--json"]).await.ok().json();
    assert_eq!(unpinned["pinned"], false);
    let human = t.hutan(&["ws", "unpin", "later"]).await.ok();
    assert!(human.stdout.starts_with("unpinned "), "{human:?}");
    assert_eq!(listed(&t, &["--all"]).await, ["later", "other", "Home"]);

    assert_eq!(t.hutan(&["ws", "pin", "nope"]).await.code, 1);
    assert_eq!(t.hutan(&["ws", "pin"]).await.code, 2);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_use_prints_an_export_and_new_honours_it() {
    let (_keep, base) = tmp();
    let t = TestDaemon::start().await;
    let work = ws_new(&t, "work", &[]).await;
    let other = ws_new(&t, "other", &[]).await;

    let line = t.hutan(&["ws", "use", "work"]).await.ok().stdout;
    assert_eq!(line.trim(), format!("export HUTAN_WORKSPACE={work}"));
    let json = t
        .hutan(&["ws", "use", &other[..20], "--json"])
        .await
        .ok()
        .json();
    assert_eq!(json["workspace_id"], other);
    assert_eq!(json["name"], "other");
    assert_eq!(json["export"], format!("export HUTAN_WORKSPACE={other}"));
    let out = t.hutan(&["ws", "use", "nope"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stdout.is_empty(), "nothing to eval: {out:?}");

    // `new` runs in a folder of its own, which would otherwise make its project.
    let cwd = mkdir(&base.join("folder"));
    let with_env = |extra: &[&str], value: &str| {
        let mut args = vec!["new", "--json"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--", "cat"]);
        let mut command = command(Path::new(HUTAN), Some(&t.paths.socket), &args);
        command.env("HUTAN_WORKSPACE", value).current_dir(&cwd);
        finish(command)
    };
    // The variable chooses the workspace (by id or name); `--ws` wins over it; without either
    // the session goes to the project of its folder.
    let out = with_env(&[], &work).await.ok().json();
    assert_eq!(out["session"]["workspace_id"], work);
    let out = with_env(&[], "other").await.ok().json();
    assert_eq!(out["session"]["workspace_id"], other);
    let out = with_env(&["--ws", "work"], "other").await.ok().json();
    assert_eq!(out["session"]["workspace_id"], work);
    let out = with_env(&[], "").await.ok().json();
    let project = t.hutan(&["project", "ls", "--json"]).await.ok().json()["projects"][0].clone();
    assert_eq!(project["name"], "folder");
    assert_eq!(
        out["session"]["workspace_id"], project["id"],
        "an empty variable is unset"
    );

    // A stale value says where it came from and creates nothing.
    let before = t.sessions().await.len();
    let out = with_env(&[], "gone").await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr
            .contains("no workspace or project matches `gone`"),
        "{out:?}"
    );
    assert!(out.stderr.contains("$HUTAN_WORKSPACE"), "{out:?}");
    assert_eq!(t.sessions().await.len(), before);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_and_new_take_workspace_names() {
    let t = TestDaemon::start().await;
    let work = ws_new(&t, "work", &[]).await;
    let id = t.new_session(&["--ws", "work"], &["cat"]).await;
    t.new_session(&[], &["cat"]).await;
    let out = t.hutan(&["ls", "--ws", "work", "--json"]).await.ok().json();
    let sessions = out["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], id);
    assert_eq!(sessions[0]["workspace_id"], work);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn home_stays_and_renames() {
    let t = TestDaemon::start().await;
    let home = t.hutan(&["ws", "ls", "--json"]).await.ok().json()["workspaces"][0].clone();
    let id = home["id"].as_str().unwrap();
    // Home is no workspace to delete and no project to close.
    let out = t.hutan(&["ws", "rm", id]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(
        out.stderr.contains("the Home workspace, not a workspace"),
        "{out:?}"
    );
    let out = t.hutan(&["project", "close", id]).await;
    assert_eq!(out.code, 1, "{out:?}");
    let renamed = t
        .hutan(&["ws", "rename", "Home", "Notes", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(renamed["name"], "Notes");
    assert_eq!(renamed["kind"], "home");
    assert_eq!(listed(&t, &[]).await, ["Notes"]);
    // And Home resolves by its new name wherever any kind is accepted.
    let line = t.hutan(&["ws", "use", "notes"]).await.ok().stdout;
    assert!(line.contains(id), "{line}");
    t.stop().await;
}
