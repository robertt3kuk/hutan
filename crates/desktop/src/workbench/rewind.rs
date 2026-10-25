//! Rewinding an agent's files in the desktop (ADR 0019, slice CK-5; spike
//! `docs/spikes/2026-10-21-checkpoints.md` §5.1, §6.3): the preview sheet a turn footer's
//! *Rewind…* opens, the "Restored N files · Undo" toast, *Undo*, the Checkpoints list (Agents
//! page → ⋯ → *Checkpoints…*, ⌘K *Chat: Rewind Files to Before This Turn…*) and ⌘K *Chat: Undo
//! Last Restore*. Drawing is `rewind_view.rs`.
//!
//! The daemon plans and writes (`checkpoint.preview`, `checkpoint.restore`); the sheet only shows
//! the plan and collects which files the user restores. Nothing is written before the user
//! clicks *Restore N files* (never the default: the sheet opens with *Cancel* focused, Return
//! presses the focused button, Esc cancels). Clean files start checked; a file somebody changed
//! since the turn (or between turns, or while it ran) starts unchecked and is only overwritten
//! when its own box is ticked, or by *Restore all, including edited-after* on its second click;
//! a file the checkpoint left out is greyed with its reason. Unticking a clean file restores the
//! others only: the plan is asked again for exactly the ticked paths, so the digest the restore
//! confirms is that plan's. A checkout that changed meanwhile answers with the new plan, which
//! the sheet shows instead and says so; a turn running in the checkout names the agent and
//! offers *Interrupt it*.

use std::collections::HashMap;

use gpui::{AppContext as _, Context, Entity, FocusHandle, KeyDownEvent, Window};
use hutan_chat_view::Body;
use hutan_chat_view::diff::{FileDiff, split_patch};
use hutan_client::ClientError;
use hutan_core::{
    CheckpointId, CheckpointInfo, CheckpointKind, CheckpointState, RestoreAction, RestoreEntry,
    RestoreState, SessionId, TurnId,
};
use hutan_protocol::ErrorCode;
use hutan_protocol::checkpoint::{
    CheckpointDiffRequest, CheckpointPreview, CheckpointPreviewRequest, CheckpointRestore,
    CheckpointRestored, DiffAgainst,
};

use super::chat::ChatModel;
use super::closing::TOAST_TIME;
use super::view::Workbench;

/// The groups of the sheet, in the order drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// As the turn left it: restored unless unticked.
    Clean,
    /// Changed since the turn.
    After,
    /// Changed between two of the turns.
    Gap,
    /// Saved by the user while a turn ran.
    During,
    /// Left out of the checkpoint (or a link, a submodule): never written.
    NotCaptured,
}

impl Group {
    pub fn of(entry: &RestoreEntry) -> Self {
        match entry.state {
            RestoreState::Clean if entry.action != RestoreAction::Skip => Self::Clean,
            RestoreState::After => Self::After,
            RestoreState::Gap => Self::Gap,
            RestoreState::During => Self::During,
            RestoreState::Clean | RestoreState::Uncaptured => Self::NotCaptured,
        }
    }

    /// The group's heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Clean => "Restored",
            Self::After => "Edited after the turn",
            Self::Gap => "Edited between turns",
            Self::During => "You edited this too",
            Self::NotCaptured => "Not captured",
        }
    }

    /// What a conflict group means; the words carry it, never the colour alone.
    pub fn warning(self) -> Option<&'static str> {
        match self {
            Self::After => Some(
                "you or another agent changed this since the turn — restoring overwrites that \
                 change",
            ),
            Self::Gap => Some("changed between the turns — restoring overwrites that change"),
            Self::During => {
                Some("you saved this while the turn ran — restoring overwrites your change")
            }
            Self::Clean | Self::NotCaptured => None,
        }
    }

    pub fn is_conflict(self) -> bool {
        matches!(self, Self::After | Self::Gap | Self::During)
    }
}

/// One file of the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindRow {
    pub entry: RestoreEntry,
    pub group: Group,
    pub checked: bool,
}

impl RewindRow {
    /// Whether the user may tick it: a clean file, or a conflict the daemon can accept.
    pub fn checkable(&self) -> bool {
        match self.group {
            Group::Clean => true,
            Group::After | Group::Gap | Group::During => self.entry.accept.is_some(),
            Group::NotCaptured => false,
        }
    }

    /// What restoring it does, in words (`M`, `will be deleted`, `will be recreated`).
    pub fn action_text(&self) -> &'static str {
        match self.entry.accept.unwrap_or(self.entry.action) {
            RestoreAction::Write => "will be written back",
            RestoreAction::Delete => "will be deleted",
            RestoreAction::Skip => "left alone",
        }
    }
}

/// The plan's files as the sheet lists them: by group, then by path; clean ones ticked.
pub fn rows_of(entries: &[RestoreEntry]) -> Vec<RewindRow> {
    let mut rows: Vec<RewindRow> = entries
        .iter()
        .map(|entry| {
            let group = Group::of(entry);
            RewindRow {
                entry: entry.clone(),
                group,
                checked: group == Group::Clean,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then_with(|| a.entry.path.cmp(&b.entry.path))
    });
    rows
}

/// The keyboard's place in the sheet: the file list or one of its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetFocus {
    Files,
    Cancel,
    RestoreAll,
    Restore,
    Interrupt,
}

/// One file's diff `now → restored`, fetched when it is selected.
#[derive(Debug, Clone)]
pub enum DiffState {
    Loading,
    Ready(Option<FileDiff>),
    Failed(String),
}

