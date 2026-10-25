//! The session catalog (`history.sessions`, protocol 0.29) end to end: a real daemon on a
//! temporary `HUTAN_HOME` whose `daemon.toml` points the transcript roots at temporary
//! directories (never the user's real agent directories), git repositories the tests create
//! (never this repository), real `history.sessions` requests over the socket.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::*;
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::WorkspaceId;
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSend};
use hutan_protocol::history::{
    HistoryRole, HistoryScope, HistorySessionRow, HistorySessionSort, HistorySessions,
    HistorySessionsRequest, HistorySetEnabled, HistorySource, HistoryStatusInfo,
    SESSIONS_MAX_LIMIT,
};
use hutan_protocol::messages::{
    CreatedBy, Request, SessionResult, WorkspaceCreate, WorkspaceInfo, WorkspaceOpen,
    WorkspaceOpened,
};
use tokio::time::sleep;

// Fixtures ------------------------------------------------------------------------------------

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

struct Env {
    _tmp: tempfile::TempDir,
    /// The temporary directory, canonical (`/private/var/…`).
    base: PathBuf,
    daemon: TestDaemon,
}

impl Env {
    fn claude(&self) -> PathBuf {
        self.base.join("sources/claude")
    }
    fn codex(&self) -> PathBuf {
        self.base.join("sources/codex")
    }
    fn omp(&self) -> PathBuf {
        self.base.join("sources/omp")
    }

    /// A repository `<base>/work/<name>` on branch `main` with one commit.
    fn repo(&self, name: &str) -> PathBuf {
        let root = self.base.join("work").join(name);
        fs::create_dir_all(root.join("src")).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "a.txt"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        root
    }
}

fn toml(base: &Path, enabled: bool) -> String {
    format!(
        "[history]\nenabled = {enabled}\n[history.roots]\nclaude = \"{}\"\ncodex = \"{}\"\nomp = \"{}\"\n",
        base.join("sources/claude").display(),
        base.join("sources/codex").display(),
        base.join("sources/omp").display(),
    )
}

async fn start_with(enabled: bool, tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig)) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    for dir in ["claude", "codex", "omp"] {
        fs::create_dir_all(base.join("sources").join(dir)).unwrap();
    }
    let daemon = TestDaemon::start_with(&toml(&base, enabled), tweak).await;
    Env {
        _tmp: tmp,
        base,
        daemon,
    }
}

async fn start(enabled: bool) -> Env {
    start_with(enabled, |_| {}).await
}

fn append(path: &Path, lines: &[String]) {
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

fn user(ts: &str, cwd: &Path, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{ts}","cwd":"{}","gitBranch":"main","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#,
        cwd.display()
    )
}

fn assistant(ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","model":"claude-opus-5","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
}

fn claude_path(env: &Env, session: &str) -> PathBuf {
    env.claude().join(format!("-work/{session}.jsonl"))
}

async fn sessions(client: &Client, req: HistorySessionsRequest) -> HistorySessions {
    client.call(Request::HistorySessions(req)).await.unwrap()
}

fn req() -> HistorySessionsRequest {
    HistorySessionsRequest {
        scope: HistoryScope::All,
        workspace_id: None,
        filter: String::new(),
        sort: HistorySessionSort::Updated,
        sources: Vec::new(),
        limit: 250,
        refresh: false,
    }
}

fn keys(answer: &HistorySessions) -> Vec<&str> {
    answer
        .sessions
        .iter()
        .map(|row| row.session_key.as_str())
        .collect()
}

/// Polls `history.sessions` until the answer has `loaded` rows and no scan runs.
async fn wait_loaded(client: &Client, loaded: u64) -> HistorySessions {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = sessions(client, req()).await;
        if answer.loaded == loaded && !answer.scan.scanning {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {loaded} sessions, have {}",
            answer.loaded
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn open(client: &Client, path: &Path) -> WorkspaceInfo {
    client
        .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
            path: path.to_owned(),
            created_by: None,
        }))
        .await
        .unwrap()
        .workspace
}

