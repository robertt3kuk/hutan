//! Setup and cleanup of worktrees in the desktop (protocol 0.49, capability `worktree.setup`,
//! `docs/orchestration.md` § Setup and cleanup).
//!
//! - The state line of a checkout row in the Changes panel ([`setup_line`]): *Setting up…*,
//!   *Setup failed* with *Show output*, *Setup waits for your confirmation* with *Review…*.
//! - The **trust sheet** ([`Workbench::review_worktree_setup`]): the exact commands the repository's
//!   `.hutan/worktree.toml` asks to run, confirmed by the user with the hash the daemon showed.
//!   Only the user can confirm (`worktree.setup_trust` is user-only on the daemon).
//! - ⌘K *Worktree: Clean Up…* ([`Workbench::clean_up_worktrees`]): the worktrees the cleanup policy
//!   offers (`worktree.cleanup`) in a destructive confirmation sheet; nothing is removed until
//!   the user confirms, and each worktree's teardown runs first.

use std::path::{Path, PathBuf};

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};
use hutan_client::ConnectionState;
use hutan_protocol::worktree::{
    CleanupReason, WorktreeCleanup, WorktreeCleanupList, WorktreeInfo, WorktreeSetup,
    WorktreeSetupState,
};

use super::panels::ChangesPanel;
use super::render::probe;
use super::ui::motion;
use super::ui::{Button, IconButton, StyledExt as _};
use super::view::{Region, Workbench};
use super::workspaces::tilde;
use crate::theme::DensityExt as _;

/// How long the toast stays.
const SETUP_TOAST_TIME: Duration = Duration::from_secs(12);

/// *Setup of `wt feature` waits for your confirmation · Review…*: told when a terminal or an
/// agent starts in a worktree whose setup has not run yet (the trust sheet is the only way to
/// run a repository's commands).
#[derive(Debug, Clone)]
pub struct SetupToast {
    pub text: SharedString,
    /// The worktree.
    pub path: PathBuf,
    /// What the daemon said it asks to run (the listing of the window may not have it yet).
    setup: WorktreeSetup,
    rev: u64,
}

/// How strongly a setup line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupTone {
    /// Something is running: the accent colour.
    Busy,
    /// It waits for the user: the warning colour.
    Warn,
    /// It failed: the danger colour.
    Failed,
}

/// What the action of a setup line does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupAction {
    /// Show the *Setup* terminal session.
    ShowOutput,
    /// Open the trust sheet.
    Review,
    /// Run the setup again.
    RunAgain,
}

/// The line a checkout row shows under its path for its setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupLine {
    pub text: String,
    pub tone: SetupTone,
    /// The link next to the text.
    pub action: Option<(&'static str, SetupAction)>,
}

/// The setup line of a worktree; `None` while nothing is to be said (no setup, or it is done).
pub fn setup_line(worktree: &WorktreeInfo) -> Option<SetupLine> {
    let setup = worktree.setup.as_ref()?;
    match setup.state {
        WorktreeSetupState::Running => Some(SetupLine {
            text: "Setting up…".to_owned(),
            tone: SetupTone::Busy,
            action: setup
                .session_id
                .map(|_| ("Show output", SetupAction::ShowOutput)),
        }),
        WorktreeSetupState::Failed => Some(SetupLine {
            text: "Setup failed".to_owned(),
            tone: SetupTone::Failed,
            action: Some(if setup.session_id.is_some() {
                ("Show output", SetupAction::ShowOutput)
            } else {
                ("Run again", SetupAction::RunAgain)
            }),
        }),
        WorktreeSetupState::NeedsTrust => Some(SetupLine {
            text: "Setup waits for your confirmation".to_owned(),
            tone: SetupTone::Warn,
            action: Some(("Review…", SetupAction::Review)),
        }),
        WorktreeSetupState::Done => None,
    }
}

