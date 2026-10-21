//! Restoring files to before turns (ADR 0019, slice CK-4, protocol 0.38): the service on temporary
//! repositories (the exact start tree, undo and undo of undo, conflicts, gaps, a stale digest,
//! links and files too large to capture, the user's index, HEAD and stash untouched), and a real
//! daemon whose Codex is `hutan-codex-fixture replay` (an agent's restore is forbidden, a running
//! turn refuses it, an open file hears it with origin `restore`).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code, wait_event};
use hutan_agents::CodexConfig;
use hutan_client::Client;
use hutan_core::{
    CheckpointId, CheckpointKind, RestoreAction, RestoreState, Seq, SessionId, TurnId,
};
use hutan_daemon::checkpoints::turns::{RestoreContext, Scope, Tuning, TurnCheckpoints};
use hutan_daemon::config::CheckpointSettings;
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{ChatCreate, ChatSend, ChatSent};
use hutan_protocol::checkpoint::{
    CheckpointPreview, CheckpointPreviewRequest, CheckpointRestore, CheckpointRestored,
};
use hutan_protocol::control::ErrorBody;
use hutan_protocol::fs::{FsOrigin, FsWatch, FsWatchPath};
use hutan_protocol::messages::{Event, Origin, Request, SessionResult};
use hutan_storage::{CheckpointRow, Storage};
use serde_json::json;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

// Git ----------------------------------------------------------------------------------------

fn run_git(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> String {
    let mut command = Command::new("git");
    command
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
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    run_git(dir, args, &[])
}

fn git_env() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ])
}

/// `<dir>/repo`: a repository on `main` with one commit of `a.txt` and `b.txt`.
fn repository(dir: &Path) -> PathBuf {
    let base = fs::canonicalize(dir).unwrap();
    git(&base, &["init", "-q", "-b", "main", "repo"]);
    let root = base.join("repo");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    fs::write(root.join("b.txt"), "beta\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

/// The tree of the working directory as `git add -A` into a fresh index sees it: nothing of
/// Hutan's, nothing of the user's index.
fn worktree_tree(root: &Path) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let index = scratch.path().join("index");
    run_git(root, &["add", "-A"], &[("GIT_INDEX_FILE", &index)]);
    run_git(root, &["write-tree"], &[("GIT_INDEX_FILE", &index)])
}

/// What the user's git state is: the index's bytes, HEAD, the stash, the refs that are not
/// Hutan's.
fn user_state(root: &Path) -> (Vec<u8>, String, String, String) {
    (
        fs::read(root.join(".git/index")).unwrap(),
        git(root, &["rev-parse", "HEAD"]),
        git(root, &["stash", "list", "--format=%H %s"]),
        git(
            root,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "--exclude=refs/hutan/",
            ],
        ),
    )
}

