//! The user's Finish (protocol 0.35, capability `finish`, ADR 0020): `worktree.finish` commits
//! a checkout, merges a worktree's branch into its target in the main checkout, and pushes the
//! branch that holds the work to its upstream, reporting each step.
//!
//! The commit and the merge are `worktree.commit` and `worktree.merge` (same git calls, hooks
//! off, a merge that fails is undone). Every refusal that does not depend on the commit is made
//! before the commit, so a Finish that cannot merge changes nothing.
//!
//! The push is the only step that leaves the machine: `git push --porcelain --no-verify
//! --no-follow-tags --recurse-submodules=no <remote> refs/heads/<branch>:<upstream ref>`, never
//! with `--force` or a `+` refspec, run in the user's login environment so git uses the user's
//! own credential helper and SSH agent (Hutan stores no credential). It runs after a read-only
//! `git fetch` of the upstream, which says how far the branch is ahead and behind: a branch
//! behind its upstream is not pushed (`rejected`; Hutan does not pull, merge or rebase for the
//! user). The write lock is released before the network is touched.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_core::WorkspaceId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::worktree::{
    FinishCommit, FinishMerge, FinishPush, MergeStrategy, StepReason, StepStatus, WorktreeFinish,
    WorktreeFinished, WorktreeMerge, WorktreeMerged, conflict_files,
};

use super::review::{commit_all, git, git_ok_in, has_staged, operation_in_progress};
use super::{GitOutput, READ_TIMEOUT, WorktreeService, bad_request, dirty, invalid_state};
use super::{run_git_in, workspaces_of_repo};

/// The fetch before a push: one ref from one remote.
const FETCH_TIMEOUT: Duration = Duration::from_secs(90);
/// A push of a branch, through the user's network and credential helper.
const PUSH_TIMEOUT: Duration = Duration::from_secs(180);

/// What `worktree.finish` did, the merge it made (the task service marks the work of a task
/// on that branch `merged`) and the checkout it finished (the work of the tasks that ran there
/// is `merged` once the Finish landed: [`Finished::landed`]).
#[derive(Debug)]
pub struct Finished {
    pub result: WorktreeFinished,
    pub merged: Option<(WorktreeMerged, Vec<WorkspaceId>)>,
    /// The checkout's top level, canonical.
    pub checkout: PathBuf,
}

impl Finished {
    /// The commit the work landed in when no step failed: the merge's, else the new commit's,
    /// else what the push sent; `None` when nothing landed or a step failed.
    pub fn landed(&self) -> Option<&str> {
        fn ok(status: StepStatus, commit: Option<&String>) -> Option<&String> {
            (status == StepStatus::Ok).then_some(commit).flatten()
        }
        let done = &self.result;
        let failed = done.commit.status == StepStatus::Failed
            || done
                .merge
                .as_ref()
                .is_some_and(|m| m.status == StepStatus::Failed)
            || done
                .push
                .as_ref()
                .is_some_and(|p| p.status == StepStatus::Failed);
        if failed {
            return None;
        }
        done.merge
            .as_ref()
            .and_then(|m| ok(m.status, m.commit.as_ref()))
            .or_else(|| ok(done.commit.status, done.commit.commit.as_ref()))
            .or_else(|| {
                done.push
                    .as_ref()
                    .and_then(|p| ok(p.status, p.commit.as_ref()))
            })
            .map(String::as_str)
    }
}

