#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! The Start… sheet (S-1) against an in-process daemon: every entry point opens it with the right
//! project, a chat in a new worktree becomes a `task.create` with that placement and is started,
//! "Queue as task" does not start, "Start & new" keeps the sheet and its place, harnesses that
//! cannot run offer Install… / Log in…, the shared-checkout warning, ⌘⇧T in both keymap bases,
//! Tab order.
//! OMP is `hutan-acp-fixture replay`; the harnesses' own CLIs are shell scripts. No real
//! harness, login or model is touched.

use crate::common;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::{TestDaemon, draw, open_options, open_with, wait_until};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use hutan_core::{Task, TaskPlacementKind, TaskRun, TaskState, WorkspaceId};
use hutan_desktop::harness_install::FakeRunner;
use hutan_desktop::sessions::launch::{LaunchKind, shell_argv, user_shell};
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::model::ItemRef;
use hutan_desktop::workbench::panels::TasksPanel;
use hutan_desktop::workbench::{
    LaunchAgent, LaunchWhere, LauncherField, LauncherPreset, LauncherStop, Models, Readiness,
    Region, Workbench, WorkbenchOptions, WorktreeList,
};
use hutan_protocol::messages::Request;
use hutan_protocol::task::{TaskCreate, TaskList, TaskListRequest, TaskResult};
use hutan_protocol::worktree::{WorktreeList as DaemonWorktrees, WorktreeListRequest};
use hutan_settings::{SettingsPaths, SettingsStore};

// Fixtures ------------------------------------------------------------------------------------

fn acp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("hutan-acp-fixture");
    assert!(bin.is_file(), "run `cargo build -p hutan-agents --bins`");
    bin
}

fn transcript(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11")
        .join(format!("{name}.jsonl"))
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A daemon whose OMP replays `plain-turn` and is logged in to `anthropic`; Claude Code's CLI
/// reports a logged-out account unless `claude_cli` names a program that does not exist.
fn daemon(scripts: &Path, claude_cli: Option<&Path>) -> TestDaemon {
    daemon_with(scripts, claude_cli, None)
}

/// [`daemon`] whose OMP offers no models over ACP, so the catalog is `omp models`, which
/// lists `models` (a comma-separated list of JSON objects).
fn daemon_with_models(scripts: &Path, claude_cli: Option<&Path>, models: &str) -> TestDaemon {
    daemon_with(scripts, claude_cli, Some(models))
}

fn daemon_with(scripts: &Path, claude_cli: Option<&Path>, models: Option<&str>) -> TestDaemon {
    let custom = models.is_some();
    let models = models.unwrap_or(
        r#"{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}"#,
    );
    let omp = script(
        scripts,
        "omp",
        &format!(
            r#"case "$1" in
  models) echo '{{"models":[{models}]}}' ;;
  usage) echo '{{"reports":[]}}' ;;
  *) echo "fake omp $*" ;;
esac"#
        ),
    );
    let claude = claude_cli.map(Path::to_path_buf).unwrap_or_else(|| {
        script(
            scripts,
            "claude",
            r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":false,"authMethod":"none"}'; else echo "fake claude $*"; fi"#,
        )
    });
    let (acp, args) = if custom {
        (script(scripts, "omp-acp", "exit 1"), String::new())
    } else {
        (
            acp_bin(),
            format!(
                "args = [\"replay\", {:?}, \"acp\"]\n",
                transcript("plain-turn")
            ),
        )
    };
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\n{args}cli = {omp:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {claude:?}\n",
        missing = scripts.join("no-such-claude-code-acp"),
    ))
}

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

/// `<tmp>/<name>` on `main` with one commit and an identity the daemon's git calls find.
fn repo(dir: &Path, name: &str) -> PathBuf {
    let root = fs::canonicalize(dir).unwrap().join(name);
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
    fs::write(root.join("a.txt"), "one\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    root
}

struct Env {
    _scripts: tempfile::TempDir,
    folders: tempfile::TempDir,
    daemon: TestDaemon,
    api: PathBuf,
    api_id: WorkspaceId,
    web_id: WorkspaceId,
}

impl Env {
    fn new() -> Self {
        Self::with(false)
    }

    /// With `claude_missing`, Claude Code's CLI is not on the machine.
    fn with(claude_missing: bool) -> Self {
        Self::build(claude_missing, None)
    }

    /// OMP offers `models` (see [`daemon_with_models`]).
    fn with_models(models: &str) -> Self {
        Self::build(false, Some(models))
    }

    fn build(claude_missing: bool, models: Option<&str>) -> Self {
        let scripts = tempfile::tempdir().unwrap();
        let folders = tempfile::tempdir().unwrap();
        let missing = scripts.path().join("no-such-claude");
        let claude = claude_missing.then_some(missing.as_path());
        let daemon = match models {
            Some(models) => daemon_with_models(scripts.path(), claude, models),
            None => daemon(scripts.path(), claude),
        };
        let api = repo(folders.path(), "api");
        let web = repo(folders.path(), "web");
        let api_id = daemon.create_workspace("api", &[&api]).id;
        let web_id = daemon.create_workspace("web", &[&web]).id;
        Self {
            _scripts: scripts,
            folders,
            daemon,
            api,
            api_id,
            web_id,
        }
    }

    fn tasks(&self) -> Vec<Task> {
        self.daemon
            .call::<TaskList>(Request::TaskList(TaskListRequest::default()))
            .tasks
    }

    fn task(&self, title: &str) -> Option<Task> {
        self.tasks().into_iter().find(|t| t.title == title)
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    env: &Env,
) -> (Entity<Workbench>, &'a mut VisualTestContext) {
    let (view, cx) = open_with(cx, settings(env), env.daemon.connection(), None);
    synced(&view, cx);
    (view, cx)
}

/// Every project shows in the sidebar, sessions or not.
fn settings(env: &Env) -> SettingsStore {
    let settings = SettingsStore::new(SettingsPaths::in_dir(env.daemon.home()));
    settings
        .write_user("workspaces.hide_empty", toml::Value::Boolean(false))
        .unwrap();
    settings
}

fn synced(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced()
                && wb.sessions().read(cx).workspaces().len() >= 3
                && wb.active_workspace().is_some()
        })
    });
}

fn region(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn bounds_of(
    view: &Entity<Workbench>,
    cx: &VisualTestContext,
    target: Region,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    view.read_with(cx, |wb, _| wb.region_bounds(target))
}

fn click(view: &Entity<Workbench>, cx: &mut VisualTestContext, target: Region) {
    wait_until(cx, &format!("{target:?} to be drawn"), |cx| {
        region(view, cx, target)
    });
    let bounds = view
        .read_with(cx, |wb, _| wb.region_bounds(target))
        .unwrap();
    cx.simulate_click(bounds.center(), Modifiers::none());
}

fn launcher_open(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| wb.launcher().is_some())
}

