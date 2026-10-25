//! Setup and teardown of worktrees (protocol 0.49, capability `worktree.setup`).
//!
//! After `worktree.create` the daemon resolves what the repository's `.hutan/worktree.toml` and
//! the user's `[worktrees.projects.<p>]` ask for ([`super::setup_file`]), copies the gitignored
//! files from the main checkout, and runs `setup` in a **terminal session named *Setup*** in the
//! new worktree: the user sees the output, can stop it, and the session's exit decides the
//! state shown on the worktree (`running`, `done`, `failed`).
//!
//! Commands that come from the repository's file need the user's confirmation, once per exact
//! content (`worktree_setup_trust`, keyed by the main checkout and the sha-256 of the lists):
//! before it, the state is `needs_trust` and nothing runs. `worktree.setup_trust` is user-only, so
//! an agent that creates a worktree can neither run an unconfirmed command nor confirm it.
//! Commands the user wrote in `daemon.toml` are theirs and run without the question.
//!
//! `teardown` runs before the worktree is removed, headless (its output is captured and, when it
//! fails, quoted in the refusal), under the same trust rule.
//!
//! The state of a setup is kept in memory: after a daemon restart the *Setup* session is restored
//! like any terminal (a fresh shell; the commands do not run again) and the worktree simply shows
//! no setup.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use hutan_core::{SessionId, SessionState, WorkspaceId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{
    Event, SessionCreate, SessionInfo, SessionListFilter, SessionSpec, TerminalSpec, WorkspaceInfo,
};
use hutan_protocol::worktree::{
    WorktreeInfo, WorktreeSetup, WorktreeSetupRequest, WorktreeSetupResult, WorktreeSetupState,
    WorktreeSetupTrust,
};
use hutan_storage::WorktreeSetupTrustRow;
use parking_lot::Mutex;
use tokio::process::Command;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::setup_file::{self, CopyReport, Resolved};
use super::{
    READ_TIMEOUT, WorktreeService, bad_request, invalid_state, join_failed, workspaces_of_repo,
};
use crate::registry::now_ms;

/// The title of the session that runs the commands.
pub const SETUP_TITLE: &str = "Setup";

/// A teardown command that runs longer is killed and the removal refused.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(600);

/// An agent chat waits at most this long for a setup that is still running.
const WAIT_LIMIT: Duration = Duration::from_secs(30 * 60);

/// How often running setups are compared with their sessions, in case the observer missed an
/// event.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// Events the observer's queue holds.
const OBSERVER_QUEUE: usize = 256;

/// The most skipped files or warnings quoted in [`WorktreeSetup::message`].
const MESSAGE_ITEMS: usize = 6;

/// The tail of a failing teardown's output quoted in the refusal.
const OUTPUT_TAIL: usize = 1500;

#[derive(Debug)]
struct Record {
    workspace_id: WorkspaceId,
    info: WorktreeSetup,
}

/// What the daemon remembers about the setups it ran.
#[derive(Debug, Default)]
pub(super) struct Setups {
    records: Mutex<HashMap<PathBuf, Record>>,
    /// Told whenever a record changes: the chats that wait for a setup look again.
    changed: Notify,
}

impl Setups {
    fn set(&self, path: &Path, workspace_id: WorkspaceId, info: WorktreeSetup) {
        self.records
            .lock()
            .insert(path.to_owned(), Record { workspace_id, info });
        self.changed.notify_waiters();
    }

    fn forget(&self, path: &Path) {
        self.records.lock().remove(path);
        self.changed.notify_waiters();
    }

    fn get(&self, path: &Path) -> Option<WorktreeSetup> {
        self.records.lock().get(path).map(|r| r.info.clone())
    }

    fn running_in(&self, dir: &Path) -> bool {
        self.records
            .lock()
            .iter()
            .any(|(path, r)| r.info.state == WorktreeSetupState::Running && dir.starts_with(path))
    }

    fn running_sessions(&self) -> Vec<SessionId> {
        self.records
            .lock()
            .values()
            .filter(|r| r.info.state == WorktreeSetupState::Running)
            .filter_map(|r| r.info.session_id)
            .collect()
    }
}

