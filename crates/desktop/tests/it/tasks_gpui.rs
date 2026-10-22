#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Tasks panel and the Changes view (M7-C) against an in-process daemon: tasks are created
//! and changed through the real `task.*` requests, git repositories are made by the tests in
//! temporary directories, and the panel, the form, the actions, the Changes view and the merge
//! flow go through the real requests too.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{
    AfterPolicy, DeliveryActor, DeliveryState, ReviewDecision, SessionId, Task, TaskId,
    TaskPlacement, TaskPlacementKind, TaskRun, TaskState, WorkspaceId,
};
use hutan_desktop::sessions::attention::Tone;
use hutan_desktop::sessions::launch::LaunchKind;
use hutan_desktop::sessions::tasks::{TaskAction, TaskRowView};
use hutan_desktop::settings::TasksSettings;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::TasksPanel;
use hutan_desktop::workbench::{
    ChangesView, CleanupDefault, DeliveryForm, LaunchWhere, LauncherField, Region, Workbench,
};
use hutan_protocol::messages::Request;
use hutan_protocol::task::{
    TaskCreate, TaskList, TaskListRequest, TaskRef, TaskResult, TaskReview, TaskReviewed,
    TaskStarted, TaskUpdate,
};
use hutan_protocol::worktree::{
    MergeStrategy, WorktreeCreate, WorktreeCreated, WorktreeList, WorktreeListRequest,
};
use hutan_settings::{SettingsPaths, SettingsStore};

// Fixtures ------------------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// `<tmp>/repo` on `main` with `a.txt` and an identity the daemon's git calls find.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap().join("repo");
    fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&root, &["config", key, value]);
    }
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&root, "first");
    (dir, root)
}

struct Env {
    daemon: TestDaemon,
    _keep: tempfile::TempDir,
    root: PathBuf,
    workspace: WorkspaceId,
}

impl Env {
    fn new() -> Self {
        Self::with_daemon(TestDaemon::start())
    }

    /// [`new`](Self::new) with `daemon_toml` as the daemon's `daemon.toml`.
    fn with_config(daemon_toml: &str) -> Self {
        Self::with_daemon(TestDaemon::start_with_config(daemon_toml))
    }

    fn with_daemon(daemon: TestDaemon) -> Self {
        let (keep, root) = repo();
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        Self {
            daemon,
            _keep: keep,
            root,
            workspace,
        }
    }

    fn create(&self, title: &str, deps: &[&Task], build: impl FnOnce(&mut TaskCreate)) -> Task {
        let mut request = TaskCreate {
            workspace_id: self.workspace,
            title: title.to_owned(),
            prompt: None,
            depends_on: deps.iter().map(|t| t.id).collect(),
            placement: None,
            run: Some(TaskRun::Terminal {
                argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
            }),
            auto_start: false,
            after_policy: AfterPolicy::Done,
        };
        build(&mut request);
        let created: TaskResult = self.daemon.call(Request::TaskCreate(request));
        created.task
    }

    fn plain(&self, title: &str, deps: &[&Task]) -> Task {
        self.create(title, deps, |_| {})
    }

    fn set_state(&self, task: &Task, state: TaskState) -> Task {
        let updated: TaskResult = self.daemon.call(Request::TaskUpdate(TaskUpdate {
            id: task.id,
            title: None,
            prompt: None,
            depends_on: None,
            placement: None,
            run: None,
            auto_start: None,
            after_policy: None,
            state: Some(state),
        }));
        updated.task
    }

    fn tasks(&self) -> Vec<Task> {
        self.daemon
            .call::<TaskList>(Request::TaskList(TaskListRequest::default()))
            .tasks
    }

    fn task(&self, id: TaskId) -> Option<Task> {
        self.tasks().into_iter().find(|t| t.id == id)
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(env.daemon.home())),
        env.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = env.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    (view, cx)
}

fn show_panel(view: &Entity<Workbench>, cx: &mut VisualTestContext) -> Entity<TasksPanel> {
    cx.dispatch_action(actions::OpenPanel("tasks".into()));
    draw(cx);
    view.read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .expect("the tasks panel is drawn in the right dock")
}

fn rows(panel: &Entity<TasksPanel>, cx: &VisualTestContext) -> Vec<TaskRowView> {
    panel.read_with(cx, |p, cx| p.rows(0, cx))
}

fn shape(panel: &Entity<TasksPanel>, cx: &VisualTestContext) -> Vec<(String, usize, &'static str)> {
    rows(panel, cx)
        .into_iter()
        .map(|r| (r.title, r.depth, r.glyph.id()))
        .collect()
}

fn wait_rows(panel: &Entity<TasksPanel>, cx: &mut VisualTestContext, what: &str, count: usize) {
    let panel = panel.clone();
    wait_until(cx, what, move |cx| rows(&panel, cx).len() == count);
}

fn region(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    region: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(region))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, what: Region) {
    wait_until(cx, &format!("{what:?} to be drawn"), |cx| {
        region(view, cx, what).is_some()
    });
    let bounds = region(view, cx, what).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn settings(view: &Entity<Workbench>, cx: &VisualTestContext) -> SettingsStore {
    view.read_with(cx, |wb, _| wb.settings_store().clone())
}

// The panel -----------------------------------------------------------------------------------

#[gpui::test]
fn the_panel_shows_the_dependency_tree_and_follows_task_updates(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let test = env.plain("Test", &[&build]);
    let ship = env.plain("Ship", &[&test]);
    let lint = env.plain("Lint", &[]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "four rows", 4);

    // Each task under the one it waits for; ready (◎) and waiting (○) tell apart.
    assert_eq!(
        shape(&panel, cx),
        [
            ("Build".to_owned(), 0, "state.ready"),
            ("Test".to_owned(), 1, "state.todo"),
            ("Ship".to_owned(), 2, "state.todo"),
            ("Lint".to_owned(), 0, "state.ready"),
        ]
    );
    let first = rows(&panel, cx).remove(0);
    assert_eq!(
        (first.placement.as_str(), first.run.as_str()),
        ("main", "$ sh -c sleep 60")
    );
    for index in 0..4 {
        wait_until(cx, "the rows to be drawn", |cx| {
            region(&view, cx, Region::TaskRow(index)).is_some()
        });
    }

    // task.updated: finishing Build makes Test ready; the tree follows without a refresh.
    env.set_state(&build, TaskState::Done);
    wait_until(cx, "Build done and Test ready", |cx| {
        shape(&panel, cx)[..2]
            == [
                ("Build".to_owned(), 0, "state.completed"),
                ("Test".to_owned(), 1, "state.ready"),
            ]
    });

    // A new task appears; a deleted one goes (task.removed).
    let extra = env.plain("Extra", &[&lint]);
    wait_rows(&panel, cx, "five rows", 5);
    assert_eq!(rows(&panel, cx).last().unwrap().depth, 1);
    let _: hutan_protocol::task::TaskDeleted =
        env.daemon
            .call(Request::TaskDelete(hutan_protocol::task::TaskDelete {
                id: extra.id,
                cascade: false,
            }));
    wait_rows(&panel, cx, "four rows again", 4);
    let _ = ship;
}

#[gpui::test]
fn filters_hide_done_tasks_and_other_workspaces(cx: &mut TestAppContext) {
    let env = Env::new();
    let other = env
        .daemon
        .create_workspace("other", &[Path::new("/tmp")])
        .id;
    let done = env.plain("Done one", &[]);
    env.set_state(&done, TaskState::Done);
    env.plain("Open one", &[]);
    let _: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: other,
        title: "Elsewhere".into(),
        prompt: None,
        depends_on: vec![],
        placement: None,
        run: None,
        auto_start: false,
        after_policy: AfterPolicy::Done,
    }));
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "this workspace's two tasks", 2);

    // tasks.show_done = false: the done one goes; failed ones would stay.
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(false))
        .unwrap();
    wait_rows(&panel, cx, "one open task", 1);
    assert_eq!(rows(&panel, cx)[0].title, "Open one");

    // tasks.scope = "all": the other workspace's task too.
    settings(&view, cx)
        .write_user("tasks.scope", toml::Value::String("all".into()))
        .unwrap();
    wait_rows(&panel, cx, "two open tasks of every workspace", 2);
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(true))
        .unwrap();
    wait_rows(&panel, cx, "all three", 3);

    // The chips write the same settings.
    click(&view, cx, Region::Tasks("chip-done"));
    assert!(!settings(&view, cx).get::<TasksSettings>().show_done);
    click(&view, cx, Region::Tasks("chip-scope"));
    assert_eq!(
        settings(&view, cx).get::<TasksSettings>().scope.key(),
        "workspace"
    );
    wait_rows(&panel, cx, "one open task of this workspace", 1);
}

