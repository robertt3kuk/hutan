//! A worktree for a forked chat (protocol 0.58, capability `chat.fork.worktree`, ADR 0023
//! decision 3, spike `2026-10-28-best-of` §2.4–2.5).
//!
//! The worktree is made like `worktree.create` makes one (a new branch from a base commit, the
//! `[worktrees]` and `.hutan/worktree.toml` rules for copying files, setup and trust), and then
//! holds the files of a turn instead of the base's:
//!
//! 1. `git worktree add -b <branch> <path> <base>`, where `base` is the commit the turn ended
//!    on (its checkpoint's `Hutan-Head`), without the setup;
//! 2. inside the new worktree only, `git read-tree --reset -u <checkpoint>` lays the
//!    checkpoint's tree on the files (modified, deleted and untracked ones alike), and
//!    `git reset --mixed <base>` puts the index back on the base, so the turn's changes are
//!    uncommitted changes of the branch. The source checkout and its index are never touched;
//!    the checkpoint's objects are shared with it through the repository's object store;
//! 3. only then the setup runs (copying ignored files, the commands, the trust question): it
//!    would race with the files being laid otherwise.
//!
//! A step that fails removes what was made: the worktree and its new branch.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hutan_core::WorkspaceId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::worktree::{WorktreeCreate, WorktreeInfo, WorktreeRemove, WorktreeSetup};
use tracing::warn;

use super::{
    READ_TIMEOUT, WRITE_TIMEOUT, WorktreeService, default_path, git_failed, git_ok, invalid_state,
    main_checkout, run_git, validate_branch,
};

/// How many numbered names (`name-2`, `name-3`…) a default branch tries before giving up.
const BRANCH_TRIES: u32 = 50;

/// What a fork needs a worktree for.
#[derive(Debug, Clone)]
pub struct ForkWorktreeSpec {
    /// The workspace the forked chat belongs to.
    pub workspace_id: WorkspaceId,
    /// The checkout the parent chat worked in; its repository is the one to branch.
    pub checkout: PathBuf,
    pub branch: String,
    /// `branch` is the daemon's own choice: a taken name gets a number. A name the user chose
    /// that exists is a `conflict`.
    pub default_branch: bool,
    /// The commit the branch starts from; `None`, or one the repository no longer has: the
    /// checkout's HEAD.
    pub base: Option<String>,
    /// A commit whose tree is laid on the new worktree; `None`: the base as it is.
    pub files: Option<String>,
}

/// A worktree that was made for a fork.
#[derive(Debug, Clone)]
pub struct ForkWorktreeMade {
    /// The repository's main checkout.
    pub main: PathBuf,
    pub worktree: WorktreeInfo,
    pub branch: String,
    /// The commit the branch starts from (full hash).
    pub base: String,
    /// The setup the worktree's configuration asked for; `None` when it asks for none.
    pub setup: Option<WorktreeSetup>,
}

async fn branch_taken(main: &Path, branch: &str) -> Result<bool, ErrorBody> {
    let reference = format!("refs/heads/{branch}");
    git_ok(main, &["show-ref", "--verify", "--quiet", &reference]).await
}

/// The full hash of the commit `name` names in `dir`, if there is one.
async fn commit_of(dir: &Path, name: &str) -> Result<Option<String>, ErrorBody> {
    if name.is_empty() || name.starts_with('-') {
        return Ok(None);
    }
    let spec = format!("{name}^{{commit}}");
    let found = run_git(
        dir,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new(&spec),
        ],
        READ_TIMEOUT,
    )
    .await?;
    Ok(found
        .success
        .then(|| String::from_utf8_lossy(&found.stdout).trim().to_owned())
        .filter(|hash| !hash.is_empty()))
}

