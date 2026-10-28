//! The folders of a session as the desktop says them (protocol 0.57, capability `session.dirs`,
//! ADR 0023 decision 4; `docs/agents.md` § Folders in a session): which sessions can hold
//! folders, the words for how far each one has been taken (`applied`), the one line that says
//! how the harness takes a folder, and when the honest next step is a restart.
//!
//! The words come from what the daemon knows (`ChatDir.applied`), never from what was asked:
//! a Claude Code chat says *needs a restart* until it restarted, a terminal agent that was
//! typed `/add-dir …` says *typed at the prompt — press Return*.

use std::path::{Path, PathBuf};

use hutan_protocol::chat::{ChatDir, ChatDirApplied, ChatDirRole};
use hutan_protocol::messages::SessionInfo;

/// What holds the folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// A chat run by a harness: `omp`, `claude` or `codex` (`ChatInfo.agent`).
    Chat(String),
    /// A terminal session; `Some(app)` while a coding agent runs in it (`omp`, `claude`,
    /// `codex`, …).
    Terminal(Option<String>),
}

/// One extra folder of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    pub applied: ChatDirApplied,
}

impl Folder {
    /// The last component, which names the folder in a row.
    pub fn name(&self) -> String {
        name_of(&self.path)
    }
}

/// The folders of one session: its own, then the extra ones in the order they were added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFolders {
    pub holder: Holder,
    /// The agent (or the program) runs now: a folder that is not applied is a restart away.
    pub live: bool,
    /// The session runs inside Hutan's Seatbelt boundary, which is fixed when the process
    /// starts (Contain, protocol 0.52).
    pub contained: bool,
    /// A restart of the session gives the agent its folders: a chat always; a terminal when it
    /// is contained or its own program is the agent (the daemon starts `claude`, `codex` and
    /// `omp` with `--add-dir`). An agent started by hand in a shell has to be started again by
    /// hand.
    pub restartable: bool,
    /// The session's own working directory; `None` while a terminal has none.
    pub primary: Option<PathBuf>,
    pub extras: Vec<Folder>,
}

/// The last component of `path`, else the whole path.
pub fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The folders of `info`; `None` for a session that has none to give: a direct chat or an
/// assistant (no tools), a chat of a daemon older than protocol 0.57 (`ChatInfo.dirs` empty).
pub fn folders_of(info: &SessionInfo) -> Option<SessionFolders> {
    let live = info.state.is_live();
    let contained = info.containment.is_some();
    match &info.chat {
        Some(chat) => {
            if chat.is_assistant() || chat.agent == "direct" || chat.dirs.is_empty() {
                return None;
            }
            let primary = chat
                .dirs
                .iter()
                .find(|dir| dir.role == ChatDirRole::Primary)
                .map(|dir| dir.path.clone());
            Some(SessionFolders {
                holder: Holder::Chat(chat.agent.clone()),
                live,
                contained,
                restartable: true,
                primary,
                extras: extras_of(&chat.dirs),
            })
        }
        None => Some(SessionFolders {
            holder: Holder::Terminal(info.terminal_agent.as_ref().map(|a| a.app.clone())),
            live,
            contained,
            restartable: contained || program_is_agent(&info.command),
            primary: info.cwd.clone(),
            extras: extras_of(&info.extra_dirs),
        }),
    }
}

/// The session's own program is one the daemon starts with `--add-dir` (`claude`, `codex`,
/// `omp`).
fn program_is_agent(command: &[String]) -> bool {
    command
        .first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| ["claude", "codex", "omp"].contains(&name))
}

fn extras_of(dirs: &[ChatDir]) -> Vec<Folder> {
    dirs.iter()
        .filter(|dir| dir.role == ChatDirRole::Extra)
        .map(|dir| Folder {
            path: dir.path.clone(),
            applied: dir.applied,
        })
        .collect()
}