fn row<'a>(answer: &'a HistorySessions, key: &str) -> &'a HistorySessionRow {
    answer
        .sessions
        .iter()
        .find(|row| row.session_key == key)
        .unwrap_or_else(|| panic!("no session {key} in {:?}", keys(answer)))
}

// Scopes, attribution, filter, sort -----------------------------------------------------------

/// Two projects (`app` with a linked worktree, `lib`), a third (`other`), a user workspace
/// `Platform` holding `app` and `lib`, and five transcripts across the three sources.
struct Fixture {
    env: Env,
    client: Client,
    app: WorkspaceInfo,
    lib: WorkspaceInfo,
    other: WorkspaceInfo,
    platform: WorkspaceInfo,
    app_path: PathBuf,
}

async fn fixture() -> Fixture {
    let env = start(true).await;
    let app_path = env.repo("app");
    let lib_path = env.repo("lib");
    let other_path = env.repo("other");
    let worktree = env.base.join("work/app-wt");
    git(
        &app_path,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    let client = env.daemon.connect().await;
    let app = open(&client, &app_path).await;
    let lib = open(&client, &lib_path).await;
    let other = open(&client, &other_path).await;
    let platform: WorkspaceInfo = client
        .call(Request::WorkspaceCreate(WorkspaceCreate {
            name: "Platform".to_owned(),
            paths: Vec::new(),
            project_ids: vec![app.id, lib.id],
            created_by: Some(CreatedBy::Cli),
        }))
        .await
        .unwrap();

    // c1: in a folder of `app`; created first, updated last of the app sessions.
    append(
        &claude_path(&env, "c1"),
        &[
            user(
                "2026-10-01T09:00:00.000Z",
                &app_path.join("src"),
                "why does the quokka scheduler stall",
            ),
            assistant("2026-10-10T09:00:05.000Z", "Checking the scheduler queue."),
            r#"{"type":"ai-title","aiTitle":"Scheduler stall"}"#.to_owned(),
        ],
    );
    // c2: in the linked worktree of `app`, a folder outside every root of ours.
    append(
        &claude_path(&env, "c2"),
        &[user(
            "2026-10-05T09:00:00.000Z",
            &worktree,
            "refactor the parser in the worktree",
        )],
    );
    // x1: Codex in `lib`.
    append(
        &env.codex().join("2026/10/06/rollout-2026-10-06T10-00-00-0198aaaa-bbbb-cccc-dddd-eeeeffff0000.jsonl"),
        &[
            format!(
                r#"{{"timestamp":"2026-10-06T10:00:00.000Z","type":"session_meta","payload":{{"id":"0198aaaa-bbbb-cccc-dddd-eeeeffff0000","cwd":"{}"}}}}"#,
                lib_path.display()
            ),
            r#"{"timestamp":"2026-10-06T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"add retry with jitter to the capybara client"}]}}"#.to_owned(),
        ],
    );
    // o1: OMP in `other`.
    append(
        &env.omp().join("-work-other/2026-10-07T11-00-00-000Z_0f4c7d52.jsonl"),
        &[
            r#"{"type":"title","v":1,"title":"Axolotl cache design","pad":"      "}"#.to_owned(),
            format!(
                r#"{{"type":"session","version":3,"id":"0f4c7d52","timestamp":"2026-10-07T11:00:00.000Z","cwd":"{}"}}"#,
                other_path.display()
            ),
            r#"{"type":"message","timestamp":"2026-10-07T11:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"sketch the axolotl eviction policy"}]}}"#.to_owned(),
        ],
    );
    // c3: a folder no project owns (and that does not exist any more).
    append(
        &claude_path(&env, "c3"),
        &[user(
            "2026-10-08T09:00:00.000Z",
            Path::new("/nonexistent/hutan-test/elsewhere"),
            "something about quokkas elsewhere",
        )],
    );
    wait_loaded(&client, 5).await;
    Fixture {
        env,
        client,
        app,
        lib,
        other,
        platform,
        app_path,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rows_carry_the_summary_attribution_and_resume_commands() {
    let f = fixture().await;
    // The scan counts come from a stat pass: the one at startup may have run before the
    // fixture wrote its files (the watcher reads them in without counting them), so ask for
    // another and have it answered when done.
    let all = sessions(
        &f.client,
        HistorySessionsRequest {
            refresh: true,
            ..req()
        },
    )
    .await;
    assert_eq!((all.matched, all.loaded), (5, 5));
    // Newest update first.
    assert_eq!(
        keys(&all),
        [
            "c1",
            "c3",
            "0f4c7d52",
            "0198aaaa-bbbb-cccc-dddd-eeeeffff0000",
            "c2"
        ]
    );

    let c1 = row(&all, "c1");
    assert_eq!(c1.source, HistorySource::Claude);
    assert_eq!(c1.title.as_deref(), Some("Scheduler stall"));
    assert_eq!(
        c1.first_prompt.as_deref(),
        Some("why does the quokka scheduler stall")
    );
    assert_eq!(c1.messages, 2);
    assert_eq!(c1.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(c1.branch.as_deref(), Some("main"));
    assert_eq!(
        c1.cwd.as_deref(),
        Some(f.app_path.join("src").to_str().unwrap())
    );
    assert_eq!(
        c1.workspace_id,
        Some(f.app.id),
        "a folder inside the project's root"
    );
    assert_eq!(c1.latest.len(), 2);
    assert_eq!(c1.latest[0].role, HistoryRole::User);
    assert_eq!(c1.latest[1].text, "Checking the scheduler queue.");
    assert_eq!(
        c1.resume_argv.as_deref(),
        Some(&["claude".to_owned(), "--resume".to_owned(), "c1".to_owned()][..])
    );
    let transcript = f.env.claude().join("-work/c1.jsonl");
    assert_eq!(
        c1.path.as_deref(),
        Some(transcript.canonicalize().unwrap().to_str().unwrap())
    );
    assert_eq!(c1.session_id, None);

    assert_eq!(
        row(&all, "c2").workspace_id,
        Some(f.app.id),
        "a linked worktree belongs to its repository's project"
    );
    assert_eq!(
        row(&all, "c3").workspace_id,
        None,
        "no project has that folder"
    );
    let codex = row(&all, "0198aaaa-bbbb-cccc-dddd-eeeeffff0000");
    assert_eq!(codex.workspace_id, Some(f.lib.id));
    assert_eq!(
        codex.resume_argv.as_ref().unwrap()[..2],
        ["codex", "resume"]
    );
    // OMP resumes by the transcript's absolute path.
    let omp = row(&all, "0f4c7d52");
    assert_eq!(omp.workspace_id, Some(f.other.id));
    assert_eq!(omp.title.as_deref(), Some("Axolotl cache design"));
    assert_eq!(
        omp.resume_argv.as_deref(),
        Some(
            &[
                "omp".to_owned(),
                format!("--resume={}", omp.path.as_deref().unwrap())
            ][..]
        )
    );
    assert!(omp.path.as_deref().unwrap().starts_with('/'));

    assert_eq!(all.scan.files, 5);
    assert_eq!(all.scan.done, 5);
    assert!(all.scan.finished_ms.is_some());
    let per_source: Vec<_> = all
        .scan
        .per_source
        .iter()
        .map(|s| (s.name.as_str(), s.files))
        .collect();
    assert_eq!(per_source, [("claude", 3), ("codex", 1), ("omp", 1)]);
    f.env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn scopes_restrict_by_project_and_by_workspace() {
    let f = fixture().await;
    let scoped = |scope, id: WorkspaceId| HistorySessionsRequest {
        scope,
        workspace_id: Some(id),
        ..req()
    };

    let app = sessions(&f.client, scoped(HistoryScope::Project, f.app.id)).await;
    assert_eq!(keys(&app), ["c1", "c2"]);
    assert_eq!(
        (app.matched, app.loaded),
        (2, 5),
        "loaded ignores the scope"
    );

    let lib = sessions(&f.client, scoped(HistoryScope::Project, f.lib.id)).await;
    assert_eq!(keys(&lib), ["0198aaaa-bbbb-cccc-dddd-eeeeffff0000"]);

    // The user workspace holds `app` and `lib`, not `other`, not the stray folder.
    let platform = sessions(&f.client, scoped(HistoryScope::Workspace, f.platform.id)).await;
    assert_eq!(
        keys(&platform),
        ["c1", "0198aaaa-bbbb-cccc-dddd-eeeeffff0000", "c2"]
    );
    assert_eq!(platform.matched, 3);

    // The workspace scope on one project is that project's folders (and worktrees).
    let other = sessions(&f.client, scoped(HistoryScope::Workspace, f.other.id)).await;
    assert_eq!(keys(&other), ["0f4c7d52"]);

    // The project scope of a workspace attributes nothing to it: its folders belong to the
    // projects inside.
    let none = sessions(&f.client, scoped(HistoryScope::Project, f.platform.id)).await;
    assert_eq!((none.matched, none.loaded), (0, 5));
    f.env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_sort_limit_and_sources() {
    let f = fixture().await;

    // Words are substrings over title, first prompt, latest turns, agent, model, branch, cwd,
    // session id; all must match.
    let by = |filter: &str| HistorySessionsRequest {
        filter: filter.to_owned(),
        ..req()
    };
    assert_eq!(keys(&sessions(&f.client, by("QUOKKA")).await), ["c1", "c3"]);
    assert_eq!(
        keys(&sessions(&f.client, by("quokka scheduler")).await),
        ["c1"]
    );
    assert_eq!(
        keys(&sessions(&f.client, by("quokka nothing")).await),
        Vec::<&str>::new()
    );
    assert_eq!(
        keys(&sessions(&f.client, by("codex jitter")).await).len(),
        1,
        "agent name"
    );
    assert_eq!(
        keys(&sessions(&f.client, by("opus-5 queue")).await),
        ["c1"],
        "model, latest turn"
    );
    assert_eq!(
        keys(&sessions(&f.client, by("\"axolotl cache\"")).await),
        ["0f4c7d52"],
        "quoted title"
    );
    assert_eq!(
        keys(&sessions(&f.client, by("0f4c7d52")).await),
        ["0f4c7d52"],
        "session id"
    );

    // `repo:` is the attributed project's name or folder; `path:` the cwd or transcript path.
    assert_eq!(
        keys(&sessions(&f.client, by("repo:app")).await),
        ["c1", "c2"]
    );
    assert_eq!(keys(&sessions(&f.client, by("repo:LIB")).await).len(), 1);
    assert_eq!(
        keys(&sessions(&f.client, by("repo:app quokka")).await),
        ["c1"]
    );
    assert!(
        sessions(&f.client, by("repo:nope"))
            .await
            .sessions
            .is_empty()
    );
    assert_eq!(
        keys(&sessions(&f.client, by("path:work/app-wt")).await),
        ["c2"],
        "cwd"
    );
    assert_eq!(
        keys(&sessions(&f.client, by("path:sources/omp")).await),
        ["0f4c7d52"],
        "transcript path"
    );
    assert_eq!(
        keys(&sessions(&f.client, by("path:\"/nonexistent/hutan\"")).await),
        ["c3"]
    );

    // A filter over 2 KiB matches nothing (and is no error).
    let huge = sessions(&f.client, by(&"quokka ".repeat(400))).await;
    assert_eq!((huge.matched, huge.loaded, huge.sessions.len()), (0, 5, 0));

    // Sorting: by the time of the last message, or of the first; ties by session id.
    let updated = sessions(&f.client, req()).await;
    assert_eq!(
        keys(&updated),
        [
            "c1",
            "c3",
            "0f4c7d52",
            "0198aaaa-bbbb-cccc-dddd-eeeeffff0000",
            "c2"
        ]
    );
    let created = sessions(
        &f.client,
        HistorySessionsRequest {
            sort: HistorySessionSort::Created,
            ..req()
        },
    )
    .await;
    assert_eq!(
        keys(&created),
        [
            "c3",
            "0f4c7d52",
            "0198aaaa-bbbb-cccc-dddd-eeeeffff0000",
            "c2",
            "c1"
        ]
    );

    // `matched` counts everything that matches, `sessions` only the first `limit`.
    let limited = sessions(&f.client, HistorySessionsRequest { limit: 2, ..req() }).await;
    assert_eq!(keys(&limited), ["c1", "c3"]);
    assert_eq!((limited.matched, limited.loaded), (5, 5));
    // A limit past the maximum is cut to it.
    let big = sessions(
        &f.client,
        HistorySessionsRequest {
            limit: SESSIONS_MAX_LIMIT + 5000,
            ..req()
        },
    )
    .await;
    assert_eq!(big.sessions.len(), 5);

    // Sources narrow `matched`, not `loaded`.
    let codex = sessions(
        &f.client,
        HistorySessionsRequest {
            sources: vec![HistorySource::Codex, HistorySource::Omp],
            ..req()
        },
    )
    .await;
    assert_eq!(
        keys(&codex),
        ["0f4c7d52", "0198aaaa-bbbb-cccc-dddd-eeeeffff0000"]
    );
    assert_eq!((codex.matched, codex.loaded), (2, 5));
    f.env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_requests_are_rejected() {
    let f = fixture().await;
    let code = |req: HistorySessionsRequest| {
        let client = f.client.clone();
        async move {
            remote_code(
                client
                    .call::<HistorySessions>(Request::HistorySessions(req))
                    .await,
            )
        }
    };
    assert_eq!(
        code(HistorySessionsRequest { limit: 0, ..req() }).await,
        ErrorCode::BadRequest
    );
    for scope in [HistoryScope::Project, HistoryScope::Workspace] {
        assert_eq!(
            code(HistorySessionsRequest {
                scope,
                workspace_id: None,
                ..req()
            })
            .await,
            ErrorCode::BadRequest,
            "{scope:?} needs a workspace"
        );
        assert_eq!(
            code(HistorySessionsRequest {
                scope,
                workspace_id: Some(WorkspaceId::new()),
                ..req()
            })
            .await,
            ErrorCode::NotFound
        );
    }
    f.env.daemon.stop().await;
}

// Refresh, scan state, disabled ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn refresh_looks_at_the_files_again_and_answers_when_done() {
    let env = start(true).await;
    let client = env.daemon.connect().await;
    let first = wait_loaded(&client, 0).await;
    assert_eq!((first.scan.files, first.scan.scanning), (0, false));
    let finished = first.scan.finished_ms.expect("the first scan finished");

    append(
        &claude_path(&env, "late"),
        &[user(
            "2026-10-09T09:00:00.000Z",
            Path::new("/work/x"),
            "a session that appeared",
        )],
    );
    sleep(Duration::from_millis(5)).await;
    let refreshed = sessions(
        &client,
        HistorySessionsRequest {
            refresh: true,
            ..req()
        },
    )
    .await;
    assert_eq!(keys(&refreshed), ["late"]);
    assert_eq!(
        (
            refreshed.scan.files,
            refreshed.scan.done,
            refreshed.scan.scanning
        ),
        (1, 1, false)
    );
    assert!(refreshed.scan.finished_ms.unwrap() > finished);
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_index_has_no_catalog() {
    let env = start(false).await;
    let client = env.daemon.connect().await;
    assert_eq!(
        remote_code(
            client
                .call::<HistorySessions>(Request::HistorySessions(req()))
                .await
        ),
        ErrorCode::InvalidState
    );
    // Enabling it makes the catalog answer.
    let status: HistoryStatusInfo = client
        .call(Request::HistorySetEnabled(HistorySetEnabled {
            enabled: true,
        }))
        .await
        .unwrap();
    assert!(status.enabled);
    assert_eq!(sessions(&client, req()).await.matched, 0);
    // And disabling it again takes it away.
    let _: HistoryStatusInfo = client
        .call(Request::HistorySetEnabled(HistorySetEnabled {
            enabled: false,
        }))
        .await
        .unwrap();
    assert_eq!(
        remote_code(
            client
                .call::<HistorySessions>(Request::HistorySessions(req()))
                .await
        ),
        ErrorCode::InvalidState
    );
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_capability_is_advertised() {
    let env = start(true).await;
    let client = env.daemon.connect().await;
    assert!(client.supports_history_sessions());
    env.daemon.stop().await;
}

// Hutan's own sessions ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn hutan_terminals_are_rows_with_their_session_title_and_project() {
    let env = start(true).await;
    let app_path = env.repo("app");
    let client = env.daemon.connect().await;
    let project = open(&client, &app_path).await;
    let session = create_in(
        &client,
        None,
        hutan_protocol::messages::TerminalSpec {
            cwd: Some(app_path.clone()),
            ..sh("printf '\\033]133;A\\007$ \\033]133;B\\007cargo test -p daemon\\r\\n\\033]133;C\\007all green\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat")
        },
    )
    .await;

    let deadline = Instant::now() + WAIT;
    let found = loop {
        let answer = sessions(&client, req()).await;
        if let Some(found) = answer
            .sessions
            .iter()
            .find(|r| r.source == HistorySource::Hutan && r.messages > 0)
        {
            break found.clone();
        }
        assert!(Instant::now() < deadline, "no Hutan row in time");
        sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(found.session_key, session.id.to_string());
    assert_eq!(found.session_id, Some(session.id));
    assert!(
        found.title.as_deref().is_some_and(|t| !t.is_empty()),
        "the session's title"
    );
    assert_eq!(found.workspace_id, Some(project.id));
    assert_eq!(found.latest.last().unwrap().role, HistoryRole::Command);
    assert_eq!(found.latest.last().unwrap().text, "cargo test -p daemon");
    assert_eq!((found.path, found.resume_argv), (None, None));

    // Scoped to the project it runs in; not in another one.
    let scoped = sessions(
        &client,
        HistorySessionsRequest {
            scope: HistoryScope::Project,
            workspace_id: Some(project.id),
            filter: "cargo test".to_owned(),
            ..req()
        },
    )
    .await;
    assert_eq!(keys(&scoped), [session.id.to_string()]);
    let hutan_only = sessions(
        &client,
        HistorySessionsRequest {
            sources: vec![HistorySource::Claude],
            ..req()
        },
    )
    .await;
    assert_eq!((hutan_only.matched, hutan_only.loaded), (0, 1));

    stop(&client, session.id).await.unwrap();
    env.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_hutan_session_stays_a_row_marked_closed_until_it_is_reopened() {
    let env = start(true).await;
    let app_path = env.repo("app");
    let client = env.daemon.connect().await;
    let project = open(&client, &app_path).await;
    let session = create_in(
        &client,
        None,
        hutan_protocol::messages::TerminalSpec {
            cwd: Some(app_path.clone()),
            title: Some("dev server".to_owned()),
            ..sh("printf '\\033]133;A\\007$ \\033]133;B\\007cargo test -p daemon\\r\\n\\033]133;C\\007all green\\r\\n\\033]133;D;0\\007\\033]133;A\\007$ \\033]133;B\\007'; exec cat")
        },
    )
    .await;
    let key = session.id.to_string();
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = sessions(&client, req()).await;
        if answer
            .sessions
            .iter()
            .any(|r| r.source == HistorySource::Hutan && r.messages > 0)
        {
            break;
        }
        assert!(Instant::now() < deadline, "no Hutan row in time");
        sleep(Duration::from_millis(50)).await;
    }
    assert!(!row(&sessions(&client, req()).await, &key).closed);

    client
        .call::<hutan_protocol::messages::Empty>(Request::SessionClose(
            hutan_protocol::messages::SessionClose {
                session_id: session.id,
                stop_running: true,
            },
        ))
        .await
        .unwrap();
    // Still there, still its own title and project, and marked closed.
    let scoped = sessions(
        &client,
        HistorySessionsRequest {
            scope: HistoryScope::Project,
            workspace_id: Some(project.id),
            ..req()
        },
    )
    .await;
    let found = row(&scoped, &key);
    assert!(found.closed);
    assert_eq!(found.session_id, Some(session.id));
    assert_eq!(found.title.as_deref(), Some("dev server"));
    assert_eq!(found.workspace_id, Some(project.id));

    client
        .call::<hutan_protocol::messages::SessionResult>(Request::SessionReopen(
            hutan_protocol::messages::SessionRef {
                session_id: session.id,
            },
        ))
        .await
        .unwrap();
    assert!(!row(&sessions(&client, req()).await, &key).closed);
    stop(&client, session.id).await.unwrap();
    env.daemon.stop().await;
}

const FIXTURE_BIN_NAME: &str = "hutan-codex-fixture";

/// The fake codex app server, next to the test binaries (`just test` builds the workspace
/// binaries first).
fn codex_replaying(transcript: &Path, received: &Path) -> CodexConfig {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join(FIXTURE_BIN_NAME);
    assert!(
        bin.is_file(),
        "{FIXTURE_BIN_NAME} not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
    );
    CodexConfig {
        program: bin,
        args: vec![
            "replay".to_owned(),
            "--received".to_owned(),
            received.display().to_string(),
            transcript.display().to_string(),
            "app-server".to_owned(),
        ],
        env: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hutan_chats_are_rows_with_their_conversation() {
    let work = tempfile::tempdir().unwrap();
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/codex-0.162/plain-turn.jsonl");
    let config = codex_replaying(&transcript, &work.path().join("received.jsonl"));
    let env = start_with(true, move |c| c.codex = Some(config)).await;
    let client = env.daemon.connect().await;
    let chat = client
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: work.path().to_owned(),
            agent: "codex".to_owned(),
            model: None,
            approval_policy: None,
            sandbox: None,
            title: Some("Ping pong".to_owned()),
            mode: None,
            provider_id: None,
            system_prompt: None,
            account: None,
        }))
        .await
        .unwrap()
        .session;
    client
        .call::<hutan_protocol::chat::ChatSent>(Request::ChatSend(ChatSend {
            session_id: chat.id,
            text: "Reply with the single word pong and nothing else.".to_owned(),
            attachments: vec![],
        }))
        .await
        .unwrap();

    let deadline = Instant::now() + WAIT;
    let found = loop {
        let answer = sessions(&client, req()).await;
        if let Some(found) = answer
            .sessions
            .iter()
            .find(|r| r.session_id == Some(chat.id) && r.messages >= 2)
        {
            break found.clone();
        }
        assert!(Instant::now() < deadline, "no chat row in time");
        sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(found.source, HistorySource::Hutan);
    assert_eq!(found.title.as_deref(), Some("Ping pong"));
    assert_eq!(
        found.first_prompt.as_deref(),
        Some("Reply with the single word pong and nothing else.")
    );
    let roles: Vec<_> = found.latest.iter().map(|t| t.role).collect();
    assert_eq!(roles, [HistoryRole::User, HistoryRole::Assistant]);
    assert_eq!(found.latest[1].text, "pong");
    env.daemon.stop().await;
}
