//! `hutan checkpoint ls|diff|rm` against a real daemon whose Codex is `hutan-codex-fixture
//! replay` working in a git repository the test creates; the test edits the files while the
//! replay holds each turn open, as the agent would (protocol 0.37, ADR 0019).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{TestDaemon, WAIT, codex_fixture_bin};
use hutan_agents::CodexConfig;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};

/// Git without the user's configuration.
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
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn refs(root: &Path) -> Vec<String> {
    git(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/hutan/"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// The recorded `plain-turn` with its turn repeated once per gate, each held after
/// `turn/started` until its gate exists; each turn has ids of its own.
fn held_turns(dir: &Path, gates: &[PathBuf]) -> PathBuf {
    let real = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../agents/tests/fixtures/codex-0.162/plain-turn.jsonl"),
    )
    .unwrap();
    let lines: Vec<&str> = real.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("\"method\":\"turn/start\""))
        .unwrap();
    let mut out: Vec<String> = lines[..at].iter().map(|l| (*l).to_owned()).collect();
    for (n, gate) in gates.iter().enumerate() {
        for line in &lines[at..] {
            let line = line
                .replace(
                    "01a00000-0000-7000-8000-000000000002",
                    &format!("01a00000-0000-7000-8000-0000000010{n:02}"),
                )
                .replace(
                    "01a00000-0000-7000-8000-000000000003",
                    &format!("01a00000-0000-7000-8000-0000000020{n:02}"),
                )
                .replace("msg_fixture_1", &format!("msg_fixture_t{n}"));
            let started = line.contains("\"method\":\"turn/started\"");
            out.push(line);
            if started {
                out.push(json!({"dir": "hold", "file": gate}).to_string());
            }
        }
    }
    let transcript = dir.join("held-turns.jsonl");
    fs::write(&transcript, out.join("\n") + "\n").unwrap();
    transcript
}

struct Repo {
    t: TestDaemon,
    work: tempfile::TempDir,
    root: PathBuf,
}

impl Repo {
    async fn start(turns: usize) -> Self {
        let work = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(work.path()).unwrap();
        git(&base, &["init", "-q", "-b", "main", "repo"]);
        let root = base.join("repo");
        fs::write(root.join("a.txt"), "alpha\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "init"]);
        let gates: Vec<PathBuf> = (0..turns).map(|n| base.join(format!("gate-{n}"))).collect();
        // One transcript per spawn: the chat's agent first, then three forks of it.
        let scripted = base.join("script");
        fs::create_dir_all(&scripted).unwrap();
        fs::copy(held_turns(&base, &gates), scripted.join("00-held.jsonl")).unwrap();
        for n in 1..=3 {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../agents/tests/fixtures/codex-0.162/fork-turn.jsonl"),
                scripted.join(format!("{n:02}-fork.jsonl")),
            )
            .unwrap();
        }
        let codex = CodexConfig {
            program: codex_fixture_bin(),
            args: vec![
                "replay".to_owned(),
                "--received".to_owned(),
                base.join("received.jsonl").display().to_string(),
                scripted.display().to_string(),
                "app-server".to_owned(),
            ],
            env: Vec::new(),
        };
        let t = TestDaemon::start_with(move |c| {
            c.codex = Some(codex);
            c.checkpoints.git_env = BTreeMap::from([
                ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
                ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ]);
        })
        .await;
        Self { t, work, root }
    }

    fn gate(&self, n: usize) -> PathBuf {
        fs::canonicalize(self.work.path())
            .unwrap()
            .join(format!("gate-{n}"))
    }

    async fn ls(&self, args: &[&str]) -> Value {
        let mut all = vec!["checkpoint", "ls", "--json"];
        all.extend_from_slice(args);
        self.t.hutan(&all).await.ok().json()
    }