impl WorktreeService {
    /// Makes the worktree of a fork (see the module). `conflict` for a branch that exists,
    /// `invalid_state` with git's words when git refuses, `not_found` for an unknown workspace.
    pub async fn create_for_fork(
        &self,
        spec: ForkWorktreeSpec,
    ) -> Result<ForkWorktreeMade, ErrorBody> {
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let workspace = workspaces
            .iter()
            .find(|w| w.id == spec.workspace_id)
            .cloned()
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("workspace {} not found", spec.workspace_id),
                )
            })?;
        let main = main_checkout(&workspace, Some(spec.checkout.clone())).await?;
        validate_branch(&main, &spec.branch).await?;

        // A branch is never taken over: a name that exists is refused, or numbered when it was
        // the daemon's own.
        let branch = if spec.default_branch {
            let mut chosen = None;
            for attempt in 1..=BRANCH_TRIES {
                let candidate = if attempt == 1 {
                    spec.branch.clone()
                } else {
                    format!("{}-{attempt}", spec.branch)
                };
                if !branch_taken(&main, &candidate).await?
                    && default_path(&main, &candidate).symlink_metadata().is_err()
                {
                    chosen = Some(candidate);
                    break;
                }
            }
            chosen.ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::Conflict,
                    format!(
                        "branch `{}` and its numbered variants are taken: name the branch",
                        spec.branch
                    ),
                )
            })?
        } else if branch_taken(&main, &spec.branch).await? {
            return Err(ErrorBody::new(
                ErrorCode::Conflict,
                format!("branch `{}` already exists", spec.branch),
            ));
        } else {
            spec.branch.clone()
        };

        // The commit the turn ended on; the files laid on it do not depend on it, so a commit
        // the repository lost falls back to the checkout's HEAD.
        let recorded = match &spec.base {
            Some(base) => commit_of(&main, base).await?,
            None => None,
        };
        let base = match recorded {
            Some(base) => base,
            None => commit_of(&spec.checkout, "HEAD").await?.ok_or_else(|| {
                invalid_state(format!(
                    "{} has no commit yet: a worktree needs a commit to start from",
                    spec.checkout.display()
                ))
            })?,
        };

        let created = self
            .create(WorktreeCreate {
                workspace_id: spec.workspace_id,
                repo: Some(spec.checkout.clone()),
                branch: branch.clone(),
                base: Some(base.clone()),
                path: None,
                no_setup: true,
            })
            .await?;
        let path = created.worktree.path;

        if let Some(files) = &spec.files
            && let Err(error) = self.lay_files(&path, files, &base).await
        {
            self.discard_fork_worktree(&main, &path, &branch).await;
            return Err(error);
        }

        let setup = self.begin_setup(&workspace, &main, &path).await;
        let worktree = match self.describe_one(&main, &path).await {
            Ok(worktree) => worktree,
            Err(error) => {
                self.discard_fork_worktree(&main, &path, &branch).await;
                return Err(error);
            }
        };
        self.changed(&[spec.workspace_id]);
        Ok(ForkWorktreeMade {
            main,
            worktree,
            branch,
            base,
            setup,
        })
    }

    /// Lays the tree of the commit `files` on the files of the worktree at `path` and leaves
    /// the difference to `base` uncommitted. Hooks never run; nothing outside `path` is
    /// written.
    async fn lay_files(&self, path: &Path, files: &str, base: &str) -> Result<(), ErrorBody> {
        let read = run_git(
            path,
            &[
                OsStr::new("-c"),
                OsStr::new("core.hooksPath=/dev/null"),
                OsStr::new("read-tree"),
                OsStr::new("--reset"),
                OsStr::new("-u"),
                OsStr::new(files),
            ],
            WRITE_TIMEOUT,
        )
        .await?;
        if !read.success {
            return Err(git_failed("read-tree", &read));
        }
        let reset = run_git(
            path,
            &[
                OsStr::new("-c"),
                OsStr::new("core.hooksPath=/dev/null"),
                OsStr::new("reset"),
                OsStr::new("--mixed"),
                OsStr::new("--quiet"),
                OsStr::new(base),
            ],
            WRITE_TIMEOUT,
        )
        .await?;
        if !reset.success {
            return Err(git_failed("reset --mixed", &reset));
        }
        Ok(())
    }

    /// Takes away a worktree this module made (of the repository whose main checkout is `main`)
    /// and the branch it started, when the fork that wanted it did not happen. Returns whether
    /// the worktree is gone: one that cannot be removed (a running Setup session is inside it)
    /// stays and is logged, and the caller says so.
    pub async fn discard_fork_worktree(&self, main: &Path, path: &Path, branch: &str) -> bool {
        let removed = self
            .remove(WorktreeRemove {
                path: path.to_owned(),
                force: true,
                delete_branch: false,
            })
            .await;
        match removed {
            Ok(_) => {
                // Our own branch, made a moment ago and never committed on: `-D` loses nothing.
                let deleted = run_git(
                    main,
                    &[
                        OsStr::new("branch"),
                        OsStr::new("-D"),
                        OsStr::new("--"),
                        OsStr::new(branch),
                    ],
                    READ_TIMEOUT,
                )
                .await;
                if !deleted.as_ref().is_ok_and(|out| out.success) {
                    warn!(branch, "the branch of a discarded fork worktree stayed");
                }
                true
            }
            Err(error) => {
                warn!(path = %path.display(), message = %error.message, "a fork's worktree could not be removed");
                false
            }
        }
    }
}
