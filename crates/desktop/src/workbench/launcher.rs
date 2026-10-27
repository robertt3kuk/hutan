//! The Start… launcher (S-1): one sheet that starts an agent or a task in a project. It is opened
//! from a project row's ▶, the sidebar header, ⌘⇧T, ⌘K "Start Agent or Task…", the Tasks panel's
//! "New task" and the empty states, and every start is a task: `task.create` (+ `task.start`
//! for "Start now"; "Queue as task" leaves it to the scheduler). Only a direct model chat has no
//! task (`TaskRun::Chat` does not run `direct`): it goes through `chat.create`.
//!
//! This file holds the sheet's state and behavior; `launcher_ui.rs` draws it. Fields, in Tab
//! order: Project, What, Agent, Model, Command, Prompt, Where (+ Branch, Base), Title, More
//! (After, System prompt), Start now / Queue. ⌘↩ starts, ⌥⌘↩ starts and keeps the sheet open
//! for the next one.
//!
//! UX-B: the sheet opens on the last start's What / Agent / Model / Where (a plain terminal the
//! first time), says in one line what Start will do, asks once per agent and model before a chat
//! sends its prompt at once, changes a choice row's value by arrows only while the row is open,
//! and runs a Command through the login shell.

use std::collections::BTreeSet;
use std::path::PathBuf;

use gpui::{Context, KeyDownEvent, Window};
use hutan_core::agent::ModelInfo;
use hutan_core::{Contain, SafetyPreset, TaskId, WorkspaceId};
use hutan_protocol::chat::ChatCreate;
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};
use hutan_protocol::worktree::{WorktreeCreate, WorktreeInfo};

use super::agents::Models;
use super::chat::TextInput;
use super::chat::vim::Outcome;
use super::harnesses::{self, Detection, HARNESSES};
use super::palette::fuzzy_score;
use super::text_field::{FieldMut, FieldRef, edit_field};
use super::view::Workbench;
use super::vim_ui::VimConfig;
use super::workspaces::project_root_of;
use crate::sessions::launch::{
    LaunchKind, LaunchPlace, LaunchValues, branch_for_title, build_create, title_from_command,
    title_from_prompt, user_shell,
};
use crate::sessions::tasks::{TaskFilters, tree_rows};
use hutan_protocol::task::TaskCreate;

/// Where the sheet's keyboard focus is; Tab walks the visible ones top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LauncherStop {
    Project,
    What,
    Agent,
    Model,
    Command,
    Prompt,
    Where,
    Branch,
    Base,
    Title,
    /// The "More" header: folds the options below it.
    More,
    /// The "After" list (tasks this one waits for), under More.
    After,
    /// The system prompt of a direct chat, under More.
    System,
    /// Start now / Queue as task.
    Mode,
    /// The "Options" header of the inline composer: folds every row but the prompt and the agent.
    Options,
}

/// A text field of the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherField {
    Project,
    Prompt,
    Title,
    Branch,
    Base,
    Command,
    System,
}

/// What a chat talks to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAgent {
    /// `omp`, `claude` or `codex`.
    Harness(String),
    /// A model provider (`provider.list`); empty until the providers are listed.
    Direct(String),
}

/// Where the new session works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchWhere {
    Main,
    NewWorktree,
    /// The worktree that has this branch.
    Existing(String),
}

/// The worktrees of the chosen project, as `worktree.list` told.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorktreeList {
    #[default]
    Loading,
    Ready(Vec<WorktreeInfo>),
    Failed(String),
}

/// What opens the sheet with: every entry point is a preset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LauncherPreset {
    /// The project; none: the one in focus.
    pub project: Option<WorkspaceId>,
    /// A checkout the session works in (the main checkout or a worktree of the project).
    pub checkout: Option<PathBuf>,
    pub prompt: Option<String>,
    pub kind: Option<LaunchKind>,
    /// Start with a direct model chat.
    pub direct: bool,
    /// Start with "Queue as task".
    pub queue: bool,
    /// The review (comments of a Changes view) whose text is the prompt: its comments count as
    /// sent once the agent has started with it.
    pub handoff: Option<super::review::ReviewHandoff>,
    /// Open as the centre's start composer (the first run, after a folder was opened) instead
    /// of the sheet: the prompt and the agent, the other rows folded under Options.
    pub inline: bool,
}

/// How ready a harness is for what the sheet is about to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The accounts have not been listed yet.
    Checking,
    Ready,
    /// The daemon could not find it; the text says how to install it.
    NeedsInstall(String),
    /// It is installed and its login says logged out.
    NeedsLogin,
    /// The daemon does not run harness chats (it is older than this app).
    NeedsNewerDaemon,
}

/// A harness of the Agent row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessChoice {
    pub id: &'static str,
    pub name: &'static str,
    pub readiness: Readiness,
}

impl HarnessChoice {
    pub fn enabled(&self) -> bool {
        self.readiness == Readiness::Ready
    }
}

/// A start that waits for the user's yes: the first chat on this agent and model that sends a
/// prompt to a model at once (`launcher.confirm_agent_start`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartConfirm {
    /// ⌥⌘↩ started it: the sheet stays open for the next one.
    pub and_new: bool,
    /// "Don't ask again for this model" is ticked.
    pub remember: bool,
    /// What the prompt goes to, as the question names it ("OMP · Claude Opus 4.5").
    pub target: String,
}

/// What the Agent row says about a harness that cannot be started (yet).
pub(crate) fn readiness_note(name: &str, readiness: &Readiness) -> Option<String> {
    match readiness {
        Readiness::NeedsInstall(how) => Some(format!("{name} is not installed. {how}")),
        Readiness::NeedsLogin => Some(format!("{name} is not logged in.")),
        Readiness::NeedsNewerDaemon => Some(format!(
            "{name}: restart hutand (Daemon → Restart) to run the version of this app."
        )),
        Readiness::Checking => Some(format!("Checking {name}…")),
        Readiness::Ready => None,
    }
}

/// How many frames the Model popover asks to scroll to the model in use: the first one only
/// lays the list out.
pub(crate) const MODEL_REVEAL_FRAMES: u8 = 2;

/// Why `harness` cannot honour `preset` on a daemon that does (or does not) offer Contain: its
/// own table says so, or the preset's boundary is Hutan's Seatbelt and the daemon does not offer
/// it (capability `sandbox`, protocol 0.52).
pub(crate) fn preset_unavailable(
    harness: &str,
    preset: SafetyPreset,
    sandbox_daemon: bool,
) -> Option<String> {
    match hutan_core::safety::plan(harness, preset) {
        Err(reason) => Some(reason),
        Ok(plan) if plan.needs_contain && !sandbox_daemon => Some(
            "this hutand cannot contain a session (it predates protocol 0.52; restart it from \
             Daemon → Restart)"
                .to_owned(),
        ),
        Ok(_) => None,
    }
}

/// One choice of the Safety row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyChoice {
    pub preset: SafetyPreset,
    /// Why the harness cannot honour it; none: it can.
    pub unavailable: Option<String>,
}

/// The open sheet.
#[derive(Debug)]
pub struct Launcher {
    pub(crate) project: Option<WorkspaceId>,
    pub(crate) project_query: TextInput,
    pub(crate) project_sel: usize,
    pub(crate) kind: LaunchKind,
    /// The kind is the sheet's own pick (nothing was remembered or preset), not the user's: an
    /// agent harness found ready after the sheet opened turns it into an agent chat.
    pub(crate) kind_defaulted: bool,
    pub(crate) agent: LaunchAgent,
    /// The user picked the agent: what the daemon finds installed no longer moves it.
    pub(crate) agent_chosen: bool,
    /// A harness the user clicked that cannot be started yet: the line below the Agent row
    /// explains it (and offers Install… or Log in…) while the sheet stays on `agent`.
    pub(crate) agent_peek: Option<String>,
    /// The model of a chat; none: the harness's own default.
    pub(crate) model: Option<String>,
    /// The account of a harness chat (protocol 0.50): none follows the defaults, `system` is
    /// the harness's own login. Shown when the harness has a managed account.
    pub(crate) account: Option<String>,
    /// The safety preset of a chat or terminal agent (protocol 0.51): the Safety row's choice,
    /// which starts on `launcher.safety`. None leaves the harness's own settings. It only
    /// applies where the harness can honour it ([`Launcher::effective_safety`]).
    pub(crate) safety: Option<SafetyPreset>,
    /// A preset the user clicked that the harness cannot honour: the line below the Safety row
    /// says why, while the choice stays where it was.
    pub(crate) safety_peek: Option<SafetyPreset>,
    /// The daemon takes `safety` on chats (capability `chat.safety`).
    pub(crate) safety_daemon: bool,
    /// The daemon can run a session inside Hutan's Seatbelt (capability `sandbox`, protocol
    /// 0.52): Contain for terminals and OMP, and OMP's *Contained* preset.
    pub(crate) sandbox_daemon: bool,
    /// The Contain row: run the session inside Hutan's Seatbelt boundary. Forced on by OMP's
    /// *Contained* preset, which is that boundary.
    pub(crate) contain: bool,
    /// Whether a contained session may use the network (the row's second choice).
    pub(crate) contain_network: bool,
    /// The row whose choices are open: ↑ ↓ change its value only then (↩ or Space opens it, Esc
    /// or Tab closes it), and ⌘↩ does not start while one is open.
    pub(crate) open_row: Option<LauncherStop>,
    /// The Model popover's scroll: it opens on the model a start would use.
    pub(crate) model_scroll: gpui::ScrollHandle,
    /// The scroll of the project, worktree and "after" lists inside the sheet: each keeps the
    /// wheel while it has rows to scroll to (`ui::scroll`).
    pub(crate) project_scroll: gpui::ScrollHandle,
    pub(crate) worktree_scroll: gpui::ScrollHandle,
    pub(crate) after_scroll: gpui::ScrollHandle,
    /// Frames left in which the popover scrolls to the model in use.
    pub(crate) model_reveal: std::cell::Cell<u8>,
    pub(crate) prompt: TextInput,
    pub(crate) title: TextInput,
    pub(crate) branch: TextInput,
    pub(crate) base: TextInput,
    pub(crate) command: TextInput,
    pub(crate) system: TextInput,
    /// The user typed the title / branch / base: they no longer follow the prompt / title / repo.
    pub(crate) title_edited: bool,
    pub(crate) branch_edited: bool,
    pub(crate) base_edited: bool,
    /// Vim's modal state over the field being typed in (`editor.vim_mode`).
    pub(crate) vim: crate::workbench::chat::vim::Vim,
    pub(crate) place: LaunchWhere,
    pub(crate) worktrees: WorktreeList,
    /// Counts the worktree lists asked for; an older answer is dropped.
    pub(crate) worktree_requests: u64,
    /// A checkout the preset asked for, chosen as soon as the worktrees are known.
    pub(crate) wanted_checkout: Option<PathBuf>,
    pub(crate) more_open: bool,
    pub(crate) after: BTreeSet<TaskId>,
    pub(crate) after_cursor: usize,
    pub(crate) queue: bool,
    pub(crate) handoff: Option<super::review::ReviewHandoff>,
    pub(crate) stop: LauncherStop,
    pub(crate) error: Option<String>,
    /// `error` is about the agent or its model (it cannot start, no provider, no model choice
    /// on this daemon): an agent preselected in its place clears it, as it clears no other.
    pub(crate) error_about_agent: bool,
    pub(crate) starting: bool,
    /// The agent and model of the last start: what the sheet offers while the user has not
    /// picked one.
    pub(crate) remembered_agent: Option<LaunchAgent>,
    pub(crate) remembered_model: Option<String>,
    /// A start waiting for the user's yes.
    pub(crate) confirm: Option<StartConfirm>,
    /// The next start was confirmed: it does not ask again.
    pub(crate) skip_confirm: bool,
    /// The composer sits in the centre instead of a sheet over the window.
    pub(crate) inline: bool,
    /// The inline composer's Options are unfolded.
    pub(crate) options_open: bool,
}