/// What the trust sheet says: whose commands these are, exactly which, and what confirming means.
pub fn trust_sheet_text(path: &Path, setup: &WorktreeSetup) -> String {
    let mut text = format!(
        ".hutan/worktree.toml in the repository asks to run these commands in {}. The file is \
         committed with the repository, so anyone who can push to it can change them: run them \
         only if you trust it.\n",
        tilde(path)
    );
    if !setup.commands.is_empty() {
        text.push_str("\nSetup, in a terminal named Setup:\n");
        for command in &setup.commands {
            text.push_str(&format!("  $ {command}\n"));
        }
    }
    if !setup.teardown.is_empty() {
        text.push_str("\nTeardown, before the worktree is removed:\n");
        for command in &setup.teardown {
            text.push_str(&format!("  $ {command}\n"));
        }
    }
    text.push_str(
        "\nHutan remembers this for exactly these commands in this repository; if they change, \
         it asks again.",
    );
    text
}

/// How many worktrees the cleanup sheet names before it says "and N more".
const SHEET_LISTED: usize = 12;

/// What the cleanup sheet says about the worktrees it would remove.
pub fn cleanup_sheet_text(list: &WorktreeCleanupList) -> String {
    let mut text = match list.mode {
        WorktreeCleanup::Merged => {
            "These worktrees have their branch merged into the default branch.".to_owned()
        }
        WorktreeCleanup::Idle => format!(
            "Nothing has touched these worktrees for {} day(s) or more.",
            list.idle_days
        ),
        WorktreeCleanup::Never => String::new(),
    };
    text.push_str(
        " None has a live session or uncommitted changes. Each one's teardown commands run first; \
         a merged branch is deleted with git branch -d, any other branch stays.\n\n",
    );
    for candidate in list.candidates.iter().take(SHEET_LISTED) {
        let why = match candidate.reason {
            CleanupReason::Merged => "merged".to_owned(),
            CleanupReason::Idle => format!("idle {} d", candidate.idle_days),
        };
        text.push_str(&format!(
            "{} — {} ({why})\n",
            candidate.branch.as_deref().unwrap_or("(detached)"),
            tilde(&candidate.path)
        ));
        for command in &candidate.teardown {
            text.push_str(&format!("    teardown $ {command}\n"));
        }
    }
    if list.candidates.len() > SHEET_LISTED {
        text.push_str(&format!(
            "and {} more\n",
            list.candidates.len() - SHEET_LISTED
        ));
    }
    text.trim_end().to_owned()
}

impl Workbench {
    fn worktree_setup_unavailable(&mut self, cx: &mut Context<Self>) -> bool {
        let store = self.sessions.read(cx);
        if store.connection_state() != ConnectionState::Connected {
            self.set_notice("Not connected to hutand", cx);
            return true;
        }
        if !store.connection().supports_worktree_setup() {
            self.set_notice(
                "This hutand does not run worktree setup; restart it to update",
                cx,
            );
            return true;
        }
        false
    }

    /// The listed worktree at `path`, as `worktree.list` last said.
    fn checkout_at(&self, path: &Path, cx: &Context<Self>) -> Option<WorktreeInfo> {
        self.sessions
            .read(cx)
            .checkouts_all()
            .iter()
            .find(|w| w.path == path)
            .cloned()
    }

    /// The trust sheet of the worktree at `path`: the repository's exact commands, then
    /// `worktree.setup_trust` with the hash the daemon showed.
    pub fn review_worktree_setup(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_setup_unavailable(cx) {
            return;
        }
        let Some(setup) = self
            .checkout_at(&path, cx)
            .and_then(|w| w.setup)
            .filter(|s| s.state == WorktreeSetupState::NeedsTrust)
        else {
            self.set_notice("This worktree's setup does not wait for a confirmation", cx);
            return;
        };
        self.open_trust_sheet(path, &setup, window, cx);
    }

