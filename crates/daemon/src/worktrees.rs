//! Git worktrees (protocol 0.14, capability `worktree`): `worktree.create`, `.list`, `.remove`;
//! review and merge (protocol 0.21, capability `worktree.review`) in [`review`]; the user's
//! Finish (protocol 0.35, capability `finish`, ADR 0020) in [`finish`].
//!
//! git is the source of truth. Worktrees are discovered with `git worktree list --porcelain -z`
//! in the main checkout of every repository the workspace's roots belong to; Hutan remembers
//! only which of them it created (`hutan-storage`, table `worktrees`) so a client can offer
//! cleanup. Every git call runs without a terminal prompt and without optional locks, under a
//! timeout; none of them touches the network except Finish's fetch and push.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{Event, SessionInfo, SessionListFilter, WorkspaceInfo};
use hutan_protocol::worktree::{
    WorktreeCreate, WorktreeCreated, WorktreeInfo, WorktreeList, WorktreeListRequest,
    WorktreeRemove, WorktreeRemoved,
};
use hutan_storage::{Storage, WorktreeRow};
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, warn};

use crate::bus::EventBus;
use crate::checkpoints::turns::TurnCheckpoints;
use crate::config::{FinishSettings, WorktreeSettings};
use crate::harness::Harnesses;
use crate::placement;
use crate::projects;
use crate::registry::{RegistryHandle, log_write, now_ms};

mod cleanup;
mod finish;
mod history;
mod init;
mod review;
mod setup;
pub mod setup_file;
mod staging;

/// Reads (list, status, rev-parse) finish well within this, even in a large repository.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// `worktree add` checks a whole tree out and `worktree remove` deletes one.
const WRITE_TIMEOUT: Duration = Duration::from_secs(180);

/// A merge or rebase rewrites the files of a checkout (and runs no hooks).
const WORKTREE_MERGE_TIMEOUT: Duration = Duration::from_secs(180);

/// A worktree `worktree.remove` took away.
#[derive(Debug, Clone)]
pub struct RemovedWorktree {
    /// The workspaces of its repository.
    pub workspaces: Vec<WorkspaceId>,
    /// The branch it had checked out.
    pub branch: Option<String>,
}

/// The settings the worktree service starts with.
#[derive(Debug, Clone, Default)]
pub struct WorktreeConfig {
    /// `[finish]`.
    pub finish: FinishSettings,
    /// `[worktrees]`.
    pub worktrees: WorktreeSettings,
}

/// The worktree service: git on one side, the registry (workspaces, sessions) and storage on
/// the other.
#[derive(Debug)]
pub struct WorktreeService {
    registry: RegistryHandle,
    storage: Storage,
    bus: Arc<EventBus>,
    /// Creating and removing worktrees one at a time keeps the checks and the git call that
    /// follows them consistent.
    writes: Mutex<()>,
    /// The login environment git's push and fetch run in (credential helpers, SSH agent).
    harnesses: Arc<Harnesses>,
    /// `[finish] push`: whether a Finish that does not say pushes.
    finish_push: AtomicBool,
    /// The user's home directory: `project.git_init` refuses it.
    user_home: Option<PathBuf>,
    /// `[checkpoints]` as it is now: the file limit of `project.git_init`.
    checkpoints: Arc<TurnCheckpoints>,
    /// `[worktrees]` as it is now: cleanup policy, waiting for setup, the user's per-project
    /// setup.
    settings: parking_lot::RwLock<WorktreeSettings>,
    /// The state of the setups this daemon ran ([`setup`]).
    setups: setup::Setups,
}

impl WorktreeService {
    pub fn new(
        registry: RegistryHandle,
        storage: Storage,
        bus: Arc<EventBus>,
        harnesses: Arc<Harnesses>,
        config: WorktreeConfig,
        user_home: Option<PathBuf>,
        checkpoints: Arc<TurnCheckpoints>,
    ) -> Arc<Self> {
        Arc::new(Self {
            registry,
            storage,
            bus,
            writes: Mutex::new(()),
            harnesses,
            finish_push: AtomicBool::new(config.finish.push),
            user_home,
            checkpoints,
            settings: parking_lot::RwLock::new(config.worktrees),
            setups: setup::Setups::default(),
        })
    }

