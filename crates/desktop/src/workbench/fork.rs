//! Fork a chat from a turn, in the window (CHAT-FORK-B; protocol 0.56 `chat.fork`, 0.58
//! `chat.fork.worktree`, ADR 0023; the spike's `mock-fork.svg`): the popover the turn footer's
//! and the message bar's *Fork…* open, the sidebar row's ⑂, the session menu's *Fork…* and
//! *Fork Session*, and ⌘K *Chat: Fork from Here*.
//!
//! The popover says **what the new chat will know** and never decides for the daemon: the cut
//! (`exact`, or — where the harness cannot cut at that turn — a *choice* between the latest
//! turn and a summary that nothing happens without), *Where* (this folder, or a new worktree
//! whose files are the turn's checkpoint; the choice is remembered in `chat.fork_where`), the
//! branch, *Restore files to the end of the turn* (this folder), *Archive the original*, and the
//! paths the checkpoint left out. A refusal of the daemon (`unsupported` with the cuts that
//! would work) turns the popover into the same choice; any other refusal is said in it, in the
//! daemon's words. What happened is said in the status line after the new chat opens.
//!
//! For a terminal agent *Fork Session* starts the harness's own CLI fork in a new terminal
//! (`omp --fork`, `claude --resume --fork-session`, `codex fork`) when the agent has reported its
//! session id; the conversation is the agent's, so only its head can be forked.

use gpui::{
    AnyElement, Context, FocusHandle, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_core::{CheckpointAvailability, ForkMode, SessionId, TurnId};
use hutan_protocol::chat::{ChatFork, ChatForked, ForkWhere};
use hutan_protocol::messages::TerminalSpec;

use super::actions::palette;
use super::chat::ended_turns;
use super::render::probe;
use super::rewind::turn_prompt;
use super::ui::motion::{self, PresentExt as _};
use super::ui::scroll::ScrollExt as _;
use super::ui::{self, Button, ButtonKind, StyledExt as _};
use super::view::{MenuEntry, Region, Workbench};
use super::vim_ui::OverlayField;
use crate::icons::Icon;
use crate::sessions::fork::{
    self, ForkFailure, Offer, Refused, TerminalFork, cut_words, exact_line,
};
use crate::settings::{ChatSettings, ForkWhereSetting, TerminalSettings};
use crate::theme::DensityExt as _;

const WIDTH: f32 = 460.0;
/// Most skipped paths the popover lists; the rest is counted.
const SKIPPED_SHOWN: usize = 4;

/// The fork popover: which chat and turn, where it hangs, the choices made so far.
#[derive(Debug)]
pub(crate) struct ForkControl {
    pub(crate) focus: FocusHandle,
    pub(crate) open: Option<Open>,
}

#[derive(Debug, Clone)]
pub(crate) struct Open {
    session: SessionId,
    /// The turn the new chat ends with; `None`: the chat's latest (the window does not know it).
    turn: Option<TurnId>,
    /// The turn's place among the chat's turns, when the transcript shows them all.
    ordinal: Option<u32>,
    /// The first line of the turn's prompt.
    prompt: Option<String>,
    /// The turn is the chat's newest (what a harness that forks only at its head can cut at).
    newest: bool,
    /// Where the button was clicked: the popover hangs by it.
    at: Point<Pixels>,
    /// False while the popover fades out (its state stays for the exit).
    shown: bool,
    where_: ForkWhereSetting,
    /// The new worktree's branch; empty: the daemon names it.
    branch: String,
    restore_files: bool,
    archive_parent: bool,
    /// The cut the user chose when the harness cannot make the exact one.
    choice: Option<ForkMode>,
    /// The daemon refused the exact cut: the cuts it offers instead.
    refused: Option<Refused>,
    /// The last thing that went wrong, said in the popover.
    problem: Option<String>,
    /// A request is in flight: its buttons wait.
    busy: bool,
}

impl ForkControl {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self { focus, open: None }
    }

    /// The new worktree's branch while its field is on screen.
    pub(crate) fn branch(&self) -> Option<&str> {
        self.open
            .as_ref()
            .filter(|open| open.shown && open.where_ == ForkWhereSetting::NewWorktree)
            .map(|open| open.branch.as_str())
    }

    pub(crate) fn branch_mut(&mut self) -> Option<&mut String> {
        self.open
            .as_mut()
            .filter(|open| open.shown && open.where_ == ForkWhereSetting::NewWorktree)
            .map(|open| &mut open.branch)
    }

    /// Typing in the branch field takes an old complaint away.
    pub(crate) fn clear_problem(&mut self) {
        if let Some(open) = self.open.as_mut() {
            open.problem = None;
        }
    }
}

/// What is known of the turn a fork starts from.
struct TurnFacts {
    turn: Option<TurnId>,
    ordinal: Option<u32>,
    prompt: Option<String>,
    newest: bool,
}

