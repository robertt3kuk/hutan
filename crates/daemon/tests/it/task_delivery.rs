//! What the turns of a chat task leave for review (FX-REVIEW), end to end: a real daemon whose
//! Codex is `hutan-codex-fixture replay` holding each turn open after `turn/started` while the
//! test edits the checkout (as the agent would), in a git repository the test creates, so the
//! turns get real checkpoints. A turn that changed nothing leaves the delivery as it was; a
//! turn that changed files leaves it `awaiting_review`; the user's next message re-opens the
//! work; a Finish of the checkout delivers it.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::checkpoint_turns::{codex, git, git_env, held_turns, repository};
use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{TestDaemon, WAIT, wait_event};
use hutan_client::Client;
use hutan_core::{
    AfterPolicy, AgentEventKind, DeliveryActor, DeliveryState, SessionId, Task, TaskId, TaskRun,
    TaskState, WorkspaceId,
};
use hutan_protocol::messages::{ClientKind, Event, Request, WorkspaceOpen, WorkspaceOpened};
use hutan_protocol::task::TaskCreate;
use hutan_protocol::worktree::{MergeStrategy, StepStatus, WorktreeFinish};
use tokio::sync::broadcast;
use tokio::time::{sleep, timeout};

struct Env {
    daemon: TestDaemon,
    work: tempfile::TempDir,
    root: PathBuf,
    /// The CLI, subscribed to events.
    client: Client,
    events: broadcast::Receiver<(hutan_core::Seq, Event)>,
    /// The desktop: the user who chats and finishes.
    user: Client,
    workspace: WorkspaceId,
}

impl Env {
    /// A daemon whose Codex replays `turns` held turns, in a fresh repository opened as a
    /// workspace.
    async fn start(turns: usize) -> Self {
        let work = tempfile::tempdir().unwrap();
        let root = repository(work.path());
        let gates: Vec<PathBuf> = (0..turns)
            .map(|n| work.path().join(format!("gate-{n}")))
            .collect();
        let transcript = held_turns(work.path(), &gates);
        let config = codex(&transcript, &work.path().join("received.jsonl"));
        let daemon = TestDaemon::start_with("", |c| {
            c.codex = Some(config);
            c.checkpoints.git_env = git_env();
        })
        .await;
        let (client, events) = daemon.connect_subscribed().await;
        let user = daemon.connect_as(ClientKind::Desktop).await;
        let workspace = client
            .call::<WorkspaceOpened>(Request::WorkspaceOpen(WorkspaceOpen {
                path: root.clone(),
                created_by: None,
            }))
            .await
            .unwrap()
            .workspace
            .id;
        Self {
            daemon,
            work,
            root,
            client,
            events,
            user,
            workspace,
        }
    }

    /// Lets held turn `n` go on to its end.
    fn release(&self, n: usize) {
        fs::write(self.work.path().join(format!("gate-{n}")), "").unwrap();
    }

    /// A Codex chat task in the main checkout, started: its first turn is held.
    async fn chat_task(&self, title: &str) -> (Task, SessionId) {
        let task = self
            .client
            .task_create(TaskCreate {
                workspace_id: self.workspace,
                title: title.to_owned(),
                prompt: Some(format!("{title}.")),
                depends_on: Vec::new(),
                placement: None,
                run: Some(TaskRun::Chat {
                    agent: "codex".into(),
                    model: None,
                    account: None,
                }),
                auto_start: false,
                after_policy: AfterPolicy::Done,
            })
            .await
            .unwrap();
        let started = self.client.task_start(task.id).await.unwrap();
        (started.task, started.session.id)
    }

    async fn task(&self, id: TaskId) -> Task {
        self.client
            .task_list(None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.id == id)
            .expect("task listed")
    }