    /// `[worktrees]` as the daemon uses it now.
    pub fn settings(&self) -> WorktreeSettings {
        self.settings.read().clone()
    }

    /// Takes a new `[worktrees]` (a live setting).
    pub fn set_settings(&self, settings: WorktreeSettings) {
        *self.settings.write() = settings;
    }

    /// `[finish] push` as the daemon uses it now.
    pub fn finish_push(&self) -> bool {
        self.finish_push.load(Ordering::Relaxed)
    }

    /// Takes a new `[finish] push` (a live setting).
    pub fn set_finish_push(&self, push: bool) {
        self.finish_push.store(push, Ordering::Relaxed);
    }

    pub async fn create(&self, req: WorktreeCreate) -> Result<WorktreeCreated, ErrorBody> {
        let writing = self.writes.lock().await;
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let workspace = workspaces
            .iter()
            .find(|w| w.id == req.workspace_id)
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("workspace {} not found", req.workspace_id),
                )
            })?;
        if let Some(path) = &req.path
            && !path.is_absolute()
        {
            return Err(bad_request(format!(
                "`{}` is not an absolute path",
                path.display()
            )));
        }
        if let Some(base) = &req.base
            && (base.is_empty() || base.starts_with('-'))
        {
            return Err(bad_request(format!("`{base}` is not a valid base ref")));
        }
        let main = main_checkout(workspace, req.repo.clone()).await?;
        validate_branch(&main, &req.branch).await?;

        let existing = list_raw(&main).await?;
        if let Some(holder) = existing
            .iter()
            .find(|w| w.branch.as_deref() == Some(req.branch.as_str()))
        {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                format!(
                    "branch `{}` is already checked out at {}",
                    req.branch,
                    holder.path.display()
                ),
            ));
        }
        let path = req
            .path
            .clone()
            .unwrap_or_else(|| default_path(&main, &req.branch));
        if path.symlink_metadata().is_ok() {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                format!("{} already exists", path.display()),
            ));
        }

        let base = req.base.as_deref().unwrap_or("HEAD");
        let branch_exists = git_ok(
            &main,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", req.branch),
            ],
        )
        .await?;
        let mut args: Vec<&OsStr> = vec![OsStr::new("worktree"), OsStr::new("add")];
        if branch_exists {
            // `base` only says where a new branch starts.
            args.extend([path.as_os_str(), OsStr::new(&req.branch)]);
        } else {
            if !git_ok(
                &main,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{base}^{{commit}}"),
                ],
            )
            .await?
            {
                return Err(bad_request(format!("`{base}` is not a commit")));
            }
            args.extend([
                OsStr::new("-b"),
                OsStr::new(&req.branch),
                path.as_os_str(),
                OsStr::new(base),
            ]);
        }
        let added = run_git(&main, &args, WRITE_TIMEOUT).await?;
        if !added.success {
            return Err(git_failed("worktree add", &added));
        }

        let canonical = std::fs::canonicalize(&path).unwrap_or(path);
        log_write(
            self.storage.worktrees().upsert(WorktreeRow {
                path: canonical.clone(),
                workspace_id: req.workspace_id,
                repo_root: main.clone(),
                branch: req.branch.clone(),
                base: base.to_owned(),
                created_at_ms: now_ms(),
            }),
            "a worktree",
        );
        self.registry.invalidate_placements().await?;
        let mut announce = workspaces_of_repo(&workspaces, &main).await;
        if !announce.contains(&req.workspace_id) {
            announce.push(req.workspace_id);
        }
        self.changed(&announce);
        // Copying files and starting the Setup session do not need the write lock.
        drop(writing);
        if !req.no_setup {
            self.begin_setup(workspace, &main, &canonical).await;
        }

        let worktree = self
            .describe_one(&main, &canonical)
            .await
            .map_err(|error| {
                ErrorBody::new(
                    ErrorCode::Internal,
                    format!(
                        "the new worktree {}: {}",
                        canonical.display(),
                        error.message
                    ),
                )
            })?;
        Ok(WorktreeCreated { worktree })
    }

    pub async fn list(&self, req: WorktreeListRequest) -> Result<WorktreeList, ErrorBody> {
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let selected: Vec<&WorkspaceInfo> = match req.workspace_id {
            Some(id) => vec![workspaces.iter().find(|w| w.id == id).ok_or_else(|| {
                ErrorBody::new(ErrorCode::NotFound, format!("workspace {id} not found"))
            })?],
            None => workspaces.iter().collect(),
        };
        let mut repos: Vec<PathBuf> = Vec::new();
        for workspace in selected {
            for main in repos_of(workspace).await {
                if !repos.contains(&main) {
                    repos.push(main);
                }
            }
        }
        let sessions = self.live_sessions().await?;
        let stored = self.stored_paths().await?;
        let mut worktrees = Vec::new();
        for main in repos {
            match list_raw(&main).await {
                Ok(raw) => worktrees.extend(describe(&main, raw, &stored, &sessions).await),
                // One broken repository (deleted, unreadable) must not hide the others.
                Err(error) => {
                    warn!(repo = %main.display(), message = %error.message, "cannot list worktrees")
                }
            }
        }
        self.annotate(&mut worktrees);
        Ok(WorktreeList { worktrees })
    }

    /// Removes a worktree; besides the result, which workspaces and branch it was (the task
    /// service marks the work of a task in it `discarded`).
    pub async fn remove(
        &self,
        req: WorktreeRemove,
    ) -> Result<(WorktreeRemoved, RemovedWorktree), ErrorBody> {
        let _writing = self.writes.lock().await;
        if !req.path.is_absolute() {
            return Err(bad_request(format!(
                "`{}` is not an absolute path",
                req.path.display()
            )));
        }
        let wanted = std::fs::canonicalize(&req.path).unwrap_or_else(|_| req.path.clone());
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let (main, entry) = self.find(&workspaces, &wanted, &req.path).await?;

        if entry.is_main {
            return Err(invalid_state(format!(
                "{} is the main worktree of {}; it cannot be removed",
                entry.path.display(),
                main.display()
            )));
        }
        let sessions = self.live_sessions().await?;
        let inside = sessions_inside(&sessions, &entry.path);
        if !inside.is_empty() {
            let titles: Vec<String> = sessions
                .iter()
                .filter(|s| inside.contains(&s.id))
                .map(|s| format!("{} ({})", s.title, s.id))
                .collect();
            return Err(invalid_state(format!(
                "{} live session(s) work in {}: {}; stop them first",
                inside.len(),
                entry.path.display(),
                titles.join(", ")
            )));
        }
        if entry.locked {
            return Err(invalid_state(format!(
                "{} is locked; unlock it with `git worktree unlock` first",
                entry.path.display()
            )));
        }
        if !entry.prunable && !req.force && dirty_strict(&entry.path).await? {
            return Err(invalid_state(format!(
                "{} has uncommitted changes; pass force to remove it anyway",
                entry.path.display()
            )));
        }

        if !entry.prunable
            && let Err(failure) = self.run_teardown(&workspaces, &main, &entry.path).await
            && !req.force
        {
            return Err(invalid_state(format!(
                "{failure}; the worktree was not removed (pass force to remove it anyway)"
            )));
        }

        let mut args: Vec<&OsStr> = vec![OsStr::new("worktree"), OsStr::new("remove")];
        // A directory that is gone has nothing to lose, but git validates it unless forced.
        if req.force || entry.prunable {
            args.push(OsStr::new("--force"));
        }
        args.push(entry.path.as_os_str());
        let removed = run_git(&main, &args, WRITE_TIMEOUT).await?;
        if !removed.success {
            return Err(git_failed("worktree remove", &removed));
        }

        let mut result = WorktreeRemoved::default();
        if req.delete_branch
            && let Some(branch) = entry.branch.as_deref()
        {
            // `-d`, never `-D`: an unmerged branch stays and is reported.
            let deleted = run_git(
                &main,
                &[
                    OsStr::new("branch"),
                    OsStr::new("-d"),
                    OsStr::new("--"),
                    OsStr::new(branch),
                ],
                READ_TIMEOUT,
            )
            .await?;
            if !deleted.success {
                result.branch_kept = Some(deleted.message());
            }
        }
        log_write(
            self.storage.worktrees().delete(entry.path.clone()),
            "a worktree",
        );
        self.forget_setup(&entry.path);
        self.registry.invalidate_placements().await?;
        let affected = workspaces_of_repo(&workspaces, &main).await;
        self.changed(&affected);
        Ok((
            result,
            RemovedWorktree {
                workspaces: affected,
                branch: entry.branch.clone(),
            },
        ))
    }

    /// The main checkouts of every repository a workspace or a remembered worktree points to.
    async fn searched_repos(
        &self,
        workspaces: &[WorkspaceInfo],
    ) -> Result<Vec<PathBuf>, ErrorBody> {
        let mut repos: Vec<PathBuf> = Vec::new();
        for workspace in workspaces {
            for main in repos_of(workspace).await {
                if !repos.contains(&main) {
                    repos.push(main);
                }
            }
        }
        let storage = self.storage.clone();
        let remembered = tokio::task::spawn_blocking(move || storage.worktrees().list())
            .await
            .map_err(join_failed)?
            .unwrap_or_default();
        for row in remembered {
            if !repos.contains(&row.repo_root) {
                repos.push(row.repo_root);
            }
        }
        Ok(repos)
    }

    /// Looks for the worktree at `wanted` in every repository a workspace or a remembered
    /// worktree points to.
    async fn find(
        &self,
        workspaces: &[WorkspaceInfo],
        wanted: &Path,
        as_given: &Path,
    ) -> Result<(PathBuf, RawWorktree), ErrorBody> {
        for main in self.searched_repos(workspaces).await? {
            let Ok(raw) = list_raw(&main).await else {
                continue;
            };
            if let Some(entry) = raw
                .into_iter()
                .find(|w| w.path == wanted || w.path == as_given)
            {
                return Ok((main, entry));
            }
        }
        Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("git lists no worktree at {}", as_given.display()),
        ))
    }

    /// Like [`Self::find`], for a path anywhere inside a worktree: the innermost listed one
    /// that holds `wanted` or `as_given`.
    async fn find_containing(
        &self,
        workspaces: &[WorkspaceInfo],
        wanted: &Path,
        as_given: &Path,
    ) -> Result<(PathBuf, RawWorktree), ErrorBody> {
        let mut best: Option<(PathBuf, RawWorktree)> = None;
        for main in self.searched_repos(workspaces).await? {
            let Ok(raw) = list_raw(&main).await else {
                continue;
            };
            for entry in raw {
                if entry.prunable
                    || entry.bare
                    || !(wanted.starts_with(&entry.path) || as_given.starts_with(&entry.path))
                {
                    continue;
                }
                let deeper = best.as_ref().is_none_or(|(_, held)| {
                    entry.path.components().count() > held.path.components().count()
                });
                if deeper {
                    best = Some((main.clone(), entry));
                }
            }
        }
        best.ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::NotFound,
                format!("{} is in no git checkout Hutan knows", as_given.display()),
            )
        })
    }

    /// Announces `worktree.changed` for each workspace.
    fn changed(&self, workspaces: &[WorkspaceId]) {
        for workspace_id in workspaces {
            if let Err(error) = self.bus.publish(&Event::WorktreeChanged {
                workspace_id: *workspace_id,
            }) {
                error!(%error, "failed to publish worktree.changed");
            }
        }
    }

    async fn live_sessions(&self) -> Result<Vec<SessionInfo>, ErrorBody> {
        Ok(self
            .registry
            .list_sessions(SessionListFilter::default())
            .await?
            .sessions
            .into_iter()
            .filter(|s| s.state.is_live())
            .collect())
    }

    async fn stored_paths(&self) -> Result<HashSet<PathBuf>, ErrorBody> {
        let storage = self.storage.clone();
        let rows = tokio::task::spawn_blocking(move || storage.worktrees().list())
            .await
            .map_err(join_failed)?
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading worktrees: {e}")))?;
        Ok(rows.into_iter().map(|row| row.path).collect())
    }
}