impl Launcher {
    pub fn project(&self) -> Option<WorkspaceId> {
        self.project
    }

    pub fn kind(&self) -> LaunchKind {
        self.kind
    }

    pub fn agent(&self) -> &LaunchAgent {
        &self.agent
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn prompt(&self) -> &str {
        self.prompt.text()
    }

    pub fn title(&self) -> &str {
        self.title.text()
    }

    pub fn branch(&self) -> &str {
        self.branch.text()
    }

    pub fn base(&self) -> &str {
        self.base.text()
    }

    pub fn command(&self) -> &str {
        self.command.text()
    }

    pub fn place(&self) -> &LaunchWhere {
        &self.place
    }

    pub fn worktrees(&self) -> &WorktreeList {
        &self.worktrees
    }

    pub fn queue(&self) -> bool {
        self.queue
    }

    pub fn after(&self) -> Vec<TaskId> {
        self.after.iter().copied().collect()
    }

    pub fn stop(&self) -> LauncherStop {
        self.stop
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Shows `error` until the user changes what it is about (or starts again).
    pub(crate) fn fail(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.error_about_agent = false;
    }

    /// Shows `error`, which is about the agent or its model: another agent clears it, also one
    /// the sheet preselects on its own.
    pub(crate) fn fail_agent(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.error_about_agent = true;
    }

    pub fn starting(&self) -> bool {
        self.starting
    }

    pub fn more_open(&self) -> bool {
        self.more_open
    }

    /// The launcher is the centre's start composer (the first run), not the sheet.
    pub fn inline(&self) -> bool {
        self.inline
    }

    /// The inline composer's Options are unfolded.
    pub fn options_open(&self) -> bool {
        self.options_open
    }

    /// The row whose choices are open.
    pub fn open_row(&self) -> Option<LauncherStop> {
        self.open_row
    }

    /// The scroll of the Model popover (its offset says where the list opened).
    pub fn model_scroll(&self) -> &gpui::ScrollHandle {
        &self.model_scroll
    }

    /// Whether the Model row's list is open.
    pub fn model_open(&self) -> bool {
        self.open_row == Some(LauncherStop::Model)
    }

    /// The start waiting for a yes, if any.
    pub fn confirm(&self) -> Option<&StartConfirm> {
        self.confirm.as_ref()
    }

    pub fn is_direct(&self) -> bool {
        matches!(self.agent, LaunchAgent::Direct(_))
    }

    /// The harness the Safety row is about: a chat's or terminal agent's, never a direct
    /// model's, a command's or a plain terminal's. A chat needs a daemon that takes presets;
    /// a terminal agent only needs flags.
    pub fn safety_harness(&self) -> Option<&str> {
        match (&self.agent, self.kind) {
            (LaunchAgent::Harness(id), LaunchKind::Chat)
                if !id.is_empty() && self.safety_daemon =>
            {
                Some(id)
            }
            (LaunchAgent::Harness(id), LaunchKind::TerminalAgent) if !id.is_empty() => Some(id),
            _ => None,
        }
    }

    /// The preset the sheet would start with: the choice, when the harness can honour it.
    pub fn effective_safety(&self) -> Option<SafetyPreset> {
        let harness = self.safety_harness()?;
        self.safety
            .filter(|preset| self.safety_unavailable(harness, *preset).is_none())
    }

    /// Why the harness cannot honour `preset` here ([`preset_unavailable`]).
    fn safety_unavailable(&self, harness: &str, preset: SafetyPreset) -> Option<String> {
        preset_unavailable(harness, preset, self.sandbox_daemon)
    }

    /// Whether the Contain row applies: the daemon offers it, and the session is a terminal,
    /// a command, or an OMP chat or agent. Codex and Claude Code run their own sandbox, which
    /// Seatbelt cannot nest in, so their *Contained* preset is the boundary there.
    pub fn contain_available(&self) -> bool {
        self.sandbox_daemon
            && match (self.kind, &self.agent) {
                (LaunchKind::Terminal | LaunchKind::Command, _) => true,
                (LaunchKind::Chat | LaunchKind::TerminalAgent, LaunchAgent::Harness(id)) => {
                    id == "omp"
                }
                _ => false,
            }
    }

    /// The Contained preset is Hutan's Seatbelt for this harness: the row is on and stays on.
    pub fn contain_forced(&self) -> bool {
        self.contain_available()
            && self.effective_safety().is_some_and(|preset| {
                self.safety_harness().is_some_and(|harness| {
                    hutan_core::safety::plan(harness, preset).is_ok_and(|plan| plan.needs_contain)
                })
            })
    }

    /// The session starts inside the boundary.
    pub fn contain_on(&self) -> bool {
        self.contain_available() && (self.contain || self.contain_forced())
    }

    /// What the sheet asks the daemon for: the boundary and its network choice.
    pub fn contain_request(&self) -> Option<Contain> {
        self.contain_on().then(|| Contain {
            network: Some(self.contain_network),
            extra_write: Vec::new(),
        })
    }

    /// The honest sentence under the Contain row: what the boundary holds back and what it
    /// leaves open, the wording the session chip uses afterwards.
    pub fn contain_line(&self) -> String {
        if self.contain_on() {
            let mut line = hutan_core::summary_line(self.contain_network);
            line[..1].make_ascii_uppercase();
            format!("{line}.")
        } else {
            "No boundary: the session can write wherever you can.".to_owned()
        }
    }

    /// The Safety row's choices for the harness: each preset, and why it is not available.
    pub fn safety_choices(&self) -> Vec<SafetyChoice> {
        let Some(harness) = self.safety_harness() else {
            return Vec::new();
        };
        SafetyPreset::ALL
            .into_iter()
            .map(|preset| SafetyChoice {
                preset,
                unavailable: self.safety_unavailable(harness, preset),
            })
            .collect()
    }

    /// The preset the row shows as chosen, and the preset it was clicked on while unavailable.
    pub fn safety_peek(&self) -> Option<SafetyPreset> {
        self.safety_peek
    }

    /// The sentence under the Safety row, and whether it is a warning: what the preset in
    /// effect (or the one the user clicked and cannot have) means for this harness, and the
    /// holes it leaves (ADR 0021 decision 6).
    pub fn safety_line(&self) -> Option<(String, bool)> {
        let harness = self.safety_harness()?;
        if let Some(peek) = self.safety_peek
            && let Some(reason) = self.safety_unavailable(harness, peek)
        {
            return Some((
                format!("{} is not available: {reason}.", peek.label()),
                true,
            ));
        }
        let chosen = self.safety;
        if let Some(preset) = chosen
            && let Some(reason) = self.safety_unavailable(harness, preset)
        {
            return Some((
                format!(
                    "{} is not available for this agent ({reason}): it starts with its own settings.",
                    preset.label()
                ),
                true,
            ));
        }
        let (preset, text) = match chosen {
            Some(preset) => (preset, hutan_core::safety::describe(harness, preset)?),
            None => {
                return Some((
                    "The agent's own approval and sandbox settings apply; Hutan changes none."
                        .to_owned(),
                    false,
                ));
            }
        };
        let flags = self.kind == LaunchKind::TerminalAgent;
        let suffix = if flags {
            " Passed as command-line flags; the agent's own screen shows what applies."
        } else {
            ""
        };
        Some((format!("{text}{suffix}"), preset.is_full_access()))
    }

    /// The worktrees git lists besides the main checkout, with a branch to name them by.
    pub fn existing_worktrees(&self) -> Vec<&WorktreeInfo> {
        match &self.worktrees {
            WorktreeList::Ready(list) => list
                .iter()
                .filter(|w| !w.main && !w.prunable && w.branch.is_some())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The main checkout of the project's repository, when the project is one.
    pub fn main_worktree(&self) -> Option<&WorktreeInfo> {
        match &self.worktrees {
            WorktreeList::Ready(list) => list.iter().find(|w| w.main),
            _ => None,
        }
    }

    /// The branch the main checkout is on: what a new worktree starts from.
    pub fn current_branch(&self) -> Option<&str> {
        self.main_worktree().and_then(|w| w.branch.as_deref())
    }

    /// The worktree the chosen place is.
    pub fn chosen_worktree(&self) -> Option<&WorktreeInfo> {
        match &self.place {
            LaunchWhere::Main => self.main_worktree(),
            LaunchWhere::NewWorktree => None,
            LaunchWhere::Existing(branch) => self
                .existing_worktrees()
                .into_iter()
                .find(|w| w.branch.as_deref() == Some(branch.as_str())),
        }
    }

    fn input_mut(&mut self, field: LauncherField) -> &mut TextInput {
        self.vim_and_input(field).1
    }

    /// The modal state and the text of `field`, borrowed together.
    fn vim_and_input(
        &mut self,
        field: LauncherField,
    ) -> (&mut crate::workbench::chat::vim::Vim, &mut TextInput) {
        let input = match field {
            LauncherField::Project => &mut self.project_query,
            LauncherField::Prompt => &mut self.prompt,
            LauncherField::Title => &mut self.title,
            LauncherField::Branch => &mut self.branch,
            LauncherField::Base => &mut self.base,
            LauncherField::Command => &mut self.command,
            LauncherField::System => &mut self.system,
        };
        (&mut self.vim, input)
    }

    /// The text field a stop types into.
    fn field_of(stop: LauncherStop) -> Option<LauncherField> {
        Some(match stop {
            LauncherStop::Project => LauncherField::Project,
            LauncherStop::Prompt => LauncherField::Prompt,
            LauncherStop::Title => LauncherField::Title,
            LauncherStop::Branch => LauncherField::Branch,
            LauncherStop::Base => LauncherField::Base,
            LauncherStop::Command => LauncherField::Command,
            LauncherStop::System => LauncherField::System,
            _ => return None,
        })
    }

    /// The branch and base follow the title and the repository until the user types them.
    fn follow(&mut self) {
        if !self.branch_edited {
            let branch = branch_for_title(self.title.text());
            self.branch.set(&branch);
        }
    }

    /// The title follows the first line of the prompt until the user types it.
    fn follow_prompt(&mut self) {
        if !self.title_edited {
            let title = title_from_prompt(self.prompt.text());
            self.title.set(&title);
        }
        self.follow();
    }

    /// A command has no prompt: its title follows the command line.
    fn follow_command(&mut self) {
        if !self.title_edited {
            let title = title_from_command(self.command.text());
            self.title.set(&title);
        }
        self.follow();
    }

    /// The task the sheet describes (the harness's `agent_name` names an untitled one).
    pub fn create_request(
        &self,
        workspace: WorkspaceId,
        agent_name: &str,
    ) -> Result<TaskCreate, String> {
        build_create(workspace, &self.values(), agent_name)
    }

    fn values(&self) -> LaunchValues {
        LaunchValues {
            kind: self.kind,
            agent: match &self.agent {
                LaunchAgent::Harness(id) => id.clone(),
                LaunchAgent::Direct(_) => String::new(),
            },
            model: (self.kind == LaunchKind::Chat && !matches!(self.agent, LaunchAgent::Direct(_)))
                .then(|| self.model.clone())
                .flatten(),
            account: (self.kind == LaunchKind::Chat
                && !matches!(self.agent, LaunchAgent::Direct(_)))
            .then(|| self.account.clone())
            .flatten(),
            safety: self.effective_safety(),
            contain: self.contain_request(),
            // A plain terminal is not described: a title typed before switching is not used.
            title: if self.kind.has_task_fields() {
                self.title.text().to_owned()
            } else {
                String::new()
            },
            prompt: self.prompt.text().to_owned(),
            command: self.command.text().to_owned(),
            shell: user_shell(),
            place: match &self.place {
                LaunchWhere::Main => LaunchPlace::Main,
                LaunchWhere::NewWorktree => LaunchPlace::NewWorktree {
                    branch: self.branch.text().to_owned(),
                    base: self.base.text().to_owned(),
                },
                LaunchWhere::Existing(branch) => LaunchPlace::Existing {
                    branch: branch.clone(),
                },
            },
            after: self.after.iter().copied().collect(),
            queue: self.queue && self.kind.has_task_fields(),
        }
    }
}

/// What the status bar says of a warning the daemon returned with a start. The daemon's
/// sentence about a shared checkout names the folder and every other session; the bar has room
/// for a short item, and the full account is where it belongs: the sheet's warning before the
/// start, and the session's *shared checkout* badge after it.
pub(crate) fn short_start_warning(warning: &str) -> String {
    const SHARED: &str = " live agent(s) already work in ";
    if let Some(at) = warning.find(SHARED)
        && let Some(count) = warning[..at]
            .strip_suffix(" other")
            .and_then(|n| n.parse::<usize>().ok())
    {
        return if count == 1 {
            "Another agent already works in this checkout".to_owned()
        } else {
            format!("{count} other agents already work in this checkout")
        };
    }
    const MAX: usize = 80;
    if warning.chars().count() <= MAX {
        return warning.to_owned();
    }
    let cut: String = warning.chars().take(MAX).collect();
    format!("{}…", cut.trim_end())
}

/// A project the sheet can start in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectChoice {
    pub id: WorkspaceId,
    pub name: String,
    pub root: Option<PathBuf>,
}

impl Workbench {
    // ---- read access -----------------------------------------------------------------------

    /// The open Start… sheet.
    pub fn launcher(&self) -> Option<&Launcher> {
        self.launcher.as_ref()
    }

    /// The projects the sheet offers: projects, Home, and workspaces that hold no project.
    pub fn launcher_projects(&self, cx: &gpui::App) -> Vec<ProjectChoice> {
        let store = self.sessions.read(cx);
        store
            .workspaces()
            .iter()
            .filter(|w| match w.kind {
                WorkspaceKind::Project | WorkspaceKind::Home => true,
                WorkspaceKind::Workspace => store.state().projects_in(w.id).next().is_none(),
            })
            .map(|w| ProjectChoice {
                id: w.id,
                name: w.name.clone(),
                root: project_root_of(w),
            })
            .collect()
    }

    /// The projects matching the sheet's project query, best first.
    pub fn launcher_project_matches(&self, cx: &gpui::App) -> Vec<ProjectChoice> {
        let Some(launcher) = &self.launcher else {
            return Vec::new();
        };
        let query = launcher.project_query.text().to_owned();
        let mut scored: Vec<(i32, usize, ProjectChoice)> = self
            .launcher_projects(cx)
            .into_iter()
            .enumerate()
            .filter_map(|(at, project)| {
                let haystack = format!(
                    "{} {}",
                    project.name,
                    project
                        .root
                        .as_ref()
                        .map(|r| r.display().to_string())
                        .unwrap_or_default()
                );
                fuzzy_score(&query, &haystack).map(|score| (score, at, project))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, project)| project).collect()
    }

    /// The harnesses of the Agent row and whether each can be started for the chosen kind.
    pub fn launcher_harness_choices(&self, cx: &gpui::App) -> Vec<HarnessChoice> {
        let chat = self
            .launcher
            .as_ref()
            .is_none_or(|l| l.kind == LaunchKind::Chat);
        self.harness_choices(chat, cx)
    }

    /// Whether some harness can start an agent chat now.
    fn chat_harness_ready(&self, cx: &gpui::App) -> bool {
        self.harness_choices(true, cx).iter().any(|h| h.enabled())
    }

    fn harness_choices(&self, chat: bool, cx: &gpui::App) -> Vec<HarnessChoice> {
        let directory = self.env.harnesses.read(cx);
        HARNESSES
            .iter()
            .map(|harness| {
                let readiness = match directory.detection(harness.id) {
                    Detection::NotFound(how) => Readiness::NeedsInstall(how),
                    _ if chat && !self.harness_available(harness.id, cx) => {
                        Readiness::NeedsNewerDaemon
                    }
                    Detection::Checking if chat => Readiness::Checking,
                    Detection::Installed
                        if chat
                            && !directory.accounts_of(harness.id).any(|a| {
                                a.status != hutan_protocol::account::AccountStatus::LoggedOut
                            }) =>
                    {
                        Readiness::NeedsLogin
                    }
                    _ => Readiness::Ready,
                };
                HarnessChoice {
                    id: harness.id,
                    name: harness.name,
                    readiness,
                }
            })
            .collect()
    }

    /// The models the chosen agent offers, as the Model row lists them.
    pub fn launcher_models(&self, cx: &gpui::App) -> Models {
        let Some(launcher) = &self.launcher else {
            return Models::Ready(Vec::new());
        };
        match &launcher.agent {
            LaunchAgent::Harness(id) => match self.env.harnesses.read(cx).models(id) {
                // In the pickers' order: by provider, family, newest first (`sessions::models`).
                Some(Models::Ready(models)) => Models::Ready(
                    crate::sessions::models::picker_rows(id, models, "", None, true)
                        .into_iter()
                        .map(|row| row.model.clone())
                        .collect(),
                ),
                Some(other) => other.clone(),
                None => Models::Loading,
            },
            LaunchAgent::Direct(provider) => {
                if let Some(models) = self.providers.models.get(provider) {
                    let default = self
                        .providers
                        .providers
                        .iter()
                        .find(|p| &p.id == provider)
                        .and_then(|p| p.default_model.clone());
                    let mut models = models.clone();
                    if let Some(default) = default {
                        for model in &mut models {
                            model.is_default = model.id == default;
                        }
                    }
                    Models::Ready(models)
                } else if let Some(error) = &self.providers.error {
                    Models::Failed(error.summary.clone())
                } else {
                    Models::Loading
                }
            }
        }
    }

    /// The model a start would use: the one picked, else the catalog's default (a harness chat
    /// without a pick runs on the harness's own default and names none).
    pub fn launcher_chosen_model(&self, cx: &gpui::App) -> Option<ModelInfo> {
        let launcher = self.launcher.as_ref()?;
        let Models::Ready(models) = self.launcher_models(cx) else {
            return None;
        };
        match &launcher.model {
            Some(id) => models.into_iter().find(|m| &m.id == id),
            None => {
                let default = models.iter().find(|m| m.is_default).cloned();
                default.or_else(|| {
                    launcher
                        .is_direct()
                        .then(|| models.first().cloned())
                        .flatten()
                })
            }
        }
    }

    /// The rows of the Start sheet's model menu over `list` (what [`launcher_models`] gave): in
    /// the pickers' order for a harness (a provider's list keeps its own), the dated twin told
    /// apart, and the model a start would use marked current.
    ///
    /// [`launcher_models`]: Self::launcher_models
    pub fn launcher_model_rows<'a>(
        &self,
        list: &'a [ModelInfo],
        cx: &gpui::App,
    ) -> Vec<crate::sessions::models::ModelRow<'a>> {
        let Some(launcher) = self.launcher.as_ref() else {
            return Vec::new();
        };
        let (agent, ordered) = match &launcher.agent {
            LaunchAgent::Harness(id) => (id.as_str(), true),
            LaunchAgent::Direct(_) => ("", false),
        };
        let current = self.launcher_chosen_model(cx).map(|m| m.id);
        crate::sessions::models::picker_rows(agent, list, "", current.as_deref(), ordered)
    }

    /// Has the open Model popover scroll to the model a start would use. A list that was just
    /// laid out for the first time ignores a scroll (GPUI knows its size and overflow only from
    /// the frame before), so the sheet asks again on the next frame: see [`MODEL_REVEAL_FRAMES`].
    pub(crate) fn launcher_reveal_model(&self, _cx: &gpui::App) {
        if let Some(launcher) = &self.launcher {
            launcher.model_reveal.set(MODEL_REVEAL_FRAMES);
        }
    }

    /// The warning that another live agent already works in the checkout the sheet would use.
    pub fn launcher_shared_warning(&self, cx: &gpui::App) -> Option<String> {
        let launcher = self.launcher.as_ref()?;
        if !launcher.kind.is_agent() {
            return None;
        }
        let checkout = launcher.chosen_worktree()?;
        let store = self.sessions.read(cx);
        let agents = checkout
            .live_sessions
            .iter()
            .filter(|id| {
                store
                    .session(**id)
                    .is_some_and(crate::sessions::works_in_checkout)
            })
            .count();
        (agents > 0).then(|| {
            let who = if agents == 1 {
                "Another live agent works".to_owned()
            } else {
                format!("{agents} other live agents work")
            };
            format!(
                "{who} in this checkout: they would write over each other. Pick New worktree to keep them apart."
            )
        })
    }

    /// The note about the agent the sheet is set to, when it cannot be started yet. Only the
    /// chosen agent has one: another harness's installation or login problem is not this
    /// start's.
    pub fn launcher_agent_status(&self, cx: &gpui::App) -> Option<String> {
        let launcher = self.launcher.as_ref()?;
        let LaunchAgent::Harness(id) = &launcher.agent else {
            return None;
        };
        let choice = self
            .launcher_harness_choices(cx)
            .into_iter()
            .find(|h| h.id == id)?;
        readiness_note(choice.name, &choice.readiness)
    }

    /// The harness the line below the Agent row is about: the one the user clicked that cannot be
    /// started, else the agent the sheet is set to when it cannot be started (its readiness
    /// changed after it was picked). `None` when nothing needs saying. The row of choices itself
    /// never changes with it.
    pub fn launcher_readiness_target(&self, cx: &gpui::App) -> Option<HarnessChoice> {
        let launcher = self.launcher.as_ref()?;
        if !launcher.kind.is_agent() {
            return None;
        }
        let choices = self.launcher_harness_choices(cx);
        let unready = |id: &str| choices.iter().find(|h| h.id == id && !h.enabled()).cloned();
        launcher
            .agent_peek
            .as_deref()
            .and_then(unready)
            .or_else(|| match &launcher.agent {
                LaunchAgent::Harness(id) => unready(id),
                LaunchAgent::Direct(_) => None,
            })
    }

    /// The name of the agent the sheet is set to ("OMP", "Direct · Ollama").
    fn launcher_agent_name(&self) -> String {
        match self.launcher.as_ref().map(|l| &l.agent) {
            Some(LaunchAgent::Harness(id)) => harnesses::harness_name(id),
            Some(LaunchAgent::Direct(provider)) => self
                .providers
                .providers
                .iter()
                .find(|p| &p.id == provider)
                .map_or_else(
                    || "Direct model".to_owned(),
                    |p| format!("Direct · {}", p.name),
                ),
            None => String::new(),
        }
    }

    /// The model the sheet would use, by name ("Claude Opus 4.5"); "its default model" while
    /// none is known.
    fn launcher_model_name(&self, cx: &gpui::App) -> String {
        self.launcher_chosen_model(cx)
            .map_or_else(|| "its default model".to_owned(), |m| m.display_name)
    }

    /// Where the sheet would work, as the summary says it: the project and its checkout.
    fn launcher_place_name(&self, cx: &gpui::App) -> String {
        let Some(launcher) = &self.launcher else {
            return String::new();
        };
        let project = launcher
            .project
            .and_then(|id| {
                self.launcher_projects(cx)
                    .into_iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name)
            })
            .unwrap_or_else(|| "no project".to_owned());
        match &launcher.place {
            LaunchWhere::Main => match launcher.current_branch() {
                Some(branch) => format!("{project} ({branch})"),
                None => project,
            },
            LaunchWhere::NewWorktree => match launcher.branch.text().trim() {
                "" => format!("{project} (new worktree)"),
                branch => format!("{project} (new worktree {branch})"),
            },
            LaunchWhere::Existing(branch) => format!("{project} (worktree {branch})"),
        }
    }

