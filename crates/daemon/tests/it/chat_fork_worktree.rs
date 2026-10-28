//! Fork a chat into a new worktree at the files of a turn, restore a folder's files with a
//! fork, and archive the parent (ADR 0023 decision 3, protocol 0.58, capability
//! `chat.fork.worktree`), end to end: a real daemon whose Codex is `hutan-codex-fixture replay`
//! working in a git repository the test creates (the test edits the files while the replay holds
//! a turn open, as the agent would; every fork takes the next transcript of the script).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, remote_code};
use hutan_client::{Client, ClientError};
use hutan_core::{AgentEventKind, CheckpointKind, SessionId, TurnId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::chat::{
    ChatCreate, ChatFork, ChatForked, ChatHistoryRequest, ForkFiles, ForkWhere,
};
use hutan_protocol::checkpoint::{CheckpointListRequest, CheckpointPreviewRequest};
use hutan_protocol::messages::{
    Event, Origin, Request, SessionInfo, SessionList, SessionListFilter, SessionResult,
    WorkspaceOpen, WorkspaceOpened,
};
use hutan_protocol::worktree::WorktreeSetupState;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

use crate::checkpoint_restore::{codex, git_env, held_turns, restore_of, turn_end};
use crate::checkpoint_turns::git;

/// Tracked files of the repository, then the commit of them.
fn repository(dir: &Path) -> PathBuf {
    let base = fs::canonicalize(dir).unwrap();
    git(&base, &["init", "-q", "-b", "main", "repo"]);
    let root = base.join("repo");
    fs::write(root.join("a.txt"), "alpha\n").unwrap();
    fs::write(root.join("b.txt"), "beta\n").unwrap();
    fs::create_dir(root.join("docs")).unwrap();
    fs::write(root.join("docs/n.md"), "notes\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    root
}

/// What the user's git state of `root` is: the index's bytes, HEAD, the stash, the branches.
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
                "--exclude=refs/heads/fork/",
            ],
        ),
    )
}