fn join_failed(error: tokio::task::JoinError) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Internal,
        format!("background task failed: {error}"),
    )
}

fn bad_request(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

// Repositories ------------------------------------------------------------------------------

/// The main checkouts of the git repositories the roots of `workspace` belong to, in root
/// order, without duplicates. Roots outside git (the Home folder) are skipped.
async fn repos_of(workspace: &WorkspaceInfo) -> Vec<PathBuf> {
    let roots = workspace.roots.clone();
    tokio::task::spawn_blocking(move || {
        let mut repos: Vec<PathBuf> = Vec::new();
        for root in roots {
            if let Some(main) = main_checkout_at(&root)
                && !repos.contains(&main)
            {
                repos.push(main);
            }
        }
        repos
    })
    .await
    .unwrap_or_default()
}

/// The main checkout of the repository containing `dir`.
fn main_checkout_at(dir: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(dir).ok()?;
    let toplevel = projects::git_root(&canonical)?;
    placement::main_checkout_of(&toplevel)
}

/// The workspaces having a root in the repository whose main checkout is `main`.
async fn workspaces_of_repo(workspaces: &[WorkspaceInfo], main: &Path) -> Vec<WorkspaceId> {
    let mut found = Vec::new();
    for workspace in workspaces {
        if repos_of(workspace).await.iter().any(|repo| repo == main) {
            found.push(workspace.id);
        }
    }
    found
}

/// The main checkout `worktree.create` works from: the repository containing `repo`, else the
/// first root of the workspace that is in a git repository.
async fn main_checkout(
    workspace: &WorkspaceInfo,
    repo: Option<PathBuf>,
) -> Result<PathBuf, ErrorBody> {
    let roots = workspace.roots.clone();
    let name = workspace.name.clone();
    tokio::task::spawn_blocking(move || match repo {
        Some(dir) => {
            let folder = projects::resolve_folder(&dir)?;
            if projects::git_root(&folder.path).is_none() {
                return Err(bad_request(format!(
                    "{} is not inside a git repository",
                    dir.display()
                )));
            }
            main_checkout_at(&folder.path).ok_or_else(|| {
                bad_request(format!(
                    "the repository of {} has no main checkout (a bare repository?)",
                    dir.display()
                ))
            })
        }
        None => roots
            .iter()
            .find_map(|root| main_checkout_at(root))
            .ok_or_else(|| {
                bad_request(format!(
                    "no root of workspace `{name}` is in a git repository; pass a repository"
                ))
            }),
    })
    .await
    .map_err(join_failed)?
}

/// `<parent of repo>/<repo name>-wt/<branch with / replaced by ->`.
pub fn default_path(main: &Path, branch: &str) -> PathBuf {
    let name = main
        .file_name()
        .map_or_else(|| "repo".into(), |n| n.to_string_lossy().into_owned());
    let parent = main.parent().unwrap_or(main);
    parent
        .join(format!("{name}-wt"))
        .join(branch.replace('/', "-"))
}

/// Cheap checks that need no git, then `git check-ref-format --branch`.
async fn validate_branch(main: &Path, branch: &str) -> Result<(), ErrorBody> {
    if branch.is_empty() || branch.starts_with('-') || branch.contains("@{") || branch == "@" {
        return Err(bad_request(format!(
            "`{branch}` is not a valid branch name"
        )));
    }
    let checked = run_git(
        main,
        &[
            OsStr::new("check-ref-format"),
            OsStr::new("--branch"),
            OsStr::new(branch),
        ],
        READ_TIMEOUT,
    )
    .await?;
    if checked.success {
        Ok(())
    } else {
        Err(bad_request(format!(
            "`{branch}` is not a valid branch name"
        )))
    }
}

// git -----------------------------------------------------------------------------------------

/// What a git process printed.
#[derive(Debug)]
struct GitOutput {
    stdout: Vec<u8>,
    stderr: String,
    success: bool,
}

impl GitOutput {
    /// git's own words: its error output, else its output.
    fn message(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            String::from_utf8_lossy(&self.stdout).into_owned()
        } else {
            self.stderr.clone()
        };
        text.trim().to_owned()
    }
}