#[gpui::test]
fn an_empty_panel_shows_the_mascot_and_offers_a_new_task(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_until(cx, "the empty state with its action", |cx| {
        region(&view, cx, Region::Tasks("empty-new")).is_some()
    });
    assert!(rows(&panel, cx).is_empty());
    // "+ New task" is the Start… sheet on "Queue as task", in the project of the panel.
    click(&view, cx, Region::Tasks("empty-new"));
    view.read_with(cx, |wb, _| {
        let launcher = wb.launcher().expect("the Start… sheet");
        assert!(launcher.queue());
        assert_eq!(launcher.project(), Some(env.workspace));
    });
}

fn nav_keys(view: &Entity<Workbench>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |wb, cx| {
        wb.nav_entries(cx).into_iter().map(|e| e.key).collect()
    })
}

#[gpui::test]
fn the_tasks_nav_row_appears_with_the_first_task_and_goes_with_the_last(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    draw(cx);
    assert!(
        !nav_keys(&view, cx).contains(&"tasks".to_owned()),
        "no task, no row: {:?}",
        nav_keys(&view, cx)
    );
    assert!(region(&view, cx, Region::Nav("tasks")).is_none());
    let agents_before = region(&view, cx, Region::Nav("agents")).expect("the Agents row");

    let first = env.plain("Build", &[]);
    wait_until(cx, "the Tasks row to reveal itself", |cx| {
        nav_keys(&view, cx).contains(&"tasks".to_owned())
    });
    draw(cx);
    assert!(region(&view, cx, Region::Nav("tasks")).is_some());
    // It comes in below the rows that were there: they do not move.
    assert_eq!(
        nav_keys(&view, cx).last().map(String::as_str),
        Some("tasks")
    );
    assert_eq!(
        region(&view, cx, Region::Nav("agents")),
        Some(agents_before),
        "the Agents row stays where it was"
    );

    let _: hutan_protocol::task::TaskDeleted =
        env.daemon
            .call(Request::TaskDelete(hutan_protocol::task::TaskDelete {
                id: first.id,
                cascade: false,
            }));
    wait_until(cx, "the Tasks row to go with the last task", |cx| {
        !nav_keys(&view, cx).contains(&"tasks".to_owned())
    });
}

#[gpui::test]
fn the_palette_has_the_task_and_review_commands(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::TogglePalette);
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    for title in [
        "Tasks: New Task…",
        "Tasks: Show Panel",
        "Worktree: Review Changes…",
    ] {
        assert!(titles.iter().any(|t| t.as_ref() == title), "{title}");
    }
}

#[gpui::test]
fn the_palette_command_opens_the_start_sheet_on_queue(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::NewTask);
    draw(cx);
    view.read_with(cx, |wb, _| {
        let launcher = wb.launcher().expect("the Start… sheet");
        assert!(launcher.queue());
        assert_eq!(launcher.project(), Some(env.workspace));
    });
    wait_until(cx, "the prompt field", |cx| {
        region(&view, cx, Region::Launcher("prompt")).is_some()
    });
}

#[gpui::test]
fn row_actions_send_the_right_requests(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let waits = env.plain("Waits", &[&build]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    // A ready task offers Start (and the others); a waiting one does not.
    click(&view, cx, Region::TaskRow(0));
    let actions_of = |cx: &VisualTestContext, title: &str| {
        rows(&panel, cx)
            .into_iter()
            .find(|r| r.title == title)
            .unwrap()
            .actions
    };
    assert_eq!(
        actions_of(cx, "Build"),
        [
            TaskAction::Start,
            TaskAction::Done,
            TaskAction::Cancel,
            TaskAction::Delete
        ]
    );
    assert_eq!(
        actions_of(cx, "Waits"),
        [TaskAction::Done, TaskAction::Cancel, TaskAction::Delete]
    );

    // Start: the daemon starts it and a session works on it.
    click(&view, cx, Region::TaskAction(0, "start"));
    wait_until(cx, "the task to run", |_| {
        env.task(build.id)
            .is_some_and(|t| t.state == TaskState::Running && t.sessions.len() == 1)
    });
    wait_until(cx, "the row to say running", |cx| {
        rows(&panel, cx)[0].state == TaskState::Running
    });
    assert_eq!(
        actions_of(cx, "Build"),
        [TaskAction::Done, TaskAction::Cancel]
    );

    // Cancel stops it.
    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskRow(0));
    wait_until(cx, "the cancel button", |cx| {
        region(&view, cx, Region::TaskAction(0, "cancel")).is_some()
    });
    click(&view, cx, Region::TaskAction(0, "cancel"));
    wait_until(cx, "the task to be cancelled", |_| {
        env.task(build.id)
            .is_some_and(|t| t.state == TaskState::Cancelled)
    });

    // Reset puts it back to todo (ready again: nothing to wait for).
    wait_until(cx, "the reset button", |cx| {
        region(&view, cx, Region::TaskAction(0, "reset")).is_some()
    });
    click(&view, cx, Region::TaskAction(0, "reset"));
    wait_until(cx, "the task to be ready again", |cx| {
        rows(&panel, cx)[0].state == TaskState::Ready
    });

    // Done marks it done; the waiting task becomes ready.
    click(&view, cx, Region::TaskAction(0, "done"));
    wait_until(cx, "Build done and Waits ready", |cx| {
        let rows = rows(&panel, cx);
        rows[0].state == TaskState::Done && rows[1].state == TaskState::Ready
    });
    assert_eq!(
        env.task(waits.id).unwrap().outcome.as_deref(),
        None,
        "only Build was marked"
    );
    assert_eq!(
        env.task(build.id).unwrap().outcome.as_deref(),
        Some("marked done")
    );
}

#[gpui::test]
fn deleting_a_task_others_wait_for_asks_about_its_dependants(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    env.plain("Test", &[&build]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskAction(0, "delete"));
    wait_until(cx, "the question", |cx| {
        panel.read_with(cx, |p, _| p.cascade_ask().is_some())
    });
    let message = panel.read_with(cx, |p, _| p.cascade_ask().unwrap().message.clone());
    assert!(message.contains("Test"), "{message}");
    assert_eq!(env.tasks().len(), 2, "nothing was deleted yet");

    // Keep: nothing changes. Ask again and delete them too.
    click(&view, cx, Region::Tasks("cascade-no"));
    assert_eq!(env.tasks().len(), 2);
    assert!(panel.read_with(cx, |p, _| p.cascade_ask().is_none()));
    click(&view, cx, Region::TaskAction(0, "delete"));
    wait_until(cx, "the question again", |cx| {
        panel.read_with(cx, |p, _| p.cascade_ask().is_some())
    });
    click(&view, cx, Region::Tasks("cascade-yes"));
    wait_until(cx, "both tasks to go", |_| env.tasks().is_empty());
    wait_rows(&panel, cx, "no rows", 0);
}

#[gpui::test]
fn clicking_a_row_focuses_the_session_of_its_task(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let started: hutan_protocol::task::TaskStarted =
        env.daemon
            .call(Request::TaskStart(hutan_protocol::task::TaskRef {
                id: build.id,
            }));
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "one row", 1);
    wait_until(cx, "the row to carry its session", |cx| {
        rows(&panel, cx)[0].session == Some(started.session.id)
    });
    click(&view, cx, Region::TaskRow(0));
    let item = ItemRef::Terminal {
        session: started.session.id.to_string(),
    };
    wait_until(cx, "the session's tab", |cx| {
        view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some())
    });
}

// The Start… sheet ---------------------------------------------------------------------------