fn launcher_project(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<WorkspaceId> {
    view.read_with(cx, |wb, _| wb.launcher().and_then(|l| l.project()))
}

/// Waits until the sheet knows the worktrees of its project and OMP can be started. The sheet
/// opens on a plain terminal, for which every harness counts as ready, so the account listing
/// a chat needs is waited for too.
fn ready(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    wait_until(cx, "the worktrees and the harnesses", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher()
                .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
                && matches!(
                    wb.harness_directory().read(cx).listing(),
                    hutan_desktop::workbench::Listing::Ready
                )
                && wb
                    .launcher_harness_choices(cx)
                    .iter()
                    .any(|h| h.id == "omp" && h.enabled())
        })
    });
}

fn open_sheet(view: &Entity<Workbench>, cx: &mut VisualTestContext, preset: LauncherPreset) {
    view.update_in(cx, |wb, window, cx| wb.open_launcher(preset, window, cx));
    assert!(launcher_open(view, cx));
}

/// Presses Start on a first chat with a prompt: the sheet asks, and the test says yes.
fn start_and_confirm(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    click(view, cx, Region::Launcher("start"));
    wait_until(cx, "the question before the first prompt is sent", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher().is_some_and(|l| l.confirm().is_some())
        })
    });
    click(view, cx, Region::Launcher("confirm-start"));
}

fn active_item(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<ItemRef> {
    view.read_with(cx, |wb, _| wb.state().active_item())
}

// Entry points --------------------------------------------------------------------------------

#[gpui::test]
fn cmd_shift_t_opens_the_sheet_in_both_keymap_bases(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    for base in ["zed", "hutan"] {
        view.read_with(cx, |wb, _| {
            wb.settings_store()
                .write_user("keymap.base", toml::Value::String(base.into()))
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!launcher_open(&view, cx), "{base}: closed to begin with");
        cx.simulate_keystrokes("cmd-shift-t");
        assert!(launcher_open(&view, cx), "{base}: ⌘⇧T opens the sheet");
        assert!(
            view.read_with(cx, |wb, _| wb.agent_picker().is_none()),
            "{base}: and not the old picker"
        );
        cx.simulate_keystrokes("escape");
        assert!(!launcher_open(&view, cx), "{base}: Escape closes it");
    }
}

#[gpui::test]
fn the_sheet_opens_from_each_entry_point_with_the_right_project(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);

    // The project in focus: api.
    cx.dispatch_action(actions::OpenWorkspace(env.api_id.to_string()));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.active_workspace()),
        Some(env.api_id)
    );

    // ⌘K "Start Agent or Task…" and the old "Session: New Agent…" are one command.
    cx.dispatch_action(actions::TogglePalette);
    let titles = view.read_with(cx, |wb, _| wb.palette_titles());
    assert!(
        titles.iter().any(|t| t.as_ref() == "Start Agent or Task…"),
        "{titles:?}"
    );
    assert!(!titles.iter().any(|t| t.as_ref() == "Session: New Agent…"));
    view.update(cx, |wb, cx| wb.set_palette_query("Start Agent or Task", cx));
    cx.simulate_keystrokes("enter");
    assert!(launcher_open(&view, cx), "from the palette");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    assert!(
        !view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()),
        "Start now by default"
    );
    cx.simulate_keystrokes("escape");

    // The ▶ of the project row names its own project, not the one in focus.
    click(&view, cx, Region::ProjectStart(env.web_id));
    assert!(launcher_open(&view, cx), "from the project row");
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
    cx.simulate_keystrokes("escape");

    // The sidebar toolbar's ＋ Start takes the project in focus.
    click(&view, cx, Region::Toolbar("start"));
    assert!(launcher_open(&view, cx), "from the sidebar toolbar");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    cx.simulate_keystrokes("escape");

    // The Tasks panel's "+ New task" opens it on "Queue as task".
    cx.dispatch_action(actions::ShowTasksPanel);
    draw(cx);
    click(&view, cx, Region::Tasks("new"));
    assert!(launcher_open(&view, cx), "from the Tasks panel");
    assert_eq!(launcher_project(&view, cx), Some(env.api_id));
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
    cx.simulate_keystrokes("escape");

    // So does the panel's empty state.
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<TasksPanel>("tasks"))
        .expect("the panel is open");
    assert!(panel.read_with(cx, |p, cx| p.rows(0, cx).is_empty()));
    click(&view, cx, Region::Tasks("empty-new"));
    assert!(launcher_open(&view, cx), "from the Tasks empty state");
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
    cx.simulate_keystrokes("escape");

    // And "Tasks: New Task…" in the palette.
    cx.dispatch_action(actions::NewTask);
    assert!(launcher_open(&view, cx), "from Tasks: New Task…");
    assert!(view.read_with(cx, |wb, _| wb.launcher().unwrap().queue()));
}

#[gpui::test]
fn an_empty_project_offers_start_in_its_empty_pane(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    cx.dispatch_action(actions::OpenWorkspace(env.web_id.to_string()));
    wait_until(cx, "the empty pane's Start", |cx| {
        region(&view, cx, Region::Launcher("empty-start"))
    });
    click(&view, cx, Region::Launcher("empty-start"));
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
}

// Starting ------------------------------------------------------------------------------------

#[gpui::test]
fn a_chat_in_a_new_worktree_creates_the_task_with_that_placement_and_starts_it(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    click(&view, cx, Region::ProjectStart(env.api_id));
    ready(&view, cx);

    // With OMP ready the sheet opens on the agent chat (FX-E2E2-C); picking it is a no-op.
    // Type the prompt: the title and the branch follow it.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().kind()),
        LaunchKind::Chat
    );
    click(&view, cx, Region::LauncherChoice("what", "chat"));
    click(&view, cx, Region::Launcher("prompt"));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::Prompt
    );
    cx.simulate_input("Fix the flaky test");
    click(&view, cx, Region::LauncherChoice("where", "new"));
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().unwrap();
        assert_eq!(l.kind(), LaunchKind::Chat);
        assert_eq!(l.agent(), &LaunchAgent::Harness("omp".into()));
        assert_eq!(l.prompt(), "Fix the flaky test");
        assert_eq!(l.title(), "Fix the flaky test");
        assert_eq!(l.place(), &LaunchWhere::NewWorktree);
        assert_eq!(l.branch(), "hutan/fix-the-flaky-test");
        assert_eq!(l.base(), "main", "from the branch the main checkout is on");
        assert!(!l.queue());
    });

    start_and_confirm(&view, cx);
    // (A chat task is done when its first turn ends, which the replayed transcript does at once.)
    wait_until(cx, "the task to start", |_| {
        env.task("Fix the flaky test")
            .is_some_and(|t| t.sessions.len() == 1)
    });
    let task = env.task("Fix the flaky test").unwrap();
    assert!(matches!(task.state, TaskState::Running | TaskState::Done));
    assert_eq!(task.placement.kind, TaskPlacementKind::Worktree);
    assert_eq!(
        task.placement.branch.as_deref(),
        Some("hutan/fix-the-flaky-test")
    );
    assert_eq!(task.placement.base.as_deref(), Some("main"));
    assert_eq!(
        task.run,
        Some(TaskRun::Chat {
            agent: "omp".into(),
            model: None,
        })
    );
    assert_eq!(task.prompt.as_deref(), Some("Fix the flaky test"));
    assert!(!task.auto_start, "Start now is not the scheduler's");

    // The worktree is made, and the session works in it.
    let list: DaemonWorktrees = env.daemon.call(Request::WorktreeList(WorktreeListRequest {
        workspace_id: Some(env.api_id),
    }));
    let made = list
        .worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some("hutan/fix-the-flaky-test"))
        .expect("the task's worktree");
    assert!(!made.main);
    assert_eq!(made.live_sessions, task.sessions);

    // The sheet closed and the new session is open and focused in the center.
    wait_until(cx, "the chat tab", |cx| {
        !launcher_open(&view, cx)
            && active_item(&view, cx).is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
    });
    let focused = view.read_with(cx, |wb, _| wb.focused_session());
    assert_eq!(focused, Some(task.sessions[0]));
}