/// What the sheet is showing.
#[derive(Debug, Clone)]
pub enum Phase {
    Loading,
    Ready,
    Failed(String),
}

/// The preview sheet of one restore.
#[derive(Debug)]
pub struct RewindSheet {
    pub session: SessionId,
    pub checkpoint: CheckpointId,
    /// Undoing a restore (no note to the agent).
    pub undo: bool,
    /// The prompt of the turn, when the transcript holds it (the note to the agent says it).
    pub prompt: Option<String>,
    pub phase: Phase,
    pub preview: Option<CheckpointPreview>,
    pub rows: Vec<RewindRow>,
    pub selected: usize,
    pub focus: SheetFocus,
    /// *Restore all, including edited-after* was clicked once: the next click restores.
    pub armed_all: bool,
    /// The plan changed under the user ("The files changed since…").
    pub notice: Option<String>,
    /// What went wrong with the last request.
    pub error: Option<String>,
    /// A restore (or the narrowed preview before it) is on its way.
    pub working: bool,
    /// Put a note for the agent in the composer after the restore (default on).
    pub note: bool,
    pub diffs: HashMap<String, DiffState>,
    /// Bumped whenever the plan is asked again: an answer to an older request is dropped.
    generation: u64,
    /// A whole plan is on its way (`refresh_preview`).
    refreshing: bool,
}

impl RewindSheet {
    fn new(
        session: SessionId,
        checkpoint: CheckpointId,
        undo: bool,
        prompt: Option<String>,
    ) -> Self {
        Self {
            session,
            checkpoint,
            undo,
            prompt,
            phase: Phase::Loading,
            preview: None,
            rows: Vec::new(),
            selected: 0,
            focus: SheetFocus::Cancel,
            armed_all: false,
            notice: None,
            error: None,
            working: false,
            note: !undo,
            diffs: HashMap::new(),
            generation: 0,
            refreshing: false,
        }
    }

    /// The ticked files.
    pub fn checked(&self) -> impl Iterator<Item = &RewindRow> {
        self.rows
            .iter()
            .filter(|row| row.checked && row.checkable())
    }

    /// `Restore N files`.
    pub fn restore_label(&self) -> String {
        let n = self.checked().count();
        if n == 1 {
            "Restore 1 file".to_owned()
        } else {
            format!("Restore {n} files")
        }
    }

    /// Conflicts the daemon can be asked to overwrite.
    pub fn conflicts(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.group.is_conflict() && row.checkable())
            .count()
    }

    /// Agents with a turn running in the checkout: nothing can be restored meanwhile.
    pub fn busy(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| !p.busy.is_empty())
    }

    /// Whether *Restore N files* may be pressed.
    pub fn can_restore(&self) -> bool {
        matches!(self.phase, Phase::Ready)
            && !self.working
            && !self.busy()
            && self.checked().next().is_some()
    }

    /// The buttons the keyboard walks, in order.
    fn stops(&self) -> Vec<SheetFocus> {
        let mut stops = vec![SheetFocus::Files, SheetFocus::Cancel];
        if self.conflicts() > 0 {
            stops.push(SheetFocus::RestoreAll);
        }
        stops.push(SheetFocus::Restore);
        if self.busy() {
            stops.push(SheetFocus::Interrupt);
        }
        stops
    }

    fn install(&mut self, preview: CheckpointPreview) {
        // What the user ticked or unticked stays as it was for the files still in the plan in
        // the same group; a file that became a conflict starts unticked again.
        let before: HashMap<String, (Group, bool)> = self
            .rows
            .iter()
            .map(|row| (row.entry.path.clone(), (row.group, row.checked)))
            .collect();
        let mut rows = rows_of(&preview.entries);
        for row in &mut rows {
            if let Some((group, checked)) = before.get(&row.entry.path)
                && *group == row.group
            {
                row.checked = *checked && row.checkable();
            }
        }
        self.rows = rows;
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.preview = Some(preview);
        self.phase = Phase::Ready;
        self.armed_all = false;
        self.diffs.clear();
        if !self.stops().contains(&self.focus) {
            self.focus = SheetFocus::Cancel;
        }
    }
}

/// The words at the top of the sheet once its plan is in
/// ([`Workbench::rewind_sheet_header`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindHeader {
    /// `Codex · 1 turn · hutan`: the agent, the turns, the project (else the `~` folder).
    pub facts: String,
    /// The honest label, which names the checkout `this checkout`.
    pub label: String,
    /// The checkout's full path: the tooltip of both lines.
    pub checkout: String,
}

/// One row of the Checkpoints list.
#[derive(Debug, Clone)]
pub struct ListRow {
    pub checkpoint: CheckpointInfo,
    /// The first line of the turn's prompt, when the transcript holds it.
    pub prompt: Option<String>,
}

/// The Checkpoints list of one chat.
#[derive(Debug)]
pub struct CheckpointsList {
    pub session: SessionId,
    pub selected: usize,
    /// The chat's transcript, for the prompts (held while the list is open).
    model: Entity<ChatModel>,
}

/// "Restored N files · Undo".
#[derive(Debug, Clone)]
pub struct RestoreToast {
    pub session: SessionId,
    pub restore: CheckpointId,
    pub files: usize,
    pub undid: bool,
    rev: u64,
}

impl RestoreToast {
    pub fn text(&self) -> String {
        let noun = if self.files == 1 { "file" } else { "files" };
        if self.undid {
            format!("Undid the restore · {} {noun}", self.files)
        } else {
            format!("Restored {} {noun}", self.files)
        }
    }
}