#[gpui::test]
fn the_sheet_creates_a_task_with_dependencies_and_a_worktree_placement(cx: &mut TestAppContext) {
    let env = Env::new();
    let build = env.plain("Build", &[]);
    let lint = env.plain("Lint", &[]);
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "two rows", 2);

    click(&view, cx, Region::Tasks("new"));
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_some()));

    // An empty command says what is missing.
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Command, cx));
    click(&view, cx, Region::Launcher("start"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .launcher()
            .unwrap()
            .error()
            .map(str::to_owned)),
        Some("Type the command to run.".to_owned())
    );

    // Fill it in with the controls and the keyboard.
    click(&view, cx, Region::Launcher("command"));
    cx.simulate_input("cargo build --release");
    click(&view, cx, Region::Launcher("title"));
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Title, "Ship it", cx);
    });
    click(&view, cx, Region::LauncherChoice("where", "new"));
    click(&view, cx, Region::Launcher("branch"));
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Branch, "release/1.0", cx);
    });
    view.update(cx, |wb, cx| {
        wb.launcher_toggle_more(cx);
        wb.launcher_toggle_after(build.id, cx);
        wb.launcher_toggle_after(lint.id, cx);
        wb.launcher_toggle_after(build.id, cx); // un-tick the first again
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().after()),
        [lint.id]
    );
    // "+ New task" opened the sheet on Queue as task.
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
    click(&view, cx, Region::Launcher("start"));

    wait_until(cx, "the task to exist", |_| {
        env.tasks().iter().any(|t| t.title == "Ship it")
    });
    let created = env
        .tasks()
        .into_iter()
        .find(|t| t.title == "Ship it")
        .unwrap();
    assert_eq!(created.depends_on, [lint.id]);
    assert!(created.auto_start);
    assert_eq!(
        created.placement,
        TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("release/1.0".into()),
            base: Some("main".into()),
        }
    );
    assert_eq!(
        created.run,
        Some(TaskRun::Terminal {
            argv: hutan_desktop::sessions::launch::shell_argv(
                &hutan_desktop::sessions::launch::user_shell(),
                "cargo build --release"
            )
        })
    );
    // The sheet closed and the new row is in the tree under Lint, with its badge.
    wait_until(cx, "the sheet to close and the row to appear", |cx| {
        view.read_with(cx, |wb, _| wb.launcher().is_none()) && rows(&panel, cx).len() == 3
    });
    let ship = rows(&panel, cx)
        .into_iter()
        .find(|r| r.title == "Ship it")
        .unwrap();
    assert_eq!(ship.depth, 1);
    assert_eq!(ship.placement, "wt release/1.0");
}

#[gpui::test]
fn the_sheet_asks_for_a_prompt_and_shows_the_daemons_refusal(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let _panel = show_panel(&view, cx);
    click(&view, cx, Region::Tasks("empty-new"));
    wait_until(cx, "the agent choices", |cx| {
        region(&view, cx, Region::LauncherChoice("agent", "codex")).is_some()
    });
    // omp and Claude Code chats are offered next to Codex.
    assert!(region(&view, cx, Region::LauncherChoice("agent", "omp")).is_some());
    assert!(region(&view, cx, Region::LauncherChoice("agent", "claude")).is_some());

    // A chat needs its first message.
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Title, "Review", cx);
    });
    click(&view, cx, Region::Launcher("start"));
    let error = view.read_with(cx, |wb, _| {
        wb.launcher().unwrap().error().map(str::to_owned)
    });
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("needs a prompt")),
        "{error:?}"
    );
    assert!(env.tasks().is_empty(), "nothing was sent");

    // The daemon's own refusal comes back in the sheet: it takes no branch with a space in it.
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(LaunchKind::Command, cx);
        wb.launcher_set_field(LauncherField::Command, "true", cx);
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx);
        wb.launcher_set_field(LauncherField::Branch, "bad branch name", cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the daemon's refusal in the sheet", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .and_then(|l| l.error().map(str::to_owned))
                .is_some_and(|e| e.contains("is not a valid branch"))
        })
    });
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_some()));
    assert!(env.tasks().is_empty(), "nothing was created");

    // Fixing the branch lets it through; "+ New task" queues it.
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Branch, "review/it", cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the task to be created", |_| !env.tasks().is_empty());
    wait_until(cx, "the sheet to close", |cx| {
        view.read_with(cx, |wb, _| wb.launcher().is_none())
    });
}

// The Changes view ------------------------------------------------------------------------------

/// A worktree `feature/x` of `env.root` with a commit, a rename-free edit and an untracked file.
fn feature_worktree(env: &Env) -> PathBuf {
    let created: WorktreeCreated = env.daemon.call(Request::WorktreeCreate(WorktreeCreate {
        workspace_id: env.workspace,
        repo: None,
        branch: "feature/x".into(),
        base: None,
        path: None,
    }));
    let wt = created.worktree.path;
    fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    commit_all(&wt, "feature work");
    fs::write(wt.join("untracked.txt"), "u1\nu2\n").unwrap();
    wt
}

fn changes(view: &Entity<Workbench>, cx: &VisualTestContext, wt: &Path) -> Entity<ChangesView> {
    view.read_with(cx, |wb, _| wb.changes(&wt.display().to_string()))
        .expect("the Changes view was opened")
}

#[gpui::test]
fn the_changes_view_lists_files_and_renders_a_diff_with_folding_hunks(cx: &mut TestAppContext) {
    let env = Env::new();
    // Thirty lines on main; the branch changes lines 2 and 28: two hunks.
    fs::write(
        env.root.join("a.txt"),
        (1..=30).map(|n| format!("line {n}\n")).collect::<String>(),
    )
    .unwrap();
    commit_all(&env.root, "thirty lines");
    let wt = feature_worktree(&env);
    let edited: String = (1..=30)
        .map(|n| match n {
            2 => "line two changed\n".to_owned(),
            28 => "line 28 changed\n".to_owned(),
            n => format!("line {n}\n"),
        })
        .collect();
    fs::write(wt.join("a.txt"), edited).unwrap();
    fs::remove_file(wt.join("untracked.txt")).unwrap();
    commit_all(&wt, "two hunks");
    // Uncommitted, and never staged.
    fs::write(wt.join("scratch.txt"), "s1\ns2\n").unwrap();

    let (view, cx) = open(cx, &env);
    view.update_in(cx, |wb, window, cx| wb.open_changes(wt.clone(), window, cx));
    draw(cx);
    // It is a tab of the active pane, titled with the branch once the worktree is listed.
    let item = ItemRef::changes(wt.display().to_string());
    assert!(view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some()));
    let changes = changes(&view, cx, &wt);
    wait_until(cx, "the file list", |cx| {
        changes.read_with(cx, |c, _| c.files().len() == 3 && c.base() == Some("main"))
    });
    let listed: Vec<String> = changes.read_with(cx, |c, _| {
        c.files()
            .iter()
            .map(|f| {
                format!(
                    "{} {:?} +{} -{}",
                    f.path, f.status, f.additions, f.deletions
                )
            })
            .collect()
    });
    assert_eq!(
        listed,
        [
            "a.txt Modified +2 -2",
            "b.txt Added +1 -0",
            "scratch.txt Added +2 -0"
        ]
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.item_title(&item, cx).to_string()),
        "Changes · feature/x"
    );
    for index in 0..3 {
        wait_until(cx, "the file rows", |cx| {
            region(&view, cx, Region::ChangesFile(index)).is_some()
        });
    }

    // The first file's diff arrives: two hunks, with the changed lines in them.
    wait_until(cx, "the first patch", |cx| {
        changes.read_with(cx, |c, _| c.selected_diff().is_some())
    });
    let (hunks, adds, removes) = changes.read_with(cx, |c, _| {
        let diff = c.selected_diff().unwrap();
        (
            hutan_desktop::workbench::hunk_ranges(diff).len(),
            diff.added,
            diff.removed,
        )
    });
    assert_eq!((hunks, adds, removes), (2, 2, 2));
    wait_until(cx, "both hunk headers", |cx| {
        region(&view, cx, Region::ChangesHunk(0)).is_some()
            && region(&view, cx, Region::ChangesHunk(1)).is_some()
    });

    // Hunks fold and unfold.
    click(&view, cx, Region::ChangesHunk(0));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [0]);
    click(&view, cx, Region::ChangesHunk(1));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [0, 1]);
    click(&view, cx, Region::ChangesHunk(0));
    assert_eq!(changes.read_with(cx, |c, _| c.collapsed_hunks()), [1]);

    // Another file: an untracked one is a new file in full.
    click(&view, cx, Region::ChangesFile(2));
    wait_until(cx, "the untracked file's patch", |cx| {
        changes.read_with(cx, |c, _| {
            c.selected() == Some("scratch.txt") && c.selected_diff().is_some()
        })
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.selected_diff().map(|d| (d.added, d.removed))),
        Some((2, 0))
    );

    // Leaving out the uncommitted changes drops it from the list.
    click(&view, cx, Region::Changes("toggle"));
    assert!(
        !changes.read_with(cx, |c, _| c.include_uncommitted()),
        "the toggle was clicked"
    );
    wait_until(cx, "the committed-only list", |cx| {
        changes.read_with(cx, |c, _| !c.include_uncommitted() && c.files().len() == 2)
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.selected().map(str::to_owned)),
        Some("a.txt".to_owned()),
        "the selection moved to a file that is still listed"
    );
}