/// Everything the popover says, decided once per frame (and read by tests as lines).
struct View {
    title: String,
    quote: Option<String>,
    /// The daemon can fork into a worktree, restore files and archive (0.58).
    worktree_capable: bool,
    where_: ForkWhereSetting,
    branch: String,
    branch_placeholder: String,
    /// What a new worktree gets, in a sentence.
    worktree_note: Option<String>,
    skipped: Vec<(String, String)>,
    skipped_more: usize,
    restore: Option<Toggle>,
    archive: Option<Toggle>,
    cut: Cut,
    fork_label: String,
    can_fork: bool,
}

struct Toggle {
    label: String,
    note: String,
    checked: bool,
    enabled: bool,
}

enum Cut {
    /// One line: how exact the fork is.
    Exact(String),
    /// A fallback to choose: the sentence about why, and the options.
    Choose {
        note: String,
        options: Vec<(ForkMode, String, String, bool)>,
    },
    Never(String),
}

fn clip(text: &str, chars: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= chars {
        line.to_owned()
    } else {
        let cut: String = line.chars().take(chars.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

impl Workbench {
    // ---- state ---------------------------------------------------------------------------------

    /// The chat whose fork popover is open.
    pub fn fork_popover_session(&self) -> Option<SessionId> {
        self.fork
            .open
            .as_ref()
            .filter(|open| open.shown)
            .map(|open| open.session)
    }

    /// What the popover says went wrong, if it does.
    pub fn fork_popover_problem(&self) -> Option<&str> {
        self.fork
            .open
            .as_ref()
            .filter(|open| open.shown)
            .and_then(|open| open.problem.as_deref())
    }

    /// What the open popover says, one line per thing, each with its role as a prefix: `title`,
    /// `quote`, `where`, `worktree`, `left out`, `option`, `cut`, `choice`, `button`.
    pub fn fork_popover_lines(&self, cx: &gpui::App) -> Vec<String> {
        let Some(view) = self.fork_view(cx) else {
            return Vec::new();
        };
        let mut lines = vec![format!("title: {}", view.title)];
        lines.extend(view.quote.map(|quote| format!("quote: {quote}")));
        if view.worktree_capable {
            lines.push(match view.where_ {
                ForkWhereSetting::ThisFolder => "where: this folder".to_owned(),
                ForkWhereSetting::NewWorktree => "where: new worktree".to_owned(),
            });
        }
        if view.where_ == ForkWhereSetting::NewWorktree && view.worktree_capable {
            lines.push(format!("branch: {}", view.branch));
        }
        lines.extend(view.worktree_note.map(|note| format!("worktree: {note}")));
        for (path, why) in &view.skipped {
            lines.push(format!("left out: {path} · {why}"));
        }
        if view.skipped_more > 0 {
            lines.push(format!("left out: and {} more", view.skipped_more));
        }
        for toggle in view.restore.iter().chain(view.archive.iter()) {
            let state = match (toggle.enabled, toggle.checked) {
                (false, _) => "unavailable",
                (true, true) => "on",
                (true, false) => "off",
            };
            lines.push(format!(
                "option: {} [{state}] {}",
                toggle.label, toggle.note
            ));
        }
        match &view.cut {
            Cut::Exact(line) => lines.push(format!("cut: {line}")),
            Cut::Choose { note, options } => {
                lines.push(format!("cut: choose — {note}"));
                for (_, title, _, selected) in options {
                    lines.push(format!(
                        "choice: {title}{}",
                        if *selected { " [chosen]" } else { "" }
                    ));
                }
            }
            Cut::Never(note) => lines.push(format!("cut: none — {note}")),
        }
        lines.push(format!(
            "button: {}{}",
            view.fork_label,
            if view.can_fork { "" } else { " [disabled]" }
        ));
        lines
    }

    /// The turn facts of `session` for `turn` (`None`: the newest ended one), or why there is
    /// no turn to fork at.
    fn fork_turn_facts(
        &self,
        session: SessionId,
        turn: Option<TurnId>,
        cx: &gpui::App,
    ) -> Result<TurnFacts, String> {
        let Some(model) = self.chat_model_of(session) else {
            return Ok(TurnFacts {
                turn,
                ordinal: None,
                prompt: None,
                newest: turn.is_none(),
            });
        };
        let model = model.read(cx);
        let transcript = model.transcript();
        if transcript.turn_active() {
            return Err(
                "A turn is running in this chat: wait for it to end (or stop it) before forking"
                    .to_owned(),
            );
        }
        let ended = ended_turns(transcript, !model.has_earlier());
        let picked = match turn {
            Some(turn) => ended.iter().find(|e| e.turn == turn),
            None => ended.last(),
        };
        let Some(picked) = picked else {
            return Err(match turn {
                Some(_) => "That turn has not ended yet".to_owned(),
                None => {
                    "This chat has no finished turn to fork at yet: send a message first".to_owned()
                }
            });
        };
        Ok(TurnFacts {
            turn: Some(picked.turn),
            ordinal: picked.ordinal,
            prompt: turn_prompt(model, picked.turn).map(|p| clip(&p, 120)),
            newest: picked.newest,
        })
    }

    /// Whether `session` can be forked at all from here: a chat the daemon can fork, not an
    /// assistant, whose agent has not said it cannot.
    fn can_fork_chat(&self, session: SessionId, cx: &gpui::App) -> Result<(), String> {
        let store = self.sessions.read(cx);
        if !store.supports_chat_fork() {
            return Err("This daemon cannot fork a chat (it needs protocol 0.56)".to_owned());
        }
        let Some(chat) = store.session(session).and_then(|s| s.chat.as_ref()) else {
            return Err("Only a chat can be forked".to_owned());
        };
        if chat.is_assistant() {
            return Err("Mawas's chats cannot be forked".to_owned());
        }
        if chat
            .fork
            .as_ref()
            .is_some_and(|fork| fork.support == hutan_core::ForkSupport::None)
        {
            return Err("This chat cannot be forked".to_owned());
        }
        Ok(())
    }

    // ---- opening -------------------------------------------------------------------------------

    /// Opens the popover for `turn` of `session` (`None`: its latest turn), hanging by `at`.
    pub fn show_fork_popover(
        &mut self,
        session: SessionId,
        turn: Option<TurnId>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_fork_popover(session, turn, at, None, window, cx);
    }

    fn open_fork_popover(
        &mut self,
        session: SessionId,
        turn: Option<TurnId>,
        at: Point<Pixels>,
        refused: Option<(Refused, Option<String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(why) = self.can_fork_chat(session, cx) {
            self.set_notice(why, cx);
            return;
        }
        let facts = match self.fork_turn_facts(session, turn, cx) {
            Ok(facts) => facts,
            Err(why) => {
                self.set_notice(why, cx);
                return;
            }
        };
        let remembered = self.settings.get::<ChatSettings>().fork_where;
        let where_ = if self.sessions.read(cx).supports_chat_fork_worktree() {
            remembered
        } else {
            ForkWhereSetting::ThisFolder
        };
        self.palette = None;
        self.context_menu = None;
        self.fork.open = Some(Open {
            session,
            turn: facts.turn,
            ordinal: facts.ordinal,
            prompt: facts.prompt,
            newest: facts.newest,
            at,
            shown: true,
            where_,
            branch: String::new(),
            restore_files: false,
            archive_parent: false,
            choice: None,
            problem: refused.as_ref().and_then(|(_, problem)| problem.clone()),
            refused: refused.map(|(refused, _)| refused),
            busy: false,
        });
        self.edits.fork_branch = super::vim_ui::FieldEdit::new();
        window.focus(&self.fork.focus, cx);
        cx.notify();
    }

    pub fn close_fork_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.fork.open.as_mut()
            && std::mem::take(&mut open.shown)
        {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// ⌘K *Chat: Fork from Here*: the popover for the turn the focused chat is showing: its
    /// newest while it follows its end, else the one at the bottom of the window.
    pub fn fork_focused_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.focused_session() else {
            self.set_notice("Fork works on the focused chat", cx);
            return;
        };
        let Some(view) = self.chat_view_of(session) else {
            self.set_notice("Fork works on a chat: open one first", cx);
            return;
        };
        let Some(turn) = view.read(cx).turn_in_view(cx) else {
            self.set_notice(
                "This chat has no finished turn to fork at yet: send a message first",
                cx,
            );
            return;
        };
        let at = self.region_bounds(Region::Chat("header")).map_or_else(
            || Point::new(px(240.0), px(120.0)),
            |header| Point::new(header.left() + px(120.0), header.bottom()),
        );
        self.show_fork_popover(session, Some(turn), at, window, cx);
    }

    /// The sidebar row's ⑂ (and the session menu's *Fork…*): the chat at its latest turn. With
    /// nothing to ask — the harness cuts there, the remembered place is this folder — it forks
    /// at once and opens the new chat; otherwise the popover opens by the row.
    pub fn fork_session_latest(
        &mut self,
        session: SessionId,
        at: Point<Pixels>,
        always_ask: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(why) = self.can_fork_chat(session, cx) {
            self.set_notice(why, cx);
            return;
        }
        let support = self
            .sessions
            .read(cx)
            .session(session)
            .and_then(|s| s.chat.as_ref())
            .and_then(|chat| chat.fork.as_ref())
            .map(|fork| fork.support);
        let remembered = self.settings.get::<ChatSettings>().fork_where;
        let asks = always_ask
            || (remembered == ForkWhereSetting::NewWorktree
                && self.sessions.read(cx).supports_chat_fork_worktree())
            || !matches!(fork::offer(support, true), Offer::Exact);
        if asks {
            self.show_fork_popover(session, None, at, window, cx);
            return;
        }
        let facts = match self.fork_turn_facts(session, None, cx) {
            Ok(facts) => facts,
            Err(why) => {
                self.set_notice(why, cx);
                return;
            }
        };
        let request = ChatFork {
            session_id: session,
            turn_id: facts.turn,
            mode: ForkMode::Exact,
            title: None,
            where_: ForkWhere::ThisFolder,
            branch: None,
            restore_files: false,
            archive_parent: false,
        };
        self.start_fork(session, request, facts.ordinal, at, window, cx);
    }

    // ---- choosing ------------------------------------------------------------------------------

    fn with_fork_open(&mut self, change: impl FnOnce(&mut Open), cx: &mut Context<Self>) {
        if let Some(open) = self.fork.open.as_mut().filter(|open| open.shown) {
            if open.busy {
                return;
            }
            change(open);
            open.problem = None;
            cx.notify();
        }
    }

    /// *Where*: this folder, or a new worktree.
    pub fn set_fork_where(&mut self, where_: ForkWhereSetting, cx: &mut Context<Self>) {
        self.with_fork_open(
            |open| {
                open.where_ = where_;
                if where_ == ForkWhereSetting::NewWorktree {
                    open.restore_files = false;
                }
            },
            cx,
        );
    }

    /// *Restore files to the end of the turn*.
    pub fn toggle_fork_restore(&mut self, cx: &mut Context<Self>) {
        self.with_fork_open(|open| open.restore_files = !open.restore_files, cx);
    }

    /// *Archive the original*.
    pub fn toggle_fork_archive(&mut self, cx: &mut Context<Self>) {
        self.with_fork_open(|open| open.archive_parent = !open.archive_parent, cx);
    }

    /// The cut the user picks when the harness cannot make the exact one.
    pub fn choose_fork_cut(&mut self, mode: ForkMode, cx: &mut Context<Self>) {
        self.with_fork_open(|open| open.choice = Some(mode), cx);
    }

    // ---- the view ------------------------------------------------------------------------------

    fn fork_view(&self, cx: &gpui::App) -> Option<View> {
        let open = self.fork.open.as_ref().filter(|open| open.shown)?;
        self.view_of(open, cx)
    }

    /// What the popover of `open` says.
    fn view_of(&self, open: &Open, cx: &gpui::App) -> Option<View> {
        let store = self.sessions.read(cx);
        let info = store.session(open.session)?;
        let chat = info.chat.as_ref()?;
        let n = open.ordinal;
        let worktree_capable = store.supports_chat_fork_worktree();
        let checkpoints = store.checkpoints().read(cx);
        let checkpoint = open
            .turn
            .and_then(|turn| checkpoints.of_turn(open.session, turn));
        let unavailable = chat
            .checkpoints
            .as_ref()
            .map(|status| status.state)
            .filter(|state| {
                matches!(
                    state,
                    CheckpointAvailability::Off | CheckpointAvailability::Unsupported
                )
            });
        let new_worktree = worktree_capable && open.where_ == ForkWhereSetting::NewWorktree;
        let turn_name = n.map_or_else(|| "the latest turn".to_owned(), |n| format!("turn {n}"));

        let (worktree_note, skipped, skipped_more) = if new_worktree {
            let note = match (checkpoint, unavailable) {
                (_, Some(_)) if chat.agent != "direct" => Some(
                    "Checkpoints are off or unavailable for this chat's folder: a worktree cannot hold the turn's files"
                        .to_owned(),
                ),
                (Some(checkpoint), _) if checkpoint.files_changed == 0 => Some(format!(
                    "No files changed by {turn_name}: the worktree starts from the files of the latest earlier turn that changed some, or the base commit"
                )),
                _ => Some(format!(
                    "A new branch and worktree with the files as {turn_name} left them, uncommitted. This folder is not touched"
                )),
            };
            let skipped_all: Vec<(String, String)> = checkpoint
                .map(|c| {
                    c.skipped
                        .iter()
                        .map(|s| (s.path.clone(), s.reason.describe()))
                        .collect()
                })
                .unwrap_or_default();
            let more = skipped_all.len().saturating_sub(SKIPPED_SHOWN);
            (
                note,
                skipped_all.into_iter().take(SKIPPED_SHOWN).collect(),
                more,
            )
        } else {
            (None, Vec::new(), 0)
        };

        // Restoring this folder's files needs a turn after this one that changed some.
        let later_changes = self.chat_model_of(open.session).and_then(|model| {
            let model = model.read(cx);
            let ended = ended_turns(model.transcript(), false);
            let at = ended.iter().position(|e| Some(e.turn) == open.turn)?;
            Some(ended[at + 1..].iter().any(|later| {
                checkpoints
                    .of_turn(open.session, later.turn)
                    .is_some_and(|c| c.files_changed > 0)
            }))
        });
        let restore = (worktree_capable && !new_worktree).then(|| {
            let (enabled, note) = if unavailable.is_some() {
                (
                    false,
                    "Checkpoints are off or unavailable for this chat's folder".to_owned(),
                )
            } else if open.newest {
                (false, "Nothing to restore: this is the latest turn".to_owned())
            } else if later_changes == Some(false) {
                (false, "Nothing to restore: no later turn changed a file".to_owned())
            } else {
                (
                    true,
                    format!(
                        "Puts this folder's files back as {turn_name} left them; the daemon previews it, and Undo stays in the turn's footer"
                    ),
                )
            };
            Toggle {
                label: format!("Restore files to the end of {turn_name}"),
                note,
                checked: open.restore_files && enabled,
                enabled,
            }
        });
        let archive = worktree_capable.then(|| Toggle {
            label: "Archive the original".to_owned(),
            note: "Closes this chat once the fork is up; Reopen brings it back".to_owned(),
            checked: open.archive_parent,
            enabled: true,
        });

        let support = chat.fork.as_ref().map(|fork| fork.support);
        let (offer, note) = match &open.refused {
            Some(refused) => (
                Offer::Choose(refused.alternatives.clone()),
                Some(refused.message.clone()),
            ),
            None => (
                fork::offer(support, open.newest),
                chat.fork.as_ref().and_then(|fork| fork.note.clone()),
            ),
        };
        let cut = match offer {
            Offer::Exact => Cut::Exact(exact_line(&chat.agent, n)),
            Offer::Choose(modes) => Cut::Choose {
                note: note.unwrap_or_else(|| {
                    format!("This agent cannot cut the conversation at {turn_name}")
                }),
                options: modes
                    .into_iter()
                    .map(|mode| {
                        let (title, detail) = cut_words(mode, n);
                        (mode, title, detail, open.choice == Some(mode))
                    })
                    .collect(),
            },
            Offer::Never => {
                Cut::Never(note.unwrap_or_else(|| "This chat cannot be forked".to_owned()))
            }
        };
        let blocked_by_files = new_worktree && unavailable.is_some() && chat.agent != "direct";
        let can_fork = !open.busy
            && !blocked_by_files
            && match &cut {
                Cut::Exact(_) => true,
                Cut::Choose { .. } => open.choice.is_some(),
                Cut::Never(_) => false,
            };
        let fork_label = if open.busy {
            "Forking…".to_owned()
        } else {
            match (&cut, open.choice) {
                (Cut::Choose { .. }, Some(ForkMode::Head)) => "Fork at the latest turn".to_owned(),
                (Cut::Choose { .. }, Some(ForkMode::Summary)) => "Fork with a summary".to_owned(),
                _ => "Fork".to_owned(),
            }
        };
        Some(View {
            title: match n {
                Some(n) => format!("Fork from turn {n}"),
                None => "Fork from the latest turn".to_owned(),
            },
            quote: open.prompt.as_deref().map(|p| format!("“{p}”")),
            worktree_capable,
            where_: if worktree_capable {
                open.where_
            } else {
                ForkWhereSetting::ThisFolder
            },
            branch: open.branch.clone(),
            branch_placeholder: match n {
                Some(n) => format!("fork/<name>-t{n} (default)"),
                None => "fork/<name>-t<N> (default)".to_owned(),
            },
            worktree_note,
            skipped,
            skipped_more,
            restore,
            archive,
            cut,
            fork_label,
            can_fork,
        })
    }

    // ---- forking -------------------------------------------------------------------------------

    /// *Fork*: the request the popover has built.
    pub fn submit_fork(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self
            .fork
            .open
            .clone()
            .filter(|open| open.shown && !open.busy)
        else {
            return;
        };
        let Some(view) = self.fork_view(cx) else {
            return;
        };
        if !view.can_fork {
            return;
        }
        let mode = match &view.cut {
            Cut::Exact(_) => ForkMode::Exact,
            Cut::Choose { .. } => match open.choice {
                Some(mode) => mode,
                None => return,
            },
            Cut::Never(_) => return,
        };
        let worktree = view.worktree_capable && open.where_ == ForkWhereSetting::NewWorktree;
        let branch = open.branch.trim();
        let request = ChatFork {
            session_id: open.session,
            // A fork at the head cuts nowhere: the daemon takes the conversation as it is.
            turn_id: if mode == ForkMode::Head {
                None
            } else {
                open.turn
            },
            mode,
            title: None,
            where_: if worktree {
                ForkWhere::NewWorktree
            } else {
                ForkWhere::ThisFolder
            },
            branch: (worktree && !branch.is_empty()).then(|| branch.to_owned()),
            restore_files: !worktree && view.restore.as_ref().is_some_and(|t| t.checked),
            archive_parent: view.archive.as_ref().is_some_and(|t| t.checked),
        };
        if let Some(open) = self.fork.open.as_mut() {
            open.busy = true;
            open.problem = None;
        }
        cx.notify();
        self.start_fork(open.session, request, open.ordinal, open.at, window, cx);
    }

    /// Sends `chat.fork`; on success opens the new chat and says what was done, on a refusal
    /// asks again or says why.
    fn start_fork(
        &mut self,
        session: SessionId,
        request: ChatFork,
        ordinal: Option<u32>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let where_ = request.where_;
        let turn = request.turn_id;
        let task = self
            .sessions
            .update(cx, |store, cx| store.fork_chat(request, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(done) => this.forked(session, where_, ordinal, done, window, cx),
                Err(error) => {
                    this.fork_failed(session, turn, at, ForkFailure::of(&error), window, cx)
                }
            })
            .ok();
        })
        .detach();
    }

    fn forked(
        &mut self,
        parent: SessionId,
        where_: ForkWhere,
        asked_ordinal: Option<u32>,
        done: ChatForked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remember = self
            .fork
            .open
            .as_ref()
            .filter(|open| open.session == parent)
            .map(|open| open.where_);
        if self
            .fork
            .open
            .as_ref()
            .is_some_and(|open| open.session == parent)
        {
            self.close_fork_popover(window, cx);
        }
        // The destination the user used is the next one's start (only when they could choose).
        if let Some(chosen) = remember
            && self.sessions.read(cx).supports_chat_fork_worktree()
            && chosen != self.settings.get::<ChatSettings>().fork_where
            && let Err(error) = self.settings.write_user(
                "chat.fork_where",
                toml::Value::String(chosen.key().to_owned()),
            )
        {
            tracing::warn!("cannot save chat.fork_where: {error}");
        }
        let info = &done.fork;
        let n = if info.ordinal > 0 {
            Some(info.ordinal)
        } else {
            asked_ordinal
        };
        let mut said = match info.mode {
            ForkMode::Exact => format!(
                "Forked at {}",
                n.map_or("the latest turn".to_owned(), |n| format!("turn {n}"))
            ),
            ForkMode::Head => "Forked at the latest turn".to_owned(),
            ForkMode::Summary => format!("Forked with a summary of {}", fork::turns_text(n)),
        };
        if where_ == ForkWhere::NewWorktree
            && let Some(worktree) = &info.worktree
        {
            said.push_str(&format!(" into worktree {}", worktree.branch));
            if !worktree.skipped.is_empty() {
                said.push_str(&format!(
                    " ({} path{} left out of the turn's files)",
                    worktree.skipped.len(),
                    if worktree.skipped.len() == 1 { "" } else { "s" }
                ));
            }
        }
        if let Some(restored) = &info.restored {
            said.push_str(&format!(
                " · restored {} file{} here, Undo is in the turn's footer",
                restored.written.len() + restored.deleted.len(),
                if restored.written.len() + restored.deleted.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if info.parent_archived {
            said.push_str(" · the original is archived");
        }
        for note in &info.notes {
            said.push_str(&format!(" · {note}"));
        }
        self.open_session(done.session.id, window, cx);
        self.set_notice(said, cx);
    }

    fn fork_failed(
        &mut self,
        session: SessionId,
        turn: Option<TurnId>,
        at: Point<Pixels>,
        failure: ForkFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open_here = self
            .fork
            .open
            .as_ref()
            .is_some_and(|open| open.session == session && open.shown);
        if open_here {
            if let Some(open) = self.fork.open.as_mut() {
                open.busy = false;
                open.choice = None;
                match failure.refused {
                    Some(refused) => {
                        open.refused = Some(refused);
                        open.problem = None;
                    }
                    None => open.problem = Some(failure.message),
                }
            }
            cx.notify();
            return;
        }
        // Forked straight from the sidebar and refused: the popover asks, in the daemon's words.
        match failure.refused {
            Some(refused) => {
                self.open_fork_popover(session, turn, at, Some((refused, None)), window, cx);
            }
            None => self.set_notice(format!("Cannot fork: {}", failure.message), cx),
        }
    }

    // ---- terminal agents -----------------------------------------------------------------------

    /// *Fork Session*: the agent's own CLI fork, in a new terminal tab of the same workspace
    /// and folder.
    pub fn fork_terminal_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.sessions.read(cx).session(session).cloned() else {
            return;
        };
        let (app, argv) = match fork::terminal_fork(&info) {
            TerminalFork::Ready { app, argv } => (app, argv),
            TerminalFork::NoSessionId { app } => {
                self.set_notice(
                    format!(
                        "{app} has not told Hutan its session id, so it cannot be forked from here: \
                         install the integration (hutan integration install) or run its own fork command"
                    ),
                    cx,
                );
                return;
            }
            TerminalFork::Nothing => {
                self.set_notice("No coding agent is known to run in this terminal", cx);
                return;
            }
        };
        let command = argv.join(" ");
        let mut spec = TerminalSpec {
            title: Some(format!("fork: {}", info.title)),
            command: argv,
            // Where the agent is now: its conversations are kept per folder.
            cwd: info.folder.clone().or_else(|| info.cwd.clone()),
            ..TerminalSpec::default()
        };
        let (workspace_root, project_root) =
            super::workspaces::layer_roots_of(self.sessions.read(cx), info.workspace_id);
        self.settings
            .get_for_roots::<TerminalSettings>(workspace_root.as_deref(), project_root.as_deref())
            .apply_to_spec(&mut spec);
        let created = self.sessions.update(cx, |store, cx| {
            store.create_session(Some(info.workspace_id), spec, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = created.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(forked) => {
                    this.open_session(forked.id, window, cx);
                    this.set_notice(format!("Forked {app} with `{command}`"), cx);
                }
                Err(error) => this.set_notice(format!("Cannot fork the session: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    // ---- menus ---------------------------------------------------------------------------------

    /// The session menu's fork entries: *Fork…* for a chat, *Fork Session* for a terminal agent
    /// (greyed, with the reason, while the agent has not told its session id).
    pub(crate) fn fork_menu_entries(
        &self,
        session: SessionId,
        position: Point<Pixels>,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let store = self.sessions.read(cx);
        let Some(info) = store.session(session) else {
            return Vec::new();
        };
        if info.chat.is_some() {
            if self.can_fork_chat(session, cx).is_err() {
                return Vec::new();
            }
            return vec![Self::entry("Fork…", move |this, window, cx| {
                this.fork_session_latest(session, position, true, window, cx);
            })];
        }
        match fork::terminal_fork(info) {
            TerminalFork::Nothing => Vec::new(),
            TerminalFork::NoSessionId { app } => vec![Self::disabled_entry(format!(
                "Fork Session ({app} has not reported its session id)"
            ))],
            TerminalFork::Ready { .. } => {
                vec![Self::entry("Fork Session", move |this, window, cx| {
                    this.fork_terminal_session(session, window, cx);
                })]
            }
        }
    }

    // ---- the popover ---------------------------------------------------------------------------

    pub(crate) fn render_fork_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let wanted = self.fork.open.as_ref().is_some_and(|open| open.shown);
        let wanted = self
            .overlays
            .track("fork-popover", wanted, motion::Kind::Popover, cx)?;
        let open = self.fork.open.clone()?;
        // While it fades out the last state is drawn as it was.
        let view = self.view_of(&open, cx)?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let busy = open.busy;
        let viewport = window.viewport_size();
        let left = (f32::from(open.at.x) - 28.0)
            .min(f32::from(viewport.width) - WIDTH - 8.0)
            .max(8.0);
        let above = f32::from(open.at.y) > f32::from(viewport.height) / 2.0;
        let anchor = if above {
            (f32::from(viewport.height) - f32::from(open.at.y) + 10.0).max(8.0)
        } else {
            (f32::from(open.at.y) + 14.0).max(8.0)
        };
        let regions = self.regions.clone();
        let muted = |text: SharedString| {
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .child(text)
        };
        let label = |text: &'static str| {
            div()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.muted)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(text)
        };
        let check = |checked: bool, enabled: bool| {
            div()
                .flex_none()
                .when(!enabled, |s| s.opacity(0.4))
                .child(crate::icons::glyph(if checked {
                    Icon::StateCompleted
                } else {
                    Icon::StateTodo
                }))
        };

        // Where: this folder / a new worktree (0.58).
        let where_row = view.worktree_capable.then(|| {
            let folder = ui::segment(
                "fork-where-folder",
                "This folder",
                view.where_ == ForkWhereSetting::ThisFolder,
                &theme,
                &tokens,
            )
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                this.set_fork_where(ForkWhereSetting::ThisFolder, cx);
            }))
            .child(probe(&regions, Region::Fork("where-folder")));
            let worktree = ui::segment(
                "fork-where-worktree",
                "New worktree",
                view.where_ == ForkWhereSetting::NewWorktree,
                &theme,
                &tokens,
            )
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                this.set_fork_where(ForkWhereSetting::NewWorktree, cx);
            }))
            .child(probe(&regions, Region::Fork("where-worktree")));
            div()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(label("Where"))
                .child(ui::segmented(&theme, &tokens).child(folder).child(worktree))
        });
        let branch_row = (view.where_ == ForkWhereSetting::NewWorktree).then(|| {
            div()
                .id("fork-branch")
                .relative()
                .flex()
                .items_center()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.control_height))
                .outlined(&theme, &tokens)
                .children(self.overlay_parts(
                    OverlayField::ForkBranch,
                    &view.branch_placeholder,
                    cx,
                ))
                .child(probe(&regions, Region::Fork("branch")))
        });
        let worktree_note = view.worktree_note.clone().map(|note| {
            div()
                .id("fork-worktree-note")
                .relative()
                .child(muted(SharedString::from(note)))
                .child(probe(&regions, Region::Fork("note")))
        });
        let skipped = (!view.skipped.is_empty()).then(|| {
            div()
                .id("fork-skipped")
                .relative()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(label("Left out of the turn's files"))
                .children(view.skipped.iter().map(|(path, why)| {
                    div()
                        .flex()
                        .gap(px(tokens.spacing))
                        .text_size(px(tokens.secondary_font_size))
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.fg)
                                .child(SharedString::from(path.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.muted)
                                .child(SharedString::from(format!("· {why}"))),
                        )
                }))
                .when(view.skipped_more > 0, |s| {
                    s.child(muted(SharedString::from(format!(
                        "and {} more",
                        view.skipped_more
                    ))))
                })
                .child(probe(&regions, Region::Fork("skipped")))
        });

        let toggle_row =
            |id: &'static str,
             region: &'static str,
             toggle: &Toggle,
             on_click: fn(&mut Workbench, &mut Context<Workbench>)| {
                let enabled = toggle.enabled && !busy;
                div()
                    .id(id)
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(tokens.spacing * 2.0))
                    .px(px(tokens.spacing))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .when(enabled, |s| {
                        s.cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
                    })
                    .child(check(toggle.checked, toggle.enabled))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_color(if toggle.enabled {
                                        theme.fg
                                    } else {
                                        theme.muted
                                    })
                                    .child(SharedString::from(toggle.label.clone())),
                            )
                            .child(muted(SharedString::from(toggle.note.clone()))),
                    )
                    .child(probe(&regions, Region::Fork(region)))
            };
        let restore_row = view.restore.as_ref().map(|t| {
            toggle_row("fork-restore", "restore", t, |this, cx| {
                this.toggle_fork_restore(cx)
            })
        });
        let archive_row = view.archive.as_ref().map(|t| {
            toggle_row("fork-archive", "archive", t, |this, cx| {
                this.toggle_fork_archive(cx)
            })
        });

        // The cut: how exact the fork is, or a choice nothing happens without.
        let cut: AnyElement = match &view.cut {
            Cut::Exact(line) => div()
                .id("fork-cut")
                .relative()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.success)
                .child(SharedString::from(line.clone()))
                .child(probe(&regions, Region::Fork("cut")))
                .into_any_element(),
            Cut::Choose { note, options } => div()
                .id("fork-cut")
                .relative()
                .flex()
                .flex_col()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .text_size(px(tokens.secondary_font_size))
                        .text_color(theme.warning)
                        .child(SharedString::from(note.clone())),
                )
                .children(options.iter().enumerate().map(
                    |(index, (mode, title, detail, selected))| {
                        let mode = *mode;
                        div()
                            .id(("fork-cut-choice", index))
                            .relative()
                            .flex()
                            .items_start()
                            .gap(px(tokens.spacing * 2.0))
                            .px(px(tokens.spacing))
                            .py(px(tokens.spacing))
                            .rounded(px(tokens.radius))
                            .when(*selected, |s| s.bg(theme.accent_soft))
                            .when(!*selected && !busy, |s| s.hover(move |s| s.bg(hover)))
                            .when(!busy, |s| {
                                s.cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose_fork_cut(mode, cx);
                                    }))
                            })
                            .child(check(*selected, true))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_color(theme.fg)
                                            .child(SharedString::from(title.clone())),
                                    )
                                    .child(muted(SharedString::from(detail.clone()))),
                            )
                            .child(probe(&regions, Region::ForkCut(index)))
                    },
                ))
                .child(probe(&regions, Region::Fork("cut")))
                .into_any_element(),
            Cut::Never(note) => div()
                .id("fork-cut")
                .relative()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.warning)
                .child(SharedString::from(note.clone()))
                .child(probe(&regions, Region::Fork("cut")))
                .into_any_element(),
        };

        let problem = open.problem.clone().map(|text| {
            div()
                .id("fork-problem")
                .relative()
                .text_size(px(tokens.secondary_font_size))
                .text_color(theme.danger)
                .child(SharedString::from(text))
                .child(probe(&regions, Region::Fork("problem")))
        });
        let fork_button = Button::new("fork-go", view.fork_label.clone(), &theme, &tokens)
            .kind(ButtonKind::Primary)
            .disabled(!view.can_fork)
            .loading(busy)
            .on_click(cx.listener(|this, _, window, cx| this.submit_fork(window, cx)))
            .child(probe(&regions, Region::Fork("fork")));
        let cancel = Button::new("fork-cancel", "Cancel", &theme, &tokens)
            .ghost()
            .on_click(cx.listener(|this, _, window, cx| this.close_fork_popover(window, cx)))
            .child(probe(&regions, Region::Fork("cancel")));

        Some(
            div()
                .id("fork-backdrop")
                .absolute()
                .inset_0()
                .when(wanted, |s| s.shield_behind())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_fork_popover(window, cx)),
                )
                .child(
                    div()
                        .id("fork-popover")
                        .key_context(super::vim_ui::vim_key_context(
                            "ForkPopover",
                            self.overlay_vim_mode(OverlayField::ForkBranch),
                        ))
                        .track_focus(&self.fork.focus)
                        .absolute()
                        .left(px(left))
                        .when(above, |s| s.bottom(px(anchor)))
                        .when(!above, |s| s.top(px(anchor)))
                        .w(px(WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.5))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 4.0))
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            // Typed text arrives through the field's platform text input.
                            if this.overlay_key(OverlayField::ForkBranch, event, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.submit_fork(window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(SharedString::from(view.title.clone())),
                                )
                                .children(view.quote.clone().map(|quote| {
                                    div()
                                        .truncate()
                                        .text_size(px(tokens.secondary_font_size))
                                        .text_color(theme.muted)
                                        .child(SharedString::from(quote))
                                })),
                        )
                        .children(where_row)
                        .children(branch_row)
                        .children(worktree_note)
                        .children(skipped)
                        .children(restore_row)
                        .children(archive_row)
                        .child(cut)
                        .children(problem)
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .child(cancel)
                                .child(fork_button),
                        )
                        .child(probe(&regions, Region::Fork("popover")))
                        .presented(
                            "fork-popover-motion",
                            motion::Kind::Popover,
                            wanted,
                            motion::Slide::Rise,
                        ),
                )
                .into_any_element(),
        )
    }
}