impl WorktreeService {
    /// `worktree.finish`.
    pub async fn finish(&self, req: WorktreeFinish) -> Result<Finished, ErrorBody> {
        let writing = self.writes.lock().await;
        let message = req.message.trim().to_owned();
        let (main, entry) = self.checkout(&req.checkout).await?;
        let Some(branch) = entry.branch.clone() else {
            return Err(invalid_state(format!(
                "{} has a detached HEAD; check a branch out there first (`git switch -c <name>`)",
                entry.path.display()
            )));
        };
        if let Some(operation) = operation_in_progress(&entry.path).await? {
            return Err(invalid_state(format!(
                "{operation} is in progress in {}; finish or abort it first",
                entry.path.display()
            )));
        }
        // A worktree: everything a merge would refuse whatever the commit does, before it.
        let target = if entry.is_main {
            None
        } else {
            let (_, target) = self.merge_target(&main, &entry, req.into.clone()).await?;
            if req.strategy == MergeStrategy::Rebase {
                self.refuse_live_sessions(&entry.path).await?;
            }
            Some(target)
        };
        let uncommitted = if req.staged_only {
            has_staged(&entry.path).await?
        } else {
            dirty(&entry.path).await?
        };
        if uncommitted && message.is_empty() {
            return Err(bad_request(format!(
                "{} has uncommitted changes: the commit needs a message",
                entry.path.display()
            )));
        }
        let push_wanted = req.push.unwrap_or_else(|| self.finish_push());

        // 1. Commit.
        let commit = if uncommitted {
            match commit_all(&entry.path, &message, req.staged_only).await {
                Ok((hash, files)) => FinishCommit {
                    status: StepStatus::Ok,
                    reason: None,
                    message: None,
                    branch: branch.clone(),
                    commit: Some(hash),
                    files,
                },
                Err(error) => FinishCommit {
                    status: StepStatus::Failed,
                    reason: Some(StepReason::Error),
                    message: Some(error.message),
                    branch: branch.clone(),
                    commit: None,
                    files: 0,
                },
            }
        } else {
            FinishCommit {
                status: StepStatus::Skipped,
                reason: Some(StepReason::NothingToCommit),
                message: Some(if req.staged_only {
                    format!("nothing is staged in {}", entry.path.display())
                } else {
                    format!("{} has nothing to commit", entry.path.display())
                }),
                branch: branch.clone(),
                commit: None,
                files: 0,
            }
        };
        if commit.status == StepStatus::Ok
            && let Ok(listed) = self.registry.list_workspaces().await
        {
            self.changed(&workspaces_of_repo(&listed.workspaces, &main).await);
        }
        let mut failed = commit.status == StepStatus::Failed;

        // 2. Merge (a worktree).
        let mut merged = None;
        let (merge, push_branch, push_dir) = match target {
            None => (None, branch.clone(), entry.path.clone()),
            Some(target) => {
                let step = if failed {
                    FinishMerge {
                        status: StepStatus::Skipped,
                        reason: Some(StepReason::EarlierStepFailed),
                        message: None,
                        branch: branch.clone(),
                        target: target.clone(),
                        strategy: req.strategy,
                        commit: None,
                        conflicts: Vec::new(),
                        source_dirty: false,
                    }
                } else {
                    let (step, made) = self
                        .finish_merge(&main, &entry.path, &branch, &target, req.strategy, &message)
                        .await;
                    merged = made;
                    step
                };
                failed |= step.status == StepStatus::Failed;
                (Some(step), target, main.clone())
            }
        };
        drop(writing);

        // 3. Push.
        let push = if !push_wanted {
            None
        } else if failed {
            Some(FinishPush {
                status: StepStatus::Skipped,
                reason: Some(StepReason::EarlierStepFailed),
                message: None,
                branch: push_branch,
                upstream: None,
                commit: None,
                ahead: None,
                behind: None,
            })
        } else {
            Some(self.push(&push_dir, &push_branch).await)
        };
        let checkout = std::fs::canonicalize(&entry.path).unwrap_or(entry.path);
        Ok(Finished {
            result: WorktreeFinished {
                commit,
                merge,
                push,
            },
            merged,
            checkout,
        })
    }