/// The sheet, the list, the toast and their keyboard.
#[derive(Debug)]
pub struct RewindState {
    pub sheet: Option<RewindSheet>,
    pub list: Option<CheckpointsList>,
    pub toast: Option<RestoreToast>,
    pub focus: FocusHandle,
    pub list_focus: FocusHandle,
    /// What had the keyboard before the sheet or the list opened.
    return_focus: Option<gpui::WeakFocusHandle>,
    toast_rev: u64,
    generation: u64,
}

impl RewindState {
    pub fn new(cx: &mut gpui::App) -> Self {
        Self {
            sheet: None,
            list: None,
            toast: None,
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            return_focus: None,
            toast_rev: 0,
            generation: 0,
        }
    }
}

/// `patch` (a unified diff from the turn's start to now) the other way round: from now to what
/// the restore writes.
pub fn reverse_patch(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len());
    for line in patch.lines() {
        let reversed = if let Some(rest) = line.strip_prefix("--- ") {
            format!("+++ {rest}")
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            format!("--- {rest}")
        } else if let Some(header) = line.strip_prefix("@@ ") {
            reverse_hunk_header(header).unwrap_or_else(|| line.to_owned())
        } else if let Some(rest) = line.strip_prefix('+') {
            format!("-{rest}")
        } else if let Some(rest) = line.strip_prefix('-') {
            format!("+{rest}")
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            format!("deleted file mode {rest}")
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            format!("new file mode {rest}")
        } else {
            line.to_owned()
        };
        out.push_str(&reversed);
        out.push('\n');
    }
    // A hunk lists its removals before its additions; reversed, they come after: put each
    // run of changed lines back in that order.
    reorder_runs(&out)
}

/// `-a,b +c,d @@ rest` → `@@ -c,d +a,b @@ rest`.
fn reverse_hunk_header(header: &str) -> Option<String> {
    let (ranges, rest) = header.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let old = old.strip_prefix('-')?;
    let new = new.strip_prefix('+')?;
    Some(format!("@@ -{new} +{old} @@{rest}"))
}