#[gpui::test]
fn a_main_checkout_without_changes_says_so_and_finishes_without_a_merge(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    let root = env.root.clone();
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(root.clone(), window, cx)
    });
    draw(cx);
    let changes = changes(&view, cx, &root);
    wait_until(cx, "the listing and the worktree", |cx| {
        changes.read_with(cx, |c, _| c.base().is_some() && c.info().is_some())
    });
    assert!(changes.read_with(cx, |c, _| c.files().is_empty() && !c.can_merge()));
    // Finish works in the main checkout too: commit and push, no merge modes.
    click(&view, cx, Region::Changes("finish"));
    assert!(changes.read_with(cx, |c, _| c.finish_open()));
    wait_until(cx, "the form", |cx| {
        region(&view, cx, Region::Changes("finish-confirm")).is_some()
    });
    assert!(region(&view, cx, Region::Changes("strategy-merge")).is_none());
    // The Push pill keeps it local for this Finish.
    click(&view, cx, Region::Changes("finish-push"));
    assert_eq!(changes.read_with(cx, |c, _| c.finish_push()), Some(false));
    click(&view, cx, Region::Changes("finish-cancel"));
    assert!(!changes.read_with(cx, |c, _| c.finish_open()));
}

/// Opens the Changes view of `wt` and waits for its list and worktree.
fn open_changes(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    wt: &Path,
) -> Entity<ChangesView> {
    view.update_in(cx, |wb, window, cx| {
        wb.open_changes(wt.to_owned(), window, cx)
    });
    draw(cx);
    let changes = changes(view, cx, wt);
    wait_until(cx, "the listing", |cx| {
        changes.read_with(cx, |c, _| !c.files().is_empty() && c.info().is_some())
    });
    changes
}

/// Opens the Finish form and turns its push off (these repositories have no remote).
fn finish_locally(
    view: &Entity<Workbench>,
    cx: &mut VisualTestContext,
    changes: &Entity<ChangesView>,
) {
    click(view, cx, Region::Changes("finish"));
    assert!(changes.read_with(cx, |c, _| c.finish_open()));
    // Through the API: a result card that just appeared moves the form under a click.
    changes.update(cx, |c, cx| c.set_finish_push(false, cx));
    assert_eq!(changes.read_with(cx, |c, _| c.finish_push()), Some(false));
}

#[gpui::test]
fn finishing_a_worktree_merges_it_in_the_main_checkout_and_offers_the_removal(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    fs::remove_file(wt.join("untracked.txt")).unwrap();
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    // Main moves on, so this is a real merge commit.
    fs::write(env.root.join("main.txt"), "m\n").unwrap();
    commit_all(&env.root, "main work");

    finish_locally(&view, cx, &changes);
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_strategy()),
        Some(MergeStrategy::Merge)
    );
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert_eq!(
        (merged.target.as_str(), merged.branch.as_str()),
        ("main", "feature/x")
    );
    assert_eq!(
        merged.commit,
        git_out(&env.root, &["rev-parse", "HEAD"]).trim()
    );
    assert_eq!(
        git_out(&env.root, &["log", "-1", "--format=%s"]).trim(),
        "Merge branch 'feature/x'"
    );
    // Each step says what it did; the toast names the merge, and the form is gone.
    let done = changes.read_with(cx, |c, _| c.finished().cloned().unwrap());
    let lines = hutan_desktop::workbench::finish_step_lines(&done);
    assert_eq!(lines[0].1, "Nothing to commit on feature/x.");
    assert!(
        lines[1].1.starts_with("Merged feature/x into main ("),
        "{lines:?}"
    );
    assert_eq!(
        lines[2].1,
        "Not pushed: push was off. The commit stays local."
    );
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string()))
            .is_some_and(|n| n.starts_with("Merged feature/x into main (")),
    );
    assert!(!changes.read_with(cx, |c, _| c.finish_open()));
    wait_until(cx, "the result and the remove offer", |cx| {
        region(&view, cx, Region::Changes("finish-result")).is_some()
            && region(&view, cx, Region::Changes("remove")).is_some()
    });

    // "Remove worktree and branch": gone, with its (merged) branch, and the tab closes.
    click(&view, cx, Region::Changes("remove"));
    wait_until(
        cx,
        "the worktree and its merged branch to be removed",
        |_| {
            !wt.exists()
                && !git_out(&env.root, &["branch", "--format=%(refname:short)"])
                    .lines()
                    .any(|b| b == "feature/x")
        },
    );
    wait_until(cx, "the tab to close", |cx| {
        view.read_with(cx, |wb, _| {
            wb.state()
                .find_item(&ItemRef::changes(wt.display().to_string()))
                .is_none()
        })
    });
}

#[gpui::test]
fn a_squash_finish_commits_the_rest_first_and_keep_dismisses_the_offer(cx: &mut TestAppContext) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    finish_locally(&view, cx, &changes);
    click(&view, cx, Region::Changes("strategy-squash"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_strategy()),
        Some(MergeStrategy::Squash)
    );
    changes.update(cx, |c, cx| c.set_finish_message("Add b", cx));
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert_eq!(merged.strategy, MergeStrategy::Squash);
    assert!(
        !merged.source_dirty,
        "Finish committed the untracked file before merging"
    );
    assert_eq!(
        changes.read_with(cx, |c, _| c.cleanup_default()),
        Some(CleanupDefault::Remove)
    );
    assert_eq!(
        git_out(&env.root, &["log", "-1", "--format=%s"]).trim(),
        "Add b"
    );
    assert_eq!(
        git_out(&env.root, &["show", "main:untracked.txt"]),
        "u1\nu2\n"
    );
    click(&view, cx, Region::Changes("keep"));
    assert!(changes.read_with(cx, |c, _| c.merged().is_none()));
    assert!(wt.exists());
    // The result stays until it is dismissed.
    click(&view, cx, Region::Changes("finish-done"));
    assert!(changes.read_with(cx, |c, _| c.finished().is_none()));
}

#[gpui::test]
fn a_conflicting_finish_says_so_with_the_files_and_a_refusal_stays_in_the_form(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let wt = feature_worktree(&env);
    fs::write(env.root.join("a.txt"), "main side\n").unwrap();
    commit_all(&env.root, "main edit");
    let head = git_out(&env.root, &["rev-parse", "HEAD"]);
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);

    finish_locally(&view, cx, &changes);
    changes.update(cx, |c, cx| c.set_finish_message("Wip", cx));
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the steps", |cx| {
        changes.read_with(cx, |c, _| c.finished().is_some())
    });
    let done = changes.read_with(cx, |c, _| c.finished().cloned().unwrap());
    let merge = done.merge.clone().unwrap();
    assert_eq!(merge.conflicts, ["a.txt"]);
    let lines = hutan_desktop::workbench::finish_step_lines(&done);
    assert!(
        lines[0].1.starts_with("Committed 1 file on feature/x"),
        "{lines:?}"
    );
    assert!(lines[1].1.contains("conflict"), "{lines:?}");
    assert_eq!(lines[1].0, hutan_desktop::workbench::StepTone::Failed);
    assert!(changes.read_with(cx, |c, _| c.merged().is_none()));
    assert_eq!(git_out(&env.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(git_out(&env.root, &["status", "--porcelain"]), "");
    // The Finish reads the checkout again; its answer changes the list above the form, so the
    // next clicks wait for it (a form that moves between mouse down and up gets no click).
    wait_until(
        cx,
        "the result card and the listing after the Finish",
        |cx| {
            region(&view, cx, Region::Changes("finish-result")).is_some()
                && !changes.read_with(cx, |c, _| c.loading())
        },
    );

    // A dirty main checkout is refused before anything changes, and the form says why.
    fs::write(env.root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit_all(&env.root, "undo the edit");
    fs::write(env.root.join("stray.txt"), "dirty\n").unwrap();
    fs::write(wt.join("more.txt"), "more\n").unwrap();
    let wt_head = git_out(&wt, &["rev-parse", "HEAD"]);
    finish_locally(&view, cx, &changes);
    changes.update(cx, |c, cx| c.set_finish_message("More", cx));
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the refusal", |cx| {
        changes.read_with(cx, |c, _| c.finish_error().is_some())
    });
    let error = changes.read_with(cx, |c, _| c.finish_error().unwrap().to_owned());
    assert!(error.contains("uncommitted changes"), "{error}");
    assert!(changes.read_with(cx, |c, _| c.finish_open()));
    assert_eq!(
        git_out(&wt, &["rev-parse", "HEAD"]),
        wt_head,
        "nothing was committed"
    );
}

// Entry points ----------------------------------------------------------------------------------