fn git_failed(what: &str, output: &GitOutput) -> ErrorBody {
    invalid_state(format!("git {what} failed: {}", output.message()))
}

/// `git args` in `cwd` as a command: no terminal prompt, no optional locks (a status must not
/// rewrite the index under a running agent), no editor or pager, English messages, the caller's
/// `GIT_*` location variables ignored, stdin closed. Killed when dropped.
fn git_command(cwd: &Path, args: &[&OsStr]) -> Command {
    git_command_in(cwd, args, &BTreeMap::new())
}

/// [`git_command`] with `env` (the user's login environment, for a fetch or push that needs the
/// user's credential helper and SSH agent) over the daemon's own; the settings above still win.
fn git_command_in(cwd: &Path, args: &[&OsStr], env: &BTreeMap<String, String>) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(args)
        .envs(env)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_NAMESPACE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// What running git failed with, before it printed anything: not installed, cannot start, or
/// outlived `limit`.
fn git_unavailable(error: &std::io::Error) -> ErrorBody {
    if error.kind() == std::io::ErrorKind::NotFound {
        ErrorBody::new(
            ErrorCode::Internal,
            "git is not installed (no `git` on the daemon's PATH)",
        )
    } else {
        ErrorBody::new(ErrorCode::Internal, format!("could not run git: {error}"))
    }
}