impl WorktreeService {
    /// Puts the setup state on listed worktrees.
    pub(super) fn annotate(&self, worktrees: &mut [WorktreeInfo]) {
        for worktree in worktrees {
            worktree.setup = self.setups.get(&worktree.path);
        }
    }

    /// Watches the sessions until `shutdown` and moves a setup to `done` or `failed` when its
    /// session ends.
    pub fn run(self: &Arc<Self>, shutdown: CancellationToken) -> JoinHandle<()> {
        let (tx, mut rx) = mpsc::channel(OBSERVER_QUEUE);
        self.bus.set_setup_observer(Some(tx));
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut reconcile = tokio::time::interval(RECONCILE_INTERVAL);
            reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    event = rx.recv() => match event {
                        Some(Event::SessionUpdated { session }) => this.observe(&session),
                        Some(_) => {}
                        None => break,
                    },
                    _ = reconcile.tick() => this.reconcile().await,
                }
            }
            this.bus.set_setup_observer(None);
        })
    }

    /// Looks at the *Setup* sessions that should be running, as they are now.
    async fn reconcile(&self) {
        let running = self.setups.running_sessions();
        if running.is_empty() {
            return;
        }
        let Ok(list) = self
            .registry
            .list_sessions(SessionListFilter::default())
            .await
        else {
            return;
        };
        for session in list.sessions.iter().filter(|s| running.contains(&s.id)) {
            self.observe(session);
        }
    }

    /// A session changed: when it is a running setup's and it ended, the setup ended.
    fn observe(&self, session: &SessionInfo) {
        let (outcome, message) = match &session.state {
            SessionState::Exited {
                code: Some(0),
                signal: None,
            } => (WorktreeSetupState::Done, None),
            SessionState::Exited {
                code: Some(code), ..
            } => (
                WorktreeSetupState::Failed,
                Some(format!("setup exited with code {code}")),
            ),
            SessionState::Exited { signal, .. } => (
                WorktreeSetupState::Failed,
                Some(match signal {
                    Some(signal) => format!("setup was ended by signal {signal}"),
                    None => "setup ended without an exit code".to_owned(),
                }),
            ),
            SessionState::Failed { reason } => (
                WorktreeSetupState::Failed,
                Some(format!("setup could not run: {reason}")),
            ),
            SessionState::Stopped => (
                WorktreeSetupState::Failed,
                Some("setup was stopped".to_owned()),
            ),
            _ => return,
        };
        let workspace = {
            let mut records = self.setups.records.lock();
            let Some(record) = records.values_mut().find(|r| {
                r.info.state == WorktreeSetupState::Running && r.info.session_id == Some(session.id)
            }) else {
                return;
            };
            record.info.state = outcome;
            if let Some(message) = message {
                // The reason comes first; what the copy left out follows.
                record.info.message = Some(match record.info.message.take() {
                    Some(notes) => format!("{message}; {notes}"),
                    None => message,
                });
            }
            record.workspace_id
        };
        self.setups.changed.notify_waiters();
        self.announce_setup(workspace);
    }

    /// Tells every workspace of the repository's clients to list again.
    fn announce_setup(&self, workspace: WorkspaceId) {
        self.changed(&[workspace]);
    }

    /// Waits while a worktree containing `dir` is setting up and `[worktrees] wait_for_setup`
    /// is on, at most [`WAIT_LIMIT`]. A setup that needs the user's confirmation, or failed,
    /// does not hold anything back.
    pub async fn wait_for_setup(&self, dir: &Path) {
        if !self.settings.read().wait_for_setup {
            return;
        }
        let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
        let wait = async {
            loop {
                let notified = self.setups.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !self.setups.running_in(&dir) {
                    return;
                }
                notified.await;
            }
        };
        if tokio::time::timeout(WAIT_LIMIT, wait).await.is_err() {
            warn!(dir = %dir.display(), "gave up waiting for a worktree setup");
        }
    }

    /// The workspace whose project settings apply to the worktree at `path` of the repository
    /// `main`: the one it was created in, else the first workspace of the repository.
    pub(super) async fn setup_workspace(
        &self,
        workspaces: &[WorkspaceInfo],
        main: &Path,
        path: &Path,
    ) -> Option<WorkspaceInfo> {
        let storage = self.storage.clone();
        let stored = tokio::task::spawn_blocking(move || storage.worktrees().list())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        if let Some(row) = stored.iter().find(|row| row.path == path)
            && let Some(workspace) = workspaces.iter().find(|w| w.id == row.workspace_id)
        {
            return Some(workspace.clone());
        }
        let ids = workspaces_of_repo(workspaces, main).await;
        workspaces.iter().find(|w| ids.contains(&w.id)).cloned()
    }

    /// What the repository and the user ask for, for the project of `workspace`.
    pub(super) async fn resolve_setup(
        &self,
        workspace: &WorkspaceInfo,
        main: &Path,
    ) -> Result<Resolved, String> {
        let main = main.to_owned();
        let file = tokio::task::spawn_blocking(move || setup_file::read(&main))
            .await
            .map_err(|e| format!("background task failed: {e}"))??;
        let user = self
            .settings
            .read()
            .for_project(&workspace.id.to_string(), &workspace.name);
        Ok(Resolved::new(file, &user))
    }

    /// Whether the user confirmed the commands with `hash` in the repository `main`.
    pub(super) async fn setup_trusted(&self, main: &Path, hash: &str) -> bool {
        let storage = self.storage.clone();
        let main = main.to_owned();
        let hash = hash.to_owned();
        tokio::task::spawn_blocking(move || storage.worktree_setup_trust().list())
            .await
            .ok()
            .and_then(Result::ok)
            .is_some_and(|rows| {
                rows.iter()
                    .any(|row| row.repo_root == main && row.hash == hash)
            })
    }

    /// Copies, then runs (or asks about) the setup of the worktree at `path`, and records the
    /// state. Returns it; `None` when nothing is configured.
    pub(super) async fn begin_setup(
        &self,
        workspace: &WorkspaceInfo,
        main: &Path,
        path: &Path,
    ) -> Option<WorktreeSetup> {
        let resolved = match self.resolve_setup(workspace, main).await {
            Ok(resolved) => resolved,
            Err(message) => {
                let info = WorktreeSetup {
                    state: WorktreeSetupState::Failed,
                    session_id: None,
                    commands: Vec::new(),
                    teardown: Vec::new(),
                    hash: None,
                    copied: 0,
                    message: Some(message),
                };
                self.setups.set(path, workspace.id, info.clone());
                self.announce_setup(workspace.id);
                return Some(info);
            }
        };
        if resolved.copy.is_empty() && resolved.setup.is_empty() {
            // Nothing to do now (a teardown alone has no state to show).
            self.setups.forget(path);
            return None;
        }
        let report = self.copy_ignored(main, path, &resolved.copy).await;
        let mut notes: Vec<String> = resolved.warnings.clone();
        notes.extend(report.skipped.iter().cloned());
        let mut info = WorktreeSetup {
            state: WorktreeSetupState::Done,
            session_id: None,
            commands: resolved.setup.clone(),
            teardown: resolved.teardown.clone(),
            hash: None,
            copied: u32::try_from(report.copied.len()).unwrap_or(u32::MAX),
            message: summarize(&notes),
        };
        if resolved.setup.is_empty() {
            self.setups.set(path, workspace.id, info.clone());
            self.announce_setup(workspace.id);
            return Some(info);
        }
        if resolved.setup_from_file
            && let Some(hash) = resolved.trust_hash()
            && !self.setup_trusted(main, &hash).await
        {
            info.state = WorktreeSetupState::NeedsTrust;
            info.hash = Some(hash);
            self.setups.set(path, workspace.id, info.clone());
            self.announce_setup(workspace.id);
            return Some(info);
        }

        info.state = WorktreeSetupState::Running;
        let script = setup_script(&report.copied, &resolved.setup);
        let spec = TerminalSpec {
            command: vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
            cwd: Some(path.to_owned()),
            env: BTreeMap::from([
                ("HUTAN_WORKTREE_SETUP".to_owned(), "1".to_owned()),
                (
                    "HUTAN_MAIN_CHECKOUT".to_owned(),
                    main.to_string_lossy().into_owned(),
                ),
            ]),
            title: Some(SETUP_TITLE.to_owned()),
            size: None,
        };
        match self
            .registry
            .create_session(SessionCreate {
                workspace_id: Some(workspace.id),
                spec: SessionSpec::Terminal(spec),
            })
            .await
        {
            Ok(session) => {
                info.session_id = Some(session.id);
                self.setups.set(path, workspace.id, info.clone());
                self.announce_setup(workspace.id);
                // The command may have ended before the record existed.
                self.reconcile().await;
                Some(self.setups.get(path).unwrap_or(info))
            }
            Err(error) => {
                info.state = WorktreeSetupState::Failed;
                info.message = Some(format!(
                    "the Setup session did not start: {}",
                    error.message
                ));
                self.setups.set(path, workspace.id, info.clone());
                self.announce_setup(workspace.id);
                Some(info)
            }
        }
    }

    /// Copies the gitignored files `globs` match from `main` into `dest`.
    async fn copy_ignored(&self, main: &Path, dest: &Path, globs: &[String]) -> CopyReport {
        if globs.is_empty() {
            return CopyReport::default();
        }
        let mut report = CopyReport::default();
        let (from, globs_owned) = (main.to_owned(), globs.to_vec());
        let Ok((found, mut skipped)) = tokio::task::spawn_blocking(move || {
            let mut skipped = Vec::new();
            let found = setup_file::candidates(&from, &globs_owned, &mut skipped);
            (found, skipped)
        })
        .await
        else {
            return report;
        };
        report.skipped.append(&mut skipped);
        let mut ignored: Vec<String> = Vec::new();
        for chunk in found.chunks(400) {
            match check_ignore(main, chunk).await {
                Ok(mut some) => ignored.append(&mut some),
                Err(message) => {
                    report
                        .skipped
                        .push(format!("every file (git check-ignore: {message})"));
                    return report;
                }
            }
        }
        for file in &found {
            if !ignored.contains(file) {
                report.skipped.push(format!(
                    "{file} (not ignored by git: only gitignored files are copied)"
                ));
            }
        }
        ignored.sort();
        let (from, to) = (main.to_owned(), dest.to_owned());
        match tokio::task::spawn_blocking(move || {
            let mut copied = CopyReport::default();
            setup_file::copy_files(&from, &to, &ignored, &mut copied);
            copied
        })
        .await
        {
            Ok(mut copied) => {
                report.copied.append(&mut copied.copied);
                report.skipped.append(&mut copied.skipped);
            }
            Err(error) => report.skipped.push(format!("copy failed: {error}")),
        }
        report
    }

    /// `worktree.setup`: runs the setup of an existing worktree again.
    pub async fn setup(&self, req: WorktreeSetupRequest) -> Result<WorktreeSetupResult, ErrorBody> {
        let (main, entry, workspace) = self.setup_target(&req.path).await?;
        self.restart_setup(&main, &entry, &workspace).await
    }

    /// `worktree.setup_trust`: confirms the repository's commands, then runs the setup.
    pub async fn setup_trust(
        &self,
        req: WorktreeSetupTrust,
        by: &str,
    ) -> Result<WorktreeSetupResult, ErrorBody> {
        let (main, entry, workspace) = self.setup_target(&req.path).await?;
        let resolved = self
            .resolve_setup(&workspace, &main)
            .await
            .map_err(invalid_state)?;
        let Some(current) = resolved.trust_hash() else {
            return Err(invalid_state(
                "this project's setup has no commands from the repository to confirm",
            ));
        };
        if current != req.hash {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                "the setup commands changed since they were shown; look at them again",
            ));
        }
        let storage = self.storage.clone();
        let row = WorktreeSetupTrustRow {
            repo_root: main.clone(),
            hash: current,
            decided_at_ms: now_ms(),
            decided_by: by.to_owned(),
        };
        tokio::task::spawn_blocking(move || storage.worktree_setup_trust().trust(row))
            .await
            .map_err(join_failed)?
            .map_err(|e| {
                ErrorBody::new(
                    ErrorCode::Internal,
                    format!("cannot remember the confirmation: {e}"),
                )
            })?;
        self.restart_setup(&main, &entry, &workspace).await
    }

    async fn setup_target(
        &self,
        path: &Path,
    ) -> Result<(PathBuf, super::RawWorktree, WorkspaceInfo), ErrorBody> {
        if !path.is_absolute() {
            return Err(bad_request(format!(
                "`{}` is not an absolute path",
                path.display()
            )));
        }
        let wanted = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let (main, entry) = self.find(&workspaces, &wanted, path).await?;
        if entry.is_main {
            return Err(invalid_state(format!(
                "{} is the main worktree of {}; setup is for the worktrees made next to it",
                entry.path.display(),
                main.display()
            )));
        }
        let workspace = self
            .setup_workspace(&workspaces, &main, &entry.path)
            .await
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("no workspace holds the repository {}", main.display()),
                )
            })?;
        Ok((main, entry, workspace))
    }

    async fn restart_setup(
        &self,
        main: &Path,
        entry: &super::RawWorktree,
        workspace: &WorkspaceInfo,
    ) -> Result<WorktreeSetupResult, ErrorBody> {
        if self
            .setups
            .get(&entry.path)
            .is_some_and(|info| info.state == WorktreeSetupState::Running)
        {
            return Err(invalid_state(format!(
                "the setup of {} is still running",
                entry.path.display()
            )));
        }
        if self
            .begin_setup(workspace, main, &entry.path)
            .await
            .is_none()
        {
            return Err(invalid_state(
                "nothing is configured for this project: add `.hutan/worktree.toml` with `copy`, \
                 `setup` or `teardown`",
            ));
        }
        let worktree = self.describe_one(main, &entry.path).await?;
        Ok(WorktreeSetupResult { worktree })
    }

    /// The listing entry of the worktree at `path` of the repository `main`, setup state included.
    pub(super) async fn describe_one(
        &self,
        main: &Path,
        path: &Path,
    ) -> Result<WorktreeInfo, ErrorBody> {
        let sessions = self.live_sessions().await?;
        let stored = self.stored_paths().await?;
        let mut listed =
            super::describe(main, super::list_raw(main).await?, &stored, &sessions).await;
        self.annotate(&mut listed);
        listed.into_iter().find(|w| w.path == path).ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::Internal,
                format!("git does not list the worktree {}", path.display()),
            )
        })
    }

    /// Forgets the setup state of a removed worktree.
    pub(super) fn forget_setup(&self, path: &Path) {
        self.setups.forget(path);
    }

    /// Runs the teardown commands that apply to the worktree at `path` before it is removed.
    /// `Err` is the reason a command failed (the removal is refused unless forced). Commands
    /// from the repository's file that the user has not confirmed are not run.
    pub(super) async fn run_teardown(
        &self,
        workspaces: &[WorkspaceInfo],
        main: &Path,
        path: &Path,
    ) -> Result<(), String> {
        let Some(workspace) = self.setup_workspace(workspaces, main, path).await else {
            return Ok(());
        };
        let resolved = self.resolve_setup(&workspace, main).await?;
        if resolved.teardown.is_empty() {
            return Ok(());
        }
        if resolved.teardown_from_file
            && let Some(hash) = resolved.trust_hash()
            && !self.setup_trusted(main, &hash).await
        {
            debug!(path = %path.display(), "teardown not run: the repository's commands are not confirmed");
            return Ok(());
        }
        let harnesses = self.harnesses.clone();
        let mut env = tokio::task::spawn_blocking(move || harnesses.environment())
            .await
            .map_err(|e| e.to_string())?;
        env.insert("HUTAN_WORKTREE_TEARDOWN".to_owned(), "1".to_owned());
        env.insert(
            "HUTAN_MAIN_CHECKOUT".to_owned(),
            main.to_string_lossy().into_owned(),
        );
        for command in &resolved.teardown {
            run_command(command, path, &env).await?;
        }
        Ok(())
    }
}