    /// One line on what Start does now: "Start OMP · Claude Opus 4.5 chat in hutan (main)",
    /// "Open a terminal in hutan (main)". It is the footer's text and what a first-time
    /// confirmation repeats.
    pub fn launcher_summary(&self, cx: &gpui::App) -> String {
        let Some(launcher) = &self.launcher else {
            return String::new();
        };
        if launcher.project.is_none() {
            return "Choose a project to start in.".to_owned();
        }
        let place = self.launcher_place_name(cx);
        let queue = launcher.queue && !launcher.is_direct();
        let start = if queue { "Queue" } else { "Start" };
        match launcher.kind {
            LaunchKind::Chat => format!(
                "{start} {} · {} chat in {place}",
                self.launcher_agent_name(),
                self.launcher_model_name(cx)
            ),
            LaunchKind::TerminalAgent => format!(
                "{start} {} in a terminal in {place}",
                self.launcher_agent_name()
            ),
            LaunchKind::Terminal => format!(
                "{} a terminal in {place}",
                if queue { "Queue" } else { "Open" }
            ),
            LaunchKind::Command => {
                let command = launcher.command.text().trim();
                let shown: String = command.chars().take(40).collect();
                let shown = if command.chars().count() > 40 {
                    format!("{shown}…")
                } else {
                    shown
                };
                let verb = if queue { "Queue" } else { "Run" };
                if shown.is_empty() {
                    format!("{verb} a command in {place}")
                } else {
                    format!("{verb} “{shown}” in {place}")
                }
            }
        }
    }