fn hutan_refs(root: &Path) -> Vec<String> {
    git(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/hutan/"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

// The service on its own ---------------------------------------------------------------------

struct Service {
    _dir: tempfile::TempDir,
    root: PathBuf,
    storage: Storage,
    turns: Arc<TurnCheckpoints>,
    session: SessionId,
}

impl Service {
    fn new(settings: CheckpointSettings) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = repository(dir.path());
        let base = fs::canonicalize(dir.path()).unwrap().join("home");
        let storage = Storage::open(base.join("hutan.sqlite")).unwrap();
        let turns = TurnCheckpoints::new(
            storage.clone(),
            &base,
            None,
            settings,
            Tuning {
                git_env: git_env(),
                ..Tuning::default()
            },
        );
        let session = SessionId::new();
        turns.open(session, root.clone(), Scope::default());
        Self {
            _dir: dir,
            root,
            storage,
            turns,
            session,
        }
    }

    fn quick() -> Self {
        Self::new(CheckpointSettings::default())
    }

    fn write(&self, path: &str, content: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.root.join(path)).ok()
    }

    fn row(&self, id: CheckpointId) -> CheckpointRow {
        self.storage.checkpoints().get(id).unwrap().unwrap()
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

    /// The tree a checkpoint's turn started from.
    fn start_tree(&self, row: &CheckpointRow) -> String {
        let start = row.start_commit.as_deref().unwrap();
        git(&self.root, &["rev-parse", &format!("{start}^{{tree}}")])
    }

    async fn preview(&self, id: CheckpointId, through: Option<CheckpointId>) -> CheckpointPreview {
        self.turns
            .preview(
                CheckpointPreviewRequest {
                    checkpoint_id: id,
                    through,
                    paths: vec![],
                },
                &RestoreContext::default(),
            )
            .await
            .unwrap()
    }

    async fn restore_with(
        &self,
        preview: &CheckpointPreview,
        id: CheckpointId,
        through: Option<CheckpointId>,
        accept: &[&str],
    ) -> Result<CheckpointRestored, ErrorBody> {
        self.turns
            .restore(
                CheckpointRestore {
                    checkpoint_id: id,
                    through,
                    paths: vec![],
                    accept_conflicts: accept.iter().map(|p| (*p).to_owned()).collect(),
                    plan_digest: preview.plan_digest.clone(),
                },
                RestoreContext::default(),
            )
            .await
    }

    /// Preview, then restore what it says.
    async fn restore(&self, id: CheckpointId) -> CheckpointRestored {
        let preview = self.preview(id, None).await;
        self.restore_with(&preview, id, None, &[]).await.unwrap()
    }
}

fn summary(preview: &CheckpointPreview) -> Vec<(&str, RestoreAction, RestoreState)> {
    preview
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.action, e.state))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_returns_the_start_tree_and_undo_and_undo_of_undo_flip_it_without_touching_git() {
    let s = Service::quick();
    // The user has a stash and a staged change: neither may move.
    s.write("a.txt", "stashed\n");
    git(&s.root, &["stash", "-q"]);
    s.write("b.txt", "staged\n");
    git(&s.root, &["add", "b.txt"]);
    let before = user_state(&s.root);

    // A turn edits two files and adds a new one in a new folder.
    let turn = s
        .turn(|s| {
            s.write("a.txt", "edited by the agent\n");
            s.write("b.txt", "rewritten by the agent\n");
            s.write("new/c.txt", "new\n");
        })
        .await;
    let (start, end) = (s.start_tree(&turn), worktree_tree(&s.root));
    assert_eq!(turn.tree.as_deref(), Some(end.as_str()));
    assert_ne!(start, end);

    let preview = s.preview(turn.id, None).await;
    assert_eq!(
        summary(&preview),
        [
            ("a.txt", RestoreAction::Write, RestoreState::Clean),
            ("b.txt", RestoreAction::Write, RestoreState::Clean),
            ("new/c.txt", RestoreAction::Delete, RestoreState::Clean),
        ]
    );
    assert_eq!(preview.checkpoints, [turn.id]);
    assert_eq!(preview.session_id, s.session);
    assert_eq!(preview.head_moved, None);
    let restored = s.restore_with(&preview, turn.id, None, &[]).await.unwrap();
    assert_eq!(restored.written, ["a.txt", "b.txt"]);
    assert_eq!(restored.deleted, ["new/c.txt"]);
    assert!(restored.skipped.is_empty());
    assert_eq!(worktree_tree(&s.root), start, "exactly the start tree");
    assert!(!s.root.join("new").exists(), "the emptied folder went");
    assert_eq!(s.read("b.txt").as_deref(), Some("staged\n"));
    // The restore is a checkpoint: its start is the end of the turn, its end the start.
    let undo = restored.checkpoint.clone();
    assert_eq!(undo.kind, CheckpointKind::Restore);
    assert_eq!(undo.files_changed, 3);
    let row = s.row(undo.id);
    assert_eq!(row.parent_id, Some(turn.id));
    assert_eq!(row.tree.as_deref(), Some(start.as_str()));
    assert_eq!(s.start_tree(&row), end);
    assert_eq!(
        hutan_refs(&s.root),
        [
            format!(
                "refs/hutan/checkpoints/{}/{}",
                s.session,
                turn.turn_id.unwrap()
            ),
            format!("refs/hutan/checkpoints/{}/{}", s.session, undo.id),
        ],
        "one ref each, no start left"
    );

    // Undo: the same algorithm over the restore.
    let undone = s.restore(undo.id).await;
    assert_eq!(undone.written, ["a.txt", "b.txt", "new/c.txt"]);
    assert_eq!(worktree_tree(&s.root), end, "the end again");
    // Undo of undo.
    let redone = s.restore(undone.checkpoint.id).await;
    assert_eq!(redone.deleted, ["new/c.txt"]);
    assert_eq!(worktree_tree(&s.root), start);

    assert_eq!(
        user_state(&s.root),
        before,
        "index, HEAD, stash and refs untouched"
    );
    assert_eq!(
        git(&s.root, &["diff", "--cached", "--name-only"]),
        "b.txt",
        "the staged change is still staged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_after_the_turn_is_left_alone_until_the_user_accepts_it() {
    let s = Service::quick();
    let turn = s
        .turn(|s| {
            s.write("a.txt", "agent a\n");
            s.write("b.txt", "agent b\n");
        })
        .await;
    s.write("b.txt", "the user's b\n");
    s.write("c.txt", "the user's own file\n");

    let preview = s.preview(turn.id, None).await;
    assert_eq!(
        summary(&preview),
        [
            ("a.txt", RestoreAction::Write, RestoreState::Clean),
            ("b.txt", RestoreAction::Skip, RestoreState::After),
        ],
        "c.txt was never the turn's"
    );
    assert_eq!(preview.entries[1].accept, Some(RestoreAction::Write));
    let restored = s.restore_with(&preview, turn.id, None, &[]).await.unwrap();
    assert_eq!(restored.written, ["a.txt"]);
    assert_eq!(restored.skipped.len(), 1);
    assert_eq!(restored.skipped[0].state, RestoreState::After);
    assert_eq!(s.read("a.txt").as_deref(), Some("alpha\n"));
    assert_eq!(
        s.read("b.txt").as_deref(),
        Some("the user's b\n"),
        "untouched"
    );
    assert_eq!(s.read("c.txt").as_deref(), Some("the user's own file\n"));

    // Naming something that is not a conflict is refused; naming the conflict overwrites it.
    let preview = s.preview(turn.id, None).await;
    assert_eq!(
        summary(&preview),
        [("b.txt", RestoreAction::Skip, RestoreState::After)]
    );
    let refused = s
        .restore_with(&preview, turn.id, None, &["c.txt"])
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::BadRequest);
    let accepted = s
        .restore_with(&preview, turn.id, None, &["b.txt"])
        .await
        .unwrap();
    assert_eq!(accepted.written, ["b.txt"]);
    assert_eq!(s.read("b.txt").as_deref(), Some("beta\n"));
    // ... and undoing it brings the user's edit back.
    s.restore(accepted.checkpoint.id).await;
    assert_eq!(s.read("b.txt").as_deref(), Some("the user's b\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_between_two_turns_is_a_gap_and_a_save_during_a_turn_is_during() {
    let s = Service::quick();
    let first = s
        .turn(|s| {
            s.write("a.txt", "turn 1\n");
            s.write("b.txt", "turn 1\n");
        })
        .await;
    // The user edits a.txt between the turns, and saves b.txt (through the file service)
    // while the second one runs.
    s.write("a.txt", "the user, between\n");
    let id = s.turns.begin(s.session).await.unwrap();
    let turn = TurnId::new();
    s.turns.sent(s.session, id, turn);
    s.write("a.txt", "turn 2\n");
    s.write("b.txt", "turn 2 and the user\n");
    s.turns.saved(&s.root.join("b.txt"));
    s.write("d.txt", "turn 2\n");
    s.turns.turn_ended(s.session, Some(turn));
    s.turns.drain(WAIT).await;
    let second = s.row(id);
    assert_eq!(second.during, ["b.txt"]);

    let preview = s.preview(first.id, Some(second.id)).await;
    assert_eq!(preview.checkpoints, [first.id, second.id]);
    assert_eq!(
        summary(&preview),
        [
            ("a.txt", RestoreAction::Skip, RestoreState::Gap),
            ("b.txt", RestoreAction::Skip, RestoreState::During),
            ("d.txt", RestoreAction::Delete, RestoreState::Clean),
        ]
    );
    // `through` must not be older, and must be of the same session.
    let backwards = s
        .turns
        .preview(
            CheckpointPreviewRequest {
                checkpoint_id: second.id,
                through: Some(first.id),
                paths: vec![],
            },
            &RestoreContext::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(backwards.code, ErrorCode::BadRequest);
    // Restoring through both: the gap and the save need the user's word.
    let restored = s
        .restore_with(&preview, first.id, Some(second.id), &["a.txt"])
        .await
        .unwrap();
    assert_eq!(restored.written, ["a.txt"]);
    assert_eq!(restored.deleted, ["d.txt"]);
    assert_eq!(
        s.read("a.txt").as_deref(),
        Some("alpha\n"),
        "the first start"
    );
    assert_eq!(s.read("b.txt").as_deref(), Some("turn 2 and the user\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_digest_answers_conflict_with_the_new_plan_and_writes_nothing() {
    let s = Service::quick();
    let turn = s
        .turn(|s| {
            s.write("a.txt", "agent a\n");
            s.write("b.txt", "agent b\n");
        })
        .await;
    let preview = s.preview(turn.id, None).await;
    assert_eq!(preview.entries.len(), 2);
    let refs = hutan_refs(&s.root);
    s.write("b.txt", "the user, after the preview\n");

    let error = s
        .restore_with(&preview, turn.id, None, &[])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict, "{error:?}");
    let fresh: CheckpointPreview = serde_json::from_value(error.data.unwrap()).unwrap();
    assert_ne!(fresh.plan_digest, preview.plan_digest);
    assert_eq!(
        summary(&fresh),
        [
            ("a.txt", RestoreAction::Write, RestoreState::Clean),
            ("b.txt", RestoreAction::Skip, RestoreState::After),
        ]
    );
    assert_eq!(
        s.read("a.txt").as_deref(),
        Some("agent a\n"),
        "nothing written"
    );
    assert_eq!(hutan_refs(&s.root), refs, "the restore's start went again");
    let rows = s
        .storage
        .checkpoints()
        .list(hutan_storage::CheckpointFilter::Session(s.session))
        .unwrap();
    assert_eq!(rows.len(), 1, "no restore row stays");
    // The new digest works.
    let restored = s.restore_with(&fresh, turn.id, None, &[]).await.unwrap();
    assert_eq!(restored.written, ["a.txt"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_symlink_and_a_file_too_large_to_capture_are_skipped_with_their_reasons() {
    let s = Service::new(CheckpointSettings {
        max_file_bytes: 1024,
        ..CheckpointSettings::default()
    });
    let turn = s
        .turn(|s| {
            s.write("a.txt", "agent a\n");
            s.write("big.bin", &"x".repeat(4096));
            std::os::unix::fs::symlink("a.txt", s.root.join("link")).unwrap();
        })
        .await;
    let preview = s.preview(turn.id, None).await;
    assert_eq!(
        summary(&preview),
        [
            ("a.txt", RestoreAction::Write, RestoreState::Clean),
            ("big.bin", RestoreAction::Skip, RestoreState::Uncaptured),
            ("link", RestoreAction::Skip, RestoreState::Clean),
        ]
    );
    let reason = |path: &str| {
        preview
            .entries
            .iter()
            .find(|e| e.path == path)
            .and_then(|e| e.reason.clone())
            .unwrap()
    };
    assert_eq!(reason("big.bin"), "not captured: too large");
    assert!(
        reason("link").contains("symbolic link"),
        "{}",
        reason("link")
    );
    let refused = s
        .restore_with(&preview, turn.id, None, &["big.bin"])
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::BadRequest);
    let restored = s.restore_with(&preview, turn.id, None, &[]).await.unwrap();
    assert_eq!(restored.written, ["a.txt"]);
    assert!(restored.deleted.is_empty());
    let skipped: Vec<&str> = restored.skipped.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(skipped, ["big.bin", "link"]);
    assert!(s.root.join("big.bin").exists());
    assert!(
        fs::symlink_metadata(s.root.join("link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

// Through a daemon and a fixture chat -----------------------------------------------------------

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

/// `plain-turn` with its turn repeated once per gate, each held after `turn/started` until its
/// gate exists; each turn has ids of its own.
fn held_turns(dir: &Path, gates: &[PathBuf]) -> PathBuf {
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../agents/tests/fixtures/codex-0.162");
    let real = fs::read_to_string(fixtures.join("plain-turn.jsonl")).unwrap();
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

async fn turn_end(events: &mut broadcast::Receiver<(Seq, Event)>) {
    wait_event(events, "the turn's end", |event| match event {
        Event::ChatEvent { event, .. } if event.kind.ends_turn() => Some(()),
        _ => None,
    })
    .await;
}

/// The preview of `id` once `ready` holds for it.
async fn preview_when(
    client: &Client,
    id: CheckpointId,
    ready: impl Fn(&CheckpointPreview) -> bool,
) -> CheckpointPreview {
    timeout(WAIT, async {
        loop {
            let preview = client
                .checkpoint_preview(CheckpointPreviewRequest {
                    checkpoint_id: id,
                    through: None,
                    paths: vec![],
                })
                .await
                .unwrap();
            if ready(&preview) {
                return preview;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the preview never got there")
}

fn restore_of(preview: &CheckpointPreview, id: CheckpointId) -> CheckpointRestore {
    CheckpointRestore {
        checkpoint_id: id,
        through: None,
        paths: vec![],
        accept_conflicts: vec![],
        plan_digest: preview.plan_digest.clone(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_user_restores_never_during_a_turn_and_an_open_file_hears_it() {
    let work = tempfile::tempdir().unwrap();
    let root = repository(work.path());
    let gates: Vec<PathBuf> = (0..2)
        .map(|n| work.path().join(format!("gate-{n}")))
        .collect();
    let config = codex(
        &held_turns(work.path(), &gates),
        &work.path().join("received.jsonl"),
    );
    let daemon = TestDaemon::start_with("", |c| {
        c.codex = Some(config);
        c.checkpoints.git_env = git_env();
    })
    .await;
    let (user, mut events) = daemon.connect_subscribed().await;
    let session = user
        .call::<SessionResult>(Request::ChatCreate(ChatCreate {
            workspace_id: None,
            cwd: root.clone(),
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
        .id;
    let send = |text: &str| ChatSend {
        session_id: session,
        text: text.to_owned(),
        attachments: vec![],
    };
    assert!(user.supports_checkpoint_restore());

    // Turn 0 edits a.txt.
    user.call::<ChatSent>(Request::ChatSend(send("turn 0")))
        .await
        .unwrap();
    fs::write(root.join("a.txt"), "edited by the agent\n").unwrap();
    fs::write(&gates[0], "").unwrap();
    turn_end(&mut events).await;
    let checkpoint = wait_event(&mut events, "checkpoint.created", |event| match event {
        Event::CheckpointCreated { checkpoint } if checkpoint.session_id == session => {
            Some(checkpoint.id)
        }
        _ => None,
    })
    .await;
    // The user has a.txt open.
    let a = root.join("a.txt");
    user.fs_watch(FsWatch {
        paths: vec![FsWatchPath {
            path: a.clone(),
            recursive: false,
        }],
        picked: false,
    })
    .await
    .unwrap();

    // The chat's own agent may preview, never restore, whatever `[control]` says.
    let mut options = daemon.options();
    options.client.origin = Some(Origin {
        session_id: session,
    });
    let agent = Client::connect(options).await.unwrap();
    let seen = agent
        .checkpoint_preview(CheckpointPreviewRequest {
            checkpoint_id: checkpoint,
            through: None,
            paths: vec![],
        })
        .await
        .unwrap();
    assert_eq!(
        summary(&seen),
        [("a.txt", RestoreAction::Write, RestoreState::Clean)]
    );
    assert_eq!(seen.agent.as_deref(), Some("codex"));
    let forbidden = agent
        .checkpoint_restore(restore_of(&seen, checkpoint))
        .await;
    assert_eq!(remote_code(forbidden), ErrorCode::Forbidden);
    assert_eq!(fs::read_to_string(&a).unwrap(), "edited by the agent\n");

    // While a turn runs in the checkout, a restore is refused.
    user.call::<ChatSent>(Request::ChatSend(send("turn 1")))
        .await
        .unwrap();
    let busy = preview_when(&user, checkpoint, |p| !p.busy.is_empty()).await;
    assert_eq!(busy.busy, [session]);
    let refused = user.checkpoint_restore(restore_of(&busy, checkpoint)).await;
    assert_eq!(remote_code(refused), ErrorCode::InvalidState);
    fs::write(&gates[1], "").unwrap();
    turn_end(&mut events).await;

    // Once it ended, the user's restore goes through and the open file hears of it.
    let ready = preview_when(&user, checkpoint, |p| p.busy.is_empty()).await;
    let restored = user
        .checkpoint_restore(restore_of(&ready, checkpoint))
        .await
        .unwrap();
    assert_eq!(restored.written, ["a.txt"]);
    assert_eq!(fs::read_to_string(&a).unwrap(), "alpha\n");
    // Subscribers hear of the restore's checkpoint, and the open file of the change, with the
    // restore as its origin (in either order).
    let (mut origin, mut created) = (None, None);
    let heard = timeout(WAIT, async {
        while !matches!(origin, Some(FsOrigin::Restore { .. })) || created.is_none() {
            match events.recv().await.expect("event stream closed").1 {
                Event::FsChanged { changes, .. } => {
                    if let Some(change) = changes.iter().find(|c| c.path == a) {
                        origin.clone_from(&change.origin);
                    }
                }
                Event::CheckpointCreated { checkpoint }
                    if checkpoint.kind == CheckpointKind::Restore =>
                {
                    created = Some(checkpoint.id);
                }
                _ => {}
            }
        }
    })
    .await
    .is_ok();
    assert!(heard, "got origin {origin:?} and checkpoint {created:?}");
    assert_eq!(
        origin,
        Some(FsOrigin::Restore {
            checkpoint_id: restored.checkpoint.id
        })
    );
    assert_eq!(created, Some(restored.checkpoint.id));
    daemon.stop().await;
}