#[gpui::test]
fn queue_as_task_does_not_start_it(cx: &mut TestAppContext) {
    let env = Env::new();
    // A task nobody finished: the queued one waits for it, so the scheduler leaves it alone.
    let first: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: env.api_id,
        title: "Plan".into(),
        prompt: None,
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        }),
        auto_start: false,
        after_policy: Default::default(),
    }));
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            queue: true,
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "cargo test", cx);
        wb.launcher_toggle_after(first.task.id, cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().title().to_owned()),
        "cargo test"
    );
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the task to exist", |_| {
        env.task("cargo test").is_some()
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));

    let queued = env.task("cargo test").unwrap();
    assert!(queued.auto_start, "the scheduler starts it when ready");
    assert_eq!(queued.depends_on, [first.task.id]);
    assert_eq!(
        queued.run,
        Some(TaskRun::Terminal {
            argv: shell_argv(&user_shell(), "cargo test")
        })
    );
    // The sheet never asked for it to run.
    assert_eq!(queued.state, TaskState::Todo);
    assert!(queued.sessions.is_empty());
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.sessions().read(cx).sessions().len()),
        0,
        "no session was started"
    );
}

#[gpui::test]
fn start_and_new_keeps_the_sheet_and_what_and_where_and_clears_the_prompt(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "sleep 61", cx);
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().branch().to_owned()),
        "hutan/sleep-61"
    );
    click(&view, cx, Region::Launcher("start-new"));
    wait_until(cx, "the first task to run", |_| {
        env.task("sleep 61")
            .is_some_and(|t| t.state == TaskState::Running)
    });
    wait_until(cx, "the sheet to be cleared", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| !l.starting() && l.command().is_empty())
        })
    });
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().expect("the sheet stays open");
        assert_eq!(l.project(), Some(env.api_id), "the project is kept");
        assert_eq!(l.kind(), LaunchKind::Command, "what is kept");
        assert_eq!(l.place(), &LaunchWhere::NewWorktree, "where is kept");
        assert_eq!(l.title(), "", "the title is cleared");
        assert_eq!(l.prompt(), "", "the prompt is cleared");
        assert_eq!(l.branch(), "", "the branch follows the (empty) title");
    });

    // The next one goes the same way, into a worktree of its own.
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "sleep 62", cx);
    });
    cx.simulate_keystrokes("cmd-alt-enter");
    wait_until(cx, "the second task to run", |_| {
        env.task("sleep 62")
            .is_some_and(|t| t.state == TaskState::Running)
    });
    wait_until(cx, "the sheet to be ready again", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| !l.starting() && l.command().is_empty())
        })
    });
    let one = env.task("sleep 61").unwrap();
    let two = env.task("sleep 62").unwrap();
    assert_eq!(one.placement.branch.as_deref(), Some("hutan/sleep-61"));
    assert_eq!(two.placement.branch.as_deref(), Some("hutan/sleep-62"));
    assert!(launcher_open(&view, cx), "still open for the next one");
    // ⌘↩ starts a last one and closes it.
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::Main, cx);
        wb.launcher_set_field(LauncherField::Command, "sleep 63", cx);
    });
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the third task to run and the sheet to close", |cx| {
        env.task("sleep 63")
            .is_some_and(|t| t.state == TaskState::Running)
            && !launcher_open(&view, cx)
    });
}

#[gpui::test]
fn a_checkout_preset_chooses_that_worktree(cx: &mut TestAppContext) {
    let env = Env::new();
    // A worktree of api, made as `worktree.create` would.
    let wt = env.folders.path().join("api-wt-topic");
    git(
        &env.api,
        &["worktree", "add", "-q", "-b", "topic", wt.to_str().unwrap()],
    );
    let wt = fs::canonicalize(&wt).unwrap();
    let (view, cx) = open(cx, &env);
    view.update_in(cx, |wb, window, cx| {
        wb.start_agent_with(
            wt.clone(),
            "Address the review comments".into(),
            None,
            window,
            cx,
        );
    });
    // The worktree is not inside the project's folder: git says whose it is.
    wait_until(cx, "the sheet and its worktrees", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
        })
    });
    wait_until(cx, "the checkout to be chosen", |cx| {
        view.read_with(cx, |wb, _| {
            wb.launcher()
                .is_some_and(|l| l.place() != &LaunchWhere::Main)
        })
    });
    view.read_with(cx, |wb, _| {
        let l = wb.launcher().unwrap();
        assert_eq!(l.project(), Some(env.api_id));
        assert_eq!(l.kind(), LaunchKind::Chat);
        assert_eq!(l.prompt(), "Address the review comments");
        assert_eq!(l.place(), &LaunchWhere::Existing("topic".into()));
    });
}

// Harnesses -----------------------------------------------------------------------------------

#[gpui::test]
fn a_harness_that_is_logged_out_is_disabled_with_log_in(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "the accounts", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .all(|h| h.readiness != Readiness::Checking)
        })
    });
    view.read_with(cx, |wb, cx| {
        let choices = wb.launcher_harness_choices(cx);
        let of = |id: &str| choices.iter().find(|h| h.id == id).unwrap();
        assert!(of("omp").enabled(), "installed and logged in");
        assert_eq!(of("claude").readiness, Readiness::NeedsLogin);
        assert!(!of("claude").enabled());
    });
    // …and clicking it says why below the row and offers Log in…, which starts the harness's own
    // flow in a terminal. Nothing is offered between the choices.
    draw(cx);
    assert!(!region(
        &view,
        cx,
        Region::LauncherChoice("login", "claude")
    ));
    let before: Vec<_> = ["omp", "claude", "codex"]
        .iter()
        .map(|id| bounds_of(&view, cx, Region::LauncherChoice("agent", id)).unwrap())
        .collect();
    click(&view, cx, Region::LauncherChoice("agent", "claude"));
    draw(cx);
    // It cannot be picked: the sheet stays on OMP.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().agent().clone()),
        LaunchAgent::Harness("omp".into())
    );
    assert!(region(&view, cx, Region::LauncherChoice("login", "claude")));
    assert!(!region(&view, cx, Region::LauncherChoice("login", "omp")));
    let after: Vec<_> = ["omp", "claude", "codex"]
        .iter()
        .map(|id| bounds_of(&view, cx, Region::LauncherChoice("agent", id)).unwrap())
        .collect();
    assert_eq!(before, after, "the agent choices did not move");
    let action = bounds_of(&view, cx, Region::LauncherChoice("login", "claude")).unwrap();
    let lowest = after.iter().map(|b| b.bottom()).max().unwrap();
    assert!(action.top() >= lowest, "Log in… sits below the row");
    click(&view, cx, Region::LauncherChoice("login", "claude"));
    wait_until(cx, "the login session to be shown", |cx| {
        view.read_with(cx, |wb, cx| {
            !launcher_open_in(wb)
                && wb.state().active_item().is_some_and(|item| match item {
                    ItemRef::Terminal { session } => session
                        .parse()
                        .ok()
                        .and_then(|id| wb.sessions().read(cx).session(id).cloned())
                        .is_some_and(|s| s.title.contains("Log in to Claude Code")),
                    _ => false,
                })
        })
    });
}