    /// The merge step: skipped when the target already has the branch, else `worktree.merge`
    /// (a squash takes the Finish's message; a merge commit keeps git's own).
    async fn finish_merge(
        &self,
        main: &Path,
        worktree: &Path,
        branch: &str,
        target: &str,
        strategy: MergeStrategy,
        message: &str,
    ) -> (FinishMerge, Option<(WorktreeMerged, Vec<WorkspaceId>)>) {
        let mut step = FinishMerge {
            status: StepStatus::Ok,
            reason: None,
            message: None,
            branch: branch.to_owned(),
            target: target.to_owned(),
            strategy,
            commit: None,
            conflicts: Vec::new(),
            source_dirty: false,
        };
        let contained = git_ok_in(
            main,
            &[
                "merge-base",
                "--is-ancestor",
                &format!("refs/heads/{branch}"),
                &format!("refs/heads/{target}"),
            ],
        )
        .await;
        match contained {
            Ok(false) => {}
            Ok(true) => {
                step.status = StepStatus::Skipped;
                step.reason = Some(StepReason::NothingToMerge);
                step.message = Some(format!("`{target}` already contains `{branch}`"));
                return (step, None);
            }
            Err(error) => {
                step.status = StepStatus::Failed;
                step.reason = Some(StepReason::Error);
                step.message = Some(error.message);
                return (step, None);
            }
        }
        let request = WorktreeMerge {
            path: worktree.to_path_buf(),
            into: Some(target.to_owned()),
            strategy,
            message: (strategy == MergeStrategy::Squash && !message.is_empty())
                .then(|| message.to_owned()),
        };
        match self.merge_locked(request).await {
            Ok((done, workspaces)) => {
                step.commit = Some(done.commit.clone());
                step.source_dirty = done.source_dirty;
                (step, Some((done, workspaces)))
            }
            Err(error) => {
                step.status = StepStatus::Failed;
                step.conflicts = conflict_files(&error);
                step.reason = Some(if error.code == ErrorCode::Conflict {
                    StepReason::Conflict
                } else {
                    StepReason::Error
                });
                step.message = Some(error.message);
                (step, None)
            }
        }
    }