    /// The Start button's label: the summary's first words.
    pub fn launcher_start_label(&self) -> String {
        let Some(launcher) = &self.launcher else {
            return String::new();
        };
        if launcher.starting {
            return "Starting…".to_owned();
        }
        if launcher.queue && !launcher.is_direct() {
            return "Queue ⌘↩".to_owned();
        }
        match launcher.kind {
            LaunchKind::Chat => format!("Start {} chat ⌘↩", self.launcher_agent_name()),
            LaunchKind::TerminalAgent => format!("Start {} ⌘↩", self.launcher_agent_name()),
            LaunchKind::Terminal => "Open terminal ⌘↩".to_owned(),
            LaunchKind::Command => "Run command ⌘↩".to_owned(),
        }
    }

    /// The stops Tab walks now, top to bottom.
    pub fn launcher_stops(&self, cx: &gpui::App) -> Vec<LauncherStop> {
        let Some(launcher) = &self.launcher else {
            return Vec::new();
        };
        let mut stops = vec![LauncherStop::Project, LauncherStop::What];
        if launcher.kind.is_agent() {
            stops.push(LauncherStop::Agent);
        }
        if launcher.kind == LaunchKind::Chat
            && matches!(self.launcher_models(cx), Models::Ready(m) if !m.is_empty())
        {
            stops.push(LauncherStop::Model);
        }
        if launcher.kind == LaunchKind::Command {
            stops.push(LauncherStop::Command);
        }
        if launcher.kind.takes_prompt() {
            stops.push(LauncherStop::Prompt);
        }
        stops.push(LauncherStop::Where);
        if launcher.place == LaunchWhere::NewWorktree {
            stops.extend([LauncherStop::Branch, LauncherStop::Base]);
        }
        if launcher.kind.has_task_fields() {
            stops.push(LauncherStop::Title);
        }
        stops.push(LauncherStop::More);
        if launcher.more_open {
            stops.push(LauncherStop::After);
            if launcher.is_direct() {
                stops.push(LauncherStop::System);
            }
        }
        if launcher.kind.has_task_fields() {
            stops.push(LauncherStop::Mode);
        }
        if !launcher.inline {
            return stops;
        }
        // The inline composer shows the command or prompt, then the agent, then Options; what
        // the sheet lists before them follows Options once it is unfolded.
        let first: Vec<LauncherStop> = [
            LauncherStop::Command,
            LauncherStop::Prompt,
            LauncherStop::Agent,
        ]
        .into_iter()
        .filter(|stop| stops.contains(stop))
        .collect();
        let mut inline = first.clone();
        inline.push(LauncherStop::Options);
        if launcher.options_open {
            inline.extend(stops.into_iter().filter(|stop| !first.contains(stop)));
        }
        inline
    }

    // ---- opening and closing ---------------------------------------------------------------