/// The paths of `paths` (relative to `main`) that git ignores and does not track, as
/// `git check-ignore --stdin -z` says.
async fn check_ignore(main: &Path, paths: &[String]) -> Result<Vec<String>, String> {
    use tokio::io::AsyncWriteExt as _;
    let mut command = super::git_command(
        main,
        &[
            std::ffi::OsStr::new("check-ignore"),
            std::ffi::OsStr::new("--stdin"),
            std::ffi::OsStr::new("-z"),
        ],
    );
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    if let Some(mut stdin) = child.stdin.take() {
        tokio::spawn(async move {
            // A git that exits early closes the pipe; its status says what happened.
            let _ = stdin.write_all(&input).await;
        });
    }
    let output = tokio::time::timeout(READ_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "it took too long".to_owned())?
        .map_err(|e| e.to_string())?;
    // Exit 1 means none of them is ignored.
    if output.status.code().is_some_and(|code| code > 1) || output.status.code().is_none() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Runs `command` with `/bin/sh -c` in `cwd`, capturing its output.
async fn run_command(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut child = Command::new("/bin/sh");
    child
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(TEARDOWN_TIMEOUT, child.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(format!("teardown `{command}` could not start: {error}")),
        Err(_) => {
            return Err(format!(
                "teardown `{command}` ran longer than {} minutes and was stopped",
                TEARDOWN_TIMEOUT.as_secs() / 60
            ));
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let text = text.trim();
    let tail = if text.len() > OUTPUT_TAIL {
        let mut cut = text.len() - OUTPUT_TAIL;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        format!("…{}", &text[cut..])
    } else {
        text.to_owned()
    };
    let how = output.status.code().map_or_else(
        || "was ended by a signal".to_owned(),
        |code| format!("exited with {code}"),
    );
    Err(if tail.is_empty() {
        format!("teardown `{command}` {how}")
    } else {
        format!("teardown `{command}` {how}: {tail}")
    })
}

/// The notes (what the copy left out, unknown keys) as one line, the first few listed.
fn summarize(notes: &[String]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }
    let mut text = notes
        .iter()
        .take(MESSAGE_ITEMS)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if notes.len() > MESSAGE_ITEMS {
        text.push_str(&format!("; and {} more", notes.len() - MESSAGE_ITEMS));
    }
    Some(text)
}

/// `'text'` for a POSIX shell.
pub(super) fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The script of the *Setup* session: what was copied, then each command echoed and run, the first
/// failure ending the session with that command's exit code.
pub(super) fn setup_script(copied: &[String], commands: &[String]) -> String {
    let mut script = String::new();
    if !copied.is_empty() {
        script.push_str(&format!(
            "printf '%s\\n' {}\n",
            sh_quote(&format!(
                "Copied {} gitignored file{} from the main checkout: {}",
                copied.len(),
                if copied.len() == 1 { "" } else { "s" },
                copied.join(", ")
            ))
        ));
    }
    for command in commands {
        let quoted = sh_quote(command);
        script.push_str(&format!(
            "printf '\\033[1m$ %s\\033[0m\\n' {quoted}\n\
             /bin/sh -c {quoted}\n\
             status=$?\n\
             if [ \"$status\" -ne 0 ]; then\n\
             \x20 printf '\\nSetup failed: %s exited with %s\\n' {quoted} \"$status\"\n\
             \x20 exit \"$status\"\n\
             fi\n"
        ));
    }
    script.push_str("printf '\\nSetup finished.\\n'\n");
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_single_quotes_and_the_script_stops_at_the_first_failure() {
        assert_eq!(sh_quote("echo 'hi'"), r"'echo '\''hi'\'''");
        let script = setup_script(&[".env".into()], &["a".into(), "b".into()]);
        assert!(script.starts_with("printf '%s\\n' 'Copied 1 gitignored file from"));
        assert_eq!(script.matches("exit \"$status\"").count(), 2);
        let ok = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(setup_script(
                &[],
                &["true".into(), "exit 3".into(), "true".into()],
            ))
            .output()
            .unwrap();
        assert_eq!(ok.status.code(), Some(3));
        let out = String::from_utf8_lossy(&ok.stdout);
        assert!(out.contains("Setup failed: exit 3 exited with 3"), "{out}");
        assert!(!out.contains("Setup finished"), "{out}");
    }

    #[test]
    fn notes_are_one_line_with_the_first_few_listed() {
        assert_eq!(summarize(&[]), None);
        let notes: Vec<String> = (0..9).map(|i| format!("n{i}")).collect();
        let text = summarize(&notes).unwrap();
        assert!(
            text.starts_with("n0; n1;") && text.ends_with("and 3 more"),
            "{text}"
        );
    }
}