/// `git status --porcelain` of `dir`, sorted.
fn status(dir: &Path) -> Vec<String> {
    let mut lines: Vec<String> = git(dir, &["status", "--porcelain", "--untracked-files=all"])
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

struct Env {
    daemon: TestDaemon,
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    work: tempfile::TempDir,
    root: PathBuf,
    gates: Vec<PathBuf>,
    session: SessionInfo,
    workspace: WorkspaceId,
}

impl Env {
    /// A chat in a repository with `turns` held turns to run and `forks` Codex spawns for forks.
    async fn start(turns: usize, forks: usize, toml: &str) -> Self {
        let work = tempfile::tempdir().unwrap();
        let root = repository(work.path());
        let gates: Vec<PathBuf> = (0..turns)
            .map(|n| work.path().join(format!("gate-{n}")))
            .collect();
        let held = held_turns(work.path(), &gates);
        let scripted = work.path().join("script");
        fs::create_dir_all(&scripted).unwrap();
        fs::copy(&held, scripted.join("00-held.jsonl")).unwrap();
        let fork_turn = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../agents/tests/fixtures/codex-0.162/fork-turn.jsonl");
        for n in 0..forks {
            fs::copy(
                &fork_turn,
                scripted.join(format!("{:02}-fork.jsonl", n + 1)),
            )
            .unwrap();
        }
        let config = codex(&scripted, &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with(toml, |c| {
            c.codex = Some(config);
            c.checkpoints.git_env = git_env();
        })
        .await;
        let (client, events) = daemon.connect_subscribed().await;
        let workspace = client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: root.clone(),
                created_by: None,
            }))
            .await
            .unwrap()
            .workspace
            .id;
        let session = client
            .call::<SessionResult>(Request::ChatCreate(ChatCreate {
                extra_dirs: Vec::new(),
                contain: None,
                workspace_id: Some(workspace),
                cwd: root.clone(),
                agent: "codex".to_owned(),
                model: None,
                approval_policy: None,
                sandbox: None,
                title: Some("Try it".to_owned()),
                mode: None,
                provider_id: None,
                system_prompt: None,
                account: None,
                safety: None,
            }))
            .await
            .unwrap()
            .session;
        assert!(client.supports_chat_fork_worktree());
        Self {
            daemon,
            client,
            events,
            work,
            root,
            gates,
            session,
            workspace,
        }
    }

    fn received_of(&self, method: &str) -> Vec<Value> {
        fs::read_to_string(self.work.path().join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
            .filter(|msg| msg["method"] == method)
            .collect()
    }

    async fn sessions(&self) -> Vec<SessionInfo> {
        self.client
            .call::<SessionList>(Request::SessionList(SessionListFilter::default()))
            .await
            .unwrap()
            .sessions
    }

    /// Waits until the session has no turn in progress.
    async fn wait_quiet(&self, id: SessionId) {
        timeout(WAIT, async {
            loop {
                let quiet = self
                    .sessions()
                    .await
                    .iter()
                    .find(|s| s.id == id)
                    .and_then(|s| s.chat.as_ref())
                    .is_some_and(|chat| !chat.turn_active);
                if quiet {
                    return;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the turn never ended");
    }

    /// Turn `n`: the agent runs while the test edits the files, then the turn ends.
    async fn turn(&mut self, n: usize, edit: impl FnOnce(&Path)) {
        self.client
            .chat_send(self.session.id, &format!("turn {n}"))
            .await
            .unwrap();
        edit(&self.root);
        fs::write(&self.gates[n], "").unwrap();
        turn_end(&mut self.events).await;
        self.wait_quiet(self.session.id).await;
    }

    /// Waits until `count` checkpoints of the session are recorded (a turn that changed no file
    /// has none to list).
    async fn wait_checkpoints(&self, count: usize) {
        timeout(WAIT, async {
            loop {
                let listed = self
                    .client
                    .checkpoint_list(CheckpointListRequest {
                        session_id: Some(self.session.id),
                        workspace_id: None,
                        limit: None,
                        before: None,
                    })
                    .await
                    .unwrap();
                if listed.checkpoints.len() >= count {
                    return;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the checkpoints were never recorded");
    }

    async fn turn_ids(&self) -> Vec<TurnId> {
        self.client
            .call::<hutan_protocol::chat::ChatHistory>(Request::ChatHistory(ChatHistoryRequest {
                session_id: self.session.id,
                before_seq: None,
                limit: 500,
            }))
            .await
            .unwrap()
            .events
            .iter()
            .filter(|e| matches!(e.kind, AgentEventKind::TurnStarted))
            .filter_map(|e| e.turn_id)
            .collect()
    }

    fn fork(&self, turn: Option<TurnId>) -> ChatFork {
        ChatFork {
            session_id: self.session.id,
            turn_id: turn,
            mode: hutan_core::ForkMode::Exact,
            title: None,
            where_: ForkWhere::ThisFolder,
            branch: None,
            restore_files: false,
            archive_parent: false,
        }
    }

    async fn try_fork(&self, fork: ChatFork) -> Result<ChatForked, ClientError> {
        self.client.chat_fork(fork).await
    }

    /// `git worktree list` of the repository: the directories.
    fn worktrees(&self) -> BTreeSet<String> {
        git(&self.root, &["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(str::to_owned)
            .collect()
    }

    async fn stop(self) {
        self.daemon.stop().await;
    }
}

/// Turn 0 changes a tracked file, deletes one, adds a plain, a secret-named and a nested file;
/// turn 1 changes more; turn 2 changes nothing.
async fn three_turns() -> Env {
    let mut env = Env::start(3, 4, "").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        fs::write(root.join("new.txt"), "new t0\n").unwrap();
        fs::write(root.join(".env.local"), "TOKEN=1\n").unwrap();
        fs::create_dir(root.join("deep")).unwrap();
        fs::write(root.join("deep/n.txt"), "deep t0\n").unwrap();
    })
    .await;
    env.turn(1, |root| {
        fs::write(root.join("a.txt"), "alpha t1\n").unwrap();
        fs::write(root.join("new.txt"), "new t1\n").unwrap();
        fs::write(root.join("t1.txt"), "only t1\n").unwrap();
        fs::remove_file(root.join("deep/n.txt")).unwrap();
    })
    .await;
    env.turn(2, |_| {}).await;
    env
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_into_a_worktree_has_the_files_of_the_turn_and_leaves_the_source_alone() {
    let env = three_turns().await;
    let turns = env.turn_ids().await;
    assert_eq!(turns.len(), 3);
    let before = user_state(&env.root);
    let source_status = status(&env.root);
    let source_tree: Vec<(String, String)> = ["a.txt", "new.txt", "t1.txt"]
        .iter()
        .map(|f| {
            (
                (*f).to_owned(),
                fs::read_to_string(env.root.join(f)).unwrap(),
            )
        })
        .collect();

    let forked = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            branch: Some("fork/try-t1".to_owned()),
            ..env.fork(Some(turns[0]))
        })
        .await
        .unwrap();
    let worktree = forked.fork.worktree.clone().expect("a worktree");
    let wt = worktree.path.clone();

    // The new worktree is the checkpoint of turn 0: modified, deleted and untracked files.
    assert_eq!(worktree.branch, "fork/try-t1");
    assert_eq!(worktree.files, ForkFiles::Turn);
    assert!(worktree.checkpoint_id.is_some());
    assert_eq!(fs::read_to_string(wt.join("a.txt")).unwrap(), "alpha t0\n");
    assert!(!wt.join("b.txt").exists(), "deleted by turn 0");
    assert_eq!(fs::read_to_string(wt.join("new.txt")).unwrap(), "new t0\n");
    assert_eq!(
        fs::read_to_string(wt.join("deep/n.txt")).unwrap(),
        "deep t0\n"
    );
    assert!(!wt.join("t1.txt").exists(), "turn 1's file is not there");
    assert_eq!(fs::read_to_string(wt.join("docs/n.md")).unwrap(), "notes\n");
    // As uncommitted changes of a branch that starts where the turn ended.
    assert_eq!(
        status(&wt),
        [" D b.txt", " M a.txt", "?? deep/n.txt", "?? new.txt"]
    );
    assert_eq!(
        git(&wt, &["rev-parse", "HEAD"]).trim(),
        worktree.base,
        "the base is the commit the turn ended on"
    );
    assert_eq!(
        worktree.base,
        git(&env.root, &["rev-parse", "HEAD"]).trim(),
        "(which is the repository's HEAD here)"
    );
    assert_eq!(
        git(&wt, &["branch", "--show-current"]).trim(),
        "fork/try-t1"
    );
    // What the checkpoint leaves out is listed: the secret-named file is in no worktree.
    assert!(!wt.join(".env.local").exists());
    let skipped: Vec<&str> = worktree.skipped.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(skipped, [".env.local"], "{:?}", worktree.skipped);

    // The source checkout is exactly as it was: files, index, HEAD, stash, branches; its only
    // addition is the new branch.
    assert_eq!(status(&env.root), source_status);
    for (file, content) in &source_tree {
        assert_eq!(&fs::read_to_string(env.root.join(file)).unwrap(), content);
    }
    assert_eq!(user_state(&env.root), before);
    assert!(
        git(&env.root, &["branch", "--format=%(refname:short)"])
            .lines()
            .any(|b| b == "fork/try-t1")
    );

    // The forked chat works there: Codex was given the new cwd, the session lists it.
    let child = &forked.session;
    assert_eq!(child.cwd.as_deref(), Some(wt.as_path()));
    assert_eq!(
        child
            .forked_from
            .as_ref()
            .map(|f| (f.session_id, f.turn_id)),
        Some((env.session.id, turns[0]))
    );
    let fork = &env.received_of("thread/fork")[0]["params"];
    assert_eq!(fork["cwd"], wt.display().to_string());
    assert_eq!(forked.fork.ordinal, 1);
    assert!(!forked.fork.parent_archived);
    assert!(forked.fork.restored.is_none());
    // The parent chat is untouched and still works where it did.
    let parent = env
        .sessions()
        .await
        .into_iter()
        .find(|s| s.id == env.session.id)
        .unwrap();
    assert_eq!(parent.cwd.as_deref(), Some(env.root.as_path()));
    // The worktree is the daemon's own: listed with its branch, marked as created by Hutan.
    let listed = env.client.worktree_list(Some(env.workspace)).await.unwrap();
    let info = listed.iter().find(|w| w.path == wt).expect("listed");
    assert!(info.created_by_hutan && info.dirty);
    assert_eq!(info.branch.as_deref(), Some("fork/try-t1"));
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_changed_nothing_takes_the_files_of_the_latest_earlier_turn() {
    let env = three_turns().await;
    let turns = env.turn_ids().await;
    let forked = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            ..env.fork(Some(turns[2]))
        })
        .await
        .unwrap();
    let worktree = forked.fork.worktree.unwrap();
    assert_eq!(worktree.files, ForkFiles::Earlier);
    let wt = &worktree.path;
    assert_eq!(fs::read_to_string(wt.join("a.txt")).unwrap(), "alpha t1\n");
    assert_eq!(fs::read_to_string(wt.join("t1.txt")).unwrap(), "only t1\n");
    assert!(!wt.join("deep/n.txt").exists(), "turn 1 deleted it");
    assert_eq!(
        status(wt),
        [" D b.txt", " M a.txt", "?? new.txt", "?? t1.txt"]
    );
    // The default branch is `fork/<title>-t<N>`; a second one for the same turn is numbered.
    assert_eq!(worktree.branch, "fork/try-it-t3");
    let again = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            ..env.fork(Some(turns[2]))
        })
        .await
        .unwrap();
    assert_eq!(again.fork.worktree.unwrap().branch, "fork/try-it-t3-2");
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_before_anything_changed_forks_the_base_as_it_is() {
    let mut env = Env::start(1, 1, "").await;
    env.turn(0, |_| {}).await;
    let forked = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            ..env.fork(None)
        })
        .await
        .unwrap();
    let worktree = forked.fork.worktree.unwrap();
    assert_eq!(worktree.files, ForkFiles::Base);
    assert!(worktree.checkpoint_id.is_none());
    assert!(status(&worktree.path).is_empty(), "base as it is");
    assert_eq!(
        fs::read_to_string(worktree.path.join("b.txt")).unwrap(),
        "beta\n"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_worktree_follows_the_worktree_rules_copy_setup_and_trust() {
    let mut env = Env::start(1, 1, "").await;
    // The repository asks for its ignored `.env` to be copied and a command to run: the command
    // is the repository's, so it waits for the user's confirmation (`needs_trust`).
    fs::write(env.root.join(".gitignore"), ".env\n").unwrap();
    fs::create_dir(env.root.join(".hutan")).unwrap();
    fs::write(
        env.root.join(".hutan/worktree.toml"),
        "copy = [\".env\"]\nsetup = [\"touch ran\"]\n",
    )
    .unwrap();
    git(&env.root, &["add", "."]);
    git(&env.root, &["commit", "-q", "-m", "worktree rules"]);
    fs::write(env.root.join(".env"), "A=1\n").unwrap();
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap();
    })
    .await;

    let forked = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            ..env.fork(None)
        })
        .await
        .unwrap();
    let worktree = forked.fork.worktree.unwrap();
    let setup = worktree.setup.expect("the setup the repository asks for");
    assert_eq!(setup.state, WorktreeSetupState::NeedsTrust);
    assert_eq!(setup.commands, ["touch ran"]);
    assert_eq!(setup.copied, 1);
    // The files of the turn are there, and so is the ignored file the rules copy; the command
    // did not run.
    assert_eq!(
        fs::read_to_string(worktree.path.join("a.txt")).unwrap(),
        "alpha t0\n"
    );
    assert_eq!(
        fs::read_to_string(worktree.path.join(".env")).unwrap(),
        "A=1\n"
    );
    assert!(!worktree.path.join("ran").exists());
    assert_eq!(status(&worktree.path), [" M a.txt"]);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_fork_is_refused_before_anything_is_made() {
    let mut env = Env::start(1, 2, "").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap()
    })
    .await;
    let worktrees = env.worktrees();
    let sessions = env.sessions().await.len();

    // A branch that exists is a conflict, not taken over.
    let error = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            branch: Some("main".to_owned()),
            ..env.fork(None)
        })
        .await;
    assert_eq!(remote_code(error), ErrorCode::Conflict);
    // `branch` and `restore_files` mean something only where they apply.
    let error = env
        .try_fork(ChatFork {
            branch: Some("x".to_owned()),
            ..env.fork(None)
        })
        .await;
    assert_eq!(remote_code(error), ErrorCode::BadRequest);
    let error = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            restore_files: true,
            ..env.fork(None)
        })
        .await;
    assert_eq!(remote_code(error), ErrorCode::BadRequest);
    let error = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            branch: Some("not a branch".to_owned()),
            ..env.fork(None)
        })
        .await;
    assert_eq!(remote_code(error), ErrorCode::BadRequest);
    assert_eq!(env.worktrees(), worktrees, "no worktree");
    assert_eq!(env.sessions().await.len(), sessions, "no session");
    assert!(
        env.received_of("thread/fork").is_empty(),
        "no agent was asked"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_checkpoints_a_worktree_fork_is_unsupported_and_says_why() {
    let mut env = Env::start(1, 1, "[checkpoints]\nmode = \"off\"\n").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap()
    })
    .await;
    let worktrees = env.worktrees();
    let error = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            ..env.fork(None)
        })
        .await
        .unwrap_err();
    let ClientError::Remote(body) = error else {
        panic!("{error:?}");
    };
    assert_eq!(body.code, ErrorCode::Unsupported);
    assert_eq!(body.data.as_ref().unwrap()["reason"], "no_checkpoint");
    assert_eq!(env.worktrees(), worktrees);
    // The same chat still forks in its own folder, without files.
    let forked = env.try_fork(env.fork(None)).await.unwrap();
    assert!(forked.fork.worktree.is_none());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_files_puts_the_folder_back_to_the_turn_and_undo_brings_it_forward() {
    let env = three_turns().await;
    let turns = env.turn_ids().await;
    let end_of_turn_2 = status(&env.root);

    let forked = env
        .try_fork(ChatFork {
            restore_files: true,
            ..env.fork(Some(turns[0]))
        })
        .await
        .unwrap();
    // The folder is as turn 0 left it (and the fork works in it, where the parent does).
    let root = &env.root;
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).unwrap(),
        "alpha t0\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("new.txt")).unwrap(),
        "new t0\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("deep/n.txt")).unwrap(),
        "deep t0\n"
    );
    assert!(!root.join("t1.txt").exists(), "turn 1's file went");
    assert!(!root.join("b.txt").exists());
    assert_eq!(forked.session.cwd.as_deref(), Some(root.as_path()));
    assert!(forked.fork.worktree.is_none());
    let restored = forked.fork.restored.clone().expect("the restore");
    assert_eq!(restored.checkpoint.kind, CheckpointKind::Restore);
    assert!(restored.written.contains(&"a.txt".to_owned()));
    assert!(restored.deleted.contains(&"t1.txt".to_owned()));

    // Undo is the restore's own checkpoint, through the usual request.
    let preview = env
        .client
        .checkpoint_preview(CheckpointPreviewRequest {
            checkpoint_id: restored.checkpoint.id,
            through: None,
            paths: vec![],
        })
        .await
        .unwrap();
    env.client
        .checkpoint_restore(restore_of(&preview, restored.checkpoint.id))
        .await
        .unwrap();
    assert_eq!(status(root), end_of_turn_2, "the files of turn 2 again");
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).unwrap(),
        "alpha t1\n"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_files_refuses_a_file_the_turns_do_not_explain_and_changes_nothing() {
    let mut env = Env::start(2, 1, "").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap()
    })
    .await;
    env.turn(1, |root| {
        fs::write(root.join("a.txt"), "alpha t1\n").unwrap()
    })
    .await;
    let turns = env.turn_ids().await;
    // The end of the last turn is recorded in the background: an edit made before that would
    // be the turn's own.
    env.wait_checkpoints(2).await;
    // The user edits the file after the last turn.
    fs::write(env.root.join("a.txt"), "by the user\n").unwrap();
    let sessions = env.sessions().await.len();
    let before = status(&env.root);

    let error = env
        .try_fork(ChatFork {
            restore_files: true,
            ..env.fork(Some(turns[0]))
        })
        .await
        .unwrap_err();
    let ClientError::Remote(body) = error else {
        panic!("{error:?}");
    };
    assert_eq!(body.code, ErrorCode::Conflict);
    assert!(
        body.data.as_ref().unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == "a.txt" && e["state"] == "after"),
        "the preview is the error's data: {body:?}"
    );
    assert_eq!(
        fs::read_to_string(env.root.join("a.txt")).unwrap(),
        "by the user\n"
    );
    assert_eq!(status(&env.root), before);
    assert_eq!(env.sessions().await.len(), sessions, "no session");
    assert!(env.received_of("thread/fork").is_empty());
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_may_fork_into_a_worktree_but_never_restore_files() {
    let env = three_turns().await;
    let turns = env.turn_ids().await;
    let mut options = env.daemon.options();
    options.client.origin = Some(Origin {
        session_id: env.session.id,
    });
    let agent = Client::connect(options).await.unwrap();
    let error = agent
        .chat_fork(ChatFork {
            restore_files: true,
            ..env.fork(Some(turns[0]))
        })
        .await;
    assert_eq!(remote_code(error), ErrorCode::Forbidden);
    assert_eq!(
        fs::read_to_string(env.root.join("a.txt")).unwrap(),
        "alpha t1\n"
    );
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn archive_parent_closes_the_original_once_the_fork_is_up() {
    let env = three_turns().await;
    let turns = env.turn_ids().await;
    let forked = env
        .try_fork(ChatFork {
            archive_parent: true,
            ..env.fork(Some(turns[1]))
        })
        .await
        .unwrap();
    assert!(forked.fork.parent_archived);
    assert!(forked.fork.notes.is_empty());
    let listed: Vec<SessionId> = env.sessions().await.iter().map(|s| s.id).collect();
    assert!(!listed.contains(&env.session.id), "closed: {listed:?}");
    assert!(listed.contains(&forked.session.id));
    // Closing keeps the journal and the session can come back (`session.reopen`).
    let reopened = env
        .client
        .call::<SessionResult>(Request::SessionReopen(
            hutan_protocol::messages::SessionRef {
                session_id: env.session.id,
            },
        ))
        .await
        .unwrap();
    assert_eq!(reopened.session.id, env.session.id);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_that_cannot_start_takes_its_worktree_away_again() {
    // The script has no transcript for the fork's agent: its start fails.
    let mut env = Env::start(1, 0, "").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap()
    })
    .await;
    let worktrees = env.worktrees();
    let branches = git(&env.root, &["branch", "--format=%(refname:short)"]);
    let sessions = env.sessions().await.len();

    let error = env
        .try_fork(ChatFork {
            where_: ForkWhere::NewWorktree,
            branch: Some("fork/never".to_owned()),
            ..env.fork(None)
        })
        .await;
    assert!(error.is_err());
    assert_eq!(env.worktrees(), worktrees, "the worktree went");
    assert_eq!(
        git(&env.root, &["branch", "--format=%(refname:short)"]),
        branches,
        "and its branch"
    );
    assert_eq!(env.sessions().await.len(), sessions);
    env.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fork_that_cannot_start_undoes_the_restore_it_made() {
    let mut env = Env::start(2, 0, "").await;
    env.turn(0, |root| {
        fs::write(root.join("a.txt"), "alpha t0\n").unwrap()
    })
    .await;
    env.turn(1, |root| {
        fs::write(root.join("a.txt"), "alpha t1\n").unwrap()
    })
    .await;
    let turns = env.turn_ids().await;
    let before = status(&env.root);

    let error = env
        .try_fork(ChatFork {
            restore_files: true,
            ..env.fork(Some(turns[0]))
        })
        .await
        .unwrap_err();
    let ClientError::Remote(body) = error else {
        panic!("{error:?}");
    };
    assert!(
        body.message.contains("were put back to how they were"),
        "{}",
        body.message
    );
    assert_eq!(status(&env.root), before);
    assert_eq!(
        fs::read_to_string(env.root.join("a.txt")).unwrap(),
        "alpha t1\n",
        "the restore was undone"
    );
    env.stop().await;
}