    /// ⌘⇧T, ⌘K "Start Agent or Task…", the ▶ buttons and the empty states.
    pub fn open_launcher(
        &mut self,
        preset: LauncherPreset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connected, chat, tasks, default_project) = {
            let store = self.sessions.read(cx);
            (
                store.connection_state() == hutan_client::ConnectionState::Connected,
                store.connection().supports_chat(),
                store.connection().supports_tasks(),
                self.target_workspace(store).map(|w| w.id),
            )
        };
        if !connected {
            self.set_notice("Not connected to hutand", cx);
            return;
        }
        if !chat || !tasks {
            self.set_notice(
                "This hutand does not run agents and tasks; restart it (Daemon → Restart)",
                cx,
            );
            return;
        }
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.agent_picker = None;
        let project = preset.project.or(default_project);
        let memory = self.launcher_memory.clone();
        // A preset with a prompt, a queue or a direct chat is about an agent. Otherwise the sheet
        // opens on what the last start was; before there was one, on an agent chat when a
        // harness is ready, else on a plain terminal (a harness found ready a moment later
        // turns the sheet into a chat too, until the user picks a kind).
        let agentic = preset.prompt.is_some() || preset.queue || preset.direct || preset.inline;
        let remembered_kind = memory
            .kind
            .as_deref()
            .and_then(|key| LaunchKind::ALL.into_iter().find(|k| k.key() == key));
        let kind_defaulted = preset.kind.is_none() && !agentic && remembered_kind.is_none();
        let kind = preset.kind.unwrap_or_else(|| {
            if agentic {
                return LaunchKind::Chat;
            }
            remembered_kind.unwrap_or_else(|| {
                if self.chat_harness_ready(cx) {
                    LaunchKind::Chat
                } else {
                    LaunchKind::Terminal
                }
            })
        });
        let remembered_agent =
            memory
                .agent
                .as_deref()
                .map(|agent| match agent.strip_prefix("direct:") {
                    Some(provider) => LaunchAgent::Direct(provider.to_owned()),
                    None => LaunchAgent::Harness(agent.to_owned()),
                });
        let agent = if preset.direct {
            LaunchAgent::Direct(String::new())
        } else {
            self.launcher_preferred_agent(kind, remembered_agent.as_ref(), cx)
        };
        let model = (!preset.direct && remembered_agent.as_ref() == Some(&agent))
            .then(|| memory.model.clone())
            .flatten();
        let place = match memory.place.as_deref() {
            _ if preset.checkout.is_some() => LaunchWhere::Main,
            Some("new_worktree") => LaunchWhere::NewWorktree,
            Some(other) => other
                .strip_prefix("worktree:")
                .map_or(LaunchWhere::Main, |branch| {
                    LaunchWhere::Existing(branch.to_owned())
                }),
            None => LaunchWhere::Main,
        };
        let mut launcher = Launcher {
            project,
            project_query: TextInput::new(),
            project_sel: 0,
            kind,
            kind_defaulted,
            agent,
            agent_chosen: preset.direct,
            agent_peek: None,
            model,
            account: None,
            safety: self
                .settings
                .get::<crate::settings::LauncherSettings>()
                .safety
                .preset(),
            safety_peek: None,
            safety_daemon: self.sessions.read(cx).supports_chat_safety(),
            sandbox_daemon: self.sessions.read(cx).supports_sandbox(),
            contain: false,
            contain_network: true,
            open_row: None,
            model_scroll: gpui::ScrollHandle::new(),
            project_scroll: gpui::ScrollHandle::new(),
            worktree_scroll: gpui::ScrollHandle::new(),
            after_scroll: gpui::ScrollHandle::new(),
            model_reveal: std::cell::Cell::new(0),
            prompt: TextInput::new(),
            title: TextInput::new(),
            branch: TextInput::new(),
            base: TextInput::new(),
            command: TextInput::new(),
            system: TextInput::new(),
            title_edited: false,
            branch_edited: false,
            base_edited: false,
            vim: crate::workbench::chat::vim::Vim::new(),
            place,
            worktrees: WorktreeList::Loading,
            worktree_requests: 0,
            wanted_checkout: preset.checkout.clone(),
            more_open: false,
            after: BTreeSet::new(),
            after_cursor: 0,
            queue: preset.queue,
            handoff: preset.handoff.clone(),
            stop: LauncherStop::Prompt,
            error: None,
            error_about_agent: false,
            starting: false,
            remembered_agent,
            remembered_model: memory.model.clone(),
            confirm: None,
            skip_confirm: false,
            inline: preset.inline,
            options_open: false,
        };
        if let Some(prompt) = &preset.prompt {
            launcher.prompt.set(prompt);
            launcher.follow_prompt();
        }
        launcher.stop = if kind.takes_prompt() {
            LauncherStop::Prompt
        } else if kind == LaunchKind::Command {
            LauncherStop::Command
        } else {
            LauncherStop::What
        };
        self.launcher = Some(launcher);
        self.edits.agent.reset();
        window.focus(&self.launcher_focus, cx);
        self.refresh_accounts(cx);
        self.refresh_providers(cx);
        self.launcher_repick_agent(cx);
        self.launcher_load_models(cx);
        self.launcher_load_worktrees(cx);
        cx.notify();
    }

    /// Opens the sheet on an agent chat in the checkout at `cwd` (the main checkout of a
    /// project, or one of its worktrees) with `text` as its prompt (Changes view: "Start a new
    /// agent with these comments"). The user still presses Start.
    pub fn start_agent_with(
        &mut self,
        cwd: PathBuf,
        text: String,
        handoff: Option<super::review::ReviewHandoff>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preset =
            move |project: Option<WorkspaceId>, cwd: PathBuf, text: String| LauncherPreset {
                project,
                checkout: Some(cwd),
                prompt: Some(text),
                handoff: handoff.clone(),
                ..LauncherPreset::default()
            };
        // The project whose folder holds the checkout (its main checkout, or a folder in it).
        let by_folder = {
            let store = self.sessions.read(cx);
            store
                .workspaces()
                .iter()
                .filter(|w| w.kind == WorkspaceKind::Project)
                .find(|w| w.roots.iter().any(|root| cwd.starts_with(root)))
                .map(|w| w.id)
        };
        if by_folder.is_some() {
            self.open_launcher(preset(by_folder, cwd, text), window, cx);
            return;
        }
        // A worktree lives next to its repository: git says which repository it belongs to.
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_all_worktrees(cx));
        cx.spawn_in(window, async move |this, cx| {
            let repo_root = listing
                .await
                .ok()
                .and_then(|list| list.into_iter().find(|w| w.path == cwd))
                .map(|w| w.repo_root);
            this.update_in(cx, |this, window, cx| {
                let project = repo_root.and_then(|root| {
                    this.sessions
                        .read(cx)
                        .workspaces()
                        .iter()
                        .filter(|w| w.kind == WorkspaceKind::Project)
                        .find(|w| project_root_of(w).as_ref() == Some(&root))
                        .map(|w| w.id)
                });
                this.open_launcher(preset(project, cwd, text), window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// A ▶ Start button, an empty state or the Tasks panel asked for the sheet.
    pub(crate) fn open_launcher_from(
        &mut self,
        open: super::actions::LauncherOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = open.project.and_then(|id| id.parse().ok());
        self.open_launcher(
            LauncherPreset {
                project,
                queue: open.queue,
                ..LauncherPreset::default()
            },
            window,
            cx,
        );
    }

    pub fn close_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.launcher.take() {
            self.leave_launcher(old, cx);
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    // ---- choosing ---------------------------------------------------------------------------

    /// Chooses the project the sheet starts in.
    pub fn launcher_set_project(&mut self, project: WorkspaceId, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting {
            return;
        }
        let changed = launcher.project != Some(project);
        launcher.project = Some(project);
        launcher.confirm = None;
        launcher.project_query.clear();
        launcher.project_sel = 0;
        if changed {
            launcher.place = LaunchWhere::Main;
            launcher.worktrees = WorktreeList::Loading;
            launcher.after.clear();
            launcher.wanted_checkout = None;
            launcher.base_edited = false;
            launcher.base.clear();
            self.launcher_load_worktrees(cx);
        }
        cx.notify();
    }

    pub fn launcher_set_kind(&mut self, kind: LaunchKind, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        launcher.kind_defaulted = false;
        if launcher.starting || launcher.kind == kind {
            return;
        }
        launcher.kind = kind;
        launcher.error = None;
        launcher.confirm = None;
        if launcher.open_row == Some(LauncherStop::Model) {
            launcher.open_row = None;
        }
        // A terminal agent runs a harness, never a direct model.
        let direct = launcher.is_direct();
        if kind != LaunchKind::Chat && direct {
            launcher.agent = LaunchAgent::Harness(String::new());
            launcher.agent_chosen = false;
        }
        if (kind == LaunchKind::Chat && direct) || !kind.has_task_fields() {
            launcher.queue = false;
        }
        if !self.launcher_stops(cx).contains(&self.launcher_stop()) {
            self.launcher_set_stop(LauncherStop::What);
        }
        self.launcher_repick_agent(cx);
        self.launcher_load_models(cx);
        cx.notify();
    }

    /// Picks the agent a chat or terminal agent runs. A harness that cannot be started is not
    /// pickable (the sheet offers Install… or Log in… for it instead).
    pub fn launcher_choose_agent(&mut self, agent: LaunchAgent, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        if launcher.starting {
            return;
        }
        match &agent {
            LaunchAgent::Harness(id) => {
                let ready = self
                    .launcher_harness_choices(cx)
                    .iter()
                    .any(|h| h.id == id && h.enabled());
                if !ready {
                    // It cannot be started, but the user is asking about it: the line below the
                    // Agent row says why and offers the way out.
                    if let Some(launcher) = &mut self.launcher {
                        launcher.agent_peek = Some(id.clone());
                    }
                    cx.notify();
                    return;
                }
            }
            LaunchAgent::Direct(provider) => {
                if launcher.kind != LaunchKind::Chat
                    || !self.providers.providers.iter().any(|p| &p.id == provider)
                {
                    return;
                }
            }
        }
        if let Some(launcher) = &mut self.launcher {
            launcher.agent = agent;
            launcher.agent_chosen = true;
            launcher.agent_peek = None;
            launcher.model = None;
            launcher.account = None;
            launcher.safety_peek = None;
            if launcher.open_row == Some(LauncherStop::Model) {
                launcher.open_row = None;
            }
            launcher.error = None;
            launcher.confirm = None;
            if launcher.is_direct() {
                launcher.queue = false;
            }
        }
        self.launcher_load_models(cx);
        cx.notify();
    }

    /// Picks the account of a harness chat (`None`: the defaults decide).
    pub fn launcher_choose_account(&mut self, account: Option<String>, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && !launcher.starting
        {
            launcher.account = account;
            launcher.error = None;
            launcher.confirm = None;
            cx.notify();
        }
    }

    /// The account the sheet would start with, as the Account row names it (tests, inspection).
    pub fn launcher_account(&self) -> Option<&str> {
        self.launcher.as_ref().and_then(|l| l.account.as_deref())
    }

    /// Picks the safety preset of a chat or terminal agent. One the harness cannot honour is
    /// not picked: the line below the row says why (and the choice stays where it was).
    pub fn launcher_choose_safety(&mut self, preset: SafetyPreset, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting {
            return;
        }
        let Some(harness) = launcher.safety_harness().map(str::to_owned) else {
            return;
        };
        if launcher.safety_unavailable(&harness, preset).is_none() {
            launcher.safety = Some(preset);
            launcher.safety_peek = None;
        } else {
            launcher.safety_peek = Some(preset);
        }
        launcher.error = None;
        launcher.confirm = None;
        cx.notify();
    }

    /// Turns the Contain row on or off (terminals, commands, OMP). Not while the *Contained*
    /// preset forces it on: that preset is this boundary.
    pub fn launcher_set_contain(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting || !launcher.contain_available() || launcher.contain_forced() {
            return;
        }
        launcher.contain = on;
        launcher.error = None;
        launcher.confirm = None;
        cx.notify();
    }

    /// Whether a contained session may use the network.
    pub fn launcher_set_contain_network(&mut self, network: bool, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting || !launcher.contain_available() {
            return;
        }
        launcher.contain_network = network;
        launcher.error = None;
        launcher.confirm = None;
        cx.notify();
    }

    /// What the sheet would ask the daemon to contain (tests, inspection).
    pub fn launcher_contain(&self) -> Option<Contain> {
        self.launcher.as_ref().and_then(Launcher::contain_request)
    }

    /// The preset the sheet would start with (tests, inspection).
    pub fn launcher_safety(&self) -> Option<SafetyPreset> {
        self.launcher.as_ref().and_then(Launcher::effective_safety)
    }

    /// Picks the model of a chat; none: the harness's default.
    pub fn launcher_choose_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && !launcher.starting
        {
            launcher.model = model;
            launcher.open_row = None;
            launcher.confirm = None;
            cx.notify();
        }
    }

    pub fn launcher_set_place(&mut self, place: LaunchWhere, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting {
            return;
        }
        launcher.place = place;
        launcher.error = None;
        launcher.confirm = None;
        let stops = self.launcher_stops(cx);
        if !stops.contains(&self.launcher_stop()) {
            self.launcher_set_stop(LauncherStop::Where);
        }
        cx.notify();
    }

    /// Replaces the text of a field and lets what follows it follow.
    pub fn launcher_set_field(&mut self, field: LauncherField, text: &str, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        launcher.input_mut(field).set(text);
        launcher.error = None;
        launcher.confirm = None;
        Self::launcher_edited(launcher, field);
        if field == LauncherField::Project {
            launcher.project_sel = 0;
        }
        cx.notify();
    }

    /// What a change of `field` moves along with it.
    fn launcher_edited(launcher: &mut Launcher, field: LauncherField) {
        match field {
            LauncherField::Prompt => launcher.follow_prompt(),
            LauncherField::Command => launcher.follow_command(),
            LauncherField::Title => {
                launcher.title_edited = !launcher.title.is_empty();
                launcher.follow();
            }
            LauncherField::Branch => launcher.branch_edited = !launcher.branch.is_empty(),
            LauncherField::Base => launcher.base_edited = !launcher.base.is_empty(),
            _ => {}
        }
    }

    /// Runs `f` over the text field `field` of the sheet, then lets what follows the text
    /// follow it when it changed: the platform's text input, the mouse and the keys edit the
    /// sheet's fields through this.
    fn with_launcher_field(&mut self, field: LauncherField, f: &mut dyn FnMut(&mut FieldMut<'_>)) {
        let vim_on = self.vim_config().enabled;
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let before = launcher.input_mut(field).text().to_owned();
        let (vim, input) = launcher.vim_and_input(field);
        f(&mut FieldMut {
            input,
            vim: vim_on.then_some(vim),
            multiline: field == LauncherField::Prompt,
        });
        if launcher.input_mut(field).text() != before {
            launcher.error = None;
            Self::launcher_edited(launcher, field);
            if field == LauncherField::Project {
                launcher.project_sel = 0;
            }
        }
    }

    /// How the element of the sheet's field `field` reaches its text.
    pub(crate) fn launcher_field_ref(&self, field: LauncherField, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.launcher_focus.clone(),
            move |this, _, f| {
                this.with_launcher_field(field, f);
            },
        )
    }

    /// "Start now" or "Queue as task". A direct chat has no task to queue.
    pub fn launcher_set_queue(&mut self, queue: bool, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && !launcher.starting
            && !(queue && launcher.is_direct())
            && !(queue && !launcher.kind.has_task_fields())
        {
            launcher.queue = queue;
            launcher.confirm = None;
            cx.notify();
        }
    }

    /// Ticks or unticks a task of the "After" list.
    pub fn launcher_toggle_after(&mut self, task: TaskId, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && !launcher.after.remove(&task)
        {
            launcher.after.insert(task);
        }
        cx.notify();
    }

    /// The inline composer's *Options*: unfolds or folds every row but the prompt and the agent.
    pub fn launcher_toggle_options(&mut self, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && launcher.inline
        {
            launcher.options_open = !launcher.options_open;
            launcher.open_row = None;
            if !launcher.options_open {
                // A row that folded away cannot keep the keyboard.
                launcher.stop = LauncherStop::Options;
            }
            cx.notify();
        }
    }

    pub fn launcher_toggle_more(&mut self, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher {
            launcher.more_open = !launcher.more_open;
            if !launcher.more_open
                && matches!(launcher.stop, LauncherStop::After | LauncherStop::System)
            {
                launcher.stop = LauncherStop::More;
            }
            cx.notify();
        }
    }

    pub(crate) fn launcher_stop(&self) -> LauncherStop {
        self.launcher
            .as_ref()
            .map_or(LauncherStop::Prompt, |l| l.stop)
    }

    fn launcher_set_stop(&mut self, stop: LauncherStop) {
        if let Some(launcher) = &mut self.launcher
            && launcher.stop != stop
        {
            launcher.stop = stop;
            launcher.vim.reset();
        }
    }

    /// Moves the keyboard to `stop` (a click, Tab).
    pub(crate) fn launcher_focus_stop(
        &mut self,
        stop: LauncherStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.launcher_set_stop(stop);
        if let Some(launcher) = &mut self.launcher {
            if launcher.open_row != Some(stop) {
                launcher.open_row = None;
            }
            launcher.confirm = None;
        }
        window.focus(&self.launcher_focus, cx);
        cx.notify();
    }

    // ---- asking the daemon -------------------------------------------------------------------

    /// What the harnesses and providers learned reaches the open sheet: the preselected agent
    /// follows what is installed until the user picks one, and the model list is the agent's.
    pub(crate) fn launcher_on_harnesses_changed(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        // The sheet picked a plain terminal for lack of a ready harness, and one is ready now.
        if launcher.kind_defaulted
            && launcher.kind == LaunchKind::Terminal
            && !launcher.starting
            && self.chat_harness_ready(cx)
        {
            let at_what = launcher.stop == LauncherStop::What;
            self.launcher_set_kind(LaunchKind::Chat, cx);
            if at_what {
                self.launcher_set_stop(LauncherStop::Prompt);
            }
        }
        self.launcher_repick_agent(cx);
        self.launcher_validate_model(cx);
        cx.notify();
    }

    /// The providers listed: a direct chat the sheet waits to name a provider for gets the first.
    pub(crate) fn launcher_on_providers_changed(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        if let LaunchAgent::Direct(provider) = &launcher.agent
            && provider.is_empty()
            && let Some(first) = self.providers.providers.first().map(|p| p.id.clone())
        {
            if let Some(launcher) = &mut self.launcher {
                launcher.agent = LaunchAgent::Direct(first);
            }
            self.launcher_load_models(cx);
        }
        self.launcher_repick_agent(cx);
        self.launcher_validate_model(cx);
        cx.notify();
    }

    /// A model the last start remembered that the agent no longer offers is dropped.
    fn launcher_validate_model(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.launcher.as_ref().and_then(|l| l.model.clone()) else {
            return;
        };
        if let Models::Ready(list) = self.launcher_models(cx)
            && !list.iter().any(|m| m.id == model)
            && let Some(launcher) = &mut self.launcher
        {
            launcher.model = None;
        }
    }

    /// The agent the sheet offers until the user picks one: the one of the last start while it
    /// can be started, else the first harness that can be.
    fn launcher_preferred_agent(
        &self,
        kind: LaunchKind,
        remembered: Option<&LaunchAgent>,
        cx: &gpui::App,
    ) -> LaunchAgent {
        if let Some(remembered) = remembered {
            let usable = match remembered {
                LaunchAgent::Harness(id) => self
                    .launcher_harness_choices(cx)
                    .iter()
                    .any(|h| h.id == id && h.enabled()),
                LaunchAgent::Direct(provider) => {
                    kind == LaunchKind::Chat
                        && self.providers.providers.iter().any(|p| &p.id == provider)
                }
            };
            if usable {
                return remembered.clone();
            }
        }
        let first_ready = self
            .launcher_harness_choices(cx)
            .into_iter()
            .find(|h| h.enabled())
            .map(|h| h.id.to_owned());
        LaunchAgent::Harness(first_ready.unwrap_or_else(|| self.default_harness(cx)))
    }

    /// Until the user picks an agent, the sheet offers the last one that was started, else the
    /// first harness that can be started. A new agent clears the old one's error (not the
    /// sheet's other errors: this runs whenever the harnesses' readiness arrives).
    fn launcher_repick_agent(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        if launcher.agent_chosen {
            // A chosen harness that turned out unusable stays chosen: the sheet says why.
            return;
        }
        let wanted =
            self.launcher_preferred_agent(launcher.kind, launcher.remembered_agent.as_ref(), cx);
        if launcher.agent != wanted {
            let model = (launcher.remembered_agent.as_ref() == Some(&wanted))
                .then(|| launcher.remembered_model.clone())
                .flatten();
            if let Some(launcher) = &mut self.launcher {
                launcher.agent = wanted;
                launcher.model = model;
                // Only what was wrong with the old agent goes: a missing command or prompt, or
                // the daemon's refusal, is still wrong.
                if launcher.error_about_agent {
                    launcher.error = None;
                }
                launcher.confirm = None;
            }
            self.launcher_load_models(cx);
        }
    }

    /// Asks for the models of the chosen agent.
    fn launcher_load_models(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        if launcher.kind != LaunchKind::Chat {
            return;
        }
        match launcher.agent.clone() {
            LaunchAgent::Harness(id) if !id.is_empty() => {
                if self.harness_available(&id, cx) && self.env.harnesses.read(cx).startable(&id) {
                    self.env
                        .harnesses
                        .update(cx, |directory, cx| directory.load_models(&id, cx));
                }
            }
            LaunchAgent::Direct(provider) if !provider.is_empty() => {
                self.provider_models(&provider, cx);
            }
            _ => {}
        }
    }

    /// Asks for the worktrees of the chosen project: where the session can work, and the branch
    /// the main checkout is on.
    fn launcher_load_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let Some(project) = launcher.project else {
            launcher.worktrees = WorktreeList::Ready(Vec::new());
            return;
        };
        if !self.sessions.read(cx).connection().supports_worktrees() {
            launcher.worktrees = WorktreeList::Ready(Vec::new());
            return;
        }
        launcher.worktree_requests += 1;
        let request = launcher.worktree_requests;
        let listing = self
            .sessions
            .update(cx, |store, cx| store.list_worktrees(project, cx));
        cx.spawn(async move |this, cx| {
            let result = listing.await;
            this.update(cx, |this, cx| {
                let Some(launcher) = &mut this.launcher else {
                    return;
                };
                if launcher.worktree_requests != request {
                    return;
                }
                launcher.worktrees = match result {
                    Ok(list) => WorktreeList::Ready(list),
                    Err(error) => WorktreeList::Failed(error),
                };
                this.launcher_worktrees_arrived();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The worktrees are known: the base follows the main checkout's branch, a checkout the
    /// preset asked for is chosen, and a place that vanished falls back to the main checkout.
    fn launcher_worktrees_arrived(&mut self) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if !launcher.base_edited
            && let Some(branch) = launcher.current_branch().map(str::to_owned)
        {
            launcher.base.set(&branch);
        }
        if let Some(wanted) = launcher.wanted_checkout.take() {
            let chosen = match &launcher.worktrees {
                WorktreeList::Ready(list) => list.iter().find(|w| w.path == wanted).map(|w| {
                    if w.main {
                        LaunchWhere::Main
                    } else {
                        w.branch
                            .clone()
                            .map_or(LaunchWhere::Main, LaunchWhere::Existing)
                    }
                }),
                _ => None,
            };
            if let Some(place) = chosen {
                launcher.place = place;
            }
        }
        if let LaunchWhere::Existing(branch) = &launcher.place
            && !launcher
                .existing_worktrees()
                .iter()
                .any(|w| w.branch.as_deref() == Some(branch.as_str()))
        {
            launcher.place = LaunchWhere::Main;
        }
        // A place the last start remembered that this project cannot give: no repository.
        if launcher.place == LaunchWhere::NewWorktree
            && matches!(&launcher.worktrees, WorktreeList::Ready(list) if list.is_empty())
        {
            launcher.place = LaunchWhere::Main;
        }
    }

    // ---- confirming and remembering -----------------------------------------------------------

    /// What a confirmed chat is remembered by: the agent and the model it starts on.
    fn launcher_confirm_key(&self, cx: &gpui::App) -> Option<String> {
        let launcher = self.launcher.as_ref()?;
        let agent = match &launcher.agent {
            LaunchAgent::Harness(id) => id.clone(),
            LaunchAgent::Direct(provider) => format!("direct:{provider}"),
        };
        let model = self
            .launcher_chosen_model(cx)
            .map_or_else(|| "default".to_owned(), |m| m.id);
        Some(format!("{agent}/{model}"))
    }

    /// Whether this start must wait for the user's yes: a chat that sends its prompt to a model
    /// at once, on an agent and model not confirmed before (`launcher.confirm_agent_start`).
    /// When it must, the sheet shows the question and this returns `true`.
    fn launcher_confirm_pending(&mut self, and_new: bool, cx: &mut Context<Self>) -> bool {
        let Some(launcher) = &mut self.launcher else {
            return false;
        };
        let skip = std::mem::take(&mut launcher.skip_confirm);
        let sends_now = launcher.kind == LaunchKind::Chat
            && !launcher.queue
            && !launcher.prompt.text().trim().is_empty();
        if skip
            || !sends_now
            || !self
                .settings
                .get::<crate::settings::LauncherSettings>()
                .confirm_agent_start
        {
            return false;
        }
        let Some(key) = self.launcher_confirm_key(cx) else {
            return false;
        };
        if self.launcher_memory.confirmed.contains(&key) {
            return false;
        }
        let target = format!(
            "{} · {}",
            self.launcher_agent_name(),
            self.launcher_model_name(cx)
        );
        if let Some(launcher) = &mut self.launcher {
            launcher.confirm = Some(StartConfirm {
                and_new,
                remember: false,
                target,
            });
        }
        cx.notify();
        true
    }

    /// The user said yes to the question: the start goes ahead, and with "Don't ask again for
    /// this model" ticked it is not asked again for this agent and model.
    pub fn launcher_confirm_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirm) = self.launcher.as_mut().and_then(|l| l.confirm.take()) else {
            return;
        };
        if confirm.remember
            && let Some(key) = self.launcher_confirm_key(cx)
            && self.launcher_memory.confirmed.insert(key)
        {
            self.schedule_save(cx);
        }
        if let Some(launcher) = &mut self.launcher {
            launcher.skip_confirm = true;
        }
        self.launcher_start(confirm.and_new, window, cx);
    }

    /// The user said no: the sheet is as it was.
    pub fn launcher_cancel_confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher
            && launcher.confirm.take().is_some()
        {
            cx.notify();
        }
    }

    /// Ticks or unticks "Don't ask again for this model" of the question.
    pub fn launcher_toggle_confirm_remember(&mut self, cx: &mut Context<Self>) {
        if let Some(confirm) = self.launcher.as_mut().and_then(|l| l.confirm.as_mut()) {
            confirm.remember = !confirm.remember;
            cx.notify();
        }
    }

    /// What the sheet starts is what it opens on next time.
    fn launcher_remember(&mut self, cx: &mut Context<Self>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        let mut memory = self.launcher_memory.clone();
        memory.kind = Some(launcher.kind.key().to_owned());
        if launcher.kind.is_agent() {
            memory.agent = Some(match &launcher.agent {
                LaunchAgent::Harness(id) => id.clone(),
                LaunchAgent::Direct(provider) => format!("direct:{provider}"),
            });
            if launcher.kind == LaunchKind::Chat {
                memory.model = if launcher.is_direct() {
                    self.launcher_chosen_model(cx).map(|m| m.id)
                } else {
                    launcher.model.clone()
                };
            }
        }
        memory.place = Some(match &launcher.place {
            LaunchWhere::Main => "main".to_owned(),
            LaunchWhere::NewWorktree => "new_worktree".to_owned(),
            LaunchWhere::Existing(branch) => format!("worktree:{branch}"),
        });
        if memory != self.launcher_memory {
            self.launcher_memory = memory;
            self.schedule_save(cx);
        }
    }

    // ---- starting ----------------------------------------------------------------------------

    /// ⌘↩ starts what the sheet describes; ⌥⌘↩ (`and_new`) starts it and keeps the sheet open
    /// for the next one, with the project, what and where kept and the prompt and title cleared.
    pub fn launcher_start(&mut self, and_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.starting {
            return;
        }
        let Some(project) = launcher.project else {
            launcher.fail("Choose a project first.");
            cx.notify();
            return;
        };
        let agent_name = match &launcher.agent {
            LaunchAgent::Harness(id) => harnesses::harness_name(id),
            LaunchAgent::Direct(_) => "Direct model".to_owned(),
        };
        if launcher.kind == LaunchKind::Chat && launcher.is_direct() {
            self.launcher_start_direct(project, and_new, window, cx);
            return;
        }
        let request = match launcher.create_request(project, &agent_name) {
            Ok(request) => request,
            Err(error) => {
                launcher.fail(error);
                cx.notify();
                return;
            }
        };
        // The agent must be one that can be started.
        let blocked = match &launcher.agent {
            LaunchAgent::Harness(id) if launcher.kind.is_agent() => {
                let id = id.clone();
                self.launcher_harness_choices(cx)
                    .into_iter()
                    .find(|h| h.id == id)
                    .filter(|h| !h.enabled())
            }
            _ => None,
        };
        if let Some(harness) = blocked {
            let hint = match harness.readiness {
                Readiness::NeedsInstall(_) => "Install it first.",
                Readiness::NeedsLogin => "Log in first.",
                Readiness::NeedsNewerDaemon => "Restart hutand (Daemon → Restart) first.",
                _ => "It is still being checked.",
            };
            if let Some(launcher) = &mut self.launcher {
                launcher.fail_agent(format!("{} cannot start yet. {hint}", harness.name));
            }
            cx.notify();
            return;
        }
        // A daemon that predates `task.chat_model` would start the chat on its default model.
        if self
            .launcher
            .as_ref()
            .is_some_and(|l| l.model.is_some() && l.kind == LaunchKind::Chat && !l.is_direct())
            && !self
                .sessions
                .read(cx)
                .connection()
                .supports_task_chat_model()
        {
            if let Some(launcher) = &mut self.launcher {
                launcher.fail_agent("This hutand cannot start a chat on a chosen model; restart it (Daemon → Restart) or pick the default.");
            }
            cx.notify();
            return;
        }
        if self.launcher_confirm_pending(and_new, cx) {
            return;
        }
        self.launcher_remember(cx);
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let queue = launcher.queue;
        let handoff = launcher.handoff.clone();
        let title = request.title.clone();
        launcher.starting = true;
        launcher.error = None;
        let creating = self
            .sessions
            .update(cx, |store, cx| store.create_task(request, cx));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let created = match creating.await {
                Ok(task) => task,
                Err(error) => {
                    this.update(cx, |this, cx| this.launcher_failed(error, cx))
                        .ok();
                    return;
                }
            };
            if queue {
                this.update_in(cx, |this, window, cx| {
                    this.launcher_done(and_new, window, cx);
                    this.set_notice(format!("Queued “{title}”: it starts by itself"), cx);
                    this.open_panel("tasks", cx);
                })
                .ok();
                return;
            }
            let Ok(starting) = this.update(cx, |this, cx| {
                this.sessions
                    .update(cx, |store, cx| store.start_task(created.id, cx))
            }) else {
                return;
            };
            match starting.await {
                Ok(started) => {
                    let session = started.session.id;
                    this.update_in(cx, |this, window, cx| {
                        this.launcher_done(and_new, window, cx);
                        this.open_session(session, window, cx);
                        if and_new {
                            this.launcher_refocus(window, cx);
                        }
                        if let Some(warning) = started.warnings.first() {
                            this.set_notice(short_start_warning(warning), cx);
                        }
                        // The agent has the review as its prompt: its comments count as sent.
                        if let Some(handoff) = &handoff {
                            this.review_handed_off(handoff, &started.session.title, cx);
                        }
                    })
                    .ok();
                }
                Err(error) => {
                    // The task never ran: take it back so a retry does not leave two.
                    let removing = this
                        .update(cx, |this, cx| {
                            this.sessions
                                .update(cx, |store, cx| store.delete_task(created.id, false, cx))
                        })
                        .ok();
                    if let Some(removing) = removing {
                        let _ = removing.await;
                    }
                    this.update(cx, |this, cx| {
                        this.launcher_failed(format!("Cannot start: {error}"), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// A direct model chat: no task (`TaskRun::Chat` does not run `direct`), so `chat.create`
    /// (in a worktree made first when asked) and the prompt as its first message.
    fn launcher_start_direct(
        &mut self,
        project: WorkspaceId,
        and_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let LaunchAgent::Direct(provider) = launcher.agent.clone() else {
            return;
        };
        if provider.is_empty() {
            launcher.fail_agent("Configure a model provider first (Settings → Models).");
            cx.notify();
            return;
        }
        let values = launcher.values();
        let prompt = values.prompt.trim().to_owned();
        let title = (!values.title.trim().is_empty()).then(|| values.title.trim().to_owned());
        let place = values.place.clone();
        let system = launcher.system.text().trim().to_owned();
        let main = launcher.main_worktree().map(|w| w.path.clone());
        let chosen = launcher.chosen_worktree().map(|w| w.path.clone());
        if let LaunchPlace::NewWorktree { branch, .. } = &place
            && branch.trim().is_empty()
        {
            launcher.fail("Name the branch of the new worktree.");
            cx.notify();
            return;
        }
        if self.launcher_confirm_pending(and_new, cx) {
            return;
        }
        self.launcher_remember(cx);
        if let Some(launcher) = &mut self.launcher {
            launcher.starting = true;
            launcher.error = None;
        }
        let model = self.launcher_chosen_model(cx).map(|m| m.id);
        let info: Option<WorkspaceInfo> = self.sessions.read(cx).workspace(project).cloned();
        let fallback_cwd = info
            .as_ref()
            .and_then(|w| w.roots.first().cloned())
            .or_else(std::env::home_dir);
        let creating_worktree = match &place {
            LaunchPlace::NewWorktree { branch, base } => {
                let base = base.trim();
                let request = WorktreeCreate {
                    workspace_id: project,
                    repo: None,
                    branch: branch.trim().to_owned(),
                    base: (!base.is_empty()).then(|| base.to_owned()),
                    path: None,
                    no_setup: false,
                };
                Some(
                    self.sessions
                        .update(cx, |store, cx| store.create_worktree(request, cx)),
                )
            }
            _ => None,
        };
        let connection = self.sessions.read(cx).connection().clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let cwd = match creating_worktree {
                Some(creating) => match creating.await {
                    Ok(worktree) => Some(worktree.path),
                    Err(error) => {
                        this.update(cx, |this, cx| this.launcher_failed(error, cx))
                            .ok();
                        return;
                    }
                },
                None => chosen.or(main).or(fallback_cwd),
            };
            let Some(cwd) = cwd else {
                this.update(cx, |this, cx| {
                    this.launcher_failed("The project has no folder to work in.".to_owned(), cx);
                })
                .ok();
                return;
            };
            let created = connection
                .chat_create(ChatCreate {
                    contain: None,
                    workspace_id: Some(project),
                    cwd,
                    agent: "direct".to_owned(),
                    model,
                    approval_policy: None,
                    sandbox: None,
                    title,
                    mode: None,
                    provider_id: Some(provider),
                    system_prompt: (!system.is_empty()).then_some(system),
                    account: None,
                    safety: None,
                })
                .await;
            let info = match created {
                Ok(info) => info,
                Err(error) => {
                    this.update(cx, |this, cx| this.launcher_failed(error.to_string(), cx))
                        .ok();
                    return;
                }
            };
            let sent = if prompt.is_empty() {
                Ok(())
            } else {
                connection.chat_send(info.id, prompt).await.map(|_| ())
            };
            this.update_in(cx, |this, window, cx| {
                this.launcher_done(and_new, window, cx);
                this.open_session(info.id, window, cx);
                if and_new {
                    this.launcher_refocus(window, cx);
                }
                if let Err(error) = sent {
                    this.set_notice(
                        format!("The chat started, its first message did not go: {error}"),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// A start went through: close the sheet, or clear it for the next one.
    fn launcher_done(&mut self, and_new: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !and_new {
            if let Some(old) = self.launcher.take() {
                self.leave_launcher(old, cx);
            }
            self.focus_root(window, cx);
            cx.notify();
            return;
        }
        if let Some(launcher) = &mut self.launcher {
            launcher.starting = false;
            launcher.error = None;
            launcher.handoff = None;
            launcher.prompt.clear();
            launcher.title.clear();
            launcher.command.clear();
            launcher.title_edited = false;
            launcher.branch_edited = false;
            launcher.follow();
            launcher.after.clear();
            launcher.vim.reset();
            launcher.stop = if launcher.kind.takes_prompt() {
                LauncherStop::Prompt
            } else if launcher.kind == LaunchKind::Command {
                LauncherStop::Command
            } else if launcher.kind.has_task_fields() {
                LauncherStop::Title
            } else {
                LauncherStop::What
            };
        }
        // A worktree the start made is a place now.
        self.launcher_load_worktrees(cx);
        cx.notify();
    }

    /// The sheet keeps the keyboard after a session opened behind it.
    pub(crate) fn launcher_refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.launcher.is_some() {
            window.focus(&self.launcher_focus, cx);
        }
    }

    fn launcher_failed(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(launcher) = &mut self.launcher {
            launcher.starting = false;
            launcher.fail(error);
        }
        cx.notify();
    }

    // ---- keyboard ----------------------------------------------------------------------------

    /// Return in the prompt (`chat.send_key`): starts, or adds a line, as the setting says;
    /// ⇧Return and ⌥Return always add one. A Return that commits an input method's marked text
    /// only commits it, and in Vim's Normal mode it is no newline.
    fn launcher_prompt_return(
        &mut self,
        shift: bool,
        alt: bool,
        vim_on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sends = self
            .settings
            .get::<crate::settings::ChatSettings>()
            .send_key
            .return_sends(shift, alt);
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let normal = vim_on && launcher.vim.mode() != super::chat::vim::Mode::Insert;
        let (_, input) = launcher.vim_and_input(LauncherField::Prompt);
        if input.marked().is_some() {
            input.unmark();
        } else if sends {
            self.launcher_start(false, window, cx);
            return;
        } else if !normal {
            input.insert("\n");
        }
        cx.notify();
    }

    /// A key in the sheet; the stop says what it edits.
    pub(crate) fn launcher_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        let ks = &event.keystroke;
        let stop = launcher.stop;
        let vim = VimConfig::of(&self.settings);
        // The question of a first start is answered before anything else: ↩ or ⌘↩ says yes, Esc
        // no, Space ticks "Don't ask again for this model".
        if launcher.confirm.is_some() {
            match ks.key.as_str() {
                "enter" => self.launcher_confirm_start(window, cx),
                "escape" => self.launcher_cancel_confirm(cx),
                "space" => self.launcher_toggle_confirm_remember(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        if ks.key == "enter" && ks.modifiers.platform {
            // ⌘↩ never fires while a row's choices are open, or a project is being searched.
            let choosing = launcher.open_row.is_some()
                || (stop == LauncherStop::Project && !launcher.project_query.is_empty());
            if !choosing {
                self.launcher_start(ks.modifiers.alt, window, cx);
            }
            cx.stop_propagation();
            return;
        }
        let field = Launcher::field_of(stop);
        match ks.key.as_str() {
            "escape" => {
                // Under Vim mode the first Escape leaves Insert (or Visual); then the list, then
                // the sheet closes.
                if vim.enabled
                    && let Some(field) = field
                    && let Some(launcher) = &mut self.launcher
                {
                    let multiline = field == LauncherField::Prompt;
                    let (state, input) = launcher.vim_and_input(field);
                    if super::vim_ui::apply(state, input, event, vim.options(!multiline, true), cx)
                        == Outcome::Handled
                    {
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                }
                if let Some(launcher) = &mut self.launcher {
                    if launcher.open_row.is_some() {
                        launcher.open_row = None;
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                    if stop == LauncherStop::Project && !launcher.project_query.is_empty() {
                        launcher.project_query.clear();
                        launcher.project_sel = 0;
                        cx.notify();
                        cx.stop_propagation();
                        return;
                    }
                }
                if self.launcher.as_ref().is_some_and(|l| l.inline) {
                    // The composer is the centre, not a sheet: Escape only gives the keyboard
                    // back to the window.
                    self.focus_root(window, cx);
                    cx.notify();
                } else {
                    self.close_launcher(window, cx);
                }
            }
            "tab" => {
                let stops = self.launcher_stops(cx);
                let at = stops.iter().position(|s| *s == stop).unwrap_or(0);
                let next = if ks.modifiers.shift {
                    stops[(at + stops.len() - 1) % stops.len()]
                } else {
                    stops[(at + 1) % stops.len()]
                };
                self.launcher_focus_stop(next, window, cx);
            }
            key => match stop {
                LauncherStop::Project => {
                    if !self.launcher_project_key(key, event, vim, cx) {
                        return;
                    }
                }
                LauncherStop::What
                | LauncherStop::Agent
                | LauncherStop::Where
                | LauncherStop::Mode
                | LauncherStop::Model => self.launcher_row_key(key, stop, cx),
                LauncherStop::More => match key {
                    "space" | "enter" | "left" | "right" => self.launcher_toggle_more(cx),
                    _ => return,
                },
                LauncherStop::Options => match key {
                    "space" | "enter" | "left" | "right" => self.launcher_toggle_options(cx),
                    _ => return,
                },
                LauncherStop::After => self.launcher_after_key(key, cx),
                _ => {
                    let Some(field) = field else { return };
                    // Return starts from a one-line field; in the prompt it follows the send
                    // key (`chat.send_key`).
                    if key == "enter" && field != LauncherField::Prompt {
                        self.launcher_start(false, window, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if key == "enter" && !ks.modifiers.control {
                        self.launcher_prompt_return(
                            ks.modifiers.shift,
                            ks.modifiers.alt,
                            vim.enabled,
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                        return;
                    }
                    let mut used = false;
                    self.with_launcher_field(field, &mut |f| used = edit_field(f, vim, event, cx));
                    if !used {
                        return;
                    }
                    cx.notify();
                }
            },
        }
        cx.stop_propagation();
    }

    /// A key at the Project stop. `false` when it is none of the list's or the field's (a text
    /// key: it reaches the field through the platform's text input).
    fn launcher_project_key(
        &mut self,
        key: &str,
        event: &KeyDownEvent,
        vim: VimConfig,
        cx: &mut Context<Self>,
    ) -> bool {
        let matches = self.launcher_project_matches(cx);
        let Some(launcher) = &mut self.launcher else {
            return false;
        };
        match key {
            "down" => {
                launcher.project_sel =
                    (launcher.project_sel + 1).min(matches.len().saturating_sub(1))
            }
            "up" => launcher.project_sel = launcher.project_sel.saturating_sub(1),
            "enter" => {
                if let Some(project) = matches.get(launcher.project_sel) {
                    let id = project.id;
                    self.launcher_set_project(id, cx);
                }
            }
            _ => {
                let mut used = false;
                self.with_launcher_field(LauncherField::Project, &mut |field| {
                    used = edit_field(field, vim, event, cx);
                });
                if used {
                    cx.notify();
                }
                return used;
            }
        }
        cx.notify();
        true
    }

    /// ← → (↑ ↓) cycle the choices of a row.
    fn launcher_cycle_key(&mut self, key: &str, stop: LauncherStop, cx: &mut Context<Self>) {
        let step: isize = match key {
            "left" | "up" => -1,
            "right" | "down" => 1,
            _ => return,
        };
        let cycle = |len: usize, at: usize| -> usize {
            ((at as isize + step).rem_euclid(len as isize)) as usize
        };
        match stop {
            LauncherStop::What => {
                let at = LaunchKind::ALL
                    .iter()
                    .position(|k| Some(*k) == self.launcher.as_ref().map(|l| l.kind))
                    .unwrap_or(0);
                self.launcher_set_kind(LaunchKind::ALL[cycle(LaunchKind::ALL.len(), at)], cx);
            }
            LauncherStop::Agent => {
                let mut choices: Vec<LaunchAgent> = self
                    .launcher_harness_choices(cx)
                    .into_iter()
                    .filter(HarnessChoice::enabled)
                    .map(|h| LaunchAgent::Harness(h.id.to_owned()))
                    .collect();
                if self
                    .launcher
                    .as_ref()
                    .is_some_and(|l| l.kind == LaunchKind::Chat)
                {
                    choices.extend(
                        self.providers
                            .providers
                            .iter()
                            .map(|p| LaunchAgent::Direct(p.id.clone())),
                    );
                }
                if choices.is_empty() {
                    return;
                }
                let current = self.launcher.as_ref().map(|l| l.agent.clone());
                let at = choices
                    .iter()
                    .position(|c| Some(c) == current.as_ref())
                    .unwrap_or(0);
                self.launcher_choose_agent(choices[cycle(choices.len(), at)].clone(), cx);
            }
            LauncherStop::Where => {
                let Some(launcher) = &self.launcher else {
                    return;
                };
                let mut places = vec![LaunchWhere::Main, LaunchWhere::NewWorktree];
                places.extend(
                    launcher
                        .existing_worktrees()
                        .into_iter()
                        .filter_map(|w| w.branch.clone().map(LaunchWhere::Existing)),
                );
                let at = places
                    .iter()
                    .position(|p| *p == launcher.place)
                    .unwrap_or(0);
                self.launcher_set_place(places[cycle(places.len(), at)].clone(), cx);
            }
            _ => {}
        }
    }

    /// A row of choices (What, Agent, Model, Where, Start now / Queue): ↩ or Space opens its
    /// choices; ← → ↑ ↓ change the value only while they are open. A key on a closed row does
    /// nothing, so a stray arrow never changes what Start would do.
    fn launcher_row_key(&mut self, key: &str, stop: LauncherStop, cx: &mut Context<Self>) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        let open = launcher.open_row == Some(stop);
        match key {
            "enter" | "space" => {
                launcher.open_row = (!open).then_some(stop);
                if !open && stop == LauncherStop::Model {
                    self.launcher_reveal_model(cx);
                }
                cx.notify();
            }
            "left" | "right" | "up" | "down" if open => match stop {
                LauncherStop::Model => self.launcher_model_key(key, cx),
                LauncherStop::Mode => {
                    let queue = launcher.queue;
                    self.launcher_set_queue(!queue, cx);
                }
                _ => self.launcher_cycle_key(key, stop, cx),
            },
            _ => {}
        }
    }

    /// ← → ↑ ↓ change the model of the open Model row.
    fn launcher_model_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Models::Ready(models) = self.launcher_models(cx) else {
            return;
        };
        if models.is_empty() {
            return;
        }
        let current = self.launcher_chosen_model(cx).map(|m| m.id);
        let at = models
            .iter()
            .position(|m| Some(&m.id) == current.as_ref())
            .unwrap_or(0);
        let next = match key {
            "down" | "right" => (at + 1).min(models.len() - 1),
            "up" | "left" => at.saturating_sub(1),
            _ => return,
        };
        let model = &models[next];
        self.launcher_choose_model((!model.is_default).then(|| model.id.clone()), cx);
        if let Some(launcher) = &mut self.launcher {
            // A direct chat always names its model.
            if launcher.is_direct() {
                launcher.model = Some(model.id.clone());
            }
            launcher.open_row = Some(LauncherStop::Model);
        }
        self.launcher_reveal_model(cx);
    }

    /// ↑ ↓ walk the After list, Space ticks.
    fn launcher_after_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let candidates = self.launcher_dependency_candidates(cx);
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        match key {
            "down" => {
                launcher.after_cursor =
                    (launcher.after_cursor + 1).min(candidates.len().saturating_sub(1))
            }
            "up" => launcher.after_cursor = launcher.after_cursor.saturating_sub(1),
            "space" | "enter" => {
                if let Some(task) = candidates.get(launcher.after_cursor).copied() {
                    self.launcher_toggle_after(task, cx);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// The tasks of the project the new one can wait for, as the tree lists them.
    pub fn launcher_dependency_candidates(&self, cx: &gpui::App) -> Vec<TaskId> {
        self.launcher_dependency_rows(cx)
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    pub(crate) fn launcher_dependency_rows(
        &self,
        cx: &gpui::App,
    ) -> Vec<crate::sessions::tasks::TaskRowView> {
        let Some(project) = self.launcher.as_ref().and_then(|l| l.project) else {
            return Vec::new();
        };
        let store = self.sessions.read(cx);
        let parent = |id| store.workspace(id).and_then(|w| w.parent_id);
        tree_rows(
            store.tasks(),
            TaskFilters {
                show_done: true,
                workspace: Some(project),
            },
            &parent,
            0,
            store.model_names(),
        )
    }
}

#[cfg(test)]
mod start_warning_tests {
    use super::short_start_warning;

    #[test]
    fn the_shared_checkout_sentence_is_short_and_other_warnings_are_clipped() {
        let daemon = "1 other live agent(s) already work in /Users/me/proj/acme-api: Look around \
                      the project and tell me what you find. (01M5FQCT2V6JRRVA8Q58PTFQTG); their \
                      edits and this task's can overwrite each other";
        assert_eq!(
            short_start_warning(daemon),
            "Another agent already works in this checkout"
        );
        assert_eq!(
            short_start_warning(&daemon.replacen("1 other", "3 other", 1)),
            "3 other agents already work in this checkout"
        );
        assert_eq!(short_start_warning("short"), "short");
        let long = "x".repeat(200);
        let shown = short_start_warning(&long);
        assert_eq!(shown.chars().count(), 81);
        assert!(shown.ends_with('…'));
    }
}

#[cfg(test)]
mod contain_tests {
    use hutan_core::SafetyPreset;

    use super::preset_unavailable;

    #[test]
    fn omp_contained_needs_a_daemon_that_offers_contain_and_nothing_else_does() {
        assert_eq!(
            preset_unavailable("omp", SafetyPreset::Contained, true),
            None
        );
        let reason = preset_unavailable("omp", SafetyPreset::Contained, false).unwrap();
        assert!(reason.contains("protocol 0.52"), "{reason}");
        for agent in ["codex", "claude"] {
            for preset in SafetyPreset::ALL {
                assert_eq!(
                    preset_unavailable(agent, preset, false),
                    None,
                    "{agent} {preset}: their own sandbox needs no Contain"
                );
            }
        }
        assert_eq!(preset_unavailable("omp", SafetyPreset::Ask, false), None);
    }
}