impl SessionFolders {
    /// The folder chip of a header: shown for a chat that can hold folders, a coding agent in a
    /// terminal, and any session that already holds some. A plain shell has no chip (the
    /// session menu's *Folders…* still opens the list).
    pub fn has_chip(&self) -> bool {
        match &self.holder {
            Holder::Chat(_) => true,
            Holder::Terminal(agent) => agent.is_some() || !self.extras.is_empty(),
        }
    }

    /// The chip's text: the working folder's name, without the count.
    pub fn chip_name(&self) -> String {
        self.primary
            .as_deref()
            .map_or_else(|| "Folders".to_owned(), name_of)
    }

    /// The `+N` of the chip; `None` without extra folders.
    pub fn chip_count(&self) -> Option<String> {
        (!self.extras.is_empty()).then(|| format!("+{}", self.extras.len()))
    }

    /// The chip's tooltip: every folder, the working one first.
    pub fn chip_tooltip(&self) -> String {
        let mut lines: Vec<String> = self
            .primary
            .iter()
            .map(|path| format!("{} (working folder)", path.display()))
            .collect();
        lines.extend(
            self.extras
                .iter()
                .map(|folder| format!("{} ({})", folder.path.display(), self.words(folder))),
        );
        if lines.is_empty() {
            "Folders of this session".to_owned()
        } else {
            lines.join("\n")
        }
    }

