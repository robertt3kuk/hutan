#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers
//! Worktree setup and cleanup in the desktop (protocol 0.49): the Changes panel's row says
//! *Setting up…* / *Setup failed* / *Setup waits for your confirmation* with its action, the trust
//! sheet lists the exact commands and confirming runs them, and ⌘K *Worktree: Clean Up…* lists
//! what the policy offers in a sheet and removes only what the user confirms. Real daemon, real
//! git repositories in temporary directories.

use crate::common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{TestDaemon, draw, open_with, wait_until};
use gpui::{Entity, TestAppContext, VisualTestContext};
use hutan_core::WorkspaceId;
use hutan_desktop::workbench::actions;
use hutan_desktop::workbench::panels::ChangesPanel;
use hutan_desktop::workbench::worktree_setup::{SetupAction, setup_line};
use hutan_desktop::workbench::{Region, Workbench};
use hutan_protocol::messages::{Request, SessionInfo, SessionList, SessionListFilter, SessionRef};
use hutan_protocol::worktree::{
    WorktreeCreate, WorktreeCreated, WorktreeInfo, WorktreeList, WorktreeListRequest,
    WorktreeSetupState,
};
use hutan_settings::{SettingsPaths, SettingsStore};

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

struct Fx {
    daemon: TestDaemon,
    root: PathBuf,
    workspace: WorkspaceId,
    _keep: tempfile::TempDir,
}

impl Fx {
    /// A repository named `proj` with `daemon_toml` as the daemon's configuration.
    fn new(daemon_toml: &str, config: Option<&str>) -> Self {
        let daemon = TestDaemon::start_with_config(daemon_toml);
        let keep = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(keep.path()).unwrap().join("proj");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
        if let Some(config) = config {
            fs::create_dir(root.join(".hutan")).unwrap();
            fs::write(root.join(".hutan/worktree.toml"), config).unwrap();
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "-q", "-m", "worktree config"]);
        }
        let workspace = daemon.create_workspace("proj", &[&root]).id;
        Self {
            daemon,
            root,
            workspace,
            _keep: keep,
        }
    }

    fn create(&self, branch: &str) -> WorktreeInfo {
        let created: WorktreeCreated = self.daemon.call(Request::WorktreeCreate(WorktreeCreate {
            workspace_id: self.workspace,
            repo: None,
            branch: branch.into(),
            base: None,
            path: None,
            no_setup: false,
        }));
        created.worktree
    }

    fn worktrees(&self) -> Vec<WorktreeInfo> {
        let list: WorktreeList = self.daemon.call(Request::WorktreeList(WorktreeListRequest {
            workspace_id: Some(self.workspace),
        }));
        list.worktrees
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        let list: SessionList = self
            .daemon
            .call(Request::SessionList(SessionListFilter::default()));
        list.sessions
    }
}

fn open<'a>(
    cx: &'a mut TestAppContext,
    fx: &Fx,
) -> (
    Entity<Workbench>,
    Entity<ChangesPanel>,
    &'a mut VisualTestContext,
) {
    let (view, cx) = open_with(
        cx,
        SettingsStore::new(SettingsPaths::in_dir(fx.daemon.home())),
        fx.daemon.connection(),
        None,
    );
    wait_until(cx, "the first synchronisation", |cx| {
        view.read_with(cx, |wb, cx| {
            wb.sessions().read(cx).synced() && wb.active_workspace().is_some()
        })
    });
    let workspace = fx.workspace;
    view.update_in(cx, |wb, window, cx| {
        wb.switch_workspace(workspace, window, cx)
    });
    cx.dispatch_action(actions::OpenPanel("changes".into()));
    draw(cx);
    let panel = view
        .read_with(cx, |wb, _| wb.panel::<ChangesPanel>("changes"))
        .expect("the Changes panel is drawn");
    (view, panel, cx)
}

