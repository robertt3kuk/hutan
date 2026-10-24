//! `project.git_init` (protocol 0.44, capability `project.git_init`): the user's opt-in
//! "Track changes in this folder" for a folder that is not a repository, the Home folder above
//! all (`docs/assistant.md` § Where Mawas works). It writes a `.gitignore` with Hutan's own
//! scratch entries, runs `git init` and commits what is there now with the user's own git
//! identity, so Changes, checkpoints (ADR 0019) and Finish work there like in any repository.
//! Nothing leaves the machine.
//!
//! Refused before anything is written: the user's home directory and `/` (a repository there
//! would swallow every folder below it), a folder that is a repository already or lies inside
//! one (the repository of the outer folder would own its files), a folder with more untracked
//! files than checkpoints can seed (`[checkpoints] max_untracked_files`). A step that fails
//! after `git init` (no identity to commit with, a file git cannot add) takes back what this
//! call made: the new `.git`, and the `.gitignore` as it was.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hutan_core::WorkspaceId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};
use hutan_protocol::project::{ProjectGitInit, ProjectGitInitialized};

use super::review::run_git_capped;
use super::{
    GitOutput, READ_TIMEOUT, WORKTREE_MERGE_TIMEOUT, WorktreeService, bad_request, git_failed,
    invalid_state, join_failed, run_git_in, workspaces_of_repo,
};
use crate::projects;

/// What the initial commit says.
const MESSAGE: &str = "Start tracking changes with Hutan";

/// Lines Hutan adds to the folder's `.gitignore`: what Hutan itself leaves behind while it
/// writes, and the Finder's litter. Never the user's own files.
pub const GITIGNORE_ENTRIES: &[&str] = &[".hutan-write-*.tmp", ".DS_Store"];

/// The header of the block Hutan adds.
const GITIGNORE_HEADER: &str = "# Hutan scratch files";

/// Bytes of `git ls-files` output read when counting files: far more than
/// `max_untracked_files` paths of any sane length; cut short means over the limit.
const LISTING_CAP: usize = 16 * 1024 * 1024;

impl WorktreeService {
    /// `project.git_init`.
    pub async fn git_init(&self, req: ProjectGitInit) -> Result<ProjectGitInitialized, ErrorBody> {
        let _writing = self.writes.lock().await;
        let workspaces = self.registry.list_workspaces().await?.workspaces;
        let root = folder_of(&workspaces, req.project_id)?;
        let user_home = self.user_home.clone();
        let root = tokio::task::spawn_blocking(move || refusals(&root, user_home))
            .await
            .map_err(join_failed)??;
        let limit =
            usize::try_from(self.checkpoints.settings().max_untracked_files).unwrap_or(usize::MAX);
        let env = self.checkpoints.engine().git_env().clone();
        let result = initialise(&root, limit, &env).await?;
        self.registry.invalidate_placements().await?;
        self.changed(&workspaces_of_repo(&workspaces, &root).await);
        Ok(result)
    }
}

/// The folder `project_id` stands for: the Home folder, or a project's root.
fn folder_of(workspaces: &[WorkspaceInfo], id: WorkspaceId) -> Result<PathBuf, ErrorBody> {
    let workspace = workspaces
        .iter()
        .find(|w| w.id == id)
        .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("project {id} not found")))?;
    let root = match workspace.kind {
        WorkspaceKind::Workspace => {
            return Err(invalid_state(format!(
                "`{}` is a workspace, not a folder; name one of its projects",
                workspace.name
            )));
        }
        WorkspaceKind::Home => workspace.roots.first().cloned(),
        WorkspaceKind::Project => workspace
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .or_else(|| workspace.roots.first().cloned()),
    };
    root.ok_or_else(|| invalid_state(format!("`{}` has no folder", workspace.name)))
}