    /// `hutan checkpoint ls SESSION --json` once it lists `count` checkpoints.
    async fn listed(&self, session: &str, count: usize) -> Vec<Value> {
        timeout(WAIT, async {
            loop {
                let list = self.ls(&[session]).await;
                let checkpoints = list["checkpoints"].as_array().unwrap().clone();
                if checkpoints.len() == count {
                    return checkpoints;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("never {count} checkpoints"))
    }

    /// The checkpoint refs once there are `count`. A turn's row names its end ref before the
    /// daemon deletes the turn's start ref, so a listed row does not yet mean the start ref is
    /// gone.
    async fn refs_when(&self, count: usize) -> Vec<String> {
        timeout(WAIT, async {
            loop {
                let refs = refs(&self.root);
                if refs.len() == count {
                    return refs;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| refs(&self.root))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ls_diff_and_rm_work_on_the_checkpoints_of_a_fixture_chat() {
    let r = Repo::start(2).await;
    let root = r.root.display().to_string();
    let created =
        r.t.hutan(&["chat", "new", "--json", "--cwd", &root])
            .await
            .ok()
            .json();
    let session = created["session"]["id"].as_str().unwrap().to_owned();

    // Turn 0 edits a tracked file; turn 1 adds a file.
    r.t.hutan(&["chat", "send", &session, "turn 0"]).await.ok();
    fs::write(r.root.join("a.txt"), "edited by the agent\n").unwrap();
    fs::write(r.gate(0), "").unwrap();
    r.listed(&session, 1).await;
    r.t.hutan(&["chat", "send", &session, "turn 1"]).await.ok();
    fs::write(r.root.join("new.txt"), "one\ntwo\n").unwrap();
    fs::write(r.gate(1), "").unwrap();
    let checkpoints = r.listed(&session, 2).await;
    let (second, first) = (
        checkpoints[0]["id"].as_str().unwrap().to_owned(),
        checkpoints[1]["id"].as_str().unwrap().to_owned(),
    );
    assert_eq!(checkpoints[1]["files_changed"], 1);
    assert_eq!(checkpoints[1]["state"], "ok");
    assert_eq!(r.refs_when(2).await.len(), 2, "one ref per turn");

    // The table: one row per changed turn, newest first.
    let text = r.t.hutan(&["checkpoint", "ls", &session]).await.ok().stdout;
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("CHECKPOINT") && lines[0].contains("STATE"));
    assert!(
        lines[1].starts_with(&second) && lines[1].contains("+2 -0"),
        "{text}"
    );
    assert!(
        lines[2].starts_with(&first) && lines[2].contains("+1 -1"),
        "{text}"
    );
    // Without a session: every checkpoint, with the session's title.
    let all = r.t.hutan(&["checkpoint", "ls"]).await.ok().stdout;
    assert!(all.lines().next().unwrap().contains("SESSION"), "{all}");

    // What turn 0 changed, as a stat and as a patch; an id prefix is enough.
    let stat =
        r.t.hutan(&["checkpoint", "diff", &first, "--stat"])
            .await
            .ok()
            .stdout;
    assert_eq!(stat.trim_end(), "M a.txt  +1 -1\n1 file changed, +1 -1");
    let patch =
        r.t.hutan(&["checkpoint", "diff", &first[..20]])
            .await
            .ok()
            .stdout;
    assert!(patch.contains("-alpha\n+edited by the agent"), "{patch}");

    // A later edit of the user shows against the checkout now.
    fs::write(r.root.join("a.txt"), "edited by the user\n").unwrap();
    let now =
        r.t.hutan(&["checkpoint", "diff", &first, "--against", "now", "--stat"])
            .await
            .ok()
            .stdout;
    assert!(
        now.contains("M a.txt") && now.contains("A new.txt"),
        "{now}"
    );
    let now =
        r.t.hutan(&["checkpoint", "diff", &first, "a.txt", "--against", "now"])
            .await
            .ok()
            .stdout;
    assert!(now.contains("+edited by the user"), "{now}");
    let through =
        r.t.hutan(&[
            "--json",
            "checkpoint",
            "diff",
            &first,
            "--against",
            &second,
            "--stat",
        ])
        .await
        .ok()
        .json();
    let files: Vec<&str> = through["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["a.txt", "new.txt"]);
    assert!(through.get("patch").is_none());

    // rm: the checkpoint's ref and row go.
    let removed = r.t.hutan(&["checkpoint", "rm", &first]).await.ok().stdout;
    assert_eq!(removed.trim_end(), "removed 1 checkpoint");
    assert_eq!(refs(&r.root).len(), 1);
    assert_eq!(r.listed(&session, 1).await[0]["id"], second.as_str());
    let gone = r.t.hutan(&["checkpoint", "diff", &first]).await;
    assert_eq!(gone.code, 1, "{gone:?}");
    // ... and a session's, all of them.
    let removed = r.t.hutan(&["checkpoint", "rm", &session]).await.ok().stdout;
    assert_eq!(
        removed.trim_end(),
        format!("removed 1 checkpoint of session {session}")
    );
    assert!(refs(&r.root).is_empty());
    let empty = r.t.hutan(&["checkpoint", "ls", &session]).await.ok().stdout;
    assert_eq!(empty.trim_end(), "no checkpoints");
    r.t.stop().await;
}

fn read(root: &Path, path: &str) -> Option<String> {
    fs::read_to_string(root.join(path)).ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_previews_asks_and_undo_flips_it_back_and_forth() {
    let r = Repo::start(1).await;
    let root = r.root.display().to_string();
    let created =
        r.t.hutan(&["chat", "new", "--json", "--cwd", &root])
            .await
            .ok()
            .json();
    let session = created["session"]["id"].as_str().unwrap().to_owned();
    r.t.hutan(&["chat", "send", &session, "turn 0"]).await.ok();
    fs::write(r.root.join("a.txt"), "edited by the agent\n").unwrap();
    fs::write(r.root.join("new.txt"), "new\n").unwrap();
    fs::write(r.gate(0), "").unwrap();
    let checkpoint = r.listed(&session, 1).await[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Without a terminal and without --yes: the preview, the label, and nothing restored.
    let asked =
        r.t.hutan(&["checkpoint", "restore", &checkpoint[..20]])
            .await;
    assert_eq!(asked.code, 2, "{asked:?}");
    assert!(asked.stdout.contains("write   a.txt"), "{}", asked.stdout);
    assert!(asked.stdout.contains("delete  new.txt"), "{}", asked.stdout);
    assert!(
        asked.stdout.contains("2 files to restore"),
        "{}",
        asked.stdout
    );
    assert!(
        asked
            .stdout
            .contains("Restores files only. Codex still remembers this turn"),
        "{}",
        asked.stdout
    );
    assert!(asked.stderr.contains("pass --yes"), "{}", asked.stderr);
    assert_eq!(
        read(&r.root, "a.txt").as_deref(),
        Some("edited by the agent\n")
    );
    // A conflict that is not one is a usage error.
    let wrong =
        r.t.hutan(&[
            "checkpoint",
            "restore",
            &checkpoint,
            "--accept-conflicts",
            "a.txt",
            "--yes",
        ])
        .await;
    assert_eq!(wrong.code, 2, "{wrong:?}");
    assert!(
        wrong.stderr.contains("is not a conflict"),
        "{}",
        wrong.stderr
    );

    // --yes restores.
    let done =
        r.t.hutan(&["checkpoint", "restore", &checkpoint, "--yes"])
            .await
            .ok()
            .stdout;
    assert!(done.contains("wrote    a.txt"), "{done}");
    assert!(done.contains("deleted  new.txt"), "{done}");
    assert!(done.contains("restored: 2 files"), "{done}");
    assert_eq!(read(&r.root, "a.txt").as_deref(), Some("alpha\n"));
    assert_eq!(read(&r.root, "new.txt"), None);
    let listed = r.listed(&session, 2).await;
    assert_eq!(listed[0]["kind"], "restore");
    let table = r.t.hutan(&["checkpoint", "ls", &session]).await.ok().stdout;
    assert!(table.contains("a restore"), "{table}");

    // Undo, then undo the undo.
    let undone =
        r.t.hutan(&["checkpoint", "undo", &session, "--yes"])
            .await
            .ok()
            .stdout;
    assert!(undone.starts_with("Undo: files of"), "{undone}");
    assert!(undone.contains("undone: 2 files"), "{undone}");
    assert_eq!(
        read(&r.root, "a.txt").as_deref(),
        Some("edited by the agent\n")
    );
    assert_eq!(read(&r.root, "new.txt").as_deref(), Some("new\n"));
    let json =
        r.t.hutan(&["--json", "checkpoint", "undo", "--yes"])
            .await
            .ok()
            .json();
    assert_eq!(json["deleted"], serde_json::json!(["new.txt"]));
    assert_eq!(json["checkpoint"]["kind"], "restore");
    assert_eq!(read(&r.root, "a.txt").as_deref(), Some("alpha\n"));
    assert_eq!(r.listed(&session, 4).await.len(), 4);
    // Nothing left to restore of the turn: said, and nothing happens.
    let nothing =
        r.t.hutan(&["checkpoint", "restore", &checkpoint, "--yes"])
            .await
            .ok()
            .stdout;
    assert!(nothing.contains("nothing to restore"), "{nothing}");
    r.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_fork_into_a_worktree_puts_the_files_back_and_archives_the_parent() {
    let r = Repo::start(2).await;
    let root = r.root.display().to_string();
    let created =
        r.t.hutan(&["chat", "new", "--json", "--cwd", &root])
            .await
            .ok()
            .json();
    let session = created["session"]["id"].as_str().unwrap().to_owned();
    r.t.hutan(&["chat", "send", &session, "turn 0"]).await.ok();
    fs::write(r.root.join("a.txt"), "edited by the agent\n").unwrap();
    fs::write(r.gate(0), "").unwrap();
    r.listed(&session, 1).await;
    r.t.hutan(&["chat", "send", &session, "turn 1"]).await.ok();
    fs::write(r.root.join("new.txt"), "one\ntwo\n").unwrap();
    fs::write(r.gate(1), "").unwrap();
    r.listed(&session, 2).await;

    // The flags that exclude each other are refused before anything happens.
    let both =
        r.t.hutan(&["chat", "fork", &session, "--worktree", "--files"])
            .await;
    assert_ne!(both.code, 0);
    assert!(both.stderr.contains("cannot be used with"), "{both:?}");

    // A worktree at the files of turn 1, on a branch of the daemon's choosing.
    let text =
        r.t.hutan(&["chat", "fork", &session, "--turn", "1", "--worktree"])
            .await
            .ok()
            .stdout;
    assert!(
        text.contains("forked from") && text.contains("after turn 1"),
        "{text}"
    );
    assert!(text.contains("on branch fork/"), "{text}");
    assert!(text.contains("the files of the turn"), "{text}");
    let path = text
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .and_then(|rest| rest.split(" on branch ").next())
        .unwrap()
        .to_owned();
    let worktree = PathBuf::from(&path);
    assert_eq!(
        fs::read_to_string(worktree.join("a.txt")).unwrap(),
        "edited by the agent\n"
    );
    assert!(
        !worktree.join("new.txt").exists(),
        "turn 2's file is not there"
    );
    assert!(git(&worktree, &["status", "--porcelain"]).contains(" M a.txt"));
    assert_eq!(
        fs::read_to_string(r.root.join("new.txt")).unwrap(),
        "one\ntwo\n",
        "the source folder is untouched"
    );

    // The files of this folder back to turn 1, and Undo.
    let text =
        r.t.hutan(&["chat", "fork", &session, "--turn", "1", "--files"])
            .await
            .ok()
            .stdout;
    assert!(
        text.contains("files put back: 0 written, 1 deleted"),
        "{text}"
    );
    assert!(
        text.contains(&format!("hutan checkpoint undo {session}")),
        "{text}"
    );
    assert!(!r.root.join("new.txt").exists());
    r.t.hutan(&["checkpoint", "undo", &session, "--yes"])
        .await
        .ok();
    assert_eq!(
        fs::read_to_string(r.root.join("new.txt")).unwrap(),
        "one\ntwo\n"
    );

    // Named branch, and the original closed.
    let forked =
        r.t.hutan(&[
            "--json",
            "chat",
            "fork",
            &session,
            "--turn",
            "1",
            "--worktree",
            "fork/by-name",
            "--archive-parent",
        ])
        .await
        .ok()
        .json();
    assert_eq!(forked["fork"]["worktree"]["branch"], "fork/by-name");
    assert_eq!(forked["fork"]["worktree"]["files"], "turn");
    assert_eq!(forked["fork"]["parent_archived"], true);
    let listed = r.t.hutan(&["ls", "--json"]).await.ok().json();
    let ids: Vec<&str> = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert!(!ids.contains(&session.as_str()), "closed: {ids:?}");
}