fn launcher_open_in(wb: &Workbench) -> bool {
    wb.launcher().is_some()
}

#[gpui::test]
fn a_harness_that_is_not_installed_is_disabled_with_install(cx: &mut TestAppContext) {
    let env = Env::with(true);
    let options = WorkbenchOptions::builtin(settings(&env), env.daemon.connection(), None)
        .with_package_runner(Arc::new(FakeRunner::new()));
    let (view, cx) = open_options(cx, options);
    synced(&view, cx);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    // The choices as they are drawn before any harness has reported anything.
    draw(cx);
    let choices = |view: &Entity<Workbench>, cx: &VisualTestContext| -> Vec<_> {
        ["omp", "claude", "codex"]
            .iter()
            .map(|id| bounds_of(view, cx, Region::LauncherChoice("agent", id)).unwrap())
            .collect()
    };
    let initial = choices(&view, cx);
    wait_until(cx, "Claude Code to be found missing", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "claude" && matches!(h.readiness, Readiness::NeedsInstall(_)))
        })
    });
    assert!(view.read_with(cx, |wb, cx| {
        let choices = wb.launcher_harness_choices(cx);
        choices.iter().any(|h| h.id == "omp" && h.enabled())
            && !choices.iter().find(|h| h.id == "claude").unwrap().enabled()
    }));
    draw(cx);
    // Nothing is offered until the user asks about Claude Code, and the choices have not moved.
    assert!(!region(
        &view,
        cx,
        Region::LauncherChoice("install", "claude")
    ));
    assert_eq!(
        initial,
        choices(&view, cx),
        "the agent choices keep their bounds when Claude Code is found missing"
    );
    // A missing harness cannot be picked; asking about it says why below the row.
    click(&view, cx, Region::LauncherChoice("agent", "claude"));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().agent().clone()),
        LaunchAgent::Harness("omp".into()),
        "a missing harness cannot be picked"
    );
    assert!(region(
        &view,
        cx,
        Region::LauncherChoice("install", "claude")
    ));
    let after = choices(&view, cx);
    assert_eq!(initial, after, "Install… does not push the choices around");
    let action = bounds_of(&view, cx, Region::LauncherChoice("install", "claude")).unwrap();
    assert!(
        action.top() >= after.iter().map(|b| b.bottom()).max().unwrap(),
        "Install… sits below the row"
    );
    // Install… asks the package registry first; it never installs before the user confirms.
    click(&view, cx, Region::LauncherChoice("install", "claude"));
    wait_until(cx, "the install to be looked at", |cx| {
        view.read_with(cx, |wb, _| {
            wb.modal().is_some() || wb.notice().is_some() || wb.harness_install_checking()
        })
    });
    assert!(
        view.read_with(cx, |wb, _| wb.harness_installs_running().is_empty()),
        "nothing runs before the user confirms"
    );
}

// The shared checkout -------------------------------------------------------------------------

#[gpui::test]
fn the_sheet_warns_when_another_live_agent_works_in_the_chosen_checkout(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    // The first agent starts in api's main checkout.
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("First agent".into()),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none(),
        "nobody works there yet"
    );
    start_and_confirm(&view, cx);
    wait_until(cx, "the first agent to start", |_| {
        env.task("First agent")
            .is_some_and(|t| t.sessions.len() == 1)
    });
    wait_until(cx, "its chat tab", |cx| {
        !launcher_open(&view, cx)
            && active_item(&view, cx).is_some_and(|i| matches!(i, ItemRef::Chat { .. }))
    });

    // The second sheet, on the same main checkout, says so; a new worktree does not.
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("Second agent".into()),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "the warning", |cx| {
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx).is_some())
    });
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("shared-warning")));
    let warning = view
        .read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
        .unwrap();
    assert!(warning.contains("Another live agent"), "{warning}");

    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx)
    });
    draw(cx);
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none()
    );
    assert!(!region(&view, cx, Region::Launcher("shared-warning")));

    // A plain terminal is not an agent: no warning for it either.
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::Main, cx);
        wb.launcher_set_kind(LaunchKind::Terminal, cx);
    });
    assert!(
        view.read_with(cx, |wb, cx| wb.launcher_shared_warning(cx))
            .is_none()
    );
}

// Keyboard ------------------------------------------------------------------------------------

#[gpui::test]
fn tab_walks_the_fields_top_to_bottom_and_the_project_is_a_fuzzy_picker(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    wait_until(&mut *cx, "the models", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx).iter().any(|h| h.enabled())
                && matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty())
                && wb
                    .launcher()
                    .is_some_and(|l| matches!(l.worktrees(), WorktreeList::Ready(_)))
        })
    });
    let stop = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.launcher().unwrap().stop());
    // The caret starts in the prompt; Tab walks down and around.
    let mut walked = vec![stop(cx)];
    for _ in 0..9 {
        cx.simulate_keystrokes("tab");
        walked.push(stop(cx));
    }
    assert_eq!(
        walked,
        [
            LauncherStop::Prompt,
            LauncherStop::Where,
            LauncherStop::Title,
            LauncherStop::More,
            LauncherStop::Mode,
            LauncherStop::Project,
            LauncherStop::What,
            LauncherStop::Agent,
            LauncherStop::Model,
            LauncherStop::Prompt,
        ]
    );
    // Shift-Tab goes back up.
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(stop(cx), LauncherStop::Model);

    // The project is a fuzzy picker: Tab around to it, type to filter, ↩ chooses.
    for _ in 0..3 {
        cx.simulate_keystrokes("shift-tab");
    }
    assert_eq!(stop(cx), LauncherStop::Project);
    cx.simulate_input("web");
    cx.simulate_keystrokes("enter");
    assert_eq!(launcher_project(&view, cx), Some(env.web_id));
}