    /// The push step for `branch`, checked out in `dir`: never an error, every failure is the
    /// step's.
    async fn push(&self, dir: &Path, branch: &str) -> FinishPush {
        let mut step = FinishPush {
            status: StepStatus::Failed,
            reason: None,
            message: None,
            branch: branch.to_owned(),
            upstream: None,
            commit: None,
            ahead: None,
            behind: None,
        };
        let fail = |mut step: FinishPush, reason: StepReason, message: String| {
            step.status = StepStatus::Failed;
            step.reason = Some(reason);
            step.message = Some(message);
            step
        };

        // A repository without any remote (the Home folder after "Track changes", a scratch
        // repository) has nowhere to push: that is a commit-only Finish, not a failure.
        match git(dir, &["remote"], READ_TIMEOUT).await {
            Ok(out) if out.success && out.stdout.iter().all(u8::is_ascii_whitespace) => {
                step.status = StepStatus::Skipped;
                step.reason = Some(StepReason::NoUpstream);
                step.message = Some(
                    "this repository has no remote, so there is nothing to push; the commit \
                     stays on this machine (commit only)"
                        .to_owned(),
                );
                return step;
            }
            _ => {}
        }

        // The upstream: `branch.<name>.remote` and `.merge`.
        let config = |key: String| async move {
            git(dir, &["config", "--get", &key], READ_TIMEOUT)
                .await
                .ok()
                .filter(|out| out.success)
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let remote = config(format!("branch.{branch}.remote")).await;
        let merge_ref = config(format!("branch.{branch}.merge")).await;
        let (Some(remote), Some(merge_ref)) = (remote, merge_ref) else {
            return fail(
                step,
                StepReason::NoUpstream,
                format!(
                    "`{branch}` has no upstream branch, so there is nowhere to push it; set one \
                     once in a terminal (`git push -u <remote> {branch}`) and Finish again"
                ),
            );
        };
        let upstream_branch = merge_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&merge_ref)
            .to_owned();
        if remote == "." {
            return fail(
                step,
                StepReason::NoUpstream,
                format!(
                    "`{branch}` tracks the local branch `{upstream_branch}`, not a remote; there \
                     is nowhere to push it"
                ),
            );
        }
        let upstream = format!("{remote}/{upstream_branch}");
        step.upstream = Some(upstream.clone());
        if remote.starts_with('-') || merge_ref.starts_with('-') {
            return fail(
                step,
                StepReason::Error,
                format!("the upstream `{upstream}` is not a name git can push to"),
            );
        }

        let env = self.login_env().await;
        // How far the branch is ahead and behind, as the upstream is now (read-only: the fetch
        // writes `FETCH_HEAD` and the remote-tracking ref, nothing the user works on).
        let fetched = run_git_in(
            dir,
            &os(&[
                "fetch",
                "--quiet",
                "--no-tags",
                "--no-recurse-submodules",
                &remote,
                &merge_ref,
            ]),
            &env,
            FETCH_TIMEOUT,
        )
        .await;
        let remote_tip = match fetched {
            Ok(out) if out.success => match fetched_tip(dir).await {
                Ok(tip) => Some(tip),
                Err(error) => return fail(step, StepReason::Error, error.message),
            },
            // The upstream branch does not exist yet: the push creates it.
            Ok(out) if missing_remote_ref(&out) => None,
            Ok(out) => {
                let reason = remote_failure(&out.message());
                return fail(
                    step,
                    reason,
                    format!(
                        "could not fetch {upstream} to compare before pushing: {}",
                        out.message()
                    ),
                );
            }
            Err(error) => {
                return fail(
                    step,
                    StepReason::Error,
                    format!(
                        "could not fetch {upstream} to compare before pushing: {}",
                        error.message
                    ),
                );
            }
        };
        let local_ref = format!("refs/heads/{branch}");
        if let Some(tip) = &remote_tip {
            match counts(dir, &local_ref, tip).await {
                Ok((ahead, behind)) => {
                    step.ahead = Some(ahead);
                    step.behind = Some(behind);
                    if behind > 0 {
                        return fail(
                            step,
                            StepReason::Rejected,
                            format!(
                                "{upstream} has {} that {branch} does not have; bring {} in \
                                 yourself (pull, then merge or rebase) and Finish again — \
                                 Hutan never force-pushes",
                                plural(behind, "commit", "commits"),
                                if behind == 1 { "it" } else { "them" }
                            ),
                        );
                    }
                    if ahead == 0 {
                        step.status = StepStatus::Skipped;
                        step.reason = Some(StepReason::UpToDate);
                        step.message =
                            Some(format!("{upstream} already has every commit of {branch}"));
                        return step;
                    }
                }
                Err(error) => return fail(step, StepReason::Error, error.message),
            }
        }

        let refspec = format!("{local_ref}:{merge_ref}");
        let pushed = run_git_in(
            dir,
            &os(&[
                "push",
                "--porcelain",
                "--no-verify",
                "--no-follow-tags",
                "--recurse-submodules=no",
                &remote,
                &refspec,
            ]),
            &env,
            PUSH_TIMEOUT,
        )
        .await;
        match pushed {
            Ok(out) if out.success => {
                step.status = StepStatus::Ok;
                step.commit = git(dir, &["rev-parse", "--verify", &local_ref], READ_TIMEOUT)
                    .await
                    .ok()
                    .filter(|out| out.success)
                    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned());
                step
            }
            Ok(out) => match rejection(&String::from_utf8_lossy(&out.stdout)) {
                Some(why) => fail(
                    step,
                    StepReason::Rejected,
                    format!("{upstream} refused the push ({why}); nothing was forced"),
                ),
                None => {
                    let reason = remote_failure(&out.message());
                    fail(step, reason, format!("git push failed: {}", out.message()))
                }
            },
            Err(error) => fail(step, StepReason::Error, error.message),
        }
    }

    /// The user's login environment (what a terminal tab gets): their credential helper on
    /// `PATH`, their SSH agent. Empty with `[terminal] env_inherit = "daemon"`: the daemon's own
    /// environment then.
    async fn login_env(&self) -> BTreeMap<String, String> {
        let harnesses = self.harnesses.clone();
        tokio::task::spawn_blocking(move || harnesses.environment())
            .await
            .unwrap_or_default()
    }
}

