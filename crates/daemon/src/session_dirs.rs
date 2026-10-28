//! Session folders (protocol 0.57, capability `session.dirs`, ADR 0023 decision 4): the checks a
//! folder passes, how a session's list becomes `ChatInfo.dirs` / `SessionInfo.extra_dirs`, and
//! what a terminal program is started with.
//!
//! A session's `cwd` stays its first folder; the extra ones are an ordered list of at most
//! [`SESSION_DIRS_MAX`] canonical, existing directories. Adding one widens what an agent may
//! touch, so the requests are user-only (`router::classify`) and a folder Hutan's boundary
//! would hide — its own data folder, the `[sandbox] read_deny` set — is refused for every
//! session, contained or not.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hutan_core::Contain;
use hutan_protocol::chat::{ChatDir, ChatDirApplied, ChatDirRole, SESSION_DIRS_MAX};
use hutan_protocol::control::{ErrorBody, ErrorCode};

use crate::contain::Sandbox;

fn bad(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::BadRequest, message)
}

/// `dirs` as a creation request's list: each one checked ([`check_one`]), at most
/// [`SESSION_DIRS_MAX`], none twice. `cwd` is the session's own folder (`None`: a terminal
/// without one).
pub(crate) async fn check_list(
    sandbox: &Arc<Sandbox>,
    cwd: Option<&Path>,
    dirs: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, ErrorBody> {
    if dirs.len() > SESSION_DIRS_MAX {
        return Err(bad(format!(
            "at most {SESSION_DIRS_MAX} extra folders per session, {} given",
            dirs.len()
        )));
    }
    let mut checked: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let dir = check_one(sandbox, cwd, &checked, dir).await?;
        checked.push(dir);
    }
    Ok(checked)
}

/// One folder for a session: an absolute, existing directory (made canonical), without control
/// characters in its name (it is typed into agents), not the session's own folder, not one of
/// `existing`, not inside Hutan's data folder or the `[sandbox] read_deny` set. Every refusal is
/// a `bad_request` that says why.
pub(crate) async fn check_one(
    sandbox: &Arc<Sandbox>,
    cwd: Option<&Path>,
    existing: &[PathBuf],
    path: PathBuf,
) -> Result<PathBuf, ErrorBody> {
    let shown = path.display().to_string();
    if !path.is_absolute() {
        return Err(bad(format!("`{shown}` is not an absolute path")));
    }
    if shown.chars().any(char::is_control) {
        return Err(bad(format!(
            "`{}` has a control character in its name",
            shown.escape_default()
        )));
    }
    let cwd = cwd.map(Path::to_path_buf);
    let (real, own) = tokio::task::spawn_blocking(move || {
        let real = std::fs::canonicalize(&path)
            .ok()
            .filter(|real| real.is_dir());
        let own = cwd.map(|cwd| std::fs::canonicalize(&cwd).unwrap_or(cwd));
        (real, own)
    })
    .await
    .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("resolving a folder: {e}")))?;
    let Some(real) = real else {
        return Err(bad(format!("`{shown}` is not an existing directory")));
    };
    if own.as_ref() == Some(&real) {
        return Err(bad(format!(
            "{} is the session's own folder",
            real.display()
        )));
    }
    if existing.contains(&real) {
        return Err(bad(format!(
            "{} is already one of the session's folders",
            real.display()
        )));
    }
    if let Some(reason) = sandbox.refuses_folder(&real) {
        return Err(bad(reason));
    }
    Ok(real)
}

/// The folders of `dirs` that still exist: a folder deleted since it was added is left out of a
/// start (the agent and the boundary are never given a place that is not there).
pub(crate) fn existing(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .filter(|dir| {
            let there = dir.is_dir();
            if !there {
                tracing::warn!(dir = %dir.display(), "a session folder no longer exists; it is left out of the start");
            }
            there
        })
        .cloned()
        .collect()
}

/// `request` with the session's folders among its writable ones: Contain's profile is fixed at
/// spawn, so the folders of the session at that moment are what the process can write.
pub(crate) fn contain_with(request: &Contain, dirs: &[PathBuf]) -> Contain {
    let mut request = request.clone();
    for dir in existing(dirs) {
        if !request.extra_write.contains(&dir) {
            request.extra_write.push(dir);
        }
    }
    request
}