#[gpui::test]
fn a_picked_model_goes_into_the_task_even_when_it_is_queued(cx: &mut TestAppContext) {
    let env = Env::new();
    let first: TaskResult = env.daemon.call(Request::TaskCreate(TaskCreate {
        workspace_id: env.api_id,
        title: "Plan".into(),
        prompt: None,
        depends_on: Vec::new(),
        placement: None,
        run: Some(TaskRun::Terminal {
            argv: vec!["sh".into(), "-c".into(), "sleep 60".into()],
        }),
        auto_start: false,
        after_policy: Default::default(),
    }));
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("Review the diff".into()),
            queue: true,
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty()),
        )
    });
    let model = view.read_with(cx, |wb, cx| match wb.launcher_models(cx) {
        Models::Ready(models) => models[0].id.clone(),
        _ => unreachable!(),
    });
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some(model.clone()), cx);
        wb.launcher_toggle_after(first.task.id, cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the task to exist", |_| {
        env.task("Review the diff").is_some()
    });
    let queued = env.task("Review the diff").unwrap();
    assert_eq!(
        queued.run,
        Some(TaskRun::Chat {
            agent: "omp".into(),
            model: Some(model)
        }),
        "the pick rides in the task, queued or not"
    );
    assert_eq!(queued.state, TaskState::Todo);
}

// UX-B: a safe Start ---------------------------------------------------------------------------

/// A daemon where OMP and Claude Code are both installed and logged in (their chats replay the
/// same transcript).
fn daemon_both(scripts: &Path) -> TestDaemon {
    let omp = script(
        scripts,
        "omp",
        r#"case "$1" in
  models) echo '{"models":[{"provider":"anthropic","kind":"chat","id":"claude-opus-4-5","selector":"anthropic/claude-opus-4-5","name":"Claude Opus 4.5"}]}' ;;
  usage) echo '{"reports":[]}' ;;
  *) echo "fake omp $*" ;;
esac"#,
    );
    let claude = script(
        scripts,
        "claude",
        r#"if [ "$1 $2" = "auth status" ]; then echo '{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max","email":"me@example.com"}'; else echo "fake claude $*"; fi"#,
    );
    TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {omp:?}\n\n[harness.claude]\nbin = {acp:?}\nargs = [\"replay\", {t:?}, \"acp\"]\ncli = {claude:?}\n",
        acp = acp_bin(),
        t = transcript("plain-turn"),
    ))
}

impl Env {
    /// OMP and Claude Code both startable.
    fn both() -> Self {
        let scripts = tempfile::tempdir().unwrap();
        let folders = tempfile::tempdir().unwrap();
        let daemon = daemon_both(scripts.path());
        let api = repo(folders.path(), "api");
        let web = repo(folders.path(), "web");
        let api_id = daemon.create_workspace("api", &[&api]).id;
        let web_id = daemon.create_workspace("web", &[&web]).id;
        Self {
            _scripts: scripts,
            folders,
            daemon,
            api,
            api_id,
            web_id,
        }
    }
}

fn summary(view: &Entity<Workbench>, cx: &VisualTestContext) -> String {
    view.read_with(cx, |wb, cx| wb.launcher_summary(cx))
}

fn kind_of(view: &Entity<Workbench>, cx: &VisualTestContext) -> LaunchKind {
    view.read_with(cx, |wb, _| wb.launcher().unwrap().kind())
}

/// Waits until the sheet has OMP's models and the worktrees.
fn chat_ready(view: &Entity<Workbench>, cx: &mut VisualTestContext) {
    ready(view, cx);
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty()),
        )
    });
}

fn confirm_shown(view: &Entity<Workbench>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |wb, _| {
        wb.launcher().is_some_and(|l| l.confirm().is_some())
    })
}

/// FX-E2E2-C: with no kind remembered, the sheet opens on an agent chat when a harness is ready.
#[gpui::test]
fn the_first_sheet_opens_on_an_agent_chat_when_a_harness_is_ready(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    chat_ready(&view, cx);
    assert_eq!(kind_of(&view, cx), LaunchKind::Chat);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::Prompt,
        "the keyboard starts in the prompt of a chat"
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher_start_label()),
        "Start OMP chat ⌘↩"
    );
}

/// FX-E2E2-C: with no harness ready, and nothing remembered, the sheet opens on a plain terminal;
/// the kind the user picks stays picked when a harness turns up later.
#[gpui::test]
fn the_first_sheet_opens_on_a_plain_terminal_without_a_ready_harness(cx: &mut TestAppContext) {
    let scripts = tempfile::tempdir().unwrap();
    let folders = tempfile::tempdir().unwrap();
    let missing = scripts.path().join("none");
    let daemon = TestDaemon::start_with_config(&format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[codex]\nbin = {missing:?}\n\n[harness.omp]\nbin = {missing:?}\ncli = {missing:?}\n\n[harness.claude]\nbin = {missing:?}\ncli = {missing:?}\n",
    ));
    let api = repo(folders.path(), "api");
    let api_id = daemon.create_workspace("api", &[&api]).id;
    let web = repo(folders.path(), "web");
    daemon.create_workspace("web", &[&web]);
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(daemon.home())),
        daemon.connection(),
        None,
    );
    synced(&view, cx);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(api_id),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "the harnesses", |cx| {
        view.read_with(cx, |wb, cx| {
            matches!(
                wb.harness_directory().read(cx).listing(),
                hutan_desktop::workbench::Listing::Ready
            )
        })
    });
    assert_eq!(kind_of(&view, cx), LaunchKind::Terminal);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::What
    );
}

#[gpui::test]
fn the_first_sheet_says_what_start_will_do_on_a_plain_terminal(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Terminal, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::What,
        "the keyboard starts on the choice, not in a prompt"
    );
    assert_eq!(summary(&view, cx), "Open a terminal in api (main)");
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher_start_label()),
        "Open terminal ⌘↩"
    );
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("summary")));

    // Start opens the terminal and spends nothing.
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the terminal task", |_| {
        env.task("Terminal")
            .is_some_and(|t| t.run.is_none() && t.sessions.len() == 1)
    });
    assert!(!confirm_shown(&view, cx));
}

#[gpui::test]
fn the_summary_names_agent_model_and_place(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    chat_ready(&view, cx);
    let model = view.read_with(cx, |wb, cx| {
        wb.launcher_chosen_model(cx).unwrap().display_name
    });
    assert_eq!(
        summary(&view, cx),
        format!("Start OMP · {model} chat in api (main)")
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher_start_label()),
        "Start OMP chat ⌘↩"
    );
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx)
    });
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Title, "Fix it", cx);
    });
    assert_eq!(
        summary(&view, cx),
        format!("Start OMP · {model} chat in api (new worktree hutan/fix-it)")
    );
    view.update(cx, |wb, cx| {
        wb.launcher_set_place(LaunchWhere::Main, cx);
        wb.launcher_set_kind(LaunchKind::Command, cx);
        wb.launcher_set_field(LauncherField::Command, "cargo test", cx);
    });
    assert_eq!(summary(&view, cx), "Run “cargo test” in api (main)");
    view.update(cx, |wb, cx| wb.launcher_set_queue(true, cx));
    assert_eq!(summary(&view, cx), "Queue “cargo test” in api (main)");
    view.update(cx, |wb, cx| {
        wb.launcher_set_queue(false, cx);
        wb.launcher_set_kind(LaunchKind::TerminalAgent, cx);
    });
    assert_eq!(summary(&view, cx), "Start OMP in a terminal in api (main)");
    // The footer draws it.
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("summary")));
}

