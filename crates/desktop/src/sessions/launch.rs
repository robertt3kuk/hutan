//! What the Start… launcher sends: the values of its sheet and the `task.create` request they
//! make (`docs/orchestration.md` § The Start… sheet). No GPUI here.
//!
//! Every start is a task: the sheet creates it with [`build_create`] and, for "Start now", starts
//! it with `task.start`; "Queue as task" leaves it to the scheduler (`auto_start`).

use hutan_core::task::{PROMPT_MAX_BYTES, TITLE_MAX_CHARS, slug};
use hutan_core::{
    AfterPolicy, SafetyPreset, TaskId, TaskPlacement, TaskPlacementKind, TaskRun, WorkspaceId,
};
use hutan_protocol::task::TaskCreate;

/// The longest title the sheet proposes from a command line (the daemon allows
/// [`TITLE_MAX_CHARS`]).
const PROPOSED_TITLE_CHARS: usize = 72;

/// The longest title proposed from a chat prompt: a name for a list row and a tab, not the
/// request (the prompt in full is the row's tooltip).
pub const PROMPT_TITLE_CHARS: usize = 40;

/// What the sheet starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaunchKind {
    /// A chat with an agent (OMP, Claude Code, Codex over their protocols, or a direct model).
    Chat,
    /// The agent's own terminal UI in a PTY.
    TerminalAgent,
    /// A login shell.
    Terminal,
    /// One command in a terminal.
    Command,
}

impl LaunchKind {
    pub const ALL: [LaunchKind; 4] = [
        LaunchKind::Chat,
        LaunchKind::TerminalAgent,
        LaunchKind::Terminal,
        LaunchKind::Command,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LaunchKind::Chat => "Agent chat",
            LaunchKind::TerminalAgent => "Terminal agent",
            LaunchKind::Terminal => "Plain terminal",
            LaunchKind::Command => "Command",
        }
    }

    /// The element-id suffix and the name tests click by.
    pub fn key(self) -> &'static str {
        match self {
            LaunchKind::Chat => "chat",
            LaunchKind::TerminalAgent => "tui",
            LaunchKind::Terminal => "terminal",
            LaunchKind::Command => "command",
        }
    }

    /// Whether the sheet asks for a prompt: the first message of a chat, or what a terminal agent
    /// starts with.
    pub fn takes_prompt(self) -> bool {
        matches!(self, LaunchKind::Chat | LaunchKind::TerminalAgent)
    }

    /// Whether the sheet shows what only a task has: its title (the name in the Tasks panel)
    /// and *Start now / Queue as task*. A plain terminal is just opened; it is not described
    /// or queued.
    pub fn has_task_fields(self) -> bool {
        self != LaunchKind::Terminal
    }

    /// Whether an agent writes into the checkout (the shared-checkout warning applies).
    pub fn is_agent(self) -> bool {
        self.takes_prompt()
    }
}

/// Where the started session works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchPlace {
    /// The repository's main checkout (or the project's folder outside git).
    Main,
    /// A worktree made for the task: `branch` from `base` (empty: `HEAD`).
    NewWorktree { branch: String, base: String },
    /// The worktree that already has `branch`.
    Existing { branch: String },
}

/// The values of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchValues {
    pub kind: LaunchKind,
    /// `omp`, `claude` or `codex`: the harness of a chat or terminal agent.
    pub agent: String,
    /// The model of a chat (one the agent offers); none: the agent's own default.
    pub model: Option<String>,
    /// The managed account of the harness (protocol 0.50) the chat runs as; `system` is the
    /// harness's own login; none: the project's default, else the harness's.
    pub account: Option<String>,
    /// The safety preset (protocol 0.51) of a chat, or the flags of a terminal agent; none: the
    /// harness's own settings. Never set for a direct chat, a command or a plain terminal.
    pub safety: Option<SafetyPreset>,
    pub title: String,
    pub prompt: String,
    /// The command of [`LaunchKind::Command`]: shell text, run by [`shell_argv`].
    pub command: String,
    /// The user's login shell, which runs a [`LaunchKind::Command`] (`$SHELL -lc "<command>"`).
    pub shell: String,
    pub place: LaunchPlace,
    pub after: Vec<TaskId>,
    /// "Queue as task": the scheduler starts it by `[tasks] max_parallel`.
    pub queue: bool,
}

