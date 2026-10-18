//! `hutan search` and `hutan index` against a real daemon whose history sources are temporary
//! directories (never the user's agent directories).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Out, TestDaemon, WAIT};
use hutan_daemon::{DaemonConfig, Paths};
use serde_json::Value;
use tokio::time::sleep;

struct Env {
    _tmp: tempfile::TempDir,
    claude: PathBuf,
    t: TestDaemon,
}

async fn start(enabled: bool) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let home = base.join("home");
    let claude = base.join("claude");
    let (codex, omp) = (base.join("codex"), base.join("omp"));
    for dir in [&home, &claude, &codex, &omp] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(
        home.join("daemon.toml"),
        format!(
            "[history]\nenabled = {enabled}\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
            claude.display(),
            codex.display(),
            omp.display()
        ),
    )
    .unwrap();
    let paths = Paths::in_home(&home);
    let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone()))
        .await
        .unwrap();
    Env {
        _tmp: tmp,
        claude,
        t: TestDaemon {
            _dir: tempfile::tempdir().unwrap(),
            paths,
            daemon,
        },
    }
}

fn append(path: &Path, lines: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

const RECENT: &str = r#"{"type":"user","cwd":"/work/app","message":{"role":"user","content":[{"type":"text","text":"why does the quokka scheduler stall under load"}]}}"#;
const OLD: &str = r#"{"type":"user","timestamp":"2020-01-01T00:00:00.000Z","cwd":"/work/old","message":{"role":"user","content":[{"type":"text","text":"ancient quokka question"}]}}"#;

/// Runs `hutan search …` until it prints at least `count` hits as JSON.
async fn search_until(env: &Env, args: &[&str], count: usize) -> Value {
    let start = Instant::now();
    loop {
        let mut full = vec!["search", "--json"];
        full.extend_from_slice(args);
        let out = env.t.hutan(&full).await;
        if out.code == 0 {
            let json = out.json();
            if json["hits"].as_array().unwrap().len() >= count {
                return json;
            }
        }
        assert!(
            start.elapsed() < WAIT,
            "no {count} hit(s) for {args:?}: {out:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_prints_json_and_human_output_and_honours_filters() {
    let env = start(true).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);
    append(&env.claude.join("-work-old/sess-b.jsonl"), &[OLD]);

    let json = search_until(&env, &["quokka"], 2).await;
    let hits = json["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    let recent = hits.iter().find(|h| h["session_key"] == "sess-a").unwrap();
    assert_eq!(recent["source"], "claude");
    assert_eq!(recent["role"], "user");
    assert_eq!(recent["cwd"], "/work/app");
    assert_eq!(
        recent["resume_argv"],
        serde_json::json!(["claude", "--resume", "sess-a"])
    );
    assert!(recent["snippet"].as_str().unwrap().contains("«quokka»"));

    let human = env.t.hutan(&["search", "quokka", "scheduler"]).await.ok();
    assert!(human.stdout.contains("claude"), "{human:?}");
    assert!(human.stdout.contains("/work/app"), "{human:?}");
    assert!(
        human.stdout.contains("resume: claude --resume sess-a"),
        "{human:?}"
    );
    assert!(
        human.stdout.contains("«quokka»"),
        "no color off a terminal: {human:?}"
    );
    assert!(!human.stdout.contains('\u{1b}'), "{human:?}");

    // --since drops the old session, --source narrows, --limit caps.
    let recent_only = search_until(&env, &["quokka", "--since", "7d"], 1).await;
    assert_eq!(recent_only["hits"].as_array().unwrap().len(), 1);
    assert_eq!(recent_only["hits"][0]["session_key"], "sess-a");
    let other = env
        .t
        .hutan(&["search", "--json", "quokka", "--source", "codex"])
        .await
        .ok();
    assert!(other.json()["hits"].as_array().unwrap().is_empty());
    let capped = env
        .t
        .hutan(&["search", "--json", "quokka", "--limit", "1"])
        .await
        .ok();
    assert_eq!(capped.json()["hits"].as_array().unwrap().len(), 1);
    let none = env.t.hutan(&["search", "zzzzqqqq"]).await.ok();
    assert_eq!(none.stdout.trim(), "no matches");

    // Usage errors exit 2.
    for bad in [
        &["search", "--since", "yesterday", "x"][..],
        &["search", "--limit", "0", "x"],
        &["search", "--source", "opencode", "x"],
        &["search"],
    ] {
        assert_eq!(env.t.hutan(bad).await.code, 2, "{bad:?}");
    }
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn index_commands_enable_disable_status_clear_rebuild() {
    let env = start(false).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);

    // Disabled: status says so, search is refused with a pointer to `index enable`.
    let status = env
        .t
        .hutan(&["index", "status", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(status["enabled"], false);
    let human = env.t.hutan(&["index", "status"]).await.ok();
    assert!(human.stdout.contains("disabled"), "{human:?}");
    let refused: Out = env.t.hutan(&["search", "quokka"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("hutan index enable"), "{refused:?}");

    let enabled = env
        .t
        .hutan(&["index", "enable", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(enabled["enabled"], true);
    search_until(&env, &["quokka"], 1).await;
    let human = env.t.hutan(&["index", "status"]).await.ok();
    assert!(human.stdout.contains("enabled"), "{human:?}");
    assert!(human.stdout.contains("claude"), "{human:?}");
    assert!(human.stdout.contains("MiB"), "{human:?}");
    let status = env
        .t
        .hutan(&["index", "status", "--json"])
        .await
        .ok()
        .json();
    let claude = status["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(
        (claude["files"].as_u64(), claude["rows"].as_u64()),
        (Some(1), Some(1))
    );

    let cleared = env.t.hutan(&["index", "clear", "--json"]).await.ok().json();
    assert_eq!(cleared["cleared"], true);
    let after = env
        .t
        .hutan(&["search", "--json", "quokka"])
        .await
        .ok()
        .json();
    assert!(after["hits"].as_array().unwrap().is_empty());

    let rebuilt = env
        .t
        .hutan(&["index", "rebuild", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(rebuilt["rebuilding"], true);
    search_until(&env, &["quokka"], 1).await;

    let disabled = env
        .t
        .hutan(&["index", "disable", "--json"])
        .await
        .ok()
        .json();
    assert_eq!(disabled["enabled"], false);
    assert_eq!(env.t.hutan(&["search", "quokka"]).await.code, 1);
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_hit_that_cannot_resume_fails_cleanly() {
    let env = start(true).await;
    append(&env.claude.join("-work-app/sess-a.jsonl"), &[RECENT]);
    search_until(&env, &["quokka"], 1).await;
    let out = env.t.hutan(&["search", "quokka", "--resume", "5"]).await;
    assert_eq!(out.code, 1, "{out:?}");
    assert!(out.stderr.contains("no hit 5"), "{out:?}");
    env.t.stop().await;
}

// `hutan history sessions` -------------------------------------------------------------------------

/// A Claude Code transcript with a user prompt (at `ts`), an answer, and an optional title.
fn session_lines(cwd: &str, ts: &str, prompt: &str, title: Option<&str>) -> Vec<String> {
    let mut lines = vec![
        format!(
            r#"{{"type":"user","timestamp":"{ts}","cwd":"{cwd}","message":{{"role":"user","content":[{{"type":"text","text":"{prompt}"}}]}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"On it."}}]}}}}"#
        ),
    ];
    if let Some(title) = title {
        lines.push(format!(r#"{{"type":"ai-title","aiTitle":"{title}"}}"#));
    }
    lines
}

fn refs(lines: &[String]) -> Vec<&str> {
    lines.iter().map(String::as_str).collect()
}

async fn sessions_until(env: &Env, args: &[&str], count: usize) -> Value {
    let start = Instant::now();
    loop {
        let mut full = vec!["history", "sessions", "--json"];
        full.extend_from_slice(args);
        let out = env.t.hutan(&full).await;
        if out.code == 0 {
            let json = out.json();
            if json["sessions"].as_array().unwrap().len() >= count {
                return json;
            }
        }
        assert!(
            start.elapsed() < WAIT,
            "no {count} session(s) for {args:?}: {out:?}"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn history_sessions_lists_the_catalog_as_a_table_and_as_json() {
    let env = start(true).await;
    append(
        &env.claude.join("-work-app/sess-a.jsonl"),
        &refs(&session_lines(
            "/work/app",
            "2026-10-01T09:00:00.000Z",
            "fix the quokka scheduler",
            Some("Scheduler stall"),
        )),
    );
    // The title is text from a transcript: an OSC 52 clipboard write and colors must not
    // reach the terminal.
    append(
        &env.claude.join("-work-app/sess-b.jsonl"),
        &refs(&session_lines(
            "/work/app",
            "2026-10-02T09:00:00.000Z",
            "second one",
            Some(r"Evil \u001b]52;c;aGVsbG8=\u0007\u001b[31mred\u001b[0m title"),
        )),
    );

    let json = sessions_until(&env, &[], 2).await;
    assert_eq!(
        (json["matched"].as_u64(), json["loaded"].as_u64()),
        (Some(2), Some(2))
    );
    let rows = json["sessions"].as_array().unwrap();
    assert_eq!(rows[0]["session_key"], "sess-b", "newest first");
    assert_eq!(rows[1]["title"], "Scheduler stall");
    assert_eq!(rows[1]["messages"], 2);
    assert_eq!(rows[1]["model"], "claude-opus-5");
    assert_eq!(rows[1]["first_prompt"], "fix the quokka scheduler");
    assert_eq!(
        rows[1]["resume_argv"],
        serde_json::json!(["claude", "--resume", "sess-a"])
    );
    assert_eq!(json["scan"]["files"], 2);

    let human = env.t.hutan(&["history", "sessions"]).await.ok();
    let lines: Vec<_> = human.stdout.lines().collect();
    assert!(lines[0].starts_with("AGENT"), "{human:?}");
    for column in ["TITLE", "MSGS", "AGE", "MODEL", "PROJECT"] {
        assert!(lines[0].contains(column), "{human:?}");
    }
    assert!(lines[2].contains("Scheduler stall"), "{human:?}");
    assert!(lines[2].contains("claude-opus-5"), "{human:?}");
    assert!(human.stdout.contains("2 of 2 sessions"), "{human:?}");
    assert!(human.stdout.contains("scanned 2 files"), "{human:?}");
    assert!(
        lines[1].contains("Evil") && lines[1].contains("red") && lines[1].contains("title"),
        "{human:?}"
    );
    for forbidden in ['\u{1b}', '\u{7}', '\u{9b}'] {
        assert!(!human.stdout.contains(forbidden), "{human:?}");
    }
    assert!(
        !human.stdout.contains("aGVsbG8"),
        "the OSC payload is dropped: {human:?}"
    );

    // --filter, --limit, --source, --sort, --refresh.
    let filtered = env
        .t
        .hutan(&["history", "sessions", "--json", "--filter", "quokka"])
        .await
        .ok()
        .json();
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["matched"], 1);
    assert_eq!(filtered["loaded"], 2);
    let capped = env
        .t
        .hutan(&[
            "history", "sessions", "--json", "--limit", "1", "--sort", "created",
        ])
        .await
        .ok()
        .json();
    assert_eq!(capped["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(capped["matched"], 2);
    assert_eq!(capped["sessions"][0]["session_key"], "sess-b");
    let none = env
        .t
        .hutan(&["history", "sessions", "--source", "codex", "--refresh"])
        .await
        .ok();
    assert!(none.stdout.starts_with("no sessions"), "{none:?}");
    assert!(none.stdout.contains("0 of 2 sessions"), "{none:?}");

    // The session-output form of `hutan history` is untouched; `sessions` is the subcommand.
    let usage = env.t.hutan(&["history"]).await;
    assert_eq!(usage.code, 2, "{usage:?}");
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_sessions_scopes_take_a_workspace_and_reject_bad_input() {
    let env = start(true).await;
    let repo = tempfile::tempdir().unwrap();
    let repo_path = repo.path().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "first"]);
    let opened = env
        .t
        .hutan(&["project", "open", repo_path.to_str().unwrap(), "--json"])
        .await;
    assert_eq!(opened.code, 0, "{opened:?}");
    let name = repo_path.file_name().unwrap().to_str().unwrap().to_owned();

    append(
        &env.claude.join("-work-in/sess-in.jsonl"),
        &refs(&session_lines(
            repo_path.to_str().unwrap(),
            "2026-10-01T09:00:00.000Z",
            "inside the project",
            None,
        )),
    );
    append(
        &env.claude.join("-work-out/sess-out.jsonl"),
        &refs(&session_lines(
            "/nowhere/else",
            "2026-10-02T09:00:00.000Z",
            "outside any project",
            None,
        )),
    );
    sessions_until(&env, &[], 2).await;

    for scope in [&["--scope", "project", "--ws", &name][..], &["--ws", &name]] {
        let mut args = vec!["history", "sessions", "--json"];
        args.extend_from_slice(scope);
        let scoped = env.t.hutan(&args).await.ok().json();
        assert_eq!(scoped["matched"], 1, "{scope:?}");
        assert_eq!(scoped["loaded"], 2, "{scope:?}");
        assert_eq!(scoped["sessions"][0]["session_key"], "sess-in", "{scope:?}");
    }
    let human = env
        .t
        .hutan(&[
            "history",
            "sessions",
            "--filter",
            "repo:",
            "--scope",
            "workspace",
            "--ws",
            &name,
        ])
        .await
        .ok();
    assert!(
        human.stdout.contains(&name),
        "the project's name is a column: {human:?}"
    );

    for bad in [
        &["history", "sessions", "--scope", "project"][..],
        &["history", "sessions", "--limit", "0"],
        &["history", "sessions", "--limit", "1001"],
        &["history", "sessions", "--sort", "newest"],
        &["history", "sessions", "--scope", "everywhere"],
    ] {
        assert_eq!(env.t.hutan(bad).await.code, 2, "{bad:?}");
    }
    let unknown = env
        .t
        .hutan(&["history", "sessions", "--ws", "no-such-project"])
        .await;
    assert_eq!(unknown.code, 1, "{unknown:?}");
    env.t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn history_sessions_needs_indexing_to_be_on() {
    let env = start(false).await;
    let refused = env.t.hutan(&["history", "sessions"]).await;
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(refused.stderr.contains("hutan index enable"), "{refused:?}");
    env.t.stop().await;
}