#[gpui::test]
fn the_sheet_reopens_on_what_was_started_last_and_keeps_it_across_restarts(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let state = env.daemon.home().join("desktop-state.json");
    let (view, cx) = open_with(
        cx,
        settings(&env),
        env.daemon.connection(),
        Some(state.clone()),
    );
    synced(&view, cx);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    // Nothing was started yet: the agent chat, as OMP is ready.
    assert_eq!(kind_of(&view, cx), LaunchKind::Chat);
    view.update(cx, |wb, cx| {
        wb.launcher_set_kind(LaunchKind::Command, cx);
        wb.launcher_set_field(LauncherField::Command, "sleep 71", cx);
        wb.launcher_set_place(LaunchWhere::NewWorktree, cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the command to run and the sheet to close", |cx| {
        env.task("sleep 71")
            .is_some_and(|t| t.state == TaskState::Running)
            && !launcher_open(&view, cx)
    });

    // The next sheet opens on a Command in a new worktree.
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    assert_eq!(kind_of(&view, cx), LaunchKind::Command);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().place().clone()),
        LaunchWhere::NewWorktree
    );
    // …and so does the next run of the app: it is in the state file.
    view.update(cx, |wb, _| wb.flush_save());
    let remembered = hutan_desktop::workbench::persistence::load_launcher(&state);
    assert_eq!(remembered.kind.as_deref(), Some("command"));
    assert_eq!(remembered.place.as_deref(), Some("new_worktree"));

    // A preset about an agent (a prompt, say) still opens on an agent chat.
    cx.simulate_keystrokes("escape");
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            prompt: Some("Review".into()),
            ..LauncherPreset::default()
        },
    );
    assert_eq!(kind_of(&view, cx), LaunchKind::Chat);
}

#[gpui::test]
fn the_first_chat_with_a_prompt_asks_once_per_agent_and_model(cx: &mut TestAppContext) {
    let env = Env::both();
    let (view, cx) = open(cx, &env);
    let open_chat = |view: &Entity<Workbench>, cx: &mut VisualTestContext, prompt: &str| {
        open_sheet(
            view,
            cx,
            LauncherPreset {
                project: Some(env.api_id),
                kind: Some(LaunchKind::Chat),
                prompt: Some(prompt.into()),
                ..LauncherPreset::default()
            },
        );
        chat_ready(view, cx);
    };

    // The first start does not send: it asks, naming agent and model, and Cancel changes nothing.
    open_chat(&view, cx, "First prompt");
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx));
    let target = view.read_with(cx, |wb, _| {
        wb.launcher().unwrap().confirm().unwrap().target.clone()
    });
    let model = view.read_with(cx, |wb, cx| {
        wb.launcher_chosen_model(cx).unwrap().display_name
    });
    assert_eq!(target, format!("OMP · {model}"));
    draw(cx);
    for part in [
        "confirm",
        "confirm-start",
        "confirm-cancel",
        "confirm-remember",
    ] {
        assert!(region(&view, cx, Region::Launcher(part)), "{part}");
    }
    assert!(env.tasks().is_empty(), "nothing was sent");
    click(&view, cx, Region::Launcher("confirm-cancel"));
    assert!(!confirm_shown(&view, cx));
    assert!(launcher_open(&view, cx));
    assert!(env.tasks().is_empty());

    // "Don't ask again for this model" starts unticked: the protection stays until the user
    // chooses to drop it.
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx));
    assert!(
        !view.read_with(cx, |wb, _| wb
            .launcher()
            .unwrap()
            .confirm()
            .unwrap()
            .remember),
        "the opt-out is off by default"
    );
    // Yes with the box ticked sends it, and OMP on this model is not asked about again.
    click(&view, cx, Region::Launcher("confirm-remember"));
    assert!(
        view.read_with(cx, |wb, _| wb
            .launcher()
            .unwrap()
            .confirm()
            .unwrap()
            .remember),
        "a click ticks it"
    );
    click(&view, cx, Region::Launcher("confirm-start"));
    wait_until(cx, "the first chat", |_| {
        env.task("First prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));
    open_chat(&view, cx, "Second prompt");
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the second chat, started without asking", |_| {
        env.task("Second prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    assert!(!confirm_shown(&view, cx));
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));

    // Another agent is another question.
    open_chat(&view, cx, "Third prompt");
    wait_until(cx, "Claude Code to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "claude" && h.enabled())
        })
    });
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(LaunchAgent::Harness("claude".into()), cx);
    });
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx), "Claude Code was never confirmed");
    // Not ticking the box asks again next time.
    click(&view, cx, Region::Launcher("confirm-start"));
    wait_until(cx, "the third chat", |_| {
        env.task("Third prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));
    open_chat(&view, cx, "Fourth prompt");
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(LaunchAgent::Harness("claude".into()), cx);
    });
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx), "it was not to be remembered");
    cx.simulate_keystrokes("escape");
    assert!(!confirm_shown(&view, cx));
    assert!(launcher_open(&view, cx), "Escape answers the question only");

    // The setting turns the question off.
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user("launcher.confirm_agent_start", toml::Value::Boolean(false))
    })
    .unwrap();
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the fourth chat, with the question off", |_| {
        env.task("Fourth prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
}