#[gpui::test]
fn a_task_row_session_badge_and_the_palette_open_the_changes_view(cx: &mut TestAppContext) {
    let env = Env::new();
    let task = env.create("Fix login", &[], |request| {
        request.placement = Some(TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("task/fix-login".into()),
            base: None,
        });
    });
    let started: hutan_protocol::task::TaskStarted =
        env.daemon
            .call(Request::TaskStart(hutan_protocol::task::TaskRef {
                id: task.id,
            }));
    let wt = {
        let list: WorktreeList = env.daemon.call(Request::WorktreeList(
            hutan_protocol::worktree::WorktreeListRequest {
                workspace_id: Some(env.workspace),
            },
        ));
        list.worktrees
            .into_iter()
            .find(|w| w.branch.as_deref() == Some("task/fix-login"))
            .expect("the task's worktree exists")
            .path
    };
    fs::write(wt.join("new.txt"), "n\n").unwrap();
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "the task", 1);
    let item = ItemRef::changes(wt.display().to_string());
    let opened = |view: &Entity<Workbench>, cx: &VisualTestContext| {
        view.read_with(cx, |wb, _| wb.state().find_item(&item).is_some())
    };

    // 1. The task row's "Changes".
    wait_until(cx, "the session on the row", |cx| {
        rows(&panel, cx)[0].session == Some(started.session.id)
    });
    click(&view, cx, Region::TaskRow(0));
    click(&view, cx, Region::TaskAction(0, "changes"));
    wait_until(cx, "the Changes tab of the task", |cx| opened(&view, cx));
    // Close it again to try the next way.
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            if let Some((pane, index)) = s.find_item(&item) {
                s.close_item(pane, index).unwrap();
            }
        });
    });
    assert!(!opened(&view, cx));

    // 2. ⌘K "Worktree: Review Changes…": a picker of the checkouts.
    cx.dispatch_action(actions::ReviewChanges);
    wait_until(cx, "the picker rows", |cx| {
        view.read_with(cx, |wb, _| wb.picker_titles().len() == 2)
    });
    let titles: Vec<String> = view.read_with(cx, |wb, _| {
        wb.picker_titles().iter().map(ToString::to_string).collect()
    });
    assert_eq!(titles, ["main", "task/fix-login"]);
    view.update_in(cx, |wb, window, cx| wb.picker_confirm(Some(1), window, cx));
    wait_until(cx, "the Changes tab from the picker", |cx| {
        opened(&view, cx)
    });
    view.update(cx, |wb, cx| {
        wb.edit_layout(cx, |s| {
            if let Some((pane, index)) = s.find_item(&item) {
                s.close_item(pane, index).unwrap();
            }
        });
    });

    // 3. The placement badge of the session's row.
    cx.dispatch_action(actions::OpenPanel("sessions".into()));
    let badge =
        Region::PlacementBadge(hutan_desktop::workbench::BadgeSite::Row, started.session.id);
    wait_until(cx, "the badge", |cx| region(&view, cx, badge).is_some());
    // The badge is wider than the dock; the sidebar list clips to its own width, so click the
    // part that shows.
    let bounds = region(&view, cx, badge).unwrap();
    cx.simulate_click(
        gpui::point(bounds.origin.x + gpui::px(8.0), bounds.center().y),
        Modifiers::none(),
    );
    draw(cx);
    let entries: Vec<String> = view
        .read_with(cx, |wb, _| wb.context_menu_entries())
        .expect("the badge opens a menu")
        .into_iter()
        .map(|(label, _)| label.to_string())
        .collect();
    assert_eq!(entries, ["Review Changes…", "Copy Checkout Path"]);
    view.update_in(cx, |wb, window, cx| wb.run_menu_entry(0, window, cx));
    wait_until(cx, "the Changes tab from the badge", |cx| opened(&view, cx));
}

#[gpui::test]
fn the_panel_requires_a_daemon_that_knows_tasks_and_a_connection(cx: &mut TestAppContext) {
    // Offline: the panel says so and the new-task button is inert.
    let (connection, settings, state) = common::offline_connection();
    let (view, cx) = open_with(cx, settings, connection, Some(state));
    cx.dispatch_action(actions::OpenPanel("tasks".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .unwrap();
    assert!(panel.read_with(cx, |p, cx| p.rows(0, cx).is_empty()));
    click_no_effect(&view, cx);
    assert!(view.read_with(cx, |wb, _| wb.launcher().is_none()));
}

fn click_no_effect(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the new button", |cx| {
        region(view, cx, Region::Tasks("new")).is_some()
    });
    let bounds = region(view, cx, Region::Tasks("new")).unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

// Delivery (DL-1) ------------------------------------------------------------------------------

/// A worktree task on `feature/x` whose run (`exit 0`) has finished: `done` and awaiting review,
/// with its worktree holding a commit (`a.txt`, `b.txt`) and an untracked `untracked.txt`.
fn awaiting_task(env: &Env, title: &str) -> (Task, PathBuf) {
    let task = env.create(title, &[], |request| {
        request.placement = Some(TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some("feature/x".into()),
            base: None,
        });
        request.run = Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "exit 0".into()],
        });
    });
    let _: TaskStarted = env.daemon.call(Request::TaskStart(TaskRef { id: task.id }));
    let deadline = Instant::now() + Duration::from_secs(20);
    let done = loop {
        let now = env.task(task.id).expect("the task exists");
        if now.state == TaskState::Done && now.delivery == DeliveryState::AwaitingReview {
            break now;
        }
        assert!(
            Instant::now() < deadline,
            "the task did not finish: {:?} {:?}",
            now.state,
            now.delivery
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let list: WorktreeList = env.daemon.call(Request::WorktreeList(WorktreeListRequest {
        workspace_id: Some(env.workspace),
    }));
    let wt = list
        .worktrees
        .into_iter()
        .find(|w| w.branch.as_deref() == Some("feature/x"))
        .expect("the task's worktree exists")
        .path;
    fs::write(wt.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    commit_all(&wt, "feature work");
    fs::write(wt.join("untracked.txt"), "u1\nu2\n").unwrap();
    (done, wt)
}

fn review(env: &Env, task: &Task, decision: ReviewDecision, note: Option<&str>) -> Task {
    let reviewed: TaskReviewed = env.daemon.call(Request::TaskReview(TaskReview {
        id: task.id,
        decision,
        note: note.map(str::to_owned),
    }));
    reviewed.task
}

#[gpui::test]
fn a_finished_task_shows_its_delivery_pill_and_needs_you_without_a_badge(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, _wt) = awaiting_task(&env, "Fix login");
    let session = *task.sessions.last().expect("the run has a session");
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    wait_rows(&panel, cx, "the task", 1);
    wait_until(cx, "the delivery pill", |cx| {
        region(&view, cx, Region::TaskDelivery(0)).is_some()
    });
    let row = rows(&panel, cx).remove(0);
    assert_eq!(
        (row.delivery, row.delivery_label),
        (DeliveryState::AwaitingReview, Some("Awaiting review"))
    );

    // The session ended before the window came; it needs the user, softly: the sidebar's
    // Needs you lists it, the menu bar and the Dock do not count it.
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    wait_until(cx, "the session", |cx| {
        sessions.read_with(cx, |s, _| s.session(session).is_some())
    });
    sessions.read_with(cx, |s, _| {
        let presentation = s.presentation(s.session(session).unwrap());
        assert_eq!(
            (presentation.label.as_str(), presentation.needs_you),
            ("review", true)
        );
        assert_eq!(presentation.tone, Tone::Accent);
        assert_eq!(s.needs_you(), [session]);
        assert!(s.attention_entries().is_empty(), "a soft item has no badge");
    });
    // Looking is not accepting, but it is seeing: read, it leaves Needs you and says its own
    // state again, with the *Changes to review* link beside it; the pill stays.
    sessions.update(cx, |s, cx| s.mark_read(session, cx));
    sessions.read_with(cx, |s, _| {
        let presentation = s.presentation(s.session(session).unwrap());
        assert_ne!(presentation.label, "review", "seen review is no state");
        assert!(!presentation.needs_you);
        assert!(s.changes_to_review(session));
        assert!(s.needs_you().is_empty());
    });
    assert_eq!(
        rows(&panel, cx)[0].delivery_label,
        Some("Awaiting review"),
        "seen work still awaits its review"
    );

    // A task that waits for review stays listed when finished tasks are hidden …
    settings(&view, cx)
        .write_user("tasks.show_done", toml::Value::Boolean(false))
        .unwrap();
    wait_until(cx, "the setting", |cx| {
        panel.read_with(cx, |p, cx| p.rows(0, cx).len() == 1)
    });
    draw(cx);
    assert_eq!(rows(&panel, cx).len(), 1);

    // … until it is accepted: the pill changes, the Needs-you item and the row go.
    review(&env, &task, ReviewDecision::Accept, None);
    wait_until(cx, "the accepted pill", |cx| {
        rows(&panel, cx).first().map(|r| r.delivery_label) == Some(Some("Accepted"))
            || rows(&panel, cx).is_empty()
    });
    wait_until(cx, "the Needs-you item to go", |cx| {
        sessions.read_with(cx, |s, _| s.needs_you().is_empty())
    });
    wait_until(cx, "the finished task to be hidden", |cx| {
        rows(&panel, cx).is_empty()
    });
}

#[gpui::test]
fn the_header_accepts_the_work_of_the_task_on_the_branch(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::AwaitingReview)
        })
    });
    for name in ["delivery", "accept", "request-changes", "finish"] {
        wait_until(cx, name, |cx| {
            region(&view, cx, Region::Changes(name)).is_some()
        });
    }
    assert!(changes.read_with(cx, |c, cx| c.can_review(cx)));

    click(&view, cx, Region::Changes("accept"));
    wait_until(cx, "the daemon to accept", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::Accepted)
    });
    assert_eq!(
        env.task(task.id)
            .unwrap()
            .delivery_history
            .last()
            .unwrap()
            .by,
        DeliveryActor::Desktop
    );
    wait_until(cx, "the view to follow", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::Accepted)
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n == "Accepted “Fix login”")
    );
}