    /// Polls until the task satisfies `pick`.
    async fn wait(&self, id: TaskId, what: &str, pick: impl Fn(&Task) -> bool) -> Task {
        let found = timeout(WAIT, async {
            loop {
                let task = self.task(id).await;
                if pick(&task) {
                    return task;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        match found {
            Ok(task) => task,
            Err(_) => panic!("task {id}: {what}; it is {:?}", self.task(id).await),
        }
    }

    /// The next turn of `session` that starts.
    async fn turn_started(&mut self, session: SessionId) {
        wait_event(
            &mut self.events,
            "a turn that starts",
            |event| match event {
                Event::ChatEvent { session_id, event }
                    if *session_id == session && event.kind == AgentEventKind::TurnStarted =>
                {
                    Some(())
                }
                _ => None,
            },
        )
        .await;
    }

    /// The end of the next turn of `session`, once its checkpoint is recorded (`unchanged`
    /// rows are not listed, so the row is read where the daemon keeps it).
    async fn turn_recorded(&mut self, session: SessionId, turns: usize) {
        wait_event(&mut self.events, "the turn's end", |event| match event {
            Event::ChatEvent { session_id, event }
                if *session_id == session && event.kind.ends_turn() =>
            {
                Some(())
            }
            _ => None,
        })
        .await;
        let database = self.daemon.paths.database.clone();
        let settled = timeout(WAIT, async {
            loop {
                let rows = settled_rows(&database, session);
                if rows >= turns {
                    return;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            settled.is_ok(),
            "the checkpoint of turn {turns} never settled"
        );
    }
}

/// How many turn checkpoints of `session` are recorded (not `running`).
fn settled_rows(database: &Path, session: SessionId) -> usize {
    let db = rusqlite::Connection::open(database).unwrap();
    db.query_row(
        "SELECT COUNT(*) FROM checkpoints WHERE session_id = ?1 AND state != 'running'",
        [session.to_string()],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| usize::try_from(n).unwrap())
    .unwrap()
}

fn deliveries(task: &Task) -> Vec<(DeliveryState, DeliveryActor)> {
    task.delivery_history
        .iter()
        .map(|r| (r.state, r.by))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_turn_that_changed_files_awaits_review_and_the_next_message_reopens_the_work() {
    let mut t = Env::start(3).await;
    let (task, session) = t.chat_task("Look around the project").await;

    // Turn 1 only reads: the task is done and nothing awaits review.
    t.release(0);
    t.wait(task.id, "never done", |task| task.state == TaskState::Done)
        .await;
    t.turn_recorded(session, 1).await;
    let read_only = t.task(task.id).await;
    assert_eq!(read_only.delivery, DeliveryState::None);
    assert!(read_only.delivery_history.is_empty(), "{read_only:?}");

    // The user goes on; turn 2 edits a file: its work awaits review.
    t.user
        .chat_send(session, "Change the greeting.")
        .await
        .unwrap();
    t.turn_started(session).await;
    let running = t.task(task.id).await;
    assert_eq!(
        running.delivery,
        DeliveryState::None,
        "nothing was re-opened"
    );
    assert!(running.delivery_history.is_empty(), "{running:?}");
    fs::write(t.root.join("a.txt"), "hello from the agent\n").unwrap();
    t.release(1);
    let changed = t
        .wait(task.id, "the changing turn never awaited review", |task| {
            task.delivery == DeliveryState::AwaitingReview
        })
        .await;
    assert_eq!(
        changed.state,
        TaskState::Done,
        "a later turn leaves the task done"
    );
    assert_eq!(
        deliveries(&changed),
        [(DeliveryState::AwaitingReview, DeliveryActor::Daemon)]
    );

    // The user's next message re-opens the work, in the user's name; the next turn that
    // changes files has it await review again.
    t.user
        .chat_send(session, "Simplify total().")
        .await
        .unwrap();
    let reopened = t
        .wait(task.id, "the new turn never re-opened the work", |task| {
            task.delivery == DeliveryState::None
        })
        .await;
    assert_eq!(
        deliveries(&reopened),
        [
            (DeliveryState::AwaitingReview, DeliveryActor::Daemon),
            (DeliveryState::None, DeliveryActor::Desktop),
        ]
    );
    fs::write(t.root.join("b.txt"), "total\n").unwrap();
    t.release(2);
    let again = t
        .wait(task.id, "the second change never awaited review", |task| {
            task.delivery == DeliveryState::AwaitingReview
        })
        .await;
    assert_eq!(again.delivery_history.len(), 3, "{again:?}");

    // Finish commits the checkout: the open work of the task that ran there is merged with
    // the commit, by the desktop.
    let finished = t
        .user
        .worktree_finish(WorktreeFinish {
            checkout: t.root.clone(),
            message: "Change the greeting".into(),
            push: Some(false),
            into: None,
            strategy: MergeStrategy::Merge,
        })
        .await
        .unwrap();
    assert_eq!(finished.commit.status, StepStatus::Ok, "{finished:?}");
    let commit = finished.commit.commit.clone().unwrap();
    assert_eq!(git(&t.root, &["rev-parse", "HEAD"]).trim(), commit);
    let merged = t.task(task.id).await;
    assert_eq!(merged.delivery, DeliveryState::Merged);
    let record = merged.delivery_history.last().unwrap();
    assert_eq!(record.commit.as_deref(), Some(commit.as_str()));
    assert_eq!(record.by, DeliveryActor::Desktop);
    t.daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finish_delivers_only_the_open_work_of_its_checkout() {
    let mut t = Env::start(1).await;
    // A chat that changed files, then one that changed nothing, in the same checkout.
    let (edits, edits_session) = t.chat_task("Change the greeting").await;
    fs::write(t.root.join("a.txt"), "hi\n").unwrap();
    t.release(0);
    t.wait(edits.id, "never awaited review", |task| {
        task.delivery == DeliveryState::AwaitingReview
    })
    .await;
    t.turn_recorded(edits_session, 1).await;
    // Each chat replays the transcript from its start: this one's turn is not held any more.
    let (reads, reads_session) = t.chat_task("Look around").await;
    t.wait(reads.id, "never done", |task| task.state == TaskState::Done)
        .await;
    t.turn_recorded(reads_session, 1).await;

    // A Finish that fails changes nothing: the remote has no upstream branch to push to (a
    // repository without any remote is a commit-only Finish, which lands).
    git(
        &t.root,
        &["remote", "add", "origin", "/nonexistent/origin.git"],
    );
    let failed = t
        .user
        .worktree_finish(WorktreeFinish {
            checkout: t.root.clone(),
            message: "Change the greeting".into(),
            push: Some(true),
            into: None,
            strategy: MergeStrategy::Merge,
        })
        .await
        .unwrap();
    assert_eq!(
        failed.push.as_ref().map(|p| p.status),
        Some(StepStatus::Failed),
        "{failed:?}"
    );
    assert_eq!(
        t.task(edits.id).await.delivery,
        DeliveryState::AwaitingReview,
        "a push that failed delivered nothing"
    );

    // That Finish committed before its push failed; the next change is finished without one.
    fs::write(t.root.join("a.txt"), "hi again\n").unwrap();
    let done = t
        .user
        .worktree_finish(WorktreeFinish {
            checkout: t.root.clone(),
            message: "Change the greeting".into(),
            push: Some(false),
            into: None,
            strategy: MergeStrategy::Merge,
        })
        .await
        .unwrap();
    assert_eq!(done.commit.status, StepStatus::Ok, "{done:?}");
    assert_eq!(t.task(edits.id).await.delivery, DeliveryState::Merged);
    let untouched = t.task(reads.id).await;
    assert_eq!(untouched.delivery, DeliveryState::None);
    assert!(untouched.delivery_history.is_empty(), "{untouched:?}");
    t.daemon.stop().await;
}