fn os<'a>(args: &[&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(|arg| OsStr::new(*arg)).collect()
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The commit the fetch just brought: `FETCH_HEAD`, which a fetch of one ref names alone.
async fn fetched_tip(dir: &Path) -> Result<String, ErrorBody> {
    let out = git(
        dir,
        &["rev-parse", "--verify", "--quiet", "FETCH_HEAD^{commit}"],
        READ_TIMEOUT,
    )
    .await?;
    if !out.success {
        return Err(invalid_state(
            "the fetch before pushing left no FETCH_HEAD to compare with",
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Commits of `local` not in `remote`, and of `remote` not in `local`.
async fn counts(dir: &Path, local: &str, remote: &str) -> Result<(u32, u32), ErrorBody> {
    let range = format!("{local}...{remote}");
    let out = git(
        dir,
        &["rev-list", "--left-right", "--count", &range],
        READ_TIMEOUT,
    )
    .await?;
    if !out.success {
        return Err(invalid_state(format!(
            "git rev-list failed: {}",
            out.message()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut numbers = text.split_whitespace().map(str::parse::<u32>);
    match (numbers.next(), numbers.next()) {
        (Some(Ok(ahead)), Some(Ok(behind))) => Ok((ahead, behind)),
        _ => Err(invalid_state(format!(
            "git rev-list printed `{}`",
            text.trim()
        ))),
    }
}

/// The fetch failed because the remote has no such branch (a first push creates it).
fn missing_remote_ref(out: &GitOutput) -> bool {
    out.stderr.contains("couldn't find remote ref")
}

/// What a failed fetch or push says about why: credentials, or something else.
pub(super) fn remote_failure(message: &str) -> StepReason {
    let lower = message.to_ascii_lowercase();
    const AUTH: [&str; 10] = [
        "authentication failed",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "permission denied",
        "host key verification failed",
        "access denied",
        "invalid username or password",
        "returned error: 401",
        "returned error: 403",
    ];
    if AUTH.iter().any(|needle| lower.contains(needle)) {
        StepReason::Auth
    } else {
        StepReason::Error
    }
}

/// The reason of a ref the remote refused, from `git push --porcelain`'s status lines
/// (`!\t<from>:<to>\t[rejected] (non-fast-forward)`).
pub(super) fn rejection(porcelain: &str) -> Option<String> {
    porcelain.lines().find_map(|line| {
        let rest = line.strip_prefix('!')?;
        let summary = rest.split('\t').nth(2).unwrap_or(rest).trim();
        Some(if summary.is_empty() {
            "rejected".to_owned()
        } else {
            summary.to_owned()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_rejections_are_read_with_their_reason() {
        let out = "To /tmp/origin.git\n\
                   !\trefs/heads/main:refs/heads/main\t[rejected] (non-fast-forward)\n\
                   Done\n";
        assert_eq!(
            rejection(out).as_deref(),
            Some("[rejected] (non-fast-forward)")
        );
        let hook = "To /tmp/origin.git\n\
                    !\trefs/heads/main:refs/heads/main\t[remote rejected] (pre-receive hook declined)\n\
                    Done\n";
        assert_eq!(
            rejection(hook).as_deref(),
            Some("[remote rejected] (pre-receive hook declined)")
        );
        let fine = "To /tmp/origin.git\n \trefs/heads/main:refs/heads/main\tabc..def\nDone\n";
        assert_eq!(rejection(fine), None);
    }

    #[test]
    fn credential_failures_are_told_apart_from_the_rest() {
        for message in [
            "fatal: Authentication failed for 'https://github.com/me/x.git/'",
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.",
            "Host key verification failed.",
            "fatal: unable to access 'https://x/': The requested URL returned error: 403",
        ] {
            assert_eq!(remote_failure(message), StepReason::Auth, "{message}");
        }
        for message in [
            "ssh: Could not resolve hostname nowhere.invalid: nodename nor servname provided",
            "fatal: '/tmp/gone.git' does not appear to be a git repository",
            "git push timed out after 180s",
        ] {
            assert_eq!(remote_failure(message), StepReason::Error, "{message}");
        }
    }
}