#[gpui::test]
fn the_changes_toolbar_wraps_so_every_action_stays_inside_a_narrow_window(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let _ = task;
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::AwaitingReview)
        })
    });
    let names = ["accept", "request-changes", "toggle", "refresh", "finish"];
    let inside = |view: &Entity<Workbench>, cx: &mut VisualTestContext, width: f32, what: &str| {
        draw(cx);
        for name in names {
            wait_until(cx, name, |cx| {
                region(view, cx, Region::Changes(name)).is_some()
            });
        }
        let pane = view
            .read_with(cx, |wb, _| wb.region_bounds(Region::Center))
            .expect("the center is drawn");
        for name in names {
            let bounds = view
                .read_with(cx, |wb, _| wb.region_bounds(Region::Changes(name)))
                .unwrap();
            assert!(
                bounds.right() <= gpui::px(width) && bounds.right() <= pane.right(),
                "{what}: {name} runs out of the pane: {bounds:?} in {pane:?}"
            );
            assert!(
                bounds.left() >= pane.left(),
                "{what}: {name} starts before the pane"
            );
        }
    };
    // The reported case: both docks visible at the default width.
    cx.simulate_resize(gpui::size(gpui::px(1280.0), gpui::px(800.0)));
    inside(&view, cx, 1280.0, "1280 px, docks visible");
    // A narrow window: 640 px with the docks hidden still shows Finish and the rest.
    cx.dispatch_action(actions::ToggleLeftDock);
    cx.dispatch_action(actions::ToggleRightDock);
    cx.simulate_resize(gpui::size(gpui::px(640.0), gpui::px(800.0)));
    inside(&view, cx, 640.0, "640 px");
    // Finish is a click away and still opens the form.
    click(&view, cx, Region::Changes("finish"));
    wait_until(cx, "the finish form", |cx| {
        region(&view, cx, Region::Changes("finish-confirm")).is_some()
    });
}

#[gpui::test]
fn a_narrow_changes_view_folds_the_files_into_a_strip_and_keeps_the_diff_and_commit_all(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (_task, wt) = awaiting_task(&env, "Fix login");
    // An uncommitted edit with a line far wider than any pane.
    let long = "x".repeat(400);
    fs::write(wt.join("a.txt"), format!("one\n{long}\nthree\nfour\n")).unwrap();
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the scope", |cx| {
        changes.read_with(cx, |c, _| c.has_uncommitted() && c.selected().is_some())
    });
    let bounds = |view: &Entity<Workbench>, cx: &VisualTestContext, what: Region| {
        view.read_with(cx, |wb, _| wb.region_bounds(what))
    };

    // 900 px with both docks visible: the reported case.
    cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(800.0)));
    for what in [Region::Changes("diff"), Region::Changes("commit-all")] {
        wait_until(cx, "a narrow layout", |cx| {
            region(&view, cx, what).is_some()
        });
    }
    draw(cx);
    let pane = bounds(&view, cx, Region::Center).expect("the center is drawn");
    let root = bounds(&view, cx, Region::Changes("root")).expect("the view is drawn");
    assert!(root.size.width < gpui::px(720.0), "{root:?} is narrow");
    // The file list is a strip: no rows until it is unfolded.
    wait_until(cx, "the strip", |cx| {
        bounds(&view, cx, Region::Changes("files-toggle")).is_some()
    });
    assert!(bounds(&view, cx, Region::ChangesFile(0)).is_none());
    // The diff has the whole width, with its text, and Commit all… is inside the pane.
    let diff = bounds(&view, cx, Region::Changes("diff")).unwrap();
    assert!(
        diff.size.width >= root.size.width - gpui::px(2.0),
        "the diff has the full width: {diff:?} in {root:?}"
    );
    wait_until(cx, "the first hunk", |cx| {
        bounds(&view, cx, Region::ChangesHunk(0)).is_some()
    });
    let hunk = bounds(&view, cx, Region::ChangesHunk(0)).unwrap();
    assert!(
        hunk.size.width > gpui::px(200.0) && hunk.right() <= pane.right(),
        "diff text is readable: {hunk:?}"
    );
    let commit = bounds(&view, cx, Region::Changes("commit-all")).unwrap();
    assert!(
        commit.left() >= pane.left() && commit.right() <= pane.right(),
        "Commit all… stays visible: {commit:?} in {pane:?}"
    );
    // Long lines scroll sideways instead of widening or clipping the pane.
    assert!(
        changes.read_with(cx, |c, _| c.diff_scroll_width()) > 100.0,
        "the 400-column line can be scrolled to"
    );

    // The strip unfolds into the list; choosing a file shows it and folds the list again.
    click(&view, cx, Region::Changes("files-toggle"));
    wait_until(cx, "the list", |cx| {
        bounds(&view, cx, Region::ChangesFile(1)).is_some()
    });
    assert!(changes.read_with(cx, |c, _| c.files_open()));
    let second = changes.read_with(cx, |c, _| c.files()[1].path.clone());
    click(&view, cx, Region::ChangesFile(1));
    assert_eq!(
        changes.read_with(cx, |c, _| c.selected().map(str::to_owned)),
        Some(second)
    );
    assert!(!changes.read_with(cx, |c, _| c.files_open()));

    // Wide again: the list sits beside the diff and there is no strip.
    cx.simulate_resize(gpui::size(gpui::px(1800.0), gpui::px(800.0)));
    wait_until(cx, "the wide layout", |cx| {
        bounds(&view, cx, Region::ChangesFile(0)).is_some()
            && bounds(&view, cx, Region::Changes("files-toggle")).is_none()
    });
}

#[gpui::test]
fn requesting_changes_sends_the_note_and_cancel_leaves_the_task_alone(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let panel = show_panel(&view, cx);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| c.can_review(cx))
    });

    click(&view, cx, Region::Changes("request-changes"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form()),
        Some(DeliveryForm::RequestChanges)
    );
    wait_until(cx, "the note field", |cx| {
        region(&view, cx, Region::Changes("request-note")).is_some()
    });
    click(&view, cx, Region::Changes("request-cancel"));
    assert_eq!(changes.read_with(cx, |c, _| c.delivery_form()), None);
    assert_eq!(
        env.task(task.id).unwrap().delivery,
        DeliveryState::AwaitingReview,
        "cancelling asks for nothing"
    );

    click(&view, cx, Region::Changes("request-changes"));
    changes.update(cx, |c, cx| {
        c.set_delivery_text("Handle the empty password", cx)
    });
    click(&view, cx, Region::Changes("request-send"));
    wait_until(cx, "the daemon to record the request", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::ChangesRequested)
    });
    let asked = env.task(task.id).unwrap();
    assert_eq!(asked.change_request(), Some("Handle the empty password"));
    assert_eq!(
        asked.delivery_history.last().unwrap().by,
        DeliveryActor::Desktop
    );
    // A terminal run cannot be written to: the note stays on the task, and the task says so.
    wait_until(cx, "the form to close", |cx| {
        changes.read_with(cx, |c, _| c.delivery_form().is_none())
    });
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.contains("the note stays on the task"))
    );
    // The Tasks panel row shows the pill and the note.
    draw(cx);
    wait_until(cx, "the Changes requested pill", |cx| {
        rows(&panel, cx).first().map(|r| r.delivery_label) == Some(Some("Changes requested"))
    });
    assert_eq!(
        rows(&panel, cx)[0].change_request.as_deref(),
        Some("Handle the empty password")
    );
}