/// Index and listing entry of the row of the worktree at `path`.
fn row_of(
    panel: &Entity<ChangesPanel>,
    cx: &VisualTestContext,
    path: &Path,
) -> Option<(usize, WorktreeInfo)> {
    panel.read_with(cx, |p, _| {
        p.rows()
            .iter()
            .position(|w| w.path == path)
            .map(|i| (i, p.rows()[i].clone()))
    })
}

fn region(view: &Entity<Workbench>, cx: &VisualTestContext, region: Region) -> bool {
    view.read_with(cx, |wb, _| wb.region_bounds(region).is_some())
}

fn modal(view: &Entity<Workbench>, cx: &VisualTestContext) -> Option<(String, String, bool)> {
    view.read_with(cx, |wb, _| {
        wb.modal()
            .map(|m| (m.title.to_string(), m.message.to_string(), m.destructive))
    })
}

#[gpui::test]
fn a_running_setup_says_setting_up_and_a_stopped_one_says_setup_failed(cx: &mut TestAppContext) {
    let fx = Fx::new("[worktrees.projects.proj]\nsetup = [\"cat\"]\n", None);
    let wt = fx.create("topic");
    let (view, panel, cx) = open(cx, &fx);

    wait_until(cx, "the row says Setting up…", |cx| {
        row_of(&panel, cx, &wt.path)
            .and_then(|(_, w)| setup_line(&w))
            .is_some_and(|line| line.text == "Setting up…")
    });
    draw(cx);
    let (index, row) = row_of(&panel, cx, &wt.path).unwrap();
    let line = setup_line(&row).unwrap();
    assert_eq!(line.action, Some(("Show output", SetupAction::ShowOutput)));
    assert!(region(&view, cx, Region::ChangesSetup(index)));
    assert!(region(&view, cx, Region::ChangesSetupAction(index)));
    // The worktree without a setup shows no such line.
    let (main_index, _) = row_of(&panel, cx, &fx.root).unwrap();
    assert!(!region(&view, cx, Region::ChangesSetup(main_index)));

    // Show output opens the Setup session.
    let session = row.setup.as_ref().unwrap().session_id.unwrap();
    cx.dispatch_action(actions::ShowWorktreeSetupOutput(
        wt.path.display().to_string(),
    ));
    draw(cx);
    assert_eq!(
        view.read_with(cx, |wb, _| wb.focused_session()),
        Some(session)
    );

    // The user stops the session: the row says the setup failed, with the same action.
    let _: SessionInfo = fx
        .daemon
        .call::<hutan_protocol::messages::SessionResult>(Request::SessionStop(SessionRef {
            session_id: session,
        }))
        .session;
    wait_until(cx, "the row says Setup failed", |cx| {
        row_of(&panel, cx, &wt.path)
            .and_then(|(_, w)| setup_line(&w))
            .is_some_and(|line| line.text == "Setup failed")
    });
    draw(cx);
    let (index, _) = row_of(&panel, cx, &wt.path).unwrap();
    assert!(region(&view, cx, Region::ChangesSetup(index)));
}