    /// The trust sheet for `setup` of the worktree at `path`.
    fn open_trust_sheet(
        &mut self,
        path: PathBuf,
        setup: &WorktreeSetup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hash) = setup.hash.clone() else {
            return;
        };
        let text = trust_sheet_text(&path, setup);
        // Running commands a repository supplied is the risky choice: Cancel is the default (Return
        // cancels), the button is drawn as a caution.
        self.open_destructive(
            "Run setup commands from this repository?",
            text,
            "Trust and Run",
            move |this, window, cx| {
                this.confirm_worktree_setup(path.clone(), hash.clone(), window, cx);
            },
            window,
            cx,
        );
    }

    /// *Trust and Run*: the setup starts, and its *Setup* terminal is shown (the focused tab) so
    /// the user sees what the repository's commands do.
    fn confirm_worktree_setup(
        &mut self,
        path: PathBuf,
        hash: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_setup_toast(cx);
        self.setup_told.remove(&path);
        let task = self
            .sessions
            .update(cx, |store, cx| store.worktree_setup_trust(path, hash, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| {
                this.refresh_changes_panel(cx);
                match result {
                    Ok(worktree) => {
                        this.set_notice("Setup started", cx);
                        if let Some(session) = worktree.setup.and_then(|s| s.session_id) {
                            this.open_session(session, window, cx);
                        }
                    }
                    Err(err) => this.set_notice(format!("Cannot run the setup: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// The toast of a setup that waits, while it shows (tests).
    pub fn setup_toast(&self) -> Option<&SetupToast> {
        self.setup_toast.as_ref()
    }

    /// A session started in `worktree`: when the worktree's setup waits for the user's
    /// confirmation, say so once, with *Review…*. Nothing is told twice for the same worktree
    /// until the setup was confirmed.
    pub(crate) fn tell_setup_waits(&mut self, worktree: &WorktreeInfo, cx: &mut Context<Self>) {
        let waits = worktree
            .setup
            .as_ref()
            .is_some_and(|s| s.state == WorktreeSetupState::NeedsTrust);
        if !waits || worktree.main || !self.setup_told.insert(worktree.path.clone()) {
            return;
        }
        let name = worktree
            .branch
            .clone()
            .unwrap_or_else(|| tilde(&worktree.path));
        self.setup_toast_rev += 1;
        let rev = self.setup_toast_rev;
        let Some(setup) = worktree.setup.clone() else {
            return;
        };
        self.setup_toast = Some(SetupToast {
            text: format!("Setup of {name} waits for your confirmation").into(),
            path: worktree.path.clone(),
            setup,
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETUP_TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.setup_toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.setup_toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// A session was opened: when it works in a worktree whose setup waits, tell
    /// ([`tell_setup_waits`](Self::tell_setup_waits)). The store's listing is read first; a
    /// worktree it does not know yet (just made) is looked up in the daemon's.
    pub(crate) fn tell_setup_waits_for_session(
        &mut self,
        session: hutan_core::SessionId,
        cx: &mut Context<Self>,
    ) {
        let store = self.sessions.read(cx);
        let Some(info) = store.session(session) else {
            return;
        };
        let Some(placement) = info
            .placement
            .as_ref()
            .filter(|p| p.kind == hutan_protocol::worktree::PlacementKind::Worktree)
        else {
            return;
        };
        let root = placement.root.clone();
        if self.setup_told.contains(&root) {
            return;
        }
        let known = store
            .checkouts_all()
            .iter()
            .find(|w| w.path == root)
            .cloned();
        if let Some(worktree) = known {
            self.tell_setup_waits(&worktree, cx);
            return;
        }
        let workspace = info.workspace_id;
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(workspace, cx));
        cx.spawn(async move |this, cx| {
            let Ok(list) = listing.await else { return };
            if let Some(worktree) = list.into_iter().find(|w| w.path == root) {
                this.update(cx, |this, cx| this.tell_setup_waits(&worktree, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Takes the toast off.
    pub fn dismiss_setup_toast(&mut self, cx: &mut Context<Self>) {
        if self.setup_toast.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn render_setup_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast =
            self.setup_toast_exit
                .follow(self.setup_toast.as_ref(), motion::Kind::Toast, cx)?;
        let (theme, tokens) = (self.theme, self.density.ui());
        let (wanted, toast) = (toast.wanted, toast.value);
        let (path, setup) = (toast.path.clone(), toast.setup.clone());
        Some(
            motion::present(
                "setup-toast-motion",
                motion::Kind::Toast,
                wanted,
                motion::Slide::Rise,
                div()
                    .id("setup-toast")
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .toast(&theme, &tokens)
                    .px(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .child(div().min_w_0().truncate().child(toast.text.clone()))
                    .child(
                        Button::new("setup-toast-review", "Review…", &theme, &tokens)
                            .link()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dismiss_setup_toast(cx);
                                if !this.worktree_setup_unavailable(cx) {
                                    this.open_trust_sheet(path.clone(), &setup, window, cx);
                                }
                            }))
                            .child(probe(&self.regions, Region::SetupToast("review"))),
                    )
                    .child(
                        IconButton::new(
                            "setup-toast-dismiss",
                            crate::icons::Icon::ActionClose,
                            "Dismiss",
                            &theme,
                            &tokens,
                        )
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_setup_toast(cx)))
                        .child(probe(&self.regions, Region::SetupToast("dismiss"))),
                    )
                    .child(probe(&self.regions, Region::SetupToast("toast"))),
            )
            .into_any_element(),
        )
    }

    /// Shows the *Setup* terminal session of the worktree at `path` (its output, live or kept).
    pub fn show_worktree_setup_output(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self
            .checkout_at(&path, cx)
            .and_then(|w| w.setup)
            .and_then(|s| s.session_id);
        match session {
            Some(session) => self.open_session(session, window, cx),
            None => self.set_notice("This worktree has no Setup session to show", cx),
        }
    }

    /// Runs the setup of the worktree at `path` again (`worktree.setup`).
    pub fn rerun_worktree_setup(
        &mut self,
        path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.worktree_setup_unavailable(cx) {
            return;
        }
        let task = self
            .sessions
            .update(cx, |store, cx| store.worktree_setup(path, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.refresh_changes_panel(cx);
                match result {
                    Ok(_) => this.set_notice("Setup started", cx),
                    Err(err) => this.set_notice(format!("Cannot run the setup: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn refresh_changes_panel(&mut self, cx: &mut Context<Self>) {
        self.sessions
            .update(cx, |store, cx| store.refresh_checkouts(cx));
        if let Some(panel) = self.panel::<ChangesPanel>("changes") {
            panel.update(cx, |panel, cx| panel.refresh(cx));
        }
    }

    /// ⌘K *Worktree: Clean Up…*: asks the daemon which worktrees the cleanup policy offers and
    /// shows them in a sheet; confirming removes exactly those.
    pub fn clean_up_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktree_setup_unavailable(cx) {
            return;
        }
        let workspace = self.target_workspace(self.sessions.read(cx)).map(|w| w.id);
        let listing = self
            .sessions
            .update(cx, |store, cx| store.worktree_cleanup(workspace, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = listing.await;
            this.update_in(cx, |this, window, cx| match result {
                Err(err) => {
                    this.set_notice(format!("Cannot list worktrees to clean up: {err}"), cx)
                }
                Ok(list) if list.mode == WorktreeCleanup::Never => this.set_notice(
                    "Cleanup is off: choose “After merge” or “After days idle” in Settings → \
                     Sessions → Worktrees",
                    cx,
                ),
                Ok(list) if list.candidates.is_empty() => {
                    this.set_notice("No worktree to clean up", cx);
                }
                Ok(list) => this.open_cleanup_sheet(workspace, list, window, cx),
            })
            .ok();
        })
        .detach();
    }

    fn open_cleanup_sheet(
        &mut self,
        workspace: Option<hutan_core::WorkspaceId>,
        list: WorktreeCleanupList,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = list.candidates.len();
        let paths: Vec<PathBuf> = list.candidates.iter().map(|c| c.path.clone()).collect();
        self.open_destructive(
            if count == 1 {
                "Clean up 1 worktree?".to_owned()
            } else {
                format!("Clean up {count} worktrees?")
            },
            cleanup_sheet_text(&list),
            "Remove",
            move |this, _window, cx| this.prune_worktrees(workspace, paths.clone(), cx),
            window,
            cx,
        );
    }

    fn prune_worktrees(
        &mut self,
        workspace: Option<hutan_core::WorkspaceId>,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.worktree_prune(workspace, paths, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.refresh_changes_panel(cx);
                match result {
                    Ok(pruned) if pruned.failed.is_empty() => {
                        this.set_notice(format!("Removed {} worktree(s)", pruned.removed.len()), cx)
                    }
                    Ok(pruned) => this.set_notice(
                        format!(
                            "Removed {}; kept {}: {}",
                            pruned.removed.len(),
                            pruned.failed.len(),
                            pruned.failed[0].reason
                        ),
                        cx,
                    ),
                    Err(err) => this.set_notice(format!("Cannot clean up: {err}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::worktree::CleanupCandidate;

    fn worktree(setup: Option<WorktreeSetup>) -> WorktreeInfo {
        WorktreeInfo {
            path: "/w/repo-wt/x".into(),
            repo_root: "/w/repo".into(),
            branch: Some("x".into()),
            head: "0".repeat(40),
            main: false,
            created_by_hutan: true,
            dirty: false,
            live_sessions: vec![],
            locked: false,
            prunable: false,
            setup,
        }
    }

    fn setup(state: WorktreeSetupState, session: bool) -> WorktreeSetup {
        WorktreeSetup {
            state,
            session_id: session.then(hutan_core::SessionId::new),
            commands: vec!["pnpm install".into()],
            teardown: vec!["docker compose down".into()],
            hash: Some("ab".repeat(32)),
            copied: 0,
            message: None,
        }
    }

    #[test]
    fn each_setup_state_has_its_line_and_action() {
        assert_eq!(setup_line(&worktree(None)), None);
        assert_eq!(
            setup_line(&worktree(Some(setup(WorktreeSetupState::Done, true)))),
            None
        );
        let running =
            setup_line(&worktree(Some(setup(WorktreeSetupState::Running, true)))).unwrap();
        assert_eq!(running.text, "Setting up…");
        assert_eq!(running.tone, SetupTone::Busy);
        assert_eq!(
            running.action,
            Some(("Show output", SetupAction::ShowOutput))
        );
        let failed = setup_line(&worktree(Some(setup(WorktreeSetupState::Failed, true)))).unwrap();
        assert_eq!(failed.text, "Setup failed");
        assert_eq!(failed.tone, SetupTone::Failed);
        assert_eq!(
            failed.action,
            Some(("Show output", SetupAction::ShowOutput))
        );
        let no_session =
            setup_line(&worktree(Some(setup(WorktreeSetupState::Failed, false)))).unwrap();
        assert_eq!(
            no_session.action,
            Some(("Run again", SetupAction::RunAgain))
        );
        let trust = setup_line(&worktree(Some(setup(
            WorktreeSetupState::NeedsTrust,
            false,
        ))))
        .unwrap();
        assert_eq!(trust.text, "Setup waits for your confirmation");
        assert_eq!(trust.action, Some(("Review…", SetupAction::Review)));
    }

    #[test]
    fn the_trust_sheet_lists_the_exact_commands() {
        let text = trust_sheet_text(
            Path::new("/w/repo-wt/x"),
            &setup(WorktreeSetupState::NeedsTrust, false),
        );
        assert!(text.contains("  $ pnpm install"), "{text}");
        assert!(
            text.contains("Teardown, before the worktree is removed:\n  $ docker compose down")
        );
        assert!(text.contains("anyone who can push"));
    }

    #[test]
    fn the_cleanup_sheet_names_each_worktree_with_its_reason_and_teardown() {
        let candidate = |n: usize| CleanupCandidate {
            path: format!("/w/repo-wt/b{n}").into(),
            repo_root: "/w/repo".into(),
            branch: Some(format!("b{n}")),
            reason: if n == 0 {
                CleanupReason::Merged
            } else {
                CleanupReason::Idle
            },
            idle_days: 40,
            teardown: if n == 0 {
                vec!["docker compose down".into()]
            } else {
                vec![]
            },
        };
        let list = WorktreeCleanupList {
            mode: WorktreeCleanup::Merged,
            idle_days: 14,
            candidates: (0..14).map(candidate).collect(),
        };
        let text = cleanup_sheet_text(&list);
        assert!(text.contains("b0 — /w/repo-wt/b0 (merged)\n    teardown $ docker compose down"));
        assert!(text.contains("b1 — /w/repo-wt/b1 (idle 40 d)"));
        assert!(text.ends_with("and 2 more"), "{text}");
        assert!(!text.contains("b13"));
    }
}