/// The "Don't ask again" choices are listed in Settings → Start Sheet; revoking one asks again
/// at the next start on that agent and model, and turning the question back on offers to
/// forget the rest.
#[gpui::test]
fn remembered_choices_are_listed_in_settings_and_revoking_asks_again(cx: &mut TestAppContext) {
    let env = Env::both();
    let (view, cx) = open(cx, &env);
    let open_chat = |view: &Entity<Workbench>, cx: &mut VisualTestContext, prompt: &str| {
        open_sheet(
            view,
            cx,
            LauncherPreset {
                project: Some(env.api_id),
                kind: Some(LaunchKind::Chat),
                prompt: Some(prompt.into()),
                ..LauncherPreset::default()
            },
        );
        chat_ready(view, cx);
    };
    assert!(
        view.read_with(cx, |wb, _| wb.start_exemptions().is_empty()),
        "nothing is remembered at first"
    );

    // Remember OMP on its model.
    open_chat(&view, cx, "First prompt");
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx));
    click(&view, cx, Region::Launcher("confirm-remember"));
    click(&view, cx, Region::Launcher("confirm-start"));
    wait_until(cx, "the first chat", |_| {
        env.task("First prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));
    open_chat(&view, cx, "Second prompt");
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the second chat, started without asking", |_| {
        env.task("Second prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));

    // The page lists it, by agent and model, with Revoke and Revoke all.
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("launcher"), window, cx);
    });
    let listed = view.read_with(cx, |wb, _| wb.start_exemptions());
    assert_eq!(listed.len(), 1, "{listed:?}");
    let label = view.read_with(cx, |wb, _| wb.exemption_label(&listed[0]));
    assert!(label.starts_with("OMP · "), "{label}");
    draw(cx);
    assert!(region(&view, cx, Region::SettingsItem("revoke", 0)));
    assert!(region(&view, cx, Region::Settings("revoke-all")));

    // Revoke: gone from the list and from the saved state; the next start asks again.
    click(&view, cx, Region::SettingsItem("revoke", 0));
    assert!(view.read_with(cx, |wb, _| wb.start_exemptions().is_empty()));
    view.update_in(cx, |wb, window, cx| wb.close_settings(window, cx));
    open_chat(&view, cx, "Third prompt");
    click(&view, cx, Region::Launcher("start"));
    assert!(confirm_shown(&view, cx), "revoked: it asks again");
    click(&view, cx, Region::Launcher("confirm-remember"));
    click(&view, cx, Region::Launcher("confirm-start"));
    wait_until(cx, "the third chat", |_| {
        env.task("Third prompt")
            .is_some_and(|t| !t.sessions.is_empty())
    });
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));
    assert_eq!(view.read_with(cx, |wb, _| wb.start_exemptions().len()), 1);

    // Turning the question back on asks whether to forget what was remembered.
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user("launcher.confirm_agent_start", toml::Value::Boolean(false))
    })
    .unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.open_settings(Some("launcher"), window, cx);
        wb.sheet_toggle_in("confirm_agent_start", window, cx);
    });
    let (title, alternative) = view.read_with(cx, |wb, _| {
        let modal = wb.modal().unwrap();
        (
            modal.title.to_string(),
            modal.choice_label().map(str::to_owned),
        )
    });
    assert_eq!(title, "Ask again for remembered choices?");
    assert_eq!(alternative.as_deref(), Some("Turn on, keep them"));
    view.update_in(cx, |wb, window, cx| wb.choose_modal_alternative(window, cx));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("confirm_agent_start")),
        Some(serde_json::json!(true))
    );
    assert_eq!(
        view.read_with(cx, |wb, _| wb.start_exemptions().len()),
        1,
        "kept"
    );
    // Forget them: the question is on and nothing is exempt.
    view.read_with(cx, |wb, _| {
        wb.settings_store()
            .write_user("launcher.confirm_agent_start", toml::Value::Boolean(false))
    })
    .unwrap();
    view.update_in(cx, |wb, window, cx| {
        wb.sheet_toggle_in("confirm_agent_start", window, cx);
    });
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    assert!(view.read_with(cx, |wb, _| wb.start_exemptions().is_empty()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("confirm_agent_start")),
        Some(serde_json::json!(true))
    );
    // With nothing remembered the toggle needs no question.
    view.update_in(cx, |wb, window, cx| {
        wb.sheet_toggle_in("confirm_agent_start", window, cx);
    });
    assert!(view.read_with(cx, |wb, _| wb.modal().is_none()));
    assert_eq!(
        view.read_with(cx, |wb, _| wb.sheet_value("confirm_agent_start")),
        Some(serde_json::json!(false))
    );
}

#[gpui::test]
fn terminals_commands_and_queued_chats_never_ask(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, "sleep 72", cx);
    });
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the command", |_| env.task("sleep 72").is_some());
    assert!(!confirm_shown(&view, cx));

    // A chat that only queues sends nothing yet.
    wait_until(cx, "the sheet to close", |cx| !launcher_open(&view, cx));
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            prompt: Some("Later".into()),
            queue: true,
            ..LauncherPreset::default()
        },
    );
    chat_ready(&view, cx);
    click(&view, cx, Region::Launcher("start"));
    wait_until(cx, "the queued chat", |_| env.task("Later").is_some());
    assert!(!confirm_shown(&view, cx));
}

#[gpui::test]
fn rows_change_value_only_when_opened_and_command_enter_waits_for_the_choice(
    cx: &mut TestAppContext,
) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    // The sheet opens on the agent chat; this test is about the What row, on a plain terminal.
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Terminal, cx));
    let stop = |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.launcher().unwrap().stop());
    let open_row =
        |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.launcher().unwrap().open_row());
    assert_eq!(stop(cx), LauncherStop::What);

    // Arrows on a closed row do nothing: nothing can change what Start does by accident.
    for key in ["down", "right", "up", "left"] {
        cx.simulate_keystrokes(key);
        assert_eq!(kind_of(&view, cx), LaunchKind::Terminal, "{key}");
    }
    // The focused row is highlighted.
    draw(cx);
    assert!(region(&view, cx, Region::Launcher("focus")));

    // ↩ opens the row; now the arrows change the value.
    cx.simulate_keystrokes("enter");
    assert_eq!(open_row(cx), Some(LauncherStop::What));
    cx.simulate_keystrokes("right");
    assert_eq!(kind_of(&view, cx), LaunchKind::Command);
    cx.simulate_keystrokes("left left");
    assert_eq!(kind_of(&view, cx), LaunchKind::TerminalAgent);

    // ⌘↩ never fires while the row's choices are open.
    cx.simulate_keystrokes("cmd-enter");
    assert!(launcher_open(&view, cx));
    assert!(!view.read_with(cx, |wb, _| wb.launcher().unwrap().starting()));
    assert!(env.tasks().is_empty());
    // Escape closes the choices, not the sheet.
    cx.simulate_keystrokes("escape");
    assert_eq!(open_row(cx), None);
    assert!(launcher_open(&view, cx));
    // Space opens too, and Tab closes the row and moves on.
    cx.simulate_keystrokes("space");
    assert_eq!(open_row(cx), Some(LauncherStop::What));
    cx.simulate_keystrokes("tab");
    assert_eq!(open_row(cx), None);
    assert_eq!(stop(cx), LauncherStop::Agent);
    // Shift-Tab goes back.
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(stop(cx), LauncherStop::What);

    // The model row follows the same rule.
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Chat, cx));
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(m) if !m.is_empty()),
        )
    });
    cx.simulate_keystrokes("tab tab");
    assert_eq!(stop(cx), LauncherStop::Model);
    cx.simulate_keystrokes("down");
    assert_eq!(open_row(cx), None);
    cx.simulate_keystrokes("enter");
    assert_eq!(open_row(cx), Some(LauncherStop::Model));
    cx.simulate_keystrokes("cmd-enter");
    assert!(env.tasks().is_empty(), "⌘↩ does not fire over an open list");
    cx.simulate_keystrokes("escape");
    assert!(launcher_open(&view, cx));
    assert_eq!(open_row(cx), None);

    // With nothing open ⌘↩ starts, from any row.
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Terminal, cx));
    cx.simulate_keystrokes("cmd-enter");
    wait_until(cx, "the terminal", |_| env.task("Terminal").is_some());
}