/// The title the sheet proposes for `prompt`: its first clause (up to the first sentence end,
/// `;`, `:` or dash), at most [`PROMPT_TITLE_CHARS`] characters, cut at a word with `…` when the
/// clause is longer. Empty while the prompt is.
pub fn title_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let clause = first_clause(line);
    if clause.chars().count() <= PROMPT_TITLE_CHARS {
        return clause.to_owned();
    }
    // The longest run of whole words that leaves room for the ellipsis.
    let room: String = clause.chars().take(PROMPT_TITLE_CHARS - 1).collect();
    let whole = clause
        .chars()
        .nth(PROMPT_TITLE_CHARS - 1)
        .is_some_and(char::is_whitespace);
    let cut = match room.rfind(' ') {
        Some(at) if !whole && at > 0 => at,
        _ => room.len(),
    };
    format!(
        "{}…",
        room[..cut].trim_end_matches([' ', ',', ';', ':', '-', '–', '—'])
    )
}

/// `line` up to its first clause break: `. `, `! `, `? `, `; `, `: `, ` - `, ` – `, ` — `, or
/// the end. A dot inside a name (`src/lib.rs`, `v1.2`) is not a break: a break is followed by a
/// space.
fn first_clause(line: &str) -> &str {
    const BREAKS: [&str; 8] = [". ", "! ", "? ", "; ", ": ", " - ", " – ", " — "];
    let end = BREAKS
        .iter()
        .filter_map(|b| line.find(b))
        .min()
        .unwrap_or(line.len());
    line[..end]
        .trim_end_matches(['.', '!', '?', ';', ':'])
        .trim()
}

/// The title proposed for a command line: its first line, at most [`PROPOSED_TITLE_CHARS`].
pub fn title_from_command(command: &str) -> String {
    let line = command
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut title: String = line.chars().take(PROPOSED_TITLE_CHARS).collect();
    if line.chars().count() > PROPOSED_TITLE_CHARS {
        title.push('…');
    }
    title
}

/// The branch the sheet proposes for a new worktree: `hutan/<slug of the title>` (empty while
/// there is no title).
pub fn branch_for_title(title: &str) -> String {
    if title.trim().is_empty() {
        String::new()
    } else {
        format!("hutan/{}", slug(title))
    }
}

/// The command a terminal agent starts: its binary, the flags of its safety preset
/// ([`hutan_core::safety::terminal_args`]), then `prompt` as the first message when there is one.
pub fn terminal_agent_argv(
    agent: &str,
    prompt: &str,
    safety: Option<SafetyPreset>,
) -> Result<Vec<String>, String> {
    let mut argv = vec![agent.to_owned()];
    if let Some(preset) = safety {
        argv.extend(
            hutan_core::safety::terminal_args(agent, preset)
                .map_err(|reason| format!("{}: {reason}", preset.label()))?,
        );
    }
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        if prompt.starts_with('-') {
            return Err(
                "A prompt for a terminal agent cannot start with `-`: it would read as an option."
                    .to_owned(),
            );
        }
        argv.push(prompt.to_owned());
    }
    Ok(argv)
}

/// The title a task gets when the sheet's title is empty.
fn fallback_title(values: &LaunchValues, agent_name: &str) -> String {
    match values.kind {
        LaunchKind::Chat => {
            let proposed = title_from_prompt(&values.prompt);
            if proposed.is_empty() {
                format!("{agent_name} chat")
            } else {
                proposed
            }
        }
        LaunchKind::TerminalAgent => {
            let proposed = title_from_prompt(&values.prompt);
            if proposed.is_empty() {
                format!("{agent_name} agent")
            } else {
                proposed
            }
        }
        LaunchKind::Terminal => "Terminal".to_owned(),
        LaunchKind::Command => {
            let command = values.command.trim();
            let shown: String = command.chars().take(PROPOSED_TITLE_CHARS).collect();
            if shown.is_empty() {
                "Command".to_owned()
            } else {
                shown
            }
        }
    }
}