/// `ChatInfo.dirs`: `cwd` first, then the extra folders with `states` (one per folder, in
/// order). Empty for a chat that has no tools to give folders to.
pub(crate) fn chat_dirs(
    cwd: Option<&Path>,
    dirs: &[PathBuf],
    states: &[ChatDirApplied],
) -> Vec<ChatDir> {
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    std::iter::once(ChatDir {
        path: cwd.to_path_buf(),
        role: ChatDirRole::Primary,
        applied: ChatDirApplied::Now,
    })
    .chain(dirs.iter().enumerate().map(|(index, dir)| {
        ChatDir {
            path: dir.clone(),
            role: ChatDirRole::Extra,
            applied: states
                .get(index)
                .copied()
                .unwrap_or(ChatDirApplied::Restart),
        }
    }))
    .collect()
}

/// `SessionInfo.extra_dirs` of a terminal: extras only.
pub(crate) fn extra_dirs(dirs: &[PathBuf], states: &[ChatDirApplied]) -> Vec<ChatDir> {
    dirs.iter()
        .enumerate()
        .map(|(index, dir)| ChatDir {
            path: dir.clone(),
            role: ChatDirRole::Extra,
            applied: states
                .get(index)
                .copied()
                .unwrap_or(ChatDirApplied::Restart),
        })
        .collect()
}

/// The harness a terminal program is, when it takes `--add-dir`: `claude`, `codex` or `omp`
/// (by the name of the program, like `term_agent`).
pub(crate) fn launch_agent(program: &str) -> Option<&'static str> {
    let name = Path::new(program).file_name()?.to_str()?;
    ["claude", "codex", "omp"]
        .into_iter()
        .find(|known| *known == name)
}

/// `argv` started with the session's folders: for `claude`, `codex` and `omp`, one
/// `--add-dir=<path>` each right after the program name (the `=` form: Claude Code's option is
/// variadic and would take the prompt that follows as another folder). Any other program, and
/// an empty list, leave `argv` as it is.
pub(crate) fn with_launch_dirs(argv: &[String], dirs: &[PathBuf]) -> Vec<String> {
    let Some(program) = argv.first() else {
        return argv.to_vec();
    };
    if dirs.is_empty() || launch_agent(program).is_none() {
        return argv.to_vec();
    }
    let mut out = Vec::with_capacity(argv.len() + dirs.len());
    out.push(program.clone());
    out.extend(
        dirs.iter()
            .map(|dir| format!("--add-dir={}", dir.display())),
    );
    out.extend(argv[1..].iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn known_agents_are_started_with_one_add_dir_per_folder_right_after_their_name() {
        let dirs = [PathBuf::from("/a"), PathBuf::from("/b c")];
        assert_eq!(
            with_launch_dirs(&argv(&["claude", "--resume", "x"]), &dirs),
            argv(&["claude", "--add-dir=/a", "--add-dir=/b c", "--resume", "x"])
        );
        assert_eq!(
            with_launch_dirs(&argv(&["/opt/homebrew/bin/omp"]), &dirs[..1]),
            argv(&["/opt/homebrew/bin/omp", "--add-dir=/a"])
        );
        assert_eq!(
            with_launch_dirs(&argv(&["codex", "resume"]), &dirs[..1]),
            argv(&["codex", "--add-dir=/a", "resume"])
        );
    }

    #[test]
    fn other_programs_and_empty_lists_are_left_alone() {
        let dirs = [PathBuf::from("/a")];
        assert_eq!(
            with_launch_dirs(&argv(&["zsh", "-l"]), &dirs),
            argv(&["zsh", "-l"])
        );
        assert_eq!(with_launch_dirs(&argv(&[]), &dirs), argv(&[]));
        assert_eq!(with_launch_dirs(&argv(&["claude"]), &[]), argv(&["claude"]));
        assert_eq!(launch_agent("/usr/local/bin/claude"), Some("claude"));
        assert_eq!(launch_agent("claude-code"), None);
    }

    #[test]
    fn a_chat_lists_its_own_folder_first_and_extras_default_to_restart() {
        let listed = chat_dirs(
            Some(Path::new("/repo")),
            &[PathBuf::from("/a"), PathBuf::from("/b")],
            &[ChatDirApplied::Now],
        );
        assert_eq!(listed[0].role, ChatDirRole::Primary);
        assert_eq!(listed[1].applied, ChatDirApplied::Now);
        assert_eq!(listed[2].applied, ChatDirApplied::Restart);
        assert!(chat_dirs(None, &[], &[]).is_empty());
    }
}