#[gpui::test]
fn a_command_runs_through_the_login_shell_in_a_real_terminal(cx: &mut TestAppContext) {
    let env = Env::new();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Command),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    // `;`, `&&`, quotes, redirection: all shell syntax, and the checkout is the cwd.
    let command = "printf 'ONE\\n' > marker.txt; pwd >> marker.txt && echo TWO >> marker.txt";
    view.update(cx, |wb, cx| {
        wb.launcher_set_field(LauncherField::Command, command, cx);
    });
    click(&view, cx, Region::Launcher("start"));
    let marker = env.api.join("marker.txt");
    wait_until(cx, "the shell to run the whole line", |_| {
        std::fs::read_to_string(&marker).is_ok_and(|text| text.lines().count() == 3)
    });
    let text = std::fs::read_to_string(&marker).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "ONE");
    assert!(lines[1].ends_with("/api"), "pwd printed {:?}", lines[1]);
    assert_eq!(lines[2], "TWO");
    let task = env.task(command).expect("the task");
    assert_eq!(
        task.run,
        Some(TaskRun::Terminal {
            argv: shell_argv(&user_shell(), command)
        })
    );
}

#[gpui::test]
fn the_error_of_another_agent_does_not_stay_when_the_agent_changes(cx: &mut TestAppContext) {
    let env = Env::both();
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    chat_ready(&view, cx);
    wait_until(cx, "Claude Code to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "claude" && h.enabled())
        })
    });
    // An empty prompt is an error on OMP…
    click(&view, cx, Region::Launcher("start"));
    let error = view.read_with(cx, |wb, _| {
        wb.launcher().unwrap().error().map(str::to_owned)
    });
    assert!(error.is_some_and(|e| e.contains("needs a prompt")));
    // …which is gone once the agent is another.
    view.update(cx, |wb, cx| {
        wb.launcher_choose_agent(LaunchAgent::Harness("claude".into()), cx);
    });
    assert_eq!(
        view.read_with(cx, |wb, _| wb
            .launcher()
            .unwrap()
            .error()
            .map(str::to_owned)),
        None
    );
}

#[gpui::test]
fn only_the_chosen_agent_says_why_it_cannot_start(cx: &mut TestAppContext) {
    let env = Env::with(true);
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            kind: Some(LaunchKind::Chat),
            ..LauncherPreset::default()
        },
    );
    wait_until(cx, "Claude Code to be found missing", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "claude" && matches!(h.readiness, Readiness::NeedsInstall(_)))
        })
    });
    wait_until(cx, "OMP to be startable", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.launcher_harness_choices(cx)
                .iter()
                .any(|h| h.id == "omp" && h.enabled())
        })
    });
    // OMP is the agent: Claude Code's missing adapter is not this start's problem.
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().agent().clone()),
        LaunchAgent::Harness("omp".into())
    );
    assert_eq!(
        view.read_with(cx, |wb, cx| wb.launcher_agent_status(cx)),
        None
    );
}

/// CL-POLISH-4 item 10: the Model row's list is a popover in front of the sheet (the form does
/// not move), it opens on the model in use, a click picks and closes it, ↩ closes an open one.
#[gpui::test]
fn the_model_list_is_a_popover_that_opens_on_the_model_in_use(cx: &mut TestAppContext) {
    let mut models = Vec::new();
    for family in ["opus", "sonnet", "haiku"] {
        for minor in 1..=8 {
            models.push(format!(
                r#"{{"provider":"anthropic","kind":"chat","id":"claude-{family}-4-{minor}","name":"Claude {family} 4.{minor}"}}"#
            ));
        }
    }
    let env = Env::with_models(&models.join(","));
    let (view, cx) = open(cx, &env);
    open_sheet(
        &view,
        cx,
        LauncherPreset {
            project: Some(env.api_id),
            ..LauncherPreset::default()
        },
    );
    ready(&view, cx);
    view.update(cx, |wb, cx| wb.launcher_set_kind(LaunchKind::Chat, cx));
    wait_until(cx, "the OMP models", |cx| {
        view.read_with(
            cx,
            |wb, cx| matches!(wb.launcher_models(cx), Models::Ready(m) if m.len() == 24),
        )
    });
    let rows = |cx: &VisualTestContext| {
        view.read_with(cx, |wb, cx| {
            let Models::Ready(list) = wb.launcher_models(cx) else {
                panic!("the models are ready");
            };
            wb.launcher_model_rows(&list, cx)
                .iter()
                .map(|r| (r.model.id.clone(), r.current))
                .collect::<Vec<_>>()
        })
    };
    // The model in use is the last of the list (Haiku, the oldest of the family order).
    let last = rows(cx).last().unwrap().0.clone();
    assert!(last.contains("haiku"));
    view.update(cx, |wb, cx| {
        wb.launcher_choose_model(Some(last.clone()), cx)
    });
    common::settle(cx);
    assert!(!region(&view, cx, Region::Launcher("model-popover")));
    let start_before = bounds_of(&view, cx, Region::Launcher("start")).unwrap();

    // A click on the field opens the list in front of the form.
    click(&view, cx, Region::Launcher("model"));
    common::settle(cx);
    // The scroll to the row is applied once the rows were laid out: the next frame.
    draw(cx);
    draw(cx);
    let popover = bounds_of(&view, cx, Region::Launcher("model-popover")).expect("the popover");
    assert_eq!(
        bounds_of(&view, cx, Region::Launcher("start")).unwrap(),
        start_before,
        "the form below does not move"
    );
    // Eight rows at most, so the 24 models scroll, and the one in use is in view.
    assert!(popover.size.height < gpui::px(24.0 * 20.0));
    let at = rows(cx).iter().position(|(_, current)| *current).unwrap();
    let current = bounds_of(&view, cx, Region::LauncherItem("model", at)).expect("the row");
    assert!(
        current.top() >= popover.top() && current.bottom() <= popover.bottom(),
        "the model in use ({current:?}) is inside the popover ({popover:?})"
    );

    // A click on a row picks it and closes the list, the keyboard stays on the row.
    let target = at - 1;
    click(&view, cx, Region::LauncherItem("model", target));
    assert_eq!(rows(cx).iter().filter(|(_, c)| *c).count(), 1);
    assert!(
        rows(cx)[target].1,
        "the clicked model is the one in use now"
    );
    let open_row =
        |cx: &VisualTestContext| view.read_with(cx, |wb, _| wb.launcher().unwrap().open_row());
    assert_eq!(open_row(cx), None);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.launcher().unwrap().stop()),
        LauncherStop::Model
    );
    draw(cx);
    assert!(!region(&view, cx, Region::Launcher("model-popover")));

    // ↩ opens it again, arrows change the model, ↩ closes it; the sheet stays.
    cx.simulate_keystrokes("enter");
    assert_eq!(open_row(cx), Some(LauncherStop::Model));
    cx.simulate_keystrokes("down");
    assert!(rows(cx)[at].1);
    cx.simulate_keystrokes("enter");
    assert_eq!(open_row(cx), None);
    assert!(launcher_open(&view, cx));
    assert!(env.tasks().is_empty());

    // A press outside the list closes it too.
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(open_row(cx), Some(LauncherStop::Model));
    click(&view, cx, Region::Launcher("start"));
    assert_eq!(open_row(cx), None);
}
