//! Checkpoints of chat turns (ADR 0019, slice CK-2), end to end: a real daemon whose Codex is
//! `hutan-codex-fixture replay` working in a git repository the test creates (the test edits
//! the files while the replay holds the turn open, as the agent would), and the service on its
//! own for what a fixture cannot show (a slow git, a crash, retention bounds).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{
    AgentEventKind, CheckpointId, CheckpointKind, CheckpointState, Seq, SessionId, TurnId,
};
use hutan_daemon::checkpoints::turns::{STOPPED_REASON, Scope, TurnCheckpoints, Tuning};
use hutan_daemon::config::{CheckpointMode, CheckpointSettings};
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::daemon_settings::DaemonSettingSet;
use hutan_protocol::messages::{Empty, Event, Request, SessionInfo, SessionRef, SessionResult};
use hutan_storage::{CheckpointFilter, CheckpointRow, Storage};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

// Git ----------------------------------------------------------------------------------------

/// Git without the user's configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"])
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

/// The environment of the daemon's git calls: no user configuration either.
fn git_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ])
}

/// `<dir>/repo`: a repository on `main` with one commit of `a.txt`.
fn repository(dir: &Path) -> PathBuf {
    let base = fs::canonicalize(dir).unwrap();
    git(&base, &["init", "-q", "-b", "main", "repo"]);
    let root = base.join("repo");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

/// The checkpoint refs of the repository at `root`.
fn refs(root: &Path) -> Vec<String> {
    git(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/hutan/"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

/// A `git` that waits at `commit-tree` while `HUTAN_TEST_GIT_GATE` names a file that does not
/// exist, then runs the real git. Written once per content (macOS vets each new executable).
fn slow_git_dir() -> PathBuf {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_owned();
    let script = format!(
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = commit-tree ] && [ -n \"$HUTAN_TEST_GIT_GATE\" ]; then\n    while [ ! -e \"$HUTAN_TEST_GIT_GATE\" ]; do sleep 0.02; done\n  fi\ndone\nexec {real} \"$@\"\n"
    );
    let mut hasher = DefaultHasher::new();
    script.hash(&mut hasher);
    let dir = std::env::temp_dir()
        .join("hutan-test-fake-git")
        .join(format!("{:016x}", hasher.finish()));
    let path = dir.join("git");
    if fs::read_to_string(&path).is_ok_and(|have| have == script) {
        return dir;
    }
    fs::create_dir_all(&dir).unwrap();
    let staging = dir.join(format!(".git-{}", std::process::id()));
    fs::write(&staging, &script).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&staging, &path).unwrap();
    dir
}

// The service on its own ---------------------------------------------------------------------

struct Service {
    _dir: tempfile::TempDir,
    root: PathBuf,
    base: PathBuf,
    storage: Storage,
    turns: Arc<TurnCheckpoints>,
    session: SessionId,
}

impl Service {
    fn new(settings: CheckpointSettings, tuning: Tuning) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = repository(dir.path());
        let base = fs::canonicalize(dir.path()).unwrap().join("home");
        let storage = Storage::open(base.join("hutan.sqlite")).unwrap();
        let turns = TurnCheckpoints::new(storage.clone(), &base, None, settings, tuning);
        let session = SessionId::new();
        turns.open(session, root.clone(), Scope::default());
        Self {
            _dir: dir,
            root,
            base,
            storage,
            turns,
            session,
        }
    }

    fn quick() -> Self {
        Self::new(
            CheckpointSettings::default(),
            Tuning {
                git_env: git_env(),
                ..Tuning::default()
            },
        )
    }

    /// A second service on the same database and store: the next daemon.
    fn next_daemon(&self, settings: CheckpointSettings) -> Arc<TurnCheckpoints> {
        TurnCheckpoints::new(
            self.storage.clone(),
            &self.base,
            None,
            settings,
            Tuning {
                git_env: git_env(),
                ..Tuning::default()
            },
        )
    }

    fn rows(&self) -> Vec<CheckpointRow> {
        self.storage
            .checkpoints()
            .list(CheckpointFilter::Session(self.session))
            .unwrap()
    }

    fn row(&self, id: CheckpointId) -> CheckpointRow {
        self.storage.checkpoints().get(id).unwrap().unwrap()
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// One turn: its start, `edit`, its end, recorded.
    async fn turn(&self, edit: impl FnOnce(&Self)) -> CheckpointRow {
        let id = self.turns.begin(self.session).await.expect("a checkpoint");
        let turn = TurnId::new();
        self.turns.sent(self.session, id, turn);
        edit(self);
        self.turns.turn_ended(self.session, Some(turn));
        self.turns.drain(WAIT).await;
        self.row(id)
    }

    fn common_dir(&self) -> PathBuf {
        self.root.join(".git")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_start_snapshot_never_holds_the_prompt_past_its_budget() {
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("open");
    let mut env = git_env();
    let fake = slow_git_dir();
    // The first run of a new executable is vetted by macOS (seconds): not within the budget.
    assert!(
        Command::new(fake.join("git"))
            .arg("--version")
            .output()
            .unwrap()
            .status
            .success()
    );
    let path = format!(
        "{}:{}",
        fake.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    env.insert("PATH".to_owned(), path);
    env.insert(
        "HUTAN_TEST_GIT_GATE".to_owned(),
        gate.display().to_string(),
    );
    let s = Service::new(
        CheckpointSettings::default(),
        Tuning {
            start_budget: Duration::from_secs(1),
            git_env: env,
        },
    );

    // The start snapshot stops at `commit-tree` until the gate opens: `begin` returns after
    // its budget all the same, with the snapshot still waiting.
    let id = timeout(WAIT, s.turns.begin(s.session))
        .await
        .expect("the prompt is not held past the budget")
        .expect("a checkpoint");
    assert!(!gate.exists());
    assert_eq!(s.row(id).state, CheckpointState::Running);
    assert!(refs(&s.root).is_empty(), "{:?}", refs(&s.root));

    let turn = TurnId::new();
    s.turns.sent(s.session, id, turn);
    s.write("a.txt", "edited by the agent\n");
    s.turns.turn_ended(s.session, Some(turn));
    fs::write(&gate, "").unwrap();
    s.turns.drain(WAIT).await;

    let row = s.row(id);
    assert_eq!(row.state, CheckpointState::Late, "{row:?}");
    assert_eq!(row.turn_id, Some(turn));
    assert!(
        row.reason.as_deref().unwrap().contains("after the prompt"),
        "{row:?}"
    );
    // The start ref is gone; at most the end ref is left.
    let left = refs(&s.root);
    assert!(left.iter().all(|r| !r.ends_with("-start")), "{left:?}");
    assert_eq!(left, row.reference.into_iter().collect::<Vec<_>>());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_keeps_one_ref_whose_parent_is_its_start_and_an_unchanged_turn_keeps_none() {
    let s = Service::quick();
    let changed = s
        .turn(|s| {
            s.write("a.txt", "edited\n");
            s.write("new/b.txt", "one\ntwo\n");
        })
        .await;
    assert_eq!(changed.state, CheckpointState::Ok, "{changed:?}");
    assert_eq!(changed.files_changed, 2);
    assert_eq!((changed.lines_added, changed.lines_removed), (3, 1));
    let reference = changed.reference.clone().unwrap();
    assert_eq!(
        reference,
        format!(
            "refs/hutan/checkpoints/{}/{}",
            s.session,
            changed.turn_id.unwrap()
        )
    );
    assert_eq!(refs(&s.root), vec![reference.clone()]);
    let commit = changed.commit.clone().unwrap();
    assert_eq!(git(&s.root, &["rev-parse", &reference]).trim(), commit);
    assert_eq!(
        git(&s.root, &["rev-parse", &format!("{commit}^")]).trim(),
        changed.start_commit.clone().unwrap()
    );
    let message = git(&s.root, &["cat-file", "commit", &commit]);
    assert!(
        message.contains(&format!(
            "Hutan-Turn: {}\nHutan-Kind: end\nHutan-Checkpoint: {}\n",
            changed.turn_id.unwrap(),
            changed.id
        )),
        "{message}"
    );
    assert_eq!(changed.checkout.as_deref(), Some(s.root.as_path()));
    assert_eq!(changed.common_dir, Some(s.common_dir()));

    let unchanged = s.turn(|_| {}).await;
    assert_eq!(unchanged.state, CheckpointState::Unchanged);
    assert_eq!((unchanged.reference, unchanged.commit), (None, None));
    assert_eq!(refs(&s.root), vec![reference]);
    // The user's branch and index are where they were.
    assert_eq!(git(&s.root, &["rev-list", "--count", "HEAD"]).trim(), "1");
    assert_eq!(
        git(&s.root, &["status", "--porcelain"]),
        " M a.txt\n?? new/\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_off_switch_applies_to_the_next_snapshot_and_to_a_running_turn() {
    let s = Service::quick();
    let mut off = CheckpointSettings::default();
    off.mode = CheckpointMode::Off;

    // Off before the turn: no snapshot, no row.
    s.turns.apply_settings(off.clone());
    assert!(s.turns.begin(s.session).await.is_none());
    assert!(s.rows().is_empty());
    assert_eq!(
        s.turns.status(s.session).unwrap().state,
        hutan_core::CheckpointAvailability::Off
    );

    // Off during a turn: its end is not taken and its start goes.
    s.turns.apply_settings(CheckpointSettings::default());
    let id = s.turns.begin(s.session).await.unwrap();
    assert_eq!(refs(&s.root).len(), 1);
    s.turns.apply_settings(off);
    s.write("a.txt", "edited\n");
    s.turns.turn_ended(s.session, Some(TurnId::new()));
    s.turns.drain(WAIT).await;
    let row = s.row(id);
    assert_eq!(row.state, CheckpointState::Off);
    assert!(refs(&s.root).is_empty());

    // A project of its own can be off while the rest is on.
    let mut project = CheckpointSettings::default();
    project.projects.insert(
        "web".to_owned(),
        hutan_daemon::config::CheckpointOverride {
            mode: Some(CheckpointMode::Off),
            ..Default::default()
        },
    );
    s.turns.apply_settings(project);
    let web = SessionId::new();
    s.turns.open(
        web,
        s.root.clone(),
        Scope {
            project_id: "01PROJECT".to_owned(),
            project_name: "web".to_owned(),
        },
    );
    assert!(s.turns.begin(web).await.is_none());
    assert!(s.turns.begin(s.session).await.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_outside_git_and_the_home_directory_are_recorded_as_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let plain = base.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let home = repository(&base);
    let storage = Storage::open(base.join("hutan.sqlite")).unwrap();
    let turns = TurnCheckpoints::new(
        storage.clone(),
        &base.join("home"),
        Some(home.clone()),
        CheckpointSettings::default(),
        Tuning {
            git_env: git_env(),
            ..Tuning::default()
        },
    );
    for (folder, says) in [
        (&plain, "is not in a git repository"),
        (&home, "the home directory or a volume root"),
    ] {
        let session = SessionId::new();
        turns.open(session, folder.clone(), Scope::default());
        assert!(turns.begin(session).await.is_none());
        let rows = storage
            .checkpoints()
            .list(CheckpointFilter::Session(session))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, CheckpointState::Unsupported);
        assert!(
            rows[0].reason.as_deref().unwrap().contains(says),
            "{:?}",
            rows[0].reason
        );
        assert_eq!(rows[0].reference, None);
        let status = turns.status(session).unwrap();
        assert_eq!(
            status.state,
            hutan_core::CheckpointAvailability::Unsupported
        );
        assert!(status.reason.unwrap().contains(says));
    }
    assert!(refs(&home).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_drops_the_oldest_turns_and_keeps_the_pre_restore_checkpoint() {
    let mut settings = CheckpointSettings::default();
    settings.keep_turns = 2;
    let s = Service::new(
        settings.clone(),
        Tuning {
            git_env: git_env(),
            ..Tuning::default()
        },
    );
    let first = s.turn(|s| s.write("a.txt", "1\n")).await;
    // A restore (CK-4) of the first turn, older than every turn after it.
    let restore_ref = format!("refs/hutan/checkpoints/{}/{}-r1", s.session, first.turn_id.unwrap());
    git(
        &s.root,
        &["update-ref", &restore_ref, first.commit.as_deref().unwrap()],
    );
    let mut restore = CheckpointRow::new(
        CheckpointId::new(),
        s.session,
        CheckpointKind::Restore,
        CheckpointState::Ok,
        first.created_ms + 1,
    );
    restore.common_dir = Some(s.common_dir());
    restore.reference = Some(restore_ref.clone());
    restore.bytes = 10;
    s.storage.checkpoints().upsert(restore.clone()).unwrap();

    let second = s.turn(|s| s.write("a.txt", "2\n")).await;
    let third = s.turn(|s| s.write("a.txt", "3\n")).await;
    // Retention ran after each end; one more run waits for those.
    s.turns.retain(Some(s.common_dir())).await;
    let ids: Vec<CheckpointId> = s.rows().iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![restore.id, second.id, third.id]);
    let mut left = refs(&s.root);
    left.sort();
    let mut want = vec![
        restore_ref.clone(),
        second.reference.clone().unwrap(),
        third.reference.clone().unwrap(),
    ];
    want.sort();
    assert_eq!(left, want);

    // Down to one ref per repository: every turn goes before the restore does.
    settings.max_repo_refs = 1;
    s.turns.apply_settings(settings);
    s.turns.retain(Some(s.common_dir())).await;
    let ids: Vec<CheckpointId> = s.rows().iter().map(|r| r.id).collect();
    assert_eq!(ids, vec![restore.id]);
    assert_eq!(refs(&s.root), vec![restore_ref]);
}

#[tokio::test(flavor = "multi_thread")]
async fn many_loose_checkpoint_refs_are_packed_and_the_users_refs_are_left_loose() {
    let s = Service::quick();
    let head = git(&s.root, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&s.root, &["tag", "v1"]);
    let mut input = String::new();
    for n in 0..201 {
        input.push_str(&format!(
            "create refs/hutan/checkpoints/{}/{n} {head}\n",
            s.session
        ));
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&s.root)
        .args(["update-ref", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    let loose = s.common_dir().join("refs/hutan/checkpoints").join(s.session.to_string());
    assert_eq!(fs::read_dir(&loose).unwrap().count(), 201);

    s.turns.retain(Some(s.common_dir())).await;
    assert!(
        !loose.exists() || fs::read_dir(&loose).unwrap().count() == 0,
        "the checkpoint refs are packed"
    );
    let packed = fs::read_to_string(s.common_dir().join("packed-refs")).unwrap();
    assert!(packed.contains(&format!("refs/hutan/checkpoints/{}/200", s.session)));
    assert!(!packed.contains("refs/tags/v1"), "{packed}");
    assert!(s.common_dir().join("refs/tags/v1").exists());
    assert!(!packed.contains("refs/heads/main"), "{packed}");
    assert_eq!(refs(&s.root).len(), 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_next_daemon_reconciles_rows_and_refs() {
    let s = Service::quick();
    let backdate = |row: &CheckpointRow| {
        let mut row = row.clone();
        row.created_ms -= 60_000;
        s.storage.checkpoints().upsert(row.clone()).unwrap();
        row
    };

    // A turn whose end ref was deleted behind Hutan's back: its row goes.
    let lost = s.turn(|s| s.write("a.txt", "lost\n")).await;
    backdate(&lost);
    git(&s.root, &["update-ref", "-d", lost.reference.as_deref().unwrap()]);

    // A turn whose row was lost: its ref is adopted from the trailers.
    let orphan = s.turn(|s| s.write("a.txt", "orphan\n")).await;
    s.storage.checkpoints().delete(vec![orphan.id]).unwrap();

    // A daemon that died after writing a turn's end ref and before recording it.
    let ended = s.turn(|s| s.write("a.txt", "ended\n")).await;
    let mut running = backdate(&ended);
    running.state = CheckpointState::Running;
    running.reference = Some(format!(
        "refs/hutan/checkpoints/{}/{}-start",
        s.session, ended.id
    ));
    running.commit = None;
    s.storage.checkpoints().upsert(running).unwrap();

    // A daemon that died during a turn: its start ref and a `running` row.
    let cut = s.turns.begin(s.session).await.unwrap();
    let cut_row = backdate(&s.row(cut));
    let cut_start = cut_row.reference.clone().unwrap();
    assert!(refs(&s.root).contains(&cut_start));
    // And a start ref nobody knows.
    let head = git(&s.root, &["rev-parse", "HEAD"]).trim().to_owned();
    let stray = format!("refs/hutan/checkpoints/{}/{}-start", s.session, CheckpointId::new());
    git(&s.root, &["update-ref", &stray, &head]);

    let next = s.next_daemon(CheckpointSettings::default());
    next.reconcile().await;

    assert!(s.storage.checkpoints().get(lost.id).unwrap().is_none());
    let adopted = s.row(orphan.id);
    assert_eq!(adopted.state, CheckpointState::Ok);
    assert_eq!(adopted.reference, orphan.reference);
    assert_eq!(adopted.turn_id, orphan.turn_id);
    assert_eq!(adopted.start_commit, orphan.start_commit);
    assert_eq!(adopted.files_changed, 1);
    let recovered = s.row(ended.id);
    assert_eq!(recovered.state, CheckpointState::Ok);
    assert_eq!(recovered.reference, ended.reference);
    assert_eq!(recovered.commit, ended.commit);
    let failed = s.row(cut);
    assert_eq!(failed.state, CheckpointState::Failed);
    assert_eq!(failed.reason.as_deref(), Some(STOPPED_REASON));
    assert_eq!(failed.reference, None);
    let left = refs(&s.root);
    assert!(!left.contains(&cut_start), "{left:?}");
    assert!(!left.contains(&stray), "{left:?}");
    let mut want = vec![orphan.reference.unwrap(), ended.reference.unwrap()];
    want.sort();
    let mut left = left;
    left.sort();
    assert_eq!(left, want);
}

// Through a daemon and a fixture chat -----------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162")
}

fn codex(transcript: &Path, received: &Path) -> CodexConfig {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-codex-fixture");
    assert!(
        bin.is_file(),
        "hutan-codex-fixture not built: run `cargo build -p hutan-agents --bins` (or `just test`)"
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

fn hold(gate: &Path) -> String {
    json!({"dir": "hold", "file": gate}).to_string()
}

/// `plain-turn` with its turn repeated once per gate, each held after `turn/started` until its
/// gate exists (the test edits files meanwhile, as the agent would). Each turn has ids of its
/// own.
fn held_turns(dir: &Path, gates: &[PathBuf]) -> PathBuf {
    let real = fs::read_to_string(fixtures().join("plain-turn.jsonl")).unwrap();
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
                out.push(hold(gate));
            }
        }
    }
    let transcript = dir.join("held-turns.jsonl");
    fs::write(&transcript, out.join("\n") + "\n").unwrap();
    transcript
}

/// A recorded fixture with a hold after `turn/started`.
fn held_fixture(dir: &Path, name: &str, gate: &Path) -> PathBuf {
    let real = fs::read_to_string(fixtures().join(format!("{name}.jsonl"))).unwrap();
    let mut out = Vec::new();
    for line in real.lines() {
        out.push(line.to_owned());
        if line.contains("\"method\":\"turn/started\"") {
            out.push(hold(gate));
        }
    }
    let transcript = dir.join(format!("{name}-held.jsonl"));
    fs::write(&transcript, out.join("\n") + "\n").unwrap();
    transcript
}

struct Chat {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    root: PathBuf,
    client: Client,
    events: broadcast::Receiver<(Seq, Event)>,
}

/// A row of the `checkpoints` table, read by the test.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    turn: Option<String>,
    state: String,
    reason: Option<String>,
    reference: Option<String>,
    start_commit: Option<String>,
    commit: Option<String>,
    files: i64,
}

impl Chat {
    /// A daemon whose Codex replays `transcript(work dir)`, in a fresh repository.
    async fn start(transcript: impl FnOnce(&Path) -> PathBuf) -> Self {
        Self::start_with(transcript, |_, _| {}).await
    }

    async fn start_with(
        transcript: impl FnOnce(&Path) -> PathBuf,
        tweak: impl FnOnce(&mut hutan_daemon::DaemonConfig, &Path),
    ) -> Self {
        let work = tempfile::tempdir().unwrap();
        let root = repository(work.path());
        let config = codex(&transcript(work.path()), &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with("", |c| {
            c.codex = Some(config);
            c.checkpoints.git_env = git_env();
            tweak(c, &root);
        })
        .await;
        let (client, events) = daemon.connect_subscribed().await;
        Self {
            daemon,
            work,
            root,
            client,
            events,
        }
    }

    fn gate(&self, n: usize) -> PathBuf {
        self.work.path().join(format!("gate-{n}"))
    }

    async fn create_in(&self, cwd: &Path) -> SessionInfo {
        self.client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                workspace_id: None,
                cwd: cwd.to_owned(),
                agent: "codex".to_owned(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: None,
                mode: None,
                provider_id: None,
                system_prompt: None,
            }))
            .await
            .unwrap()
            .session
    }

    async fn create(&self) -> SessionInfo {
        self.create_in(&self.root).await
    }

    async fn send(&self, id: SessionId, text: &str) -> TurnId {
        self.client
            .call::<ChatSent>(Request::ChatSend(ChatSend {
                session_id: id,
                text: text.to_owned(),
                attachments: vec![],
            }))
            .await
            .unwrap()
            .turn_id
    }

    async fn turn_end(&mut self) -> AgentEventKind {
        wait_event(&mut self.events, "the turn's end", |event| match event {
            Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(event.kind.clone()),
            _ => None,
        })
        .await
    }

    fn rows(&self, session: SessionId) -> Vec<Row> {
        rows_in(&self.daemon.paths.database, session)
    }

    /// The session's rows once `ready` holds for them.
    async fn rows_when(&self, session: SessionId, ready: impl Fn(&[Row]) -> bool) -> Vec<Row> {
        timeout(WAIT, async {
            loop {
                let rows = self.rows(session);
                if ready(&rows) {
                    return rows;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("checkpoints never got there: {:?}", self.rows(session)))
    }

    async fn set(&self, key: &str, value: Value) {
        self.client
            .call::<hutan_protocol::daemon_settings::DaemonSettings>(Request::DaemonSettingsSet(
                DaemonSettingSet {
                    key: key.to_owned(),
                    value: Some(value),
                },
            ))
            .await
            .unwrap();
    }
}

fn rows_in(database: &Path, session: SessionId) -> Vec<Row> {
    let db = rusqlite::Connection::open(database).unwrap();
    let mut stmt = db
        .prepare(
            "SELECT id, turn_id, state, reason, ref, start_commit, commit_id, files_changed \
             FROM checkpoints WHERE session_id = ?1 ORDER BY created_ms, id",
        )
        .unwrap();
    stmt.query_map([session.to_string()], |r| {
        Ok(Row {
            id: r.get(0)?,
            turn: r.get(1)?,
            state: r.get(2)?,
            reason: r.get(3)?,
            reference: r.get(4)?,
            start_commit: r.get(5)?,
            commit: r.get(6)?,
            files: r.get(7)?,
        })
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

fn settled(rows: &[Row], count: usize) -> bool {
    rows.len() == count && rows.iter().all(|r| r.state != "running")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixture_chat_gets_one_ref_per_turn_that_changed_files_and_none_for_a_read_only_turn() {
    let mut t = Chat::start(|dir| {
        let gates: Vec<PathBuf> = (0..3).map(|n| dir.join(format!("gate-{n}"))).collect();
        held_turns(dir, &gates)
    })
    .await;
    let session = t.create().await.id;

    // Turn 1 edits a tracked file; turn 2 only reads; turn 3 adds a file in a new folder.
    let mut turns = Vec::new();
    for n in 0..3 {
        let turn = t.send(session, &format!("turn {n}")).await;
        turns.push(turn);
        match n {
            0 => fs::write(t.root.join("a.txt"), "edited by the agent\n").unwrap(),
            2 => {
                fs::create_dir_all(t.root.join("src")).unwrap();
                fs::write(t.root.join("src/new.rs"), "fn main() {}\n").unwrap();
            }
            _ => {}
        }
        fs::write(t.gate(n), "").unwrap();
        assert!(matches!(
            t.turn_end().await,
            AgentEventKind::TurnCompleted { .. }
        ));
        t.rows_when(session, |rows| settled(rows, n + 1)).await;
    }

    let rows = t.rows(session);
    let states: Vec<&str> = rows.iter().map(|r| r.state.as_str()).collect();
    assert_eq!(states, ["ok", "unchanged", "ok"], "{rows:?}");
    for (row, turn) in rows.iter().zip(&turns) {
        assert_eq!(row.turn.as_deref(), Some(turn.to_string().as_str()));
    }
    assert_eq!((rows[0].files, rows[1].files, rows[2].files), (1, 0, 1));
    assert_eq!(rows[1].reference, None);
    let mut want: Vec<String> = [0, 2]
        .iter()
        .map(|n| format!("refs/hutan/checkpoints/{session}/{}", turns[*n]))
        .collect();
    want.sort();
    let mut have = refs(&t.root);
    have.sort();
    assert_eq!(have, want, "one ref per turn that changed files, no start ref left");
    for n in [0, 2] {
        let commit = rows[n].commit.clone().unwrap();
        assert_eq!(
            git(&t.root, &["rev-parse", &format!("{commit}^")]).trim(),
            rows[n].start_commit.clone().unwrap()
        );
    }
    // The end of turn 1 has the agent's edit; its start does not.
    let end = rows[0].commit.clone().unwrap();
    assert_eq!(
        git(&t.root, &["show", &format!("{end}:a.txt")]),
        "edited by the agent\n"
    );
    assert_eq!(git(&t.root, &["show", &format!("{end}^:a.txt")]), "alpha\n");
    // The user's history is untouched: the branch has its one commit.
    assert_eq!(git(&t.root, &["rev-list", "--count", "--branches"]).trim(), "1");
    t.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_and_an_interrupted_turn_still_get_their_end() {
    // Failed: the edit lands while the turn is held, then the turn fails.
    let mut t = Chat::start(|dir| held_fixture(dir, "turn-failed", &dir.join("gate-0"))).await;
    let session = t.create().await.id;
    t.send(session, "hi").await;
    fs::write(t.root.join("a.txt"), "half done\n").unwrap();
    fs::write(t.gate(0), "").unwrap();
    assert!(matches!(
        t.turn_end().await,
        AgentEventKind::TurnFailed { .. }
    ));
    let rows = t.rows_when(session, |rows| settled(rows, 1)).await;
    assert_eq!(rows[0].state, "ok", "{rows:?}");
    assert_eq!(rows[0].files, 1);
    assert!(rows[0].reference.is_some());
    t.daemon.stop().await;

    // Interrupted by the user: the replay streams until Hutan asks it to stop.
    let mut t = Chat::start(|_| fixtures().join("interrupt.jsonl")).await;
    let session = t.create().await.id;
    t.send(session, "Write the numbers from 1 to 300").await;
    fs::write(t.root.join("b.txt"), "written before the interrupt\n").unwrap();
    t.client
        .call::<Empty>(Request::ChatInterrupt(SessionRef {
            session_id: session,
        }))
        .await
        .unwrap();
    assert!(matches!(t.turn_end().await, AgentEventKind::TurnCancelled));
    let rows = t.rows_when(session, |rows| settled(rows, 1)).await;
    assert_eq!(rows[0].state, "ok", "{rows:?}");
    assert_eq!(rows[0].files, 1);
    t.daemon.stop().await;

    // Cut off by the daemon's stop: the end is recorded before the daemon is gone.
    let t = Chat::start(|dir| held_fixture(dir, "plain-turn", &dir.join("gate-0"))).await;
    let session = t.create().await.id;
    t.send(session, "never finishes").await;
    fs::write(t.root.join("a.txt"), "cut off\n").unwrap();
    let root = t.root.clone();
    let _work = t.work;
    // Stopped by hand: the home must outlive the daemon to be read.
    let TestDaemon {
        _dir: home,
        paths,
        daemon,
    } = t.daemon;
    daemon.shutdown();
    timeout(WAIT, daemon.wait()).await.unwrap().unwrap();
    let rows = rows_in(&paths.database, session);
    drop(home);
    assert!(settled(&rows, 1), "{rows:?}");
    assert_eq!(rows[0].state, "ok", "{rows:?}");
    assert_eq!(rows[0].files, 1);
    assert_eq!(refs(&root), vec![rows[0].reference.clone().unwrap()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_daemon_reconciles_rows_and_refs() {
    let mut t = Chat::start(|dir| {
        let gates: Vec<PathBuf> = (0..2).map(|n| dir.join(format!("gate-{n}"))).collect();
        held_turns(dir, &gates)
    })
    .await;
    let session = t.create().await.id;
    for n in 0..2 {
        t.send(session, &format!("turn {n}")).await;
        fs::write(t.root.join("a.txt"), format!("turn {n}\n")).unwrap();
        fs::write(t.gate(n), "").unwrap();
        t.turn_end().await;
        t.rows_when(session, |rows| settled(rows, n + 1)).await;
    }
    let rows = t.rows(session);
    let (kept, lost) = (rows[0].clone(), rows[1].clone());
    let Chat {
        daemon, work, root, ..
    } = t;

    // While no daemon runs: one turn's ref is deleted, a stray start ref appears.
    git(&root, &["update-ref", "-d", lost.reference.as_deref().unwrap()]);
    let head = git(&root, &["rev-parse", "HEAD"]).trim().to_owned();
    let stray = format!("refs/hutan/checkpoints/{session}/{}-start", CheckpointId::new());
    git(&root, &["update-ref", &stray, &head]);

    // The restored chat gets a replay that refuses to resume (it exits): never a real Codex.
    let refusing = codex(
        &fixtures().join("plain-turn.jsonl"),
        &work.path().join("received-after-restart.jsonl"),
    );
    let daemon = daemon
        .restart_with(|c| {
            c.codex = Some(refusing);
            c.checkpoints.git_env = git_env();
        })
        .await;
    let database = daemon.paths.database.clone();
    let reconciled = timeout(WAIT, async {
        loop {
            let rows = rows_in(&database, session);
            if rows.len() == 1 && !refs(&root).contains(&stray) {
                return rows;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the restart reconciles");
    assert_eq!(reconciled[0].id, kept.id);
    assert_eq!(refs(&root), vec![kept.reference.clone().unwrap()]);
    daemon.stop().await;
    drop(work);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_off_setting_takes_effect_for_the_next_turn() {
    let mut t = Chat::start(|dir| {
        let gates: Vec<PathBuf> = (0..3).map(|n| dir.join(format!("gate-{n}"))).collect();
        held_turns(dir, &gates)
    })
    .await;
    let session = t.create().await.id;
    t.set("checkpoints.mode", json!("off")).await;
    t.send(session, "turn 0").await;
    fs::write(t.root.join("a.txt"), "edited\n").unwrap();
    fs::write(t.gate(0), "").unwrap();
    t.turn_end().await;
    // Off for a project: the chat's project is its repository's folder name.
    t.set("checkpoints.mode", json!("on")).await;
    t.set("checkpoints.projects.repo.mode", json!("off")).await;
    t.send(session, "turn 1").await;
    fs::write(t.root.join("a.txt"), "edited again\n").unwrap();
    fs::write(t.gate(1), "").unwrap();
    t.turn_end().await;
    assert!(t.rows(session).is_empty(), "{:?}", t.rows(session));
    assert!(refs(&t.root).is_empty());

    t.set("checkpoints.projects.repo.mode", json!("on")).await;
    t.send(session, "turn 2").await;
    fs::write(t.root.join("a.txt"), "edited a third time\n").unwrap();
    fs::write(t.gate(2), "").unwrap();
    t.turn_end().await;
    let rows = t.rows_when(session, |rows| settled(rows, 1)).await;
    assert_eq!(rows[0].state, "ok");
    t.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_outside_git_or_in_the_home_directory_records_unsupported() {
    let mut t = Chat::start_with(
        |dir| {
            let gates: Vec<PathBuf> = (0..2).map(|n| dir.join(format!("gate-{n}"))).collect();
            held_turns(dir, &gates)
        },
        // The repository is the user's home directory.
        |c, root| c.paths.user_home = Some(root.to_owned()),
    )
    .await;
    let plain = t.work.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    for (n, (cwd, says)) in [
        (plain.clone(), "is not in a git repository"),
        (t.root.clone(), "the home directory"),
    ]
    .into_iter()
    .enumerate()
    {
        let session = t.create_in(&cwd).await.id;
        t.send(session, "hello").await;
        fs::write(t.gate(n), "").unwrap();
        t.turn_end().await;
        let rows = t.rows_when(session, |rows| settled(rows, 1)).await;
        assert_eq!(rows[0].state, "unsupported", "{rows:?}");
        assert!(
            rows[0].reason.as_deref().unwrap().contains(says),
            "{rows:?}"
        );
    }
    assert!(refs(&t.root).is_empty());
    t.daemon.stop().await;
}