#[gpui::test]
fn the_scope_says_what_a_merge_takes_and_commit_all_brings_the_rest(cx: &mut TestAppContext) {
    let env = Env::new();
    let (_task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the scope", |cx| {
        changes.read_with(cx, |c, _| {
            !c.commits().is_empty() && !c.uncommitted_files().is_empty()
        })
    });
    let paths = |files: Vec<&hutan_protocol::worktree::DiffFile>| -> Vec<String> {
        files.into_iter().map(|f| f.path.clone()).collect()
    };
    let (commits, committed, uncommitted) = changes.read_with(cx, |c, _| {
        (
            c.commits()
                .iter()
                .map(|commit| commit.subject.clone())
                .collect::<Vec<_>>(),
            paths(c.committed_files()),
            paths(c.uncommitted_files()),
        )
    });
    assert_eq!(commits, ["feature work"]);
    assert_eq!(committed, ["a.txt", "b.txt"]);
    assert_eq!(uncommitted, ["untracked.txt"]);
    for name in ["scope-committed", "scope-uncommitted", "commit-all"] {
        wait_until(cx, name, |cx| {
            region(&view, cx, Region::Changes(name)).is_some()
        });
    }

    // Leaving the uncommitted changes out of the list does not hide that they exist.
    click(&view, cx, Region::Changes("toggle"));
    wait_until(cx, "the committed-only list", |cx| {
        changes.read_with(cx, |c, _| {
            c.uncommitted_files().is_empty() && c.files().len() == 2
        })
    });
    assert!(changes.read_with(cx, |c, _| c.has_uncommitted()));
    wait_until(cx, "the uncommitted section", |cx| {
        region(&view, cx, Region::Changes("scope-uncommitted")).is_some()
    });
    click(&view, cx, Region::Changes("toggle"));
    wait_until(cx, "the full list", |cx| {
        changes.read_with(cx, |c, _| c.files().len() == 3)
    });

    // The Finish form lists exactly the commits the merge integrates and says the rest is
    // committed first.
    click(&view, cx, Region::Changes("finish"));
    wait_until(cx, "the merge preview", |cx| {
        region(&view, cx, Region::Changes("finish-commits")).is_some()
    });
    click(&view, cx, Region::Changes("finish-cancel"));

    // Commit all…: a message is required; then the worktree is clean and the commit is new.
    click(&view, cx, Region::Changes("commit-all"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form()),
        Some(DeliveryForm::Commit)
    );
    click(&view, cx, Region::Changes("commit-confirm"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.delivery_form_error().map(str::to_owned)),
        Some("A commit needs a message.".to_owned())
    );
    changes.update(cx, |c, cx| c.set_delivery_text("Add the scratch file", cx));
    click(&view, cx, Region::Changes("commit-confirm"));
    wait_until(cx, "the commit to show in the scope", |cx| {
        changes.read_with(cx, |c, _| {
            c.commits().len() == 2
                && c.uncommitted_files().is_empty()
                && c.delivery_form().is_none()
        })
    });
    assert_eq!(
        git_out(&wt, &["log", "-1", "--format=%s"]).trim(),
        "Add the scratch file"
    );
    assert!(!changes.read_with(cx, |c, _| c.has_uncommitted()));
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(ToString::to_string))
            .is_some_and(|n| n.starts_with("Committed 1 file on feature/x ("))
    );
    draw(cx);
    assert!(region(&view, cx, Region::Changes("scope-uncommitted")).is_none());
}

#[gpui::test]
fn finishing_takes_the_task_title_commits_the_rest_and_marks_the_task_merged(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    wait_until(cx, "the task of the view", |cx| {
        changes.read_with(cx, |c, cx| c.can_review(cx))
    });
    finish_locally(&view, cx, &changes);
    // The message starts as the task's title, and stays editable.
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_message()),
        Some("Fix login".to_owned())
    );
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    let merged = changes.read_with(cx, |c, _| c.merged().cloned().unwrap());
    assert!(
        !merged.source_dirty,
        "untracked.txt was committed with the task's title before the merge"
    );
    assert_eq!(
        git_out(&wt, &["log", "-1", "--format=%s"]).trim(),
        "Fix login"
    );

    // The task is merged, with the commit the merge made.
    wait_until(cx, "the task to be merged", |_| {
        env.task(task.id)
            .is_some_and(|t| t.delivery == DeliveryState::Merged)
    });
    let record = env
        .task(task.id)
        .unwrap()
        .delivery_history
        .last()
        .cloned()
        .unwrap();
    assert_eq!(record.commit.as_deref(), Some(merged.commit.as_str()));
    assert_eq!(record.strategy.as_deref(), Some("merge"));
    wait_until(cx, "the view to follow", |cx| {
        changes.read_with(cx, |c, cx| {
            c.delivery_state(cx) == Some(DeliveryState::Merged) && !c.can_review(cx)
        })
    });

    // Nothing is left in the worktree: removing it is the default.
    assert_eq!(
        changes.read_with(cx, |c, _| c.cleanup_default()),
        Some(CleanupDefault::Remove)
    );
    assert!(env.root.join("untracked.txt").exists());
    draw(cx);
    assert!(region(&view, cx, Region::Changes("remove")).is_some());
    click(&view, cx, Region::Changes("keep"));
    assert!(changes.read_with(cx, |c, _| c.merged().is_none()));
}

#[gpui::test]
fn after_commit_all_nothing_is_left_and_removal_is_the_default(cx: &mut TestAppContext) {
    let env = Env::new();
    let (task, wt) = awaiting_task(&env, "Fix login");
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &wt);
    click(&view, cx, Region::Changes("commit-all"));
    changes.update(cx, |c, cx| c.set_delivery_text("Add the scratch file", cx));
    click(&view, cx, Region::Changes("commit-confirm"));
    wait_until(cx, "the commit", |cx| {
        changes.read_with(cx, |c, _| c.commits().len() == 2 && !c.has_uncommitted())
    });
    finish_locally(&view, cx, &changes);
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the merge result", |cx| {
        changes.read_with(cx, |c, _| c.merged().is_some())
    });
    assert_eq!(
        changes.read_with(cx, |c, _| c.cleanup_default()),
        Some(CleanupDefault::Remove)
    );
    // The merge took the committed scratch file too.
    assert!(env.root.join("untracked.txt").exists());
    click(&view, cx, Region::Changes("remove"));
    wait_until(cx, "the worktree to go", |_| !wt.exists());
    // Removing a merged worktree discards nothing: the task stays merged.
    assert_eq!(env.task(task.id).unwrap().delivery, DeliveryState::Merged);
}

// Finish and push -------------------------------------------------------------------------------

/// `env.root`'s `main` tracking `main` of a bare `origin` next to it; the path of `origin`.
fn with_origin(env: &Env) -> PathBuf {
    let bare = env.root.parent().unwrap().join("origin.git");
    git(
        env.root.parent().unwrap(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        &env.root,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(&env.root, &["push", "-q", "-u", "origin", "main"]);
    bare
}

/// The daemon pushes in its own environment, not a login shell's: the tests do not depend on
/// the account's shell.
const DAEMON_ENV: &str = "[terminal]\nenv_inherit = \"daemon\"\n";

#[gpui::test]
fn finishing_the_main_checkout_commits_and_pushes_to_its_upstream(cx: &mut TestAppContext) {
    let env = Env::with_config(DAEMON_ENV);
    let bare = with_origin(&env);
    fs::write(env.root.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &env.root);

    click(&view, cx, Region::Changes("finish"));
    // Push starts as `[finish] push` (on by default).
    assert_eq!(changes.read_with(cx, |c, _| c.finish_push()), Some(true));
    changes.update(cx, |c, cx| c.set_finish_message("Add four", cx));
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the steps", |cx| {
        changes.read_with(cx, |c, _| c.finished().is_some())
    });
    let done = changes.read_with(cx, |c, _| c.finished().cloned().unwrap());
    let lines = hutan_desktop::workbench::finish_step_lines(&done);
    assert!(
        lines[0].1.starts_with("Committed 1 file on main ("),
        "{lines:?}"
    );
    assert!(
        lines[1].1.starts_with("Pushed main to origin/main ("),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .all(|(tone, _)| *tone == hutan_desktop::workbench::StepTone::Done)
    );
    assert!(done.merge.is_none());
    assert_eq!(
        git_out(&bare, &["log", "-1", "--format=%s", "main"]).trim(),
        "Add four"
    );
    assert_eq!(
        git_out(&bare, &["rev-parse", "main"]),
        git_out(&env.root, &["rev-parse", "HEAD"])
    );
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string()))
            .is_some_and(|n| n.starts_with("Pushed main to origin/main (")),
    );
    wait_until(cx, "the result card", |cx| {
        region(&view, cx, Region::Changes("finish-result")).is_some()
    });
}