/// The `task.create` request for `values` in `workspace`, or what is wrong with them (the daemon
/// checks again). `agent_name` is the harness's name for a title nobody typed.
pub fn build_create(
    workspace: WorkspaceId,
    values: &LaunchValues,
    agent_name: &str,
) -> Result<TaskCreate, String> {
    let title = values.title.trim();
    let title = if title.is_empty() {
        fallback_title(values, agent_name)
    } else {
        title.to_owned()
    };
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(format!(
            "The title is longer than {TITLE_MAX_CHARS} characters."
        ));
    }
    let prompt = values.prompt.trim();
    if values.kind.takes_prompt() && prompt.len() > PROMPT_MAX_BYTES {
        return Err("The prompt is longer than 64 KiB.".to_owned());
    }
    let run = match values.kind {
        LaunchKind::Chat => {
            if prompt.is_empty() {
                return Err(format!(
                    "{agent_name} chat needs a prompt: it is the first message."
                ));
            }
            Some(TaskRun::Chat {
                contain: None,
                agent: values.agent.clone(),
                model: values.model.clone(),
                account: values.account.clone(),
                safety: values.safety,
            })
        }
        LaunchKind::TerminalAgent => Some(TaskRun::Terminal {
            contain: None,
            argv: terminal_agent_argv(&values.agent, prompt, values.safety)?,
        }),
        LaunchKind::Terminal => None,
        LaunchKind::Command => {
            let command = values.command.trim();
            if command.is_empty() {
                return Err("Type the command to run.".to_owned());
            }
            Some(TaskRun::Terminal {
                contain: None,
                argv: shell_argv(&values.shell, command),
            })
        }
    };
    let placement = match &values.place {
        LaunchPlace::Main => None,
        LaunchPlace::NewWorktree { branch, base } => {
            let branch = branch.trim();
            if branch.is_empty() {
                return Err("Name the branch of the new worktree.".to_owned());
            }
            let base = base.trim();
            Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some(branch.to_owned()),
                base: (!base.is_empty()).then(|| base.to_owned()),
            })
        }
        LaunchPlace::Existing { branch } => Some(TaskPlacement {
            kind: TaskPlacementKind::Worktree,
            branch: Some(branch.clone()),
            base: None,
        }),
    };
    Ok(TaskCreate {
        workspace_id: workspace,
        title,
        prompt: (values.kind.takes_prompt() && !prompt.is_empty()).then(|| prompt.to_owned()),
        depends_on: values.after.clone(),
        placement,
        run,
        auto_start: values.queue,
        after_policy: AfterPolicy::Done,
    })
}