fn git_timed_out(args: &[&OsStr], limit: Duration) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Internal,
        format!(
            "git {} timed out after {}s",
            args.first()
                .map_or_else(String::new, |a| a.to_string_lossy().into_owned()),
            limit.as_secs()
        ),
    )
}

/// Runs `git args` in `cwd` (see [`git_command`]). Killed when it outlives `limit`.
async fn run_git(cwd: &Path, args: &[&OsStr], limit: Duration) -> Result<GitOutput, ErrorBody> {
    run_git_in(cwd, args, &BTreeMap::new(), limit).await
}

/// [`run_git`] in `env` (see [`git_command_in`]).
async fn run_git_in(
    cwd: &Path,
    args: &[&OsStr],
    env: &BTreeMap<String, String>,
    limit: Duration,
) -> Result<GitOutput, ErrorBody> {
    let mut command = git_command_in(cwd, args, env);
    let output = match tokio::time::timeout(limit, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(git_unavailable(&error)),
        Err(_) => return Err(git_timed_out(args, limit)),
    };
    Ok(GitOutput {
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
}

/// Whether `git args` exits 0.
async fn git_ok(cwd: &Path, args: &[&str]) -> Result<bool, ErrorBody> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    Ok(run_git(cwd, &args, READ_TIMEOUT).await?.success)
}

/// The worktrees of the repository whose main checkout is `main`, as git lists them.
async fn list_raw(main: &Path) -> Result<Vec<RawWorktree>, ErrorBody> {
    let listed = run_git(
        main,
        &[
            OsStr::new("worktree"),
            OsStr::new("list"),
            OsStr::new("--porcelain"),
            OsStr::new("-z"),
        ],
        READ_TIMEOUT,
    )
    .await?;
    if !listed.success {
        return Err(git_failed("worktree list", &listed));
    }
    let mut raw = parse_porcelain(&listed.stdout);
    if let Some(first) = raw.first_mut() {
        first.is_main = true;
    }
    for entry in &mut raw {
        // git prints resolved paths already; a missing (prunable) directory stays as it is.
        if let Ok(canonical) = std::fs::canonicalize(&entry.path) {
            entry.path = canonical;
        }
    }
    Ok(raw)
}

/// Whether the worktree at `path` has changes: tracked ones, or untracked files not ignored.
async fn dirty(path: &Path) -> Result<bool, ErrorBody> {
    let status = run_git(
        path,
        &[
            OsStr::new("-c"),
            OsStr::new("core.fsmonitor=false"),
            OsStr::new("status"),
            OsStr::new("--porcelain=v1"),
            OsStr::new("-z"),
            OsStr::new("--untracked-files=normal"),
        ],
        READ_TIMEOUT,
    )
    .await?;
    if !status.success {
        return Err(git_failed("status", &status));
    }
    Ok(!status.stdout.is_empty())
}

/// [`dirty`] for a decision that deletes data: when git cannot say, the answer is an error.
async fn dirty_strict(path: &Path) -> Result<bool, ErrorBody> {
    dirty(path).await.map_err(|error| {
        invalid_state(format!(
            "cannot tell whether {} has uncommitted changes ({}); pass force to remove it anyway",
            path.display(),
            error.message
        ))
    })
}

/// One entry of `git worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawWorktree {
    pub path: PathBuf,
    pub head: String,
    /// The short branch name; `None` when detached (or bare).
    pub branch: Option<String>,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
    /// The first entry of the listing: the repository's main worktree.
    pub is_main: bool,
}