/// Within each run of `+` and `-` lines (outside the headers), the `-` lines first.
fn reorder_runs(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len());
    let mut removed: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    let mut in_hunk = false;
    let flush = |out: &mut String, removed: &mut Vec<&str>, added: &mut Vec<&str>| {
        for line in removed.drain(..).chain(added.drain(..)) {
            out.push_str(line);
            out.push('\n');
        }
    };
    for line in patch.lines() {
        if line.starts_with("@@ ") {
            in_hunk = true;
        } else if line.starts_with("diff ") {
            in_hunk = false;
        }
        let body = in_hunk && !line.starts_with("@@ ");
        if body && line.starts_with('-') {
            removed.push(line);
        } else if body && line.starts_with('+') {
            added.push(line);
        } else {
            flush(&mut out, &mut removed, &mut added);
            out.push_str(line);
            out.push('\n');
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

/// The first line of the prompt of `turn` in `model`'s transcript: the user's message nearest
/// before the turn's first entry.
pub fn turn_prompt(model: &ChatModel, turn: TurnId) -> Option<String> {
    let entries = model.transcript().entries();
    let first = entries.iter().position(|e| e.turn == Some(turn))?;
    entries[..=first].iter().rev().find_map(|e| match &e.body {
        Body::User(message) => {
            let (typed, _) = super::chat::split_context(&message.text);
            typed
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        }
        _ => None,
    })
}

/// The new plan a stale restore was answered with (`conflict` with the preview as `data`).
fn conflict_preview(error: &ClientError) -> Option<CheckpointPreview> {
    let ClientError::Remote(body) = error else {
        return None;
    };
    if body.code != ErrorCode::Conflict {
        return None;
    }
    serde_json::from_value(body.data.clone()?).ok()
}

fn remote_code(error: &ClientError) -> Option<ErrorCode> {
    match error {
        ClientError::Remote(body) => Some(body.code),
        _ => None,
    }
}

/// What the request asks of the daemon after the user chose.
struct Choice {
    /// Every clean file is ticked: the preview's own plan (and digest) goes as it is.
    whole: bool,
    paths: Vec<String>,
    accept: Vec<String>,
}

impl Workbench {
    // ---- opening ---------------------------------------------------------------------------

    /// The checkpoints of agent chats.
    fn checkpoint_store(
        &self,
        cx: &gpui::App,
    ) -> Entity<crate::sessions::checkpoints::CheckpointStore> {
        self.sessions.read(cx).checkpoints().clone()
    }

    /// The Rewind preview sheet, if it is open.
    pub fn rewind_sheet(&self) -> Option<&RewindSheet> {
        self.rewind.sheet.as_ref()
    }

    /// The Checkpoints list, if it is open.
    pub fn checkpoints_list(&self) -> Option<&CheckpointsList> {
        self.rewind.list.as_ref()
    }

    /// The "Restored … · Undo" toast, if it is showing.
    pub fn restore_toast(&self) -> Option<&RestoreToast> {
        self.rewind.toast.as_ref()
    }

    fn remember_focus(&mut self, window: &Window, cx: &gpui::App) {
        if self.rewind.sheet.is_some() || self.rewind.list.is_some() {
            return;
        }
        self.rewind.return_focus = window
            .focused(cx)
            .filter(|f| *f != self.rewind.focus && *f != self.rewind.list_focus)
            .map(|f| f.downgrade());
    }

    fn give_focus_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewind.list.is_some() {
            window.focus(&self.rewind.list_focus, cx);
            return;
        }
        match self.rewind.return_focus.take().and_then(|w| w.upgrade()) {
            Some(handle) => window.focus(&handle, cx),
            None => self.focus_root(window, cx),
        }
    }

    /// The prompt of the turn of `checkpoint`, from a transcript this window holds.
    fn prompt_of(
        &self,
        session: SessionId,
        checkpoint: CheckpointId,
        cx: &gpui::App,
    ) -> Option<String> {
        let turn = self
            .checkpoint_store(cx)
            .read(cx)
            .get(checkpoint)
            .and_then(|c| c.turn_id)?;
        let model = self.chat_models.get(&session).or_else(|| {
            self.rewind
                .list
                .as_ref()
                .filter(|l| l.session == session)
                .map(|l| &l.model)
        })?;
        turn_prompt(model.read(cx), turn)
    }

    /// *Rewind…*: the preview sheet of restoring the files of `checkpoint` (nothing is written).
    pub fn open_rewind(
        &mut self,
        session: SessionId,
        checkpoint: CheckpointId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let undo = self
            .checkpoint_store(cx)
            .read(cx)
            .get(checkpoint)
            .is_some_and(|c| c.kind == CheckpointKind::Restore);
        let prompt = self.prompt_of(session, checkpoint, cx);
        self.open_sheet(
            RewindSheet::new(session, checkpoint, undo, prompt),
            window,
            cx,
        );
    }

    fn open_sheet(&mut self, sheet: RewindSheet, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_focus(window, cx);
        self.palette = None;
        self.context_menu = None;
        self.rewind.sheet = Some(sheet);
        window.focus(&self.rewind.focus, cx);
        self.refresh_preview(cx);
        cx.notify();
    }

    /// Esc, *Cancel*, a click beside the sheet: closes it, nothing written.
    pub fn close_rewind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.rewind.sheet.take() {
            self.leave_rewind(old, cx);
            self.give_focus_back(window, cx);
            cx.notify();
        }
    }

    /// Asks the daemon for the plan again (the whole one: the user's ticks are kept).
    fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.rewind.sheet else {
            return;
        };
        sheet.refreshing = true;
        self.rewind.generation += 1;
        sheet.generation = self.rewind.generation;
        let generation = sheet.generation;
        let request = CheckpointPreviewRequest {
            checkpoint_id: sheet.checkpoint,
            through: None,
            paths: Vec::new(),
        };
        let connection = self.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let answer = connection.checkpoint_preview(request).await;
            this.update(cx, |this, cx| {
                let Some(sheet) = this
                    .rewind
                    .sheet
                    .as_mut()
                    .filter(|s| s.generation == generation)
                else {
                    return;
                };
                sheet.refreshing = false;
                match answer {
                    Ok(preview) => {
                        sheet.install(preview);
                        this.load_selected_diff(cx);
                        // The turn may have ended while this plan was on its way.
                        this.recheck_rewind_busy(cx);
                    }
                    Err(error) if sheet.preview.is_some() => sheet.error = Some(error.to_string()),
                    Err(error) => sheet.phase = Phase::Failed(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A turn of `session` ended: a sheet that waited on it asks again.
    pub(crate) fn on_turn_ended(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let waiting = self.rewind.sheet.as_ref().is_some_and(|sheet| {
            sheet
                .preview
                .as_ref()
                .is_some_and(|p| p.busy.contains(&session))
        });
        if waiting {
            self.recheck_rewind_busy(cx);
        }
    }

    /// The sessions changed: once none of the turns that held a sheet's checkout runs any
    /// longer (as the daemon's session list says, which is what its preview reads), the sheet
    /// asks for the plan again. The turn's end event can arrive before that change.
    pub(crate) fn recheck_rewind_busy(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = &self.rewind.sheet else {
            return;
        };
        let Some(preview) = sheet.preview.as_ref().filter(|p| !p.busy.is_empty()) else {
            return;
        };
        if sheet.refreshing {
            return;
        }
        let store = self.sessions.read(cx);
        // The daemon's rule (`occupants`): a chat with a turn running, or an agent at work. A
        // session this window does not know counts as still running.
        let still = preview.busy.iter().any(|id| {
            store.session(*id).is_none_or(|s| {
                s.chat.as_ref().is_some_and(|chat| chat.turn_active)
                    || s.agent
                        .as_ref()
                        .is_some_and(|a| a.state == hutan_protocol::messages::AgentState::Working)
            })
        });
        if !still {
            self.refresh_preview(cx);
        }
    }

    // ---- the sheet's controls --------------------------------------------------------------

    /// Ticks or unticks file `index` (a file that cannot be restored stays as it is).
    pub fn toggle_rewind_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.rewind.sheet else {
            return;
        };
        sheet.selected = index.min(sheet.rows.len().saturating_sub(1));
        if let Some(row) = sheet.rows.get_mut(index)
            && row.checkable()
        {
            row.checked = !row.checked;
            sheet.armed_all = false;
        }
        self.load_selected_diff(cx);
        cx.notify();
    }

    /// Selects file `index` (its diff shows) and puts the keyboard in the list.
    pub fn select_rewind_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.rewind.sheet else {
            return;
        };
        if index < sheet.rows.len() {
            sheet.selected = index;
            sheet.focus = SheetFocus::Files;
            self.load_selected_diff(cx);
            cx.notify();
        }
    }

    /// The note to the agent: on or off.
    pub fn toggle_rewind_note(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.rewind.sheet {
            sheet.note = !sheet.note;
            cx.notify();
        }
    }

    /// Where the keyboard is in the sheet.
    pub fn set_rewind_focus(&mut self, focus: SheetFocus, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.rewind.sheet {
            sheet.focus = focus;
            cx.notify();
        }
    }

    /// The sheet's keys: ↑↓ walk the files, Space ticks, Tab walks the buttons, Return presses
    /// the focused button (nothing in the file list), Esc cancels.
    pub(crate) fn rewind_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(sheet) = &mut self.rewind.sheet else {
            return false;
        };
        let mods = event.keystroke.modifiers;
        if mods.platform || mods.control || mods.alt {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_rewind(window, cx),
            "up" | "down" => {
                let up = event.keystroke.key == "up";
                let last = sheet.rows.len().saturating_sub(1);
                let next = if sheet.focus != SheetFocus::Files {
                    sheet.selected
                } else if up {
                    sheet.selected.saturating_sub(1)
                } else {
                    (sheet.selected + 1).min(last)
                };
                self.select_rewind_file(next, cx);
            }
            "space" if sheet.focus == SheetFocus::Files => {
                let at = sheet.selected;
                self.toggle_rewind_file(at, cx);
            }
            "tab" => {
                let stops = sheet.stops();
                let at = stops.iter().position(|s| *s == sheet.focus).unwrap_or(0);
                let next = if mods.shift {
                    (at + stops.len() - 1) % stops.len()
                } else {
                    (at + 1) % stops.len()
                };
                sheet.focus = stops[next];
                cx.notify();
            }
            "enter" | "space" => {
                let focus = sheet.focus;
                self.press_rewind(focus, window, cx);
            }
            _ => return false,
        }
        true
    }

    /// A button of the sheet, clicked or pressed with the keyboard.
    pub fn press_rewind(
        &mut self,
        button: SheetFocus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match button {
            SheetFocus::Files => {}
            SheetFocus::Cancel => self.close_rewind(window, cx),
            SheetFocus::Restore => self.confirm_rewind(false, window, cx),
            SheetFocus::RestoreAll => {
                let Some(sheet) = &mut self.rewind.sheet else {
                    return;
                };
                sheet.focus = SheetFocus::RestoreAll;
                if sheet.armed_all {
                    self.confirm_rewind(true, window, cx);
                } else {
                    // The first click only arms it: overwriting others' changes takes two.
                    sheet.armed_all = true;
                    cx.notify();
                }
            }
            SheetFocus::Interrupt => self.interrupt_busy(cx),
        }
    }

    /// *Interrupt it*: stops the turns that hold the checkout (`chat.interrupt`); the plan is
    /// asked again when they end.
    fn interrupt_busy(&mut self, cx: &mut Context<Self>) {
        let Some(busy) = self
            .rewind
            .sheet
            .as_ref()
            .and_then(|s| s.preview.as_ref())
            .map(|p| p.busy.clone())
        else {
            return;
        };
        let connection = self.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            for session in busy {
                if let Err(error) = connection.chat_interrupt(session).await {
                    this.update(cx, |this, cx| {
                        if let Some(sheet) = &mut this.rewind.sheet {
                            sheet.error = Some(format!("Could not interrupt: {error}"));
                            cx.notify();
                        }
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// What the user chose, as the request says it.
    fn choice(sheet: &RewindSheet, all: bool) -> Choice {
        let ticked = |row: &RewindRow| row.checkable() && (all || row.checked);
        let whole = sheet
            .rows
            .iter()
            .filter(|row| row.group == Group::Clean)
            .all(ticked);
        Choice {
            whole,
            paths: sheet
                .rows
                .iter()
                .filter(|row| ticked(row))
                .map(|row| row.entry.path.clone())
                .collect(),
            accept: sheet
                .rows
                .iter()
                .filter(|row| row.group.is_conflict() && ticked(row))
                .map(|row| row.entry.path.clone())
                .collect(),
        }
    }

    /// *Restore N files* (or the armed *Restore all*): restores what is ticked.
    fn confirm_rewind(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.rewind.sheet else {
            return;
        };
        let can = if all {
            matches!(sheet.phase, Phase::Ready) && !sheet.working && !sheet.busy()
        } else {
            sheet.can_restore()
        };
        let Some(preview) = sheet.preview.clone().filter(|_| can) else {
            return;
        };
        let choice = Self::choice(sheet, all);
        if choice.paths.is_empty() {
            return;
        }
        sheet.working = true;
        sheet.error = None;
        self.rewind.generation += 1;
        sheet.generation = self.rewind.generation;
        let generation = sheet.generation;
        let checkpoint = sheet.checkpoint;
        let connection = self.sessions.read(cx).connection().clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            // A narrower choice is planned again for exactly those files: the digest a
            // restore confirms is that plan's.
            let (digest, paths, stale) = if choice.whole {
                (preview.plan_digest.clone(), Vec::new(), None)
            } else {
                let narrowed = connection
                    .checkpoint_preview(CheckpointPreviewRequest {
                        checkpoint_id: checkpoint,
                        through: None,
                        paths: choice.paths.clone(),
                    })
                    .await;
                match narrowed {
                    Ok(narrowed) => {
                        let same = narrowed.entries.iter().all(|entry| {
                            preview.entries.iter().any(|old| {
                                old.path == entry.path
                                    && old.state == entry.state
                                    && old.action == entry.action
                            })
                        });
                        let stale = (!same).then_some(());
                        (narrowed.plan_digest, choice.paths.clone(), stale)
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| this.restore_failed(generation, &error, cx))
                            .ok();
                        return;
                    }
                }
            };
            if stale.is_some() {
                this.update(cx, |this, cx| {
                    if let Some(sheet) = this
                        .rewind
                        .sheet
                        .as_mut()
                        .filter(|s| s.generation == generation)
                    {
                        sheet.working = false;
                        sheet.notice = Some(STALE.to_owned());
                    }
                    this.refresh_preview(cx);
                })
                .ok();
                return;
            }
            let answer = connection
                .checkpoint_restore(CheckpointRestore {
                    checkpoint_id: checkpoint,
                    through: None,
                    paths,
                    accept_conflicts: choice.accept,
                    plan_digest: digest,
                })
                .await;
            this.update_in(cx, |this, window, cx| match answer {
                Ok(restored) => this.restored(generation, restored, window, cx),
                Err(error) => this.restore_failed(generation, &error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn restore_failed(&mut self, generation: u64, error: &ClientError, cx: &mut Context<Self>) {
        let Some(sheet) = self
            .rewind
            .sheet
            .as_mut()
            .filter(|s| s.generation == generation)
        else {
            return;
        };
        sheet.working = false;
        if let Some(preview) = conflict_preview(error) {
            sheet.install(preview);
            sheet.notice = Some(STALE.to_owned());
            self.load_selected_diff(cx);
        } else if remote_code(error) == Some(ErrorCode::InvalidState) {
            sheet.error = Some(format!("Nothing was restored: {}", message_of(error)));
            self.refresh_preview(cx);
        } else {
            sheet.error = Some(format!("Nothing was restored: {}", message_of(error)));
        }
        cx.notify();
    }

    /// The restore went through: the sheet closes, the toast says how many files, the footer
    /// of the restored turn says `Restored — Undo`, and the note goes to the composer.
    fn restored(
        &mut self,
        generation: u64,
        restored: CheckpointRestored,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sheet) = self.rewind.sheet.take_if(|s| s.generation == generation) else {
            return;
        };
        let files = restored.written.len() + restored.deleted.len();
        let store = self.checkpoint_store(cx);
        store.update(cx, |store, cx| {
            store.note_restore(&restored.checkpoint, cx);
        });
        if sheet.note {
            let mut changed: Vec<&str> = restored
                .written
                .iter()
                .chain(&restored.deleted)
                .map(String::as_str)
                .collect();
            changed.sort_unstable();
            let note = agent_note(sheet.prompt.as_deref(), &changed);
            // Mawas's conversation is drawn by her page, which owns its composer's model.
            let model = self
                .panel::<super::panels::AssistantPanel>("assistant")
                .and_then(|panel| panel.read(cx).chat_model(sheet.session))
                .unwrap_or_else(|| self.ensure_chat_model(sheet.session, cx));
            model.update(cx, |model, cx| model.add_note(&note, cx));
        }
        self.show_restore_toast(sheet.session, restored.checkpoint.id, files, sheet.undo, cx);
        self.leave_rewind(sheet, cx);
        self.give_focus_back(window, cx);
        cx.notify();
    }

    // ---- the toast and Undo ----------------------------------------------------------------

    fn show_restore_toast(
        &mut self,
        session: SessionId,
        restore: CheckpointId,
        files: usize,
        undid: bool,
        cx: &mut Context<Self>,
    ) {
        self.rewind.toast_rev += 1;
        let rev = self.rewind.toast_rev;
        self.rewind.toast = Some(RestoreToast {
            session,
            restore,
            files,
            undid,
            rev,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_TIME).await;
            this.update(cx, |this, cx| {
                if this.rewind.toast.as_ref().is_some_and(|t| t.rev == rev) {
                    this.rewind.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The toast's *Undo*.
    pub fn undo_restore_toast(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(toast) = self.rewind.toast.take() {
            self.undo_restore(toast.session, toast.restore, window, cx);
        }
        cx.notify();
    }

    /// Undo of a restore: restores the restore's checkpoint. Straight away when that plan is
    /// clean; with the sheet when it has conflicts, files it cannot write, or a busy checkout.
    pub fn undo_restore(
        &mut self,
        session: SessionId,
        restore: CheckpointId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = self.sessions.read(cx).connection().clone();
        cx.spawn_in(window, async move |this, cx| {
            let preview = connection
                .checkpoint_preview(CheckpointPreviewRequest {
                    checkpoint_id: restore,
                    through: None,
                    paths: Vec::new(),
                })
                .await;
            let straight = match &preview {
                Ok(p) => {
                    p.busy.is_empty()
                        && !p.entries.is_empty()
                        && p.entries.iter().all(|e| {
                            e.state == RestoreState::Clean && e.action != RestoreAction::Skip
                        })
                }
                Err(_) => false,
            };
            let answer = match &preview {
                Ok(p) if straight => Some(
                    connection
                        .checkpoint_restore(CheckpointRestore {
                            checkpoint_id: restore,
                            through: None,
                            paths: Vec::new(),
                            accept_conflicts: Vec::new(),
                            plan_digest: p.plan_digest.clone(),
                        })
                        .await,
                ),
                _ => None,
            };
            this.update_in(cx, |this, window, cx| match (preview, answer) {
                (Ok(_), Some(Ok(restored))) => {
                    let files = restored.written.len() + restored.deleted.len();
                    let store = this.checkpoint_store(cx);
                    store.update(cx, |store, cx| {
                        store.note_restore(&restored.checkpoint, cx);
                    });
                    this.show_restore_toast(session, restored.checkpoint.id, files, true, cx);
                }
                (Err(error), _) => {
                    this.set_notice(
                        format!("Cannot undo the restore: {}", message_of(&error)),
                        cx,
                    );
                }
                // Anything but a clean plan is shown first, as any restore is.
                _ => {
                    let mut sheet = RewindSheet::new(session, restore, true, None);
                    sheet.notice =
                        Some("Undo needs a look: some files changed since the restore.".to_owned());
                    this.open_sheet(sheet, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// ⌘K *Chat: Undo Last Restore*: the newest restore of the focused chat.
    pub fn undo_last_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.focused_session() else {
            self.set_notice("Undo Last Restore works on the focused chat", cx);
            return;
        };
        let store = self.checkpoint_store(cx);
        if !store.read(cx).restore_supported() {
            self.set_notice("This daemon cannot restore files", cx);
            return;
        }
        if !store.read(cx).loaded(session) {
            store.update(cx, |store, cx| store.ensure(session, cx));
        }
        match store.read(cx).last_restore(session).map(|c| c.id) {
            Some(restore) => self.undo_restore(session, restore, window, cx),
            None => self.set_notice("This chat has no restore to undo", cx),
        }
    }

    // ---- the Checkpoints list --------------------------------------------------------------

    /// The Checkpoints list of `session`: each checkpoint with its *Rewind…*.
    pub fn show_checkpoints(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let store = self.checkpoint_store(cx);
        if !store.read(cx).supported() {
            self.set_notice("This daemon keeps no checkpoints", cx);
            return;
        }
        store.update(cx, |store, cx| store.reload(session, cx));
        let model = match self.chat_models.get(&session) {
            Some(model) => model.clone(),
            None => {
                let connection = self.sessions.read(cx).connection().clone();
                cx.new(|cx| ChatModel::new(session, connection, cx))
            }
        };
        self.remember_focus(window, cx);
        self.palette = None;
        self.context_menu = None;
        self.rewind.list = Some(CheckpointsList {
            session,
            selected: 0,
            model,
        });
        window.focus(&self.rewind.list_focus, cx);
        cx.notify();
    }

    /// ⌘K *Chat: Rewind Files to Before This Turn…*: the Checkpoints list of the focused chat,
    /// to pick the turn.
    pub fn rewind_focused_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused_session() {
            Some(session) => self.show_checkpoints(session, window, cx),
            None => self.set_notice("Rewind works on the focused chat", cx),
        }
    }

    pub fn close_checkpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.rewind.list.take() {
            self.leave_checkpoints(old, cx);
            self.give_focus_back(window, cx);
            cx.notify();
        }
    }

    /// The rows of the open list: the checkpoints that kept files, newest first.
    pub fn checkpoint_rows(&self, cx: &gpui::App) -> Vec<ListRow> {
        let Some(list) = &self.rewind.list else {
            return Vec::new();
        };
        let model = list.model.read(cx);
        self.checkpoint_store(cx)
            .read(cx)
            .of(list.session)
            .iter()
            .filter(|c| c.files_changed > 0 || c.state != CheckpointState::Ok)
            .map(|c| ListRow {
                checkpoint: c.clone(),
                prompt: c.turn_id.and_then(|turn| turn_prompt(model, turn)),
            })
            .collect()
    }

    /// Row `index`'s *Rewind…* (the list stays under the sheet).
    pub fn rewind_list_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.rewind.list.as_ref().map(|l| l.session) else {
            return;
        };
        let Some(row) = self.checkpoint_rows(cx).into_iter().nth(index) else {
            return;
        };
        if row.checkpoint.state != CheckpointState::Ok
            || !self.checkpoint_store(cx).read(cx).restore_supported()
        {
            return;
        }
        if let Some(list) = &mut self.rewind.list {
            list.selected = index;
        }
        let undo = row.checkpoint.kind == CheckpointKind::Restore;
        let sheet = RewindSheet::new(session, row.checkpoint.id, undo, row.prompt);
        self.open_sheet(sheet, window, cx);
    }

    pub(crate) fn checkpoints_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = self.checkpoint_rows(cx).len();
        let Some(list) = &mut self.rewind.list else {
            return false;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.close_checkpoints(window, cx),
            "up" => {
                list.selected = list.selected.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                list.selected = (list.selected + 1).min(count.saturating_sub(1));
                cx.notify();
            }
            "enter" => {
                let at = list.selected;
                self.rewind_list_row(at, window, cx);
            }
            _ => return false,
        }
        true
    }

    // ---- menus -----------------------------------------------------------------------------

    /// The ⋯ menus' line about a chat's checkpoints: *Checkpoints…* (the list), or, for a chat
    /// whose folder cannot have them, the reason, greyed (said once here, never per turn).
    /// Nothing for a chat whose checkpoints are off, for other sessions, and without the
    /// daemon's capability.
    pub(crate) fn checkpoints_menu_entry(
        &self,
        session: SessionId,
        cx: &gpui::App,
    ) -> Option<super::view::MenuEntry> {
        use hutan_core::CheckpointAvailability;
        if !self.checkpoint_store(cx).read(cx).supported() {
            return None;
        }
        let info = self.sessions.read(cx).session(session)?;
        let chat = info.chat.as_ref()?;
        match chat.checkpoints.as_ref() {
            Some(status) if status.state == CheckpointAvailability::Unsupported => {
                let reason = status.reason.as_deref().unwrap_or("not available here");
                Some(super::view::MenuEntry {
                    label: format!("Checkpoints: {reason}").into(),
                    disabled: true,
                    ..Self::entry("Checkpoints", |_, _, _| {})
                })
            }
            Some(status) if status.state == CheckpointAvailability::Off => None,
            Some(_) => Some(Self::entry("Checkpoints…", move |this, window, cx| {
                this.show_checkpoints(session, window, cx);
            })),
            // Assistants and direct chats have none.
            None => None,
        }
    }

    // ---- the diff of the selected file -----------------------------------------------------

    /// Fetches the diff `now → restored` of the selected file once.
    fn load_selected_diff(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.rewind.sheet else {
            return;
        };
        let Some(row) = sheet.rows.get(sheet.selected) else {
            return;
        };
        if row.group == Group::NotCaptured {
            return;
        }
        let path = row.entry.path.clone();
        if sheet.diffs.contains_key(&path) {
            return;
        }
        // The diff from the first rewound checkpoint's start to now, read backwards.
        let Some(first) = sheet
            .preview
            .as_ref()
            .and_then(|p| p.checkpoints.first().copied())
        else {
            return;
        };
        sheet.diffs.insert(path.clone(), DiffState::Loading);
        let generation = sheet.generation;
        let connection = self.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let answer = connection
                .checkpoint_diff(CheckpointDiffRequest {
                    checkpoint_id: first,
                    against: DiffAgainst::Now,
                    path: Some(path.clone()),
                    stat: false,
                })
                .await;
            this.update(cx, |this, cx| {
                let Some(sheet) = this
                    .rewind
                    .sheet
                    .as_mut()
                    .filter(|s| s.generation == generation)
                else {
                    return;
                };
                let state = match answer {
                    Ok(diff) => DiffState::Ready(
                        diff.patch
                            .as_deref()
                            .map(reverse_patch)
                            .and_then(|patch| split_patch(&patch).into_iter().next()),
                    ),
                    Err(error) => DiffState::Failed(error.to_string()),
                };
                sheet.diffs.insert(path, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// The words the sheet says when the plan changed under it.
pub const STALE: &str =
    "The files changed since this preview: this is the new plan. Nothing was restored.";

fn message_of(error: &ClientError) -> String {
    match error {
        ClientError::Remote(body) => body.message.clone(),
        other => other.to_string(),
    }
}

/// The note for the agent (spike §5.4): Hutan's one bridge to what the agent remembers.
pub fn agent_note(prompt: Option<&str>, files: &[&str]) -> String {
    let turn = match prompt {
        Some(prompt) => format!("the turn \u{201c}{prompt}\u{201d}"),
        None => "an earlier turn".to_owned(),
    };
    format!(
        "Hutan restored these files to before {turn}: {}. Disregard the changes of that turn.",
        files.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, action: RestoreAction, state: RestoreState) -> RestoreEntry {
        RestoreEntry {
            path: path.to_owned(),
            action,
            state,
            reason: None,
            accept: state.is_conflict().then_some(RestoreAction::Write),
        }
    }

    #[test]
    fn rows_group_conflicts_unticked_and_uncaptured_greyed() {
        let rows = rows_of(&[
            entry("z.txt", RestoreAction::Skip, RestoreState::After),
            entry("b.txt", RestoreAction::Write, RestoreState::Clean),
            entry("big.bin", RestoreAction::Skip, RestoreState::Uncaptured),
            entry("a.txt", RestoreAction::Delete, RestoreState::Clean),
            entry("g.txt", RestoreAction::Skip, RestoreState::Gap),
        ]);
        let summary: Vec<(&str, Group, bool, bool)> = rows
            .iter()
            .map(|r| (r.entry.path.as_str(), r.group, r.checked, r.checkable()))
            .collect();
        assert_eq!(
            summary,
            [
                ("a.txt", Group::Clean, true, true),
                ("b.txt", Group::Clean, true, true),
                ("z.txt", Group::After, false, true),
                ("g.txt", Group::Gap, false, true),
                ("big.bin", Group::NotCaptured, false, false),
            ]
        );
        assert_eq!(rows[0].action_text(), "will be deleted");
        assert_eq!(rows[2].action_text(), "will be written back");
    }

    #[test]
    fn a_patch_reads_backwards() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,3 @@ fn\n alpha\n-beta\n+gamma\n+delta\n";
        assert_eq!(
            reverse_patch(patch),
            "diff --git a/a.txt b/a.txt\n+++ a/a.txt\n--- b/a.txt\n@@ -1,3 +1,2 @@ fn\n alpha\n-gamma\n-delta\n+beta\n"
        );
        let created = "--- /dev/null\n+++ b/n.txt\n@@ -0,0 +1 @@\n+new\n";
        let files = split_patch(&reverse_patch(created));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].removed, 1);
        assert_eq!(files[0].added, 0);
    }

    #[test]
    fn the_note_names_the_turn_and_the_files() {
        assert_eq!(
            agent_note(Some("fix the parser"), &["a.rs", "b.rs"]),
            "Hutan restored these files to before the turn \u{201c}fix the parser\u{201d}: a.rs, \
             b.rs. Disregard the changes of that turn."
        );
        assert!(agent_note(None, &["a.rs"]).contains("an earlier turn"));
    }
}