#[gpui::test]
fn a_finish_whose_push_fails_says_why_and_keeps_the_commit(cx: &mut TestAppContext) {
    // `main` has no upstream: the commit is made, the push says so.
    let env = Env::with_config(DAEMON_ENV);
    fs::write(env.root.join("a.txt"), "local\n").unwrap();
    let (view, cx) = open(cx, &env);
    let changes = open_changes(&view, cx, &env.root);
    click(&view, cx, Region::Changes("finish"));
    changes.update(cx, |c, cx| c.set_finish_message("Local work", cx));
    click(&view, cx, Region::Changes("finish-confirm"));
    wait_until(cx, "the steps", |cx| {
        changes.read_with(cx, |c, _| c.finished().is_some())
    });
    let done = changes.read_with(cx, |c, _| c.finished().cloned().unwrap());
    let lines = hutan_desktop::workbench::finish_step_lines(&done);
    assert_eq!(lines[1].0, hutan_desktop::workbench::StepTone::Failed);
    assert!(
        lines[1].1.starts_with("Not pushed — no upstream:") && lines[1].1.contains("git push -u"),
        "{lines:?}"
    );
    assert_eq!(
        git_out(&env.root, &["log", "-1", "--format=%s"]).trim(),
        "Local work"
    );
    // The failure is what the toast says.
    assert!(
        view.read_with(cx, |wb, _| wb.notice().map(|n| n.to_string()))
            .is_some_and(|n| n.starts_with("Not pushed — no upstream")),
    );
}

// Review that clears (FX-REVIEW) ---------------------------------------------------------------

/// `plain-turn` with its turn repeated `turns` times, each held after `turn/started` until the
/// file `gate-<n>` exists in `dir` (the test edits the checkout meanwhile, as the agent would).
fn held_turns(dir: &Path, turns: usize) -> PathBuf {
    let real = fs::read_to_string(common::chat::fixtures().join("plain-turn.jsonl")).unwrap();
    let lines: Vec<&str> = real.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("\"method\":\"turn/start\""))
        .unwrap();
    let mut out: Vec<String> = lines[..at].iter().map(|l| (*l).to_owned()).collect();
    for n in 0..turns {
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
                let gate = dir.join(format!("gate-{n}"));
                out.push(serde_json::json!({"dir": "hold", "file": gate}).to_string());
            }
        }
    }
    let path = dir.join("held-turns.jsonl");
    fs::write(&path, out.join("\n") + "\n").unwrap();
    path
}

#[gpui::test]
fn review_follows_the_turns_the_user_sees_and_a_finish_closes_it(cx: &mut TestAppContext) {
    use hutan_desktop::sessions::{Group, agents_text};
    use hutan_protocol::chat::{ChatSend, ChatSent};

    let chat = common::chat::ChatEnv::new();
    let transcript = held_turns(chat.work.path(), 3);
    let env = Env::with_daemon(TestDaemon::start_with_codex(chat.replay(&transcript)));
    let release = |n: usize| fs::write(chat.work.path().join(format!("gate-{n}")), "").unwrap();
    let start_chat = |title: &str| {
        let task = env.create(title, &[], |request| {
            request.prompt = Some(format!("{title}."));
            request.run = Some(TaskRun::Chat {
                agent: "codex".into(),
                model: None,
            });
        });
        let started: TaskStarted = env.daemon.call(Request::TaskStart(TaskRef { id: task.id }));
        (task, started.session.id)
    };
    let done = |id: TaskId| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while env.task(id).is_none_or(|t| t.state != TaskState::Done) {
            assert!(Instant::now() < deadline, "the chat task never finished");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let send = |session: SessionId, text: &str| {
        let _: ChatSent = env.daemon.call(Request::ChatSend(ChatSend {
            session_id: session,
            text: text.to_owned(),
            attachments: Vec::new(),
        }));
    };
    // A chat that only looks around, first in the list; then the one that edits.
    let (look, _) = start_chat("Look around the project");
    release(0);
    done(look.id);
    // Each chat replays its transcript from the start: this first turn is not held any more.
    let (edit, session) = start_chat("Change the greeting");
    done(edit.id);
    assert_eq!(env.task(look.id).unwrap().delivery, DeliveryState::None);
    let (view, cx) = open(cx, &env);
    let sessions = view.read_with(cx, |wb, _| wb.sessions().clone());
    let label = |cx: &VisualTestContext| {
        sessions.read_with(cx, |s, _| {
            s.session(session)
                .map(|info| s.presentation(info).label)
                .unwrap_or_default()
        })
    };
    let group = |cx: &VisualTestContext| {
        sessions.read_with(cx, |s, _| {
            s.agent_presentation().get(session).map(|r| r.group)
        })
    };

    // The user's next message changes a file: the work needs the user, here and in Today alike.
    send(session, "Change the greeting.");
    fs::write(env.root.join("a.txt"), "hello\n").unwrap();
    release(1);
    wait_until(cx, "the change to await review", |cx| label(cx) == "review");
    sessions.read_with(cx, |s, _| {
        assert_eq!(s.needs_you(), [session]);
        let page: Vec<SessionId> = s
            .agent_presentation()
            .records
            .iter()
            .filter(|r| r.group == Group::NeedsYou)
            .map(|r| r.session)
            .collect();
        let today: Vec<SessionId> = hutan_desktop::workbench::panels::today(s)
            .needs_you
            .iter()
            .map(|row| row.session)
            .collect();
        assert_eq!(page, [session]);
        assert_eq!(today, page, "Today lists what the Agents page lists");
    });

    // A second turn runs: the session is working, not under review.
    send(session, "Simplify total().");
    wait_until(cx, "the second turn to work", |cx| label(cx) == "working");
    assert_eq!(group(cx), Some(Group::Working));
    assert!(sessions.read_with(cx, |s, _| s.needs_you().is_empty()));
    fs::write(env.root.join("b.txt"), "total\n").unwrap();
    release(2);
    wait_until(cx, "the second change to await review", |cx| {
        label(cx) == "review"
    });
    sessions.read_with(cx, |s, _| {
        let record = s.agent_presentation().get(session).cloned().unwrap();
        assert_eq!(agents_text::state_word(&record), "Needs you · review");
        assert!(
            !agents_text::changes_link(&record),
            "unseen: no link, a reason"
        );
    });

    // The user looks at the chat: it leaves Needs you, the status bar's count drops, the row
    // says Idle — one word — with the *Changes to review* link beside it.
    cx.dispatch_action(actions::OpenSession(session.to_string()));
    wait_until(cx, "the chat to be seen", |cx| {
        sessions.read_with(cx, |s, _| s.changes_to_review(session))
    });
    assert_eq!(label(cx), "idle");
    sessions.read_with(cx, |s, _| {
        assert!(s.needs_you().is_empty(), "the status bar counts nothing");
        let record = s.agent_presentation().get(session).cloned().unwrap();
        assert_eq!(agents_text::state_word(&record), "Idle");
        assert_eq!(record.group, Group::Idle);
        assert!(agents_text::changes_link(&record));
    });
    // The Agents page draws the link beside the state word, and it opens the changes.
    cx.dispatch_action(actions::ShowAgentsPage);
    click(&view, cx, Region::AgentAction(session, "changes-to-review"));
    wait_until(cx, "the Changes view of the checkout", |cx| {
        view.read_with(cx, |wb, _| {
            wb.changes(&env.root.display().to_string()).is_some()
        })
    });

    // Finish: the message is the chat that edited files; the work is merged and nothing waits.
    let changes = open_changes(&view, cx, &env.root);
    click(&view, cx, Region::Changes("finish"));
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_message()),
        Some("Change the greeting".to_owned())
    );
    assert_eq!(
        changes.read_with(cx, |c, _| c.finish_hint().map(str::to_owned)),
        None
    );
    changes.update(cx, |c, cx| {
        c.set_finish_push(false, cx);
        c.confirm_finish(cx);
    });
    wait_until(cx, "the work to be merged", |_| {
        env.task(edit.id)
            .is_some_and(|t| t.delivery == DeliveryState::Merged)
    });
    wait_until(cx, "the review to go", |cx| {
        sessions.read_with(cx, |s, _| !s.changes_to_review(session))
    });
    assert!(sessions.read_with(cx, |s, _| s.needs_you().is_empty()));
    assert_eq!(env.task(look.id).unwrap().delivery, DeliveryState::None);
}