/// Parses `git worktree list --porcelain -z`: attributes end in NUL, a record ends with an
/// empty attribute. Paths may contain any byte but NUL.
pub fn parse_porcelain(output: &[u8]) -> Vec<RawWorktree> {
    let mut records = Vec::new();
    let mut current: Option<RawWorktree> = None;
    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            records.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|b| *b == b' ') {
            Some(at) => (&field[..at], &field[at + 1..]),
            None => (field, &field[field.len()..]),
        };
        if key == b"worktree" {
            // A listing without the separating blank still starts a new record here.
            records.extend(current.take());
            current = Some(RawWorktree {
                path: PathBuf::from(OsStr::from_bytes(value)),
                ..RawWorktree::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key {
            b"HEAD" => entry.head = String::from_utf8_lossy(value).into_owned(),
            b"branch" => {
                let name = String::from_utf8_lossy(value);
                entry.branch = Some(name.strip_prefix("refs/heads/").unwrap_or(&name).to_owned());
            }
            b"bare" => entry.bare = true,
            b"locked" => entry.locked = true,
            b"prunable" => entry.prunable = true,
            // `detached` and attributes of newer git versions need no field.
            _ => {}
        }
    }
    records.extend(current);
    records
}

/// Worktrees as clients see them: their uncommitted changes (asked of git concurrently), the
/// live sessions working in each, and whether Hutan created it.
async fn describe(
    main: &Path,
    raw: Vec<RawWorktree>,
    stored: &HashSet<PathBuf>,
    sessions: &[SessionInfo],
) -> Vec<WorktreeInfo> {
    let states = futures::future::join_all(raw.iter().map(|entry| async move {
        if entry.prunable || entry.bare {
            return false;
        }
        dirty(&entry.path).await.unwrap_or_else(|error| {
            warn!(path = %entry.path.display(), message = %error.message, "cannot tell whether a worktree is dirty");
            false
        })
    }))
    .await;
    raw.into_iter()
        .zip(states)
        .map(|(entry, dirty)| WorktreeInfo {
            live_sessions: sessions_in(sessions, &entry.path),
            created_by_hutan: stored.contains(&entry.path),
            main: entry.is_main,
            repo_root: main.to_path_buf(),
            branch: entry.branch,
            head: entry.head,
            dirty,
            locked: entry.locked,
            prunable: entry.prunable,
            path: entry.path,
            setup: None,
        })
        .collect()
}

/// The sessions whose checkout is exactly `root`.
fn sessions_in(sessions: &[SessionInfo], root: &Path) -> Vec<SessionId> {
    sessions
        .iter()
        .filter(|s| s.placement.as_ref().is_some_and(|p| p.root == root))
        .map(|s| s.id)
        .collect()
}

/// The sessions that would lose their directory when `root` goes: those placed in it, and any
/// whose directory lies below it (a nested repository, a submodule).
fn sessions_inside(sessions: &[SessionInfo], root: &Path) -> Vec<SessionId> {
    sessions
        .iter()
        .filter(|s| {
            s.placement.as_ref().is_some_and(|p| p.root == root)
                || s.cwd.as_deref().is_some_and(|cwd| {
                    cwd.starts_with(root)
                        || std::fs::canonicalize(cwd).is_ok_and(|real| real.starts_with(root))
                })
        })
        .map(|s| s.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(parts: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for part in parts {
            out.extend_from_slice(part.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn porcelain_lists_branches_detached_locked_and_prunable_worktrees() {
        let output = nul(&[
            "worktree /work/repo",
            "HEAD 89e6c98d92887913cadf06b2adb97f26cde4849b",
            "branch refs/heads/master",
            "",
            "worktree /work/repo-wt/feature-x",
            "HEAD 3f786850e387550fdab836ed7e6dc881de23001b",
            "branch refs/heads/feature/x",
            "locked because I said so",
            "",
            "worktree /work/repo-wt/bisect",
            "HEAD 6b3a3b8b5a0b7c8c0f4e4c1f8e6d5c4b3a291807",
            "detached",
            "prunable gitdir file points to non-existent location",
            "",
        ]);
        let parsed = parse_porcelain(&output);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].path, Path::new("/work/repo"));
        assert_eq!(parsed[0].branch.as_deref(), Some("master"));
        assert!(!parsed[0].locked && !parsed[0].prunable);
        assert_eq!(parsed[1].branch.as_deref(), Some("feature/x"));
        assert!(parsed[1].locked);
        assert_eq!(parsed[2].branch, None);
        assert_eq!(parsed[2].head, "6b3a3b8b5a0b7c8c0f4e4c1f8e6d5c4b3a291807");
        assert!(parsed[2].prunable && !parsed[2].locked);
    }

    #[test]
    fn porcelain_keeps_paths_with_spaces_and_newlines_and_a_bare_main() {
        let output = nul(&[
            "worktree /work/my repo.git",
            "bare",
            "",
            "worktree /work/odd\nname/with space",
            "HEAD 3f786850e387550fdab836ed7e6dc881de23001b",
            "branch refs/heads/a_b",
            "locked",
            "",
        ]);
        let parsed = parse_porcelain(&output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, Path::new("/work/my repo.git"));
        assert!(parsed[0].bare);
        assert_eq!(parsed[0].branch, None);
        assert_eq!(parsed[1].path, Path::new("/work/odd\nname/with space"));
        assert_eq!(parsed[1].branch.as_deref(), Some("a_b"));
        assert!(parsed[1].locked, "a lock without a reason is still a lock");
    }

    #[test]
    fn porcelain_tolerates_a_missing_final_separator_and_empty_output() {
        assert!(parse_porcelain(b"").is_empty());
        let output = nul(&["worktree /a", "HEAD abc", "", "worktree /b", "HEAD def"]);
        let parsed = parse_porcelain(&output);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].path, Path::new("/b"));
        // No blank line between the records still separates them.
        let output = nul(&["worktree /a", "HEAD abc", "worktree /b", "HEAD def"]);
        assert_eq!(parse_porcelain(&output).len(), 2);
    }

    #[tokio::test]
    async fn branch_names_are_checked_by_git_and_by_the_rules_git_does_not_apply() {
        let dir = tempfile::tempdir().unwrap();
        for good in ["main", "feature/login", "slice/M7-A", "fix_1.2", "ünï"] {
            validate_branch(dir.path(), good).await.unwrap_or_else(|e| {
                panic!("`{good}` should be valid: {}", e.message);
            });
        }
        for bad in [
            "",
            "-x",
            "--orphan",
            "a..b",
            "a b",
            "x.lock",
            "x/",
            "/x",
            "a~b",
            "a:b",
            "a\\b",
            "@",
            "@{-1}",
            "HEAD",
            "line\nbreak",
        ] {
            let error = validate_branch(dir.path(), bad)
                .await
                .expect_err(&format!("`{bad:?}` should be refused"));
            assert_eq!(error.code, ErrorCode::BadRequest, "{bad:?}");
        }
    }

    #[test]
    fn the_default_path_sits_next_to_the_repository_and_flattens_slashes() {
        assert_eq!(
            default_path(Path::new("/Users/me/proj/hutan"), "slice/M7-A"),
            Path::new("/Users/me/proj/hutan-wt/slice-M7-A")
        );
        assert_eq!(
            default_path(Path::new("/work/my repo"), "fix"),
            Path::new("/work/my repo-wt/fix")
        );
    }
}