/// The user's login shell: `$SHELL`, else macOS's default (`/bin/zsh`).
pub fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// The argv that runs `command` through `shell` as a login shell: `;`, `&&`, pipes, quotes,
/// globs and variables mean what they mean in a terminal. The daemon starts it in a session
/// with the user's login environment.
pub fn shell_argv(shell: &str, command: &str) -> Vec<String> {
    vec![
        shell.to_owned(),
        "-lc".to_owned(),
        command.trim().to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> LaunchValues {
        LaunchValues {
            kind: LaunchKind::Terminal,
            agent: "omp".into(),
            model: None,
            account: None,
            safety: None,
            title: "  Build it  ".into(),
            prompt: String::new(),
            command: String::new(),
            shell: "/bin/zsh".into(),
            place: LaunchPlace::Main,
            after: vec![],
            queue: false,
        }
    }

    fn build(values: &LaunchValues) -> Result<TaskCreate, String> {
        build_create(WorkspaceId::new(), values, "OMP")
    }

    #[test]
    fn a_plain_terminal_is_a_task_without_a_run() {
        let plain = build(&values()).unwrap();
        assert_eq!(plain.title, "Build it");
        assert_eq!(
            (plain.run, plain.placement, plain.prompt, plain.auto_start),
            (None, None, None, false)
        );
    }

    #[test]
    fn a_command_in_a_new_worktree_after_a_task_queues_by_itself() {
        let dep = TaskId::new();
        let full = build(&LaunchValues {
            kind: LaunchKind::Command,
            command: "cargo test -p \"my crate\"".into(),
            place: LaunchPlace::NewWorktree {
                branch: " hutan/x ".into(),
                base: " main ".into(),
            },
            after: vec![dep],
            queue: true,
            ..values()
        })
        .unwrap();
        assert_eq!(full.depends_on, [dep]);
        assert!(full.auto_start);
        assert_eq!(
            full.run,
            Some(TaskRun::Terminal {
                contain: None,
                argv: vec![
                    "/bin/zsh".into(),
                    "-lc".into(),
                    "cargo test -p \"my crate\"".into()
                ]
            }),
            "the text goes to the login shell as it is"
        );
        assert_eq!(
            full.placement,
            Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("hutan/x".into()),
                base: Some("main".into()),
            })
        );
    }

    #[test]
    fn an_existing_worktree_is_named_by_its_branch() {
        let reuse = build(&LaunchValues {
            place: LaunchPlace::Existing {
                branch: "feature/y".into(),
            },
            ..values()
        })
        .unwrap();
        assert_eq!(
            reuse.placement,
            Some(TaskPlacement {
                kind: TaskPlacementKind::Worktree,
                branch: Some("feature/y".into()),
                base: None,
            })
        );
    }

    #[test]
    fn a_chat_is_a_chat_run_with_the_prompt_as_its_first_message() {
        let chat = build(&LaunchValues {
            kind: LaunchKind::Chat,
            agent: "codex".into(),
            model: Some("gpt-x".into()),
            title: String::new(),
            prompt: "  fix the flaky test\nthen rebase ".into(),
            ..values()
        })
        .unwrap();
        assert_eq!(chat.title, "fix the flaky test");
        assert_eq!(
            chat.prompt.as_deref(),
            Some("fix the flaky test\nthen rebase")
        );
        assert_eq!(
            chat.run,
            Some(TaskRun::Chat {
                contain: None,
                agent: "codex".into(),
                model: Some("gpt-x".into()),
                account: None,
                safety: None,
            })
        );
    }

    #[test]
    fn a_terminal_agent_starts_its_own_ui_with_the_prompt() {
        let tui = build(&LaunchValues {
            kind: LaunchKind::TerminalAgent,
            agent: "claude".into(),
            prompt: "refactor".into(),
            ..values()
        })
        .unwrap();
        assert_eq!(
            tui.run,
            Some(TaskRun::Terminal {
                contain: None,
                argv: vec!["claude".into(), "refactor".into()]
            })
        );
        let bare = build(&LaunchValues {
            kind: LaunchKind::TerminalAgent,
            agent: "omp".into(),
            title: String::new(),
            ..values()
        })
        .unwrap();
        assert_eq!(bare.title, "OMP agent");
        assert_eq!(
            bare.run,
            Some(TaskRun::Terminal {
                contain: None,
                argv: vec!["omp".into()]
            })
        );
    }

    #[test]
    fn a_chats_safety_rides_in_its_task_and_a_terminal_agents_in_its_flags() {
        let chat = build(&LaunchValues {
            kind: LaunchKind::Chat,
            agent: "codex".into(),
            prompt: "fix it".into(),
            safety: Some(SafetyPreset::Contained),
            ..values()
        })
        .unwrap();
        assert_eq!(
            chat.run,
            Some(TaskRun::Chat {
                contain: None,
                agent: "codex".into(),
                model: None,
                account: None,
                safety: Some(SafetyPreset::Contained),
            })
        );
        let argv = |agent: &str, safety| {
            let tui = build(&LaunchValues {
                kind: LaunchKind::TerminalAgent,
                agent: agent.into(),
                prompt: "fix it".into(),
                safety,
                ..values()
            })
            .unwrap();
            match tui.run {
                Some(TaskRun::Terminal { argv, .. }) => argv,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(argv("omp", None), ["omp", "fix it"]);
        assert_eq!(
            argv("codex", Some(SafetyPreset::ReadOnly)),
            ["codex", "-s", "read-only", "-a", "never", "fix it"]
        );
        assert_eq!(
            argv("omp", Some(SafetyPreset::Ask)),
            ["omp", "--approval-mode", "always-ask", "fix it"]
        );
        let claude = argv("claude", Some(SafetyPreset::Contained));
        assert_eq!(claude[..3], ["claude", "--permission-mode", "acceptEdits"]);
        assert_eq!(claude.last().map(String::as_str), Some("fix it"));
        // A harness that cannot honour the preset is an error, never a plain start.
        let error = build(&LaunchValues {
            kind: LaunchKind::TerminalAgent,
            agent: "omp".into(),
            prompt: "fix it".into(),
            safety: Some(SafetyPreset::Contained),
            ..values()
        })
        .unwrap_err();
        assert!(
            error.contains("Contained") && error.contains("no OS sandbox"),
            "{error}"
        );
    }

    #[test]
    fn a_sheet_with_a_problem_says_which() {
        let err = |values: LaunchValues| build(&values).unwrap_err();
        assert!(
            err(LaunchValues {
                title: "x".repeat(201),
                ..values()
            })
            .contains("200")
        );
        assert!(
            err(LaunchValues {
                kind: LaunchKind::Chat,
                ..values()
            })
            .contains("needs a prompt")
        );
        assert!(
            err(LaunchValues {
                kind: LaunchKind::Command,
                ..values()
            })
            .contains("command")
        );
        assert!(
            err(LaunchValues {
                place: LaunchPlace::NewWorktree {
                    branch: " ".into(),
                    base: String::new()
                },
                ..values()
            })
            .contains("branch")
        );
        assert!(
            err(LaunchValues {
                kind: LaunchKind::TerminalAgent,
                prompt: "--help".into(),
                ..values()
            })
            .contains("option")
        );
    }

    #[test]
    fn the_title_and_the_branch_follow_the_prompt() {
        assert_eq!(title_from_prompt("\n  Add a retry\nmore"), "Add a retry");
        // The first clause, not the request: the sentence end, then the 40-character cut at a word.
        assert_eq!(
            title_from_prompt("Add a retry. Change nothing else, keep the tests."),
            "Add a retry"
        );
        assert_eq!(
            title_from_prompt(
                "Add a one-line doc comment above fn count in src/lib.rs. Change nothing else."
            ),
            "Add a one-line doc comment above fn…"
        );
        assert_eq!(
            title_from_prompt("Bump v1.2 now"),
            "Bump v1.2 now",
            "a dot in a name stays"
        );
        assert_eq!(
            title_from_prompt("Why does it fail?  Look at src/x.rs"),
            "Why does it fail"
        );
        for prompt in ["a".repeat(100), "word ".repeat(30), "ünï ".repeat(40)] {
            let title = title_from_prompt(&prompt);
            assert!(title.chars().count() <= PROMPT_TITLE_CHARS, "{title}");
            assert!(title.ends_with('…'), "{title}");
        }
        let long = "a".repeat(100);
        let title = title_from_command(&long);
        assert_eq!(title.chars().count(), PROPOSED_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
        assert_eq!(branch_for_title("Add a retry!"), "hutan/add-a-retry");
        assert_eq!(branch_for_title("  "), "");
    }

    #[test]
    fn a_command_runs_as_the_login_shells_own_text() {
        assert_eq!(
            shell_argv("/bin/zsh", "  printf 'X\\n'; pwd && ls *.rs | wc -l  "),
            ["/bin/zsh", "-lc", "printf 'X\\n'; pwd && ls *.rs | wc -l"]
        );
        let shell = |command: &str| {
            build(&LaunchValues {
                kind: LaunchKind::Command,
                command: command.into(),
                shell: "/bin/bash".into(),
                ..values()
            })
            .unwrap()
            .run
        };
        assert_eq!(
            shell("echo 'unclosed"),
            Some(TaskRun::Terminal {
                contain: None,
                argv: vec!["/bin/bash".into(), "-lc".into(), "echo 'unclosed".into()]
            }),
            "the shell, not the sheet, judges the text"
        );
    }
}