#[gpui::test]
fn the_trust_sheet_lists_the_commands_and_confirming_runs_them(cx: &mut TestAppContext) {
    let fx = Fx::new(
        "",
        Some("setup = [\"echo from-the-repository\"]\nteardown = [\"echo bye\"]\n"),
    );
    let wt = fx.create("topic");
    assert_eq!(
        wt.setup.as_ref().map(|s| s.state),
        Some(WorktreeSetupState::NeedsTrust)
    );
    let (view, panel, cx) = open(cx, &fx);
    wait_until(cx, "the row asks for a confirmation", |cx| {
        row_of(&panel, cx, &wt.path)
            .and_then(|(_, w)| setup_line(&w))
            .is_some_and(|line| {
                line.text == "Setup waits for your confirmation"
                    && line.action == Some(("Review…", SetupAction::Review))
            })
    });
    draw(cx);
    let (index, _) = row_of(&panel, cx, &wt.path).unwrap();
    assert!(region(&view, cx, Region::ChangesSetupAction(index)));
    assert!(
        fx.sessions().iter().all(|s| s.title != "Setup"),
        "nothing ran before the confirmation"
    );

    // The sheet is the exact commands.
    cx.dispatch_action(actions::ReviewWorktreeSetup(wt.path.display().to_string()));
    draw(cx);
    let (title, message, destructive) = modal(&view, cx).expect("the trust sheet");
    assert_eq!(title, "Run setup commands from this repository?");
    assert!(
        message.contains("  $ echo from-the-repository"),
        "{message}"
    );
    assert!(message.contains("  $ echo bye"), "{message}");
    assert!(
        destructive,
        "Return cancels: the repository's commands are the risky choice"
    );
    assert!(fx.sessions().iter().all(|s| s.title != "Setup"));

    // Confirming runs the setup; the row stops asking.
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "a Setup session ran", |_| {
        fx.sessions().iter().any(|s| s.title == "Setup")
    });
    wait_until(cx, "the row has nothing to ask any more", |cx| {
        row_of(&panel, cx, &wt.path).is_some_and(|(_, w)| {
            w.setup
                .as_ref()
                .is_some_and(|s| s.state == WorktreeSetupState::Done)
        })
    });
    draw(cx);
    let (index, _) = row_of(&panel, cx, &wt.path).unwrap();
    assert!(!region(&view, cx, Region::ChangesSetup(index)));
}

#[gpui::test]
fn clean_up_lists_what_the_policy_offers_and_removes_only_on_confirm(cx: &mut TestAppContext) {
    let fx = Fx::new("[worktrees]\ncleanup = \"merged\"\n", None);
    let done = fx.create("done");
    let open_one = fx.create("open");
    fs::write(done.path.join("d.txt"), "d\n").unwrap();
    git(&done.path, &["add", "d.txt"]);
    git(&done.path, &["commit", "-q", "-m", "d"]);
    git(
        &fx.root,
        &["merge", "-q", "--no-ff", "-m", "merge done", "done"],
    );
    let (view, _panel, cx) = open(cx, &fx);

    cx.dispatch_action(actions::CleanUpWorktrees);
    wait_until(cx, "the cleanup sheet", |cx| modal(&view, cx).is_some());
    let (title, message, destructive) = modal(&view, cx).unwrap();
    assert_eq!(title, "Clean up 1 worktree?");
    assert!(message.contains("done — "), "{message}");
    assert!(message.contains("(merged)"), "{message}");
    assert!(!message.contains("open — "), "{message}");
    assert!(destructive, "removing is a destructive confirmation");
    assert!(
        done.path.exists(),
        "nothing is removed before the user confirms"
    );

    // Cancel keeps everything.
    view.update_in(cx, |wb, window, cx| wb.close_modal(window, cx));
    draw(cx);
    assert!(done.path.exists());

    cx.dispatch_action(actions::CleanUpWorktrees);
    wait_until(cx, "the cleanup sheet again", |cx| {
        modal(&view, cx).is_some()
    });
    view.update_in(cx, |wb, window, cx| wb.confirm_modal(window, cx));
    wait_until(cx, "the merged worktree to go", |_| !done.path.exists());
    assert!(open_one.path.exists());
    assert_eq!(fx.worktrees().len(), 2);
}

#[gpui::test]
fn clean_up_with_the_policy_never_opens_no_sheet(cx: &mut TestAppContext) {
    let fx = Fx::new("", None);
    let wt = fx.create("topic");
    let (view, _panel, cx) = open(cx, &fx);
    cx.dispatch_action(actions::CleanUpWorktrees);
    draw(cx);
    // Give the daemon's answer time to arrive: the notice names the setting.
    wait_until(cx, "the notice", |cx| {
        view.read_with(cx, |wb, _| {
            wb.notice().is_some_and(|n| n.contains("Cleanup is off"))
        })
    });
    assert!(modal(&view, cx).is_none());
    assert!(wt.path.exists());
}