/// The folder (canonical) once nothing refuses it.
fn refusals(folder: &Path, user_home: Option<PathBuf>) -> Result<PathBuf, ErrorBody> {
    let root = fs::canonicalize(folder).map_err(|e| {
        bad_request(format!(
            "cannot use {}: {e} (the folder may be gone)",
            folder.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(bad_request(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let home = user_home.and_then(|home| fs::canonicalize(home).ok());
    if root.parent().is_none() || home.as_deref() == Some(root.as_path()) {
        return Err(invalid_state(format!(
            "{} is {}; Hutan will not put it under version control",
            root.display(),
            if root.parent().is_none() {
                "the root of the disk"
            } else {
                "your home directory"
            }
        )));
    }
    if root.join(".git").symlink_metadata().is_ok() {
        return Err(invalid_state(format!(
            "{} is a git repository already",
            root.display()
        )));
    }
    if let Some(outer) = root.parent().and_then(projects::git_root) {
        return Err(invalid_state(format!(
            "{} lies inside the git repository at {}; its files belong to that repository",
            root.display(),
            outer.display()
        )));
    }
    Ok(root)
}

/// `.gitignore` with Hutan's entries added to what is there; `None` when it needs no change.
fn gitignore_with_entries(existing: Option<&str>) -> Option<String> {
    let have: Vec<&str> = existing
        .map(|text| text.lines().map(str::trim).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = GITIGNORE_ENTRIES
        .iter()
        .copied()
        .filter(|entry| !have.contains(entry))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut text = existing.unwrap_or_default().to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(GITIGNORE_HEADER);
    text.push('\n');
    for entry in missing {
        text.push_str(entry);
        text.push('\n');
    }
    Some(text)
}

/// What a failed run undoes.
struct Undo<'a> {
    root: &'a Path,
    /// `.gitignore` as it was: `None` when this call created it.
    gitignore: Option<Option<Vec<u8>>>,
    git_dir: bool,
}

impl Undo<'_> {
    fn run(self) {
        if self.git_dir {
            let _ = fs::remove_dir_all(self.root.join(".git"));
        }
        if let Some(original) = self.gitignore {
            let path = self.root.join(".gitignore");
            let _ = match original {
                Some(bytes) => fs::write(&path, bytes),
                None => fs::remove_file(&path),
            };
        }
    }
}

async fn initialise(
    root: &Path,
    limit: usize,
    env: &BTreeMap<String, String>,
) -> Result<ProjectGitInitialized, ErrorBody> {
    let mut undo = Undo {
        root,
        gitignore: None,
        git_dir: false,
    };
    match steps(root, limit, env, &mut undo).await {
        Ok(done) => Ok(done),
        Err(error) => {
            undo.run();
            Err(error)
        }
    }
}

async fn steps(
    root: &Path,
    limit: usize,
    env: &BTreeMap<String, String>,
    undo: &mut Undo<'_>,
) -> Result<ProjectGitInitialized, ErrorBody> {
    // `.gitignore` first, so the count and the commit already leave the scratch files out.
    let path = root.join(".gitignore");
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_failed("reading .gitignore", &e)),
    };
    let existing = original
        .as_deref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
    let gitignore_written = match gitignore_with_entries(existing.as_deref()) {
        Some(text) => {
            undo.gitignore = Some(original);
            let mut file =
                fs::File::create(&path).map_err(|e| io_failed("writing .gitignore", &e))?;
            file.write_all(text.as_bytes())
                .map_err(|e| io_failed("writing .gitignore", &e))?;
            true
        }
        None => false,
    };

    let init = git(root, &["init", "--quiet"], env, READ_TIMEOUT).await?;
    if !init.success {
        return Err(git_failed("init", &init));
    }
    undo.git_dir = true;

    // Nested repositories stay out of the commit: adding one would record an embedded
    // repository nobody can restore. Kept in `.git/info/exclude`, the user's `.gitignore`
    // stays theirs.
    let args = [
        OsStr::new("ls-files"),
        OsStr::new("--others"),
        OsStr::new("--exclude-standard"),
        OsStr::new("-z"),
    ];
    let listed = run_git_capped(root, &args, READ_TIMEOUT, LISTING_CAP).await?;
    if listed.code != Some(0) && !listed.truncated {
        return Err(invalid_state(format!(
            "git ls-files failed: {}",
            listed.stderr.trim()
        )));
    }
    let entries: Vec<&[u8]> = listed
        .stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    if listed.truncated || entries.len() > limit {
        return Err(invalid_state(format!(
            "{} has more than {limit} files that git would track; ignore build output and \
             large folders in a .gitignore first, or raise `[checkpoints] max_untracked_files`",
            root.display()
        )));
    }
    let nested: Vec<&[u8]> = entries
        .iter()
        .copied()
        .filter(|entry| entry.ends_with(b"/"))
        .collect();
    if !nested.is_empty() {
        let mut exclude = Vec::new();
        for entry in &nested {
            exclude.push(b'/');
            exclude.extend_from_slice(entry);
            exclude.push(b'\n');
        }
        let info = root.join(".git").join("info");
        fs::create_dir_all(&info).map_err(|e| io_failed("writing .git/info/exclude", &e))?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(info.join("exclude"))
            .map_err(|e| io_failed("writing .git/info/exclude", &e))?;
        file.write_all(&exclude)
            .map_err(|e| io_failed("writing .git/info/exclude", &e))?;
    }

    let staged = git(root, &["add", "--all"], env, WORKTREE_MERGE_TIMEOUT).await?;
    if !staged.success {
        return Err(git_failed("add", &staged));
    }
    let committed = git(
        root,
        &[
            "commit",
            "--no-verify",
            "--allow-empty",
            "--cleanup=strip",
            "-m",
            MESSAGE,
        ],
        env,
        WORKTREE_MERGE_TIMEOUT,
    )
    .await?;
    if !committed.success {
        let message = committed.message();
        return Err(if needs_identity(&message) {
            invalid_state(
                "git does not know who you are, so it cannot make the first commit; run \
                 `git config --global user.name \"Your Name\"` and \
                 `git config --global user.email you@example.com`, then try again",
            )
        } else {
            git_failed("commit", &committed)
        });
    }

    let tip = git(root, &["rev-parse", "--verify", "HEAD"], env, READ_TIMEOUT).await?;
    if !tip.success {
        return Err(git_failed("rev-parse", &tip));
    }
    let branch = git(
        root,
        &["symbolic-ref", "--short", "HEAD"],
        env,
        READ_TIMEOUT,
    )
    .await?;
    let files = git(
        root,
        &["ls-tree", "-r", "--name-only", "-z", "HEAD"],
        env,
        READ_TIMEOUT,
    )
    .await?;
    let files = files
        .stdout
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .count();
    Ok(ProjectGitInitialized {
        root: root.to_owned(),
        branch: String::from_utf8_lossy(&branch.stdout).trim().to_owned(),
        commit: String::from_utf8_lossy(&tip.stdout).trim().to_owned(),
        files: u32::try_from(files).unwrap_or(u32::MAX),
        gitignore_written,
    })
}

/// `git args` in `cwd` with the engine's extra environment (empty outside tests).
async fn git(
    cwd: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    limit: Duration,
) -> Result<GitOutput, ErrorBody> {
    let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
    run_git_in(cwd, &args, env, limit).await
}

/// git's refusal to commit without a name or an email.
fn needs_identity(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("please tell me who you are")
        || lower.contains("empty ident name")
        || lower.contains("unable to auto-detect email address")
}

fn io_failed(what: &str, error: &std::io::Error) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, format!("{what}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_gets_entries_once_and_keeps_the_users_lines() {
        let fresh = gitignore_with_entries(None).unwrap();
        assert_eq!(
            fresh,
            "# Hutan scratch files\n.hutan-write-*.tmp\n.DS_Store\n"
        );
        assert_eq!(gitignore_with_entries(Some(&fresh)), None);

        let theirs = gitignore_with_entries(Some("node_modules\n.DS_Store")).unwrap();
        assert_eq!(
            theirs,
            "node_modules\n.DS_Store\n\n# Hutan scratch files\n.hutan-write-*.tmp\n"
        );
        assert_eq!(gitignore_with_entries(Some(&theirs)), None);
    }

    #[test]
    fn identity_refusals_are_recognised() {
        assert!(needs_identity(
            "Author identity unknown\n\n*** Please tell me who you are.\n"
        ));
        assert!(needs_identity(
            "fatal: empty ident name (for <a@b>) not allowed"
        ));
        assert!(!needs_identity("nothing to commit"));
    }
}