    /// How far the agent has taken `folder`, in words.
    pub fn words(&self, folder: &Folder) -> &'static str {
        match folder.applied {
            ChatDirApplied::Now => "applied now",
            ChatDirApplied::NextTurn => "applies from the next turn",
            ChatDirApplied::Restart if !self.live => "given when the session starts",
            ChatDirApplied::Restart if !self.restartable => {
                "read when the agent starts: start it again with --add-dir"
            }
            ChatDirApplied::Restart => "needs a restart",
            ChatDirApplied::Paste => "typed at the prompt: press Return there to confirm",
            ChatDirApplied::Unsupported => "this agent cannot use it",
        }
    }

    /// Whether `folder` waits for something the user does: a restart or a Return.
    pub fn waits(&self, folder: &Folder) -> bool {
        match folder.applied {
            ChatDirApplied::Now => false,
            ChatDirApplied::NextTurn | ChatDirApplied::Paste | ChatDirApplied::Unsupported => true,
            ChatDirApplied::Restart => self.live,
        }
    }

    /// The popover's last line: how this harness takes a folder (the spike's mock).
    pub fn how(&self) -> String {
        let contain =
            "Contain fixes its boundary when the process starts, so a folder needs a restart.";
        match &self.holder {
            Holder::Chat(agent) => match agent.as_str() {
                "claude" => "Claude Code takes folders when its session opens, so a folder added now needs a restart.".to_owned(),
                "omp" if self.contained => format!("OMP runs inside Contain: {contain}"),
                "omp" => "OMP is given /add-dir, a local command: applied now, no model call, not in the transcript.".to_owned(),
                "codex" => "Codex gets the folders with its next turn, as writable roots of its sandbox.".to_owned(),
                other => format!("{other} takes folders when it starts."),
            },
            Holder::Terminal(_) if self.contained => format!("This terminal runs inside Contain: {contain}"),
            Holder::Terminal(Some(app)) => match app.as_str() {
                "omp" | "claude" => "Hutan types /add-dir <folder> at the prompt of the running agent; press Return in the terminal to confirm.".to_owned(),
                "codex" => "Codex's terminal reads --add-dir when it starts, so a folder added now needs a restart.".to_owned(),
                other => format!("{other} takes folders when it starts, so a folder added now needs a restart."),
            },
            Holder::Terminal(None) => "No coding agent runs here: the folder is open in Files and the file service for this session.".to_owned(),
        }
    }

    /// The extra folders only a restart gives to the running agent.
    pub fn restart_folders(&self) -> Vec<&Folder> {
        if !self.live || !self.restartable {
            return Vec::new();
        }
        self.extras
            .iter()
            .filter(|folder| folder.applied == ChatDirApplied::Restart)
            .collect()
    }

    /// *Restart with this folder* is offered: a live session holds a folder that only a restart
    /// gives it.
    pub fn needs_restart(&self) -> bool {
        !self.restart_folders().is_empty()
    }

    /// The paths of the extra folders, in order (what the Files panel adds as roots).
    pub fn extra_paths(&self) -> Vec<PathBuf> {
        self.extras.iter().map(|f| f.path.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders(holder: Holder, live: bool, applied: &[ChatDirApplied]) -> SessionFolders {
        SessionFolders {
            holder,
            live,
            contained: false,
            restartable: true,
            primary: Some(PathBuf::from("/work/app")),
            extras: applied
                .iter()
                .enumerate()
                .map(|(n, applied)| Folder {
                    path: PathBuf::from(format!("/work/lib{n}")),
                    applied: *applied,
                })
                .collect(),
        }
    }

    #[test]
    fn each_applied_state_has_words_and_a_restart_is_only_asked_of_a_live_session() {
        let live = folders(
            Holder::Chat("claude".into()),
            true,
            &[
                ChatDirApplied::Now,
                ChatDirApplied::NextTurn,
                ChatDirApplied::Restart,
                ChatDirApplied::Paste,
                ChatDirApplied::Unsupported,
            ],
        );
        let words: Vec<&str> = live.extras.iter().map(|f| live.words(f)).collect();
        assert_eq!(
            words,
            [
                "applied now",
                "applies from the next turn",
                "needs a restart",
                "typed at the prompt: press Return there to confirm",
                "this agent cannot use it",
            ]
        );
        assert_eq!(
            live.extras
                .iter()
                .map(|f| live.waits(f))
                .collect::<Vec<_>>(),
            [false, true, true, true, true]
        );
        assert!(live.needs_restart());
        assert_eq!(live.restart_folders().len(), 1);

        // Not running: every folder is given at the next start; there is nothing to restart.
        let stopped = folders(
            Holder::Chat("claude".into()),
            false,
            &[ChatDirApplied::Restart],
        );
        assert_eq!(
            stopped.words(&stopped.extras[0]),
            "given when the session starts"
        );
        assert!(!stopped.waits(&stopped.extras[0]));
        assert!(!stopped.needs_restart());
    }

    #[test]
    fn the_chip_names_the_working_folder_and_counts_the_extra_ones() {
        let mut one = folders(Holder::Chat("omp".into()), true, &[]);
        assert_eq!(one.chip_name(), "app");
        assert_eq!(one.chip_count(), None);
        assert!(one.has_chip());
        one.extras.push(Folder {
            path: PathBuf::from("/work/docs"),
            applied: ChatDirApplied::Now,
        });
        assert_eq!(one.chip_count().as_deref(), Some("+1"));
        assert!(one.chip_tooltip().contains("/work/app (working folder)"));
        assert!(one.chip_tooltip().contains("/work/docs (applied now)"));
    }

    #[test]
    fn a_plain_shell_has_a_chip_only_once_it_holds_a_folder() {
        let shell = folders(Holder::Terminal(None), true, &[]);
        assert!(!shell.has_chip());
        let with = folders(Holder::Terminal(None), true, &[ChatDirApplied::Now]);
        assert!(with.has_chip());
        let agent = folders(Holder::Terminal(Some("omp".into())), true, &[]);
        assert!(agent.has_chip());
    }

    #[test]
    fn the_harness_line_says_how_each_one_takes_a_folder() {
        let how = |holder, contained| {
            let mut f = folders(holder, true, &[]);
            f.contained = contained;
            f.how()
        };
        assert!(how(Holder::Chat("claude".into()), false).contains("needs a restart"));
        assert!(how(Holder::Chat("omp".into()), false).contains("/add-dir"));
        assert!(how(Holder::Chat("omp".into()), true).contains("Contain"));
        assert!(how(Holder::Chat("codex".into()), false).contains("next turn"));
        assert!(how(Holder::Terminal(Some("omp".into())), false).contains("press Return"));
        assert!(how(Holder::Terminal(Some("codex".into())), false).contains("--add-dir"));
        assert!(how(Holder::Terminal(Some("claude".into())), true).contains("Contain"));
    }
}
